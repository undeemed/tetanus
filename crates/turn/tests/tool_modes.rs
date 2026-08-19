//! Test Design Specification: how a pending tool call is classified.
//!
//! Feature under test: [`ToolMode`] and the two ways it is read - a tool
//! classifying one of its own calls, and the registry classifying a call it may
//! not recognise. Upstream calls the same decision `executionMode()`
//! (`packages/core/tools`), and it is fail-closed there for the same reason.
//!
//! Approach: the registry alone, with no engine and no turn. What the scheduler
//! then does with the answer is a separate suite, driven end to end.
//!
//! Upstream also pins that a tool whose `executionMode()` throws is treated as
//! exclusive. A Rust `mode()` returns a `ToolMode` and cannot fail, so that
//! case has nothing to restate.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use serde_json::json;
use tetanus_turn::tools::{
    EchoTool, Tool, ToolCall, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};

/// TC-TOOL-MODE-1: a tool that says nothing about overlap is exclusive.
///
/// Upstream: `concurrencySafe === true ? parallel : exclusive` - anything that
/// is not an explicit yes is a no.
///
/// Input: a tool that does not override `mode`.
/// Expected: [`ToolMode::Exclusive`]. Overlapping is opted into, never granted
/// by silence, so a tool written before this seam existed cannot be run beside
/// a sibling by accident.
#[test]
fn a_tool_that_says_nothing_is_exclusive() {
    let registry = ToolRegistry::new().with(Arc::new(Quiet));

    assert_eq!(
        registry.mode(&call("quiet", json!({}))),
        ToolMode::Exclusive
    );
}

/// TC-TOOL-MODE-2: the class belongs to the call, not to the tool.
///
/// Upstream: `executionMode(args)` is given the arguments, so one tool can be
/// safe for a read and unsafe for a write.
///
/// Input: one tool, asked about two calls that differ only in their arguments.
/// Expected: parallel-safe for the read, exclusive for the write.
#[test]
fn the_same_tool_classifies_its_calls_separately() {
    let registry = ToolRegistry::new().with(Arc::new(Access));

    let read = registry.mode(&call("access", json!({ "write": false })));
    let write = registry.mode(&call("access", json!({ "write": true })));

    assert_eq!(read, ToolMode::Parallel);
    assert_eq!(write, ToolMode::Exclusive);
}

/// TC-TOOL-MODE-3: the built-in `echo` is parallel-safe.
///
/// Input: an `echo` call.
/// Expected: [`ToolMode::Parallel`]. Echoing reads nothing and writes nothing,
/// so it is the one shipped tool that may overlap, and it is what the
/// conformance fixture schedules.
#[test]
fn the_built_in_echo_may_overlap() {
    let registry = ToolRegistry::new().with(Arc::new(EchoTool));

    assert_eq!(
        registry.mode(&call("echo", json!({ "text": "hi" }))),
        ToolMode::Parallel
    );
}

/// TC-TOOL-MODE-4: a call naming no registered tool is exclusive.
///
/// Input: a call on an empty registry.
/// Expected: [`ToolMode::Exclusive`]. The call is about to fail as unknown, and
/// classifying it as safe would let it start beside work it was never checked
/// against.
#[test]
fn an_unknown_call_is_exclusive() {
    let registry = ToolRegistry::new();

    assert_eq!(
        registry.mode(&call("nowhere", json!({}))),
        ToolMode::Exclusive
    );
}

/// A tool that leaves `mode` at its default.
struct Quiet;

#[async_trait::async_trait]
impl Tool for Quiet {
    fn schema(&self) -> ToolSchema {
        schema("quiet")
    }
    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(""))
    }
}

/// A tool whose class depends on what the call asks it to do.
struct Access;

#[async_trait::async_trait]
impl Tool for Access {
    fn schema(&self) -> ToolSchema {
        schema("access")
    }
    fn mode(&self, arguments: &serde_json::Value) -> ToolMode {
        match arguments["write"].as_bool().unwrap_or(true) {
            true => ToolMode::Exclusive,
            false => ToolMode::Parallel,
        }
    }
    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(""))
    }
}

fn schema(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: "A tool that exists only to be classified.".into(),
        parameters: json!({ "type": "object" }),
    }
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments,
    }
}
