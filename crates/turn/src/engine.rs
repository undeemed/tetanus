use crate::event::TurnEvent;
use crate::llm::LlmAdapter;
use crate::tools::ToolRegistry;
use tokio::sync::mpsc;

pub struct TurnEngine {
    pub llm: Box<dyn LlmAdapter>,
    pub tools: ToolRegistry,
    next_turn: u64,
}

impl TurnEngine {
    pub fn new(llm: Box<dyn LlmAdapter>, tools: ToolRegistry) -> Self {
        Self { llm, tools, next_turn: 1 }
    }

    /// Run one turn; emits the documented event sequence to `emit`.
    pub async fn run_turn(
        &mut self,
        user_input: String,
        mut emit: impl FnMut(TurnEvent),
    ) -> Result<String, crate::llm::LlmError> {
        let turn_id = self.next_turn;
        self.next_turn += 1;
        emit(TurnEvent::TurnStart { turn_id });
        emit(TurnEvent::UserMessage { turn_id, content: user_input.clone() });

        let (tx, mut rx) = mpsc::channel(64);
        let stream = self.llm.stream(user_input, tx);
        tokio::pin!(stream);
        let mut full = String::new();
        loop {
            tokio::select! {
                chunk = rx.recv() => match chunk {
                    Some(delta) => {
                        full.push_str(&delta);
                        emit(TurnEvent::AssistantChunk { turn_id, delta });
                    }
                    None => break,
                },
                res = &mut stream => { res?; while let Some(delta) = rx.recv().await {
                    full.push_str(&delta);
                    emit(TurnEvent::AssistantChunk { turn_id, delta });
                } break; }
            }
        }
        emit(TurnEvent::AssistantMessage { turn_id, content: full.clone() });
        emit(TurnEvent::StepEnd { turn_id, step: 1 });
        emit(TurnEvent::TurnStopping { turn_id, reason: "complete".into() });
        Ok(full)
    }
}
