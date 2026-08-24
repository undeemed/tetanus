//! Test Design Specification: an ACP client driving a real agent process.
//!
//! Feature under test: `tetanus_acp::client` - the consumer half of the
//! protocol. This is upstream's subagent-ACP driver, the client its own bridge
//! README names as its primary consumer, restated against the tetanus bridge.
//!
//! Approach: **every case here spawns a second process.** The agent is this
//! test binary re-entered with an environment variable set, which runs the real
//! `tetanus_acp::serve` over its own stdin and stdout against a real
//! `HarnessEngine` on the offline mock adapter. That is the same self-re-entry
//! `crates/ui/tests/killed.rs` uses, and it is the point of the file: a codec
//! answering frames a suite wrote is a weaker claim than a process that spawns
//! another process, negotiates with it, prompts it, reads what comes back and
//! reaps it. Frames cross an operating-system pipe here, not a `duplex`.
//!
//! `crates/acp/tests/upstream_acp.rs` covers the bridge's own behaviour against
//! an in-process double and is not restated. What is asserted here is only what
//! needs two processes to be true: the negotiation, the turn, the permission
//! answer that stops the agent waiting, and the teardown.
//!
//! Every wait has a deadline, in the client and in the cases. A child that
//! stops answering is the failure this file exists to catch, and it is
//! indistinguishable from a slow model unless something is counting.
//!
//! Environmental needs: a unix-or-windows host that can spawn a process, and a
//! writable temp directory. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, a panic, or any wait exceeding its
//! deadline.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tetanus_acp::wire::{SessionUpdate, ToolCallStatus};
use tetanus_acp::{AcpClient, ClientError, ContentBlock, Launch, PermissionPolicy, StopReason};
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::Engine;

/// Set on the child, and only on the child.
const CHILD: &str = "TETANUS_ACP_AGENT_CHILD";
/// Where the child puts the journals it writes.
const CHILD_DIR: &str = "TETANUS_ACP_AGENT_DIR";

/// Bound on any one case. Generous for a loaded box, finite because the thing
/// it bounds would otherwise be forever - and short enough that a whole suite
/// of hangs is minutes rather than an hour.
const DEADLINE: Duration = Duration::from_secs(30);

/// The agent half of every case below: a real process serving ACP on its own
/// stdin and stdout.
///
/// A `#[test]` because that is the only entry point a test binary has. Run by
/// the suite it returns at once; run as a child, with [`CHILD`] set, it serves
/// until its stdin reaches end of file.
#[test]
fn agent_serves_acp_on_stdio() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let engine: Arc<dyn Engine> = Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: PathBuf::from(dir),
        ..EngineConfig::default()
    }));
    // Multi-threaded for the reason `tetanus serve` is: the carrier's
    // properties are concurrency properties, and a current-thread runtime
    // serves frames one at a time and quietly loses them.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime
        .block_on(async {
            use tokio::io::AsyncWriteExt;
            let mut out = tokio::io::stdout();
            // Terminate whatever the harness left dangling on this stream
            // before the protocol takes it over. Under one test thread libtest
            // writes `test <name> ... ` with no trailing newline before running
            // the case, and this process then writes its first frame onto the
            // end of that line - so the client reads one line that is a header
            // followed by JSON, fails to parse it, and drops the frame. The
            // first thing written here is therefore a newline, which closes the
            // header's line and leaves every frame after it alone on its own.
            //
            // The client stays strict rather than learning to skip a prefix: a
            // reader that tolerates leading junk cannot tell a test harness
            // from a corrupted stream, and this is a property of being hosted
            // inside libtest, not of the protocol.
            out.write_all(b"\n").await.expect("clear the line");
            out.flush().await.expect("flush");
            tetanus_acp::serve(engine, tokio::io::stdin(), out).await
        })
        .expect("served");
}

/// Spawn the agent as a child process and connect a client to it.
async fn connect(dir: &TempDir, policy: PermissionPolicy) -> AcpClient {
    let launch = Launch::new(std::env::current_exe().expect("this test binary"))
        .arg("--exact")
        .arg("agent_serves_acp_on_stdio")
        .arg("--nocapture")
        .env(CHILD, "1")
        .env(CHILD_DIR, dir.path().join("sessions").to_string_lossy());
    AcpClient::spawn(launch, policy)
        .await
        .expect("spawn the agent")
        .with_timeout(DEADLINE)
}

/// TC-PORT-ACP-17: a client spawns an agent, negotiates, and is told what it
/// is talking to.
///
/// Input: a spawned agent process, and `initialize` over its stdin.
/// Expected: the agent's own protocol version and name come back across a real
/// pipe, and no prompt capability is advertised. Until this passed, nothing had
/// ever spoken to the bridge as a separate process.
#[tokio::test]
async fn a_client_spawns_an_agent_and_negotiates_with_it() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;

    let hello = client.initialize().await.expect("initialize");

    assert_eq!(hello.protocol_version, tetanus_acp::PROTOCOL_VERSION);
    assert_eq!(hello.agent_info.name, "tetanus-acp");
    assert!(!hello.agent_capabilities.prompt_capabilities.image);
    assert!(hello.auth_methods.is_empty());

    client.close().await.expect("close");
}

/// TC-PORT-ACP-18: a client completes a whole turn against a separate process,
/// including a tool call and its result.
///
/// Input: initialize, `session/new`, then one prompt, all over pipes.
/// Expected: the turn ends `end_turn`; the client collected the assistant's two
/// committed messages and the `echo` tool call completing with its output. This
/// is the end-to-end claim: two processes, one protocol, a real turn.
#[tokio::test]
async fn a_client_completes_a_whole_turn_against_a_separate_process() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");
    let session = client.new_session(dir.path()).await.expect("session/new");
    assert!(!session.is_empty(), "the agent minted a session id");

    let outcome = client
        .prompt(&session, vec![ContentBlock::text("hello over a pipe")])
        .await
        .expect("prompt");

    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert_eq!(
        outcome.messages(),
        vec!["Let me echo that back.", "You said: hello over a pipe"],
        "committed messages only, in order",
    );

    let calls = outcome.tool_calls();
    assert_eq!(calls.len(), 1, "the mock turn calls one tool");
    assert_eq!(calls[0].1, "echo");

    let completed = outcome
        .updates
        .iter()
        .find_map(|update| match update {
            SessionUpdate::ToolCallUpdate {
                tool_call_id,
                status,
                content,
            } => Some((tool_call_id.clone(), *status, content.clone())),
            _ => None,
        })
        .expect("the call was answered");
    assert_eq!(completed.0, calls[0].0, "the result names its call");
    assert_eq!(completed.1, ToolCallStatus::Completed);
    assert!(!completed.2.is_empty(), "the tool's output crossed too");

    client.close().await.expect("close");
}

/// TC-PORT-ACP-19: two sessions on one connection stay apart.
///
/// Input: two sessions from one agent, prompted in turn.
/// Expected: each prompt's updates carry only its own session's work, and the
/// two answers differ. The client filters by session id rather than handing
/// back whatever arrived, which is the difference between one connection
/// serving two sessions and one connection corrupting both.
#[tokio::test]
async fn two_sessions_on_one_connection_do_not_mix() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");

    let first = client.new_session(dir.path()).await.expect("first");
    let second = client.new_session(dir.path()).await.expect("second");
    assert_ne!(first, second, "two sessions, two ids");

    let one = client
        .prompt(&first, vec![ContentBlock::text("apples")])
        .await
        .expect("prompt");
    let two = client
        .prompt(&second, vec![ContentBlock::text("oranges")])
        .await
        .expect("prompt");

    assert!(
        one.messages().iter().any(|said| said.contains("apples")),
        "{:?}",
        one.messages(),
    );
    assert!(
        two.messages().iter().any(|said| said.contains("oranges")),
        "{:?}",
        two.messages(),
    );
    assert!(
        !two.messages().iter().any(|said| said.contains("apples")),
        "the second turn carried the first's work: {:?}",
        two.messages(),
    );

    client.close().await.expect("close");
}

/// TC-PORT-ACP-20: the agent's refusal reaches the client whole, over the pipe.
///
/// Input: a prompt naming a session the agent never opened, and a prompt
/// carrying an image the agent did not advertise.
/// Expected: both come back as `ClientError::Refused` carrying the agent's own
/// code and, for the image, the block kind in `data`. A client that flattened
/// these to "it failed" would leave the caller unable to tell a mistake it can
/// fix from one it cannot.
#[tokio::test]
async fn an_agent_refusal_crosses_the_pipe_intact() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");
    let session = client.new_session(dir.path()).await.expect("session");

    let unknown = client
        .prompt("never-opened", vec![ContentBlock::text("hi")])
        .await
        .expect_err("no such session");
    let ClientError::Refused(error) = unknown else {
        panic!("expected the agent's own refusal, got {unknown:?}");
    };
    assert!(error.message.contains("unknown session"), "{error:?}");

    let image = client
        .prompt(
            &session,
            vec![ContentBlock::Image {
                data: "AA".into(),
                mime_type: "image/png".into(),
            }],
        )
        .await
        .expect_err("images were not advertised");
    let ClientError::Refused(error) = image else {
        panic!("expected a refusal, got {image:?}");
    };
    assert_eq!(error.data, Some(serde_json::json!({ "kind": "image" })));

    client.close().await.expect("close");
}

/// TC-PORT-ACP-21: a client answers the agent's permission question, and only
/// with an option the agent offered.
///
/// Input: the agent's `session/request_permission`, put to a client under each
/// policy.
/// Expected: the allowing client answers `allow-once` and the refusing one
/// `reject-once`, each recorded against the call id it was asked about. A
/// client that only sent frames would leave the agent waiting for ever, which
/// is why answering is what makes this a client at all.
///
/// The question is driven directly rather than through a turn because the
/// engine has no approval seam to raise one from yet - `EventSink` carries no
/// server-to-client request and `ApprovalService` is constructed only by its
/// own suite - so the two joins named in `docs/parity.md` are what stand
/// between this and a tool call actually asking.
#[tokio::test]
async fn a_client_answers_the_agents_permission_question() {
    use std::sync::Mutex;
    use tetanus_acp::AcpBridge;
    use tetanus_protocol::types::ApprovalOutcome;
    use tetanus_rpc::FrameSink;

    #[derive(Default)]
    struct Frames(Mutex<Vec<serde_json::Value>>);
    impl FrameSink for Frames {
        fn send_frame(&self, frame: String) {
            self.0
                .lock()
                .expect("frames")
                .push(serde_json::from_str(&frame).expect("JSON"));
        }
    }

    for (policy, expected) in [
        (PermissionPolicy::AllowOnce, ApprovalOutcome::AllowedOnce),
        (PermissionPolicy::Reject, ApprovalOutcome::Rejected),
    ] {
        let dir = TempDir::new().expect("temp dir");
        let engine: Arc<dyn Engine> = Arc::new(HarnessEngine::new(EngineConfig {
            sessions_root: dir.path().join("sessions"),
            ..EngineConfig::default()
        }));
        let bridge = Arc::new(AcpBridge::new(engine));
        let frames = Arc::new(Frames::default());
        let out: Arc<dyn FrameSink> = Arc::clone(&frames) as Arc<dyn FrameSink>;

        let asking = {
            let bridge = Arc::clone(&bridge);
            let out = Arc::clone(&out);
            tokio::spawn(async move { bridge.request_permission("s", "call_7", &out).await })
        };

        // The question, as the agent wrote it.
        let asked = loop {
            if let Some(frame) = frames.0.lock().expect("frames").first().cloned() {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        assert_eq!(
            asked["method"],
            serde_json::json!("session/request_permission")
        );

        // The answer, as this client's policy would write it.
        let chosen = match policy {
            PermissionPolicy::AllowOnce => "allow-once",
            PermissionPolicy::Reject => "reject-once",
        };
        let offered: Vec<String> = asked["params"]["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| option["optionId"].as_str().expect("an id").to_string())
            .collect();
        assert!(offered.iter().any(|id| id == chosen), "{offered:?}");

        let reply = serde_json::json!({
            "jsonrpc": "2.0",
            "id": asked["id"],
            "result": { "outcome": { "outcome": "selected", "optionId": chosen } },
        });
        bridge.frame(&reply.to_string(), &out).await;

        let settled = tokio::time::timeout(DEADLINE, asking)
            .await
            .expect("the question settles")
            .expect("joins");
        assert_eq!(settled, expected, "under {policy:?}");
    }
}

/// TC-PORT-ACP-22: closing reaps the child *gracefully*, and closing twice is
/// not an error.
///
/// Input: a connected agent, closed, then closed again.
/// Expected: both calls return, the child is gone, and the first close takes
/// well under the kill fallback - which is the assertion that matters. Closing
/// the pipe is how a well-behaved agent is told to stop, and a pipe is closed
/// by dropping the writer, not by calling `shutdown` on a handle the client
/// still owns. Getting that wrong leaves the descriptor open, the child never
/// reaches end of file, and every teardown silently waits out the fallback and
/// kills a process that would have exited on its own. It passes either way,
/// which is why the bound is here.
#[tokio::test]
async fn closing_reaps_the_child_gracefully_and_is_idempotent() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");

    let started = std::time::Instant::now();
    tokio::time::timeout(DEADLINE, client.close())
        .await
        .expect("close returns rather than hanging")
        .expect("close");
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(5),
        "the child took end-of-file and left; it did not wait out the kill \
         fallback: took {took:?}",
    );

    tokio::time::timeout(DEADLINE, client.close())
        .await
        .expect("the second close returns too")
        .expect("close");
}

/// TC-PORT-ACP-23: a call to an agent that is gone fails, and says so, rather
/// than waiting out its deadline.
///
/// Input: a closed client, then a call on it.
/// Expected: `Transport`, promptly - well inside the deadline. When the pipe
/// closes, every waiter is released at once rather than left to time out: the
/// answer is already known, and making a caller wait two minutes to be told
/// something that was true immediately is the difference between a diagnosis
/// and a hang.
#[tokio::test]
async fn a_call_after_the_agent_is_gone_fails_promptly() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");
    client.close().await.expect("close");

    let started = std::time::Instant::now();
    let refused = client
        .new_session(dir.path())
        .await
        .expect_err("the agent is gone");
    let took = started.elapsed();

    assert!(
        matches!(refused, ClientError::Transport(_)),
        "expected a transport failure, got {refused:?}",
    );
    assert!(
        took < Duration::from_secs(5),
        "released at once, not at the deadline: took {took:?}",
    );
}

/// TC-PORT-ACP-24: a relative working directory is refused by the client,
/// without a round trip.
///
/// Input: `session/new` with a relative path.
/// Expected: `Protocol`, raised locally. ACP requires an absolute `cwd` and the
/// agent checks it too (TC-PORT-ACP-5); checking here as well means a caller
/// learns of its own mistake without paying a round trip to be told, and the
/// agent's check remains the one that binds.
#[tokio::test]
async fn a_relative_cwd_is_refused_before_it_is_sent() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");

    let refused = client
        .new_session(&PathBuf::from("workspace"))
        .await
        .expect_err("a relative cwd");
    assert!(
        matches!(refused, ClientError::Protocol(_)),
        "raised locally, got {refused:?}",
    );

    client.close().await.expect("close");
}

/// TC-PORT-ACP-30: a client re-opens a session on a second connection to the
/// same agent process and is handed the first connection's work.
///
/// Input: one turn on a session, the client closed, a second client connected
/// to a *new* agent process over the same journal root, and `session/load`.
/// Expected: the load answers, and the history it returns carries the first
/// turn's committed messages and its tool call. This is the claim a resume is
/// for: the conversation outlives the connection that started it, and outlives
/// the process too, because the journal and not the connection is where it
/// lives.
#[tokio::test]
async fn a_session_survives_the_connection_that_opened_it() {
    let dir = TempDir::new().expect("temp dir");

    let session = {
        let mut first = connect(&dir, PermissionPolicy::Reject).await;
        first.initialize().await.expect("initialize");
        let session = first.new_session(dir.path()).await.expect("session/new");
        let outcome = first
            .prompt(&session, vec![ContentBlock::text("remember this")])
            .await
            .expect("prompt");
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        first.close().await.expect("close");
        session
    };

    // A second agent process entirely: same journal root, no shared memory.
    let mut second = connect(&dir, PermissionPolicy::Reject).await;
    let hello = second.initialize().await.expect("initialize");
    assert!(
        hello.agent_capabilities.load_session,
        "the agent says it can do this before we ask it to",
    );

    let history = second
        .load_session(&session, dir.path())
        .await
        .expect("session/load");

    let said: Vec<String> = history
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::AgentMessageChunk {
                content: ContentBlock::Text { text },
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        vec!["Let me echo that back.", "You said: remember this"],
        "the earlier turn crossed a process boundary: {history:?}",
    );
    assert!(
        history
            .iter()
            .any(|update| matches!(update, SessionUpdate::ToolCall { .. })),
        "including its tool call: {history:?}",
    );

    second.close().await.expect("close");
}

/// TC-PORT-ACP-31: a relative working directory is refused by the client on a
/// load too, without a round trip.
///
/// Input: `session/load` with a relative path.
/// Expected: `Protocol`, raised locally. The check is the same one
/// `session/new` makes (TC-PORT-ACP-24), and it is shared rather than written
/// twice, because a rule enforced in two places is a rule that will be enforced
/// in one of them after the next edit.
#[tokio::test]
async fn a_relative_cwd_is_refused_on_a_load_too() {
    let dir = TempDir::new().expect("temp dir");
    let mut client = connect(&dir, PermissionPolicy::Reject).await;
    client.initialize().await.expect("initialize");

    let refused = client
        .load_session("whatever", &PathBuf::from("workspace"))
        .await
        .expect_err("a relative cwd");
    assert!(
        matches!(refused, ClientError::Protocol(_)),
        "raised locally, got {refused:?}",
    );

    client.close().await.expect("close");
}
