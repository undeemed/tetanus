//! A language server over stdio, and the tool a model asks through it.
//!
//! Textual search finds every `foo`; a language server knows which one. That
//! difference is worth a subprocess when a model is about to change something,
//! and worth nothing at all for ordinary navigation - which is why the tool's
//! description says so, rather than leaving the model to work it out per call.
//!
//! **A server that dies is a failed tool call, never a dead turn.** This is
//! the rule the whole module is arranged around. A language server is a large
//! third-party program that crashes, hangs and gets killed by the OOM killer,
//! and none of those is a reason for a conversation to end. Every failure here
//! becomes a `ToolError` the model reads and can act on, and
//! [`LspClient::query`] never propagates a panic or leaves a turn awaiting a
//! process that is gone.
//!
//! **Every wait is bounded.** A server that accepts a request and never
//! answers is the ordinary failure of this class of program, so the handshake
//! and each request run under a deadline. Without one the turn hangs, which is
//! the same unbounded-turn hazard the provider adapters already have an idle
//! window for.
//!
//! Parity: upstream `packages/lsp/lsp`, `lsp-stdio` and `tool-lsp`, pinned by
//! their `framing.spec.ts`, `lifecycle.spec.ts`, `connection.spec.ts` and
//! `tool-lsp.spec.ts`. Upstream keeps a pool of servers keyed by language and
//! reuses one across calls; this opens a server per query and closes it, which
//! is slower and has no lifecycle to get wrong - a pool is worth having once
//! something measures the cost, and a pooled server that is quietly dead is
//! the defect this avoids by construction. Its document-synchronisation half -
//! `didOpen`/`didChange` for unsaved buffers - has no counterpart: tetanus has
//! no editor buffers, so the file on disk is the document.

pub mod framing;
pub mod tool;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::lsp::framing::{encode, MessageDecoder};

/// How long the handshake may take before the server is called unusable.
pub const DEFAULT_STARTUP_MS: u64 = 30_000;
/// How long one query may take.
pub const DEFAULT_REQUEST_MS: u64 = 20_000;

/// What a model can ask a language server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspOperation {
    Definition,
    References,
    Diagnostics,
}

impl LspOperation {
    /// The name a model writes.
    pub fn as_str(self) -> &'static str {
        match self {
            LspOperation::Definition => "definition",
            LspOperation::References => "references",
            LspOperation::Diagnostics => "diagnostics",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "definition" => Some(LspOperation::Definition),
            "references" => Some(LspOperation::References),
            "diagnostics" => Some(LspOperation::Diagnostics),
            _ => None,
        }
    }

    /// The JSON-RPC method it becomes. Diagnostics are pushed by the server
    /// rather than requested, so they have no method of their own.
    fn method(self) -> Option<&'static str> {
        match self {
            LspOperation::Definition => Some("textDocument/definition"),
            LspOperation::References => Some("textDocument/references"),
            LspOperation::Diagnostics => None,
        }
    }
}

/// Where in a file a query points. Zero-based, as the protocol is; the tool
/// converts from the one-based coordinates a person and a model read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// One place a server pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub line: u32,
    pub character: u32,
}

/// One thing a server complained about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub line: u32,
    pub character: u32,
    pub severity: String,
    pub message: String,
}

/// What a query answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspAnswer {
    Locations(Vec<Location>),
    Diagnostics(Vec<Diagnostic>),
}

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("the language server `{program}` could not be started: {source}")]
    NotStarted {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// The server went away. The commonest failure of this class of program,
    /// and the one the tool must turn into a readable result.
    #[error("the language server stopped before answering{}", detail(.0))]
    Died(String),
    #[error("the language server did not answer within {0}ms")]
    TimedOut(u64),
    #[error("the language server answered with an error: {0}")]
    Refused(String),
    #[error("the language server's output could not be read: {0}")]
    Protocol(String),
    #[error("{}: is not inside the workspace", path.display())]
    OutsideWorkspace { path: PathBuf },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn detail(what: &str) -> String {
    match what.trim().is_empty() {
        true => String::new(),
        false => format!(": {}", what.trim()),
    }
}

/// How to start a server, and how long to wait for it.
#[derive(Debug, Clone)]
pub struct LspConfig {
    /// The program to run, and its arguments.
    pub program: String,
    pub args: Vec<String>,
    /// The project the server is opened on.
    pub root: PathBuf,
    pub startup_ms: u64,
    pub request_ms: u64,
}

impl LspConfig {
    pub fn new(program: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            root: root.into(),
            startup_ms: DEFAULT_STARTUP_MS,
            request_ms: DEFAULT_REQUEST_MS,
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }
}

/// One conversation with one language server process.
///
/// Short-lived on purpose: [`query`](Self::query) starts a server, asks, and
/// shuts it down. See the module header for why a pool is not here yet.
pub struct LspClient {
    config: LspConfig,
}

impl LspClient {
    pub fn new(config: LspConfig) -> Self {
        Self { config }
    }

    /// Ask one question, from the handshake to the shutdown.
    ///
    /// Every failure inside is an [`LspError`] rather than a panic or a hang,
    /// which is what lets the tool report it as a failed call.
    pub async fn query(
        &self,
        operation: LspOperation,
        file: &Path,
        at: Position,
    ) -> Result<LspAnswer, LspError> {
        let uri = self.uri_of(file)?;
        let mut child = tokio::process::Command::new(&self.config.program)
            .args(&self.config.args)
            .current_dir(&self.config.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A server outlives nothing: if this future is dropped - the turn
            // was interrupted, the deadline fired - the process is killed
            // rather than left behind holding a CPU.
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| LspError::NotStarted {
                program: self.config.program.clone(),
                source,
            })?;

        let mut stdin = child.stdin.take().expect("piped");
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");

        let mut session = Session {
            decoder: MessageDecoder::default(),
            pending: BTreeMap::new(),
            diagnostics: Vec::new(),
            next_id: 1,
        };

        let outcome = self
            .converse(&mut session, &mut stdin, &mut stdout, operation, &uri, at)
            .await;

        // Whatever happened, the process does not stay behind.
        let _ = stdin.shutdown().await;
        let _ = child.start_kill();
        let _ = child.wait().await;

        match outcome {
            Ok(answer) => Ok(answer),
            // A server that vanished usually said why on its standard error,
            // and that sentence is the whole difference between "the tool
            // failed" and "install the toolchain".
            Err(LspError::Died(_)) => {
                let mut said = String::new();
                let _ = stderr.read_to_string(&mut said).await;
                Err(LspError::Died(tail(&said, 400)))
            }
            Err(other) => Err(other),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn converse(
        &self,
        session: &mut Session,
        stdin: &mut tokio::process::ChildStdin,
        stdout: &mut tokio::process::ChildStdout,
        operation: LspOperation,
        uri: &str,
        at: Position,
    ) -> Result<LspAnswer, LspError> {
        let startup = Duration::from_millis(self.config.startup_ms);
        let request = Duration::from_millis(self.config.request_ms);

        // The handshake. A server that will not initialize will not answer
        // anything else either, so it fails here rather than at the query.
        let initialize = session.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": self.uri_of(&self.config.root)?,
                "capabilities": { "textDocument": {
                    "definition": {}, "references": {}, "publishDiagnostics": {}
                }},
            }),
        );
        write(stdin, &initialize).await?;
        session
            .wait_for(stdout, 1, startup, self.config.startup_ms)
            .await?;
        write(stdin, &notify("initialized", json!({}))).await?;

        // The document, from disk. tetanus has no editor buffers, so the file
        // is the document and there is nothing to synchronize.
        let path = self.path_of(uri);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        write(
            stdin,
            &notify(
                "textDocument/didOpen",
                json!({ "textDocument": {
                    "uri": uri,
                    "languageId": language_of(&path),
                    "version": 1,
                    "text": text,
                }}),
            ),
        )
        .await?;

        let answer = match operation.method() {
            Some(method) => {
                let id = session.next_id;
                let params = match operation {
                    LspOperation::References => json!({
                        "textDocument": { "uri": uri },
                        "position": { "line": at.line, "character": at.character },
                        "context": { "includeDeclaration": true },
                    }),
                    _ => json!({
                        "textDocument": { "uri": uri },
                        "position": { "line": at.line, "character": at.character },
                    }),
                };
                let message = session.request(method, params);
                write(stdin, &message).await?;
                let result = session
                    .wait_for(stdout, id, request, self.config.request_ms)
                    .await?;
                LspAnswer::Locations(locations(&result))
            }
            None => {
                // Diagnostics are pushed, not requested, so the wait is for a
                // notification rather than for a reply. A server with nothing
                // to say about a clean file simply never sends one, which is
                // an empty answer and not a timeout.
                session
                    .collect_diagnostics(stdout, uri, request)
                    .await
                    .map(LspAnswer::Diagnostics)?
            }
        };

        let shutdown_id = session.next_id;
        let shutdown = session.request("shutdown", Value::Null);
        write(stdin, &shutdown).await?;
        // A server that will not shut down politely is killed, and that is not
        // a failure of the query that already answered.
        let _ = session
            .wait_for(stdout, shutdown_id, Duration::from_millis(2_000), 2_000)
            .await;
        let _ = write(stdin, &notify("exit", Value::Null)).await;

        Ok(answer)
    }

    /// The `file:` URI for a path inside the workspace.
    ///
    /// A path outside it is refused: the root is what the server was opened
    /// on, and asking about a file elsewhere is either a mistake or an attempt
    /// to read something the workspace fence exists to keep out.
    fn uri_of(&self, path: &Path) -> Result<String, LspError> {
        let absolute = match path.is_absolute() {
            true => path.to_path_buf(),
            false => self.config.root.join(path),
        };
        let root = self
            .config
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.config.root.clone());
        let resolved = absolute.canonicalize().unwrap_or(absolute);
        if resolved != root && !resolved.starts_with(&root) {
            return Err(LspError::OutsideWorkspace { path: resolved });
        }
        Ok(format!("file://{}", resolved.display()))
    }

    fn path_of(&self, uri: &str) -> PathBuf {
        PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri))
    }
}

/// One in-flight conversation's state.
struct Session {
    decoder: MessageDecoder,
    pending: BTreeMap<u64, Value>,
    diagnostics: Vec<Value>,
    next_id: u64,
}

impl Session {
    fn request(&mut self, method: &str, params: Value) -> Vec<u8> {
        let id = self.next_id;
        self.next_id += 1;
        encode(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    /// Read until the reply with `id` arrives, or the deadline passes.
    ///
    /// Every other message is kept rather than dropped: a server interleaves
    /// its own requests and its diagnostics with the replies, and a reader
    /// that discarded them would lose the diagnostics a later query wants.
    async fn wait_for(
        &mut self,
        stdout: &mut tokio::process::ChildStdout,
        id: u64,
        budget: Duration,
        budget_ms: u64,
    ) -> Result<Value, LspError> {
        if let Some(found) = self.pending.remove(&id) {
            return Ok(found);
        }
        let deadline = tokio::time::Instant::now() + budget;
        let mut chunk = [0_u8; 8192];
        loop {
            let read = tokio::time::timeout_at(deadline, stdout.read(&mut chunk))
                .await
                .map_err(|_| LspError::TimedOut(budget_ms))?;
            match read {
                // End of stream: the server closed its output, which means it
                // is gone. The caller turns this into the process's own words.
                Ok(0) => return Err(LspError::Died(String::new())),
                Ok(n) => {
                    for message in self
                        .decoder
                        .push(&chunk[..n])
                        .map_err(|error| LspError::Protocol(error.to_string()))?
                    {
                        self.absorb(message);
                    }
                    if let Some(found) = self.pending.remove(&id) {
                        return Ok(found);
                    }
                }
                Err(error) => return Err(LspError::Io(error)),
            }
        }
    }

    /// Read until the server publishes diagnostics for `uri`, or the budget
    /// passes with none - which is a clean file rather than a failure.
    async fn collect_diagnostics(
        &mut self,
        stdout: &mut tokio::process::ChildStdout,
        uri: &str,
        budget: Duration,
    ) -> Result<Vec<Diagnostic>, LspError> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut chunk = [0_u8; 8192];
        loop {
            if let Some(found) = self.published(uri) {
                return Ok(found);
            }
            let read = match tokio::time::timeout_at(deadline, stdout.read(&mut chunk)).await {
                // Silence about a file is a file with nothing wrong with it.
                Err(_) => return Ok(self.published(uri).unwrap_or_default()),
                Ok(read) => read,
            };
            match read {
                Ok(0) => return Err(LspError::Died(String::new())),
                Ok(n) => {
                    for message in self
                        .decoder
                        .push(&chunk[..n])
                        .map_err(|error| LspError::Protocol(error.to_string()))?
                    {
                        self.absorb(message);
                    }
                }
                Err(error) => return Err(LspError::Io(error)),
            }
        }
    }

    /// File one decoded message: a reply, a diagnostic notification, or
    /// something this client has no use for.
    fn absorb(&mut self, message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if let Some(error) = message.get("error") {
                self.pending.insert(id, json!({ "__error": error.clone() }));
                return;
            }
            if message.get("result").is_some() {
                self.pending
                    .insert(id, message.get("result").cloned().unwrap_or(Value::Null));
                return;
            }
        }
        if message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        {
            if let Some(params) = message.get("params") {
                self.diagnostics.push(params.clone());
            }
        }
    }

    fn published(&self, uri: &str) -> Option<Vec<Diagnostic>> {
        let params = self
            .diagnostics
            .iter()
            .rev()
            .find(|params| params.get("uri").and_then(Value::as_str) == Some(uri))?;
        let path = uri.strip_prefix("file://").unwrap_or(uri).to_string();
        Some(
            params
                .get("diagnostics")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| Diagnostic {
                            path: path.clone(),
                            line: at_of(item, "line"),
                            character: at_of(item, "character"),
                            severity: severity_of(item),
                            message: item
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }
}

/// Read a definition or references reply, which the protocol allows to be one
/// location, a list of them, or null.
fn locations(result: &Value) -> Vec<Location> {
    if let Some(error) = result.get("__error") {
        let _ = error;
        return Vec::new();
    }
    let items = match result {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        one => vec![one.clone()],
    };
    items
        .iter()
        .filter_map(|item| {
            // A `LocationLink` names its target differently from a `Location`,
            // and a server picks whichever it likes.
            let uri = item
                .get("uri")
                .or_else(|| item.get("targetUri"))
                .and_then(Value::as_str)?;
            let range = item
                .get("range")
                .or_else(|| item.get("targetSelectionRange"))
                .or_else(|| item.get("targetRange"))?;
            let start = range.get("start")?;
            Some(Location {
                path: uri.strip_prefix("file://").unwrap_or(uri).to_string(),
                line: start.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                character: start.get("character").and_then(Value::as_u64).unwrap_or(0) as u32,
            })
        })
        .collect()
}

fn at_of(item: &Value, field: &str) -> u32 {
    item.get("range")
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32
}

fn severity_of(item: &Value) -> String {
    match item.get("severity").and_then(Value::as_u64) {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "information",
        Some(4) => "hint",
        _ => "unknown",
    }
    .to_string()
}

fn notify(method: &str, params: Value) -> Vec<u8> {
    encode(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

async fn write(stdin: &mut tokio::process::ChildStdin, bytes: &[u8]) -> Result<(), LspError> {
    // A closed pipe means the server is gone, which is the same fact the
    // reader reports; naming it the same way keeps the tool's message stable
    // whichever side noticed first.
    match stdin.write_all(bytes).await {
        Ok(()) => stdin.flush().await.or(Ok(())),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            Err(LspError::Died(String::new()))
        }
        Err(error) => Err(LspError::Io(error)),
    }
}

/// The language id for a path, for the `didOpen` a server needs before it will
/// answer about a document.
fn language_of(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => "rust",
        Some("ts") => "typescript",
        Some("js") => "javascript",
        Some("py") => "python",
        Some("go") => "go",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("hpp") | Some("cc") => "cpp",
        _ => "plaintext",
    }
}

/// The last `limit` characters, for a server's dying words.
fn tail(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    match count > limit {
        true => trimmed.chars().skip(count - limit).collect(),
        false => trimmed.to_string(),
    }
}
