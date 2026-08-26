//! The engine behind `docs/interface-contract.md`.
//!
//! [`HarnessEngine`] is the one implementation of [`tetanus_protocol::Engine`].
//! The JSON-RPC carriers and the CLI both drive it, so no surface can serve a
//! different contract from another.
//!
//! This crate is a library. It prints nothing, and it owns no binary: the
//! presentation lane owns the binary and wires each subcommand to the calls
//! section 4.7 of the contract lists for it.

pub mod agent;
pub mod boot;
pub mod catalog;
pub mod convert;
pub mod preset;
pub mod retry;
pub mod session;
pub mod subscribe;
pub mod tools;

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use tetanus_protocol::methods::{
    capability, Ack, AgentPromptParams, AgentPromptResult, AgentStatusResult, ConfigDumpResult,
    Engine, EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo, SessionCreateParams,
    SessionEventsParams, SessionEventsResult, SessionForkParams, SessionListResult, SessionRef,
    SessionSubscribeParams, SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::SessionInfo;
use tetanus_protocol::{is_compatible, PROTOCOL_VERSION};
use tetanus_turn::tools::{EchoTool, ToolRegistry};

use crate::agent::{MockProviders, Providers, Runtime};
use crate::catalog::Catalogs;
use crate::session::{SessionBackend, SessionDefaults, SessionStore};
use crate::subscribe::Hub;

/// Where journals land when no caller names a directory.
///
/// A missing root here is a history nobody has written yet; a missing root the
/// caller named is a path that is wrong. [`session::SessionStore`] turns the
/// two into different answers, so the reader knows whether to run a turn or to
/// fix the path.
pub const DEFAULT_SESSIONS_ROOT: &str = "sessions";

/// Everything the engine needs that is not a call.
#[derive(Clone)]
pub struct EngineConfig {
    /// Directory holding this deployment's journals.
    pub sessions_root: PathBuf,
    /// The artifact those journals live in. Resolved from `sessions.backend`
    /// by [`crate::boot`], which opens a database before the engine is built
    /// so an unreadable store is a boot fault and not a first-turn surprise.
    pub sessions_backend: SessionBackend,
    /// Where this run came from, recorded on every journal it opens (contract
    /// section 4.4.9). The default records the process's own directory and no
    /// delegation, which is what an ordinary run is.
    pub session_origin: crate::session::SessionOrigin,
    /// Provider a `session.create` with no override resolves to.
    pub default_provider: String,
    /// Model a `session.create` with no override resolves to.
    pub default_model: String,
    pub max_steps: u32,
    /// How many parallel-safe tool calls of one step may be in flight at once,
    /// carried to every turn this engine runs.
    pub max_parallel_tool_calls: NonZeroUsize,
    /// The order the model reads its tools in, or `None` for the canonical
    /// one. Read against [`EngineConfig::tools`] by [`crate::tools::order`].
    pub tool_order: Option<tetanus_turn::tools::ToolOrder>,
    /// What a turn does with a model request that failed, on the route the
    /// session names. Resolved from the document by [`crate::retry::policy`].
    pub retry: tetanus_turn::llm::retry::RetryPolicy,
    /// The routes whose provider wrote a block of its own, and the policy that
    /// block describes. A route named here never reads [`EngineConfig::retry`]
    /// at all: a provider's block is the whole policy for its route. Resolved
    /// from the document by [`crate::retry::provider_policies`].
    pub provider_retry: BTreeMap<String, tetanus_turn::llm::retry::RetryPolicy>,
    /// What every child a composition starts for this deployment is confined
    /// to: commands, persistent shells, terminals and hooks alike.
    ///
    /// The engine starts no processes itself, so it never applies this - it
    /// settles it, because a policy is a document's answer like any other and
    /// two compositions parsing `sandbox.mode` for themselves is how one seam
    /// ends up confined and another does not. `crates/exec` applies it.
    pub sandbox: tetanus_sandbox::Policy,
    /// The adapter behind each provider a session may name.
    pub providers: Arc<dyn Providers>,
    /// The tools every turn on this engine can call, and the list
    /// `catalog.tools` advertises. A session composed from a preset that
    /// names a subset sees only that subset.
    pub tools: Arc<ToolRegistry>,
    /// Builds one session's own tools against that session's interrupt, for a
    /// composition whose tools hold work outside the process - a shell command
    /// is the case it exists for. `None` shares [`EngineConfig::tools`] with
    /// every session, which is right for tools that touch nothing an interrupt
    /// would have to reach.
    pub session_tools: Option<crate::agent::SessionTools>,
    /// The named agents a `session.create` may ask for, and the one it gets
    /// when it asks for none. Resolved from the settings document by
    /// [`preset::roster`].
    pub presets: preset::Roster,
    /// The layered config the caller resolved. The engine does not read it to
    /// configure itself - the fields above are already resolved - it reports
    /// its provenance, so `config.dump` can say where a value came from.
    pub resolved: Arc<tetanus_config::Config>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sessions_root: PathBuf::from(DEFAULT_SESSIONS_ROOT),
            sessions_backend: SessionBackend::Jsonl,
            session_origin: crate::session::SessionOrigin::default(),
            default_provider: tetanus_turn::llm::mock::PROVIDER.to_string(),
            default_model: tetanus_turn::llm::mock::MODEL.to_string(),
            max_steps: 8,
            max_parallel_tool_calls: tetanus_turn::engine::DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            tool_order: None,
            retry: tetanus_turn::llm::retry::RetryPolicy::default(),
            provider_retry: BTreeMap::new(),
            // No confinement unless a deployment asks for one, and named
            // rather than implied: this is the behaviour the harness has
            // always had, and a reader of the config page sees the word.
            sandbox: tetanus_sandbox::Policy::danger_full_access(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            // Offline by default: a build with no configuration still runs a
            // full documented turn, with no key and no network.
            providers: Arc::new(MockProviders),
            // The offline minimum, which is the `builtin` source of the
            // assembly the binary composes. Not the whole assembly: this
            // engine has no session, so the file tools would key their
            // observations on nobody and the feature tools would fold over a
            // journal that is not a session's - and `crates/engine` would gain
            // a dependency on every tool crate, which is the line ARCHITECTURE
            // §4.2 draws. TC-TOOLSET-2 holds the two together by name.
            tools: Arc::new(ToolRegistry::new().with(Arc::new(EchoTool))),
            // The library composes no tool that leaves the process; the
            // binary does, and sets this when it does.
            session_tools: None,
            presets: preset::Roster::new(),
            resolved: Arc::new(tetanus_config::Config::default()),
        }
    }
}

pub struct HarnessEngine {
    sessions: Arc<SessionStore>,
    hub: Arc<Hub>,
    runtime: Arc<Runtime>,
    catalogs: Catalogs,
}

impl HarnessEngine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            sessions: Arc::new(SessionStore::with_origin(
                config.sessions_root.clone(),
                SessionDefaults {
                    provider: config.default_provider.clone(),
                    model: config.default_model.clone(),
                    max_steps: config.max_steps,
                    presets: config.presets.clone(),
                },
                config.sessions_backend.clone(),
                config.session_origin.clone(),
            )),
            hub: Arc::new(Hub::new()),
            runtime: Arc::new(Runtime::new(&config)),
            catalogs: Catalogs::new(&config),
        }
    }

    /// Stop taking new work and close the turns already running (contract
    /// section 4.4.11).
    ///
    /// The engine half of a shutdown: it interrupts every running turn at the
    /// next step boundary and waits, bounded, for them to close, so a clean
    /// exit leaves nothing for crash repair to synthesize. Answers how many
    /// turns were still open when the budget ran out - zero for a drain that
    /// finished.
    ///
    /// **Refusing new calls is the carrier's, not this method's.** Section
    /// 4.4.11 says a stopping server closes the connection rather than
    /// answering a "server is stopping" code, because adding one is a change
    /// both lanes land together; a carrier that keeps accepting while this
    /// runs would simply be starting turns the drain has already passed.
    pub async fn drain(&self, budget: std::time::Duration) -> usize {
        self.runtime.drain(budget).await
    }

    pub fn sessions(&self) -> &Arc<SessionStore> {
        &self.sessions
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The tools one session may call, after the preset it was composed from
    /// has narrowed them. `tool.catalog` answers what the engine holds; this
    /// answers what this session is offered, and the two differ exactly when a
    /// preset says so.
    pub fn session_tools(&self, session_id: &str) -> Result<Vec<String>, RpcError> {
        let session = self.sessions.open(session_id)?;
        self.runtime.tools_for(&session)
    }

    /// The optional calls this build actually serves. A surface hides an
    /// affordance whose capability is absent, rather than discovering the
    /// absence as an error.
    pub fn capabilities(&self) -> Vec<String> {
        // A capability is a promise that the call behind it is served.
        vec![
            capability::SESSION_SUBSCRIBE.to_string(),
            capability::SESSION_FORK.to_string(),
            capability::AGENT_INTERRUPT.to_string(),
        ]
    }
}

#[async_trait::async_trait]
impl Engine for HarnessEngine {
    async fn hello(&self, params: HelloParams) -> Result<HelloResult, RpcError> {
        if !is_compatible(&params.protocol_version) {
            return Err(RpcError::new(
                ErrorCode::UnsupportedProtocolVersion,
                format!(
                    "this build serves contract {PROTOCOL_VERSION}, the client asked for {}",
                    params.protocol_version
                ),
            )
            .with_data(serde_json::json!({
                "server": PROTOCOL_VERSION,
                "client": params.protocol_version,
            })));
        }
        Ok(HelloResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            server: PeerInfo {
                name: "tetanus".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: self.capabilities(),
        })
    }

    async fn session_create(&self, params: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        self.sessions.create(params)
    }

    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        Ok(SessionListResult {
            sessions: self.sessions.list()?,
        })
    }

    async fn session_events(
        &self,
        params: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError> {
        self.sessions
            .events(&params.session_id, params.from_seq, params.limit)
    }

    async fn session_fork(&self, params: SessionForkParams) -> Result<SessionInfo, RpcError> {
        self.sessions.fork(params)
    }

    async fn session_subscribe(
        &self,
        params: SessionSubscribeParams,
        sink: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        let session = self.sessions.open(&params.session_id)?;
        Ok(self.hub.subscribe(&session, params.from_seq, sink))
    }

    async fn session_unsubscribe(&self, params: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        self.hub.unsubscribe(params)
    }

    async fn agent_prompt(&self, params: AgentPromptParams) -> Result<AgentPromptResult, RpcError> {
        self.runtime.prompt(&self.sessions, &self.hub, params).await
    }

    async fn agent_status(&self, params: SessionRef) -> Result<AgentStatusResult, RpcError> {
        self.runtime.status(&self.sessions, &params.session_id)
    }

    async fn agent_interrupt(&self, params: SessionRef) -> Result<Ack, RpcError> {
        self.runtime.interrupt(&self.sessions, &params.session_id)
    }

    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError> {
        Ok(self.catalogs.tools())
    }

    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError> {
        Ok(self.catalogs.models())
    }

    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError> {
        Ok(self.catalogs.dump())
    }
}
