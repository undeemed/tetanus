//! Files and blobs carried into a turn.
//!
//! Somebody drags a screenshot into a chat, or a script hands the harness a log
//! file. The bytes have to be admitted, stored once, and recorded on the
//! journal so a replay can say what the turn was looking at.
//!
//! **The whole batch is judged before anything is stored.** Count, aggregate
//! size, media type and each item's own bytes are all checked first, and a
//! batch with one bad member stores none of it. A half-admitted batch is the
//! worst outcome available: the turn sees some of what the user attached, and
//! nobody can tell which part is missing from the record.
//!
//! **Equal bytes are stored once.** An object is addressed by the digest of its
//! content, so attaching the same screenshot twice writes one file and produces
//! two references to it. That is upstream's content-addressed store, and the
//! property that matters is not the space saved but that a reference is stable:
//! the same bytes always name the same object.
//!
//! **An image is measured before it is decoded.** The dimensions come out of
//! the header, and a picture whose pixel count is over the limit is refused
//! without ever being decoded - because decoding is where a hostile image costs
//! a gigabyte of memory, and a check that happens after it has not checked
//! anything.
//!
//! **A caller's mistake and a storage fault are different answers.** The first
//! is something the person or the model can fix by attaching something else;
//! the second is the deployment's. Collapsing them would tell a user their
//! screenshot is invalid when the disk is full.
//!
//! Parity: upstream `packages/attachment/attachment` and its local store,
//! pinned by their `index.spec.ts`, `store.spec.ts` and `image.spec.ts`.

use std::path::{Path, PathBuf};

use serde_json::json;
use tetanus_session::{SessionError, SessionEvent, SessionLog};

/// The durable type this module writes.
pub mod topic {
    /// One admitted attachment: what it was, how big, and where its bytes are.
    /// The bytes themselves are never on the journal - a base64 screenshot in
    /// a JSONL line is a line no reader can read.
    pub const ATTACHMENT_ADDED: &str = "attachment/added";
}

/// What a deployment will admit.
///
/// Every field is a bound rather than a preference, and each exists because
/// something without it is unbounded: a batch of ten thousand items, a single
/// file the size of memory, a set of small files that add up to the same, or a
/// picture that decodes to more pixels than a machine can hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub max_items: usize,
    pub max_item_bytes: usize,
    pub max_total_bytes: usize,
    /// Decoded pixels, not bytes: a 200-byte PNG can declare a 60000x60000
    /// image, and it is the decode that would cost the memory.
    pub max_pixels: u64,
    /// The media types this deployment accepts. Empty accepts any type that is
    /// otherwise valid.
    pub media_types: Vec<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_items: 20,
            max_item_bytes: 10 * 1024 * 1024,
            max_total_bytes: 32 * 1024 * 1024,
            max_pixels: 40_000_000,
            media_types: Vec::new(),
        }
    }
}

impl Limits {
    fn admits(&self, media_type: &str) -> bool {
        self.media_types.is_empty() || self.media_types.iter().any(|kind| kind == media_type)
    }
}

/// One thing a caller wants to attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// What to call it. Shown to the model and the user; never used as a path.
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// One admitted attachment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    /// The content address: equal bytes give an equal id, always.
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub bytes: usize,
    /// Set for a picture whose header this build can read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Dimensions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

impl Dimensions {
    fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Why a batch was not admitted.
///
/// Split into what a caller can fix and what it cannot, because those need
/// different words and different responses.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdmissionError {
    #[error("{count} attachments is more than the {limit} this deployment admits at once")]
    TooMany { count: usize, limit: usize },
    #[error("{name:?} is {bytes} bytes, over the {limit}-byte limit for one attachment")]
    ItemTooLarge {
        name: String,
        bytes: usize,
        limit: usize,
    },
    #[error("the batch is {bytes} bytes together, over the {limit}-byte limit")]
    BatchTooLarge { bytes: usize, limit: usize },
    #[error("{name:?} is {media_type:?}, which this deployment does not admit")]
    MediaType { name: String, media_type: String },
    #[error("{name:?} is empty, so there is nothing to attach")]
    Empty { name: String },
    #[error("{name:?} has no name")]
    Unnamed { name: String },
    #[error(
        "{name:?} declares {media_type:?} but its bytes are not that format, so it cannot be read"
    )]
    Malformed { name: String, media_type: String },
    #[error(
        "{name:?} is {width}x{height}, which is {pixels} pixels and over the {limit}-pixel limit"
    )]
    TooManyPixels {
        name: String,
        width: u32,
        height: u32,
        pixels: u64,
        limit: u64,
    },
}

/// A failure that is the deployment's rather than the caller's.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Refused(#[from] AdmissionError),
    #[error("the attachment store at {path} could not be written: {reason}")]
    Storage { path: String, reason: String },
    #[error(
        "{path} already holds different bytes under this content address, so the store is \
         inconsistent and nothing was written"
    )]
    Collision { path: String },
    #[error(transparent)]
    Log(#[from] SessionError),
}

/// Judge one batch, in the order it was given, without storing anything.
///
/// Batch-wide limits are checked before per-item ones so a caller that attached
/// forty files is told that first, rather than being told about the first file
/// and discovering the count limit on the next attempt.
pub fn admit(batch: &[Incoming], limits: &Limits) -> Result<Vec<Attachment>, AdmissionError> {
    if batch.len() > limits.max_items {
        return Err(AdmissionError::TooMany {
            count: batch.len(),
            limit: limits.max_items,
        });
    }
    let total: usize = batch.iter().map(|item| item.bytes.len()).sum();
    if total > limits.max_total_bytes {
        return Err(AdmissionError::BatchTooLarge {
            bytes: total,
            limit: limits.max_total_bytes,
        });
    }

    batch.iter().map(|item| judge(item, limits)).collect()
}

/// Judge one member of a batch the batch-wide limits have already passed.
///
/// Separate from [`admit`] so each function answers one question: that one is
/// about the batch, this one is about an item, and neither has to be read to
/// understand the other.
fn judge(item: &Incoming, limits: &Limits) -> Result<Attachment, AdmissionError> {
    let name = item.name.trim().to_string();
    if name.is_empty() {
        return Err(AdmissionError::Unnamed {
            name: item.name.clone(),
        });
    }
    if item.bytes.is_empty() {
        return Err(AdmissionError::Empty { name });
    }
    if item.bytes.len() > limits.max_item_bytes {
        return Err(AdmissionError::ItemTooLarge {
            name,
            bytes: item.bytes.len(),
            limit: limits.max_item_bytes,
        });
    }
    if !limits.admits(&item.media_type) {
        return Err(AdmissionError::MediaType {
            name,
            media_type: item.media_type.clone(),
        });
    }
    Ok(Attachment {
        id: address(&item.bytes),
        dimensions: picture(item, &name, limits)?,
        name,
        media_type: item.media_type.clone(),
        bytes: item.bytes.len(),
    })
}

/// The dimensions of an item that declares itself a picture, or `None` for one
/// that does not.
///
/// This is where the pixel limit is applied, and it is applied to what the
/// header declares rather than to anything decoded - which is the whole reason
/// the measurement is a header read.
fn picture(
    item: &Incoming,
    name: &str,
    limits: &Limits,
) -> Result<Option<Dimensions>, AdmissionError> {
    if !item.media_type.starts_with("image/") {
        return Ok(None);
    }
    let measured = measure(&item.bytes).ok_or_else(|| AdmissionError::Malformed {
        name: name.to_string(),
        media_type: item.media_type.clone(),
    })?;
    if measured.pixels() > limits.max_pixels {
        return Err(AdmissionError::TooManyPixels {
            name: name.to_string(),
            width: measured.width,
            height: measured.height,
            pixels: measured.pixels(),
            limit: limits.max_pixels,
        });
    }
    Ok(Some(measured))
}

/// Admit a batch, store its objects, and record them on the journal.
///
/// Nothing is written until the whole batch has been judged, which is the
/// property `admit` exists to make possible: a rejected batch leaves no object
/// and no record.
pub fn attach(
    log: &dyn SessionLog,
    root: &Path,
    batch: &[Incoming],
    limits: &Limits,
) -> Result<Vec<Attachment>, StoreError> {
    let admitted = admit(batch, limits)?;

    std::fs::create_dir_all(root).map_err(|error| StoreError::Storage {
        path: root.display().to_string(),
        reason: error.to_string(),
    })?;
    for (attachment, item) in admitted.iter().zip(batch) {
        publish(root, &attachment.id, &item.bytes)?;
    }
    for attachment in &admitted {
        log.append(topic::ATTACHMENT_ADDED, json!(attachment))?;
    }
    Ok(admitted)
}

/// Write one object, or confirm the one already there is the same bytes.
///
/// The comparison on a hit is what makes the address trustworthy: the digest
/// below is a content address for deduplication and not a cryptographic one, so
/// the store verifies rather than assumes. A mismatch is refused as an
/// inconsistent store rather than overwritten, because whichever of the two
/// callers is wrong, silently replacing one's bytes with the other's is worse.
fn publish(root: &Path, id: &str, bytes: &[u8]) -> Result<(), StoreError> {
    let path = object_path(root, id);
    match std::fs::read(&path) {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {
            return Err(StoreError::Collision {
                path: path.display().to_string(),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(StoreError::Storage {
                path: path.display().to_string(),
                reason: error.to_string(),
            })
        }
    }
    std::fs::write(&path, bytes).map_err(|error| StoreError::Storage {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

/// Where one object lives under a store root.
///
/// The name *is* the content address, so the layout is flat: nothing here needs
/// a fan-out directory for a store whose size is one session's attachments, and
/// a reader outside this module - the surface vocabulary in [`crate::view`] -
/// asks here rather than keeping a second copy of the rule.
pub fn object_path(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

/// Read one stored object back.
pub fn read(root: &Path, id: &str) -> Result<Vec<u8>, StoreError> {
    let path = object_path(root, id);
    std::fs::read(&path).map_err(|error| StoreError::Storage {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

/// Everything this journal says was attached, oldest first.
pub fn recorded(events: &[SessionEvent]) -> Vec<Attachment> {
    events
        .iter()
        .filter(|event| event.ty == topic::ATTACHMENT_ADDED)
        .filter_map(|event| serde_json::from_value::<Attachment>(event.data.clone()).ok())
        .collect()
}

/// The content address of one blob.
///
/// A 128-bit FNV-1a over the bytes, with the length appended. **Not a
/// cryptographic digest**, and nothing here treats it as one: it is a
/// deduplication key, every hit is verified byte for byte by [`publish`], and
/// an object is only ever addressed by content this process just hashed. A real
/// digest would mean a dependency, and the property this needs - equal bytes,
/// equal name - does not require one.
pub fn address(bytes: &[u8]) -> String {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:032x}-{}", bytes.len())
}

/// Read a picture's dimensions out of its header.
///
/// Header only: the point is to know the size *before* deciding whether to
/// decode, so a hostile file is refused for what it declares rather than after
/// it has cost the memory. A format this build cannot read answers `None`,
/// which the caller turns into "declares an image but is not one" - the same
/// stable answer upstream gives for malformed bytes and unsupported formats
/// alike, because a caller can do the same thing about both.
pub fn measure(bytes: &[u8]) -> Option<Dimensions> {
    png(bytes).or_else(|| gif(bytes)).or_else(|| jpeg(bytes))
}

fn png(bytes: &[u8]) -> Option<Dimensions> {
    const MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if !bytes.starts_with(MAGIC) || bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some(Dimensions {
        width: u32::from_be_bytes(bytes[16..20].try_into().ok()?),
        height: u32::from_be_bytes(bytes[20..24].try_into().ok()?),
    })
}

fn gif(bytes: &[u8]) -> Option<Dimensions> {
    if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) || bytes.len() < 10 {
        return None;
    }
    Some(Dimensions {
        width: u32::from(u16::from_le_bytes(bytes[6..8].try_into().ok()?)),
        height: u32::from(u16::from_le_bytes(bytes[8..10].try_into().ok()?)),
    })
}

/// Walk a JPEG's segments to the frame header that carries the size.
///
/// A JPEG has no fixed offset for its dimensions, so the walk is the only way
/// to read them. It is bounded by the file: each step advances past a segment
/// whose length the file states, and a length that would not advance ends the
/// walk rather than looping.
fn jpeg(bytes: &[u8]) -> Option<Dimensions> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut at = 2;
    while at + 9 < bytes.len() {
        if bytes[at] != 0xff {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        // Start-of-frame markers, minus the four that are not frames.
        let is_frame =
            (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc | 0xd8);
        let length = usize::from(u16::from_be_bytes(bytes[at + 2..at + 4].try_into().ok()?));
        if is_frame {
            return Some(Dimensions {
                height: u32::from(u16::from_be_bytes(bytes[at + 5..at + 7].try_into().ok()?)),
                width: u32::from(u16::from_be_bytes(bytes[at + 7..at + 9].try_into().ok()?)),
            });
        }
        if length < 2 {
            return None;
        }
        at += 2 + length;
    }
    None
}

/// Where a session keeps its attachments, under a harness home.
///
/// One directory per session rather than one shared pool: a session's blobs go
/// when its journal does, and a shared pool would need reference counting
/// nothing here has a reason to build.
pub fn store_root(home: &Path, session_id: &str) -> PathBuf {
    home.join("attachments").join(session_id)
}
