//! The tool registry and the model-facing schemas it contributes to prompt
//! assembly. Phase ① runs one call at a time through the documented pipeline;
//! the concurrency classes upstream schedules (barriers, rolling pool) are a
//! Phase ② concern.

use std::collections::BTreeMap;
use std::sync::Arc;

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

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
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

    pub async fn execute(&self, call: &ToolCall) -> Result<ToolOutcome, ToolError> {
        match self.tools.get(&call.name) {
            Some(tool) => tool.execute(&call.arguments).await,
            None => Err(ToolError::Unknown(call.name.clone())),
        }
    }
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
