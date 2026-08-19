//! Test Design Specification: boot composition.
//!
//! Feature under test: the model adapter, the tool registry and the session log
//! are resolved through the typed service registry at boot. Nothing in the turn
//! engine names a concrete implementation, and a missing provider fails at boot
//! with the service named.

use std::sync::Arc;

use tetanus_core::{Context, EventBus, Registry, Service};
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::{
    boot, AgentLoopPlugin, LlmPlugin, LlmService, SessionService, ToolsPlugin, ToolsService,
};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine};

fn fixture() -> (tempfile::TempDir, EventBus, Arc<dyn SessionLog>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let bus = EventBus::new();
    let log: Arc<dyn SessionLog> =
        JsonlSessionLog::create("boot", dir.path().join("boot.jsonl"), bus.clone())
            .expect("journal");
    (dir, bus, log)
}

/// TC-BOOT-1: a full boot provides the three services under their documented
/// keys and yields an engine.
/// Expected: keys `["llm", "sessions", "tools"]`; the engine resolves and runs.
#[tokio::test]
async fn boot_provides_every_service_the_loop_needs() {
    let (_dir, bus, log) = fixture();
    let tools = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));

    let ctx = boot(bus, Arc::new(MockAdapter::new()), tools, log).expect("boot");

    assert_eq!(
        ctx.services.keys().collect::<Vec<_>>(),
        vec!["llm", "sessions", "tools"]
    );
    assert_eq!(
        ctx.services.require::<LlmService>().unwrap().provider(),
        "mock"
    );
    assert_eq!(
        ctx.services
            .require::<ToolsService>()
            .unwrap()
            .names()
            .collect::<Vec<_>>(),
        vec!["echo"]
    );
    assert_eq!(
        ctx.services.require::<SessionService>().unwrap().id(),
        "boot"
    );

    let engine = TurnEngine::from_context(&ctx, TurnConfig::default()).expect("engine");
    assert_eq!(engine.run_turn("hello").await.unwrap().steps, 2);
}

/// TC-BOOT-2: plugins mount in dependency order, so the driver never resolves a
/// service before its provider installed it.
/// Expected: `agent-loop` is last in the start order.
#[test]
fn plugins_mount_in_dependency_order() {
    let (_dir, bus, log) = fixture();
    let mut registry = Registry::new();
    registry.insert(Box::new(AgentLoopPlugin)).unwrap();
    registry
        .insert(Box::new(LlmPlugin {
            adapter: Arc::new(MockAdapter::new()),
        }))
        .unwrap();
    registry
        .insert(Box::new(ToolsPlugin {
            tools: Arc::new(ToolRegistry::new()),
        }))
        .unwrap();
    registry
        .insert(Box::new(tetanus_turn::boot::SessionPlugin { log }))
        .unwrap();

    let mut ctx = Context::with_bus(bus);
    let order = registry.start_all(&mut ctx).expect("start");

    assert_eq!(order.last().unwrap().0, "agent-loop");
}

/// TC-BOOT-3: a missing provider is a boot failure naming the service, not a
/// surprise mid-turn.
/// Expected: `start_all` fails and the message names `"llm"`.
#[test]
fn a_missing_provider_fails_at_boot() {
    let bus = EventBus::new();
    let mut registry = Registry::new();
    registry.insert(Box::new(AgentLoopPlugin)).unwrap();

    let mut ctx = Context::with_bus(bus);
    let err = registry
        .start_all(&mut ctx)
        .expect_err("agent-loop needs providers");

    assert!(err.to_string().contains("agent-loop"), "{err}");
}

/// TC-BOOT-4: one service definition takes exactly one provider.
/// Expected: the second `provide` fails naming the key.
#[test]
fn a_service_takes_one_provider() {
    let mut services = tetanus_core::Services::new();
    services
        .provide::<ToolsService>(Arc::new(ToolRegistry::new()))
        .unwrap();

    let err = services
        .provide::<ToolsService>(Arc::new(ToolRegistry::new()))
        .expect_err("duplicate");

    assert!(err.to_string().contains(ToolsService::KEY), "{err}");
}

/// TC-BOOT-5: the engine is adapter-agnostic. Swapping only the `llm` provider
/// changes the answer without touching the loop.
/// Expected: the turn ends in one step with the stub adapter's text.
#[tokio::test]
async fn swapping_the_adapter_is_a_boot_time_change() {
    struct AlwaysSilent;
    #[async_trait::async_trait]
    impl tetanus_turn::llm::LlmAdapter for AlwaysSilent {
        fn provider(&self) -> &str {
            "silent"
        }
        async fn stream(
            &self,
            _request: &tetanus_turn::llm::ModelRequest,
            _sink: &mut dyn tetanus_turn::llm::ChunkSink,
        ) -> Result<tetanus_turn::llm::ModelResponse, tetanus_turn::llm::LlmError> {
            Ok(tetanus_turn::llm::ModelResponse {
                content: "nothing to add".into(),
                finish_reason: "stop".into(),
                ..Default::default()
            })
        }
    }

    let (_dir, bus, log) = fixture();
    let tools = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));
    let ctx = boot(bus, Arc::new(AlwaysSilent), tools, log).expect("boot");
    let engine = TurnEngine::from_context(&ctx, TurnConfig::default()).expect("engine");

    let outcome = engine.run_turn("hi").await.unwrap();

    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.content, "nothing to add");
}
