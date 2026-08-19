//! Test Design Specification: a call that fails is the model's news, not the
//! turn's end.
//!
//! Features under test: what [`ToolRegistry::execute`] does with a call that
//! names no tool, and with a tool whose body panics instead of returning.
//! Upstream pins the same two in `packages/core/tools/tests/tools.spec.ts`,
//! where both come back as an `isError` result the model reads.
//!
//! Approach: the registry alone for the failure classes, then one end-to-end
//! turn for the claim that matters - a tool with a bug leaves the documented
//! event sequence unchanged. Upstream's error `code` and `name` fields have no
//! counterpart: in tetanus the [`ToolError`] variant is the class, so a case
//! pins the variant and its rendered message instead.
//!
//! These cases panic on purpose. The default hook would print each one and make
//! a passing run look broken, so the suite installs a hook that drops exactly
//! the payloads its own tools panic with. Every other panic, a failed assertion
//! included, still prints.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic that escapes the
//! registry.

mod harness;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Once};

use harness::{Harness, MOCK_TURN_FLOW};
use serde_json::json;
use tetanus_turn::events::StopReason;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{
    EchoTool, Tool, ToolCall, ToolError, ToolOutcome, ToolRegistry, ToolSchema,
};

/// TC-PORT-TOOLS-1: a call naming no registered tool fails with a stable
/// message.
///
/// Upstream: "ToolNotFoundError carries a stable message and code", and the
/// unknown half of "returns isError results for unknown tools and throwing
/// tools".
///
/// Input: a registry holding only `echo`, asked to run `ghost`.
/// Expected: [`ToolError::Unknown`] naming `ghost`, rendering exactly
/// `unknown tool "ghost"` - the same sentence upstream promises, so a caller
/// may match on the text and a model reading it is told which tool was missing.
#[tokio::test]
async fn an_unknown_tool_fails_with_a_stable_message() {
    let registry = ToolRegistry::new().with(Arc::new(EchoTool));

    let err = registry
        .execute(&call("ghost", json!({})))
        .await
        .expect_err("nothing answers to that name");

    assert!(matches!(err, ToolError::Unknown(ref name) if name == "ghost"));
    assert_eq!(err.to_string(), "unknown tool \"ghost\"");
}

/// TC-PORT-TOOLS-2: a tool whose body panics fails that one call.
///
/// Upstream: the throwing half of "returns isError results for unknown tools
/// and throwing tools" - a thrown value becomes `Error: exploded` on the
/// result, not an exception the caller has to survive.
///
/// Input: a registered tool whose body panics with `exploded`.
/// Expected: the call returns [`ToolError::Failed`] naming the tool and
/// carrying the panic message. The panic does not reach the caller, so the
/// engine can commit a `tool/result` and go on.
#[tokio::test]
async fn a_tool_that_panics_fails_that_call() {
    quiet_deliberate_panics();
    let registry = ToolRegistry::new().with(Arc::new(Boom { name: "boom" }));

    let err = registry
        .execute(&call("boom", json!({})))
        .await
        .expect_err("the body panicked");

    assert!(matches!(err, ToolError::Failed(ref name, _) if name == "boom"));
    assert_eq!(err.to_string(), "tool \"boom\" failed: exploded");
}

/// TC-PORT-TOOLS-3: a panic carrying something unprintable still reads as a
/// failure.
///
/// Upstream: "normalizes a hostile thrown value whose inspection and coercion
/// both throw", which lands as `Error: <unprintable thrown value>`.
///
/// Input: a tool that panics with a payload that is neither string type.
/// Expected: the same [`ToolError::Failed`] shape, with a stated placeholder
/// instead of a message. Nothing about the payload can make the call
/// unreportable.
#[tokio::test]
async fn a_panic_with_no_readable_message_still_fails_the_call() {
    quiet_deliberate_panics();
    let registry = ToolRegistry::new().with(Arc::new(Hostile));

    let err = registry
        .execute(&call("hostile", json!({})))
        .await
        .expect_err("the body panicked");

    assert_eq!(
        err.to_string(),
        "tool \"hostile\" failed: <unprintable panic payload>"
    );
}

/// TC-PORT-TOOLS-4: a tool with a bug does not take the turn down.
///
/// Upstream keeps this claim on the runtime rather than the loop; tetanus
/// states it where it is visible, on a whole turn.
///
/// Input: one mock turn whose only registered tool - the one the model calls -
/// panics.
/// Expected: the trace equals [`MOCK_TURN_FLOW`], so a failing tool changes no
/// part of the sequence; the durable `tool/result` says `ok: false` and carries
/// the failure; the second request carries that same text back to the model;
/// and the turn stops naturally, answering with what it was told.
#[tokio::test]
async fn a_turn_survives_a_tool_with_a_bug() {
    quiet_deliberate_panics();
    let h = Harness::with_tools(
        "port-tools-bug",
        ToolRegistry::new().with(Arc::new(Boom { name: "echo" })),
    )
    .await;

    let outcome = h
        .engine
        .run_turn("do the thing")
        .await
        .expect("the turn is not the tool author's to fail");

    assert_eq!(h.trace(), MOCK_TURN_FLOW, "documented turn flow");

    let result = tetanus_session::replay(&h.log_path)
        .expect("journal")
        .into_iter()
        .find(|e| e.ty == topic::TOOL_RESULT)
        .expect("tool/result");
    assert_eq!(result.data["ok"], false);
    assert_eq!(result.data["content"], "tool \"echo\" failed: exploded");

    assert_eq!(outcome.reason, StopReason::Natural);
    assert_eq!(
        outcome.content, "You said: tool \"echo\" failed: exploded",
        "the model answered from the failure it was told about"
    );
}

/// TC-PORT-TOOLS-5: containment latches nothing.
///
/// Upstream runs each dispatch on its own, so a thrown call leaves the runtime
/// usable; tetanus states that directly, because containment that disabled the
/// tool would be a worse bug than the panic.
///
/// Input: a tool that panics on its first call and answers on its second, run
/// twice through the same registry.
/// Expected: the first call fails, the second returns `recovered`, and the body
/// ran both times.
#[tokio::test]
async fn a_contained_panic_does_not_disable_the_tool() {
    quiet_deliberate_panics();
    let flaky = Arc::new(Flaky {
        calls: AtomicU32::new(0),
    });
    let registry = ToolRegistry::new().with(Arc::clone(&flaky) as Arc<dyn Tool>);

    let first = registry.execute(&call("flaky", json!({}))).await;
    let second = registry.execute(&call("flaky", json!({}))).await;

    assert!(first.is_err(), "{first:?}");
    assert_eq!(
        second.expect("the second call runs"),
        ToolOutcome::ok("recovered")
    );
    assert_eq!(flaky.calls.load(Ordering::Relaxed), 2);
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call_{name}"),
        name: name.into(),
        arguments,
    }
}

fn schema(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.into(),
        description: "A tool with a bug in it.".into(),
        parameters: json!({ "type": "object", "properties": {} }),
    }
}

/// What the tools below panic with. A hook cannot be scoped to one call, so the
/// suite recognises its own panics by payload rather than silencing the run.
const DELIBERATE: &str = "exploded";

static QUIET: Once = Once::new();

/// Drop the printout for the panics this suite asks for, and only those.
fn quiet_deliberate_panics() {
    QUIET.call_once(|| {
        let inherited = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = info.payload();
            let ours = payload
                .downcast_ref::<String>()
                .is_some_and(|message| message == DELIBERATE)
                || payload.downcast_ref::<u8>() == Some(&HOSTILE_PAYLOAD);
            if !ours {
                inherited(info);
            }
        }));
    });
}

/// A tool whose body panics with a message.
struct Boom {
    name: &'static str,
}

#[async_trait::async_trait]
impl Tool for Boom {
    fn schema(&self) -> ToolSchema {
        schema(self.name)
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        panic!("{DELIBERATE}")
    }
}

/// A tool whose panic payload is neither `&str` nor `String`.
struct Hostile;

/// The payload `Hostile` panics with: a value the containment cannot print.
const HOSTILE_PAYLOAD: u8 = 7;

#[async_trait::async_trait]
impl Tool for Hostile {
    fn schema(&self) -> ToolSchema {
        schema("hostile")
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        std::panic::panic_any(HOSTILE_PAYLOAD)
    }
}

/// A tool that panics once and works afterwards.
struct Flaky {
    calls: AtomicU32,
}

#[async_trait::async_trait]
impl Tool for Flaky {
    fn schema(&self) -> ToolSchema {
        schema("flaky")
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            panic!("{DELIBERATE}");
        }
        Ok(ToolOutcome::ok("recovered"))
    }
}
