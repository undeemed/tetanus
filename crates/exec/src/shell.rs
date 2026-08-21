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
        }
    }
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
    /// The budget this run actually had, after defaulting and capping - the
    /// number the `[timed out after ...]` marker quotes, so a model reads the
    /// limit that applied rather than the one it asked for.
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error(transparent)]
    Backend(#[from] BackendError),
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
}

impl ShellExec {
    /// Resolve the backend now, so a deployment without the shell it named
    /// fails at composition instead of inside a turn.
    pub fn new(backend: Arc<dyn ShellBackend>, config: ShellConfig) -> Result<Self, BackendError> {
        let resolved = backend.resolve()?;
        Ok(Self {
            backend,
            resolved,
            config,
        })
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
        let output = match interrupt {
            Some(interrupt) => command.run_watching(interrupt).await?,
            None => command.run().await?,
        };
        Ok(ShellRun {
            output,
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

    let mut markers: Vec<String> = Vec::new();
    if run.output.swept {
        markers.push(
            "[the command left processes running; they were killed with its process group]"
                .to_string(),
        );
    }
    match run.output.ending {
        // A command can trap SIGTERM and exit 0 after its budget ran out; the
        // interruption is still worth reporting, so this is not an `else`.
        Ending::TimedOut => {
            markers.push(format!("[timed out after {}ms]", run.timeout.as_millis()))
        }
        Ending::Interrupted => markers.push("[interrupted]".to_string()),
        Ending::Exited => {}
    }
    match (&run.output.signal, run.output.code) {
        (Some(signal), _) => markers.push(format!("[killed by signal: {signal}]")),
        (None, Some(0)) | (None, None) => {}
        (None, Some(code)) => markers.push(format!("[exit code: {code}]")),
    }

    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body + &markers.join("\n")
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
/// log they are reading is not the whole log.
fn stream_text(captured: &crate::proc::Captured) -> String {
    if !captured.truncated {
        return captured.text.clone();
    }
    format!(
        "{}\n[output truncated; the beginning was dropped to fit the capture bound]",
        captured.text
    )
}
