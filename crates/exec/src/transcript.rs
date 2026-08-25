//! Everything a long-lived process has printed, bounded, with a way to wait for
//! more.
//!
//! Shared by the two things that hold a process open across many calls: a
//! persistent shell reading a pipe ([`crate::session`]) and a terminal reading
//! a pseudo-terminal ([`crate::pty`]). One implementation because the hazard is
//! one hazard - a process that prints faster than anyone reads must cost
//! bounded memory, and what is dropped must be the beginning, because the end
//! is what someone is reading.
//!
//! Positions are absolute and survive a drop: a caller that noted where a
//! command started can still ask for everything since, and is told when the
//! bound ate part of it rather than being handed a shorter answer that looks
//! complete.

use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// Told what the bound is about to forget, and where it started.
///
/// The only moment the dropped bytes still exist is inside `push`, between the
/// buffer overflowing and the drain. A reader polling from outside always
/// arrives after they are gone - which is a mistake this crate made once and
/// caught in a case: an artifact meant to hold a command's whole output held
/// its last 1,915 lines of 3,000, because the writer was opened when the drop
/// was *noticed* rather than when it happened.
pub type Overflow = Arc<dyn Fn(usize, &str) + Send + Sync>;

/// Everything a shell has printed, bounded, with a way to wait for more.
pub struct Transcript {
    bound: usize,
    state: Mutex<TranscriptState>,
    /// Who to tell when the bound forgets something. `None` for the terminal
    /// transcript, which has a screen model beside it, and set for the length
    /// of one command by a session that is keeping the whole of it.
    overflow: Mutex<Option<Overflow>>,
    pub changed: Notify,
}

#[derive(Default)]
struct TranscriptState {
    kept: String,
    /// How many bytes the bound has dropped off the front, so a position in
    /// the transcript stays meaningful after a drop.
    dropped: usize,
}

/// The transcript as it stood at one moment.
pub struct Snapshot {
    kept: String,
    pub dropped: usize,
}

impl Snapshot {
    /// Everything from absolute position `from` onwards, as far as it is still
    /// retained.
    pub fn since(&self, from: usize) -> String {
        let start = from.saturating_sub(self.dropped).min(self.kept.len());
        // A position can land inside a character after a drop; the next
        // boundary is close enough and is always valid.
        let start = (start..=self.kept.len())
            .find(|at| self.kept.is_char_boundary(*at))
            .unwrap_or(self.kept.len());
        self.kept[start..].to_string()
    }

    /// Where the transcript ends, counted from its very beginning: an absolute
    /// position, not the size of what is retained. A caller marks a spot and
    /// asks for everything since, and that has to keep meaning the same thing
    /// after the bound has dropped something off the front.
    pub fn len(&self) -> usize {
        self.dropped + self.kept.len()
    }

    /// Whether the process has printed nothing at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The whole retained transcript.
    pub fn text(self) -> String {
        self.kept
    }
}

impl Transcript {
    pub fn new(bound: usize) -> Self {
        Self {
            bound,
            state: Mutex::new(TranscriptState::default()),
            overflow: Mutex::new(None),
            changed: Notify::new(),
        }
    }

    /// Hear about what the bound forgets, for as long as the handle is set.
    ///
    /// One at a time: a session installs one for the length of a command and
    /// takes it away afterwards, because what is dropped belongs to whichever
    /// command was running.
    pub fn watch_overflow(&self, sink: Option<Overflow>) {
        *self.overflow.lock().expect("no panic holds this lock") = sink;
    }

    pub fn push(&self, text: &str) {
        {
            let mut state = self.state.lock().expect("no panic holds this lock");
            state.kept.push_str(text);
            if state.kept.len() > self.bound {
                let excess = state.kept.len() - self.bound;
                let at = (excess..=state.kept.len())
                    .find(|at| state.kept.is_char_boundary(*at))
                    .unwrap_or(state.kept.len());
                // Told before it is forgotten, which is the only moment it
                // exists.
                if let Some(sink) = self
                    .overflow
                    .lock()
                    .expect("no panic holds this lock")
                    .as_ref()
                {
                    sink(state.dropped, &state.kept[..at]);
                }
                state.kept.drain(..at);
                state.dropped += at;
            }
        }
        self.changed.notify_waiters();
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().expect("no panic holds this lock");
        Snapshot {
            kept: state.kept.clone(),
            dropped: state.dropped,
        }
    }

    /// Where the transcript ends now - an absolute position, as on
    /// [`Snapshot::len`].
    pub fn len(&self) -> usize {
        let state = self.state.lock().expect("no panic holds this lock");
        state.dropped + state.kept.len()
    }

    /// Whether the process has printed nothing at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
