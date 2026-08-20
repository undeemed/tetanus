//! Test Design Specification: how one pending call is classified for overlap.
//!
//! Feature under test: [`ToolRegistry::mode`], which answers whether a call may
//! run beside its siblings in the same step. Upstream pins the same answers for
//! `ToolRuntime.executionMode` in
//! `packages/core/tools/tests/execution-mode.spec.ts`, where the rule is
//! fail-closed: parallel only for a classifier that explicitly said so, and
//! exclusive for everything else, including a classifier that throws.
//!
//! Approach: the registry alone. The scheduler that reads the class is
//! `TurnEngine::run_tool_calls`, and what it does with a class is the tool
//! pipeline's own suite; a case here would restate it.
//!
//! Features NOT tested here: the pipeline's ordering around an exclusive call,
//! and what a tool's body does once it runs (`upstream_tools.rs`). Two upstream
//! cases have no counterpart and are not restated: a classifier returning a
//! truthy non-boolean, and the type-level shape of the mode union. [`ToolMode`]
//! is a two-variant enum, so both are unrepresentable rather than unported -
//! `docs/parity.md` records that.
//!
//! Environmental needs: none. No case reaches a network, a clock or a file.
//!
//! One case panics on purpose. The default hook would print it and make a
//! passing run look broken, so that case installs a hook dropping exactly the
//! payload its own tool panics with; every other panic still prints.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic that escapes the
//! registry.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};

use serde_json::json;
use tetanus_turn::tools::{
    EchoTool, Tool, ToolCall, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};

/// TC-PORT-MODE-1: a classifier that says parallel gets parallel.
///
/// Upstream: "returns parallel only for an explicit true classifier".
///
/// Input: a registry holding `echo`, which reads nothing and writes nothing,
/// asked to classify an ordinary echo call.
/// Expected: [`ToolMode::Parallel`]. Nothing else in the suite can prove the
/// fail-closed default is a default rather than the only answer.
#[test]
fn a_classifier_that_says_parallel_gets_parallel() {
    let registry = ToolRegistry::new().with(Arc::new(EchoTool));

    assert_eq!(
        registry.mode(&call("echo", json!({ "text": "hi" }))),
        ToolMode::Parallel
    );
}

/// TC-PORT-MODE-2: a tool that declares nothing runs alone.
///
/// Upstream: "defaults to exclusive for a tool with no isConcurrencySafe
/// declaration".
///
/// Input: a tool that does not implement `mode` at all, so the trait's own
/// default answers.
/// Expected: [`ToolMode::Exclusive`]. Overlapping is opted into by a tool that
/// looked at its arguments, never granted by silence.
#[test]
fn a_tool_that_declares_nothing_runs_alone() {
    let registry = ToolRegistry::new().with(Arc::new(Silent));

    assert_eq!(
        registry.mode(&call("silent", json!({}))),
        ToolMode::Exclusive
    );
}

/// TC-PORT-MODE-3: a call naming no tool runs alone.
///
/// Upstream: "returns exclusive for an unknown tool".
///
/// Input: a registry holding only `echo`, asked to classify `ghost`.
/// Expected: [`ToolMode::Exclusive`]. The call is about to fail as unknown
/// (`upstream_tools.rs`, TC-PORT-TOOLS-1), and it fails on its own.
#[test]
fn a_call_naming_no_tool_runs_alone() {
    let registry = ToolRegistry::new().with(Arc::new(EchoTool));

    assert_eq!(
        registry.mode(&call("ghost", json!({}))),
        ToolMode::Exclusive
    );
}

/// TC-PORT-MODE-4: the class belongs to the call, not to the tool.
///
/// Upstream: "returns exclusive when the classifier returns false for these
/// args".
///
/// Input: one tool that reads or writes depending on `mode`, classified once
/// for a read and once for a write.
/// Expected: parallel for the read and exclusive for the write. The same tool
/// answering both is the whole reason the classifier takes arguments.
#[test]
fn the_class_belongs_to_the_call_not_the_tool() {
    let registry = ToolRegistry::new().with(Arc::new(ReadOrWrite::default()));

    assert_eq!(
        registry.mode(&call("rw", json!({ "mode": "read" }))),
        ToolMode::Parallel
    );
    assert_eq!(
        registry.mode(&call("rw", json!({ "mode": "write" }))),
        ToolMode::Exclusive
    );
}

/// TC-PORT-MODE-5: arguments the classifier cannot read run alone.
///
/// Upstream: "classifies invalid arguments as exclusive without throwing" -
/// there a schema parse fails before the classifier is asked.
/// tetanus hands the classifier the arguments as they arrived, so the same
/// answer has to come from the classifier itself.
///
/// Input: the read-or-write tool, classified for a call with no `mode` at all.
/// Expected: [`ToolMode::Exclusive`], and no panic. A tool that cannot tell
/// what it was asked to do must not guess that it is safe.
#[test]
fn arguments_the_classifier_cannot_read_run_alone() {
    let registry = ToolRegistry::new().with(Arc::new(ReadOrWrite::default()));

    assert_eq!(registry.mode(&call("rw", json!({}))), ToolMode::Exclusive);
}

/// TC-PORT-MODE-6: a classifier that panics runs its call alone, and the tool
/// still works.
///
/// Upstream: "treats a throwing raw classifier as exclusive".
///
/// Input: a tool whose `mode` panics, classified twice, then executed.
/// Expected: both classifications are exclusive, the panic does not escape the
/// registry, and the body still runs and answers. A classifier with a bug in it
/// costs the call its concurrency, not its result - containment that disabled
/// the tool would be the worse bug.
#[tokio::test]
async fn a_classifier_that_panics_runs_its_call_alone() {
    quiet_deliberate_panics();
    let hostile = Arc::new(HostileClassifier {
        classified: AtomicU32::new(0),
    });
    let registry = ToolRegistry::new().with(Arc::clone(&hostile) as Arc<dyn Tool>);

    assert_eq!(
        registry.mode(&call("hostile", json!({}))),
        ToolMode::Exclusive
    );
    assert_eq!(
        registry.mode(&call("hostile", json!({}))),
        ToolMode::Exclusive
    );
    assert_eq!(hostile.classified.load(Ordering::Relaxed), 2);

    let outcome = registry.execute(&call("hostile", json!({}))).await;
    assert_eq!(outcome.expect("the body runs"), ToolOutcome::ok("answered"));
}

/// TC-PORT-MODE-7: the classifier reads the arguments the call carried.
///
/// Upstream: "passes parsed arguments directly to a raw definition".
///
/// Input: a tool that records what it was classified with, given arguments no
/// schema mentions.
/// Expected: it sees exactly the JSON the call carried. A registry that reshaped
/// the arguments on the way in would classify a call the tool never saw.
#[test]
fn the_classifier_reads_the_arguments_the_call_carried() {
    let spy = Arc::new(ReadOrWrite::default());
    let registry = ToolRegistry::new().with(Arc::clone(&spy) as Arc<dyn Tool>);

    registry.mode(&call("rw", json!({ "anything": 1 })));

    assert_eq!(
        spy.seen.lock().expect("classified once").clone(),
        Some(json!({ "anything": 1 }))
    );
}

/// TC-PORT-MODE-8: the classifier never reaches the model.
///
/// Upstream: "isConcurrencySafe never reaches the model-facing schemas()
/// projection".
///
/// Input: the schema of a tool that classifies, serialized as the model would
/// read it.
/// Expected: exactly the keys `description`, `name` and `parameters`. The class
/// is the scheduler's business, and a model shown it would start reasoning about
/// the harness instead of the task.
#[test]
fn the_classifier_never_reaches_the_model() {
    let registry = ToolRegistry::new().with(Arc::new(ReadOrWrite::default()));

    let schema = serde_json::to_value(&registry.schemas()[0]).expect("serialize");
    let mut keys: Vec<&str> = schema
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();

    assert_eq!(keys, ["description", "name", "parameters"]);
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call_{name}"),
        name: name.into(),
        arguments,
    }
}

fn schema(name: &'static str) -> ToolSchema {
    ToolSchema {
        name: name.into(),
        description: "A tool the classifier cases register.".into(),
        parameters: json!({ "type": "object", "properties": {} }),
    }
}

/// A tool that says nothing about overlapping, so the trait's default answers.
struct Silent;

#[async_trait::async_trait]
impl Tool for Silent {
    fn schema(&self) -> ToolSchema {
        schema("silent")
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(""))
    }
}

/// A tool that is safe to overlap for a read and not for a write, and that
/// remembers what it was last asked to classify.
#[derive(Default)]
struct ReadOrWrite {
    seen: Mutex<Option<serde_json::Value>>,
}

#[async_trait::async_trait]
impl Tool for ReadOrWrite {
    fn schema(&self) -> ToolSchema {
        schema("rw")
    }

    fn mode(&self, arguments: &serde_json::Value) -> ToolMode {
        *self.seen.lock().expect("record") = Some(arguments.clone());
        match arguments.get("mode").and_then(serde_json::Value::as_str) {
            Some("read") => ToolMode::Parallel,
            _ => ToolMode::Exclusive,
        }
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(""))
    }
}

/// A tool whose classifier panics and whose body does not.
struct HostileClassifier {
    classified: AtomicU32,
}

#[async_trait::async_trait]
impl Tool for HostileClassifier {
    fn schema(&self) -> ToolSchema {
        schema("hostile")
    }

    fn mode(&self, _arguments: &serde_json::Value) -> ToolMode {
        self.classified.fetch_add(1, Ordering::Relaxed);
        panic!("{DELIBERATE}");
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok("answered"))
    }
}

/// What the classifier above panics with. A hook cannot be scoped to one call,
/// so the case recognises its own panic by payload rather than silencing the
/// run.
const DELIBERATE: &str = "classifier exploded";

static QUIET: Once = Once::new();

/// Drop the printout for the panic this suite asks for, and only that one.
fn quiet_deliberate_panics() {
    QUIET.call_once(|| {
        let inherited = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = info
                .payload()
                .downcast_ref::<String>()
                .is_some_and(|message| message == DELIBERATE);
            if !ours {
                inherited(info);
            }
        }));
    });
}
