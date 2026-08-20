//! Test Design Specification: what a run of prompts guarantees, for every run
//! rather than for one fixture.
//!
//! Feature under test: the scheduling `agent.prompt` promises - one turn per
//! prompt it accepts, turn numbers counting up from one, one user message per
//! turn, and the `agent/status` transitions contract §4.6 fixes. Upstream
//! pins the same invariants with fast-check in
//! `packages/core/agent-loop/tests/properties.spec.ts`.
//!
//! Approach: generate the prompts, run them against a real engine over a real
//! journal in a temp directory, and assert over the journal the run left
//! behind rather than over the answers alone. Upstream queues a prompt that
//! arrives while a turn is running; tetanus refuses it (`SessionBusy`,
//! TC-AGENT-4). So the burst property is stated as the rule that survives the
//! difference: a prompt is either a turn of its own or an error, and never a
//! message the journal quietly drops.
//!
//! Features NOT tested here: what one turn emits (`crates/turn/tests/
//! turn_flow.rs`), what a journal guarantees a reader (`crates/turn/tests/
//! properties.rs`), and the one-example forms of these rules - TC-AGENT-1,
//! TC-AGENT-3, TC-AGENT-4 and TC-AGENT-5 - which stay where they are.
//!
//! Environmental needs: a writable temp directory and a multi-threaded
//! runtime, so a burst is raced rather than merely interleaved. No case
//! reaches a network or an API key, and no case sleeps: the mock adapter
//! answers immediately, so a hang is a failure and not a slow machine.
//!
//! Pass criteria: each case's stated expected result holds for every
//! generated run.
//! Fail criteria: any counterexample, or a panic.

use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, AgentStatusPush, Engine, EventSink, SessionCreateParams, SessionEventPush,
    SessionEventsParams, SessionSubscribeParams,
};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::{AgentState, SessionEvent};

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    /// TC-PROP-AGENT-1: prompts sent one after another are one turn each,
    /// numbered from one, and no message is lost or reordered.
    ///
    /// Input: one to four prompts, sent in order, each awaited before the
    /// next.
    /// Expected: the answers report turns `1..=n`; the journal holds `n`
    /// `turn/start` and `n` `turn/end` events with those same numbers; the
    /// user messages are the prompts, in the order they were sent; and every
    /// turn holds exactly one of them.
    #[test]
    fn sequential_prompts_are_one_turn_each(prompts in prompts(1..5usize)) {
        let outcome: Result<(), TestCaseError> = runtime().block_on(async {
            let dir = TempDir::new().expect("temp dir");
            let engine = engine(&dir);
            let id = session(&engine).await;

            let mut answered = Vec::new();
            for prompt in &prompts {
                answered.push(ask(&engine, &id, prompt).await.expect("prompt").summary.turn);
            }

            let numbered: Vec<u64> = (1..=prompts.len() as u64).collect();
            prop_assert_eq!(&answered, &numbered, "each answer reports its own turn");

            let journal = events(&engine, &id).await;
            prop_assert_eq!(turns(&journal, "turn/start"), numbered.clone());
            prop_assert_eq!(turns(&journal, "turn/end"), numbered);
            prop_assert_eq!(&messages(&journal), &prompts, "the journal is the run");
            prop_assert_eq!(messages_per_turn(&journal), vec![1; prompts.len()]);
            Ok(())
        });
        outcome?;
    }

    /// TC-PROP-AGENT-2: a burst against one session opens no two turns at
    /// once, and loses no prompt silently.
    ///
    /// Input: two to five prompts spawned together on a multi-threaded
    /// runtime, so which of them reaches the busy claim first is not fixed.
    /// Expected: at least one is answered; every refusal is `SessionBusy` and
    /// nothing else; the answered ones hold the turn numbers `1..=answered`
    /// between them; and the journal holds exactly those turns, each with one
    /// user message, each message one of the prompts and none of them twice.
    #[test]
    fn a_burst_opens_one_turn_at_a_time(prompts in prompts(2..6usize)) {
        let outcome: Result<(), TestCaseError> = runtime().block_on(async {
            let dir = TempDir::new().expect("temp dir");
            let engine = engine(&dir);
            let id = session(&engine).await;

            let mut racing = Vec::new();
            for prompt in &prompts {
                let engine = Arc::clone(&engine);
                let (id, prompt) = (id.clone(), prompt.clone());
                racing.push(tokio::spawn(async move { ask(&engine, &id, &prompt).await }));
            }

            let mut answered = Vec::new();
            let mut refused = 0;
            for race in racing {
                match race.await.expect("join") {
                    Ok(result) => answered.push(result.summary.turn),
                    Err(error) => {
                        prop_assert_eq!(
                            error.kind(),
                            Some(ErrorCode::SessionBusy),
                            "the only refusal a burst can earn: {}",
                            error.message
                        );
                        refused += 1;
                    }
                }
            }
            prop_assert!(!answered.is_empty(), "a burst runs at least its first prompt");
            prop_assert_eq!(answered.len() + refused, prompts.len(), "every prompt is accounted for");
            answered.sort_unstable();
            prop_assert_eq!(&answered, &(1..=answered.len() as u64).collect::<Vec<u64>>());

            let journal = events(&engine, &id).await;
            prop_assert_eq!(turns(&journal, "turn/start"), answered.clone());
            prop_assert_eq!(turns(&journal, "turn/end"), answered.clone(), "no turn is left open");
            prop_assert_eq!(messages_per_turn(&journal), vec![1; answered.len()]);

            let mut written = messages(&journal);
            prop_assert!(written.iter().all(|text| prompts.contains(text)), "the journal invents nothing");
            written.sort();
            written.dedup();
            prop_assert_eq!(written.len(), answered.len(), "one message per turn, and no message twice");
            Ok(())
        });
        outcome?;
    }

    /// TC-PROP-AGENT-3: §4.6's state machine holds over a whole run, not only
    /// over one turn.
    ///
    /// Input: one to four prompts, sent in order, with a subscriber recording
    /// every push.
    /// Expected: the statuses pushed are `running, idle` per prompt, in that
    /// order, so the states alternate, never repeat, and the run ends idle.
    /// Every durable event of a turn falls between that turn's `running` and
    /// its `idle`, which is what lets a surface show a turn as running before
    /// its first event arrives.
    #[test]
    fn a_run_alternates_running_and_idle(prompts in prompts(1..5usize)) {
        let outcome: Result<(), TestCaseError> = runtime().block_on(async {
            let dir = TempDir::new().expect("temp dir");
            let engine = engine(&dir);
            let id = session(&engine).await;
            let sink = Arc::new(Recorder::default());
            engine
                .session_subscribe(
                    SessionSubscribeParams { session_id: id.clone(), from_seq: None },
                    Arc::clone(&sink) as Arc<dyn EventSink>,
                )
                .await
                .expect("subscribe");

            for prompt in &prompts {
                ask(&engine, &id, prompt).await.expect("prompt");
            }

            let seen = sink.seen();
            let statuses: Vec<&str> = seen
                .iter()
                .filter(|push| push.starts_with("status:"))
                .map(String::as_str)
                .collect();
            let expected: Vec<&str> = prompts
                .iter()
                .flat_map(|_| ["status:running", "status:idle"])
                .collect();
            prop_assert_eq!(&statuses, &expected, "one bracketed turn per prompt");
            for pair in statuses.windows(2) {
                prop_assert_ne!(pair[0], pair[1], "a repeated state is no transition");
            }
            prop_assert_eq!(seen.first().map(String::as_str), Some("status:running"),
                "the state changes before the first event of the run");
            prop_assert_eq!(seen.last().map(String::as_str), Some("status:idle"),
                "the run ends idle");
            Ok(())
        });
        outcome?;
    }
}

/// The prompts of one generated run: distinct, so a message on the journal
/// names the send it came from even when a run repeats a word.
fn prompts(size: std::ops::Range<usize>) -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-z ]{1,12}", size).prop_map(|texts| {
        texts
            .into_iter()
            .enumerate()
            .map(|(i, text)| format!("{i}: {text}"))
            .collect()
    })
}

/// A runtime with threads to spare, so a burst is raced and not merely
/// interleaved on one.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime")
}

fn engine(dir: &TempDir) -> Arc<HarnessEngine> {
    Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    }))
}

async fn session(engine: &HarnessEngine) -> String {
    engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id
}

async fn ask(
    engine: &HarnessEngine,
    session_id: &str,
    content: &str,
) -> Result<tetanus_protocol::methods::AgentPromptResult, tetanus_protocol::rpc::RpcError> {
    engine
        .agent_prompt(AgentPromptParams {
            session_id: session_id.to_string(),
            content: content.to_string(),
        })
        .await
}

async fn events(engine: &HarnessEngine, session_id: &str) -> Vec<SessionEvent> {
    engine
        .session_events(SessionEventsParams {
            session_id: session_id.to_string(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events")
        .events
}

/// The turn numbers carried by every event of one type, in journal order.
fn turns(journal: &[SessionEvent], ty: &str) -> Vec<u64> {
    journal
        .iter()
        .filter(|event| event.ty == ty)
        .filter_map(|event| event.data["turn"].as_u64())
        .collect()
}

/// The user messages of the journal, in the order they were written.
fn messages(journal: &[SessionEvent]) -> Vec<String> {
    journal
        .iter()
        .filter(|event| event.ty == "user/message")
        .filter_map(|event| event.data["content"].as_str().map(str::to_string))
        .collect()
}

/// How many user messages each turn holds, in turn order.
fn messages_per_turn(journal: &[SessionEvent]) -> Vec<usize> {
    let mut counts: Vec<usize> = Vec::new();
    for event in journal {
        match event.ty.as_str() {
            "turn/start" => counts.push(0),
            "user/message" => {
                if let Some(open) = counts.last_mut() {
                    *open += 1;
                }
            }
            _ => {}
        }
    }
    counts
}

/// Every push a carrier would have written, in arrival order.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<String>>,
}

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }
}

impl EventSink for Recorder {
    fn session_event(&self, push: SessionEventPush) {
        self.seen
            .lock()
            .expect("seen")
            .push(format!("event:{}", push.event.ty));
    }

    fn agent_status(&self, push: AgentStatusPush) {
        let state = match push.state {
            AgentState::Idle => "idle".to_string(),
            AgentState::Running => "running".to_string(),
            AgentState::Other(other) => other,
        };
        self.seen
            .lock()
            .expect("seen")
            .push(format!("status:{state}"));
    }
}
