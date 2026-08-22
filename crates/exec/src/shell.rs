//! One command, through one shell: defaults and caps a deployment sets, the
//! run itself, and the text a model reads afterwards.
//!
//! The seam below this ([`crate::proc`]) is argv-only and knows nothing about
//! shells. This is where a command line becomes an argv, where an unstated
//! timeout becomes the configured one, and where an exit status becomes the
//! `[exit code: N]` marker upstream's tools emit and its presentation parses
//! back.
//!
//! **A request is resolved before it is run.** Upstream splits the two
//! (`ShellExecutor.resolve` then `run`) so a caller cannot skip the
//! deployment's caps by handing a raw request straight to an executor, and
//! this keeps that shape: [`ShellExec::resolve`] returns a [`ShellSpec`], and
//! [`ShellExec::run`] takes only a spec.
//!
//! Parity: upstream `packages/shell/shell` (the seam), `bash-local` (the
//! defaults and the run), `tool-bash/src/render.ts` and `shell/src/render.ts`
//! (the markers).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tetanus_sandbox::{Confinement, Enforcement, Mode, Policy, SandboxError};
use tetanus_turn::interrupt::Interrupt;

use crate::backend::{BackendError, Resolved, ShellBackend};
use crate::proc::{Command, Ending, Limits, Output, OutputSink, ProcessError};

/// What a deployment settles once, and every command then runs under.
#[derive(Debug, Clone)]
pub struct ShellConfig {
    /// Where a command runs when the caller names no directory.
    pub cwd: PathBuf,
    /// The budget a command with no timeout of its own gets.
    pub timeout: Duration,
    /// The ceiling a caller's own timeout is clamped to. A model that asks for
    /// a day gets the deployment's answer, not its own.
    pub max_timeout: Duration,
    /// Bytes kept per stream.
    pub max_capture: usize,
    /// How long a killed process group has between SIGTERM and SIGKILL.
    pub grace: Duration,
    /// Where the whole of a stream goes when `max_capture` drops part of it,
    /// and which session's directory it goes in.
    ///
    /// `None` is the default and means the bound is the end of the story: a
    /// truncated result says so and the dropped bytes are gone. A deployment
    /// that sets it trades disk for the ability to answer "what did the build
    /// actually print", which is the question a tail cannot answer.
    pub spill: Option<SpillTo>,
    /// The kernel boundary every command runs behind.
    ///
    /// The default is `danger-full-access`, which is the behaviour this seam
    /// had before there was a sandbox, and it is spelled out rather than
    /// implied: a deployment reading its own configuration can see that it
    /// chose no confinement. A deployment that wants one writes the mode.
    pub sandbox: Policy,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            // Upstream's `timeoutMs` and `maxTimeoutMs` defaults.
            timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
            max_capture: 64 * 1024,
            grace: Duration::from_secs(3),
            spill: None,
            sandbox: Policy::danger_full_access(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
        }
    }
}

/// Where spilled output is kept, and whose it is.
///
/// The session scopes the directory, so one run's artifacts are not scattered
/// through another's. There is no model-issued call id here because a tool
/// does not get one: `Tool::execute` is handed arguments, not the call that
/// carried them, so the executor numbers its own runs and the number is what
/// makes two commands in one session distinguishable.
#[derive(Debug, Clone)]
pub struct SpillTo {
    pub store: Arc<tetanus_core::spill::SpillStore>,
    pub session: String,
}

/// What a caller asks for. Everything optional is filled by
/// [`ShellExec::resolve`] from the deployment's configuration.
#[derive(Debug, Clone)]
pub struct ShellRequest {
    pub command: String,
    pub workdir: Option<PathBuf>,
    pub timeout: Option<Duration>,
    /// Extra environment for this command, over the backend's own overrides.
    pub env: BTreeMap<String, String>,
    /// Bytes written to the command's input, which is then closed.
    pub stdin: Option<String>,
}

impl ShellRequest {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            workdir: None,
            timeout: None,
            env: BTreeMap::new(),
            stdin: None,
        }
    }

    pub fn workdir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workdir = Some(dir.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn stdin(mut self, data: impl Into<String>) -> Self {
        self.stdin = Some(data.into());
        self
    }
}

/// A request with every blank filled in and every cap applied.
#[derive(Debug, Clone)]
pub struct ShellSpec {
    pub command: String,
    pub workdir: PathBuf,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<String>,
    pub limits: Limits,
}

/// What one finished command produced, and under which budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRun {
    pub output: Output,
    /// The mode this command ran under, and how completely it was enforced.
    /// Carried on the result rather than looked up again, because a reader
    /// deciding whether a failure was a denial needs the policy that was
    /// actually applied.
    pub sandbox: Option<(Mode, Enforcement)>,
    /// The budget this run actually had, after defaulting and capping - the
    /// number the `[timed out after ...]` marker quotes, so a model reads the
    /// limit that applied rather than the one it asked for.
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// The policy could not be enforced on this host. A refusal, never a
    /// downgrade: the caller asked for a boundary and this host cannot give
    /// one, so nothing runs.
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// A command that is only whitespace. Refused rather than run, because a
    /// shell would accept it, exit 0, and tell the model nothing happened in a
    /// way indistinguishable from a command that did nothing.
    #[error("a command must not be empty")]
    EmptyCommand,
}

/// One shell backend, resolved, with the deployment's defaults around it.
pub struct ShellExec {
    backend: Arc<dyn ShellBackend>,
    resolved: Resolved,
    config: ShellConfig,
    /// Prepared once, at composition, and shared by every command this
    /// executor runs. Preparing it here is what makes a host that cannot
    /// honour the policy fail while someone is still watching, rather than on
    /// the first command a model writes.
    confinement: Arc<Confinement>,
    /// How many commands this executor has run, so two spilled artifacts in
    /// one session have different names and a reader can see which came
    /// first.
    runs: std::sync::atomic::AtomicU64,
}

impl ShellExec {
    /// Resolve the backend now, so a deployment without the shell it named
    /// fails at composition instead of inside a turn.
    pub fn new(backend: Arc<dyn ShellBackend>, config: ShellConfig) -> Result<Self, ShellError> {
        let resolved = backend.resolve()?;
        let confinement = Arc::new(tetanus_sandbox::prepare(&config.sandbox)?);
        Ok(Self {
            backend,
            resolved,
            config,
            confinement,
            runs: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The boundary every command from this executor runs behind.
    pub fn confinement(&self) -> &Arc<Confinement> {
        &self.confinement
    }

    pub fn backend(&self) -> &Arc<dyn ShellBackend> {
        &self.backend
    }

    pub fn config(&self) -> &ShellConfig {
        &self.config
    }

    /// Fill in what the caller left out, and clamp what it over-asked for.
    pub fn resolve(&self, request: ShellRequest) -> Result<ShellSpec, ShellError> {
        if request.command.trim().is_empty() {
            return Err(ShellError::EmptyCommand);
        }
        let timeout = request
            .timeout
            .unwrap_or(self.config.timeout)
            .min(self.config.max_timeout);
        let workdir = match request.workdir {
            // A relative directory is resolved against the deployment's own,
            // so `workdir: "src"` means what a reader thinks it means.
            Some(dir) if dir.is_relative() => self.config.cwd.join(dir),
            Some(dir) => dir,
            None => self.config.cwd.clone(),
        };
        // The backend's overrides go on first, so a caller that names one of
        // them wins: the caller knows something the default list does not.
        let mut env = self.backend.environment();
        env.extend(request.env);
        Ok(ShellSpec {
            command: request.command,
            workdir,
            timeout,
            env,
            stdin: request.stdin,
            limits: Limits {
                max_capture: self.config.max_capture,
                timeout,
                grace: self.config.grace,
            },
        })
    }

    /// Run one resolved spec in the foreground.
    ///
    /// A non-zero exit, a timeout and an interrupt are all results, not
    /// errors: only a shell that could not be started at all is an error,
    /// because only that leaves the caller with nothing to show.
    pub async fn run(&self, spec: &ShellSpec) -> Result<ShellRun, ShellError> {
        self.run_with(spec, None, None).await
    }

    /// [`ShellExec::run`], with each piece of output handed to `sink` as it
    /// arrives and the whole thing ended if the turn is interrupted.
    pub async fn run_with(
        &self,
        spec: &ShellSpec,
        sink: Option<Arc<dyn OutputSink>>,
        interrupt: Option<&Interrupt>,
    ) -> Result<ShellRun, ShellError> {
        let mut command = Command::new(self.resolved.program().display().to_string())
            .args(self.backend.one_shot(&spec.command))
            .cwd(&spec.workdir)
            .envs(spec.env.clone())
            .limits(spec.limits);
        if let Some(stdin) = &spec.stdin {
            command = command.stdin(stdin.clone());
        }
        if let Some(sink) = sink {
            command = command.streaming(sink);
        }
        if self.confinement.confines() {
            command = command.confined(Arc::clone(&self.confinement));
        }
        if let Some(spill) = &self.config.spill {
            let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            command = command.spilling(
                Arc::clone(&spill.store),
                tetanus_core::spill::SpillSource {
                    session_id: spill.session.clone(),
                    tool: "shell".to_string(),
                    call_id: format!("run-{run}"),
                },
            );
        }
        let output = match interrupt {
            Some(interrupt) => command.run_watching(interrupt).await?,
            None => command.run().await?,
        };
        Ok(ShellRun {
            output,
            sandbox: self
                .confinement
                .confines()
                .then(|| (self.config.sandbox.mode(), self.confinement.enforcement)),
            timeout: spec.timeout,
        })
    }
}

/// The exit status recovered from a rendered result, and the body it was split
/// off from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedExit {
    pub body: String,
    pub code: Option<i32>,
    pub signal: Option<String>,
}

/// Shape one finished run into the text a model reads: stdout, then a marked
/// stderr section, then the status markers.
///
/// A non-zero exit is reported rather than raised. The model decides what to
/// do about a command that failed, and it can only decide with the output and
/// the code in front of it; a caller that turned this into an error would hand
/// it neither.
///
/// The exit marker is last because [`parse_exit`] anchors on the end of the
/// text: a presentation shows the exit as its own pill and needs to find it
/// again in a replayed result, where the rendered string is all that survives.
pub fn render(run: &ShellRun) -> String {
    let mut body = body_of(run);
    let markers = markers_of(run);
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body + &markers.join("\n")
}

/// What the command printed: stdout, then a marked stderr section, and a word
/// for a command that printed nothing at all.
fn body_of(run: &ShellRun) -> String {
    let mut body = stream_text(&run.output.stdout);
    let err = stream_text(&run.output.stderr);
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err);
    }
    if body.is_empty() {
        body.push_str("(no output)");
    }
    body
}

/// Every marker this run earned, in the order a reader needs them: what the
/// policy did, what the harness did, what the budget did, and last - because
/// `parse_exit` anchors on the end of the text - what the command exited with.
fn markers_of(run: &ShellRun) -> Vec<String> {
    let mut markers = sandbox_markers(run);
    markers.extend(sweep_marker(run));
    markers.extend(ending_marker(run));
    markers.extend(status_marker(run));
    markers
}

/// What the sandbox did, when this run had one.
///
/// Upstream's `sandboxDenialMarker`: a policy denial is not a bug in the
/// command, and a model that reads it as one will rewrite a correct command
/// until it gives up. Naming the mode tells it what would have to change
/// instead.
fn sandbox_markers(run: &ShellRun) -> Vec<String> {
    let Some((mode, enforcement)) = run.sandbox else {
        return Vec::new();
    };
    let mut markers = Vec::new();
    if denied(run) {
        markers.push(format!(
            "[sandbox: a file or network operation was denied under {mode} mode - this is policy, \
             not a bug in the command]"
        ));
    }
    if enforcement == Enforcement::Partial {
        markers.push("[sandbox: this host enforces only part of that policy]".to_string());
    }
    markers
}

/// What the command left running, when the group had to be swept.
fn sweep_marker(run: &ShellRun) -> Option<String> {
    run.output.swept.then(|| {
        "[the command left processes running; they were killed with its process group]".to_string()
    })
}

/// What ended the command, when it was not the command's own choice.
///
/// A command can trap SIGTERM and exit 0 after its budget ran out, so this is
/// reported beside the exit status rather than instead of it.
fn ending_marker(run: &ShellRun) -> Option<String> {
    match run.output.ending {
        Ending::TimedOut => Some(format!("[timed out after {}ms]", run.timeout.as_millis())),
        Ending::Interrupted => Some("[interrupted]".to_string()),
        Ending::Exited => None,
    }
}

/// The exit status, as the marker `parse_exit` reads back. A clean exit says
/// nothing: silence is what success looks like.
fn status_marker(run: &ShellRun) -> Option<String> {
    match (&run.output.signal, run.output.code) {
        (Some(signal), _) => Some(format!("[killed by signal: {signal}]")),
        (None, Some(0)) | (None, None) => None,
        (None, Some(code)) => Some(format!("[exit code: {code}]")),
    }
}

/// Split a rendered result back into its body and its exit status: the inverse
/// of the markers [`render`] appends.
///
/// A replayed session keeps the rendered text and nothing else, so a
/// presentation that wants to show the exit as a pill has to recover it from
/// here. The match needs a leading newline and the end of the string, so
/// output that merely ends with marker-shaped text is left alone unless its
/// last line is indistinguishable from a real marker.
pub fn parse_exit(text: &str) -> ParsedExit {
    if let Some((body, signal)) = suffix_marker(text, "[killed by signal: ", "]") {
        return ParsedExit {
            body,
            code: None,
            signal: Some(signal),
        };
    }
    if let Some((body, code)) = suffix_marker(text, "[exit code: ", "]") {
        if let Ok(code) = code.parse() {
            return ParsedExit {
                body,
                code: Some(code),
                signal: None,
            };
        }
    }
    ParsedExit {
        body: text.to_string(),
        code: Some(0),
        signal: None,
    }
}

/// The value of a `\n[prefix...suffix]` marker at the very end of `text`, and
/// the text before it.
fn suffix_marker(text: &str, prefix: &str, suffix: &str) -> Option<(String, String)> {
    let rest = text.strip_suffix(suffix)?;
    let at = rest.rfind(prefix)?;
    if !rest[..at].ends_with('\n') {
        return None;
    }
    let value = &rest[at + prefix.len()..];
    if value.contains('\n') || value.contains(']') {
        return None;
    }
    Some((text[..at - 1].to_string(), value.to_string()))
}

/// One stream's text, with the truncation notice a reader needs to know the
/// log they are reading is not the whole log - and, where the deployment kept
/// it, where the rest of it is.
fn stream_text(captured: &crate::proc::Captured) -> String {
    if !captured.truncated {
        return captured.text.clone();
    }
    match &captured.spilled {
        None => format!(
            "{}\n[output truncated; the beginning was dropped to fit the capture bound]",
            captured.text
        ),
        Some(locator) => format!(
            "{}\n[output truncated; the beginning was dropped to fit the capture bound; the whole \
             stream is at {locator}]",
            captured.text
        ),
    }
}

/// Whether this run looks like the sandbox refused something.
///
/// A guess, and deliberately a conservative one: the kernel denies with
/// `EACCES` and the program reports it in its own words, so there is nothing
/// structured to read. The command has to have failed *and* said something
/// this backend's denial dialect recognises. Upstream carries the same
/// per-backend dialect rather than a union across backends, because a union
/// claims denials a given backend never produces.
fn denied(run: &ShellRun) -> bool {
    if run.output.ok() {
        return false;
    }
    let text = format!("{}{}", run.output.stdout.text, run.output.stderr.text);
    let hints = ["Permission denied", "EACCES", "Operation not permitted"];
    hints.iter().any(|hint| text.contains(hint))
}
