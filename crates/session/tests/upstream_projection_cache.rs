//! Test Design Specification: the durable half of the projection seam.
//!
//! Feature under test: `tetanus_session::projection_cache`, which keeps each
//! unit's fold in the key-value store so a cold reader folds a tail instead of
//! a journal.
//!
//! Upstream: `packages/session/session-projection-cache`, whose own summary is
//! the rule this suite is built around - a row is a shortcut and never an
//! authority, possibly stale but never wrong, every path fail-soft, and a
//! version mismatch discards the row instead of migrating it.
//!
//! Approach: a counting projection, so a case can assert that a warm start
//! folded *fewer events* rather than merely producing the same answer - the
//! answer would be identical with no cache at all, which is exactly the bug a
//! cache suite has to be able to see.
//!
//! Features NOT tested here: which rows `Projections::restore` accepts, which
//! is `upstream_projection.rs` (TC-PORT-PROJ-*). This suite is about what
//! survives a process.
//!
//! Environmental needs: a temporary directory. No network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tetanus_core::storage::{SharedStore, Store};
use tetanus_session::projection::{Checkpoint, Projection, Projections};
use tetanus_session::projection_cache::{ProjectionCache, TABLE};
use tetanus_session::SessionEvent;

/// A unit that counts how many events it was asked to fold, so a case can see
/// the work a cache saved rather than only the answer it produced.
struct Counting {
    key: &'static str,
    version: u32,
    folds: Arc<AtomicU64>,
}

impl Projection for Counting {
    fn key(&self) -> &str {
        self.key
    }
    fn state_version(&self) -> u32 {
        self.version
    }
    fn init(&self) -> Value {
        json!(0)
    }
    fn apply(&self, state: Value, _event: &SessionEvent) -> Value {
        self.folds.fetch_add(1, Ordering::Relaxed);
        json!(state.as_u64().unwrap_or(0) + 1)
    }
    fn view(&self, state: &Value) -> Value {
        state.clone()
    }
}

fn unit(key: &'static str, version: u32) -> (Arc<Counting>, Arc<AtomicU64>) {
    let folds = Arc::new(AtomicU64::new(0));
    (
        Arc::new(Counting {
            key,
            version,
            folds: Arc::clone(&folds),
        }),
        folds,
    )
}

fn events(count: u64) -> Vec<SessionEvent> {
    (0..count)
        .map(|seq| SessionEvent {
            seq,
            time: 0,
            ty: "mark".into(),
            data: json!({ "n": seq }),
            source_event_seqs: None,
        })
        .collect()
}

fn store(dir: &std::path::Path) -> SharedStore {
    Arc::new(Mutex::new(
        Store::open(dir.join("cache.store"), &[TABLE]).expect("the store opens"),
    ))
}

/// TC-PORT-PCACHE-1: a warm start folds the tail, not the journal.
///
/// The whole point of the cache, and the assertion has to be about the work
/// rather than the answer: a reader with no cache at all produces the same
/// value, so a case that only compared values would pass with the cache
/// removed.
///
/// Input: ten events folded and checkpointed, then a fresh registry warmed
/// from the cache against a log that has grown to fourteen.
/// Expected: the warm reader folds four events and reads fourteen.
#[test]
fn a_warm_start_folds_the_tail_not_the_journal() {
    let home = tempfile::tempdir().expect("temp dir");
    let cache = ProjectionCache::new(store(home.path()));

    let cold = Projections::new();
    let (counter, first_folds) = unit("counter", 1);
    cold.register(counter).expect("registered");
    cold.drive(&events(10));
    assert_eq!(first_folds.load(Ordering::Relaxed), 10);
    assert!(cache.save("s-1", &cold.checkpoint()), "the row was stored");

    let warm = Projections::new();
    let (counter, second_folds) = unit("counter", 1);
    warm.register(counter).expect("registered");
    let changed = cache.warm("s-1", &warm, &events(14));

    assert_eq!(
        second_folds.load(Ordering::Relaxed),
        4,
        "the warm reader refolded events the cache already had"
    );
    assert_eq!(warm.value("counter"), Some(json!(14)));
    assert_eq!(changed, vec!["counter".to_string()]);
}

/// TC-PORT-PCACHE-2: a row survives the process that wrote it.
///
/// A cache held in memory is not a cache. This is the case that fails if the
/// store is never actually published.
///
/// Input: a checkpoint saved through one cache over a store, then a second
/// cache opened over the same file.
/// Expected: the second reads what the first wrote.
#[test]
fn a_row_survives_a_fresh_store() {
    let home = tempfile::tempdir().expect("temp dir");
    let projections = Projections::new();
    let (counter, _folds) = unit("counter", 1);
    projections.register(counter).expect("registered");
    projections.drive(&events(3));

    ProjectionCache::new(store(home.path())).save("s-1", &projections.checkpoint());

    let reopened = ProjectionCache::new(store(home.path())).load("s-1");
    assert_eq!(
        reopened.get("counter").map(|row| (row.ver, row.seq)),
        Some((1, 2)),
        "the row did not survive the store being reopened"
    );
}

/// TC-PORT-PCACHE-3: a version mismatch discards the row instead of migrating
/// it.
///
/// Upstream states this rule where it defines the cache. A unit that has
/// changed what it stores cannot read the old shape, and a cache that tried to
/// convert would be guessing at a state whose meaning it does not know - so
/// the answer is the one that is always right: refold from the log.
///
/// Input: a checkpoint written by version 1, read back by a unit at version 2.
/// Expected: the whole log is refolded, and the value is correct rather than
/// carried over.
#[test]
fn a_version_mismatch_refolds_from_the_log() {
    let home = tempfile::tempdir().expect("temp dir");
    let cache = ProjectionCache::new(store(home.path()));

    let old = Projections::new();
    let (counter, _folds) = unit("counter", 1);
    old.register(counter).expect("registered");
    old.drive(&events(6));
    cache.save("s-1", &old.checkpoint());

    let new = Projections::new();
    let (counter, folds) = unit("counter", 2);
    new.register(counter).expect("registered");
    cache.warm("s-1", &new, &events(6));

    assert_eq!(
        folds.load(Ordering::Relaxed),
        6,
        "a row from another version was adopted"
    );
    assert_eq!(new.value("counter"), Some(json!(6)));
}

/// TC-PORT-PCACHE-4: a row claiming more log than exists is discarded.
///
/// The row is stale-safe in one direction only. A row folded to seq 40 against
/// a journal that now ends at 10 describes a history this reader cannot show -
/// the log may have been truncated, replaced or forked - and folding the tail
/// onto it would mix two conversations.
///
/// Input: a checkpoint from a long log, warmed against a short one.
/// Expected: the short log is refolded from the beginning.
#[test]
fn a_row_from_a_longer_log_is_discarded() {
    let home = tempfile::tempdir().expect("temp dir");
    let cache = ProjectionCache::new(store(home.path()));

    let long = Projections::new();
    let (counter, _folds) = unit("counter", 1);
    long.register(counter).expect("registered");
    long.drive(&events(40));
    cache.save("s-1", &long.checkpoint());

    let short = Projections::new();
    let (counter, folds) = unit("counter", 1);
    short.register(counter).expect("registered");
    cache.warm("s-1", &short, &events(10));

    assert_eq!(
        folds.load(Ordering::Relaxed),
        10,
        "the stale row was adopted"
    );
    assert_eq!(short.value("counter"), Some(json!(10)));
}

/// TC-PORT-PCACHE-5: every failure is a slower read, never a failed one.
///
/// A session that would not open because its *cache* was corrupt would be a
/// session lost to an optimisation. Each of the three ways of not having a
/// usable row answers the same way, because the correct behaviour without a
/// cache is the behaviour this build had before there was one.
///
/// Input: an absent row, a row that is not checkpoints at all, and a save into
/// a store whose table was never declared.
/// Expected: empty from both reads, `false` from the save, and a warm start
/// that still produces the right value in every case.
#[test]
fn every_cache_failure_is_a_slower_read() {
    let home = tempfile::tempdir().expect("temp dir");
    let backing = store(home.path());
    let cache = ProjectionCache::new(Arc::clone(&backing));

    assert!(cache.load("never-written").is_empty(), "an absent row");

    backing
        .lock()
        .expect("the store")
        .put(TABLE, "s-1", json!("not a checkpoint map"))
        .expect("stored");
    assert!(cache.load("s-1").is_empty(), "a row that does not parse");

    let undeclared: SharedStore = Arc::new(Mutex::new(
        Store::open(home.path().join("other.store"), &["something.else"]).expect("opens"),
    ));
    let wrong = ProjectionCache::new(undeclared);
    let rows: BTreeMap<String, Checkpoint> = BTreeMap::from([(
        "counter".to_string(),
        Checkpoint {
            ver: 1,
            seq: 3,
            val: json!(4),
        },
    )]);
    assert!(
        !wrong.save("s-1", &rows),
        "a store that refused the write must answer false, not panic"
    );

    let projections = Projections::new();
    let (counter, folds) = unit("counter", 1);
    projections.register(counter).expect("registered");
    cache.warm("s-1", &projections, &events(5));
    assert_eq!(
        folds.load(Ordering::Relaxed),
        5,
        "it refolded, as it should"
    );
    assert_eq!(projections.value("counter"), Some(json!(5)));
}

/// TC-PORT-PCACHE-6: one row per session, and a forgotten session leaves none.
///
/// Two sessions sharing a fold would be the worst failure this cache could
/// have: not a slow read but a wrong answer, and one that looks right.
///
/// Input: two sessions checkpointed at different depths through one cache,
/// then one of them forgotten.
/// Expected: each loads its own row; forgetting one leaves the other; and
/// forgetting a session that has no row is not an error.
#[test]
fn each_session_keeps_its_own_row() {
    let home = tempfile::tempdir().expect("temp dir");
    let cache = ProjectionCache::new(store(home.path()));

    for (session, depth) in [("s-1", 4u64), ("s-2", 9)] {
        let projections = Projections::new();
        let (counter, _folds) = unit("counter", 1);
        projections.register(counter).expect("registered");
        projections.drive(&events(depth));
        assert!(cache.save(session, &projections.checkpoint()));
    }

    assert_eq!(cache.load("s-1")["counter"].val, json!(4));
    assert_eq!(cache.load("s-2")["counter"].val, json!(9));

    assert!(cache.forget("s-1"), "the row was there to forget");
    assert!(cache.load("s-1").is_empty(), "and is gone");
    assert_eq!(
        cache.load("s-2")["counter"].val,
        json!(9),
        "forgetting one session must not touch another"
    );
    assert!(
        !cache.forget("s-1"),
        "forgetting a session with no row answers false rather than failing"
    );
}
