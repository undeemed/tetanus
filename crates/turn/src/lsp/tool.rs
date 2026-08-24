//! The model-facing `lsp` tool.
//!
//! **A server that dies is a failed call, never a dead turn.** Every
//! [`LspError`] becomes a `ToolError::Failed` the model reads, because a
//! language server crashing is an ordinary event and ending the conversation
//! over it would be the wrong response to a program the user did not write.
//!
//! **Coordinates are one-based here and zero-based on the wire.** A person and
//! a model both count lines from one, and the protocol counts from zero. The
//! conversion happens in exactly this one place, so nothing else in the
//! workspace has to remember which convention it is holding.
//!
//! **The tool is exclusive.** It starts a subprocess that indexes a project,
//! which is not something to have several of at once, and the default mode is
//! exclusive anyway - overlapping is something a tool opts into for arguments
//! it has looked at.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::lsp::{LspAnswer, LspClient, LspConfig, LspOperation, Position};
use crate::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// The name the model calls this by.
pub const NAME: &str = "lsp";

/// Most locations the tool prints before it says how many it left out.
pub const MAX_LOCATIONS: usize = 50;

/// The guidance that stops a model reaching for this instead of `grep`.
pub const LSP_GUIDANCE: &str = "\
Use ordinary search and read for navigation. Use this when a textual match is \
ambiguous, or before a change that needs the exact definition or every real \
reference. Line and character are one-based, at the cursor; a position that is \
not on a symbol answers nothing.";

/// The `lsp` tool.
pub struct LspTool {
    config: LspConfig,
}

impl LspTool {
    pub fn new(config: LspConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl Tool for LspTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.into(),
            description: format!(
                "Ask a language server a precise question about this project. {LSP_GUIDANCE}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["definition", "references", "diagnostics"],
                        "description": "definition, references, or diagnostics.",
                    },
                    "file": {
                        "type": "string",
                        "description": "Path to the file, relative to the project root.",
                    },
                    "line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "One-based line of the cursor. Not needed for diagnostics.",
                    },
                    "character": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "One-based character of the cursor. Not needed for diagnostics.",
                    },
                },
                "required": ["operation", "file"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let operation = arguments
            .get("operation")
            .and_then(Value::as_str)
            .and_then(LspOperation::parse)
            .ok_or_else(|| {
                ToolError::InvalidArguments(
                    NAME.into(),
                    "operation must be one of definition, references, diagnostics".into(),
                )
            })?;
        let file = arguments
            .get("file")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| ToolError::InvalidArguments(NAME.into(), "file is required".into()))?;

        // One-based from the model, zero-based on the wire. A zero would
        // underflow, so it is refused with the rule rather than wrapped.
        let at = match operation {
            LspOperation::Diagnostics => Position {
                line: 0,
                character: 0,
            },
            _ => Position {
                line: one_based(arguments, "line")?,
                character: one_based(arguments, "character")?,
            },
        };

        let client = LspClient::new(self.config.clone());
        match client.query(operation, &PathBuf::from(file), at).await {
            Ok(answer) => Ok(ToolOutcome::ok(render(operation, &answer))),
            // Every failure of the server is a failed call the model can read
            // and act on, and never the end of the turn.
            Err(error) => Err(ToolError::Failed(NAME.into(), error.to_string())),
        }
    }
}

fn one_based(arguments: &Value, field: &str) -> Result<u32, ToolError> {
    match arguments.get(field).and_then(Value::as_u64) {
        Some(0) => Err(ToolError::InvalidArguments(
            NAME.into(),
            format!("{field} is one-based, so it cannot be 0"),
        )),
        Some(value) => Ok((value - 1) as u32),
        None => Err(ToolError::InvalidArguments(
            NAME.into(),
            format!("{field} is required for this operation"),
        )),
    }
}

/// What the model reads back.
///
/// An empty answer says so in words rather than being an empty string: "no
/// results" and "the tool printed nothing" are different facts, and a model
/// that cannot tell them apart will ask again.
fn render(operation: LspOperation, answer: &LspAnswer) -> String {
    match answer {
        LspAnswer::Locations(found) if found.is_empty() => {
            format!("no {} found at that position", operation.as_str())
        }
        LspAnswer::Locations(found) => {
            let shown = found.len().min(MAX_LOCATIONS);
            let mut lines: Vec<String> = found
                .iter()
                .take(shown)
                .map(|at| format!("{}:{}:{}", at.path, at.line + 1, at.character + 1))
                .collect();
            if found.len() > shown {
                lines.push(format!("... and {} more", found.len() - shown));
            }
            lines.join("\n")
        }
        LspAnswer::Diagnostics(found) if found.is_empty() => "no diagnostics".to_string(),
        LspAnswer::Diagnostics(found) => found
            .iter()
            .take(MAX_LOCATIONS)
            .map(|item| {
                format!(
                    "{}:{}:{}: {}: {}",
                    item.path,
                    item.line + 1,
                    item.character + 1,
                    item.severity,
                    item.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
