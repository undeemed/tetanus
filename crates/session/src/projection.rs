//! Derived views over a session log: a fold per domain, driven once and read
//! many times.
//!
//! A session log is the authority, and almost nothing a reader wants is a
//! single event. Token usage, context pressure, how many turns ran, what the
//! session should be called - each is a fold over the whole journal, and each
//! would otherwise be recomputed from scratch by whoever asked. A projection
//! is that fold, named, driven forward as events commit, and remembered.
//!
//! **A unit contributes mathematics and nothing else.** It says what the empty
//! log looks like, how one event changes its state, and how that state reads.
//! It holds no subscription, no cache and no clock: the registry owns the
//! driving and the watermark. That split is what lets a checkpoint exist at
//! all - a value is reproducible from the log alone, so a stored one is only
//! ever a shortcut and never an authority.
//!
//! **State is JSON on purpose**, not an opaque Rust value. It is the
//! precondition for persisting a checkpoint, and it keeps the rule that
//! anything a projection knows could have been recomputed rather than
//! remembered.
//!
//! **A stored value is a shortcut, never an authority.** `restore` discards a
//! row whose `state_version` does not match the unit that would use it, and a
//! row claiming to have folded events the log does not have. Forward-applying
//! a stale row would turn one bad checkpoint into a permanently wrong value
//! that no amount of new events corrects.
//!
//! Parity: upstream `packages/session/session-projection`, pinned by its
//! `registry.spec.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::SessionEvent;

/// The seq an empty log reflects, matching `SessionSubscribeResult.last_seq`
/// so a reader comparing the two never has to translate.
pub const EMPTY_LOG: i64 = -1;

/// One domain's fold over the session log.
///
/// All three operations are pure and synchronous. An asynchronous one would
/// let two readers see values folded to different points and call it a
/// snapshot, which is the one thing a consistent cut has to rule out.
pub trait Projection: Send + Sync {
    /// The name this unit's value is served under. Unique in a registry.
    fn key(&self) -> &str;

    /// Bump whenever the state's shape or the fold's meaning changes.
    ///
    /// It is the only thing that makes a persisted checkpoint safe: a row
    /// written by an older unit is discarded rather than folded forward into a
    /// value that is quietly wrong.
    fn state_version(&self) -> u32 {
        0
    }

    /// The state of the empty log.
    fn init(&self) -> Value;

    /// Fold one committed event. A unit uninterested in an event returns the
    /// state it was given.
    fn apply(&self, state: Value, event: &SessionEvent) -> Value;

    /// The state as a reader sees it.
    ///
    /// Separate from the state so a unit may keep whatever it needs to fold
    /// cheaply while serving something smaller.
    fn view(&self, state: &Value) -> Value;
}

/// One unit's persisted fold: what it had folded, how far, and which version
/// of the unit produced it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Checkpoint {
    /// The `state_version` of the unit that wrote this.
    pub ver: u32,
    /// Seq of the last event folded into `val`; [`EMPTY_LOG`] for none.
    pub seq: i64,
    /// The unit's own state.
    pub val: Value,
}

/// One consistent read across every registered unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Seq of the last event every value here reflects; [`EMPTY_LOG`] for an
    /// empty log. One number for the whole snapshot, because a reader
    /// comparing two keys folded to different points would be comparing two
    /// different sessions.
    pub as_of_seq: i64,
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("a projection is already registered under the key {0:?}")]
    DuplicateKey(String),
}

/// One unit's live fold, and how far it has been driven.
struct Cell {
    state: Value,
    seq: i64,
}

struct Registered {
    unit: Arc<dyn Projection>,
    cell: Cell,
}

/// Holds the registered units and drives them over one session's events.
///
/// One registry belongs to one session, which is what makes the watermark a
/// single number rather than a table. Upstream keys its cells by session
/// because one Cordis context serves every session at once; a tetanus session
/// already owns its own log and its own bus, so the containment is structural
/// here and needs no key.
#[derive(Default)]
pub struct Projections {
    units: Mutex<Vec<Registered>>,
}

impl Projections {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register one unit. Its key must be free.
    ///
    /// The unit starts at the empty log rather than at the current one, and
    /// [`drive`](Self::drive) catches it up. A unit registered after events
    /// have flowed therefore serves the same value as one registered before
    /// them, which is what makes registration order stop mattering.
    pub fn register(&self, unit: Arc<dyn Projection>) -> Result<(), ProjectionError> {
        let mut units = self.units.lock().expect("projections");
        if units.iter().any(|held| held.unit.key() == unit.key()) {
            return Err(ProjectionError::DuplicateKey(unit.key().to_string()));
        }
        let cell = Cell {
            state: unit.init(),
            seq: EMPTY_LOG,
        };
        units.push(Registered { unit, cell });
        Ok(())
    }

    /// Stop serving a key. Answers whether one was being served.
    pub fn remove(&self, key: &str) -> bool {
        let mut units = self.units.lock().expect("projections");
        let before = units.len();
        units.retain(|held| held.unit.key() != key);
        units.len() != before
    }

    pub fn keys(&self) -> Vec<String> {
        self.units
            .lock()
            .expect("projections")
            .iter()
            .map(|held| held.unit.key().to_string())
            .collect()
    }

    /// Fold every event a unit has not seen yet, and answer which keys changed.
    ///
    /// `events` is the whole log, and each unit takes only the tail past its
    /// own watermark, so calling this with the same log twice does nothing the
    /// second time. That is what makes it safe to drive on every append
    /// without tracking what was already delivered.
    ///
    /// A key whose value is unchanged is not reported. Upstream compares state
    /// by reference (`Object.is`), so a unit that rebuilds an equal state
    /// still notifies there; comparing by value here reports strictly fewer
    /// changes, and never a change that did not happen.
    pub fn drive(&self, events: &[SessionEvent]) -> Vec<String> {
        let mut units = self.units.lock().expect("projections");
        let mut changed = Vec::new();
        for held in units.iter_mut() {
            let from = (held.cell.seq + 1).max(0) as usize;
            let mut state = held.cell.state.clone();
            let before = held.unit.view(&state);
            for event in events.iter().skip(from) {
                state = held.unit.apply(state, event);
            }
            if let Some(last) = events.last() {
                held.cell.seq = last.seq as i64;
            }
            let after = held.unit.view(&state);
            held.cell.state = state;
            if after != before {
                changed.push(held.unit.key().to_string());
            }
        }
        changed
    }

    /// Every unit's current value, as one cut.
    ///
    /// `as_of_seq` is the lowest watermark across the units, so the snapshot
    /// never claims to reflect an event some value has not folded. With every
    /// unit driven by the same call they agree, and the minimum is only
    /// interesting for a unit registered since the last drive.
    pub fn snapshot(&self) -> Snapshot {
        let units = self.units.lock().expect("projections");
        let as_of_seq = units
            .iter()
            .map(|held| held.cell.seq)
            .min()
            .unwrap_or(EMPTY_LOG);
        let values = units
            .iter()
            .map(|held| {
                (
                    held.unit.key().to_string(),
                    held.unit.view(&held.cell.state),
                )
            })
            .collect();
        Snapshot { as_of_seq, values }
    }

    /// One key's current value, without building the whole cut.
    pub fn value(&self, key: &str) -> Option<Value> {
        let units = self.units.lock().expect("projections");
        units
            .iter()
            .find(|held| held.unit.key() == key)
            .map(|held| held.unit.view(&held.cell.state))
    }

    /// Every unit's fold, for storing.
    ///
    /// The states are cloned out, so a caller that keeps or edits them cannot
    /// reach back into the live cells. A checkpoint that shared its state with
    /// the cache would let a consumer corrupt a value nobody asked it to
    /// touch.
    pub fn checkpoint(&self) -> BTreeMap<String, Checkpoint> {
        self.units
            .lock()
            .expect("projections")
            .iter()
            .map(|held| {
                (
                    held.unit.key().to_string(),
                    Checkpoint {
                        ver: held.unit.state_version(),
                        seq: held.cell.seq,
                        val: held.cell.state.clone(),
                    },
                )
            })
            .collect()
    }

    /// Adopt what stored rows can be trusted, then fold the rest of the log.
    ///
    /// A row is used only when it was written by this unit's own
    /// `state_version` and does not claim events the supplied log lacks.
    /// Everything else refolds from `init`, which is always correct because
    /// the log is the authority and the row was only ever a shortcut. That is
    /// the whole safety argument for persisting these at all.
    ///
    /// Answers the keys whose value changed, as [`drive`](Self::drive) does.
    pub fn restore(
        &self,
        stored: &BTreeMap<String, Checkpoint>,
        events: &[SessionEvent],
    ) -> Vec<String> {
        let last_seq = events.last().map(|e| e.seq as i64).unwrap_or(EMPTY_LOG);
        {
            let mut units = self.units.lock().expect("projections");
            for held in units.iter_mut() {
                let usable = stored.get(held.unit.key()).filter(|row| {
                    row.ver == held.unit.state_version()
                        && row.seq >= EMPTY_LOG
                        // A row from a longer log than the one in hand
                        // describes a history this caller cannot show; the
                        // log may have been truncated or replaced, and
                        // folding the tail onto it would mix two of them.
                        && row.seq <= last_seq
                });
                match usable {
                    Some(row) => {
                        held.cell.state = row.val.clone();
                        held.cell.seq = row.seq;
                    }
                    None => {
                        held.cell.state = held.unit.init();
                        held.cell.seq = EMPTY_LOG;
                    }
                }
            }
        }
        self.drive(events)
    }
}
