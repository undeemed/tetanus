//! `run_code`, as the model sees it.
//!
//! **A failed program is a failed tool call, and the turn survives it.** That
//! is the same containment every other tool gets from
//! `crates/turn/src/tools.rs`, and it is why the failure class leads the text:
//! `[timeout]` and `[exception]` are different news for the model, and the
//! second one is something it can fix by writing the program again.
//!
//! **The tool is exclusive.** A program can call host bindings that do
//! anything the deployment wired up, so nothing here knows whether two
//! programs may overlap - and `ToolMode` is opt-in for exactly that reason.
//!
//! **What the model reads back is the value, then the logs.** In that order,
//! because the value is the answer and the logs are the working; and both,
//! because a program that logged its way to a conclusion has put the useful
//! part in the logs.

use std::sync::Arc;

use serde_json::Value;
use tetanus_turn::tools::{Tool, ToolError, ToolMode, ToolOutcome, ToolSchema};

use crate::types::{CodeRuntime, Namespace, RunRequest, RunResult};

/// Run a program on a code runtime, as a tool.
pub struct CodeTool {
    runtime: Arc<dyn CodeRuntime>,
    /// The namespaces every program this tool runs is given. Fixed at
    /// composition: a model cannot ask for a binding it was not offered.
    bindings: Vec<Namespace>,
    /// How much of the result the model reads. A separate bound from the
    /// runtime's output cap: one is what the program may produce, this is what
    /// one step of a turn spends on reading it.
    max_output: usize,
}

impl CodeTool {
    pub const NAME: &'static str = "run_code";

    pub fn new(runtime: Arc<dyn CodeRuntime>) -> Self {
        Self {
            runtime,
            bindings: Vec::new(),
            max_output: 16_000,
        }
    }

    /// Offer every program this tool runs one more namespace.
    pub fn binding(mut self, namespace: Namespace) -> Self {
        self.bindings.push(namespace);
        self
    }

    pub fn max_output(mut self, max_output: usize) -> Self {
        self.max_output = max_output;
        self
    }

    /// What the model is told it can write, including the bindings it has.
    ///
    /// The list is generated from the namespaces rather than written by hand,
    /// so a deployment that adds a binding does not also have to remember to
    /// describe it.
    fn description(&self) -> String {
        let mut text = format!(
            "Run a short program on the {} runtime and get back its value and its logs. \
             Statements: `let x = ...;`, assignment, `if (c) {{ }} else {{ }}`, \
             `while (c) {{ }}`, `return v;`. Values are JSON: numbers, strings, booleans, null, \
             lists and objects. Builtins: log, len, keys, str, num, push, floor.",
            self.runtime.language()
        );
        for namespace in &self.bindings {
            let members = namespace
                .functions
                .keys()
                .map(|name| format!("{}.{name}(argument)", namespace.global))
                .collect::<Vec<String>>()
                .join(", ");
            text.push_str(&format!(" Available: {members}."));
        }
        text
    }
}

#[async_trait::async_trait]
impl Tool for CodeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.to_string(),
            description: self.description(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "program": {
                        "type": "string",
                        "description": "The program to run. Its `return` value is the answer.",
                    },
                },
                "required": ["program"],
            }),
        }
    }

    /// Exclusive: see the module note. A program can reach whatever its
    /// bindings reach, and nothing here has looked at them.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let program = match arguments.get("program").and_then(Value::as_str) {
            Some(program) if !program.trim().is_empty() => program.to_string(),
            Some(_) => {
                return Err(ToolError::InvalidArguments(
                    Self::NAME.to_string(),
                    "`program` is empty; there is nothing to run".to_string(),
                ))
            }
            None => {
                return Err(ToolError::InvalidArguments(
                    Self::NAME.to_string(),
                    "`program` is required".to_string(),
                ))
            }
        };

        let mut request = RunRequest::new(program);
        request.bindings = self.bindings.clone();

        match self.runtime.run(request).await {
            Ok(result) if result.is_ok() => Ok(ToolOutcome::ok(render(&result, self.max_output))),
            // A program that failed is the model's news, with the class first
            // so it can tell "fix your program" from "it ran too long".
            Ok(result) => Err(ToolError::Failed(
                Self::NAME.to_string(),
                render(&result, self.max_output),
            )),
            // Seam misuse is the composer's mistake, not the model's: the
            // model wrote a program and the harness was wired up wrongly.
            Err(misuse) => Err(ToolError::Failed(
                Self::NAME.to_string(),
                format!("[seam] {misuse}"),
            )),
        }
    }
}

/// One run, as the model reads it.
pub fn render(result: &RunResult, max_output: usize) -> String {
    let mut out = String::new();
    if let Some(failure) = &result.error {
        out.push_str(&format!("[{}] {}\n", failure.kind, failure.message));
    }
    if let Some(value) = &result.value {
        out.push_str(&format!("value: {value}\n"));
    } else if result.error.is_none() {
        out.push_str("value: (the program returned nothing)\n");
    }
    if !result.logs.is_empty() {
        out.push_str("logs:\n");
        for line in &result.logs {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.push_str(&format!("ran in {}ms", result.duration.as_millis()));

    match out.char_indices().nth(max_output) {
        None => out,
        Some((at, _)) => {
            out.truncate(at);
            out.push_str("\n[the result was longer than this tool returns]");
            out
        }
    }
}
