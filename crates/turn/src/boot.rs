//! Boot composition: the service definitions the turn engine consumes and the
//! plugins that provide them. The engine resolves `llm`, `tools` and `sessions`
//! from the typed registry; swapping any of them is a boot-time change, not an
//! edit to the loop.

use std::sync::Arc;

use tetanus_core::effects::EffectError;
use tetanus_core::{Context, EventBus, Plugin, PluginId, Registry, RegistryError, Service};
use tetanus_session::SessionLog;

use crate::interrupt::Interrupt;
use crate::llm::LlmAdapter;
use crate::prompt::PromptRegistry;
use crate::runtime_context::ContextRegistry;
use crate::tools::ToolRegistry;

/// The model-provider seam.
pub struct LlmService;
impl Service for LlmService {
    const KEY: &'static str = "llm";
    type Provider = dyn LlmAdapter;
}

/// The model-facing capability registry.
pub struct ToolsService;
impl Service for ToolsService {
    const KEY: &'static str = "tools";
    type Provider = ToolRegistry;
}

/// The named prompt-section registry the assembly starts from.
pub struct PromptService;
impl Service for PromptService {
    const KEY: &'static str = "system-prompt";
    type Provider = PromptRegistry;
}

/// The durable session log.
pub struct SessionService;
impl Service for SessionService {
    const KEY: &'static str = "sessions";
    type Provider = dyn SessionLog;
}

/// The turn's stop switch, when the composition wants to share one.
///
/// The engine makes its own when nobody provides this, which is what every
/// composition did before there was work outside the process to stop. A tool
/// that starts a child process is the caller that needs the same switch the
/// loop reads: an interrupt has to reach the command, not just the step
/// boundary, or a turn the user stopped leaves a process running that nobody
/// will ever read the output of.
pub struct InterruptService;
impl Service for InterruptService {
    const KEY: &'static str = "interrupt";
    type Provider = Interrupt;
}

/// The providers that tell the model where it is (contract section 4.4.8).
///
/// Optional, like [`InterruptService`]: a composition that installs none gets
/// an empty registry and writes no `context/snapshot`, rather than having to
/// register something in order to say it wants nothing.
pub struct ContextService;
impl Service for ContextService {
    const KEY: &'static str = "runtimeContext";
    type Provider = ContextRegistry;
}

pub fn context_plugin_id() -> PluginId {
    PluginId::from("runtimeContext")
}

pub fn llm_plugin_id() -> PluginId {
    PluginId::from("llm")
}
pub fn tools_plugin_id() -> PluginId {
    PluginId::from("tools")
}
pub fn prompt_plugin_id() -> PluginId {
    PluginId::from("system-prompt")
}
pub fn session_plugin_id() -> PluginId {
    PluginId::from("session")
}
pub fn agent_loop_plugin_id() -> PluginId {
    PluginId::from("agent-loop")
}
pub fn interrupt_plugin_id() -> PluginId {
    PluginId::from("interrupt")
}

pub struct LlmPlugin {
    pub adapter: Arc<dyn LlmAdapter>,
}
impl Plugin for LlmPlugin {
    fn id(&self) -> PluginId {
        llm_plugin_id()
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        ctx.services
            .provide::<LlmService>(Arc::clone(&self.adapter))
            .map_err(|e| EffectError::Failed(e.to_string()))
    }
}

pub struct ToolsPlugin {
    pub tools: Arc<ToolRegistry>,
}
impl Plugin for ToolsPlugin {
    fn id(&self) -> PluginId {
        tools_plugin_id()
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        ctx.services
            .provide::<ToolsService>(Arc::clone(&self.tools))
            .map_err(|e| EffectError::Failed(e.to_string()))
    }
}

pub struct PromptPlugin {
    pub sections: Arc<PromptRegistry>,
}
impl Plugin for PromptPlugin {
    fn id(&self) -> PluginId {
        prompt_plugin_id()
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        ctx.services
            .provide::<PromptService>(Arc::clone(&self.sections))
            .map_err(|e| EffectError::Failed(e.to_string()))
    }
}

pub struct SessionPlugin {
    pub log: Arc<dyn SessionLog>,
}
impl Plugin for SessionPlugin {
    fn id(&self) -> PluginId {
        session_plugin_id()
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        ctx.services
            .provide::<SessionService>(Arc::clone(&self.log))
            .map_err(|e| EffectError::Failed(e.to_string()))
    }
}

/// Installs a composition's runtime-context providers.
pub struct ContextPlugin {
    pub context: Arc<ContextRegistry>,
}
impl Plugin for ContextPlugin {
    fn id(&self) -> PluginId {
        context_plugin_id()
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        ctx.services
            .provide::<ContextService>(Arc::clone(&self.context))
            .map_err(|e| EffectError::Failed(e.to_string()))
    }
}

pub struct InterruptPlugin {
    pub interrupt: Arc<Interrupt>,
}
impl Plugin for InterruptPlugin {
    fn id(&self) -> PluginId {
        interrupt_plugin_id()
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        ctx.services
            .provide::<InterruptService>(Arc::clone(&self.interrupt))
            .map_err(|e| EffectError::Failed(e.to_string()))
    }
}

/// The driver's own plugin. It provides nothing; it declares what the loop
/// needs, so a missing provider fails at boot with a named service instead of
/// mid-turn.
pub struct AgentLoopPlugin;
impl Plugin for AgentLoopPlugin {
    fn id(&self) -> PluginId {
        agent_loop_plugin_id()
    }
    fn deps(&self) -> Vec<PluginId> {
        vec![
            llm_plugin_id(),
            tools_plugin_id(),
            prompt_plugin_id(),
            session_plugin_id(),
        ]
    }
    fn start(&self, ctx: &mut Context) -> Result<(), EffectError> {
        let missing = [
            ctx.services
                .require::<LlmService>()
                .err()
                .map(|e| e.to_string()),
            ctx.services
                .require::<ToolsService>()
                .err()
                .map(|e| e.to_string()),
            ctx.services
                .require::<PromptService>()
                .err()
                .map(|e| e.to_string()),
            ctx.services
                .require::<SessionService>()
                .err()
                .map(|e| e.to_string()),
        ]
        .into_iter()
        .flatten()
        .next();
        match missing {
            Some(probe) => Err(EffectError::Failed(probe)),
            None => Ok(()),
        }
    }
}

/// Compose the Phase ① tree: four providers plus the driver, mounted in
/// dependency order onto one shared context.
pub fn boot(
    bus: EventBus,
    adapter: Arc<dyn LlmAdapter>,
    tools: Arc<ToolRegistry>,
    log: Arc<dyn SessionLog>,
) -> Result<Context, RegistryError> {
    boot_composed(bus, adapter, tools, log, None)
}

/// [`boot`], with the stop switch supplied by the composition rather than
/// minted by the engine.
///
/// A composition uses this when something outside the loop has to hear the
/// interrupt too - a tool holding a child process is the case it exists for.
/// The engine then reads this switch instead of making its own, so `cancel`
/// reaches both.
pub fn boot_with(
    bus: EventBus,
    adapter: Arc<dyn LlmAdapter>,
    tools: Arc<ToolRegistry>,
    log: Arc<dyn SessionLog>,
    interrupt: Arc<Interrupt>,
) -> Result<Context, RegistryError> {
    boot_composed(bus, adapter, tools, log, Some(interrupt))
}

fn boot_composed(
    bus: EventBus,
    adapter: Arc<dyn LlmAdapter>,
    tools: Arc<ToolRegistry>,
    log: Arc<dyn SessionLog>,
    interrupt: Option<Arc<Interrupt>>,
) -> Result<Context, RegistryError> {
    let mut registry = Registry::new();
    if let Some(interrupt) = interrupt {
        registry.insert(Box::new(InterruptPlugin { interrupt }))?;
    }
    registry.insert(Box::new(LlmPlugin { adapter }))?;
    registry.insert(Box::new(ToolsPlugin { tools }))?;
    registry.insert(Box::new(PromptPlugin {
        sections: PromptRegistry::new(),
    }))?;
    registry.insert(Box::new(SessionPlugin { log }))?;
    // Always installed and always empty to begin with, the same way prompt
    // sections are: a composition registers providers on it after boot rather
    // than choosing a different boot function, which is what stops one
    // optional extra from doubling the number of ways to start a harness.
    registry.insert(Box::new(ContextPlugin {
        context: Arc::new(ContextRegistry::new()),
    }))?;
    registry.insert(Box::new(AgentLoopPlugin))?;

    let mut ctx = Context::with_bus(bus);
    registry.start_all(&mut ctx)?;
    Ok(ctx)
}
