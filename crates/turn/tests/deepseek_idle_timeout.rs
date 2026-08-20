//! Test Design Specification: the idle window on a DeepSeek stream.
//!
//! Feature under test: the watchdog `ReqwestTransport` runs a request under,
//! and the failure a provider that stops speaking is reported as. A provider
//! may take as long as it likes to answer; what it may not do is accept a
//! connection and then go silent, because until this window existed that was a
//! turn that never ended.
//!
//! Approach: three cases drive the real adapter and the real HTTP client
//! against a loopback endpoint that writes bytes on a schedule the case
//! decides, so the thing under test is the transport rather than a stand-in
//! for it. The fourth case is arithmetic and needs no endpoint.
//!
//! Features NOT tested here: TLS, the retry that follows the failure
//! (`crates/turn/tests/upstream_retry_executor.rs`), wire serialization and
//! stream decoding (`crates/turn/tests/deepseek_adapter.rs`).
//!
//! Environmental needs: a loopback port. No case reaches a network or a real
//! API key, and none reads a variable another test binary writes.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, a hang, or a panic.

use std::sync::Once;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use tetanus_turn::llm::deepseek::{
    DeepSeekAdapter, DeepSeekConfig, DEFAULT_STREAM_IDLE_TIMEOUT_MS, PROVIDER,
};
use tetanus_turn::llm::retry::DEFAULT_RETRYABLE_CODES;
use tetanus_turn::llm::{CollectingSink, LlmAdapter, LlmError, Message, ModelRequest};

/// The window every scripted case runs with. Long enough that a loaded runner
/// scheduling a write late is not a failure, short enough that the two cases
/// which wait it out cost half a second each.
const WINDOW: Duration = Duration::from_millis(500);

/// The gap between the keep-alive comments TC-DS-IDLE-3 sends. Five of them
/// outlive one window, and each gap is under a third of it, so that case turns
/// red only if a write is more than twice its own interval late.
const KEEPALIVE: Duration = Duration::from_millis(150);

/// TC-DS-IDLE-1: a service that accepts a connection and then never answers
/// fails as a timeout, and does so only once the window has passed.
///
/// Input: an endpoint that accepts the request and writes nothing at all, not
/// even a response head.
/// Expected: [`LlmError::Timeout`] naming the window it exceeded, code
/// `TIMEOUT`, no chunk in the sink, and a call that returned no earlier than
/// the window. Without the watchdog this call never returns at all: the socket
/// is open, so nothing below reports a fault, and the turn waits for ever.
#[tokio::test]
async fn a_service_that_never_answers_fails_instead_of_hanging() {
    let endpoint = endpoint(Vec::new(), Hangup::No).await;
    let adapter = adapter(&endpoint.base_url);
    let mut sink = CollectingSink::default();

    let started = Instant::now();
    let failed = adapter
        .stream(&request(), &mut sink)
        .await
        .expect_err("the service never answered");

    assert!(matches!(failed, LlmError::Timeout(_)), "{failed:?}");
    assert_eq!(failed.code(), "TIMEOUT");
    assert!(
        failed.to_string().contains("500ms"),
        "the failure names no window: {failed}"
    );
    assert!(
        started.elapsed() >= WINDOW,
        "it gave up after {:?}, before the window",
        started.elapsed()
    );
    assert!(sink.chunks.is_empty(), "{:?}", sink.chunks);
}

/// TC-DS-IDLE-2: a provider that answers and then stops speaking part way
/// through the stream fails the same way.
///
/// Upstream: `llm-deepseek/tests/adapter.spec.ts`, "aborts the underlying body
/// when the stream stays idle past its watchdog".
///
/// Input: an endpoint that writes the head and one content frame, then holds
/// the connection open.
/// Expected: [`LlmError::Timeout`], and the delta that did arrive reached the
/// sink. What makes this different from TC-DS-IDLE-1 is where the silence
/// falls: the head arrived, so the window is being rearmed on body reads, and
/// this is the read that never came.
#[tokio::test]
async fn a_stream_that_stalls_part_way_through_fails() {
    let endpoint = endpoint(vec![beat(0, HEAD), beat(0, DELTA)], Hangup::No).await;
    let adapter = adapter(&endpoint.base_url);
    let mut sink = CollectingSink::default();

    let failed = adapter
        .stream(&request(), &mut sink)
        .await
        .expect_err("the provider stopped mid-answer");

    assert!(matches!(failed, LlmError::Timeout(_)), "{failed:?}");
    assert_eq!(failed.code(), "TIMEOUT");
    assert_eq!(
        sink.chunks.len(),
        1,
        "the delta that arrived is missing: {:?}",
        sink.chunks
    );
}

/// TC-DS-IDLE-3: keep-alive comments hold a stream open, so a model that
/// thinks for longer than one window keeps its answer.
///
/// Upstream: `llm-deepseek/tests/adapter.spec.ts`, "keeps an idle provider read
/// alive through SSE comments".
///
/// Input: an endpoint that writes the head, then five `:` comment lines one
/// keep-alive interval apart, then the answer and the sentinel. The comments
/// span longer than one window, and no comment carries a frame.
/// Expected: the ordinary answer, and a call that took longer than the window.
/// This is why the watchdog sits on the connection: a window measured in
/// decoded frames would cut this stream, because comments decode to nothing.
#[tokio::test]
async fn keep_alive_comments_hold_the_window_open() {
    let mut script = vec![beat(0, HEAD)];
    script.extend((0..5).map(|_| beat(KEEPALIVE.as_millis() as u64, ": keep-alive\n\n")));
    script.push(beat(0, ANSWER));
    let endpoint = endpoint(script, Hangup::Yes).await;
    let adapter = adapter(&endpoint.base_url);
    let mut sink = CollectingSink::default();

    let started = Instant::now();
    let answered = adapter
        .stream(&request(), &mut sink)
        .await
        .expect("the stream stayed alive");

    assert_eq!(answered.content, "hi");
    assert_eq!(answered.finish_reason, "stop");
    assert!(
        started.elapsed() > WINDOW,
        "the stream ended inside one window, so it proves nothing: {:?}",
        started.elapsed()
    );
}

/// TC-DS-IDLE-4: the window a route runs with, and what its failure means to a
/// retry policy.
///
/// Input: the default configuration, a configuration asking for no window at
/// all, and a timeout failure.
/// Expected: five minutes by default, upstream's own figure; a zero read as
/// that default rather than as a window of no time, which would fail every
/// request; and the code `TIMEOUT`, which is in the default retryable set, so
/// a provider that goes quiet is asked again rather than ending the turn.
#[test]
fn the_default_window_is_upstreams_and_its_failure_is_retryable() {
    assert_eq!(DEFAULT_STREAM_IDLE_TIMEOUT_MS, 300_000);
    assert_eq!(
        DeepSeekConfig::default().idle_window(),
        Duration::from_secs(300)
    );
    assert_eq!(
        DeepSeekConfig {
            stream_idle_timeout_ms: 0,
            ..DeepSeekConfig::default()
        }
        .idle_window(),
        Duration::from_secs(300),
        "a zero window would fail every request"
    );

    let stalled = LlmError::Timeout("the stream from x was idle for 1ms".to_string());
    assert_eq!(stalled.code(), "TIMEOUT");
    assert!(DEFAULT_RETRYABLE_CODES.contains(&stalled.code()));
}

/// The response head every scripted endpoint answers with. No content length
/// and no chunked encoding: an SSE body ends when the connection does, which
/// is what lets a case hold one open.
const HEAD: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";

/// One content delta, and nothing that ends the stream.
const DELTA: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";

/// A whole answer: the delta, its finish reason, and the sentinel.
const ANSWER: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// One scripted write: the silence before it, then the bytes.
struct Beat {
    after: Duration,
    bytes: &'static str,
}

fn beat(after_ms: u64, bytes: &'static str) -> Beat {
    Beat {
        after: Duration::from_millis(after_ms),
        bytes,
    }
}

/// What the endpoint does when its script runs out.
enum Hangup {
    /// Close the connection, which ends an SSE body.
    Yes,
    /// Hold it open and stay silent, which is the stall under test.
    No,
}

/// A loopback endpoint that speaks one HTTP response on the case's schedule.
struct Endpoint {
    base_url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // The stalled cases leave a task parked on a socket nobody will read
        // again. Ending it with the case keeps one test from paying for
        // another's connection.
        self.server.abort();
    }
}

async fn endpoint(script: Vec<Beat>, hangup: Hangup) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let port = listener.local_addr().expect("the bound address").port();
    let server = tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        read_request(&mut socket).await;
        for beat in script {
            tokio::time::sleep(beat.after).await;
            if socket.write_all(beat.bytes.as_bytes()).await.is_err() {
                return;
            }
        }
        match hangup {
            Hangup::Yes => {
                let _ = socket.shutdown().await;
            }
            // Parked, not closed: a closed connection is a stream that ended,
            // and this case is a provider that has not said so.
            Hangup::No => std::future::pending::<()>().await,
        }
    });
    Endpoint {
        base_url: format!("http://127.0.0.1:{port}"),
        server,
    }
}

/// Read until the request head is complete. The body that follows is the
/// serialized request, which these cases do not judge; what matters is that
/// the client has finished asking before the endpoint starts answering.
async fn read_request(socket: &mut TcpStream) {
    let mut seen = Vec::new();
    let mut buffer = [0u8; 1024];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => seen.extend_from_slice(&buffer[..read]),
        }
    }
}

/// The adapter under test: the real HTTP transport, pointed at a loopback
/// endpoint, watched by a window short enough to test.
fn adapter(base_url: &str) -> DeepSeekAdapter {
    DeepSeekAdapter::with_http(DeepSeekConfig {
        api_key_env: key_env().to_string(),
        base_url: base_url.to_string(),
        stream_idle_timeout_ms: WINDOW.as_millis() as u64,
        ..DeepSeekConfig::default()
    })
}

/// A credential variable only this binary reads, so no case here decides
/// whether the live case in another binary skips.
///
/// It is written once. Every case asks for it through this function, and the
/// `Once` puts that single write before every later read, so the environment
/// is never written while another case reads it.
fn key_env() -> &'static str {
    const NAME: &str = "TETANUS_TEST_IDLE_API_KEY";
    static WRITTEN: Once = Once::new();
    WRITTEN.call_once(|| std::env::set_var(NAME, "test-key"));
    NAME
}

fn request() -> ModelRequest {
    ModelRequest {
        provider: PROVIDER.into(),
        model: "deepseek-v4-flash".into(),
        messages: vec![Message::user("say hi")],
        tools: Vec::new(),
        max_tokens: None,
    }
}
