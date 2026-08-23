//! The six terminal tools a model can call: `terminal_open`, `terminal_send`,
//! `terminal_read`, `terminal_signal`, `terminal_close` and `terminal_list`.
//!
//! Upstream's `tool-terminal` family, over [`crate::terminals`]. They are
//! beside the `shell_*` family rather than instead of it, because the two
//! answer different questions: `shell_run` feeds a command to a shell reading
//! a pipe and hands back everything it printed, which is what most work wants,
//! and `terminal_send` types at a real terminal, which is the only thing that
//! can drive a program that refuses to run without one - a REPL, `ssh`, `git
//! rebase -i`, anything that asks for a password. The description of each says
//! which is which, because a model that reaches for a terminal when a command
//! would do pays for a whole session it then has to remember to close.
//!
//! **What the model reads is rendered text with markers**, as everywhere else
//! in this crate: `[wait: …]` says why a send came back, `[session: …]` says
//! whether the shell is still there, `[exit code: N]` is on a send the shell
//! reported a failure for, `[lines: a-b of N]` puts a page in its context, and
//! `[output truncated]` says the bound ate something. They are the same
//! markers upstream renders, so a presentation that parses one parses both.
//!
//! **`ok` means the command succeeded.** A send that came back on a prompt
//! with a non-zero status is a failed result carrying the text, for the reason
//! [`crate::tools`] gives: the flag can afford to mean the plain thing because
//! the markers carry the detail.
//!
//! Parity: upstream `packages/terminal/tool-terminal`.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use serde_json::Value;
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{
    Tool, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema, REDACTED,
};

use crate::terminal::{Status, TerminalError, TerminalSession, TerminalSignal, WaitReason};
use crate::terminals::{Closed, OpenRequest, Owner, Terminals};

pub const TERMINAL_OPEN: &str = "terminal_open";
pub const TERMINAL_SEND: &str = "terminal_send";
pub const TERMINAL_READ: &str = "terminal_read";
pub const TERMINAL_SIGNAL: &str = "terminal_signal";
pub const TERMINAL_CLOSE: &str = "terminal_close";
pub const TERMINAL_LIST: &str = "terminal_list";

/// The most one rendered result may carry. Upstream's default, and for the
/// same reason: a result is a message in the next request, so an unbounded one
/// is a context window spent on a `yes` loop.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;

/// Everything the six tools share.
pub struct TerminalTools {
    terminals: Arc<Terminals>,
    /// Who these tools open sessions as. One registry per session today, so
    /// this is the composition's own name; the scoping is the registry's.
    owner: Owner,
    /// The turn's stop switch. A send a stopped turn is no longer waiting for
    /// interrupts the command rather than abandoning it.
    interrupt: Arc<Interrupt>,
    max_result_bytes: usize,
}

impl TerminalTools {
    pub fn new(terminals: Arc<Terminals>, owner: Owner, interrupt: Arc<Interrupt>) -> Arc<Self> {
        Arc::new(Self {
            terminals,
            owner,
            interrupt,
            max_result_bytes: MAX_RESULT_BYTES,
        })
    }

    /// Register all six on `registry`.
    pub fn register(self: &Arc<Self>, registry: &mut ToolRegistry) {
        registry.register(Arc::new(OpenTool(Arc::clone(self))));
        registry.register(Arc::new(SendTool(Arc::clone(self))));
        registry.register(Arc::new(ReadTool(Arc::clone(self))));
        registry.register(Arc::new(SignalTool(Arc::clone(self))));
        registry.register(Arc::new(CloseTool(Arc::clone(self))));
        registry.register(Arc::new(ListTool(Arc::clone(self))));
    }

    /// A registry holding exactly these six, for a caller composing one from
    /// nothing.
    pub fn registry(self: &Arc<Self>) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        self.register(&mut registry);
        registry
    }

    /// The sessions these tools have open, for a composition that has to close
    /// them on the way down.
    pub fn terminals(&self) -> &Arc<Terminals> {
        &self.terminals
    }

    fn session(&self, tool: &str, arguments: &Value) -> Result<Arc<TerminalSession>, ToolError> {
        let id = text(arguments, "session_id", tool)?;
        self.terminals
            .get(&self.owner, &id)
            .map_err(|refused| refused_call(tool, refused))
    }
}

/// Open a terminal.
struct OpenTool(Arc<TerminalTools>);

#[async_trait::async_trait]
impl Tool for OpenTool {
    fn schema(&self) -> ToolSchema {
        let types = self.0.terminals.backends();
        ToolSchema {
            name: TERMINAL_OPEN.into(),
            description: format!(
                "Open a persistent terminal session - a real terminal, so a program that only \
                 runs on one (a REPL, `ssh`, an editor, anything that asks for a password) works \
                 here. Returns a session id to use with `{TERMINAL_SEND}`. Prefer `shell` or \
                 `shell_run` for ordinary commands: a terminal costs a session you have to close \
                 with `{TERMINAL_CLOSE}` when the work is done."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": types,
                        "description": "Which shell to run. Defaults to the first one this deployment offers.",
                    },
                    "name": {
                        "type": "string",
                        "description": "A name for you to tell your own sessions apart by, such as \"build\" or \"psql\".",
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Where the shell starts. Relative paths resolve against the workspace root.",
                    },
                },
                "additionalProperties": false,
            }),
        }
    }

    /// Starting a shell writes nothing outside the process, but the session it
    /// publishes is state the calls after it name: a barrier, so an id cannot
    /// be minted while something else in the same step is reading the list.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let request = OpenRequest {
            kind: optional_text(arguments, "type", TERMINAL_OPEN)?,
            name: optional_text(arguments, "name", TERMINAL_OPEN)?,
            cwd: optional_text(arguments, "cwd", TERMINAL_OPEN)?.map(std::path::PathBuf::from),
        };
        let session = self
            .0
            .terminals
            .open(&self.0.owner, request)
            .await
            .map_err(|refused| refused_call(TERMINAL_OPEN, refused))?;
        Ok(ToolOutcome::ok(bounded(
            render_open(&session),
            self.0.max_result_bytes,
        )))
    }
}

/// Type at a terminal.
struct SendTool(Arc<TerminalTools>);

#[async_trait::async_trait]
impl Tool for SendTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: TERMINAL_SEND.into(),
            description: format!(
                "Type text at a terminal session and wait for it to answer. Enter is pressed for \
                 you unless `submit` is false, which is how you send a control character (`\\u0003` \
                 is Ctrl-C) or half a line to a REPL. The answer says why it came back: \
                 `[wait: stdin_read]` is the shell asking for input again, with `[exit code: N]` \
                 when the command failed; `[wait: inferred_idle]` and `[wait: timeout]` mean the \
                 command is probably still running, so read with `{TERMINAL_READ}` or stop it \
                 with `{TERMINAL_SIGNAL}` rather than sending another command."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id `terminal_open` returned.",
                    },
                    "text": {
                        "type": "string",
                        "description": "What to type. May be empty with `submit` true, which is just pressing Enter.",
                    },
                    "submit": {
                        "type": "boolean",
                        "description": "Press Enter after the text. Defaults to true.",
                    },
                    "wait_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "How long to wait for an answer before returning and leaving the command running. Defaults to the deployment's own bound, which also caps this.",
                    },
                    "secret": {
                        "type": "boolean",
                        "description": "Set this when the text is a password, a token or anything else that must not be written down. The terminal still receives it; the session journal keeps `<redacted>` in its place. Set it whenever you are answering a prompt that is not echoing what you type.",
                    },
                },
                "required": ["session_id", "text"],
                "additionalProperties": false,
            }),
        }
    }

    /// One terminal runs one thing at a time, and what it runs can write
    /// anything: a barrier, like `shell`.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    /// The text of a send is the one argument in this crate that routinely
    /// carries a credential: this tool exists so a model can drive `ssh`,
    /// `sudo` and anything else that asks for a password, and the answer is an
    /// ordinary string argument. When the model says so, the journal keeps the
    /// sentinel and the terminal still gets the password.
    ///
    /// Only `text` is withheld. The session id, the flags and the tool's own
    /// name stay, so the record still says that a secret was typed at that
    /// terminal at that moment - which is what an audit needs and what a
    /// blanket redaction would destroy.
    fn recorded(&self, arguments: &Value) -> Value {
        if !arguments
            .get("secret")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return arguments.clone();
        }
        let mut recorded = arguments.clone();
        if let Some(object) = recorded.as_object_mut() {
            object.insert("text".to_string(), Value::String(REDACTED.to_string()));
        }
        recorded
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let session = self.0.session(TERMINAL_SEND, arguments)?;
        let typed = match arguments.get("text") {
            Some(Value::String(text)) => text.clone(),
            None | Some(Value::Null) => {
                return Err(ToolError::InvalidArguments(
                    TERMINAL_SEND.into(),
                    "missing `text`; send an empty string with `submit` to press Enter".into(),
                ))
            }
            Some(other) => {
                return Err(ToolError::InvalidArguments(
                    TERMINAL_SEND.into(),
                    format!("`text` must be a string, got {other}"),
                ))
            }
        };
        let submit = optional_flag(arguments, "submit", TERMINAL_SEND)?.unwrap_or(true);
        let within = optional_millis(arguments, "wait_ms", TERMINAL_SEND)?;

        match session
            .send_waiting(&typed, submit, within, Some(&self.0.interrupt))
            .await
        {
            Ok(outcome) => {
                let text = bounded(render_send(&outcome), self.0.max_result_bytes);
                // A send that came back on a prompt reporting a failure is a
                // failed result; one that came back on silence or a deadline
                // has no status to judge, so it is the plain answer it is.
                let failed = outcome.code.is_some_and(|code| code != 0)
                    || matches!(outcome.wait, WaitReason::SessionExit);
                Ok(if failed {
                    ToolOutcome::failed(text)
                } else {
                    ToolOutcome::ok(text)
                })
            }
            // Everything that went wrong with the session is told to the model
            // as a failed result rather than raised: its next move - open
            // another session, read what is there, give up on this one -
            // depends on reading it.
            Err(refused) => Ok(ToolOutcome::failed(refused.to_string())),
        }
    }
}

/// Read back what a terminal has printed.
struct ReadTool(Arc<TerminalTools>);

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: TERMINAL_READ.into(),
            description: "Read a page of what a terminal session has printed, without typing \
                          anything at it. Lines are counted back from the newest, so `offset` 0 \
                          is the end of the transcript and `offset` 100 is the hundred lines \
                          before that."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id `terminal_open` returned.",
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many lines back from the newest this page starts. Defaults to 0.",
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "How many lines to read. Defaults to 500; long pages are cut to their tail.",
                    },
                },
                "required": ["session_id"],
                "additionalProperties": false,
            }),
        }
    }

    /// Reading a transcript touches nothing outside the process, so it is safe
    /// beside its siblings.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let session = self.0.session(TERMINAL_READ, arguments)?;
        let offset = optional_count(arguments, "offset", TERMINAL_READ)?.unwrap_or(0);
        let count = optional_count(arguments, "count", TERMINAL_READ)?;
        let page = session
            .read(offset, count)
            .map_err(|refused| refused_call(TERMINAL_READ, refused))?;
        Ok(ToolOutcome::ok(bounded(
            render_read(&page),
            self.0.max_result_bytes,
        )))
    }
}

/// Signal whatever a terminal is running.
struct SignalTool(Arc<TerminalTools>);

#[async_trait::async_trait]
impl Tool for SignalTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: TERMINAL_SIGNAL.into(),
            description: format!(
                "Send a signal to whatever a terminal session is running now - what pressing \
                 Ctrl-C at a terminal does. It reaches the command, not the shell, so the \
                 session survives. A signal that would end the shell itself is refused: close \
                 the session with `{TERMINAL_CLOSE}` instead."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id `terminal_open` returned.",
                    },
                    "signal": {
                        "type": "string",
                        "enum": TerminalSignal::NAMES,
                        "description": "Which signal. SIGINT is Ctrl-C, SIGTSTP is Ctrl-Z.",
                    },
                },
                "required": ["session_id", "signal"],
                "additionalProperties": false,
            }),
        }
    }

    /// Delivering a signal changes what a running command does, and nothing
    /// else: safe beside the reads it will be batched with.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let session = self.0.session(TERMINAL_SIGNAL, arguments)?;
        let named = text(arguments, "signal", TERMINAL_SIGNAL)?;
        let Some(signal) = TerminalSignal::parse(&named) else {
            return Err(ToolError::InvalidArguments(
                TERMINAL_SIGNAL.into(),
                format!(
                    "`signal` must be one of {}, not {named:?}",
                    TerminalSignal::NAMES.join(", ")
                ),
            ));
        };
        match session.signal(signal) {
            Ok(group) => Ok(ToolOutcome::ok(format!(
                "delivered {name} to foreground process group {group}",
                name = signal.name()
            ))),
            // A refusal here is about this terminal's state - the shell is
            // idle, or has gone - which the model has to read to know what to
            // do next.
            Err(refused) => Ok(ToolOutcome::failed(refused.to_string())),
        }
    }
}

/// Close a terminal.
struct CloseTool(Arc<TerminalTools>);

#[async_trait::async_trait]
impl Tool for CloseTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: TERMINAL_CLOSE.into(),
            description: "Close a terminal session and wait until its shell and everything it \
                          started are gone."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id `terminal_open` returned.",
                    },
                },
                "required": ["session_id"],
                "additionalProperties": false,
            }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let id = text(arguments, "session_id", TERMINAL_CLOSE)?;
        match self.0.terminals.kill(&self.0.owner, &id).await {
            Ok(Closed::Now) => Ok(ToolOutcome::ok(format!("closed terminal session {id}"))),
            Ok(Closed::Already) => Ok(ToolOutcome::ok(format!(
                "terminal session {id} was already closing"
            ))),
            Err(refused) => Ok(ToolOutcome::failed(refused.to_string())),
        }
    }
}

/// List the terminals that are open.
struct ListTool(Arc<TerminalTools>);

#[async_trait::async_trait]
impl Tool for ListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: TERMINAL_LIST.into(),
            description: "List your open terminal sessions, with what each one runs, where it \
                          started, and whether its shell is still alive."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, _arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let sessions = self.0.terminals.list(&self.0.owner);
        if sessions.is_empty() {
            return Ok(ToolOutcome::ok("no terminal sessions are open"));
        }
        let rows: Vec<String> = sessions.iter().map(|session| render_row(session)).collect();
        Ok(ToolOutcome::ok(bounded(
            rows.join("\n"),
            self.0.max_result_bytes,
        )))
    }
}

// ------------------------------------------------------------- rendering

/// One opened session, and whatever its shell said on the way up.
fn render_open(session: &TerminalSession) -> String {
    let named = match session.name() {
        Some(name) => format!("{} ({name})", session.id()),
        None => session.id().to_string(),
    };
    let motd = session.motd();
    let motd = if motd.trim().is_empty() {
        "(no startup output)"
    } else {
        motd.trim()
    };
    format!(
        "opened terminal session {named} [type: {kind}] in {cwd}\n{motd}",
        kind = session.kind(),
        cwd = session.opened_in().display()
    )
}

/// One settled send: what the terminal printed, and the three facts about how
/// it came back.
fn render_send(outcome: &crate::terminal::SendOutcome) -> String {
    let seen = if outcome.viewport.trim().is_empty() {
        "(no new output)"
    } else {
        outcome.viewport.trim_end_matches('\n')
    };
    let mut text = format!(
        "{seen}\n[wait: {wait}]\n[session: {status}]",
        wait = outcome.wait,
        status = outcome.status
    );
    if let Some(code) = outcome.code.filter(|code| *code != 0) {
        text.push_str(&format!("\n[exit code: {code}]"));
    }
    if outcome.truncated {
        text.push_str("\n[output truncated]");
    }
    text
}

/// One page of history, in its context.
fn render_read(page: &crate::terminal::Page) -> String {
    let seen = if page.text.trim().is_empty() {
        "(no retained output)"
    } else {
        page.text.trim_end_matches('\n')
    };
    let mut text = format!(
        "{seen}\n[lines: {begin}-{end} of {total}]",
        begin = page.line_begin,
        end = page.line_end,
        total = page.total_lines
    );
    if page.truncated {
        text.push_str("\n[output truncated]");
    }
    text
}

/// One line of the list.
fn render_row(session: &TerminalSession) -> String {
    let named = match session.name() {
        Some(name) => format!("{} ({name})", session.id()),
        None => session.id().to_string(),
    };
    format!(
        "{named}\t{kind}\t{cwd}\t{status}\tpid={pid}",
        kind = session.kind(),
        cwd = session.opened_in().display(),
        status = match session.status() {
            Status::Running => "running".to_string(),
            exited => exited.to_string(),
        },
        pid = session.pid()
    )
}

/// A result cut to what a request can carry, saying so when it was cut.
///
/// The head is kept rather than the tail, because these results end with their
/// markers and a caller that lost them would be told nothing about why the
/// send came back. What the terminal printed is already bounded before it gets
/// here.
fn bounded(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    const CUT: &str = "\n[output truncated]";
    let room = max_bytes.saturating_sub(CUT.len());
    let at = (0..=room.min(text.len()))
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    format!("{}{CUT}", &text[..at])
}

// ------------------------------------------------------------- arguments

fn text(arguments: &Value, field: &str, tool: &str) -> Result<String, ToolError> {
    match arguments.get(field).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(value.to_string()),
        Some(_) => Err(ToolError::InvalidArguments(
            tool.into(),
            format!("`{field}` must not be empty"),
        )),
        None => Err(ToolError::InvalidArguments(
            tool.into(),
            format!("missing `{field}`"),
        )),
    }
}

fn optional_text(arguments: &Value, field: &str, tool: &str) -> Result<Option<String>, ToolError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(other) => Err(ToolError::InvalidArguments(
            tool.into(),
            format!("`{field}` must be a non-empty string, got {other}"),
        )),
    }
}

fn optional_flag(arguments: &Value, field: &str, tool: &str) -> Result<Option<bool>, ToolError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(ToolError::InvalidArguments(
            tool.into(),
            format!("`{field}` must be true or false, got {other}"),
        )),
    }
}

/// A wait, in milliseconds. Zero is refused rather than treated as "do not
/// wait": a send that waited no time at all would answer before the terminal
/// had echoed anything, and a model reading that empty viewport would conclude
/// the command printed nothing.
fn optional_millis(
    arguments: &Value,
    field: &str,
    tool: &str,
) -> Result<Option<std::time::Duration>, ToolError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(millis) if millis > 0 => Ok(Some(std::time::Duration::from_millis(millis))),
            _ => Err(ToolError::InvalidArguments(
                tool.into(),
                format!("`{field}` must be a positive whole number of milliseconds, got {number}"),
            )),
        },
        Some(other) => Err(ToolError::InvalidArguments(
            tool.into(),
            format!("`{field}` must be a positive whole number of milliseconds, got {other}"),
        )),
    }
}

/// A whole number of lines. Negative and fractional are refused rather than
/// clamped: a model that wrote one meant something this cannot honour.
fn optional_count(arguments: &Value, field: &str, tool: &str) -> Result<Option<usize>, ToolError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(count) => Ok(Some(count as usize)),
            None => Err(ToolError::InvalidArguments(
                tool.into(),
                format!("`{field}` must be a whole number of lines, got {number}"),
            )),
        },
        Some(other) => Err(ToolError::InvalidArguments(
            tool.into(),
            format!("`{field}` must be a whole number of lines, got {other}"),
        )),
    }
}

/// A call this tool could not make at all: a session that was never opened, a
/// backend this host does not have, a page with no answer. Distinct from a
/// command that ran and failed, which is a result.
fn refused_call(tool: &str, refused: TerminalError) -> ToolError {
    ToolError::Failed(tool.to_string(), refused.to_string())
}
