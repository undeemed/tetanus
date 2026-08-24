//! Test Design Specification: the whole-request deadline on a DeepSeek
//! stream.
//!
//! Feature under test: the second of the two bounds `ReqwestTransport` runs a
//! request under. The idle window (`deepseek_idle_timeout.rs`) catches a
//! provider that stops speaking. This catches one that never stops: a route
//! answering a byte every few moments for ever resets the idle window on every
//! byte, so before this bound existed it was a turn that never ended - the
//! same unbounded turn the idle window was added to prevent, reached the other
//! way round.
//!
//! Upstream has no equivalent; the idle window is the only bound it ships.
//! This is the `llm/*` gap section 3 of `docs/parity.md` names, so these cases
//! are written against that gap rather than translated from an upstream suite.
//!
//! Approach: a loopback endpoint that trickles keep-alive comments for ever,
//! under an idle window deliberately set *longer* than the deadline. That
//! ordering is the whole design of the case: the idle window cannot be what
//! ends the request, so anything that ends it is the deadline, and a build
//! without the deadline hangs rather than failing in some other way.
//!
//! Features NOT tested here: the idle window itself
//! (`deepseek_idle_timeout.rs`), the retry that follows the failure
//! (`upstream_retry_executor.rs`), and wire decoding
//! (`deepseek_adapter.rs`).
//!
//! Environmental needs: a loopback port. No case reaches a network or a real
//! API key. The slowest case waits for its own 700ms deadline.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, a hang, or a panic.

use std::sync::Once;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use tetanus_turn::llm::deepseek::{
    DeepSeekAdapter, DeepSeekConfig, DEFAULT_REQUEST_DEADLINE_MS, PROVIDER,
};
use tetanus_turn::llm::retry::DEFAULT_RETRYABLE_CODES;
use tetanus_turn::llm::{CollectingSink, LlmAdapter, LlmError, Message, ModelRequest};

/// The deadline the scripted cases run under.
const DEADLINE: Duration = Duration::from_millis(700);

/// The idle window they run under: longer than the deadline on purpose, so a
/// case that ends can only have ended because of the deadline.
const IDLE: Duration = Duration::from_secs(30);

/// How often the trickling endpoint writes. Well inside the idle window, so
/// the connection never looks quiet.
const TRICKLE: Duration = Duration::from_millis(80);

/// TC-DS-DEADLINE-1: a provider that never stops talking is stopped anyway.
///
/// This is the hole the idle window leaves. The endpoint here is never silent
/// for even a tenth of its idle window, so the watchdog that catches a quiet
/// route can never fire; without a whole-request bound the call runs until the
/// process dies.
///
/// Input: an endpoint that sends a response head and then a keep-alive comment
/// every 80ms for ever, under a 30-second idle window and a 700ms deadline.
/// Expected: [`LlmError::Timeout`] whose message names the deadline and not
/// the window; code `TIMEOUT`; and a call that returned at about the deadline
/// rather than running on. A build without this bound does not fail this case,
/// it hangs in it, which is why the elapsed time is asserted from both sides.
#[tokio::test]
async fn a_provider_that_never_stops_talking_is_stopped_by_the_deadline() {
    let endpoint = trickling().await;
    let adapter = adapter(&endpoint.base_url, IDLE, DEADLINE);
    let mut sink = CollectingSink::default();

    let started = Instant::now();
    let failed = bounded(adapter.stream(&request(), &mut sink))
        .await
        .expect_err("a request that never ends must be ended");
    let elapsed = started.elapsed();

    match &failed {
        LlmError::Timeout(message) => {
            assert!(
                message.contains("deadline"),
                "the failure names the bound it reached: {message}"
            );
            assert!(
                message.contains(&DEADLINE.as_millis().to_string()),
                "and says what that bound was: {message}"
            );
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert_eq!(failed.code(), "TIMEOUT");
    assert!(
        elapsed >= DEADLINE,
        "it cannot fail before its own budget: {elapsed:?}"
    );
    assert!(
        elapsed < IDLE,
        "and it must not have waited for the idle window: {elapsed:?}"
    );
}

/// TC-DS-DEADLINE-2: the two bounds are told apart in the message.
///
/// Both are `TIMEOUT`, because the retry policy should ask again either way.
/// What differs is what the route is doing - quiet, or slow - and that is the
/// only thing that tells a reader whether to raise the deadline or to look at
/// why the provider went silent. A single message for both would hide the one
/// fact the failure is good for.
///
/// Input: the trickling endpoint against a short deadline, and a silent
/// endpoint against a short idle window with a long deadline.
/// Expected: the first names the deadline, the second names the idle window,
/// and neither says the other's word.
#[tokio::test]
async fn the_message_names_which_bound_was_reached() {
    let trickle = trickling().await;
    let slow = bounded(
        adapter(&trickle.base_url, IDLE, DEADLINE)
            .stream(&request(), &mut CollectingSink::default()),
    )
    .await
    .expect_err("the deadline ends it");

    let silence = silent().await;
    let quiet = bounded(
        adapter(&silence.base_url, Duration::from_millis(400), IDLE)
            .stream(&request(), &mut CollectingSink::default()),
    )
    .await
    .expect_err("the idle window ends it");

    let (slow, quiet) = (message(&slow), message(&quiet));
    assert!(slow.contains("deadline"), "{slow}");
    assert!(!slow.contains("idle"), "{slow}");
    assert!(quiet.contains("idle"), "{quiet}");
    assert!(!quiet.contains("deadline"), "{quiet}");
}

/// TC-DS-DEADLINE-3: a request that finishes inside its budget is untouched.
///
/// A bound that also ended healthy requests would be worse than no bound: it
/// would turn a slow-but-working route into a failing one, and the failure
/// would look exactly like the provider's fault.
///
/// Input: an endpoint that answers a complete stream, slowly enough to be
/// interesting but inside the deadline.
/// Expected: the answer arrives, the content is what was sent, and no failure.
#[tokio::test]
async fn a_request_that_finishes_in_time_is_untouched() {
    let endpoint = answering().await;
    let adapter = adapter(&endpoint.base_url, IDLE, DEADLINE);
    let mut sink = CollectingSink::default();

    let answered = adapter
        .stream(&request(), &mut sink)
        .await
        .expect("a stream that finishes in time is not a timeout");

    assert_eq!(answered.content, "hi");
    assert!(!sink.chunks.is_empty(), "and it streamed on the way");
}

/// TC-DS-DEADLINE-4: the budget a route runs with, and what its failure means
/// to a retry policy.
///
/// Input: the default configuration, a configuration asking for no deadline at
/// all, and a deadline failure.
/// Expected: ten minutes by default - longer than any legitimate completion,
/// short enough that a wedged route fails while someone is watching; a zero
/// read as that default rather than as a deadline of no time, which would fail
/// every request, exactly as the idle window reads a zero; and the code
/// `TIMEOUT`, which is in the default retryable set, so a route that is merely
/// slow this once is asked again rather than ending the turn.
#[test]
fn the_default_deadline_is_generous_and_its_failure_is_retryable() {
    assert_eq!(DEFAULT_REQUEST_DEADLINE_MS, 600_000);
    assert_eq!(
        DeepSeekConfig::default().deadline(),
        Duration::from_secs(600)
    );
    assert_eq!(
        DeepSeekConfig {
            request_deadline_ms: 0,
            ..DeepSeekConfig::default()
        }
        .deadline(),
        Duration::from_secs(600),
        "a zero deadline would fail every request"
    );

    // The two bounds are independent: setting one does not disturb the other.
    let tuned = DeepSeekConfig {
        request_deadline_ms: 1_000,
        stream_idle_timeout_ms: 2_000,
        ..DeepSeekConfig::default()
    };
    assert_eq!(tuned.deadline(), Duration::from_secs(1));
    assert_eq!(tuned.idle_window(), Duration::from_secs(2));

    let overran = LlmError::Timeout("the request to x exceeded its 1ms deadline".to_string());
    assert_eq!(overran.code(), "TIMEOUT");
    assert!(DEFAULT_RETRYABLE_CODES.contains(&overran.code()));
}

// ---------------------------------------------------------------- fixtures

/// Run a call that is supposed to be bounded, under a bound of the case's own.
///
/// The property under test is liveness, so a build that lost it does not
/// produce a wrong value - it produces no value at all. Left alone these cases
/// would hang, and a hung suite wedges a CI run instead of failing it. This
/// turns that into a red test with a sentence saying what happened, while
/// being far enough above the deadline that a loaded runner never trips it.
async fn bounded<F, T>(call: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(Duration::from_secs(15), call).await {
        Ok(answer) => answer,
        Err(_) => panic!(
            "the request was still running after 15s: the whole-request deadline did not fire, \
             which is the unbounded turn these cases exist to prevent"
        ),
    }
}

const HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";

/// What the endpoint does after its head.
enum Body {
    /// A keep-alive comment every [`TRICKLE`], for ever. Never silent, never
    /// finished.
    Trickle,
    /// Nothing at all, connection held open.
    Silence,
    /// A complete short stream, then close.
    Answer,
}

struct Endpoint {
    base_url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // Every case here leaves a task parked on a socket nobody will read
        // again; ending it with the case keeps one from paying for another's.
        self.server.abort();
    }
}

async fn trickling() -> Endpoint {
    endpoint(Body::Trickle).await
}

async fn silent() -> Endpoint {
    endpoint(Body::Silence).await
}

async fn answering() -> Endpoint {
    endpoint(Body::Answer).await
}

async fn endpoint(body: Body) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let port = listener.local_addr().expect("the bound address").port();
    let server = tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        read_request(&mut socket).await;
        match body {
            Body::Trickle => {
                if socket.write_all(HEAD.as_bytes()).await.is_err() {
                    return;
                }
                loop {
                    tokio::time::sleep(TRICKLE).await;
                    // A comment line is alive on the wire and decodes to no
                    // frame, which is exactly the shape that defeats a
                    // frame-counting watchdog.
                    if socket.write_all(b": keep-alive\n\n").await.is_err() {
                        return;
                    }
                }
            }
            // Not even a response head: the connection is open and the
            // provider has said nothing.
            Body::Silence => std::future::pending::<()>().await,
            Body::Answer => {
                let stream = format!(
                    "{HEAD}data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    r#"{"choices":[{"delta":{"content":"h"}}]}"#,
                    r#"{"choices":[{"delta":{"content":"i"},"finish_reason":"stop"}]}"#,
                );
                let _ = socket.write_all(stream.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        }
    });
    Endpoint {
        base_url: format!("http://127.0.0.1:{port}"),
        server,
    }
}

/// Read until the request head is complete, so the endpoint answers a client
/// that has finished asking.
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

/// The real adapter and the real HTTP client, pointed at a loopback endpoint
/// and bounded by the two windows the case chose.
fn adapter(base_url: &str, idle: Duration, deadline: Duration) -> DeepSeekAdapter {
    DeepSeekAdapter::with_http(DeepSeekConfig {
        api_key_env: key_env().to_string(),
        base_url: base_url.to_string(),
        stream_idle_timeout_ms: idle.as_millis() as u64,
        request_deadline_ms: deadline.as_millis() as u64,
        ..DeepSeekConfig::default()
    })
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

fn message(error: &LlmError) -> String {
    match error {
        LlmError::Timeout(message) => message.clone(),
        other => panic!("expected a timeout, got {other:?}"),
    }
}

static KEY: Once = Once::new();

/// A credential variable this binary owns, so no case depends on the ambient
/// environment or on what another test binary set.
fn key_env() -> &'static str {
    const NAME: &str = "TETANUS_DEADLINE_TEST_KEY";
    KEY.call_once(|| {
        // Safety: one name, owned by this binary, written once before any
        // case reads it.
        unsafe { std::env::set_var(NAME, "not-a-real-key") };
    });
    NAME
}
