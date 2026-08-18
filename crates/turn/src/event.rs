/// Session-durable vs live event taxonomy (upstream parity).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnEvent {
    TurnStart { turn_id: u64 },
    UserMessage { turn_id: u64, content: String },
    AssistantChunk { turn_id: u64, delta: String },
    AssistantMessage { turn_id: u64, content: String },
    ToolPreExecute { turn_id: u64, tool: String, args: serde_json::Value },
    ToolResult { turn_id: u64, tool: String, ok: bool, output: String },
    StepEnd { turn_id: u64, step: u32 },
    TurnStopping { turn_id: u64, reason: String },
}
