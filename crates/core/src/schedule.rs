//! Work that is due at a time rather than when somebody asks: one-shot
//! reminders and fixed-rate recurrences, durable across a restart.
//!
//! **The clock is an argument, never a global.** Every decision this module
//! makes is a function of a `now` its caller supplies. That is what lets the
//! whole of it be tested by moving time rather than by sleeping, and it is
//! also what makes a restart honest: a process that comes back up asks "what
//! is due *now*", and gets the same answer the process that died would have
//! given at the same instant.
//!
//! **A missed recurrence fires once, and realigns to its anchor.** A harness
//! that was down for a day does not owe a day of hourly reminders; it owes
//! one, and then the next on the original grid. Catching up would flood a
//! session with stale work the moment it came back, and drifting the anchor
//! would mean a job set for the top of the hour slowly wandered off it. This
//! is upstream's rule too, stated there as "advances directly past missed
//! occurrences".
//!
//! **A fire that lands on a run still going has an explicit answer.** There is
//! no defensible default, so [`OverlapPolicy`] makes the caller say: skip the
//! fire, hold it until the run ends, or let the two overlap. What is *not*
//! offered is the accidental behaviour - firing anyway and letting two copies
//! of the same work race - which is what a scheduler with no opinion does.
//!
//! **Durability is the journal discipline the job store already uses.** An
//! append-only log of changes, the newline as the commit, a torn tail dropped
//! and a damaged committed line refused. The state is a fold, so a schedule's
//! next occurrence is re-derived rather than stored twice.
//!
//! Parity: upstream `packages/schedule/schedule`, pinned by its
//! `recurrence.spec.ts`, `runtime.spec.ts`, `domain.spec.ts` and
//! `jsonl-restart.spec.ts`. Upstream's reminders are delivered as user
//! messages into the session that created them, and its `session-local`
//! delivery mode is the only one it has; tetanus keeps the payload opaque and
//! lets the caller decide what a fire means, because the same seam has to
//! carry a workflow step and a reminder. Its local-calendar and IANA time-zone
//! input is a parsing surface this workspace has no dependency for: a target
//! arrives here as an instant.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The shortest recurrence this store accepts, in milliseconds.
///
/// Upstream's floor is five minutes. The reason to have one at all is that a
/// recurrence shorter than the work it triggers is a queue that only grows,
/// and the failure shows up as a harness that is always busy rather than as an
/// error anyone can read.
pub const MIN_INTERVAL_MS: u64 = 60_000;

/// When a schedule is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "lowercase")]
pub enum ScheduleRule {
    /// Once, at an instant. A delay is this, resolved against the clock when
    /// the schedule was created - so a restart cannot restart the countdown.
    At { at_ms: u64 },
    /// Every `interval_ms`, on a grid anchored at `anchor_ms`.
    Every { interval_ms: u64, anchor_ms: u64 },
}

/// What to do when a schedule comes due while its previous run is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlapPolicy {
    /// Drop the fire and wait for the next one. The default, because for the
    /// work a harness schedules - a check, a sweep, a reminder - a late
    /// duplicate is worth less than the one already running.
    #[default]
    Skip,
    /// Hold the fire and deliver it when the run ends. At most one is held: a
    /// backlog of identical work is the thing a scheduler must not build.
    Queue,
    /// Let them overlap. For work that is genuinely independent per fire.
    Concurrent,
}

/// One durable schedule, as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    /// A one-line description, for a human or a model.
    pub label: String,
    /// What the fire means. Opaque here: this module decides *when*, and the
    /// caller decides what to do about it.
    pub payload: String,
    pub rule: ScheduleRule,
    pub overlap: OverlapPolicy,
    /// The session that owns this, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The next instant this is due. Re-derived from the rule on every
    /// dispatch, never drifted.
    pub scheduled_at: u64,
    pub created_at: u64,
    /// How many times this has fired.
    pub dispatches: u64,
    /// When it last fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dispatch: Option<u64>,
    /// A fire held by [`OverlapPolicy::Queue`], waiting for the run to end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<u64>,
    /// False once a one-shot has fired, or once it is deleted.
    pub active: bool,
}

/// What one poll decided about one schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    pub id: String,
    /// The occurrence this fire is for - the grid instant, not the instant the
    /// poll happened, so a late poll still reports the time the work was owed.
    pub occurrence_at: u64,
    pub decision: Decision,
}

/// What the overlap policy said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Run it now.
    Run,
    /// Dropped because the previous run was still going.
    Skipped,
    /// Held until the previous run ends.
    Held,
    /// A held fire, released because the run ended.
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Change {
    id: String,
    at: u64,
    #[serde(flatten)]
    op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Op {
    Create {
        label: String,
        payload: String,
        rule: ScheduleRule,
        overlap: OverlapPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        scheduled_at: u64,
    },
    /// One decided occurrence. `accepted_at` is the poll's clock reading,
    /// which is what advances a recurrence past everything it missed.
    Dispatch {
        accepted_at: u64,
        decision: Decision,
    },
    Delete,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
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
    #[error("{}: line {line} is not a schedule change", path.display())]
    Corrupt { path: PathBuf, line: usize },
    #[error("no schedule {0:?}")]
    NoSuchSchedule(String),
    #[error("schedule {0:?} already exists")]
    Duplicate(String),
    #[error("a schedule needs something to say: its payload is empty")]
    EmptyPayload,
    #[error("an interval of {0}ms is below the {MIN_INTERVAL_MS}ms floor")]
    IntervalTooShort(u64),
    #[error("a one-shot target must be in the future: {at_ms} is not after {now_ms}")]
    NotFuture { at_ms: u64, now_ms: u64 },
    #[error("schedule id {0:?} must be 1 to 128 characters of [A-Za-z0-9._-]")]
    BadId(String),
}

/// The durable set of schedules.
#[derive(Debug)]
pub struct ScheduleStore {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    file: std::fs::File,
    schedules: BTreeMap<String, Schedule>,
    minted: u64,
}

impl ScheduleStore {
    /// Open the store at `path`.
    ///
    /// There is no repair pass here, and that is the design rather than an
    /// omission: a schedule's whole state is its rule and its dispatch
    /// history, so a process that died mid-poll left nothing half-done. What
    /// it may have left is work that is now overdue, and [`Self::poll`]
    /// answers that from the clock.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ScheduleError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|source| ScheduleError::Unwritable {
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
            .map_err(|source| ScheduleError::Unwritable {
                path: path.clone(),
                source,
            })?;
        let length = file
            .metadata()
            .map_err(|source| ScheduleError::Unreadable {
                path: path.clone(),
                source,
            })?
            .len();
        if scan.committed < length {
            file.set_len(scan.committed)
                .and_then(|()| file.sync_all())
                .map_err(|source| ScheduleError::Unwritable {
                    path: path.clone(),
                    source,
                })?;
        }
        Ok(Self {
            path,
            state: Mutex::new(State {
                file,
                schedules: scan.schedules,
                minted: 0,
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record a schedule. `now_ms` is the clock this creation is judged
    /// against - a one-shot must be in the future by it.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        id: Option<&str>,
        label: &str,
        payload: &str,
        rule: ScheduleRule,
        overlap: OverlapPolicy,
        session: Option<&str>,
        now_ms: u64,
    ) -> Result<Schedule, ScheduleError> {
        if payload.trim().is_empty() {
            return Err(ScheduleError::EmptyPayload);
        }
        let scheduled_at = match rule {
            ScheduleRule::At { at_ms } => {
                if at_ms <= now_ms {
                    return Err(ScheduleError::NotFuture { at_ms, now_ms });
                }
                at_ms
            }
            ScheduleRule::Every {
                interval_ms,
                anchor_ms,
            } => {
                if interval_ms < MIN_INTERVAL_MS {
                    return Err(ScheduleError::IntervalTooShort(interval_ms));
                }
                // The first occurrence on the grid that has not already passed,
                // so a recurrence anchored in the past does not arrive owing a
                // backlog the moment it is created.
                next_on_grid(anchor_ms, interval_ms, now_ms)
            }
        };

        let id = match id {
            Some(id) => {
                check_id(id)?;
                if self
                    .state
                    .lock()
                    .expect("schedules")
                    .schedules
                    .contains_key(id)
                {
                    return Err(ScheduleError::Duplicate(id.to_string()));
                }
                id.to_string()
            }
            None => self.mint(),
        };
        self.append(Change {
            id: id.clone(),
            at: now_ms,
            op: Op::Create {
                label: label.to_string(),
                payload: payload.to_string(),
                rule,
                overlap,
                session: session.map(str::to_string),
                scheduled_at,
            },
        })?;
        self.get(&id).ok_or(ScheduleError::NoSuchSchedule(id))
    }

    /// Stop a schedule. A schedule that is already inactive is not an error.
    pub fn delete(&self, id: &str, now_ms: u64) -> Result<bool, ScheduleError> {
        let Some(schedule) = self.get(id) else {
            return Err(ScheduleError::NoSuchSchedule(id.to_string()));
        };
        if !schedule.active {
            return Ok(false);
        }
        self.append(Change {
            id: id.to_string(),
            at: now_ms,
            op: Op::Delete,
        })?;
        Ok(true)
    }

    /// Which schedules are due at `now_ms`, without deciding or recording
    /// anything. The read a caller uses to plan.
    pub fn due(&self, now_ms: u64) -> Vec<Schedule> {
        self.state
            .lock()
            .expect("schedules")
            .schedules
            .values()
            .filter(|schedule| schedule.active && schedule.scheduled_at <= now_ms)
            .cloned()
            .collect()
    }

    /// The next instant anything is due, or `None` when nothing is scheduled.
    /// What a runner sleeps until, instead of polling in a loop.
    pub fn next_wake(&self) -> Option<u64> {
        self.state
            .lock()
            .expect("schedules")
            .schedules
            .values()
            .filter(|schedule| schedule.active)
            .map(|schedule| schedule.scheduled_at)
            .min()
    }

    /// Decide every due schedule at `now_ms`, recording each decision, and
    /// answer what the caller should run.
    ///
    /// `running` is the set of schedule ids whose previous fire has not
    /// finished. It is supplied rather than tracked here because only the
    /// caller knows when its own work ended, and a scheduler that guessed
    /// would be wrong in exactly the case the overlap policy exists for.
    ///
    /// A held fire is released first, before the schedule's own next
    /// occurrence is considered, so the work happens in the order it came due.
    pub fn poll(
        &self,
        now_ms: u64,
        running: &BTreeSet<String>,
    ) -> Result<Vec<Fire>, ScheduleError> {
        let mut fired = Vec::new();

        // A held fire whose run has ended is released, whatever the clock says
        // about the next occurrence.
        for schedule in self.list() {
            let Some(held) = schedule.held else { continue };
            if running.contains(&schedule.id) {
                continue;
            }
            self.append(Change {
                id: schedule.id.clone(),
                at: now_ms,
                op: Op::Dispatch {
                    accepted_at: now_ms,
                    decision: Decision::Released,
                },
            })?;
            fired.push(Fire {
                id: schedule.id,
                occurrence_at: held,
                decision: Decision::Released,
            });
        }

        let mut due = self.due(now_ms);
        // Oldest occurrence first, then by id, so a poll that finds several
        // due is deterministic rather than dependent on map order.
        due.sort_by(|left, right| {
            left.scheduled_at
                .cmp(&right.scheduled_at)
                .then_with(|| left.id.cmp(&right.id))
        });

        for schedule in due {
            let busy = running.contains(&schedule.id);
            let decision = match (busy, schedule.overlap) {
                (false, _) | (true, OverlapPolicy::Concurrent) => Decision::Run,
                (true, OverlapPolicy::Skip) => Decision::Skipped,
                // At most one held fire: a second one collapses into the first
                // rather than building the backlog this policy exists to avoid.
                (true, OverlapPolicy::Queue) if schedule.held.is_some() => Decision::Skipped,
                (true, OverlapPolicy::Queue) => Decision::Held,
            };
            let occurrence_at = occurrence_of(&schedule, now_ms);
            self.append(Change {
                id: schedule.id.clone(),
                at: now_ms,
                op: Op::Dispatch {
                    accepted_at: now_ms,
                    decision,
                },
            })?;
            fired.push(Fire {
                id: schedule.id,
                occurrence_at,
                decision,
            });
        }
        Ok(fired)
    }

    pub fn get(&self, id: &str) -> Option<Schedule> {
        self.state
            .lock()
            .expect("schedules")
            .schedules
            .get(id)
            .cloned()
    }

    /// Every schedule this store holds, by id.
    pub fn list(&self) -> Vec<Schedule> {
        self.state
            .lock()
            .expect("schedules")
            .schedules
            .values()
            .cloned()
            .collect()
    }

    /// Every schedule that is still due to fire again.
    pub fn active(&self) -> Vec<Schedule> {
        self.list()
            .into_iter()
            .filter(|schedule| schedule.active)
            .collect()
    }

    fn append(&self, change: Change) -> Result<(), ScheduleError> {
        let mut state = self.state.lock().expect("schedules");
        let line = serde_json::to_string(&change).map_err(|source| ScheduleError::Unwritable {
            path: self.path.clone(),
            source: std::io::Error::other(source),
        })?;
        writeln!(state.file, "{line}")
            .and_then(|()| state.file.sync_data())
            .map_err(|source| ScheduleError::Unwritable {
                path: self.path.clone(),
                source,
            })?;
        fold(&mut state.schedules, change);
        Ok(())
    }

    fn mint(&self) -> String {
        let mut state = self.state.lock().expect("schedules");
        loop {
            state.minted += 1;
            let id = format!("schedule-{}", state.minted);
            if !state.schedules.contains_key(&id) {
                return id;
            }
        }
    }
}

/// The occurrence a fire at `now` is for: the grid instant it was owed at,
/// not the instant the poll noticed.
fn occurrence_of(schedule: &Schedule, now_ms: u64) -> u64 {
    match schedule.rule {
        ScheduleRule::At { at_ms } => at_ms,
        ScheduleRule::Every {
            interval_ms,
            anchor_ms,
        } => latest_on_grid(anchor_ms, interval_ms, now_ms).unwrap_or(schedule.scheduled_at),
    }
}

/// The first grid instant strictly after `now`.
fn next_on_grid(anchor_ms: u64, interval_ms: u64, now_ms: u64) -> u64 {
    if anchor_ms > now_ms {
        return anchor_ms;
    }
    let elapsed = now_ms - anchor_ms;
    // `+ 1` because a grid instant exactly at `now` has already come due.
    anchor_ms + (elapsed / interval_ms + 1) * interval_ms
}

/// The latest grid instant at or before `now`, or `None` when the grid has not
/// started yet.
fn latest_on_grid(anchor_ms: u64, interval_ms: u64, now_ms: u64) -> Option<u64> {
    if anchor_ms > now_ms {
        return None;
    }
    Some(anchor_ms + ((now_ms - anchor_ms) / interval_ms) * interval_ms)
}

/// Fold one change into the held schedules.
fn fold(schedules: &mut BTreeMap<String, Schedule>, change: Change) {
    match change.op {
        Op::Create {
            label,
            payload,
            rule,
            overlap,
            session,
            scheduled_at,
        } => {
            schedules.entry(change.id.clone()).or_insert(Schedule {
                id: change.id,
                label,
                payload,
                rule,
                overlap,
                session,
                scheduled_at,
                created_at: change.at,
                dispatches: 0,
                last_dispatch: None,
                held: None,
                active: true,
            });
        }
        Op::Delete => {
            if let Some(schedule) = schedules.get_mut(&change.id) {
                schedule.active = false;
                schedule.held = None;
            }
        }
        Op::Dispatch {
            accepted_at,
            decision,
        } => {
            let Some(schedule) = schedules.get_mut(&change.id) else {
                return;
            };
            match decision {
                Decision::Released => {
                    schedule.held = None;
                    schedule.dispatches += 1;
                    schedule.last_dispatch = Some(accepted_at);
                    return;
                }
                Decision::Held => {
                    schedule.held = Some(occurrence_of(schedule, accepted_at));
                }
                Decision::Run => {
                    schedule.dispatches += 1;
                    schedule.last_dispatch = Some(accepted_at);
                }
                Decision::Skipped => {}
            }
            // Every decided occurrence advances the schedule, including a
            // skipped one: the occurrence was decided, and leaving it due
            // would make the next poll decide it again for ever.
            match schedule.rule {
                ScheduleRule::At { .. } => {
                    schedule.active = false;
                }
                ScheduleRule::Every {
                    interval_ms,
                    anchor_ms,
                } => {
                    // Straight to the next occurrence after now, so a harness
                    // that was down for a day owes one fire and not a day of
                    // them - and the grid is the original anchor's, so a
                    // schedule set for the top of the hour stays there.
                    schedule.scheduled_at = next_on_grid(anchor_ms, interval_ms, accepted_at);
                }
            }
        }
    }
}

#[derive(Default)]
struct Scan {
    schedules: BTreeMap<String, Schedule>,
    committed: u64,
}

/// Read every committed change, with the job journal's crash rules.
fn scan(path: &Path) -> Result<Scan, ScheduleError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Scan::default()),
        Err(source) => {
            return Err(ScheduleError::Unreadable {
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
        let corrupt = || ScheduleError::Corrupt {
            path: path.to_path_buf(),
            line: index + 1,
        };
        let text = std::str::from_utf8(record).map_err(|_| corrupt())?.trim();
        if text.is_empty() {
            continue;
        }
        fold(
            &mut found.schedules,
            serde_json::from_str(text).map_err(|_| corrupt())?,
        );
    }
    Ok(found)
}

fn check_id(id: &str) -> Result<(), ScheduleError> {
    let shaped = (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    match shaped {
        true => Ok(()),
        false => Err(ScheduleError::BadId(id.to_string())),
    }
}
