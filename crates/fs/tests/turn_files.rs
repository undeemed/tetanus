//! Test Design Specification: a whole turn touching real files.
//!
//! Feature under test: the composition, end to end - a turn engine booted over
//! the fs tools, a scripted model that calls them, and a journal that records
//! what happened. Every other suite in this crate tests one layer; this one
//! tests that the layers are actually connected, which no unit case can.
//!
//! Approach: a scripted adapter standing in for the model, so the tool calls
//! are exactly the ones the case is about and the run is deterministic and
//! offline. A real provider would make the case a network test of somebody
//! else's sampler.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime. No
//! case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use support::Fixture;
use tetanus_core::EventBus;
use tetanus_fs::observation::ObservedState;
use tetanus_fs::FsTools;
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::approval::{ApprovalAsk, ApprovalOutcome, TOOL_NOT_PERMITTED};
use tetanus_turn::boot::boot;
use tetanus_turn::llm::{
    ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse, StreamChunk,
};
use tetanus_turn::log::topic;
use tetanus_turn::tools::{ToolCall, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine};

/// A model that was told what to call.
///
/// Each entry is one step's tool calls; when the script runs out the turn is
/// answered with text, which is how a real turn ends.
struct Script {
    steps: Mutex<VecDeque<Vec<ToolCall>>>,
    answer: &'static str,
}

impl Script {
    fn new(steps: Vec<Vec<ToolCall>>, answer: &'static str) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            answer,
        })
    }
}

#[async_trait::async_trait]
impl LlmAdapter for Script {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn models(&self) -> Vec<String> {
        vec!["scripted-1".into()]
    }

    async fn stream(
        &self,
        _request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        let step = self.steps.lock().expect("script").pop_front();
        match step {
            Some(calls) => {
                for call in &calls {
                    sink.chunk(StreamChunk::ToolCall { call: call.clone() })
                        .await?;
                }
                Ok(ModelResponse {
                    content: String::new(),
                    reasoning: String::new(),
                    tool_calls: calls,
                    finish_reason: "tool_calls".into(),
                    usage: None,
                })
            }
            None => {
                sink.chunk(StreamChunk::Text {
                    delta: self.answer.to_string(),
                })
                .await?;
                Ok(ModelResponse {
                    content: self.answer.into(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: "stop".into(),
                    usage: None,
                })
            }
        }
    }
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }
}

/// One booted engine over the fs tools, and the bus its answerers attach to.
struct Composed {
    engine: TurnEngine,
    bus: EventBus,
    log_path: std::path::PathBuf,
}

fn compose(fixture: &Fixture, name: &str, script: Arc<Script>) -> Composed {
    let mut tools = ToolRegistry::new();
    FsTools::new(
        fixture.sandboxed(),
        Arc::new(ObservedState::new()),
        format!("session-{name}"),
    )
    .register(&mut tools);

    let bus = EventBus::new();
    // The journal lives outside the workspace, so a glob or a listing in a case
    // never meets the record of the run that produced it.
    let log_path = fixture.outside().join(format!("{name}.jsonl"));
    let log: Arc<dyn SessionLog> =
        JsonlSessionLog::create(name, &log_path, bus.clone()).expect("journal");
    let ctx = boot(bus.clone(), script, Arc::new(tools), log).expect("boot");
    let engine = TurnEngine::from_context(
        &ctx,
        TurnConfig {
            model: "scripted-1".into(),
            ..TurnConfig::default()
        },
    )
    .expect("engine");
    Composed {
        engine,
        bus,
        log_path,
    }
}

fn results(events: &[SessionEvent]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter(|event| event.ty == topic::TOOL_RESULT)
        .collect()
}

/// TC-PORT-FS-48: a turn reads a real file, writes a real file, and the disk
/// agrees.
///
/// Upstream: the tool suite over `ctx.fs` is what a coding agent does all day.
///
/// Input: a two-step script - read `notes.md`, then write `summary.md` - run as
/// one turn.
/// Expected: both results are `ok`, the new file is on disk with the content
/// the model sent, and the journal carries a `tool/call` and a `tool/result`
/// per call. This is the composition working: a fault anywhere from the
/// registry to the backend shows up here.
#[tokio::test]
async fn a_turn_reads_a_real_file_and_writes_a_real_file() {
    let fixture = Fixture::new();
    fixture.write("notes.md", "alpha\nbeta\n");
    let script = Script::new(
        vec![
            vec![call("c1", "read", json!({ "path": "notes.md" }))],
            vec![call(
                "c2",
                "write",
                json!({ "path": "summary.md", "content": "two notes\n" }),
            )],
        ],
        "Done: I read the notes and wrote a summary.",
    );
    let composed = compose(&fixture, "reads-and-writes", script);

    let outcome = composed
        .engine
        .run_turn("summarise the notes")
        .await
        .expect("turn");

    assert_eq!(outcome.steps, 3, "two tool steps and the step that answers");
    let events = composed.engine.log().events();
    let results = results(&events);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|event| event.data["ok"] == true));
    assert!(
        results[0].data["content"]
            .as_str()
            .expect("content")
            .contains("     1\talpha"),
        "the model read numbered lines: {}",
        results[0].data["content"]
    );
    assert_eq!(fixture.read("summary.md"), "two notes\n");
    assert_eq!(
        outcome.content,
        "Done: I read the notes and wrote a summary."
    );
}

/// TC-PORT-FS-49: a write outside the workspace is refused with something the
/// model can act on.
///
/// Upstream: `FS_SANDBOX_DENIED`, and the tool layer maps it to a model-facing
/// marker.
///
/// Input: a script that writes outside the workspace and then writes inside it.
/// Expected: the first result is `ok: false`, carries the class and names the
/// workspace, and creates nothing outside; the second lands. The second half is
/// the important half: a fence that stopped the turn would be a fence that
/// makes a model give up, and what this one does is redirect it.
#[tokio::test]
async fn a_write_outside_the_root_is_refused_and_the_turn_carries_on() {
    let fixture = Fixture::new();
    let escaped = fixture.outside().join("escaped.txt");
    let script = Script::new(
        vec![
            vec![call(
                "c1",
                "write",
                json!({ "path": escaped.display().to_string(), "content": "out\n" }),
            )],
            vec![call(
                "c2",
                "write",
                json!({ "path": "inside.txt", "content": "in\n" }),
            )],
        ],
        "I could not write outside the workspace, so I wrote inside it.",
    );
    let composed = compose(&fixture, "fenced-write", script);

    composed
        .engine
        .run_turn("write the file")
        .await
        .expect("turn");

    let events = composed.engine.log().events();
    let results = results(&events);
    assert_eq!(results[0].data["ok"], false);
    let refusal = results[0].data["content"].as_str().expect("content");
    assert!(refusal.starts_with("FS_SANDBOX_DENIED: "), "{refusal}");
    assert!(refusal.contains("Work inside the workspace"), "{refusal}");
    assert!(!escaped.exists(), "nothing was written outside the fence");
    assert_eq!(results[1].data["ok"], true);
    assert_eq!(fixture.read("inside.txt"), "in\n");
}

/// TC-PORT-FS-50: the delete a session cannot take back is decided first.
///
/// Contract §4.4.7 end to end: the gate, the audit, and the two outcomes.
///
/// Input: the same delete run twice - once with an answerer that rejects, once
/// with one that grants.
/// Expected: refused, the file still there, the result carrying
/// `TOOL_NOT_PERMITTED`; then granted, the file gone, and one audit pair per
/// run. The counter proves the second run reached the disk and the first did
/// not, which the result text alone could not distinguish.
#[tokio::test]
async fn a_delete_is_decided_before_it_happens_either_way() {
    for (answer, survives) in [
        (ApprovalOutcome::Rejected, true),
        (ApprovalOutcome::AllowedOnce, false),
    ] {
        let fixture = Fixture::new();
        fixture.write("doomed.txt", "content\n");
        let script = Script::new(
            vec![vec![call("c1", "delete", json!({ "path": "doomed.txt" }))]],
            "Handled.",
        );
        let composed = compose(&fixture, "gated-delete", script);
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let _answerer = composed
            .bus
            .on_waterfall::<ApprovalAsk, _>(move |ev, _next| {
                counter.fetch_add(1, Ordering::SeqCst);
                assert_eq!(ev.request.tool_name, "delete");
                assert!(
                    ev.request
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("cannot be undone")),
                    "whoever answers is told what disappears: {:?}",
                    ev.request.reason
                );
                Box::pin(async move { answer })
            });

        composed
            .engine
            .run_turn("delete the file")
            .await
            .expect("turn");

        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.exists("doomed.txt"), survives);
        let events = composed.engine.log().events();
        let result = results(&events)[0];
        assert_eq!(result.data["ok"], !survives);
        if survives {
            assert_eq!(result.data["code"], TOOL_NOT_PERMITTED);
        } else {
            assert!(result.data.get("code").is_none());
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| event.ty == topic::APPROVAL_ASKED)
                .count(),
            1,
            "one ask, one pair"
        );
        // The journal is the record, so the case reads it back from disk too:
        // an assertion against the live log alone would pass on a build that
        // never flushed what it decided.
        composed.engine.flush().await.expect("flush");
        let replayed = tetanus_session::replay(&composed.log_path).expect("replay");
        assert_eq!(
            replayed
                .iter()
                .filter(|event| event.ty == topic::APPROVAL_DECIDED)
                .map(|event| event.data["outcome"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string())
                .collect::<Vec<_>>(),
            vec![answer.as_str().to_string()]
        );
    }
}
