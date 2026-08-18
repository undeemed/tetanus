use tetanus_turn::engine::TurnEngine;
use tetanus_turn::event::TurnEvent;
use tetanus_turn::llm::EchoAdapter;
use tetanus_turn::tools::ToolRegistry;

#[tokio::test]
async fn emits_documented_event_sequence() {
    let mut eng = TurnEngine::new(Box::new(EchoAdapter), ToolRegistry::default());
    let mut kinds = Vec::new();
    let out = eng
        .run_turn("hello world".into(), |ev| {
            kinds.push(match ev {
                TurnEvent::TurnStart { .. } => "turn/start",
                TurnEvent::UserMessage { .. } => "user/message",
                TurnEvent::AssistantChunk { .. } => "assistant/chunk",
                TurnEvent::AssistantMessage { .. } => "assistant/message",
                TurnEvent::StepEnd { .. } => "step/end",
                TurnEvent::TurnStopping { .. } => "agent/turn-stopping",
                _ => "other",
            });
        })
        .await
        .unwrap();
    assert_eq!(out, "hello world");
    assert_eq!(kinds.first(), Some(&"turn/start"));
    assert!(kinds.contains(&"assistant/chunk"));
    assert_eq!(kinds.last(), Some(&"agent/turn-stopping"));
}
