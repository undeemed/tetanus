//! Test Design Specification: bounded retry across a real HTTP/SSE boundary.
//!
//! Features under test: upstream
//! `packages/llm/llm-retry/tests/transport-recovery.spec.ts` - what a turn does
//! when the provider fails the way a socket fails rather than the way a mock
//! does. The adapter, the decoder, the recovery point and the retry executor
//! all run, so a case here passes only if the whole path agrees.
//!
//! Approach: each case scripts a fake provider on a loopback port and drives
//! the real [`DeepSeekAdapter`] against it over HTTP. The script says how each
//! request ends - completely, with no content, with a clean body and no
//! `[DONE]`, cut off mid-message, or closed before any answer - which is how
//! the failures that only exist on a wire are reached.
//!
//! Features NOT tested here: which failures the policy lists
//! (`upstream_retry_policy.rs`), the executor's own records
//! (`upstream_retry_executor.rs`), and the decoder read frame by frame
//! (`upstream_deepseek_wire.rs`). Upstream's stalled-body case is not ported:
//! tetanus sets no request or idle timeout, so nothing yet turns a stall into
//! `TIMEOUT`. Section 3 of `docs/parity.md` carries the gap.
//!
//! Environmental needs: a writable temp directory and a loopback socket. No
//! case reaches the network or a real API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use tempfile::TempDir;
use tetanus_core::{Context, EffectHandle, EventBus};
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::events::FAILED_STOP_REASON;
use tetanus_turn::llm::deepseek::{DeepSeekAdapter, DeepSeekConfig, PROVIDER, STREAM_CLOSED};
use tetanus_turn::llm::retry::{
    install, Backoff, RetryPolicy, DEFAULT_MAX_RETRIES, DEFAULT_RETRYABLE_CODES, RETRY_EVENT,
};
use tetanus_turn::log::topic;
use tetanus_turn::tools::ToolRegistry;
use tetanus_turn::{TurnConfig, TurnEngine, TurnError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The credential the fake provider is addressed with. A test never reads the
/// real one, and never leaves the real one's name holding a made-up value.
const TEST_API_KEY_ENV: &str = "TETANUS_TEST_XPORT_KEY";
const MODEL: &str = "mock-model";
const PROMPT: &str = "recover through the provider boundary";

/// What a cut-off attempt manages to say before it dies. Three frames, so a
/// case can tell chunks that arrived from chunks that were committed.
const PARTIAL: [&str; 3] = ["dis", "card", " me"];

/// TC-PORT-XPORT-1: a refused connection recovers when the endpoint starts
/// during the wait.
///
/// Upstream: "recovers from a true refused connection after the endpoint
/// starts during backoff".
///
/// Input: a base URL naming a port nothing listens on, and a provider that
/// binds that port once the scheduled retry is on the journal.
/// Expected: the turn answers with what the second attempt streamed; the
/// endpoint saw one request, because the first never arrived; one `llm/retry`
/// classed `TRANSPORT`; and one step, because a retry is another attempt at
/// the same step and not a new one.
#[tokio::test]
async fn a_refused_connection_recovers_when_the_endpoint_starts_during_the_wait() {
    let port = unused_port().await;
    let route = route("xport-refused", &url(port), 200.0).await;

    let log = Arc::clone(route.engine.log());
    let starting = tokio::spawn(async move {
        until_recorded(&log, RETRY_EVENT).await;
        Provider::on(
            TcpListener::bind(("127.0.0.1", port)).await.expect("bind"),
            vec![Answer::Text("connected after retry")],
        )
    });

    let outcome = route.engine.run_turn(PROMPT).await.expect("the turn ran");
    let provider = starting.await.expect("the endpoint started");

    assert_eq!(outcome.content, "connected after retry");
    assert_eq!(
        provider.requests().len(),
        1,
        "the refused attempt sent none"
    );
    assert_eq!(codes(&route), ["TRANSPORT"]);
    assert_eq!(count(&route, topic::STEP_START), 1);
}

/// TC-PORT-XPORT-2: a body cut off mid-message is retried, and what it
/// streamed is not committed.
///
/// Upstream: "retries %s without committing failed chunks".
///
/// Input: a provider whose first answer stops mid-body after three frames,
/// and whose second answers completely.
/// Expected: two requests carrying the same body, because a retry re-sends the
/// conversation the failed attempt was given rather than one grown by it; the
/// three chunks the failed attempt streamed are on the journal before the
/// `llm/retry`, since a reader saw them arrive; and one `assistant/message`,
/// holding the second answer.
#[tokio::test]
async fn a_cut_off_body_is_retried_and_commits_nothing() {
    let provider = Provider::start(vec![Answer::Cut, Answer::Text("recovered response")]).await;
    let route = route("xport-cut", &provider.base_url(), 2.0).await;

    let outcome = route.engine.run_turn(PROMPT).await.expect("the turn ran");

    assert_eq!(outcome.content, "recovered response");
    let sent = provider.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the retry re-sent the same request");
    assert_eq!(codes(&route), ["TRANSPORT"]);

    let retry = records(&route, RETRY_EVENT)[0].seq;
    let before = records(&route, topic::ASSISTANT_CHUNK)
        .into_iter()
        .filter(|event| event.seq < retry)
        .count();
    assert_eq!(before, PARTIAL.len(), "what arrived was streamed");
    let committed = records(&route, topic::ASSISTANT_MESSAGE);
    assert_eq!(committed.len(), 1, "only the attempt that finished");
    assert_eq!(committed[0].data["content"], "recovered response");
}

/// TC-PORT-XPORT-3: a wire-valid completion carrying nothing is retried, and
/// no empty message is committed.
///
/// Upstream: "retries a wire-valid content-less completion without committing
/// an empty message".
///
/// Input: a provider whose first answer is a well-formed stream that finishes
/// on `stop` with no content at all, and whose second answers.
/// Expected: two requests carrying the same body; one `llm/retry` classed
/// `EMPTY_RESPONSE`, which is in the default set for this reason; one
/// committed message; and a turn that ended naturally.
#[tokio::test]
async fn a_content_less_completion_is_retried_and_commits_nothing() {
    let provider = Provider::start(vec![
        Answer::Contentless,
        Answer::Text("recovered from empty"),
    ])
    .await;
    let route = route("xport-empty", &provider.base_url(), 2.0).await;

    let outcome = route.engine.run_turn(PROMPT).await.expect("the turn ran");

    assert_eq!(outcome.content, "recovered from empty");
    let sent = provider.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the retry re-sent the same request");
    assert_eq!(codes(&route), ["EMPTY_RESPONSE"]);
    assert_eq!(count(&route, topic::ASSISTANT_MESSAGE), 1);
    assert_eq!(closer(&route)["stop_reason"], "natural");
}

/// TC-PORT-XPORT-4: a body that ends cleanly without `[DONE]` ends the turn
/// and is not retried.
///
/// Upstream: "exposes a clean partial EOF as non-default-retryable
/// STREAM_CLOSED".
///
/// Input: a provider whose answer streams three frames and then closes the
/// body normally, never sending the sentinel.
/// Expected: the turn fails with the protocol failure, naming what is missing;
/// the endpoint saw one request, because `PROTOCOL` is outside the default
/// retryable set; the three frames are on the journal, but nothing is
/// committed from an answer that never finished; and the closer says failed.
#[tokio::test]
async fn a_clean_end_without_the_sentinel_is_not_retried() {
    let provider = Provider::start(vec![Answer::CleanEof, Answer::Text("never asked for")]).await;
    let route = route("xport-clean-eof", &provider.base_url(), 2.0).await;

    let failed = route.engine.run_turn(PROMPT).await;

    match failed {
        Err(TurnError::Llm(error)) => {
            assert_eq!(error.code(), "PROTOCOL");
            assert!(error.to_string().contains(STREAM_CLOSED), "{error}");
        }
        other => panic!("expected the stream to be refused, got {other:?}"),
    }
    assert_eq!(provider.requests().len(), 1, "asked once, not again");
    assert_eq!(count(&route, topic::ASSISTANT_CHUNK), PARTIAL.len());
    assert_eq!(count(&route, topic::ASSISTANT_MESSAGE), 0);
    assert_eq!(count(&route, RETRY_EVENT), 0);
    assert_eq!(closer(&route)["stop_reason"], FAILED_STOP_REASON);
}

/// TC-PORT-XPORT-5: the transport retry budget is a budget.
///
/// Upstream: "stops after the configured transport retry budget is exhausted".
///
/// Input: a provider that closes every connection before answering, under the
/// default bound of two retries.
/// Expected: three requests reached the endpoint, two retries were scheduled,
/// the turn opened one step, and it failed with the transport failure naming
/// the endpoint that could not be reached.
#[tokio::test]
async fn an_endpoint_that_never_answers_stops_at_the_budget() {
    let provider = Provider::start(vec![Answer::Reset, Answer::Reset, Answer::Reset]).await;
    let route = route("xport-exhausted", &provider.base_url(), 2.0).await;

    let failed = route.engine.run_turn(PROMPT).await;

    match failed {
        Err(TurnError::Llm(error)) => {
            assert_eq!(error.code(), "TRANSPORT");
            assert!(error.to_string().contains(&provider.base_url()), "{error}");
        }
        other => panic!("expected the route to be unreachable, got {other:?}"),
    }
    assert_eq!(provider.requests().len(), 3, "the first, then two retries");
    assert_eq!(count(&route, RETRY_EVENT), 2);
    assert_eq!(count(&route, topic::STEP_START), 1);
    assert_eq!(closer(&route)["stop_reason"], FAILED_STOP_REASON);
}

/// One booted turn engine talking to a fake provider, with the retry executor
/// installed on that provider's route.
struct Route {
    engine: TurnEngine,
    log_path: PathBuf,
    /// Declared before the context so it is removed first: it listens on the
    /// same bus the context unwinds.
    _retry: EffectHandle,
    _ctx: Context,
    _dir: TempDir,
}

/// The fixture: the real HTTP adapter pointed at `base_url`, under a normal
/// policy over the default retryable set and the default bound.
async fn route(name: &str, base_url: &str, delay_ms: f64) -> Route {
    static KEY: Once = Once::new();
    KEY.call_once(|| std::env::set_var(TEST_API_KEY_ENV, "mock-key"));

    let dir = tempfile::tempdir().expect("temp dir");
    let log_path = dir.path().join(format!("{name}.jsonl"));
    let bus = EventBus::new();
    let log: Arc<dyn SessionLog> =
        JsonlSessionLog::create(name, &log_path, bus.clone()).expect("journal");
    let adapter = DeepSeekAdapter::with_http(DeepSeekConfig {
        api_key_env: TEST_API_KEY_ENV.to_string(),
        base_url: base_url.to_string(),
        models: vec![MODEL.to_string()],
        max_tokens: None,
    });
    let ctx = boot(
        bus.clone(),
        Arc::new(adapter),
        // No tool is offered: these cases are about the request, and a turn
        // that answers in text closes in one step.
        Arc::new(ToolRegistry::new()),
        Arc::clone(&log),
    )
    .expect("boot");
    let policy = RetryPolicy::Normal {
        max_retries: DEFAULT_MAX_RETRIES,
        retryable_codes: DEFAULT_RETRYABLE_CODES.map(str::to_string).to_vec(),
        backoff: Backoff {
            initial_delay_ms: delay_ms,
            max_delay_ms: delay_ms,
            jitter_ratio: 0.0,
        },
    };
    // The jitter source samples the middle of the range; at a ratio of zero
    // the wait is the delay itself, so a case spends what it configured.
    let retry = install(&bus, log, PROVIDER, policy, Arc::new(|| 0.5));
    let config = TurnConfig {
        model: MODEL.to_string(),
        ..TurnConfig::default()
    };
    let engine = TurnEngine::from_context(&ctx, config).expect("engine");

    Route {
        engine,
        log_path,
        _retry: retry,
        _ctx: ctx,
        _dir: dir,
    }
}

/// The records of one type, read back off the journal rather than out of the
/// log's memory: what a surface or a resumed session sees is the file.
fn records(route: &Route, ty: &str) -> Vec<SessionEvent> {
    tetanus_session::replay(&route.log_path)
        .expect("the journal reads back")
        .into_iter()
        .filter(|event| event.ty == ty)
        .collect()
}

fn count(route: &Route, ty: &str) -> usize {
    records(route, ty).len()
}

/// How each scheduled retry classed the failure that caused it.
fn codes(route: &Route) -> Vec<String> {
    records(route, RETRY_EVENT)
        .into_iter()
        .map(|event| event.data["code"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `turn/end` the turn wrote, whichever way it ended.
fn closer(route: &Route) -> serde_json::Value {
    records(route, topic::TURN_END)
        .pop()
        .expect("the turn closed")
        .data
}

/// Wait until the log carries a record of type `ty`, so a case can act at a
/// point in the run rather than at a point on the clock.
async fn until_recorded(log: &Arc<dyn SessionLog>, ty: &str) {
    for _ in 0..2_000 {
        if log.events().iter().any(|event| event.ty == ty) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("no {ty} record was written");
}

/// A port bound and released, so the first attempt at it is refused rather
/// than merely unanswered.
async fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    listener.local_addr().expect("address").port()
}

fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// How one scripted request ends.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// A complete stream: the text, a `stop`, and the sentinel.
    Text(&'static str),
    /// A complete stream that carries no content at all.
    Contentless,
    /// Frames, then the body ends normally with no sentinel.
    CleanEof,
    /// Frames, then the body stops mid-message.
    Cut,
    /// The request is read and the connection closed with no answer.
    Reset,
}

/// A fake provider on a loopback port, answering a scripted sequence and
/// keeping every request body it was sent.
struct Provider {
    port: u16,
    sent: Arc<Mutex<Vec<serde_json::Value>>>,
    serving: tokio::task::JoinHandle<()>,
}

impl Provider {
    async fn start(answers: Vec<Answer>) -> Self {
        Self::on(
            TcpListener::bind("127.0.0.1:0").await.expect("bind"),
            answers,
        )
    }

    fn on(listener: TcpListener, answers: Vec<Answer>) -> Self {
        let port = listener.local_addr().expect("address").port();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let recording = Arc::clone(&sent);
        let serving = tokio::spawn(async move {
            for answer in answers {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                // The request is recorded before the answer, so a connection
                // closed without one still counts as a request that arrived.
                let request = read(&mut socket).await;
                recording.lock().expect("sent").push(request);
                write_answer(&mut socket, answer).await;
            }
        });
        Self {
            port,
            sent,
            serving,
        }
    }

    fn base_url(&self) -> String {
        url(self.port)
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.sent.lock().expect("sent").clone()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

/// Read one request and answer with its JSON body.
async fn read(socket: &mut TcpStream) -> serde_json::Value {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match socket.read(&mut buffer).await.expect("read") {
            0 => panic!("the request ended before its body"),
            read => raw.extend_from_slice(&buffer[..read]),
        }
        if let Some(body) = body(&raw) {
            return body;
        }
    }
}

/// The request body, once all of it has arrived. `None` means read on.
fn body(raw: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(raw);
    let (head, rest) = text.split_once("\r\n\r\n")?;
    let length: usize = head.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("content-length:")?
            .trim()
            .parse()
            .ok()
    })?;
    (rest.len() >= length).then(|| serde_json::from_str(&rest[..length]).expect("a JSON body"))
}

const OK_HEAD: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
const CHUNKED_HEAD: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";

async fn write_answer(socket: &mut TcpStream, answer: Answer) {
    let partial = || PARTIAL.iter().map(|piece| delta(piece)).collect();
    // A cut answer is chunked and never gets its terminating chunk: the body
    // stops mid-message, which is what a dropped stream looks like. Every
    // other answer ends its body by closing, so a clean end is a clean end.
    let (head, frames): (&str, Vec<String>) = match answer {
        // Nothing is written: the close is the answer.
        Answer::Reset => (OK_HEAD, Vec::new()),
        Answer::Text(text) => (OK_HEAD, vec![delta(text), finish(), "[DONE]".to_string()]),
        Answer::Contentless => (OK_HEAD, vec![finish(), "[DONE]".to_string()]),
        Answer::CleanEof => (OK_HEAD, partial()),
        Answer::Cut => (CHUNKED_HEAD, partial()),
    };
    if !frames.is_empty() {
        let mut body = String::from(head);
        for data in frames {
            let sse = frame(&data);
            match answer {
                Answer::Cut => body.push_str(&format!("{:x}\r\n{sse}\r\n", sse.len())),
                _ => body.push_str(&sse),
            }
        }
        write(socket, &body).await;
    }
    let _ = socket.shutdown().await;
}

async fn write(socket: &mut TcpStream, text: &str) {
    socket.write_all(text.as_bytes()).await.expect("write");
    socket.flush().await.expect("flush");
}

fn frame(data: &str) -> String {
    format!("data: {data}\n\n")
}

fn delta(text: &str) -> String {
    serde_json::json!({ "choices": [{ "index": 0, "delta": { "content": text } }] }).to_string()
}

fn finish() -> String {
    serde_json::json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] })
        .to_string()
}
