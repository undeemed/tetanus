//! Test Design Specification: time-triggered work, ported.
//!
//! Features under test: `tetanus_core::schedule` - when a schedule is due,
//! what a missed recurrence owes, what happens to a fire that lands on a run
//! still going, and what survives a restart. Upstream pins the same behaviour
//! in `packages/schedule/schedule/tests/{recurrence,runtime,domain,
//! jsonl-restart}.spec.ts`.
//!
//! Approach: the clock is an argument to every call, so every case *moves*
//! time instead of waiting for it. No case sleeps, and none is timing
//! dependent: a schedule due in an hour is tested by asking what is due an
//! hour later. A restart is modelled by dropping the store and opening the
//! same file, so the second store learns only from disk.
//!
//! What is not restated, and why. Upstream delivers a reminder as a user
//! message into the session that created it and has one delivery mode; tetanus
//! keeps the payload opaque, because the same seam carries a workflow step and
//! a reminder, so its delivery-framing and session-liveness cases have nothing
//! to restate. Its local-calendar and IANA time-zone input is a parsing
//! surface this workspace has no dependency for - a target arrives here as an
//! instant - so its zone-validation cases have no counterpart.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeSet;

use tetanus_core::schedule::{
    Decision, OverlapPolicy, ScheduleError, ScheduleRule, ScheduleStore, MIN_INTERVAL_MS,
};

/// A fixed instant to hang every case off, so the numbers in a case are
/// readable offsets rather than epoch milliseconds.
const T0: u64 = 1_700_000_000_000;
const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;

fn store() -> (ScheduleStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = ScheduleStore::open(dir.path().join("schedules.jsonl")).expect("open");
    (store, dir)
}

fn nobody() -> BTreeSet<String> {
    BTreeSet::new()
}

fn running(id: &str) -> BTreeSet<String> {
    BTreeSet::from([id.to_string()])
}

/// TC-PORT-SCHED-1: a one-shot is not due before its instant, is due at it,
/// and fires once.
///
/// Upstream: `runtime.spec.ts`, "dispatches a one-shot at its target".
///
/// Expected: nothing due a millisecond early; one fire at the target; the
/// schedule inactive afterwards, so a later poll finds nothing.
#[test]
fn a_one_shot_fires_once_at_its_instant() {
    let (store, _dir) = store();
    store
        .create(
            Some("remind"),
            "check the build",
            "look at CI",
            ScheduleRule::At { at_ms: T0 + HOUR },
            OverlapPolicy::Skip,
            None,
            T0,
        )
        .expect("create");

    assert!(store.due(T0 + HOUR - 1).is_empty());
    assert_eq!(store.next_wake(), Some(T0 + HOUR));

    let fired = store.poll(T0 + HOUR, &nobody()).expect("poll");
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].id, "remind");
    assert_eq!(fired[0].decision, Decision::Run);
    assert_eq!(fired[0].occurrence_at, T0 + HOUR);

    let after = store.get("remind").expect("still recorded");
    assert!(!after.active, "a one-shot does not fire twice");
    assert_eq!(after.dispatches, 1);
    assert!(store.poll(T0 + 2 * HOUR, &nobody()).unwrap().is_empty());
    assert_eq!(store.next_wake(), None);
}

/// TC-PORT-SCHED-2: a recurrence fires on its anchor grid.
///
/// Upstream: `recurrence.spec.ts`, "keeps the next target creation-anchor
/// aligned".
///
/// Expected: successive fires land exactly one interval apart, on the grid the
/// anchor set, and a poll a little late does not drag the grid with it.
#[test]
fn a_recurrence_fires_on_its_anchor_grid() {
    let (store, _dir) = store();
    store
        .create(
            Some("sweep"),
            "hourly sweep",
            "sweep",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Skip,
            None,
            T0,
        )
        .expect("create");

    // Anchored at T0 and created at T0, so the first occurrence is the next
    // one on the grid rather than one that has already passed.
    assert_eq!(store.get("sweep").unwrap().scheduled_at, T0 + HOUR);

    store.poll(T0 + HOUR, &nobody()).expect("poll");
    assert_eq!(store.get("sweep").unwrap().scheduled_at, T0 + 2 * HOUR);

    // A poll ninety seconds late still leaves the next target on the grid.
    store.poll(T0 + 2 * HOUR + 90_000, &nobody()).expect("poll");
    assert_eq!(
        store.get("sweep").unwrap().scheduled_at,
        T0 + 3 * HOUR,
        "a late poll does not drag the grid"
    );
    assert_eq!(store.get("sweep").unwrap().dispatches, 2);
}

/// TC-PORT-SCHED-3: a recurrence that missed a day owes one fire, not a day of
/// them.
///
/// Upstream: `recurrence.spec.ts`, "advances directly past missed
/// occurrences". Catching up would flood a session with stale work the moment
/// the harness came back, which is the failure mode a restart must not have.
///
/// Expected: one fire, reporting the *latest* missed occurrence, and the next
/// target back on the grid an hour later.
#[test]
fn a_recurrence_that_missed_a_day_owes_one_fire() {
    let (store, _dir) = store();
    store
        .create(
            Some("sweep"),
            "hourly sweep",
            "sweep",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Skip,
            None,
            T0,
        )
        .expect("create");

    let day_later = T0 + 24 * HOUR;
    let fired = store.poll(day_later, &nobody()).expect("poll");

    assert_eq!(fired.len(), 1, "one fire, not twenty-four: {fired:?}");
    assert_eq!(
        fired[0].occurrence_at, day_later,
        "the fire reports the occurrence it was owed at"
    );
    assert_eq!(store.get("sweep").unwrap().scheduled_at, day_later + HOUR);
    assert_eq!(store.get("sweep").unwrap().dispatches, 1);
}

/// TC-PORT-SCHED-4: a scheduled job survives a restart and runs at the right
/// time afterwards.
///
/// The acceptance claim, and the reason the clock is an argument: the case
/// moves time rather than sleeping, so it proves the behaviour without being a
/// timing test.
///
/// Expected: a store reopened over the same file knows both schedules, fires
/// neither before their instants, and fires each at the right one.
#[test]
fn a_scheduled_job_survives_a_restart_and_runs_at_the_right_time() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("schedules.jsonl");
    {
        let store = ScheduleStore::open(&path).expect("open");
        store
            .create(
                Some("once"),
                "one-shot",
                "do it once",
                ScheduleRule::At {
                    at_ms: T0 + 3 * HOUR,
                },
                OverlapPolicy::Skip,
                Some("s-1"),
                T0,
            )
            .unwrap();
        store
            .create(
                Some("repeat"),
                "recurrence",
                "do it hourly",
                ScheduleRule::Every {
                    interval_ms: HOUR,
                    anchor_ms: T0,
                },
                OverlapPolicy::Skip,
                None,
                T0,
            )
            .unwrap();
    }

    let restarted = ScheduleStore::open(&path).expect("reopen");
    assert_eq!(restarted.active().len(), 2);
    assert_eq!(restarted.next_wake(), Some(T0 + HOUR));
    assert_eq!(
        restarted.get("once").unwrap().session.as_deref(),
        Some("s-1"),
        "everything the schedule was created with survived"
    );

    // Half an hour after the restart: nothing is owed yet.
    assert!(restarted
        .poll(T0 + 30 * MINUTE, &nobody())
        .unwrap()
        .is_empty());

    // At the recurrence's first target, only it fires.
    let fired = restarted.poll(T0 + HOUR, &nobody()).expect("poll");
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].id, "repeat");

    // At the one-shot's target both are owed, and the recurrence is owed
    // *first*: nothing polled at T0+2h, so it has been due since then while
    // the one-shot only came due now. The order is by how long each has been
    // owed, which is what stops a poll that finds several depending on map
    // order.
    let fired = restarted.poll(T0 + 3 * HOUR, &nobody()).expect("poll");
    let ids: Vec<&str> = fired.iter().map(|fire| fire.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["repeat", "once"],
        "ordered by occurrence: {fired:?}"
    );
    assert!(!restarted.get("once").unwrap().active);
    assert!(restarted.get("repeat").unwrap().active);
}

/// TC-PORT-SCHED-5: a fire that lands on a run still going is skipped, by
/// default.
///
/// The explicit answer for the overlap the brief asks about. `Skip` is the
/// default because for the work a harness schedules a late duplicate is worth
/// less than the one already running.
///
/// Expected: the fire is reported as `Skipped` and not as a run; the schedule
/// still advances, so the skipped occurrence is not decided again; the next
/// occurrence runs normally once the run has ended.
#[test]
fn a_fire_on_a_busy_schedule_is_skipped_by_default() {
    let (store, _dir) = store();
    store
        .create(
            Some("sweep"),
            "sweep",
            "sweep",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Skip,
            None,
            T0,
        )
        .expect("create");

    let fired = store.poll(T0 + HOUR, &running("sweep")).expect("poll");
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].decision, Decision::Skipped);
    assert_eq!(
        store.get("sweep").unwrap().dispatches,
        0,
        "a skipped fire is not a run"
    );
    assert_eq!(
        store.get("sweep").unwrap().scheduled_at,
        T0 + 2 * HOUR,
        "the decided occurrence advanced, so it is not decided again"
    );

    let fired = store.poll(T0 + 2 * HOUR, &nobody()).expect("poll");
    assert_eq!(fired[0].decision, Decision::Run);
    assert_eq!(store.get("sweep").unwrap().dispatches, 1);
}

/// TC-PORT-SCHED-6: a queued fire is held and released when the run ends.
///
/// Expected: `Held` while busy, then `Released` on the first poll after the
/// run ends, carrying the occurrence it was originally owed at rather than the
/// instant it was released.
#[test]
fn a_queued_fire_is_held_and_released() {
    let (store, _dir) = store();
    store
        .create(
            Some("sweep"),
            "sweep",
            "sweep",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Queue,
            None,
            T0,
        )
        .expect("create");

    let fired = store.poll(T0 + HOUR, &running("sweep")).expect("poll");
    assert_eq!(fired[0].decision, Decision::Held);
    assert_eq!(store.get("sweep").unwrap().held, Some(T0 + HOUR));

    // Still busy a little later: nothing new is owed, and the held one waits.
    let fired = store
        .poll(T0 + 90 * MINUTE, &running("sweep"))
        .expect("poll");
    assert!(fired.is_empty(), "{fired:?}");
    assert_eq!(store.get("sweep").unwrap().held, Some(T0 + HOUR));

    let fired = store.poll(T0 + 95 * MINUTE, &nobody()).expect("poll");
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].decision, Decision::Released);
    assert_eq!(
        fired[0].occurrence_at,
        T0 + HOUR,
        "released carrying the occurrence it was owed at"
    );
    assert_eq!(store.get("sweep").unwrap().held, None);
    assert_eq!(store.get("sweep").unwrap().dispatches, 1);
}

/// TC-PORT-SCHED-7: at most one fire is ever held.
///
/// A backlog of identical work is the thing a scheduler must not build, so a
/// second fire arriving while one is held collapses into it.
///
/// Expected: the second is `Skipped`, and one release follows, not two.
#[test]
fn at_most_one_fire_is_held() {
    let (store, _dir) = store();
    store
        .create(
            Some("sweep"),
            "sweep",
            "sweep",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Queue,
            None,
            T0,
        )
        .expect("create");

    assert_eq!(
        store.poll(T0 + HOUR, &running("sweep")).unwrap()[0].decision,
        Decision::Held
    );
    assert_eq!(
        store.poll(T0 + 2 * HOUR, &running("sweep")).unwrap()[0].decision,
        Decision::Skipped,
        "the second collapses into the first"
    );

    let released = store.poll(T0 + 3 * HOUR, &nobody()).expect("poll");
    let releases = released
        .iter()
        .filter(|fire| fire.decision == Decision::Released)
        .count();
    assert_eq!(releases, 1, "one release, not two: {released:?}");
}

/// TC-PORT-SCHED-8: a concurrent schedule fires regardless.
///
/// Expected: `Run` even while the previous run is going, for work that is
/// genuinely independent per fire.
#[test]
fn a_concurrent_schedule_fires_regardless() {
    let (store, _dir) = store();
    store
        .create(
            Some("poll-inbox"),
            "poll",
            "poll",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Concurrent,
            None,
            T0,
        )
        .expect("create");

    let fired = store.poll(T0 + HOUR, &running("poll-inbox")).expect("poll");
    assert_eq!(fired[0].decision, Decision::Run);
    assert_eq!(store.get("poll-inbox").unwrap().dispatches, 1);
}

/// TC-PORT-SCHED-9: a deleted schedule stops, and stays stopped across a
/// restart.
///
/// Expected: nothing due after the delete, deleting twice is not an error, and
/// a reopened store agrees.
#[test]
fn a_deleted_schedule_stays_deleted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("schedules.jsonl");
    {
        let store = ScheduleStore::open(&path).expect("open");
        store
            .create(
                Some("gone"),
                "sweep",
                "sweep",
                ScheduleRule::Every {
                    interval_ms: HOUR,
                    anchor_ms: T0,
                },
                OverlapPolicy::Skip,
                None,
                T0,
            )
            .unwrap();
        assert!(store.delete("gone", T0 + MINUTE).unwrap());
        assert!(
            !store.delete("gone", T0 + 2 * MINUTE).unwrap(),
            "deleting an inactive schedule is not an error"
        );
        assert!(store.poll(T0 + HOUR, &nobody()).unwrap().is_empty());
    }

    let restarted = ScheduleStore::open(&path).expect("reopen");
    assert!(restarted.active().is_empty());
    assert!(restarted.poll(T0 + 5 * HOUR, &nobody()).unwrap().is_empty());
    assert!(
        restarted.get("gone").is_some(),
        "the record survives; only its activity ended"
    );
}

/// TC-PORT-SCHED-10: what a schedule cannot be created with.
///
/// Upstream: `domain.spec.ts`'s refusals. Each is refused where it is set
/// rather than where it would misbehave.
///
/// Expected: an empty payload, a past one-shot, a sub-floor interval, an
/// unsafe id and a duplicate id are each refused by name.
#[test]
fn a_schedule_that_cannot_work_is_refused_at_creation() {
    let (store, _dir) = store();
    let hourly = ScheduleRule::Every {
        interval_ms: HOUR,
        anchor_ms: T0,
    };

    assert!(matches!(
        store.create(None, "l", "   ", hourly, OverlapPolicy::Skip, None, T0),
        Err(ScheduleError::EmptyPayload)
    ));
    assert!(matches!(
        store.create(
            None,
            "l",
            "p",
            ScheduleRule::At { at_ms: T0 },
            OverlapPolicy::Skip,
            None,
            T0
        ),
        Err(ScheduleError::NotFuture { .. })
    ));
    assert!(matches!(
        store.create(
            None,
            "l",
            "p",
            ScheduleRule::Every {
                interval_ms: MIN_INTERVAL_MS - 1,
                anchor_ms: T0
            },
            OverlapPolicy::Skip,
            None,
            T0
        ),
        Err(ScheduleError::IntervalTooShort(_))
    ));
    assert!(matches!(
        store.create(
            Some("has space"),
            "l",
            "p",
            hourly,
            OverlapPolicy::Skip,
            None,
            T0
        ),
        Err(ScheduleError::BadId(_))
    ));

    store
        .create(
            Some("taken"),
            "l",
            "p",
            hourly,
            OverlapPolicy::Skip,
            None,
            T0,
        )
        .expect("create");
    assert!(matches!(
        store.create(
            Some("taken"),
            "l",
            "p",
            hourly,
            OverlapPolicy::Skip,
            None,
            T0
        ),
        Err(ScheduleError::Duplicate(_))
    ));
    assert!(matches!(
        store.delete("never-made", T0),
        Err(ScheduleError::NoSuchSchedule(_))
    ));
}

/// TC-PORT-SCHED-11: a recurrence anchored in the past starts from now, not
/// from its anchor.
///
/// A schedule created at noon on a grid anchored at midnight is due at the
/// next grid point, not owing twelve fires the instant it exists.
///
/// Expected: the first target is the next grid instant after creation, and the
/// first poll at creation time finds nothing.
#[test]
fn a_recurrence_anchored_in_the_past_owes_nothing_yet() {
    let (store, _dir) = store();
    let created = store
        .create(
            Some("sweep"),
            "sweep",
            "sweep",
            ScheduleRule::Every {
                interval_ms: HOUR,
                anchor_ms: T0,
            },
            OverlapPolicy::Skip,
            None,
            T0 + 12 * HOUR + 30 * MINUTE,
        )
        .expect("create");

    assert_eq!(created.scheduled_at, T0 + 13 * HOUR);
    assert!(store
        .poll(T0 + 12 * HOUR + 30 * MINUTE, &nobody())
        .unwrap()
        .is_empty());
}

/// TC-PORT-SCHED-12: the journal's crash rules hold here too.
///
/// Expected: a line the writer never finished is dropped and truncated; a
/// finished line that does not parse is refused naming its number.
#[test]
fn the_schedule_journal_follows_the_crash_rules() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("schedules.jsonl");
    {
        let store = ScheduleStore::open(&path).expect("open");
        store
            .create(
                Some("whole"),
                "sweep",
                "sweep",
                ScheduleRule::Every {
                    interval_ms: HOUR,
                    anchor_ms: T0,
                },
                OverlapPolicy::Skip,
                None,
                T0,
            )
            .unwrap();
    }

    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(r#"{"id":"torn","at":1,"op":"create""#);
    std::fs::write(&path, &text).unwrap();

    let store = ScheduleStore::open(&path).expect("a torn tail is not a corrupt log");
    assert!(store.get("whole").is_some());
    assert!(store.get("torn").is_none());
    store
        .delete("whole", T0 + MINUTE)
        .expect("the file is usable");

    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("not a change at all\n");
    std::fs::write(&path, &text).unwrap();
    let refused = ScheduleStore::open(&path).expect_err("a finished line must parse");
    assert!(
        matches!(refused, ScheduleError::Corrupt { line: 3, .. }),
        "got {refused:?}"
    );
}
