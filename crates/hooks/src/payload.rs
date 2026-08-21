//! What each dialect writes to a hook's stdin.
//!
//! A hook is told what is happening as a JSON object on stdin, and the two
//! dialects describe the same event differently. These are the shapes, kept
//! together so the differences are visible side by side rather than buried in
//! two adapters.
//!
//! # The differences that matter
//!
//! Four of them, each of which would silently break a real hook:
//!
//! | | Claude Code | Codex |
//! | --- | --- | --- |
//! | no transcript yet | `""` | `null` |
//! | `tool_input` | the call's arguments, verbatim | just `{ "command": … }` |
//! | every payload also carries | — | `model`, `permission_mode` |
//! | turn-scoped events also carry | — | `turn_id`, as a **string** |
//!
//! A hook written for one dialect reads the other's payload as missing data,
//! not as an error, so each of these is a case rather than a comment.
//!
//! # What is not here
//!
//! Registering these against the interception points — deciding *when* a
//! `PreToolUse` fires — is the bridge, and it needs interception points the
//! turn engine does not have yet. The payloads are separable, pure, and where
//! the parity risk actually lives, so they land first. See
//! `docs/parity-updates/core-hook-payloads.md`.
//!
//! Parity: the payload builders of upstream
//! `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`.

use serde_json::{json, Map, Value};

/// Claude Code's default subagent type.
pub const SUBAGENT_TYPE: &str = "general-purpose";

/// The facts every payload is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PayloadContext {
    /// The session this happened in, or empty when there is no session yet.
    pub session_id: String,
    /// Where the journal is on disk, when it has been written anywhere.
    pub transcript_path: Option<String>,
    /// The working directory the agent is running in.
    pub cwd: String,
    /// The open turn. Only the turn-scoped Codex events carry it.
    pub turn: u64,
}

/// A tool call, as a hook is told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallFacts {
    /// The real tool name. It must be the name the matcher tests, or a
    /// deployment's tool matcher silently never fires.
    pub tool_name: String,
    /// The arguments the model produced.
    pub arguments: Value,
    /// The id correlating this call with its result.
    pub tool_use_id: String,
}

// ------------------------------------------------------------- Claude Code

/// The fields on every Claude Code payload.
///
/// An absent transcript is `""` here and `null` in Codex. Neither is more
/// correct; both are what the respective hook ecosystem expects to read.
fn claude_base(context: &PayloadContext, event: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("session_id".into(), json!(context.session_id));
    payload.insert(
        "transcript_path".into(),
        json!(context.transcript_path.clone().unwrap_or_default()),
    );
    payload.insert("cwd".into(), json!(context.cwd));
    payload.insert("hook_event_name".into(), json!(event));
    payload
}

/// `SessionStart`, which says what started the session.
pub fn claude_session_start(context: &PayloadContext, source: &str) -> Value {
    let mut payload = claude_base(context, "SessionStart");
    payload.insert("source".into(), json!(source));
    Value::Object(payload)
}

/// `UserPromptSubmit`, carrying the prompt as text.
pub fn claude_prompt(context: &PayloadContext, prompt: &str) -> Value {
    let mut payload = claude_base(context, "UserPromptSubmit");
    payload.insert("prompt".into(), json!(prompt));
    Value::Object(payload)
}

/// `PreToolUse`. The arguments go through verbatim, which is what lets a
/// Claude Code hook inspect any tool's input rather than only a shell one.
pub fn claude_pre_tool(context: &PayloadContext, call: &ToolCallFacts) -> Value {
    let mut payload = claude_base(context, "PreToolUse");
    insert_tool(&mut payload, call, call.arguments.clone());
    Value::Object(payload)
}

/// `PostToolUse`, which adds what the tool produced.
pub fn claude_post_tool(context: &PayloadContext, call: &ToolCallFacts, response: &str) -> Value {
    let mut payload = claude_base(context, "PostToolUse");
    insert_tool(&mut payload, call, call.arguments.clone());
    payload.insert("tool_response".into(), json!(response));
    Value::Object(payload)
}

/// `Stop`. The loop-guard flag is always false: this harness does not re-enter
/// a stop hook, so a hook that checks it must see that it is not a re-entry.
pub fn claude_stop(context: &PayloadContext) -> Value {
    let mut payload = claude_base(context, "Stop");
    payload.insert("stop_hook_active".into(), json!(false));
    Value::Object(payload)
}

/// `SubagentStart` and `SubagentStop`, described by the *child's* context.
///
/// `stop_hook_active` appears on the stop half only, matching the shape of the
/// top-level `Stop` payload.
pub fn claude_subagent(child: &PayloadContext, event: &str, agent_id: &str) -> Value {
    let mut payload = claude_base(child, event);
    payload.insert("agent_id".into(), json!(agent_id));
    payload.insert("agent_type".into(), json!(SUBAGENT_TYPE));
    if event == "SubagentStop" {
        payload.insert("stop_hook_active".into(), json!(false));
    }
    Value::Object(payload)
}

// ------------------------------------------------------------------- Codex

/// The fields on every Codex payload.
fn codex_base(context: &PayloadContext, event: &str, model: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("session_id".into(), json!(context.session_id));
    payload.insert(
        "transcript_path".into(),
        // `null`, not `""`: this dialect distinguishes "not written anywhere
        // yet" from "written to a path with no name".
        match &context.transcript_path {
            Some(path) => json!(path),
            None => Value::Null,
        },
    );
    payload.insert("cwd".into(), json!(context.cwd));
    payload.insert("hook_event_name".into(), json!(event));
    payload.insert("model".into(), json!(model));
    payload.insert("permission_mode".into(), json!("default"));
    payload
}

/// Base plus `turn_id`, for the turn-scoped events.
///
/// The turn is a *string* on the wire even though it is a number everywhere
/// else, because that is what this dialect's hooks parse.
fn codex_turn_base(context: &PayloadContext, event: &str, model: &str) -> Map<String, Value> {
    let mut payload = codex_base(context, event, model);
    payload.insert("turn_id".into(), json!(context.turn.to_string()));
    payload
}

/// `SessionStart`, which is not turn-scoped.
pub fn codex_session_start(context: &PayloadContext, model: &str) -> Value {
    Value::Object(codex_base(context, "SessionStart", model))
}

/// `UserPromptSubmit`.
pub fn codex_prompt(context: &PayloadContext, model: &str, prompt: &str) -> Value {
    let mut payload = codex_turn_base(context, "UserPromptSubmit", model);
    payload.insert("prompt".into(), json!(prompt));
    Value::Object(payload)
}

/// `PreToolUse`.
///
/// `tool_input` is narrowed to `{ "command": … }` because that is the shape
/// this dialect's hooks read. The tool *name* is still the real one, since it
/// is what the matcher tests — a constant there would make a configured tool
/// matcher never fire.
pub fn codex_pre_tool(context: &PayloadContext, model: &str, call: &ToolCallFacts) -> Value {
    let mut payload = codex_turn_base(context, "PreToolUse", model);
    insert_tool(
        &mut payload,
        call,
        json!({ "command": command_of(&call.arguments) }),
    );
    Value::Object(payload)
}

/// `PostToolUse`.
pub fn codex_post_tool(
    context: &PayloadContext,
    model: &str,
    call: &ToolCallFacts,
    response: &str,
) -> Value {
    let mut payload = codex_turn_base(context, "PostToolUse", model);
    insert_tool(
        &mut payload,
        call,
        json!({ "command": command_of(&call.arguments) }),
    );
    payload.insert("tool_response".into(), json!(response));
    Value::Object(payload)
}

/// `Stop`.
pub fn codex_stop(context: &PayloadContext, model: &str) -> Value {
    Value::Object(codex_turn_base(context, "Stop", model))
}

// ------------------------------------------------------------------ shared

/// The three tool fields, in the order both dialects write them.
fn insert_tool(payload: &mut Map<String, Value>, call: &ToolCallFacts, input: Value) {
    payload.insert("tool_name".into(), json!(call.tool_name));
    payload.insert("tool_input".into(), input);
    payload.insert("tool_use_id".into(), json!(call.tool_use_id));
}

/// The `command` argument of a call, or empty when it has none.
///
/// Empty rather than absent: this dialect's hooks index into `tool_input`, and
/// a missing key would be a different failure from an empty command.
fn command_of(arguments: &Value) -> &str {
    arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
}
