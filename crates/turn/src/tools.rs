//! The tool registry and the model-facing schemas it contributes to prompt
//! assembly, plus the concurrency class a pending call is scheduled by.
//! The scheduler that reads that class is `TurnEngine::run_tool_calls`.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;

/// One call the model asked for.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// What a tool advertises to the model. Registered schemas join prompt
/// assembly; nothing else decides what the model may call.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// The immutable outcome of one call, as `tool/result` records it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolOutcome {
    pub ok: bool,
    pub content: String,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
        }
    }
    pub fn failed(content: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool {0:?} failed: {1}")]
    Failed(String, String),
    #[error("unknown tool {0:?}")]
    Unknown(String),
    #[error("invalid arguments for {0:?}: {1}")]
    InvalidArguments(String, String),
}

/// How one pending call may overlap with its siblings in the same step.
///
/// The classification is per call, not per tool: the same tool can be safe to
/// overlap for a read and unsafe for a write, and only the arguments say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    /// Safe to run beside its siblings.
    Parallel,
    /// Runs alone. It is a barrier: everything before it settles first, and
    /// nothing after it starts until it is done.
    Exclusive,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;

    /// Classify one pending call. Fail closed: the default is
    /// [`ToolMode::Exclusive`], so overlapping is something a tool opts into
    /// for arguments it has looked at, never something it gets by silence.
    fn mode(&self, arguments: &serde_json::Value) -> ToolMode {
        let _ = arguments;
        ToolMode::Exclusive
    }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.schema().name, tool);
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    /// Schemas in a stable order, so one prompt is byte-identical across runs.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.tools.keys()
    }

    /// Classify a pending call. A call naming no registered tool is exclusive:
    /// it is about to fail, and it fails on its own.
    pub fn mode(&self, call: &ToolCall) -> ToolMode {
        self.tools
            .get(&call.name)
            .map_or(ToolMode::Exclusive, |tool| tool.mode(&call.arguments))
    }

    /// Run one call. A tool that fails, and a call naming no tool at all, both
    /// come back as a [`ToolError`] the engine turns into a failed result the
    /// model reads.
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolOutcome, ToolError> {
        match self.tools.get(&call.name) {
            Some(tool) => contained(&call.name, tool.execute(&call.arguments)).await,
            None => Err(ToolError::Unknown(call.name.clone())),
        }
    }
}

/// Run a tool body, and treat a panic in it as that call failing.
///
/// A tool body is somebody else's code. A bug in it is one call's failure, told
/// to the model like any other, never the turn's: the loop is not the tool
/// author's to take down, and the sibling calls in the same step still owe the
/// model a result. Upstream contains a thrown value the same way
/// (`packages/core/tools`, "returns isError results for unknown tools and
/// throwing tools").
///
/// A dispatch listener is the other side of this line: `serial` and `waterfall`
/// listeners decide, so a panic in one stays loud.
async fn contained(
    name: &str,
    body: impl std::future::Future<Output = Result<ToolOutcome, ToolError>>,
) -> Result<ToolOutcome, ToolError> {
    match AssertUnwindSafe(body).catch_unwind().await {
        Ok(result) => result,
        Err(payload) => {
            let fault = panicked(payload);
            tracing::error!(tool = name, %fault, "a tool panicked");
            Err(ToolError::Failed(name.to_string(), fault))
        }
    }
}

/// What a caught panic was about, as far as the payload says.
fn panicked(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "<unprintable panic payload>".to_string()
}

/// The one built-in tool Phase ① ships: enough to drive the documented tool
/// pipeline end to end without touching the filesystem or a subprocess.
pub struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "echo".into(),
            description: "Return the given text unchanged.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
        }
    }

    /// Echoing reads nothing and writes nothing, so any number of echoes may
    /// overlap.
    fn mode(&self, _arguments: &serde_json::Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        match arguments.get("text").and_then(serde_json::Value::as_str) {
            Some(text) => Ok(ToolOutcome::ok(text)),
            None => Err(ToolError::InvalidArguments(
                "echo".into(),
                "missing `text`".into(),
            )),
        }
    }
}
