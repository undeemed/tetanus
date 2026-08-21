//! Durable records of work the harness owes: what was queued, what is
//! running, and how each one ended.
//!
//! A job is work with a lifetime longer than the call that asked for it - a
//! command still running when the turn that started it ended, a delegation, a
//! scheduled reminder's delivery. The registry that holds them has to survive
//! a restart, because the interesting question after a crash is exactly the
//! one an in-memory registry cannot answer: what was running when we died?
//!
//! **A journal, not a table.** The store is an append-only log of transitions,
//! for the reason the session log is: a record of what happened cannot be
//! corrupted by a later write, a crash can only ever cut the last line, and
//! the state is a fold anyone can re-derive. A mutable row per job would give
//! a torn write no way to be detected and a lost transition no way to be
//! noticed.
//!
//! **Reopening repairs, exactly as the session store does.** A job the log
//! last saw `running` cannot still be running: the process that owned it is
//! gone. [`JobStore::open`] closes each one as [`JobStatus::Interrupted`],
//! appending that transition rather than assuming it, so the log stays the
//! whole story and a second reopen finds nothing to do. Leaving them
//! `running` would make the store lie about live work for ever; deleting them
//! would lose the fact that the work was cut off.
//!
//! **One terminal transition per job.** A job that has ended does not end
//! again, so a late `finish` for work a repair already closed is refused
//! rather than appended. Upstream states the same rule with its `reported`
//! flag; the reason is the same - a second terminal record makes "how did this
//! end" a question with two answers.
//!
//! Parity: upstream `packages/jobs/jobs` and `jobs-local`, pinned by their
//! `service.spec.ts` and `jobs.spec.ts`. Upstream's registry is in memory and
//! its durability is the agent's session; the persistence is the tetanus
//! difference and the reason the acceptance case is a restart. Its output
//! cursor - `readOutput` consuming a delta per read - belongs to a producer
//! that streams, which is the tool pipeline's, so this stores the terminal
//! output a producer hands over and not a stream.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Where a job is in its life.
///
/// `Queued` and `Running` are live; the other four are terminal and a job
/// reaches exactly one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Accepted and not started.
    Queued,
    /// Started and not settled.
    Running,
    /// Finished on its own terms.
    Completed,
    /// Stopped because someone asked.
    Cancelled,
    /// Stopped because it broke.
    Failed,
    /// The process that owned it went away while it was live. Distinct from
    /// [`JobStatus::Failed`] on purpose: the work did not report a failure,
    /// and nobody knows how far it got.
    Interrupted,
}

impl JobStatus {
    /// Whether this is an end state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, JobStatus::Queued | JobStatus::Running)
    }
}

/// One job, as a reader sees it: a fold of every transition recorded for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// The producer that owns the work - `bash`, `workflow`, `schedule`.
    pub kind: String,
    /// A one-line label for a human or a model to read.
    pub label: String,
    /// The session that asked for this, when one did. An unowned job belongs
    /// to the harness rather than to a conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub status: JobStatus,
    /// Whatever the producer said about how it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The producer's final output, for a job that has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Epoch milliseconds when the job was recorded.
    pub queued_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
}

impl Job {
    pub fn is_live(&self) -> bool {
        !self.status.is_terminal()
    }
}

/// One line of the job journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Transition {
    id: String,
    at: u64,
    #[serde(flatten)]
    change: Change,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Change {
    /// The job exists, and everything about it that never changes.
    Queue {
        kind: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// It began.
    Start,
    /// It ended, one way or another.
    Finish {
        status: JobStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("{}: cannot be read: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: cannot be written: {source}", path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A line the writer finished that does not parse. Not a crash tail - the
    /// newline is the commit - so the log is not the log that was written and
    /// is refused rather than read past, naming the line.
    #[error("{}: line {line} is not a job transition", path.display())]
    Corrupt { path: PathBuf, line: usize },
    #[error("no job {0:?}")]
    NoSuchJob(String),
    #[error("job {id:?} already exists")]
    Duplicate { id: String },
    /// A terminal transition for a job that already has one.
    #[error("job {id:?} already ended as {status:?}; it cannot end twice")]
    AlreadyEnded { id: String, status: JobStatus },
    #[error("job {id:?} is {status:?}, so it cannot start")]
    NotStartable { id: String, status: JobStatus },
    #[error("{what} {value:?} must be 1 to 128 characters of [A-Za-z0-9._-]")]
    BadName { what: &'static str, value: String },
}

/// The durable registry of jobs.
#[derive(Debug)]
pub struct JobStore {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    file: std::fs::File,
    jobs: BTreeMap<String, Job>,
    /// How many ids this store has minted, so a fresh one never collides with
    /// a job the log already holds.
    minted: u64,
}

impl JobStore {
    /// Open the store at `path`, repairing what a crash left live.
    ///
    /// Every job the log last saw `Queued` or `Running` is closed as
    /// [`JobStatus::Interrupted`], and that closure is *appended*: the log
    /// stays the whole story, and a second open of the same file finds
    /// nothing left to repair.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JobError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|source| JobError::Unwritable {
                    path: path.clone(),
                    source,
                })?;
            }
        }

        let scan = scan(&path)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| JobError::Unwritable {
                path: path.clone(),
                source,
            })?;
        // A record the writer did not finish is dropped from the file, not
        // only from the reading: appending after a half-written line would
        // splice the next transition onto it.
        let length = file
            .metadata()
            .map_err(|source| JobError::Unreadable {
                path: path.clone(),
                source,
            })?
            .len();
        if scan.committed < length {
            file.set_len(scan.committed)
                .and_then(|()| file.sync_all())
                .map_err(|source| JobError::Unwritable {
                    path: path.clone(),
                    source,
                })?;
        }

        let store = Self {
            path,
            state: Mutex::new(State {
                file,
                jobs: scan.jobs,
                minted: 0,
            }),
        };
        store.repair()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Close everything the log left live. See [`JobStore::open`].
    fn repair(&self) -> Result<usize, JobError> {
        let live: Vec<String> = {
            let state = self.state.lock().expect("jobs");
            state
                .jobs
                .values()
                .filter(|job| job.is_live())
                .map(|job| job.id.clone())
                .collect()
        };
        for id in &live {
            self.append(Transition {
                id: id.clone(),
                at: now_ms(),
                change: Change::Finish {
                    status: JobStatus::Interrupted,
                    detail: Some("the harness stopped while this job was live".into()),
                    output: None,
                },
            })?;
        }
        Ok(live.len())
    }

    /// Record a new job, and answer it.
    ///
    /// `id` may be `None` for one this store mints, which is what an ordinary
    /// caller wants; a caller that names its own is telling the store the id
    /// means something outside it, so a duplicate is refused rather than
    /// reopened.
    pub fn queue(
        &self,
        id: Option<&str>,
        kind: &str,
        label: &str,
        session: Option<&str>,
    ) -> Result<Job, JobError> {
        check_name("kind", kind)?;
        let id = match id {
            Some(id) => {
                check_name("job id", id)?;
                if self.state.lock().expect("jobs").jobs.contains_key(id) {
                    return Err(JobError::Duplicate { id: id.to_string() });
                }
                id.to_string()
            }
            None => self.mint(kind),
        };
        self.append(Transition {
            id: id.clone(),
            at: now_ms(),
            change: Change::Queue {
                kind: kind.to_string(),
                label: label.to_string(),
                session: session.map(str::to_string),
            },
        })?;
        self.get(&id).ok_or(JobError::NoSuchJob(id))
    }

    /// Mark a job started.
    pub fn start(&self, id: &str) -> Result<Job, JobError> {
        match self.get(id) {
            None => return Err(JobError::NoSuchJob(id.to_string())),
            Some(job) if job.status != JobStatus::Queued => {
                return Err(JobError::NotStartable {
                    id: id.to_string(),
                    status: job.status,
                })
            }
            Some(_) => {}
        }
        self.append(Transition {
            id: id.to_string(),
            at: now_ms(),
            change: Change::Start,
        })?;
        self.get(id).ok_or(JobError::NoSuchJob(id.to_string()))
    }

    /// Settle a job. A job that has already ended cannot end again.
    pub fn finish(
        &self,
        id: &str,
        status: JobStatus,
        detail: Option<&str>,
        output: Option<&str>,
    ) -> Result<Job, JobError> {
        match self.get(id) {
            None => return Err(JobError::NoSuchJob(id.to_string())),
            Some(job) if job.status.is_terminal() => {
                return Err(JobError::AlreadyEnded {
                    id: id.to_string(),
                    status: job.status,
                })
            }
            Some(_) => {}
        }
        self.append(Transition {
            id: id.to_string(),
            at: now_ms(),
            change: Change::Finish {
                status,
                detail: detail.map(str::to_string),
                output: output.map(str::to_string),
            },
        })?;
        self.get(id).ok_or(JobError::NoSuchJob(id.to_string()))
    }

    /// One job, as it stands.
    pub fn get(&self, id: &str) -> Option<Job> {
        self.state.lock().expect("jobs").jobs.get(id).cloned()
    }

    /// Every job, oldest first.
    pub fn list(&self) -> Vec<Job> {
        let state = self.state.lock().expect("jobs");
        let mut jobs: Vec<Job> = state.jobs.values().cloned().collect();
        jobs.sort_by(|left, right| {
            left.queued_at
                .cmp(&right.queued_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        jobs
    }

    /// Every job that has not settled.
    pub fn live(&self) -> Vec<Job> {
        self.list().into_iter().filter(Job::is_live).collect()
    }

    /// Every job a session asked for.
    pub fn owned_by(&self, session: &str) -> Vec<Job> {
        self.list()
            .into_iter()
            .filter(|job| job.session.as_deref() == Some(session))
            .collect()
    }

    /// Append one transition and fold it into the held state.
    ///
    /// The fold happens under the same lock as the write and only after the
    /// write succeeded, so memory never gets ahead of the disk: a caller that
    /// ignores an error still reads what a fresh open would read.
    fn append(&self, transition: Transition) -> Result<(), JobError> {
        let mut state = self.state.lock().expect("jobs");
        let line = serde_json::to_string(&transition).map_err(|source| JobError::Unwritable {
            path: self.path.clone(),
            source: std::io::Error::other(source),
        })?;
        let written = writeln!(state.file, "{line}").and_then(|()| state.file.sync_data());
        written.map_err(|source| JobError::Unwritable {
            path: self.path.clone(),
            source,
        })?;
        fold(&mut state.jobs, transition);
        Ok(())
    }

    fn mint(&self, kind: &str) -> String {
        let mut state = self.state.lock().expect("jobs");
        loop {
            state.minted += 1;
            let id = format!("{kind}-{}", state.minted);
            if !state.jobs.contains_key(&id) {
                return id;
            }
        }
    }
}

/// Fold one transition into the held jobs.
///
/// A transition naming a job no `Queue` ever introduced is dropped rather than
/// inventing one: the fields a job needs live on the `Queue`, so a job
/// synthesized here would have to make its kind and label up.
fn fold(jobs: &mut BTreeMap<String, Job>, transition: Transition) {
    match transition.change {
        Change::Queue {
            kind,
            label,
            session,
        } => {
            jobs.entry(transition.id.clone()).or_insert(Job {
                id: transition.id,
                kind,
                label,
                session,
                status: JobStatus::Queued,
                detail: None,
                output: None,
                queued_at: transition.at,
                started_at: None,
                finished_at: None,
            });
        }
        Change::Start => {
            if let Some(job) = jobs.get_mut(&transition.id) {
                job.status = JobStatus::Running;
                job.started_at = Some(transition.at);
            }
        }
        Change::Finish {
            status,
            detail,
            output,
        } => {
            if let Some(job) = jobs.get_mut(&transition.id) {
                job.status = status;
                job.finished_at = Some(transition.at);
                if detail.is_some() {
                    job.detail = detail;
                }
                if output.is_some() {
                    job.output = output;
                }
            }
        }
    }
}

/// What one pass over a job journal found.
#[derive(Default)]
struct Scan {
    jobs: BTreeMap<String, Job>,
    /// Bytes that end on a record boundary.
    committed: u64,
}

/// Read every committed transition, the way the session journal is read.
///
/// The newline is the commit, so the only record a crash can cut short is the
/// last one, and it is cut short exactly when the file does not end in one.
/// That tail is dropped: no caller was ever told it was durable. Any other
/// unparsable line is refused, because the writer finished it.
fn scan(path: &Path) -> Result<Scan, JobError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Scan::default()),
        Err(source) => {
            return Err(JobError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let mut found = Scan::default();
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let Some(record) = line.strip_suffix(b"\n") else {
            break;
        };
        found.committed += line.len() as u64;
        let corrupt = || JobError::Corrupt {
            path: path.to_path_buf(),
            line: index + 1,
        };
        let text = std::str::from_utf8(record).map_err(|_| corrupt())?.trim();
        if text.is_empty() {
            continue;
        }
        fold(
            &mut found.jobs,
            serde_json::from_str(text).map_err(|_| corrupt())?,
        );
    }
    Ok(found)
}

/// The character set that is safe in an id, a file name and a log line at
/// once, so a name never has to be escaped differently depending on where it
/// is shown.
fn check_name(what: &'static str, value: &str) -> Result<(), JobError> {
    let shaped = (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    match shaped {
        true => Ok(()),
        false => Err(JobError::BadName {
            what,
            value: value.to_string(),
        }),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
