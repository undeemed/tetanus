//! Keeping one MCP server up, for as long as that is a sensible thing to do.
//!
//! A server this process started can die at any moment - it is a program with
//! its own bugs, and the pipe it speaks on is the only thing holding it. The
//! supervisor owns what happens next, and every part of that is bounded and
//! stated:
//!
//! **One outage, one budget.** Delays double from
//! [`ReconnectPolicy::initial_delay`] to [`ReconnectPolicy::max_delay`], and
//! after [`ReconnectPolicy::max_attempts`] consecutive failures the supervisor
//! gives up for good. Calls then fail with [`McpFault::Unavailable`] naming
//! what was tried, rather than queueing behind a reconnect that is never
//! coming.
//!
//! **A connection that stays up earns a fresh budget, and a crash loop does
//! not.** The budget resets only when the last connection lived longer than
//! the backoff ceiling, so a server that connects and dies four times a second
//! still exhausts its cap - upstream reaches the same place with the same
//! stability window, and the reason is that "it connected" is not evidence a
//! restart fixed anything.
//!
//! **The tool set is re-read on every connect, and the registry is not
//! rebuilt.** tetanus's tool registry is settled before the engine is built,
//! so a server's tools are published once, at boot. What a reconnect can
//! change is whether a tool is still *there*, and a call for one that is gone
//! fails as [`McpFault::UnknownTool`] rather than being sent to a server that
//! never heard of it. Upstream re-registers a whole generation instead,
//! because its registry is mutable at run time; the difference is a
//! `docs/parity.md` row rather than a silent divergence.
//!
//! **Stopping wins every race.** [`Supervisor::shutdown`] cancels a pending
//! backoff instead of waiting it out, and nothing is launched afterwards.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Notify;

use crate::client::{ClientInfo, McpClient, Timeouts, ToolAnswer, ToolDescription};
use crate::fault::McpFault;
use crate::link::{Departure, Link};
use crate::stdio::ServerCommand;
use crate::wire::method;

/// The longest delay a policy may name. A wait nobody will live to see is a
/// configuration mistake, not a patient policy.
pub const MAX_DELAY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("{field} must be a positive delay no longer than {}s, not {}ms", MAX_DELAY.as_secs(), given.as_millis())]
    Delay {
        field: &'static str,
        given: Duration,
    },
    #[error("initial_delay must be no longer than max_delay")]
    Order,
    #[error("max_attempts must be at least one attempt")]
    Attempts,
}

/// What to do when the connection to a server is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Whether to reconnect at all. Off means one lost connection is the end
    /// of that server for this run, and the operator restarts it.
    pub enabled: bool,
    /// The first wait. Doubles per consecutive failed attempt.
    pub initial_delay: Duration,
    /// The backoff ceiling, and the uptime past which the attempt budget
    /// resets.
    pub max_delay: Duration,
    /// Consecutive failed attempts in one outage before giving up.
    pub max_attempts: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        // Upstream's defaults, for the reason upstream's are what they are: a
        // server that is restarting takes about half a second, and ten
        // attempts across a doubling backoff is about five minutes of trying.
        Self {
            enabled: true,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_attempts: 10,
        }
    }
}

impl ReconnectPolicy {
    /// Read a policy, refusing one that cannot be run.
    ///
    /// Every bound is judged here rather than where it is used, so a
    /// misconfigured server fails at load - which is the difference between an
    /// operator seeing a message and an operator seeing a harness that never
    /// reconnects for a reason nobody logged.
    pub fn resolve(self) -> Result<Self, PolicyError> {
        for (field, given) in [
            ("initial_delay", self.initial_delay),
            ("max_delay", self.max_delay),
        ] {
            if given.is_zero() || given > MAX_DELAY {
                return Err(PolicyError::Delay { field, given });
            }
        }
        if self.initial_delay > self.max_delay {
            return Err(PolicyError::Order);
        }
        if self.max_attempts == 0 {
            return Err(PolicyError::Attempts);
        }
        Ok(self)
    }
}

/// How a server is started. A [`ServerCommand`] is the one that spawns a
/// process; a test hands over links it made itself.
#[async_trait::async_trait]
pub trait Launcher: Send + Sync + 'static {
    async fn launch(&self) -> Result<Link, McpFault>;
}

#[async_trait::async_trait]
impl Launcher for ServerCommand {
    async fn launch(&self) -> Result<Link, McpFault> {
        self.spawn().map_err(|source| {
            McpFault::Transport(format!("{} could not be started: {source}", self.program))
        })
    }
}

/// What the supervisor currently has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// A live connection, serving calls.
    Up,
    /// No connection, and the supervisor is trying to get one back.
    Reconnecting(String),
    /// No connection, and no more attempts will be made.
    GaveUp(String),
    /// [`Supervisor::shutdown`] was called.
    Stopped,
}

struct State {
    client: Option<Arc<McpClient>>,
    /// The raw names the *live* server advertises, re-read on every connect.
    tools: BTreeSet<String>,
    health: Health,
}

/// One supervised MCP server.
pub struct Supervisor {
    server: String,
    launcher: Arc<dyn Launcher>,
    policy: ReconnectPolicy,
    timeouts: Timeouts,
    info: ClientInfo,
    state: Mutex<State>,
    stopped: AtomicBool,
    /// Woken by [`Supervisor::shutdown`], so a backoff wait ends at once
    /// rather than being waited out.
    stopping: Notify,
    /// How many times a server was launched, live and reconnects together.
    launches: Mutex<u32>,
}

impl Supervisor {
    /// Connect, discover, and start watching.
    ///
    /// The initial connection is the caller's to handle: a server that is not
    /// there at boot is a configuration question, not an outage, and a
    /// deployment may want either to fail or to carry on without it.
    pub async fn start(
        server: impl Into<String>,
        launcher: Arc<dyn Launcher>,
        policy: ReconnectPolicy,
        timeouts: Timeouts,
        info: ClientInfo,
    ) -> Result<(Arc<Self>, Vec<ToolDescription>), McpFault> {
        let policy = policy.resolve().map_err(|why| {
            McpFault::Unavailable(format!("the reconnect policy cannot be run: {why}"))
        })?;
        let supervisor = Arc::new(Self {
            server: server.into(),
            launcher,
            policy,
            timeouts,
            info,
            state: Mutex::new(State {
                client: None,
                tools: BTreeSet::new(),
                health: Health::Reconnecting("the first connection has not been made".into()),
            }),
            stopped: AtomicBool::new(false),
            stopping: Notify::new(),
            launches: Mutex::new(0),
        });

        let client = supervisor.connect_once().await?;
        let tools = client.list_tools().await?;
        supervisor.adopt(Arc::clone(&client), &tools);

        let watching = Arc::clone(&supervisor);
        tokio::spawn(async move { watching.watch(client).await });
        Ok((supervisor, tools))
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn policy(&self) -> ReconnectPolicy {
        self.policy
    }

    /// What this supervisor has right now.
    pub fn health(&self) -> Health {
        self.state.lock().expect("state").health.clone()
    }

    /// How many times a server process was started for this supervisor. The
    /// first connection counts, so a run that never reconnected reads one.
    pub fn launches(&self) -> u32 {
        *self.launches.lock().expect("launches")
    }

    /// Call a tool on whatever connection is live now.
    ///
    /// Three refusals, told apart on purpose: there is no connection
    /// ([`McpFault::Unavailable`]), there is one and it does not advertise
    /// this tool ([`McpFault::UnknownTool`]), or the call itself failed.
    pub async fn call_tool(
        &self,
        raw_name: &str,
        arguments: &Value,
    ) -> Result<ToolAnswer, McpFault> {
        let client = {
            let state = self.state.lock().expect("state");
            match (&state.client, &state.health) {
                (Some(client), Health::Up) => {
                    if !state.tools.contains(raw_name) {
                        return Err(McpFault::UnknownTool(raw_name.to_string()));
                    }
                    Arc::clone(client)
                }
                (_, health) => {
                    return Err(McpFault::Unavailable(unavailable(&self.server, health)));
                }
            }
        };
        client.call_tool(raw_name, arguments).await
    }

    /// Stop watching, stop the server, and answer how it went.
    ///
    /// Cancels a backoff that is waiting rather than joining it, so shutting
    /// down a harness is never held up by a server that is already gone.
    pub async fn shutdown(&self) -> Departure {
        self.stopped.store(true, Ordering::Release);
        self.stopping.notify_waiters();
        let client = {
            let mut state = self.state.lock().expect("state");
            state.health = Health::Stopped;
            state.tools.clear();
            state.client.take()
        };
        match client {
            Some(client) => client.close().await,
            None => Departure::closed(),
        }
    }

    /// One launch and one handshake.
    async fn connect_once(&self) -> Result<Arc<McpClient>, McpFault> {
        *self.launches.lock().expect("launches") += 1;
        let link = self.launcher.launch().await?;
        let client =
            McpClient::connect(self.server.clone(), link, self.timeouts, self.info.clone()).await?;
        Ok(Arc::new(client))
    }

    /// Take a fresh connection as the live one, with the tools it advertises.
    fn adopt(&self, client: Arc<McpClient>, tools: &[ToolDescription]) {
        let mut state = self.state.lock().expect("state");
        state.tools = tools
            .iter()
            .map(|tool| tool.raw_name.clone())
            .collect::<BTreeSet<String>>();
        state.client = Some(client);
        state.health = Health::Up;
    }

    fn fell_down(&self, why: String) {
        let mut state = self.state.lock().expect("state");
        if state.health == Health::Stopped {
            return;
        }
        state.client = None;
        state.tools.clear();
        state.health = Health::Reconnecting(why);
    }

    fn gave_up(&self, why: String) {
        let mut state = self.state.lock().expect("state");
        if state.health == Health::Stopped {
            return;
        }
        state.client = None;
        state.tools.clear();
        state.health = Health::GaveUp(why);
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Watch one connection, and every connection that replaces it.
    async fn watch(self: Arc<Self>, initial: Arc<McpClient>) {
        let mut client = initial;
        let mut budget = self.policy.max_attempts;

        loop {
            let up_since = Instant::now();
            self.serve(&client).await;
            if self.is_stopped() {
                return;
            }
            if !self.policy.enabled {
                self.gave_up(format!(
                    "{}: the connection was lost and reconnecting is off; restart the harness to \
                     get its tools back",
                    self.server
                ));
                return;
            }
            // Only real uptime buys a fresh budget: a server that connects and
            // dies immediately is not recovering, however often it manages the
            // first half.
            if up_since.elapsed() >= self.policy.max_delay {
                budget = self.policy.max_attempts;
            }
            self.fell_down(format!("{}: the connection was lost", self.server));

            match self.reconnect(&mut budget).await {
                Some(fresh) => client = fresh,
                None => return,
            }
        }
    }

    /// Serve one live connection until it ends, refreshing the tool set
    /// whenever the server says it changed.
    async fn serve(&self, client: &Arc<McpClient>) {
        let mut notices = client.connection().notices();
        loop {
            tokio::select! {
                () = client.connection().departed() => return,
                () = self.stopping.notified() => return,
                notice = notices.recv() => match notice {
                    Ok(notice) if notice.method == method::TOOL_LIST_CHANGED => {
                        // A re-list can fail because the server is already on
                        // its way out; the departure branch above is what
                        // reports that, so this only keeps the last good set.
                        if let Ok(tools) = client.list_tools().await {
                            let mut state = self.state.lock().expect("state");
                            if state.health == Health::Up {
                                state.tools =
                                    tools.iter().map(|tool| tool.raw_name.clone()).collect();
                            }
                        }
                    }
                    Ok(_) => {}
                    // A subscriber that fell behind has missed notices, not
                    // the connection: re-reading the tool set is the answer to
                    // both, and the loop continues either way.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    }

    /// Spend the budget trying to get a connection back.
    async fn reconnect(&self, budget: &mut u32) -> Option<Arc<McpClient>> {
        let mut delay = self.policy.initial_delay;
        let mut attempts = 0u32;
        let mut last: Option<McpFault> = None;

        while *budget > 0 {
            *budget -= 1;
            attempts += 1;
            if !self.wait(delay).await {
                return None;
            }
            match self.connect_once().await {
                Ok(client) => match client.list_tools().await {
                    Ok(tools) => {
                        self.adopt(Arc::clone(&client), &tools);
                        return Some(client);
                    }
                    Err(fault) => {
                        // A server that connects and will not say what it
                        // serves is no more useful than one that is down, and
                        // leaving it running would leak a process per attempt.
                        client.close().await;
                        last = Some(fault);
                    }
                },
                Err(fault) => last = Some(fault),
            }
            delay = (delay * 2).min(self.policy.max_delay);
        }

        let reason = last.map_or_else(
            || "no attempt was made".to_string(),
            |fault| format!("[{}] {fault}", fault.class()),
        );
        self.gave_up(format!(
            "{}: gave up after {attempts} attempt(s) to reconnect; last failure: {reason}",
            self.server
        ));
        None
    }

    /// Wait out a backoff, unless shutdown arrives first. `false` means it did.
    async fn wait(&self, delay: Duration) -> bool {
        if self.is_stopped() {
            return false;
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => !self.is_stopped(),
            () = self.stopping.notified() => false,
        }
    }
}

fn unavailable(server: &str, health: &Health) -> String {
    match health {
        Health::Up => format!("{server}: the connection went away while the call was being made"),
        Health::Reconnecting(why) => format!("{why}; a reconnect is in progress"),
        Health::GaveUp(why) => why.clone(),
        Health::Stopped => format!("{server}: this server was shut down"),
    }
}
