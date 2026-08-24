//! Test Design Specification: keeping an MCP server up, and knowing when to
//! stop trying, ported.
//!
//! Feature under test: `tetanus_mcp::supervisor` - the reconnect policy, the
//! attempt budget and what resets it, the states a call is refused in, and
//! shutdown winning a race against a pending backoff. Upstream pins the same
//! behaviour in `packages/mcp/mcp-client/tests/reconnect.spec.ts`.
//!
//! Approach: a scripted launcher over in-memory links. Reconnect is a state
//! machine driven by a clock, and every case here is about which state it
//! reaches after which event: spending a process per attempt would add nothing
//! but seconds. The delays are milliseconds, and no case asserts on a
//! duration - only on what happened and how often, which is what makes them
//! insensitive to a slow machine.
//!
//! What is not restated, and why. Upstream's generation bookkeeping - a
//! disposer that unregisters the current generation, a stale notification
//! handler from a replaced one, a re-sync racing disposal - is about a
//! registry it mutates at run time. A tetanus registry is settled before the
//! engine is built, so a bridged tool resolves the live connection at call
//! time and there is no generation to publish or roll back; what a reconnect
//! can change is whether a tool is still advertised, which is TC-PORT-MCP-28.
//! Its Cordis load-path and HMR cases have no counterpart.
//!
//! Environmental needs: none. No process, no network, no key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_mcp::fault::class;
use tetanus_mcp::supervisor::{Health, PolicyError, MAX_DELAY};
use tetanus_mcp::{ClientInfo, ReconnectPolicy, Supervisor, Timeouts};

use harness::{eventually, fake_server, FakeServer, ScriptedLauncher};

/// Brisk but not instant: a backoff of a few milliseconds keeps the cases
/// under a second while still exercising the wait.
fn brisk_policy(max_attempts: u32) -> ReconnectPolicy {
    ReconnectPolicy {
        enabled: true,
        initial_delay: Duration::from_millis(2),
        max_delay: Duration::from_millis(20),
        max_attempts,
    }
}

fn timeouts() -> Timeouts {
    Timeouts {
        handshake: Duration::from_secs(2),
        request: Duration::from_secs(2),
    }
}

/// TC-PORT-MCP-21: a lost connection is replaced, and calls run on the
/// replacement.
///
/// Upstream: "reconnects after a transport close, re-syncs tools through the
/// new generation, and serves calls".
///
/// Input: a server that hangs up after one call, and a launcher with a second
/// server behind it.
/// Expected: the supervisor launches twice, comes back to `Up`, and the tool
/// call after the outage is answered by the new connection.
#[tokio::test]
async fn a_lost_connection_is_replaced_and_calls_run_on_the_replacement() {
    let servers: Arc<std::sync::Mutex<Vec<FakeServer>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = Arc::clone(&servers);
    let launcher = ScriptedLauncher::new(move |_attempt| {
        let (link, server) = fake_server(vec!["ping".to_string()]);
        kept.lock().expect("servers").push(server);
        Some(link)
    });

    let (supervisor, tools) = Supervisor::start(
        "fake",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        brisk_policy(5),
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("the first connection is made");
    assert_eq!(tools.len(), 1);
    assert_eq!(supervisor.launches(), 1);

    // The first server goes away under the supervisor's feet.
    servers.lock().expect("servers")[0].hang_up();

    assert!(
        eventually(Duration::from_secs(5), || supervisor.health() == Health::Up
            && supervisor.launches() == 2)
        .await,
        "the supervisor did not reconnect: {:?}",
        supervisor.health()
    );
    let answer = supervisor
        .call_tool("ping", &json!({}))
        .await
        .expect("the replacement serves the call");
    assert_eq!(answer.text, "ran ping");
    supervisor.shutdown().await;
}

/// TC-PORT-MCP-22: the attempt cap is a floor under the retrying.
///
/// Upstream: "stops at the failure cap, unregisters the tools, and reports
/// final failure".
///
/// A supervisor that retried for ever would hold every tool call of every
/// later turn behind a server that is never coming back.
///
/// Input: a first connection that succeeds and hangs up, then a launcher that
/// fails every attempt, with a cap of three.
/// Expected: exactly three further launches, `GaveUp` naming the count and the
/// last failure, and a call refused as `unavailable` rather than waiting.
#[tokio::test]
async fn the_attempt_cap_is_a_floor_under_the_retrying() {
    let first: Arc<std::sync::Mutex<Option<FakeServer>>> = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&first);
    let launcher = ScriptedLauncher::new(move |attempt| {
        if attempt > 1 {
            return None;
        }
        let (link, server) = fake_server(vec!["ping".to_string()]);
        *kept.lock().expect("first") = Some(server);
        Some(link)
    });

    let (supervisor, _) = Supervisor::start(
        "fake",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        brisk_policy(3),
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");
    first
        .lock()
        .expect("first")
        .as_mut()
        .expect("a server")
        .hang_up();

    assert!(
        eventually(Duration::from_secs(5), || matches!(
            supervisor.health(),
            Health::GaveUp(_)
        ))
        .await,
        "the supervisor kept trying: {:?}",
        supervisor.health()
    );
    assert_eq!(
        supervisor.launches(),
        4,
        "one connection plus the three attempts the cap allows"
    );
    let Health::GaveUp(why) = supervisor.health() else {
        panic!("expected GaveUp");
    };
    assert!(why.contains("3 attempt"), "the count is named: {why}");

    let fault = supervisor
        .call_tool("ping", &json!({}))
        .await
        .expect_err("there is nothing to call");
    assert_eq!(fault.class(), class::UNAVAILABLE);
    assert!(
        fault.to_string().contains("gave up"),
        "the refusal says the supervisor stopped trying: {fault}"
    );
    supervisor.shutdown().await;
}

/// TC-PORT-MCP-23: a crash loop exhausts the cap although every connect
/// succeeds.
///
/// Upstream: "a crash loop with briefly successful connects still exhausts the
/// cap".
///
/// A budget reset by a successful connect would never end: a server that
/// connects and dies immediately connects successfully every time.
///
/// Input: a launcher whose servers all hang up as soon as they are connected,
/// with a cap of two and a stability window of 20ms.
/// Expected: three launches in all - the first connection plus the two the cap
/// allows - and `GaveUp`.
#[tokio::test]
async fn a_crash_loop_exhausts_the_cap_although_every_connect_succeeds() {
    let launcher = ScriptedLauncher::new(move |_attempt| {
        let (link, mut server) = fake_server(vec!["ping".to_string()]);
        // It answers the handshake, then goes away at once.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            server.hang_up();
        });
        Some(link)
    });

    let (supervisor, _) = Supervisor::start(
        "flapping",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        brisk_policy(2),
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("the first connection is made");

    assert!(
        eventually(Duration::from_secs(5), || matches!(
            supervisor.health(),
            Health::GaveUp(_)
        ))
        .await,
        "the crash loop was not stopped: {:?}",
        supervisor.health()
    );
    assert_eq!(supervisor.launches(), 3);
    supervisor.shutdown().await;
}

/// TC-PORT-MCP-24: an uptime past the stability window buys a fresh budget.
///
/// Upstream: "an uptime past the stability window resets the attempt budget".
///
/// Without it, a harness running for a week would exhaust its cap on a server
/// that restarted ten times over that week - each recovery perfect, each
/// counted against the same budget.
///
/// Input: a cap of one, a 20ms window, and servers that stay up for 60ms
/// before hanging up.
/// Expected: two outages are both recovered, so the third connection is live
/// and three launches happened - which a budget of one could not have paid
/// for without the reset.
#[tokio::test]
async fn an_uptime_past_the_stability_window_buys_a_fresh_budget() {
    // The third server has to be held somewhere, or dropping its handle would
    // hang it up and the case would be measuring a fourth outage.
    let standing: Arc<std::sync::Mutex<Vec<FakeServer>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = Arc::clone(&standing);
    let launcher = ScriptedLauncher::new(move |attempt| {
        let (link, mut server) = fake_server(vec!["ping".to_string()]);
        if attempt < 3 {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                server.hang_up();
            });
        } else {
            kept.lock().expect("standing").push(server);
        }
        Some(link)
    });

    let (supervisor, _) = Supervisor::start(
        "steady",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        brisk_policy(1),
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");

    assert!(
        eventually(Duration::from_secs(5), || supervisor.launches() == 3
            && supervisor.health() == Health::Up)
        .await,
        "the budget was not reset by uptime: {:?} after {} launches",
        supervisor.health(),
        supervisor.launches()
    );
    supervisor.shutdown().await;
}

/// TC-PORT-MCP-25: with reconnecting off, one outage is the end of it.
///
/// Upstream: "reconnect disabled keeps the registered tools and reports manual
/// recovery".
///
/// tetanus differs in what survives, and says so: upstream keeps the tools
/// registered because its registry is mutable and the operator may restore the
/// server by hand. Here the tools stay in the registry too - it is settled at
/// boot - but every call to them is refused with a reason naming the manual
/// recovery, which is the same promise reached the other way round.
///
/// Input: a policy with `enabled: false`, and a server that hangs up.
/// Expected: no second launch, `GaveUp` naming the restart, and a refused call.
#[tokio::test]
async fn with_reconnecting_off_one_outage_is_the_end_of_it() {
    let held: Arc<std::sync::Mutex<Option<FakeServer>>> = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&held);
    let launcher = ScriptedLauncher::new(move |_attempt| {
        let (link, server) = fake_server(vec!["ping".to_string()]);
        *kept.lock().expect("held") = Some(server);
        Some(link)
    });
    let policy = ReconnectPolicy {
        enabled: false,
        ..brisk_policy(5)
    };

    let (supervisor, _) = Supervisor::start(
        "once",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        policy,
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");
    held.lock()
        .expect("held")
        .as_mut()
        .expect("a server")
        .hang_up();

    assert!(
        eventually(Duration::from_secs(5), || matches!(
            supervisor.health(),
            Health::GaveUp(_)
        ))
        .await,
        "the outage was not final: {:?}",
        supervisor.health()
    );
    assert_eq!(supervisor.launches(), 1, "nothing was relaunched");
    let fault = supervisor
        .call_tool("ping", &json!({}))
        .await
        .expect_err("refused");
    assert!(
        fault.to_string().contains("restart"),
        "the refusal says how to recover: {fault}"
    );
}

/// TC-PORT-MCP-26: shutting down cancels a backoff instead of waiting it out.
///
/// Upstream: "dispose during the backoff wait cancels the pending reconnect",
/// "a transport close after dispose schedules nothing".
///
/// A harness stopping must not be held up by a wait on a server that is
/// already gone.
///
/// Input: a five-second first delay, a launcher that fails every attempt after
/// the first, and a shutdown while the wait is in flight.
/// Expected: the shutdown returns well inside the delay, the health is
/// `Stopped`, and no further launch is made.
#[tokio::test]
async fn shutting_down_cancels_a_backoff_instead_of_waiting_it_out() {
    let held: Arc<std::sync::Mutex<Option<FakeServer>>> = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&held);
    let launcher = ScriptedLauncher::new(move |attempt| {
        if attempt > 1 {
            return None;
        }
        let (link, server) = fake_server(vec!["ping".to_string()]);
        *kept.lock().expect("held") = Some(server);
        Some(link)
    });
    let policy = ReconnectPolicy {
        enabled: true,
        initial_delay: Duration::from_secs(5),
        max_delay: Duration::from_secs(30),
        max_attempts: 10,
    };

    let (supervisor, _) = Supervisor::start(
        "patient",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        policy,
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");
    held.lock()
        .expect("held")
        .as_mut()
        .expect("a server")
        .hang_up();
    assert!(
        eventually(Duration::from_secs(2), || matches!(
            supervisor.health(),
            Health::Reconnecting(_)
        ))
        .await,
        "the outage was not noticed: {:?}",
        supervisor.health()
    );

    let started = std::time::Instant::now();
    supervisor.shutdown().await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the shutdown waited out the backoff: {:?}",
        started.elapsed()
    );
    assert_eq!(supervisor.health(), Health::Stopped);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        launcher.launches(),
        1,
        "a server was started after shutdown"
    );
}

/// TC-PORT-MCP-27: a policy that cannot be run is refused where it is written.
///
/// Upstream: "rejects out-of-range delays", "rejects an initial delay above
/// the ceiling", "rejects non-positive-integer attempt caps", "apply fails
/// loud at load on a misconfigured reconnect".
///
/// Input: each way of writing a policy that makes no sense.
/// Expected: the matching [`PolicyError`], and the defaults resolving cleanly.
#[test]
fn a_policy_that_cannot_be_run_is_refused_where_it_is_written() {
    let base = ReconnectPolicy::default();
    assert!(base.resolve().is_ok(), "the defaults are runnable");

    assert_eq!(
        ReconnectPolicy {
            initial_delay: Duration::ZERO,
            ..base
        }
        .resolve()
        .expect_err("a delay of no time is not a delay"),
        PolicyError::Delay {
            field: "initial_delay",
            given: Duration::ZERO
        }
    );
    assert_eq!(
        ReconnectPolicy {
            max_delay: MAX_DELAY + Duration::from_secs(1),
            ..base
        }
        .resolve()
        .expect_err("a wait nobody will see is a mistake"),
        PolicyError::Delay {
            field: "max_delay",
            given: MAX_DELAY + Duration::from_secs(1)
        }
    );
    assert_eq!(
        ReconnectPolicy {
            initial_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(5),
            ..base
        }
        .resolve()
        .expect_err("a first wait past the ceiling"),
        PolicyError::Order
    );
    assert_eq!(
        ReconnectPolicy {
            max_attempts: 0,
            ..base
        }
        .resolve()
        .expect_err("a cap of no attempts never reconnects"),
        PolicyError::Attempts
    );
}

/// TC-PORT-MCP-28: a tool the live server no longer advertises is refused as
/// unknown, not sent.
///
/// Upstream: "unregisters previous tools before re-syncing" - the same fact
/// about a new generation, reached through a registry that cannot be rewritten
/// mid-run.
///
/// A call sent to a server that has never heard of the tool comes back as
/// whatever that server's error handling happens to say. Refusing it here
/// gives the model one sentence with a class on it instead.
///
/// Input: a first server advertising `ping` and `pong`, then, after an outage,
/// one advertising only `ping`.
/// Expected: `ping` still runs; `pong` is refused with class `unknown-tool`.
#[tokio::test]
async fn a_tool_the_live_server_no_longer_advertises_is_refused_as_unknown() {
    let held: Arc<std::sync::Mutex<Option<FakeServer>>> = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&held);
    let launcher = ScriptedLauncher::new(move |attempt| {
        let tools = if attempt == 1 {
            vec!["ping".to_string(), "pong".to_string()]
        } else {
            vec!["ping".to_string()]
        };
        let (link, server) = fake_server(tools);
        *kept.lock().expect("held") = Some(server);
        Some(link)
    });

    let (supervisor, tools) = Supervisor::start(
        "shrinking",
        Arc::clone(&launcher) as Arc<dyn tetanus_mcp::Launcher>,
        brisk_policy(5),
        timeouts(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");
    assert_eq!(tools.len(), 2);

    held.lock()
        .expect("held")
        .as_mut()
        .expect("a server")
        .hang_up();
    assert!(
        eventually(Duration::from_secs(5), || supervisor.launches() == 2
            && supervisor.health() == Health::Up)
        .await,
        "the supervisor did not reconnect: {:?}",
        supervisor.health()
    );

    assert_eq!(
        supervisor
            .call_tool("ping", &json!({}))
            .await
            .expect("still advertised")
            .text,
        "ran ping"
    );
    let fault = supervisor
        .call_tool("pong", &json!({}))
        .await
        .expect_err("no longer advertised");
    assert_eq!(fault.class(), class::UNKNOWN_TOOL);
    supervisor.shutdown().await;
}
