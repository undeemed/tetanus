//! The tool registry and the model-facing schemas it contributes to prompt
//! assembly, plus the concurrency class a pending call is scheduled by.
//! The scheduler that reads that class is `TurnEngine::run_tool_calls`.
//!
//! Schemas leave the registry in one settled order, because the order the model
//! reads its tools in is part of the prompt. That order is lexicographic unless
//! the harness configured a [`ToolOrder`], which names the tools it cares about
//! and leaves the rest to [`TOOL_ORDER_REST`].

use std::collections::{BTreeMap, BTreeSet};
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

/// The reserved entry in a configured tool order: the one place every tool the
/// order does not name is inserted, in canonical order.
///
/// Upstream exports the same string from its system-prompt plugin
/// (`TOOL_ORDER_REST`), and a deployment writes it in its config, so the value
/// is part of the surface rather than an implementation detail.
pub const TOOL_ORDER_REST: &str = "<unlisted-tools>";

#[derive(Debug, thiserror::Error)]
pub enum ToolOrderError {
    #[error("tool order lists {0:?} more than once")]
    Duplicate(String),
    #[error("tool order must contain the {TOOL_ORDER_REST:?} entry, which is where the tools it does not name go")]
    NoRest,
    #[error("a registered tool is named {TOOL_ORDER_REST:?}, which a tool order keeps for its rest entry")]
    Reserved,
    #[error("tool order lists {} {}; registered: {}", plural(.missing), quoted(.missing), listed(.registered))]
    Unregistered {
        missing: Vec<String>,
        registered: Vec<String>,
    },
}

/// A checked order for the tools the model is offered.
///
/// A value of this type has been read against the registry it was built from:
/// the rest entry is present exactly once, no name is listed twice, and every
/// other name is a tool that registry holds.
///
/// Upstream checks the same things later and in two places, because its plugins
/// register tools after the order is read: an unregistered name is only found
/// while a turn assembles its prompt, and that turn closes with no step. A
/// tetanus registry is settled before the engine is built, so the check has an
/// earlier home - the order cannot be constructed at all, and no turn starts on
/// a harness whose configuration was already wrong. `docs/parity.md` records the
/// difference.
#[derive(Debug, Clone)]
pub struct ToolOrder {
    names: Vec<String>,
}

impl ToolOrder {
    /// Read a configured order against the registry it will arrange.
    pub fn new<I, S>(names: I, registry: &ToolRegistry) -> Result<Self, ToolOrderError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        let mut seen = BTreeSet::new();
        for name in &names {
            if !seen.insert(name.as_str()) {
                return Err(ToolOrderError::Duplicate(name.clone()));
            }
        }
        if !seen.contains(TOOL_ORDER_REST) {
            return Err(ToolOrderError::NoRest);
        }
        if registry.tools.contains_key(TOOL_ORDER_REST) {
            return Err(ToolOrderError::Reserved);
        }
        let missing: Vec<String> = names
            .iter()
            .filter(|name| {
                name.as_str() != TOOL_ORDER_REST && !registry.tools.contains_key(name.as_str())
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(ToolOrderError::Unregistered {
                missing,
                registered: registry.names().cloned().collect(),
            });
        }
        Ok(Self { names })
    }
}

fn plural(missing: &[String]) -> &'static str {
    if missing.len() == 1 {
        "unregistered tool"
    } else {
        "unregistered tools"
    }
}

fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn listed(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
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

    /// Schemas in a configured order: each name the order lists at its place,
    /// and every other tool at [`TOOL_ORDER_REST`], in canonical order.
    ///
    /// A listed name this registry does not hold contributes nothing.
    /// [`ToolOrder::new`] has already refused that against the registry it read,
    /// so it takes an order applied to a second, different registry to reach -
    /// and upstream's `orderTools` drops such a name the same way.
    pub fn schemas_in(&self, order: &ToolOrder) -> Vec<ToolSchema> {
        let listed: BTreeSet<&str> = order.names.iter().map(String::as_str).collect();
        order
            .names
            .iter()
            .flat_map(|name| -> Vec<ToolSchema> {
                if name == TOOL_ORDER_REST {
                    self.tools
                        .iter()
                        .filter(|(name, _)| !listed.contains(name.as_str()))
                        .map(|(_, tool)| tool.schema())
                        .collect()
                } else {
                    self.tools
                        .get(name)
                        .map(|tool| tool.schema())
                        .into_iter()
                        .collect()
                }
            })
            .collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.tools.keys()
    }

    /// Classify a pending call. A call naming no registered tool is exclusive:
    /// it is about to fail, and it fails on its own.
    ///
    /// A classifier that panics is exclusive for the reason [`contained`]
    /// gives: the body is the tool author's, the answer is the scheduler's,
    /// and the answer that cannot make things worse is the one that overlaps
    /// nothing. Upstream fails the same way round (`executionMode` catches a
    /// throwing `isConcurrencySafe`), and the call still runs - a classifier
    /// with a bug in it costs concurrency, not the call.
    pub fn mode(&self, call: &ToolCall) -> ToolMode {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolMode::Exclusive;
        };
        match std::panic::catch_unwind(AssertUnwindSafe(|| tool.mode(&call.arguments))) {
            Ok(mode) => mode,
            Err(payload) => {
                let fault = panicked(payload);
                tracing::error!(tool = call.name, %fault, "a tool's classifier panicked");
                ToolMode::Exclusive
            }
        }
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
pub(crate) fn panicked(payload: Box<dyn std::any::Any + Send>) -> String {
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
