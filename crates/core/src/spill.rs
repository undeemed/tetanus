//! Somewhere to put a payload that is too big to carry, and the rule that
//! decides when to put it there.
//!
//! A tool that reads a large file produces a result the model cannot afford
//! and the user may still want. Spilling keeps the whole thing on disk and
//! hands the model a bounded preview plus a locator, so nothing is lost and
//! the context is not spent.
//!
//! **The replacement never exceeds the cap.** The notice costs bytes, so the
//! preview budget is the cap *minus* a worst-case notice, priced before the
//! preview is cut. A naive implementation spends the whole budget on the
//! preview and then appends the notice, which for a marginally over-cap result
//! produces a replacement bigger than the original - the one outcome a size
//! policy must never have.
//!
//! **A spill that cannot help declines.** When the notice alone will not fit
//! the cap - a tiny cap, or a long spill root - there is no replacement worth
//! making, so the original is kept. Serving a truncated locator would be worse
//! than serving the content: the content is at least usable.
//!
//! **A spill failure is never a tool failure.** Storage that is full, refused
//! or absent leaves the result exactly as it was. A successful call must not
//! become an error because the harness could not file its output away.
//!
//! **The head and the tail, not the head.** The two ends are where the
//! information is, which is the same reason [`crate`]'s tool-result pruner
//! keeps both.
//!
//! **Bytes, and never a split character.** The budget is in bytes because that
//! is what a size cap means, but a cut at a byte offset lands mid-character
//! routinely, so every cut is moved to the nearest character boundary. A
//! preview that is not text is not a preview.
//!
//! Parity: upstream `packages/spill`, `spill-local` and `spill-policy`, pinned
//! by their `spill-local.spec.ts` and `spill-policy.spec.ts`. Upstream's
//! policy is a `tools/post-execute` listener; tetanus has no post-execute
//! projection seam yet, so this is the decision and the storage, published for
//! the pipeline to call when that seam lands. Its content-block handling has
//! nothing to restate: a tetanus tool result carries a `String`.

use std::path::{Path, PathBuf};

/// One saved artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRef {
    /// The handle a reader opens. A path for this backend; a caller renders it
    /// and does not parse it.
    pub locator: String,
    /// How many bytes were stored.
    pub bytes: usize,
}

/// What produced an artifact. Descriptive, never used for access control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillSource {
    /// The session the artifact belongs to. It scopes the directory, so one
    /// session's output is not scattered through another's.
    pub session_id: String,
    /// The tool whose result this was.
    pub tool: String,
    /// The model-issued call id.
    pub call_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SpillError {
    #[error("{}: cannot be written: {source}", path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A directory of spilled artifacts, one subdirectory per session.
#[derive(Debug, Clone)]
pub struct SpillStore {
    root: PathBuf,
}

impl SpillStore {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store text and answer where it went.
    ///
    /// The name carries a counter, so two results of the same tool in the same
    /// session do not collide and a reader can see which came first. The file
    /// is created exclusively and owner-only: an exclusive create fails on any
    /// existing path, symlink included, so a pre-planted target in a shared
    /// root cannot redirect the write.
    pub fn save(&self, source: &SpillSource, content: &str) -> Result<SpillRef, SpillError> {
        let mut writing = self.open(source)?;
        writing.write(content.as_bytes())?;
        writing.finish()
    }

    /// Open an artifact and write it as it is produced.
    ///
    /// [`SpillStore::save`] is the whole-payload case and is written in terms
    /// of this one, so both put files in the same place under the same names
    /// and an operator finds one kind of artifact rather than two.
    ///
    /// The streaming half exists because of who has the bytes. A tool result
    /// is spilled *after* the fact by whoever holds it, but a process's output
    /// is bounded *while* it runs - by the time anything above the seam sees a
    /// result, the dropped prefix is gone and no post-hoc spill can bring it
    /// back. Only the producer can keep what it is about to drop, and it must
    /// do so without holding the whole stream in memory, which is what it was
    /// dropping bytes to avoid.
    pub fn open(&self, source: &SpillSource) -> Result<SpillWriter, SpillError> {
        let dir = self
            .root
            .join(format!("session-{}", segment(&source.session_id)));
        std::fs::create_dir_all(&dir).map_err(|error| unwritable(&dir, error))?;

        let base = format!("{}-{}", segment(&source.tool), segment(&source.call_id));
        for attempt in 0..1_000 {
            let path = dir.join(match attempt {
                0 => format!("{base}.txt"),
                n => format!("{base}-{n}.txt"),
            });
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(SpillWriter {
                        file,
                        path,
                        bytes: 0,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(unwritable(&path, error)),
            }
        }
        Err(unwritable(
            &dir,
            std::io::Error::other("a thousand names for one call are all taken"),
        ))
    }
}

/// One artifact being written as its producer makes it.
#[derive(Debug)]
pub struct SpillWriter {
    file: std::fs::File,
    path: PathBuf,
    bytes: usize,
}

impl SpillWriter {
    /// Where this artifact will be, for a producer that wants to say so before
    /// it has finished writing.
    pub fn locator(&self) -> String {
        self.path.display().to_string()
    }

    /// Append. Bytes rather than text, because a process's output is bytes
    /// and a character split across two reads must not become two writes of
    /// something else.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), SpillError> {
        use std::io::Write;
        self.file
            .write_all(bytes)
            .map_err(|error| unwritable(&self.path, error))?;
        self.bytes += bytes.len();
        Ok(())
    }

    /// Flush to the filesystem and answer where it went.
    pub fn finish(mut self) -> Result<SpillRef, SpillError> {
        use std::io::Write;
        self.file
            .flush()
            .map_err(|error| unwritable(&self.path, error))?;
        self.file
            .sync_all()
            .map_err(|error| unwritable(&self.path, error))?;
        Ok(SpillRef {
            locator: self.locator(),
            bytes: self.bytes,
        })
    }
}

fn unwritable(path: &Path, source: std::io::Error) -> SpillError {
    SpillError::Unwritable {
        path: path.to_path_buf(),
        source,
    }
}

/// The cap a model-facing payload is held to, in UTF-8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpillPolicy {
    /// A payload larger than this is spilled and replaced.
    pub max_inline_bytes: usize,
}

/// What one policy decision produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spilled {
    /// What the model reads instead: a bounded preview and the locator.
    pub replacement: String,
    /// Where the whole thing went.
    pub reference: SpillRef,
}

impl SpillPolicy {
    /// Apply the cap to one text payload.
    ///
    /// `None` means keep the original, and it is the answer for every reason
    /// keeping it is right: the payload fits, the store refused, or no
    /// replacement can be built inside the cap. A caller therefore has one
    /// branch to write and cannot accidentally treat a storage failure as an
    /// empty result.
    pub fn apply(
        &self,
        store: &SpillStore,
        source: &SpillSource,
        content: &str,
    ) -> Option<Spilled> {
        if content.len() <= self.max_inline_bytes {
            return None;
        }
        // A storage failure keeps the inline content: a successful tool call
        // must not become an error because the harness could not file its
        // output away.
        let reference = store.save(source, content).ok()?;

        // The notice is priced at its worst case - an omission count of the
        // whole payload - before the preview is cut, because the digit count
        // of the real omission can only be smaller. Reserving after the fact
        // is what makes a replacement that overruns the cap.
        let reserve = notice(content.len(), &reference).len() + 2;
        let budget = self.max_inline_bytes.saturating_sub(reserve);
        let preview = head_tail(content, budget);
        let omitted = content.len() - preview.len();
        let notice = notice(omitted, &reference);
        let replacement = match preview.is_empty() {
            true => notice,
            false => format!("{preview}\n\n{notice}"),
        };

        // There is no within-cap replacement when the notice alone overruns.
        // A replacement inside the cap is always smaller than the original,
        // which was over it, so this one check covers both rules.
        (replacement.len() <= self.max_inline_bytes).then_some(Spilled {
            replacement,
            reference,
        })
    }
}

/// The line that tells the model what happened and where the rest is.
fn notice(omitted: usize, reference: &SpillRef) -> String {
    format!(
        "({omitted} bytes omitted. The full result is stored at: {}. Read that path to see it.)",
        reference.locator
    )
}

/// `budget` bytes of `text`, split across its two ends, never splitting a
/// character.
fn head_tail(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let head = ceiling(text, budget.div_ceil(2));
    let tail = floor(text, text.len() - budget / 2);
    // The two halves are cut to character boundaries independently, so a
    // pathological input can leave them overlapping; an overlap would repeat
    // content rather than omit it.
    if tail <= head {
        return text.to_string();
    }
    format!("{}{}", &text[..head], &text[tail..])
}

/// The largest character boundary at or below `at`.
fn floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The largest character boundary at or below `at`, for a head cut. Rounding
/// down on both ends is what keeps the result inside the budget.
fn ceiling(text: &str, at: usize) -> usize {
    floor(text, at)
}

/// An arbitrary string as one safe path segment.
///
/// A session id, a tool name and a call id are all untrusted to some degree -
/// a call id is minted by a model - so `../`, an absolute path, a separator
/// and a NUL are all neutralized before any of them reaches the filesystem.
/// Anything outside the safe set becomes `_`, and an empty string becomes `_`
/// rather than an empty segment.
fn segment(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') => c,
            _ => '_',
        })
        .collect();
    // `.` and `..` are whole-segment tokens that traverse, so they are not
    // names even though every character in them is safe.
    match mapped.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => mapped.chars().take(64).collect(),
    }
}
