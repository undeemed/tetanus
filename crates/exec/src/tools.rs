//! The model-facing tools over the shell seam: one for a single command, and
//! four for the persistent sessions.
//!
//! `shell` is upstream's `bash` tool, named for what it is rather than for one
//! backend, because the backend is configuration here (`docs/parity.md`
//! records the rename). `shell_open`, `shell_run`, `shell_close` and
//! `shell_list` are upstream's `terminal_*` family, minus the calls that only
//! mean something with a PTY behind them.
//!
//! **What the model is told is the rendered result, markers and all.** A
//! non-zero exit comes back as `[exit code: N]`, a timeout as `[timed out
//! after Nms]`, a signal as `[killed by signal: X]`. The markers are a wire
//! format in all but name: upstream's presentation parses them back to show an
//! exit pill, and [`crate::shell::parse_exit`] is that parser.
//!
//! **`ok` says whether the command succeeded, not whether the tool worked.**
//! Upstream sets `isError` only for infrastructure failures, so a command that
//! exits 3 is a successful tool call carrying a failure marker. A tetanus
//! outcome carries the text either way, so the flag can afford to mean the
//! plainer thing: `ok` is false when the command failed, was killed, or never
//! ran, and the content says which. A caller that wants upstream's narrower
//! reading has the markers.
//!
//! Parity: upstream `packages/shell/tool-bash`, `packages/shell/tool-pwsh`,
//! `packages/shell/tool-bash-persistent` and `packages/terminal/tool-terminal`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tetanus_sandbox::Mode;
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{
    Permission, Tool, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema, REDACTED,
};

use crate::backend::ShellBackend;
use crate::proc::OutputSink;
use crate::session::{SessionConfig, SessionError, ShellSessions};
use crate::shell::{render, ShellConfig, ShellError, ShellExec, ShellRequest};

/// The name of the one-shot tool.
pub const SHELL: &str = "shell";
/// The names of the persistent-session tools.
pub const SHELL_OPEN: &str = "shell_open";
pub const SHELL_RUN: &str = "shell_run";
pub const SHELL_CLOSE: &str = "shell_close";
pub const SHELL_LIST: &str = "shell_list";

/// Everything the five tools share: one resolved backend, the one-shot
/// executor, the open sessions, and the turn's stop switch.
pub struct ShellTools {
    exec: ShellExec,
    backend: Arc<dyn ShellBackend>,
    sessions: Arc<ShellSessions>,
    session_config: SessionConfig,
    /// The turn's interrupt. A command a stopped turn is no longer waiting for
    /// is killed with its process group rather than left running.
    interrupt: Arc<Interrupt>,
    /// Where incremental output goes while a command runs, when a surface has
    /// asked to watch. Behind a lock because the tools are shared and the
    /// watcher is attached after they are built.
    watching: Mutex<Option<Arc<dyn OutputSink>>>,
}

impl ShellTools {
    /// Resolve the backend and build the five tools around it.
    ///
    /// The backend is resolved here, so a deployment whose shell is missing
    /// fails while it is being composed rather than the first time a model
    /// asks for a command.
    pub fn new(
        backend: Arc<dyn ShellBackend>,
        config: ShellConfig,
        session_config: SessionConfig,
        interrupt: Arc<Interrupt>,
    ) -> Result<Arc<Self>, ShellError> {
        let exec = ShellExec::new(Arc::clone(&backend), config)?;
        Ok(Arc::new(Self {
            exec,
            backend,
            sessions: Arc::new(ShellSessions::new()),
            session_config,
            interrupt,
            watching: Mutex::new(None),
        }))
    }

    /// Register all five tools on `registry`.
    pub fn register(self: &Arc<Self>, registry: &mut ToolRegistry) {
        registry.register(Arc::new(ShellTool(Arc::clone(self))));
        registry.register(Arc::new(ShellOpenTool(Arc::clone(self))));
        registry.register(Arc::new(ShellRunTool(Arc::clone(self))));
        registry.register(Arc::new(ShellCloseTool(Arc::clone(self))));
        registry.register(Arc::new(ShellListTool(Arc::clone(self))));
    }

    /// A registry holding exactly these five tools, for a caller composing one
    /// from nothing.
    pub fn registry(self: &Arc<Self>) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        self.register(&mut registry);
        registry
    }

    /// Register the tools, or - on a host whose shell is missing, or whose
    /// kernel cannot enforce the policy - register one `shell` tool that
    /// answers every call with the refusal.
    ///
    /// A composition that dropped the tools silently would leave the model
    /// with no shell and no way to find out why: it would look like a build
    /// that never had one. A binary that refused to start would be worse,
    /// because every other tool still works. So the refusal becomes the tool's
    /// answer, once per call, in the words [`BackendError`] uses.
    pub fn register_or_explain(
        registry: &mut ToolRegistry,
        backend: Arc<dyn ShellBackend>,
        config: ShellConfig,
        session_config: SessionConfig,
        interrupt: Arc<Interrupt>,
    ) {
        match Self::new(backend, config, session_config, interrupt) {
            Ok(tools) => tools.register(registry),
            Err(refused) => {
                tracing::warn!(%refused, "the shell tools are unavailable on this host");
                registry.register(Arc::new(MissingShell(refused.to_string())));
            }
        }
    }

    /// Send incremental output here while a command runs.
    pub fn watch(&self, sink: Arc<dyn OutputSink>) {
        *self.watching.lock().expect("no panic holds this lock") = Some(sink);
    }

    /// The sessions this build has open, for a composition that has to close
    /// them on the way down.
    pub fn sessions(&self) -> &Arc<ShellSessions> {
        &self.sessions
    }

    /// The mode every ordinary call from these tools runs under.
    pub fn mode(&self) -> Mode {
        self.exec.config().sandbox.mode()
    }

    /// Whether this composition can escalate at all: there is no wider mode to
    /// ask for when nothing is being confined.
    fn escalates(&self) -> bool {
        !self.mode().wider_modes().is_empty() && self.mode().confines()
    }

    /// The two escalation properties, for a schema that should advertise them.
    ///
    /// Unadvertised where the executor confines nothing, because an argument a
    /// deployment cannot honour is one a model will spend a call discovering.
    fn escalation_properties(&self) -> serde_json::Map<String, Value> {
        let mut properties = serde_json::Map::new();
        if !self.escalates() {
            return properties;
        }
        let targets: Vec<&'static str> = self
            .mode()
            .wider_modes()
            .into_iter()
            .map(Mode::as_str)
            .collect();
        properties.insert(
            "sandbox_permissions".to_string(),
            serde_json::json!({
                "type": "string",
                "enum": targets,
                "description": "The wider sandbox mode this command needs. Only for retrying a command the sandbox just denied; needs a justification, and the person running this harness has to agree.",
            }),
        );
        properties.insert(
            "justification".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Required with sandbox_permissions: one sentence saying why this exact command needs the wider access.",
            }),
        );
        properties
    }

    /// One command's confinement, which is the standing one unless this call
    /// was approved for something wider.
    async fn run_under(
        &self,
        escalated: Option<Mode>,
        spec: &crate::shell::ShellSpec,
    ) -> Result<crate::shell::ShellRun, ShellError> {
        let Some(mode) = escalated else {
            return self
                .exec
                .run_with(spec, self.sink(), Some(&self.interrupt))
                .await;
        };
        // A wider policy for exactly this call. Built here rather than kept,
        // because an escalation is one command's grant and a cached wider
        // executor is a grant that outlives the question that bought it.
        let widened = self
            .exec
            .config()
            .sandbox
            .widened_to(mode)
            .expect("the request was checked as strictly wider before it was asked");
        let once = ShellExec::new(
            Arc::clone(&self.backend),
            ShellConfig {
                sandbox: widened,
                ..self.exec.config().clone()
            },
        )?;
        once.run_with(spec, self.sink(), Some(&self.interrupt))
            .await
    }

    fn sink(&self) -> Option<Arc<dyn OutputSink>> {
        self.watching
            .lock()
            .expect("no panic holds this lock")
            .clone()
    }
}

/// The two arguments an escalation is made of, read off one call.
///
/// Upstream pairs them for a reason worth keeping: a wider mode with no
/// justification is a request nobody can answer, and a justification with no
/// mode is a sentence about nothing. Either alone is a mistake in what the
/// model wrote, and it is reported rather than half-honoured.
struct Escalation {
    to: Mode,
    justification: String,
}

/// Read an escalation request off a call's arguments.
///
/// `Ok(None)` is the ordinary call, which is almost all of them. An `Err` is a
/// request that cannot be granted as written - unpaired, empty, an unknown
/// mode, or one no wider than the policy already in force - and it is refused
/// before anything is asked of a human, because a question whose answer cannot
/// be applied is worse than no question.
fn escalation(arguments: &Value, from: Mode) -> Result<Option<Escalation>, ToolError> {
    let mode = optional_text(arguments, "sandbox_permissions", SHELL)?;
    let justification = optional_text(arguments, "justification", SHELL)?;
    let (Some(mode), Some(justification)) = (mode.clone(), justification.clone()) else {
        return match (mode, justification) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(ToolError::InvalidArguments(
                SHELL.into(),
                "`sandbox_permissions` needs a `justification`: one sentence for the person who \
                 has to decide"
                    .into(),
            )),
            (None, Some(_)) => Err(ToolError::InvalidArguments(
                SHELL.into(),
                "`justification` means nothing without `sandbox_permissions`, which names the \
                 wider mode this command needs"
                    .into(),
            )),
            (Some(_), Some(_)) => unreachable!("both present is the branch above"),
        };
    };
    // The vocabulary is the sandbox crate's, and reading it is too: a
    // hand-written match here is a fourth mode away from being wrong.
    let Some(to) = Mode::parse(&mode) else {
        return Err(ToolError::InvalidArguments(
            SHELL.into(),
            format!("`sandbox_permissions` must name a sandbox mode, not {mode:?}"),
        ));
    };
    if !to.is_wider_than(from) {
        return Err(ToolError::InvalidArguments(
            SHELL.into(),
            format!(
                "this command already runs under {from}, so escalating to {to} would not widen \
                 anything; ask for a mode that permits more, or run the command as it is"
            ),
        ));
    }
    Ok(Some(Escalation { to, justification }))
}

/// Run one command in a fresh shell.
struct ShellTool(Arc<ShellTools>);

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line to run. The shell parses it, so pipes, redirections and quoting all work.",
                },
                "description": {
                    "type": "string",
                    "description": "What this command does, in five to ten words, for the person watching.",
                },
                "workdir": {
                    "type": "string",
                    "description": "Where to run it. Relative paths resolve against the workspace root. Defaults to the workspace root.",
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "How long the command may run before it and everything it started are killed. The deployment caps this.",
                },
                "secret": {
                    "type": "boolean",
                    "description": "Set this when the command line itself carries a credential - a password in a flag, a token in a header. The command still runs as written; the session journal keeps `<redacted>` in place of it. Prefer a command that reads the secret from a file or the environment, because a redacted command line is one nobody can audit either.",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        });
        if let Some(properties) = parameters["properties"].as_object_mut() {
            properties.extend(self.0.escalation_properties());
        }
        ToolSchema {
            name: SHELL.into(),
            description: format!(
                "Run one command through {shell} and return what it printed. Each call is a \
                 fresh shell: nothing survives between calls, so pass `workdir` instead of using \
                 `cd`, and use `{SHELL_OPEN}` when the work needs state that lasts. A non-zero \
                 exit is reported as `[exit code: N]` and is not a tool failure - read it. Long \
                 output is cut to its tail.",
                shell = self.0.backend.name()
            ),
            parameters,
        }
    }

    /// A command can write anything, anywhere. Two of them overlapping is two
    /// unrelated programs sharing a working tree, so this is a barrier: the
    /// step's earlier calls settle first, and nothing after it starts until it
    /// is done.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    /// A command line can carry a credential - `mysql -pSECRET`, a token in a
    /// header - and the journal is forever. When the model says so, the whole
    /// command is withheld rather than the flag inside it: this seam does not
    /// parse command lines, and a redactor that tried to find the secret part
    /// would be a parser for every shell syntax there is, wrong in the
    /// direction that publishes the password.
    ///
    /// That is a real cost, and the schema says so: a withheld command is one
    /// an auditor cannot read either. The better shape is a command that reads
    /// its secret from a file or the environment, and the description says
    /// that first.
    fn recorded(&self, arguments: &Value) -> Value {
        withheld_when_secret(arguments, "command")
    }

    /// An ordinary command runs; a command asking for a wider sandbox is
    /// decided first.
    ///
    /// The gate is the engine's existing one, so an escalation is audited as
    /// one `approval/asked`/`approval/decided` pair like every other decision
    /// and needs no vocabulary of its own. The reason carries what the person
    /// answering has to weigh: the command, the mode it would gain, and the
    /// model's own sentence for why.
    ///
    /// A malformed request asks nothing. `execute` refuses it in words, which
    /// is the better answer than putting an unanswerable question to a human.
    fn permission(&self, arguments: &Value) -> Permission {
        let Ok(Some(escalation)) = escalation(arguments, self.0.mode()) else {
            return Permission::Allow;
        };
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("(no command)");
        Permission::ask(format!(
            "run `{command}` under {to} instead of {from}, because: {why}",
            to = escalation.to,
            from = self.0.mode(),
            why = escalation.justification
        ))
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let command = text(arguments, "command", SHELL)?;
        // Reaching `execute` at all means the gate let this through: either
        // there was no escalation, or the person answering granted it. What is
        // left here is applying the mode that was approved.
        let escalated = escalation(arguments, self.0.mode())?.map(|escalation| escalation.to);
        let mut request = ShellRequest::new(command);
        if let Some(workdir) = optional_text(arguments, "workdir", SHELL)? {
            request = request.workdir(workdir);
        }
        if let Some(timeout) = optional_millis(arguments, "timeout_ms", SHELL)? {
            request = request.timeout(timeout);
        }

        let spec = self
            .0
            .exec
            .resolve(request)
            .map_err(|refused| refused_call(SHELL, refused))?;
        let run = self
            .0
            .run_under(escalated, &spec)
            .await
            .map_err(|refused| refused_call(SHELL, refused))?;

        let text = render(&run);
        Ok(if run.output.ok() {
            ToolOutcome::ok(text)
        } else {
            // The command ran and did not succeed. The text says exactly how -
            // an exit code, a signal, a timeout - so the flag can mean the
            // plain thing rather than upstream's narrower `isError`.
            ToolOutcome::failed(text)
        })
    }
}

/// Open a persistent shell.
struct ShellOpenTool(Arc<ShellTools>);

#[async_trait::async_trait]
impl Tool for ShellOpenTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: SHELL_OPEN.into(),
            description: format!(
                "Open a persistent {shell} session and return its id. The working directory and \
                 the variables a command exports survive into the next `{SHELL_RUN}` on the same \
                 id. Close it with `{SHELL_CLOSE}` when the work is done. If the shell dies you \
                 are told, and you open a new one - nothing is restarted behind you.",
                shell = self.0.backend.name()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cwd": {
                        "type": "string",
                        "description": "Where the shell starts. Relative paths resolve against the workspace root. Defaults to the workspace root.",
                    },
                },
                "additionalProperties": false,
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let mut config = self.0.session_config.clone();
        if let Some(cwd) = optional_text(arguments, "cwd", SHELL_OPEN)? {
            let named = std::path::PathBuf::from(cwd);
            config.cwd = if named.is_relative() {
                config.cwd.join(named)
            } else {
                named
            };
        }
        let session = self
            .0
            .sessions
            .open(Arc::clone(&self.0.backend), config)
            .await
            .map_err(|refused| refused_call(SHELL_OPEN, refused))?;
        Ok(ToolOutcome::ok(format!(
            "opened {backend} session {id} in {cwd}",
            backend = session.backend(),
            id = session.id(),
            cwd = session.opened_in().display()
        )))
    }
}

/// Run one command in a persistent shell.
struct ShellRunTool(Arc<ShellTools>);

#[async_trait::async_trait]
impl Tool for ShellRunTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: SHELL_RUN.into(),
            description: format!(
                "Run one command in a persistent session opened by `{SHELL_OPEN}`. The session \
                 keeps its working directory and its exported variables from the previous call. \
                 A non-zero exit is reported as `[exit code: N]`. If the shell died, the answer \
                 says so and the session stays dead - open a new one."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id `shell_open` returned.",
                    },
                    "command": {
                        "type": "string",
                        "description": "The command line to run in that session.",
                    },
                    "secret": {
                        "type": "boolean",
                        "description": "Set this when the command line carries a credential. It still runs as written; the journal keeps `<redacted>` in place of it.",
                    },
                },
                "required": ["session_id", "command"],
                "additionalProperties": false,
            }),
        }
    }

    /// One session runs one command at a time whatever the scheduler does, and
    /// a command in a shell can write anything: a barrier, like `shell`.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    /// As `shell`: a command line that carries a credential is withheld whole
    /// when the model says it carries one.
    fn recorded(&self, arguments: &Value) -> Value {
        withheld_when_secret(arguments, "command")
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let id = text(arguments, "session_id", SHELL_RUN)?;
        let command = text(arguments, "command", SHELL_RUN)?;
        let session = self
            .0
            .sessions
            .get(&id)
            .map_err(|refused| refused_call(SHELL_RUN, refused))?;

        match session
            .run_watching(&command, self.0.sink(), Some(&self.0.interrupt))
            .await
        {
            Ok(run) => {
                let mut text = run.text;
                if run.truncated {
                    // The locator when the deployment kept the rest, and the
                    // plain notice when it did not: a marker naming nowhere is
                    // worse than no marker.
                    text = match &run.spilled {
                        Some(locator) => format!(
                            "[output truncated; the beginning was dropped to fit the session's \
                             scrollback; the whole of this command's output is at {locator}]\n{text}"
                        ),
                        None => format!(
                            "[output truncated; the beginning was dropped to fit the session's \
                             scrollback]\n{text}"
                        ),
                    };
                }
                if run.code == 0 {
                    Ok(ToolOutcome::ok(text))
                } else {
                    Ok(ToolOutcome::failed(marked(text, run.code)))
                }
            }
            // Everything that went wrong with the session is told to the model
            // as a failed result rather than raised: the model's next move -
            // open a new session, or give up on this one - depends on reading
            // it, and it needs whatever the command printed first.
            Err(refused) => {
                let mut text = refused.to_string();
                if let Some(partial) = refused.partial() {
                    if !partial.is_empty() {
                        text = format!("{partial}\n[{text}]");
                    }
                }
                Ok(ToolOutcome::failed(text))
            }
        }
    }
}

/// Close a persistent shell.
struct ShellCloseTool(Arc<ShellTools>);

#[async_trait::async_trait]
impl Tool for ShellCloseTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: SHELL_CLOSE.into(),
            description: "Close a persistent shell session and wait until it and everything it \
                          started are gone."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The id `shell_open` returned.",
                    },
                },
                "required": ["session_id"],
                "additionalProperties": false,
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let id = text(arguments, "session_id", SHELL_CLOSE)?;
        match self.0.sessions.close(&id).await {
            Ok(()) => Ok(ToolOutcome::ok(format!("closed session {id}"))),
            Err(refused) => Ok(ToolOutcome::failed(refused.to_string())),
        }
    }
}

/// List the persistent shells that are open.
struct ShellListTool(Arc<ShellTools>);

#[async_trait::async_trait]
impl Tool for ShellListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: SHELL_LIST.into(),
            description: "List the persistent shell sessions that are open, with the backend \
                          each one runs and whether it is still usable."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        }
    }

    /// Listing reads a registry and touches nothing outside the process, so it
    /// is safe beside its siblings.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, _arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let sessions = self.0.sessions.list();
        if sessions.is_empty() {
            return Ok(ToolOutcome::ok("no shell sessions are open"));
        }
        let rows: Vec<String> = sessions
            .iter()
            .map(|session| {
                let state = match session.gone() {
                    Some(reason) => format!("gone: {reason}"),
                    None => "running".to_string(),
                };
                format!(
                    "{id}\t{backend}\t{cwd}\t{state}",
                    id = session.id(),
                    backend = session.backend(),
                    cwd = session.opened_in().display()
                )
            })
            .collect();
        Ok(ToolOutcome::ok(rows.join("\n")))
    }
}

/// The `shell` tool on a host that has no shell to run.
///
/// It advertises itself so the model can see that a shell exists as a concept
/// here, and answers every call with the deployment fault that stopped it -
/// which is something a model can report to a human, unlike a tool that was
/// never registered.
struct MissingShell(String);

#[async_trait::async_trait]
impl Tool for MissingShell {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: SHELL.into(),
            description: format!(
                "Unavailable in this deployment: {}. Calling it reports the same thing.",
                self.0
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line that would have run." },
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        }
    }

    async fn execute(&self, _arguments: &Value) -> Result<ToolOutcome, ToolError> {
        Err(ToolError::Failed(SHELL.into(), self.0.clone()))
    }
}

/// One argument replaced by the sentinel, when the call said it holds a
/// secret. Shared by the two tools that take a command line, so both answer
/// the flag the same way.
fn withheld_when_secret(arguments: &Value, field: &str) -> Value {
    if !arguments
        .get("secret")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return arguments.clone();
    }
    let mut recorded = arguments.clone();
    if let Some(object) = recorded.as_object_mut() {
        object.insert(field.to_string(), Value::String(REDACTED.to_string()));
    }
    recorded
}

/// Put the exit marker back on a session result, which has no renderer of its
/// own because a session has already merged its two streams.
fn marked(text: String, code: i32) -> String {
    if text.is_empty() {
        return format!("[exit code: {code}]");
    }
    format!("{}\n[exit code: {code}]", text.trim_end_matches('\n'))
}

/// A required string argument.
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

/// An optional string argument. Present but not a string is a mistake worth
/// reporting: a model that wrote `workdir: 3` meant something, and running in
/// the default directory instead would hide it.
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

/// An optional duration in milliseconds. Zero and negative are refused rather
/// than clamped: they are not budgets, and a command that ran under one was
/// never going to finish.
fn optional_millis(
    arguments: &Value,
    field: &str,
    tool: &str,
) -> Result<Option<Duration>, ToolError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(millis) if millis > 0 => Ok(Some(Duration::from_millis(millis))),
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

/// A call this tool could not make at all: a shell that is not there, a
/// command that is empty, a session that was never opened. Distinct from a
/// command that ran and failed, which is a result.
fn refused_call(tool: &str, refused: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(tool.to_string(), refused.to_string())
}

/// `ShellError` and `SessionError` both reach the model through
/// [`refused_call`]; naming them here keeps the conversion in one place if
/// either grows a variant that should be reported differently.
const _: fn(&ShellError, &SessionError) = |_, _| {};
