//! Test Design Specification: the `tools/post-execute` additional-contexts
//! channel, ported.
//!
//! Feature under test: what a post-execute listener can put in front of the
//! model *next*, as distinct from what it makes the model read as this call's
//! result. Upstream carries it as `PostToolDecision.additionalContexts`,
//! accepted at result commit by `packages/core/agent-loop/src/tool-calls.ts`
//! and delivered at the next step boundary through the agent's inbox; the two
//! consumers that write it are the hook bridges and the repeat-tool guard
//! (`packages/guard/repeat-tool-reminder`).
//!
//! Approach: real offline turns over a temporary journal, driven through the
//! bus, so every assertion is about durable events rather than intentions.
//! The channel is only meaningful end to end - a listener writes, and the
//! model reads at the next step - so the cases assert the journal, not the
//! decision struct.
//!
//! What is not restated, and why. Upstream folds contexts onto both decision
//! variants, `accept` and `block`; tetanus's post-execute output is one
//! outcome plus the contexts, so there is no second variant to fold onto and
//! the blocked-call case has no counterpart. Its `next-turn` half - a queued
//! prompt claimed at a *turn* boundary - is not exercised here: `run_turn`
//! takes its prompt from the caller rather than the queue, which stays a row
//! in `docs/parity.md`.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::events::ToolsPostExecute;
use tetanus_turn::inbox::{own_suffix, Inbox};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::llm::Message;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine};

const INBOX_SPLICED: &str = "agent/inbox/spliced";
const NOTE: &str = "a note about the caller";

struct Fixture {
    engine: TurnEngine,
    log: Arc<JsonlSessionLog>,
    bus: EventBus,
    _dir: TempDir,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(format!("{name}.jsonl"));
        let bus = EventBus::new();
        let concrete = JsonlSessionLog::create(name, &path, bus.clone()).expect("journal");
        let log: Arc<dyn SessionLog> = Arc::clone(&concrete) as Arc<dyn SessionLog>;
        let ctx = boot(
            bus.clone(),
            Arc::new(MockAdapter::new()),
            Arc::new(ToolRegistry::new().with(Arc::new(EchoTool))),
            log,
        )
        .expect("boot");
        let engine = TurnEngine::from_context(&ctx, TurnConfig::default()).expect("engine");
        Self {
            engine,
            log: concrete,
            bus,
            _dir: dir,
        }
    }
}

fn types(events: &[SessionEvent]) -> Vec<&str> {
    events.iter().map(|e| e.ty.as_str()).collect()
}

fn contents_of(events: &[SessionEvent], ty: &str) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.ty == ty)
        .map(|e| {
            e.data
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// TC-POSTCTX-1: a context a listener attaches reaches the model, as a message
/// and not as part of the tool's answer.
///
/// The rule the channel exists for. A guard with something to say about the
/// *caller* - "you have called this five times" - must not say it by editing
/// the tool's output: that corrupts a tool author's contract, and a tool
/// parsing its own result back would find a sentence nobody wrote. So the
/// note travels beside the result and arrives as its own `user/message`.
#[tokio::test]
async fn an_attached_context_arrives_as_its_own_message_and_leaves_the_result_alone() {
    let f = Fixture::new("postctx-delivered").await;
    // Once, on the first call only. A guard that spoke on every call would
    // feed this fixture's echo tool its own note back, which says nothing
    // about the channel and everything about the tool.
    let spoken = Arc::new(AtomicBool::new(false));
    let once = Arc::clone(&spoken);
    let _guard = f.bus.on_waterfall::<ToolsPostExecute, _>(move |ev, next| {
        let once = Arc::clone(&once);
        Box::pin(async move {
            let decision = next.run(ev).await;
            if once.swap(true, Ordering::SeqCst) {
                decision
            } else {
                decision.with_context(Message::user(NOTE))
            }
        })
    });

    f.engine.run_turn("use the tool").await.expect("turn");
    let events = f.log.events();

    let first_result = events
        .iter()
        .find(|e| e.ty == topic::TOOL_RESULT)
        .expect("a tool result");
    assert_ne!(
        first_result.data.get("content").and_then(|v| v.as_str()),
        Some(NOTE),
        "the tool's own answer is untouched"
    );
    assert!(
        contents_of(&events, topic::USER_MESSAGE)
            .iter()
            .any(|c| c == NOTE),
        "the note is delivered as a message of its own"
    );
}

/// TC-POSTCTX-2: the context is committed after the result it came from.
///
/// A note landing before the result it describes would put a journal on disk
/// where the complaint precedes the thing complained about, and a replay would
/// show the model reacting to something that had not happened yet.
#[tokio::test]
async fn a_context_is_recorded_after_the_result_it_came_from() {
    let f = Fixture::new("postctx-order").await;
    let _guard = f.bus.on_waterfall::<ToolsPostExecute, _>(|ev, next| {
        Box::pin(async move { next.run(ev).await.with_context(Message::user("after me")) })
    });

    f.engine.run_turn("use the tool").await.expect("turn");
    let events = f.log.events();
    let order = types(&events);

    let result_at = order
        .iter()
        .position(|t| *t == topic::TOOL_RESULT)
        .expect("a tool result");
    let splice_at = order
        .iter()
        .position(|t| *t == INBOX_SPLICED)
        .expect("the queued context");
    assert!(
        result_at < splice_at,
        "the result precedes the context it produced: {order:?}"
    );
}

/// TC-POSTCTX-3: the context is delivered at the *next* step, not this one.
///
/// It is input for the model's next decision. Delivering it inside the step
/// that produced it would put it in a request the model has already been sent,
/// which is not possible, or force a second request nobody asked for.
#[tokio::test]
async fn a_context_is_delivered_at_the_next_step_boundary() {
    let f = Fixture::new("postctx-boundary").await;
    let _guard = f.bus.on_waterfall::<ToolsPostExecute, _>(|ev, next| {
        Box::pin(async move { next.run(ev).await.with_context(Message::user("next step")) })
    });

    f.engine.run_turn("use the tool").await.expect("turn");
    let events = f.log.events();
    let order = types(&events);

    let queued = order
        .iter()
        .position(|t| *t == INBOX_SPLICED)
        .expect("the queued context");
    let delivered = events
        .iter()
        .position(|e| {
            e.ty == topic::USER_MESSAGE
                && e.data.get("content").and_then(|v| v.as_str()) == Some("next step")
        })
        .expect("the delivered context");
    let boundary = order
        .iter()
        .enumerate()
        .filter(|(index, t)| **t == topic::STEP_START && *index > queued)
        .map(|(index, _)| index)
        .next()
        .expect("a later step opened");

    assert!(
        queued < boundary && boundary < delivered,
        "queued, then a step opens, then delivered: {order:?}"
    );
}

/// TC-POSTCTX-4: a listener that attaches nothing changes nothing.
///
/// The seam has to be free. A journal from a session whose listeners attach no
/// context must be byte-identical to the journal it had before the channel
/// existed - no empty splice, no zero-length record - or every deployment pays
/// for a feature it does not use and every diff of a journal is noisy.
#[tokio::test]
async fn a_turn_with_no_attached_context_writes_no_queue_record() {
    let f = Fixture::new("postctx-free").await;
    let _observer = f
        .bus
        .on_waterfall::<ToolsPostExecute, _>(|ev, next| Box::pin(next.run(ev)));

    f.engine.run_turn("use the tool").await.expect("turn");
    let events = f.log.events();

    assert!(
        !types(&events).contains(&INBOX_SPLICED),
        "no context, no record: {:?}",
        types(&events)
    );
}

/// TC-POSTCTX-5: contexts are delivered in the order their calls committed,
/// and one queued by the last step is kept rather than lost.
///
/// Commitment is in model order however the calls settled, and the notes ride
/// the commit, so a guard reporting on a run of calls tells the model about
/// the sequence that actually happened. The tail matters just as much: a turn
/// ends while the note its final step produced is still queued, and that note
/// must survive in the durable queue for the next turn. Dropping it would make
/// the channel silently lossy exactly at the boundary a guard cares about -
/// the step that ended the turn.
#[tokio::test]
async fn contexts_are_delivered_in_commit_order_and_the_last_one_is_kept() {
    let f = Fixture::new("postctx-order-multi").await;
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let _guard = f.bus.on_waterfall::<ToolsPostExecute, _>(move |ev, next| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            let nth = counter.fetch_add(1, Ordering::SeqCst);
            next.run(ev)
                .await
                .with_context(Message::user(format!("note {nth}")))
        })
    });

    f.engine.run_turn("use the tool").await.expect("turn");
    let events = f.log.events();

    let attached = seen.load(Ordering::SeqCst);
    let delivered: Vec<String> = contents_of(&events, topic::USER_MESSAGE)
        .into_iter()
        .filter(|c| c.starts_with("note "))
        .collect();
    let expected: Vec<String> = (0..attached).map(|n| format!("note {n}")).collect();

    assert!(attached >= 2, "the fixture produced a run of calls");
    assert_eq!(
        delivered,
        expected[..delivered.len()],
        "delivered notes are the earliest ones, in commit order"
    );
    assert_eq!(
        delivered.len(),
        attached - 1,
        "every note but the last one was delivered inside the turn"
    );

    // The last one is not lost: it is still on the durable queue, which is
    // what a resumed session would fold back.
    let waiting = Inbox::replay(own_suffix(&events)).expect("the queue folds");
    let held: Vec<String> = waiting
        .next_step()
        .iter()
        .map(|p| p.message.content.clone())
        .collect();
    assert_eq!(
        held,
        [expected.last().expect("a last note").clone()],
        "the final step's note is waiting for the next turn"
    );
}

/// TC-POSTCTX-6: a fork's seed is not folded back into the child's queue.
///
/// A fork copies its parent's events in, and those include the parent's queue
/// records. Folding them would hand the child prompts a person queued for the
/// parent - and, because those coordinates were normalized against the
/// parent's list, in an order neither session ever had.
#[test]
fn a_forked_journal_folds_only_its_own_events() {
    let event = |ty: &str, seq: u64, data: serde_json::Value| SessionEvent {
        ty: ty.into(),
        seq,
        time: seq,
        data,
        source_event_seqs: None,
    };
    // seq 0 the child's header, seq 1..=2 the parent's copied events, then its
    // own work - contract section 4.4.6's layout.
    let forked = [
        event(
            "session/start",
            0,
            serde_json::json!({"session_id": "child", "parent_session": "parent", "fork_seq": 2}),
        ),
        event(INBOX_SPLICED, 1, serde_json::json!({"target": "next-step"})),
        event(topic::USER_MESSAGE, 2, serde_json::json!({"content": "x"})),
        event(INBOX_SPLICED, 3, serde_json::json!({"target": "next-step"})),
    ];
    let own = own_suffix(&forked);
    assert_eq!(own.len(), 1, "only the child's own work");
    assert_eq!(own[0].seq, 3);

    // A journal nobody forked is returned whole, seed logic and all.
    let plain = [
        event(
            "session/start",
            0,
            serde_json::json!({"session_id": "root"}),
        ),
        event(INBOX_SPLICED, 1, serde_json::json!({"target": "next-step"})),
    ];
    assert_eq!(own_suffix(&plain).len(), 2);
    assert_eq!(own_suffix(&[]).len(), 0);
}
