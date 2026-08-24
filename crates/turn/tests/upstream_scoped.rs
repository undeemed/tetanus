//! Ported from upstream `packages/core/session/tests/scoped.spec.ts`.
//!
//! That file asks two questions of a session: who hears its events, and what
//! `flush` promises the caller who awaits it. Upstream answers the first with
//! Cordis scopes - a listener registered on a scoped context hears only the
//! sessions minted in that scope - and tetanus answers it structurally
//! instead, by giving every session its own [`EventBus`]. So the scope keys,
//! the minting plugin and the parent chain have nothing to restate, and the
//! property they exist to give does: a listener on one session's bus never
//! hears another session's facts, whoever wrote them.
//!
//! The flush half ports as it stands, because `TurnEngine::flush` is the same
//! barrier: dispatch `session/flush` to every participant, then put the
//! journal on disk.
//!
//! Test design: every case runs offline on the mock adapter, against a journal
//! in a temporary directory.

mod harness;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use harness::Harness;
use tetanus_session::{SessionEventDispatch, SessionFlush};

/// Every topic a listener heard, in arrival order.
#[derive(Default)]
struct Heard(Mutex<Vec<String>>);

impl Heard {
    fn topics(&self) -> Vec<String> {
        self.0.lock().expect("heard").clone()
    }
    fn push(&self, what: String) {
        self.0.lock().expect("heard").push(what);
    }
}

/// Read the journal back off disk, which is the only way to ask what a barrier
/// actually committed. The live log would answer from memory and say yes to a
/// flush that never reached the filesystem.
fn on_disk(h: &Harness) -> Vec<tetanus_session::SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("replay")
}

/// TC-PORT-SCOPE-1: a barrier with no participant is still a barrier.
///
/// Input: a session with one durable fact and nothing listening for
/// `session/flush`.
/// Expected: the flush returns, and the fact is on disk. Upstream answers
/// `false` here, meaning "no listener participated"; tetanus returns no such
/// report, because the thing the caller awaited is durability and not
/// attendance, and the journal is fsynced on every append in any case. What is
/// restated is that a flush nobody joined is a success rather than a no-op the
/// caller has to check.
#[tokio::test]
async fn a_barrier_with_no_participant_still_commits() {
    let h = Harness::new("flush-alone").await;
    h.engine
        .log()
        .append("turn/start", serde_json::json!({ "turn": 1 }))
        .expect("append");

    h.engine.flush().await.expect("flush");

    let stored = on_disk(&h);
    assert_eq!(stored.last().expect("last").ty, "turn/start");
}

/// TC-PORT-SCOPE-2: every participant is awaited before the barrier returns.
///
/// Input: three `session/flush` participants, each sleeping a different length
/// of time before recording that it finished.
/// Expected: all three have finished by the time `flush` returns, and each was
/// handed the id of the session being flushed. Upstream states this as
/// "awaits all listeners"; the point of the case is that a barrier that
/// returned early would be a promise of durability that a participant had not
/// yet kept.
#[tokio::test]
async fn the_barrier_waits_for_every_participant() {
    let h = Harness::new("flush-awaits").await;
    let heard = Arc::new(Heard::default());

    let mut handles = Vec::new();
    for (name, delay) in [("slow", 30u64), ("middling", 15), ("quick", 1)] {
        let heard = Arc::clone(&heard);
        handles.push(h.engine.bus().on_parallel(move |ev: &SessionFlush| {
            let heard = Arc::clone(&heard);
            let session = ev.session_id.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                heard.push(format!("{name}:{session}"));
            }
            .boxed()
        }));
    }

    h.engine.flush().await.expect("flush");

    let mut finished = heard.topics();
    finished.sort();
    assert_eq!(
        finished,
        vec![
            "middling:flush-awaits".to_string(),
            "quick:flush-awaits".to_string(),
            "slow:flush-awaits".to_string(),
        ],
        "the barrier returned before a participant had finished"
    );
    drop(handles);
}

/// TC-PORT-SCOPE-3: one participant's failure does not starve the others.
///
/// Input: three participants; the first panics at once.
/// Expected: the other two still run, and the barrier still succeeds.
///
/// This is the one claim in upstream's file that tetanus answers differently.
/// There a rejecting flush listener is propagated, on the reasoning that the
/// caller owns the failure policy; here `parallel` is a contained mode by
/// design (`docs/parity.md`, the `core/*` row), so a participant with a bug
/// cannot fail a barrier it merely attended. The half both agree on - that a
/// failure must not starve the listeners after it - is what this pins, and it
/// is the half a durability barrier cannot do without: a participant that
/// silently stopped being called is a fact nobody flushed.
#[tokio::test]
async fn a_failing_participant_does_not_starve_the_others() {
    let h = Harness::new("flush-panics").await;
    let heard = Arc::new(Heard::default());

    let panicking = h
        .engine
        .bus()
        .on_parallel(|_: &SessionFlush| async { panic!("a participant with a bug") }.boxed());
    let mut handles = vec![panicking];
    for name in ["after-1", "after-2"] {
        let heard = Arc::clone(&heard);
        handles.push(h.engine.bus().on_parallel(move |_: &SessionFlush| {
            let heard = Arc::clone(&heard);
            async move {
                heard.push(name.to_string());
            }
            .boxed()
        }));
    }

    h.engine
        .flush()
        .await
        .expect("a contained participant does not fail the barrier");

    let mut ran = heard.topics();
    ran.sort();
    assert_eq!(ran, vec!["after-1".to_string(), "after-2".to_string()]);
    drop(handles);
}

/// TC-PORT-SCOPE-4: what a participant writes during the barrier is in the
/// journal when the barrier returns.
///
/// Input: a participant that appends its own durable fact when it is asked to
/// flush.
/// Expected: that fact is on disk after `flush` returns. A participant is
/// being told "put what you owe on the log now", and a caller that continued
/// before it had done so would be reading a journal it was promised was
/// complete.
///
/// What this does *not* pin, deliberately, is the order of the dispatch and
/// the engine's own `log.flush()`. A mutation check says so: moving the sync
/// before the dispatch fails nothing here, because `JsonlSessionLog` fsyncs
/// every record as it appends it, so the trailing sync commits nothing the
/// appends did not. The sync stays because `SessionLog` is a seam and a
/// batching implementation would need it; the claim that has teeth today is
/// attendance, and dropping the dispatch fails this case and two others.
#[tokio::test]
async fn a_participant_that_writes_during_the_barrier_is_committed_by_it() {
    let h = Harness::new("flush-writes").await;
    let log = Arc::clone(h.engine.log());

    let handle = h.engine.bus().on_parallel(move |_: &SessionFlush| {
        let log = Arc::clone(&log);
        async move {
            log.append("test/participant", serde_json::json!({ "wrote": true }))
                .expect("append");
        }
        .boxed()
    });

    h.engine.flush().await.expect("flush");

    let stored = on_disk(&h);
    assert!(
        stored.iter().any(|e| e.ty == "test/participant"),
        "the barrier committed before its participant wrote: {:?}",
        stored.iter().map(|e| &e.ty).collect::<Vec<_>>()
    );
    drop(handle);
}

/// TC-PORT-SCOPE-5: a listener hears its own session and no other.
///
/// Input: two engines on two journals, each with a listener on its own bus,
/// and a durable fact written to each.
/// Expected: each listener heard exactly its own session's fact.
///
/// Upstream reaches this with Cordis scopes: a listener on a scoped context
/// hears only the sessions minted in that scope, and its case turns on a
/// second scope hearing nothing. tetanus has no scopes and needs none here,
/// because a bus belongs to a session rather than to the process, so the
/// containment is structural rather than a rule the dispatcher applies. The
/// engine states the same property one layer up (TC-SUB-7, a push reaches only
/// its own session's subscriptions); this states it where the events are
/// actually published, so a future shared bus would fail here first.
#[tokio::test]
async fn a_listener_hears_its_own_session_and_no_other() {
    let owner = Harness::new("owner").await;
    let other = Harness::new("other").await;

    let heard = Arc::new(Heard::default());
    let mut handles = Vec::new();
    for (label, h) in [("owner", &owner), ("other", &other)] {
        let heard = Arc::clone(&heard);
        handles.push(h.engine.bus().on_emit(move |ev: &SessionEventDispatch| {
            heard.push(format!("{label}:{}", ev.event.ty));
        }));
    }

    owner
        .engine
        .log()
        .append("turn/start", serde_json::json!({ "turn": 1 }))
        .expect("append");
    other
        .engine
        .log()
        .append("turn/end", serde_json::json!({ "turn": 1 }))
        .expect("append");

    assert_eq!(
        heard.topics(),
        vec!["owner:turn/start".to_string(), "other:turn/end".to_string()]
    );
    drop(handles);
}

/// TC-PORT-SCOPE-6: a session nobody is listening to publishes to nobody, and
/// still writes its journal.
///
/// Upstream's "bare session" case: one created outside any scope dispatches
/// subject-less, so a scoped listener never hears it. Restated here as the
/// consequence that matters - the durable log is what a session is, and being
/// heard is not a condition of being written. The listener count on the bus is
/// asserted as well, so a case that heard nothing because it registered on the
/// wrong bus would not pass by accident.
#[tokio::test]
async fn an_unheard_session_still_writes_its_journal() {
    let bare = Harness::new("bare").await;
    let watcher = Harness::new("watcher").await;

    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let handle = watcher
        .engine
        .bus()
        .on_emit(move |_: &SessionEventDispatch| {
            seen.fetch_add(1, Ordering::Relaxed);
        });

    bare.engine
        .log()
        .append("turn/start", serde_json::json!({ "turn": 1 }))
        .expect("append");
    bare.engine.flush().await.expect("flush");

    assert_eq!(count.load(Ordering::Relaxed), 0, "nobody was listening");
    assert_eq!(
        on_disk(&bare).last().expect("last").ty,
        "turn/start",
        "and it was written anyway"
    );

    // The watcher's bus is live: the same append on it is heard, so the zero
    // above is silence rather than a listener registered on nothing.
    watcher
        .engine
        .log()
        .append("turn/start", serde_json::json!({ "turn": 1 }))
        .expect("append");
    assert_eq!(count.load(Ordering::Relaxed), 1);
    drop(handle);
}
