//! Commands that outlive the call that started them.
//!
//! A one-shot `shell` call runs a command and answers with its output, and a
//! command that takes an hour is one a model cannot start that way: the call
//! blocks the step, and the turn's interrupt sweeps the process group when the
//! call returns. Backgrounding it means three things this module holds
//! together - the work is recorded where a later call can find it, the output
//! goes somewhere durable while it is produced, and the process is not swept
//! by the interrupt belonging to the turn that happened to start it.
//!
//! **Where the pieces live, and why they are not here.** The record is a job in
//! [`tetanus_core::jobs`], which the `workflow/*`, `schedule/*`, `jobs/*` row
//! owns and this row deliberately did not rebuild. The output is a spill
//! artifact from [`tetanus_core::spill`], filed beside the session's own
//! journal. This module is the join, and the convention that joins them is
//! published as contract section 4.3.6 rather than left here, because it is a
//! convention over another row's type: the store records *transitions*, not
//! output, so the artifact's path rides on the job's `detail` as JSON, by
//! convention and not by type. That section is marked provisional; if the jobs
//! row replaces it, this module changes with it.

use std::sync::{Arc, Mutex};

use tetanus_core::jobs::{JobStatus, JobStore};
use tetanus_core::spill::{SpillSource, SpillStore, SpillWriter};

use crate::proc::{Chunk, OutputSink};

/// The `kind` every job this module queues carries.
pub const JOB_KIND: &str = "shell";

/// What a composition must have before a command may be backgrounded.
///
/// Both halves are required and neither is invented when it is missing: a
/// backgrounded command with no store is work nothing can find again, and one
/// with no artifact is output that exists only while the process holds it. A
/// call that asks for a background run without these is refused by name rather
/// than quietly run in the foreground, because a tool that answers a different
/// question than the one it was asked is worse than one that refuses.
#[derive(Clone)]
pub struct BackgroundTo {
    /// Where the transitions are recorded.
    pub jobs: Arc<JobStore>,
    /// Where the output is written while the command runs.
    pub spill: Arc<SpillStore>,
    /// The session both are scoped to.
    pub session: String,
}

/// The `detail` a finished job carries: contract section 4.3.6's JSON object.
///
/// One key is fixed by that section, `artifact`. The others are additions a
/// reader ignores if it does not know them, which is why this is an object and
/// not the bare path it could have been.
pub fn detail(artifact: &str, code: Option<i32>, signal: Option<&str>) -> String {
    let mut object = serde_json::json!({ "artifact": artifact });
    if let Some(code) = code {
        object["exit"] = serde_json::json!(code);
    }
    if let Some(signal) = signal {
        object["signal"] = serde_json::json!(signal);
    }
    object.to_string()
}

/// Read the artifact path back out of a job's `detail`.
///
/// Absence is an answer, not a fault: `detail` is a free string on a type this
/// row does not own, so a job queued by something else carries something else
/// there, and a reader that faulted on it would break the first time the jobs
/// row used its own store for its own work.
pub fn artifact_of(detail: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(detail?).ok()?;
    Some(value.get("artifact")?.as_str()?.to_string())
}

/// A sink that writes every chunk into a spill artifact as it arrives.
///
/// The artifact is opened when the command starts rather than when a bound is
/// exceeded, which is the one place this differs from the truncation artifact
/// the same store serves: a backgrounded command's output has no other home,
/// so there is no tail to fall back on.
pub struct ArtifactSink {
    writer: Mutex<Option<SpillWriter>>,
}

impl ArtifactSink {
    /// Open the artifact for `id` under `to`.
    pub fn open(to: &BackgroundTo, id: &str) -> Result<Self, tetanus_core::spill::SpillError> {
        let writer = to.spill.open(&SpillSource {
            session_id: to.session.clone(),
            tool: JOB_KIND.to_string(),
            call_id: id.to_string(),
        })?;
        Ok(Self {
            writer: Mutex::new(Some(writer)),
        })
    }

    /// Where this artifact is, for the answer that names it.
    pub fn locator(&self) -> String {
        self.writer
            .lock()
            .expect("no panic holds this lock")
            .as_ref()
            .map(SpillWriter::locator)
            .unwrap_or_default()
    }

    /// Close the artifact. A writer that cannot be closed still leaves the
    /// bytes it wrote on disk, so the locator stays the answer either way.
    pub fn finish(&self) {
        let taken = self.writer.lock().expect("no panic holds this lock").take();
        if let Some(writer) = taken {
            let _ = writer.finish();
        }
    }
}

impl OutputSink for ArtifactSink {
    fn chunk(&self, chunk: Chunk) {
        let mut guard = self.writer.lock().expect("no panic holds this lock");
        if let Some(writer) = guard.as_mut() {
            let _ = writer.write(chunk.text.as_bytes());
        }
    }
}

/// What a collection call answers with.
pub struct Collected {
    pub status: JobStatus,
    pub label: String,
    pub artifact: Option<String>,
    pub text: String,
}

/// Read what a job has produced so far, whether or not it has finished.
///
/// A running job is readable for the reason a running command is watchable:
/// the question "what has the build printed" has an answer before the build is
/// over, and a collection that only worked at the end would make a model wait
/// for the thing it backgrounded to avoid waiting for.
///
/// `live` is the artifact path the calling process knows for a job it started
/// and that has not finished. It is a parameter rather than a lookup because
/// of a fact about the store this row does not own: `queue` and `start` take
/// no detail, so until `finish` writes one there is nowhere on the record to
/// put the path. A process therefore knows its own live jobs' artifacts and
/// the record knows every terminal one's, which is what contract section 4.3.6
/// says and the reason it says it rather than leaving a reader to find out by
/// collecting a running job and getting nothing.
pub fn collect(store: &JobStore, id: &str, tail: usize, live: Option<String>) -> Option<Collected> {
    let job = store.get(id)?;
    let artifact = artifact_of(job.detail.as_deref()).or(live);
    let text = match &artifact {
        Some(path) => read_tail(path, tail),
        None => String::new(),
    };
    Some(Collected {
        status: job.status,
        label: job.label,
        artifact,
        text,
    })
}

/// The last `tail` bytes of an artifact, on a character boundary.
///
/// Bounded for the reason every other output in this crate is bounded: an
/// artifact holds the whole stream on purpose, and handing the whole of it to a
/// model is how one backgrounded `yes` fills a context window.
fn read_tail(path: &str, tail: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    if bytes.len() <= tail {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let mut start = bytes.len() - tail;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}
