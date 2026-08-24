//! Test Design Specification: a server's tools in the tetanus registry, and a
//! turn that runs one, ported.
//!
//! Feature under test: `tetanus_mcp::tools` - the naming contract and the
//! bridge - and what a turn does with a bridged tool that answers, one whose
//! server has stopped answering, and one whose server died. Upstream pins the
//! naming and registration halves in
//! `packages/mcp/mcp-client/tests/mcp-client.spec.ts` (`publicToolName`,
//! `syncTools`).
//!
//! Approach: the naming cases are pure and need nothing; the registry cases
//! use in-memory servers; the turn cases spend the real fixture process,
//! because the claim being made is about a program dying and a turn carrying
//! on, and a mock of a dying program would be asserting the mock.
//!
//! What is not restated, and why. Upstream rolls back a whole registration
//! generation when a foreign tool squats on the namespace, and refuses a tool
//! list with a duplicate raw name. A tetanus registry is settled before the
//! engine is built and a server's own list cannot hold one name twice, so
//! there is no generation to roll back; the collision rules that remain are
//! TC-PORT-MCP-30. Its Code-Mode projections and attachment admission have no
//! surface here.
//!
//! Environmental needs: the `tetanus-mcp-fixture` binary and a writable temp
//! directory. No network, no key. The turn cases are unix-only because they
//! read `/proc` to prove no server was left behind.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_mcp::client::ToolDescription;
use tetanus_mcp::tools::{install, public_name, McpTool, MAX_NAME};
use tetanus_mcp::{ClientInfo, Launcher, ReconnectPolicy, Supervisor, Timeouts};
use tetanus_turn::events::StopReason;
use tetanus_turn::tools::{EchoTool, Tool, ToolRegistry};

use harness::{
    eventually, fake_server, fixture, FakeServer, ModelAsking, ScriptedLauncher, TurnFixture,
};

fn described(name: &str) -> ToolDescription {
    ToolDescription {
        raw_name: name.to_string(),
        description: "a tool".to_string(),
        input_schema: json!({ "type": "object", "properties": {} }),
    }
}

async fn supervisor_over(server: &str, tools: Vec<String>) -> (Arc<Supervisor>, FakeServer) {
    let held: Arc<std::sync::Mutex<Option<FakeServer>>> = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&held);
    let launcher = ScriptedLauncher::new(move |_| {
        let (link, server) = fake_server(tools.clone());
        *kept.lock().expect("held") = Some(server);
        Some(link)
    });
    let (supervisor, _) = Supervisor::start(
        server,
        launcher as Arc<dyn Launcher>,
        ReconnectPolicy::default(),
        Timeouts::default(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");
    let server = held.lock().expect("held").take().expect("a server");
    (supervisor, server)
}

/// TC-PORT-MCP-29: a public name is the identity, normalised only when it has
/// to be.
///
/// Upstream: "joins clean names verbatim", "replaces invalid characters and
/// appends an identity hash", "truncates over-long names and appends an
/// identity hash", "is deterministic and collision-free for distinct
/// identities".
///
/// The provider's function-name grammar is `[A-Za-z0-9_-]` up to 64
/// characters. A name that does not fit is not a reason to drop a tool, and
/// two names that normalise onto each other are not a reason to lose one.
///
/// Input: a clean pair, a pair with characters the wire refuses, an over-long
/// pair, two raw names that normalise onto each other, and two identities that
/// the `__` separator makes ambiguous.
/// Expected: the clean pair joins verbatim; the others are within the bound,
/// carry a hash, are stable across calls, and no two of them collide. The last
/// pair is tetanus's addition, argued at [`public_name`]: upstream joins both
/// verbatim and they collide.
#[test]
fn a_public_name_is_the_identity_normalised_only_when_it_has_to_be() {
    assert_eq!(public_name("files", "read_file"), "mcp__files__read_file");

    let dotted = public_name("files", "fs.read");
    assert!(
        dotted.starts_with("mcp__files__fs_read_"),
        "the invalid character is replaced, not dropped: {dotted}"
    );
    assert!(dotted.len() <= MAX_NAME);
    assert_eq!(dotted, public_name("files", "fs.read"), "it is stable");

    let long = public_name("files", &"a".repeat(200));
    assert_eq!(
        long.len(),
        MAX_NAME,
        "an over-long name is cut to the bound"
    );

    // Upstream's own collision case: two raw names with the same normal form.
    assert_ne!(
        public_name("srv", "admin.reset"),
        public_name("srv", "admin_reset"),
        "names that normalise alike keep distinct identities"
    );

    // Two identities whose plain join is the same string.
    assert_ne!(
        public_name("a__b", "c"),
        public_name("a", "b__c"),
        "a server name carrying the separator does not swallow another server's tool"
    );
}

/// TC-PORT-MCP-30: bridged tools sit beside each other and beside a native
/// one.
///
/// Upstream: "lets two servers publish the same raw name side by side",
/// "coexists with a native tool of the same raw name".
///
/// Input: two servers both advertising `echo`, installed into a registry that
/// already holds tetanus's own `echo`.
/// Expected: three tools, each with its own name, and the native one
/// untouched.
#[tokio::test]
async fn bridged_tools_sit_beside_each_other_and_beside_a_native_one() {
    let (first, _first_server) = supervisor_over("alpha", vec!["echo".to_string()]).await;
    let (second, _second_server) = supervisor_over("beta", vec!["echo".to_string()]).await;

    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    let added_first = install(&mut registry, &first, &[described("echo")]);
    let added_second = install(&mut registry, &second, &[described("echo")]);

    assert_eq!(added_first, vec!["mcp__alpha__echo".to_string()]);
    assert_eq!(added_second, vec!["mcp__beta__echo".to_string()]);
    let names: Vec<String> = registry.names().cloned().collect();
    assert_eq!(
        names,
        vec![
            "echo".to_string(),
            "mcp__alpha__echo".to_string(),
            "mcp__beta__echo".to_string(),
        ]
    );

    // The schema the model reads names the server, since two `echo` tools with
    // one description would be a coin toss.
    let bridged = McpTool::new(Arc::clone(&first), &described("echo"));
    assert!(
        bridged.schema().description.contains("alpha"),
        "the description names the server: {}",
        bridged.schema().description
    );
    assert_eq!(bridged.raw_name(), "echo", "the wire name is unchanged");

    first.shutdown().await;
    second.shutdown().await;
}

/// TC-PORT-MCP-31: a real server's tool is called through a real turn.
///
/// Upstream: `mcp-client.e2e.ts`, and its "executes through the real registry"
/// cases.
///
/// This is the acceptance the crate exists for: a tool a program advertised,
/// registered beside tetanus's own, dispatched by the ordinary pipeline, with
/// its answer on the journal and in the model's next prompt.
///
/// Input: the fixture server bridged into a registry that also holds `echo`,
/// and a model that asks for `mcp__fixture__echo`.
/// Expected: the turn ends on `Stop`; the journal holds one successful
/// `tool/result` under the public name carrying the server's text; the model's
/// closing message repeats it; and no server is left behind.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_real_servers_tool_is_called_through_a_real_turn() {
    let command = fixture("serve");
    let (supervisor, tools) = Supervisor::start(
        "fixture",
        Arc::new(command) as Arc<dyn Launcher>,
        ReconnectPolicy::default(),
        Timeouts::default(),
        ClientInfo::default(),
    )
    .await
    .expect("the fixture server connects");

    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    let added = install(&mut registry, &supervisor, &tools);
    assert!(added.contains(&"mcp__fixture__echo".to_string()));

    let turn = TurnFixture::new(
        "mcp-turn",
        registry,
        Arc::new(ModelAsking {
            tool: "mcp__fixture__echo".to_string(),
            arguments: json!({ "text": "through a turn" }),
        }),
    );
    let outcome = turn
        .engine
        .run_turn("use the server")
        .await
        .expect("the turn runs");

    assert_eq!(outcome.reason, StopReason::Natural);
    assert_eq!(
        turn.tool_results(),
        vec![(
            "mcp__fixture__echo".to_string(),
            true,
            "echo: through a turn".to_string()
        )]
    );
    assert!(
        outcome.content.contains("echo: through a turn"),
        "the model read the server's answer: {}",
        outcome.content
    );

    let departure = supervisor.shutdown().await;
    if let Some(pid) = departure.pid {
        assert!(
            !harness::process_exists(pid),
            "the server {pid} outlived the turn"
        );
    }
}

/// TC-PORT-MCP-32: a server that stops answering fails its call and not the
/// turn.
///
/// Upstream: the containment its bridge gives a failed `callTool`, which comes
/// back as an error result rather than as a thrown turn.
///
/// This is the containment promise stated as behaviour: the model asks for a
/// tool whose server will never answer, and the turn goes on to its next step
/// with a failed result it can read.
///
/// Input: the fixture's `hang` tool under a 200ms request budget.
/// Expected: the turn still ends on `Stop`; the one `tool/result` is a failure
/// whose text carries the `[timeout]` class; the model's closing message
/// repeats it; and the server process is gone afterwards.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_server_that_stops_answering_fails_its_call_and_not_the_turn() {
    let timeouts = Timeouts {
        handshake: Duration::from_secs(2),
        request: Duration::from_millis(200),
    };
    let (supervisor, tools) = Supervisor::start(
        "fixture",
        Arc::new(fixture("serve")) as Arc<dyn Launcher>,
        ReconnectPolicy::default(),
        timeouts,
        ClientInfo::default(),
    )
    .await
    .expect("connected");

    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    install(&mut registry, &supervisor, &tools);

    let turn = TurnFixture::new(
        "mcp-hang",
        registry,
        Arc::new(ModelAsking {
            tool: "mcp__fixture__hang".to_string(),
            arguments: json!({}),
        }),
    );
    let outcome = turn
        .engine
        .run_turn("ask the server something it will not answer")
        .await
        .expect("the turn still completes");

    assert_eq!(
        outcome.reason,
        StopReason::Natural,
        "the turn was not ended"
    );
    let results = turn.tool_results();
    assert_eq!(results.len(), 1);
    let (name, ok, content) = &results[0];
    assert_eq!(name, "mcp__fixture__hang");
    assert!(!ok, "the call is recorded as failed");
    assert!(
        content.contains("[timeout]"),
        "the class is in the result the model reads: {content}"
    );

    let departure = supervisor.shutdown().await;
    if let Some(pid) = departure.pid {
        assert!(
            !harness::process_exists(pid),
            "the hung server {pid} was left running"
        );
    }
}

/// TC-PORT-MCP-33: a server that dies mid-turn is replaced, and the next call
/// runs.
///
/// Upstream: "reconnects after a transport close ... and serves calls", here
/// with a real process dying in the middle of a real turn.
///
/// Input: the fixture's `crash` tool, called through a turn, then a second
/// turn calling `echo` after the supervisor has replaced the server.
/// Expected: the first turn completes with a failed result carrying
/// `[transport]`; the supervisor launches a second server; the second turn's
/// call succeeds; and neither process is left behind.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_server_that_dies_mid_turn_is_replaced_and_the_next_call_runs() {
    let policy = ReconnectPolicy {
        enabled: true,
        initial_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(50),
        max_attempts: 5,
    };
    let (supervisor, tools) = Supervisor::start(
        "fixture",
        Arc::new(fixture("serve")) as Arc<dyn Launcher>,
        policy,
        Timeouts::default(),
        ClientInfo::default(),
    )
    .await
    .expect("connected");

    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    install(&mut registry, &supervisor, &tools);
    let registry = Arc::new(registry);

    let crashing = TurnFixture::new(
        "mcp-crash",
        clone_registry(&registry, &supervisor, &tools),
        Arc::new(ModelAsking {
            tool: "mcp__fixture__crash".to_string(),
            arguments: json!({}),
        }),
    );
    let outcome = crashing
        .engine
        .run_turn("kill the server")
        .await
        .expect("the turn completes");
    assert_eq!(outcome.reason, StopReason::Natural);
    let (_, ok, content) = crashing.tool_results().remove(0);
    assert!(!ok);
    assert!(
        content.contains("[transport]"),
        "the class says the server went away: {content}"
    );

    // Up, not merely launched: the launch counter moves when a process is
    // started, and the handshake it is about to do is what makes it usable.
    assert!(
        eventually(Duration::from_secs(5), || supervisor.health()
            == tetanus_mcp::Health::Up
            && supervisor.launches() == 2)
        .await,
        "the supervisor did not replace the dead server: {:?}",
        supervisor.health()
    );

    let recovered = TurnFixture::new(
        "mcp-recovered",
        clone_registry(&registry, &supervisor, &tools),
        Arc::new(ModelAsking {
            tool: "mcp__fixture__echo".to_string(),
            arguments: json!({ "text": "after the crash" }),
        }),
    );
    recovered
        .engine
        .run_turn("try again")
        .await
        .expect("the turn completes");
    assert_eq!(
        recovered.tool_results(),
        vec![(
            "mcp__fixture__echo".to_string(),
            true,
            "echo: after the crash".to_string()
        )]
    );

    let departure = supervisor.shutdown().await;
    if let Some(pid) = departure.pid {
        assert!(
            !harness::process_exists(pid),
            "the server {pid} is still up"
        );
    }
}

/// A second registry over the same supervisor. `ToolRegistry` is not
/// cloneable and a turn engine takes ownership of one, so a case that runs two
/// turns builds the same set twice - the tools are the same objects behind
/// them, which is the point.
#[cfg(target_os = "linux")]
fn clone_registry(
    _first: &Arc<ToolRegistry>,
    supervisor: &Arc<Supervisor>,
    tools: &[ToolDescription],
) -> ToolRegistry {
    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    install(&mut registry, supervisor, tools);
    registry
}
