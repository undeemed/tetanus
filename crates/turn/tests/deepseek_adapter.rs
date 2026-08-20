//! Test Design Specification: the DeepSeek chat-completions adapter.
//!
//! Feature under test: wire serialization, SSE stream decoding, how a stream
//! ends, and the wait a throttled provider asks for, for the provider route
//! `deepseek-official`, exercised through a replaying transport and - where the
//! answer is an HTTP response rather than a stream - through the production
//! transport against a loopback socket.
//! Features NOT tested here: TLS, the policy that acts on a provider-asked wait
//! (`upstream_retry_policy.rs` holds it), thinking-mode configuration and the
//! credential seam - Phase ② concerns.
//!
//! Environmental needs: none. TC-DS-LIVE-1 additionally needs `DEEPSEEK_API_KEY`
//! and network, and reports itself skipped without them.
//!
//! The environment is process-wide, so a case that writes it changes what a
//! case running beside it reads. Two rules keep that from being a flake, and
//! both are needed: a case that writes a credential writes one no other case
//! reads, and every case that touches the environment holds [`environment`]
//! while it does.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, MutexGuard};

use tetanus_turn::events::RequestFailure;
use tetanus_turn::llm::deepseek::{
    retry_after_ms, take_frames, wire_request, DeepSeekAdapter, DeepSeekConfig, ReplayTransport,
    ReqwestTransport, SseTransport, StreamDecoder, DEFAULT_API_KEY_ENV, PROVIDER, PUBLIC_BASE_URL,
    STREAM_CLOSED,
};
use tetanus_turn::llm::{CollectingSink, LlmAdapter, LlmError, Message, ModelRequest, StreamChunk};
use tetanus_turn::tools::{ToolCall, ToolSchema};

/// A credential variable only the offline adapter cases read. Writing the real
/// one would decide, from another thread, whether TC-DS-LIVE-1 skips.
const TEST_API_KEY_ENV: &str = "TETANUS_TEST_DEEPSEEK_API_KEY";

static ENVIRONMENT: Mutex<()> = Mutex::const_new(());

/// Serializes the cases that read or write the environment. Reading one
/// variable while another thread writes a different one is still a data race,
/// so the guard is taken for any access, not only for a shared name.
///
/// It is the async mutex because a case holds it across the adapter call it is
/// testing, and a case that panics while holding it does not poison the ones
/// that follow.
async fn environment() -> MutexGuard<'static, ()> {
    ENVIRONMENT.lock().await
}

fn request() -> ModelRequest {
    ModelRequest {
        provider: PROVIDER.into(),
        model: "deepseek-v4-flash".into(),
        messages: vec![
            Message::system("be brief"),
            Message::user("echo hi"),
            Message::assistant(
                "",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({ "text": "hi" }),
                }],
            ),
            Message::tool("call_1", "hi"),
        ],
        tools: vec![ToolSchema {
            name: "echo".into(),
            description: "Return the given text unchanged.".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }],
        max_tokens: Some(1024),
    }
}

/// TC-DS-WIRE-1: a request serializes to the official body shape.
/// Expected: streaming on with usage requested; tool-call arguments carried as
/// a JSON *string*; the tool message keyed by `tool_call_id`; `max_tokens` set.
#[test]
fn serializes_the_official_request_body() {
    let body = wire_request(&request(), None);

    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_tokens"], 1024);

    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[2]["tool_calls"][0]["type"], "function");
    assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "echo");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        r#"{"text":"hi"}"#
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "echo");
}

/// TC-DS-WIRE-2: the adapter-configured cap applies only when the request has
/// none of its own.
/// Expected: `max_tokens == 4096` from config; a request value wins over it.
#[test]
fn the_request_output_cap_wins_over_the_adapter_default() {
    let mut req = request();
    req.max_tokens = None;
    assert_eq!(wire_request(&req, Some(4096))["max_tokens"], 4096);
    assert_eq!(wire_request(&request(), Some(4096))["max_tokens"], 1024);
}

/// TC-DS-SSE-1: a raw byte buffer splits into complete `data:` payloads, and a
/// partial event stays buffered for the next read.
/// Expected: two frames returned; the trailing partial event left in `buffer`.
#[test]
fn splits_sse_events_and_keeps_the_partial_tail() {
    let mut buffer =
        String::from(": keep-alive\n\ndata: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: {\"c\"");

    let frames = take_frames(&mut buffer);

    assert_eq!(frames, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    assert_eq!(buffer, "data: {\"c\"");
}

/// TC-DS-DECODE-1: content, reasoning and fragmented tool-call arguments decode
/// into the `StreamChunk` protocol.
/// Expected: text and reasoning chunks in arrival order; one assembled tool
/// call with parsed arguments; finish reason and usage carried through.
#[test]
fn decodes_text_reasoning_and_fragmented_tool_calls() {
    let mut decoder = StreamDecoder::default();
    let mut chunks = Vec::new();
    for frame in TOOL_CALL_STREAM {
        chunks.extend(decoder.push(frame).expect("frame decodes"));
    }
    let (tail, response) = decoder.finish();
    chunks.extend(tail);

    assert_eq!(
        chunks,
        vec![
            StreamChunk::Reasoning {
                delta: "thinking".into()
            },
            StreamChunk::Text {
                delta: "Let me ".into()
            },
            StreamChunk::Text {
                delta: "echo that.".into()
            },
            StreamChunk::ToolCall {
                call: ToolCall {
                    id: "call_abc".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
            },
        ]
    );
    assert_eq!(response.content, "Let me echo that.");
    assert_eq!(response.reasoning, "thinking");
    assert_eq!(response.finish_reason, "tool_calls");
    assert_eq!(response.usage.expect("usage").prompt_tokens, 11);
    assert_eq!(response.usage.expect("usage").completion_tokens, 7);
}

/// TC-DS-DECODE-2: an in-band provider error becomes an `LlmError`, not a
/// silently empty response.
/// Expected: `LlmError::Provider` whose message names the provider text.
#[test]
fn an_in_band_error_frame_fails_the_stream() {
    let mut decoder = StreamDecoder::default();
    let err = decoder
        .push(r#"{"error":{"message":"rate limited"}}"#)
        .expect_err("in-band error");
    assert!(matches!(err, LlmError::Provider { .. }), "{err}");
    assert!(err.to_string().contains("rate limited"), "{err}");
}

/// TC-DS-DECODE-3: nothing that follows the sentinel is decoded.
///
/// Upstream: `sse.spec.ts`, "stops yielding after DONE even when more data
/// follows". Upstream stops its parser; tetanus has no parser stage to stop, so
/// the decoder itself refuses the late payload.
///
/// Input: `[DONE]`, then a content frame after it.
/// Expected: no chunks and no text on the response. The provider said the
/// answer was finished, so appending to it would report a message it never
/// sent.
#[test]
fn a_frame_after_the_sentinel_is_not_part_of_the_answer() {
    let mut decoder = StreamDecoder::default();
    let mut chunks = decoder.push("[DONE]").expect("the sentinel");
    chunks.extend(
        decoder
            .push(r#"{"choices":[{"delta":{"content":"late"}}]}"#)
            .expect("a late frame is not an error"),
    );

    let (tail, response) = decoder.finish();
    chunks.extend(tail);

    assert!(chunks.is_empty(), "{chunks:?}");
    assert_eq!(response.content, "");
}

/// TC-DS-ADAPTER-1: end to end through the replaying transport, the adapter
/// streams chunks into the sink and returns the assembled response.
/// Expected: the sink sees four chunks in stream order; the transport received
/// the serialized body; the response carries the tool call.
#[tokio::test]
async fn streams_through_the_transport_seam() {
    let _environment = environment().await;
    // The adapter reads its key from the environment exactly as it does in
    // production, but from a variable this case owns.
    std::env::set_var(TEST_API_KEY_ENV, "sk-test-key");
    let config = DeepSeekConfig {
        api_key_env: TEST_API_KEY_ENV.into(),
        ..DeepSeekConfig::default()
    };

    let transport = Arc::new(ReplayTransport::new(TOOL_CALL_STREAM.iter().copied()));
    let adapter = DeepSeekAdapter::new(config, transport.clone());
    let mut sink = CollectingSink::default();

    let response = adapter.stream(&request(), &mut sink).await.expect("stream");

    assert_eq!(adapter.provider(), PROVIDER);
    assert_eq!(adapter.config().base_url, PUBLIC_BASE_URL);
    assert_eq!(sink.chunks.len(), 4);
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "echo");
    assert_eq!(
        transport.last_body().expect("body")["model"],
        "deepseek-v4-flash"
    );

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-CLOSE-1: a stream that ends without `[DONE]` fails, and what it
/// already said still reached the sink.
///
/// Upstream: `sse.spec.ts`, "throws STREAM_CLOSED when the stream ends without
/// DONE". tetanus has no `STREAM_CLOSED` code of its own, so the failure is a
/// `PROTOCOL` one, which keeps it outside the default retryable set exactly as
/// upstream keeps `STREAM_CLOSED` outside it.
///
/// Input: two content frames and then the end of the stream, with no sentinel.
/// Expected: [`LlmError::Protocol`] carrying [`STREAM_CLOSED`]; the two chunks
/// that did arrive are still in the sink, because the failure is about the
/// stream ending and not about what it managed to say.
#[tokio::test]
async fn a_stream_that_ends_without_the_sentinel_fails() {
    let _environment = environment().await;
    let adapter = keyed_adapter(&[
        r#"{"choices":[{"delta":{"content":"half an "}}]}"#,
        r#"{"choices":[{"delta":{"content":"answer"}}]}"#,
    ]);
    let mut sink = CollectingSink::default();

    let err = adapter
        .stream(&request(), &mut sink)
        .await
        .expect_err("the stream was cut short");

    assert!(matches!(err, LlmError::Protocol(ref message) if message == STREAM_CLOSED));
    assert_eq!(err.to_string(), format!("PROTOCOL: {STREAM_CLOSED}"));
    assert_eq!(sink.chunks.len(), 2, "{:?}", sink.chunks);

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-CLOSE-2: a stream that says nothing at all fails the same way.
///
/// Upstream: `sse.spec.ts`, "throws STREAM_CLOSED for an empty stream".
///
/// Input: a transport that yields no frames.
/// Expected: the same failure, and an empty sink. A stream with no frames is
/// not an empty answer: nothing said it ended.
#[tokio::test]
async fn an_empty_stream_fails_rather_than_answering_with_nothing() {
    let _environment = environment().await;
    let adapter = keyed_adapter(&[]);
    let mut sink = CollectingSink::default();

    let err = adapter
        .stream(&request(), &mut sink)
        .await
        .expect_err("nothing arrived");

    assert!(matches!(err, LlmError::Protocol(ref message) if message == STREAM_CLOSED));
    assert!(sink.chunks.is_empty(), "{:?}", sink.chunks);

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-EMPTY-1: a completion that ended cleanly and said nothing is a
/// failure, not an empty answer.
///
/// Upstream: `llm-pi-ai/tests/convert.spec.ts`, "classifies a completed stop
/// with no content as an EMPTY_RESPONSE error", and the `EMPTY_RESPONSE` the
/// official adapter throws for a response with no body.
///
/// Input: one frame that finishes with `stop` and carries no delta at all, then
/// the sentinel.
/// Expected: [`LlmError::EmptyResponse`] naming the model that did it, code
/// `EMPTY_RESPONSE`, and an empty sink. The code is in the default retryable
/// set, so the executor asks again rather than handing the turn a blank answer.
#[tokio::test]
async fn a_completed_response_with_no_content_fails_as_empty() {
    let _environment = environment().await;
    let adapter = keyed_adapter(&[
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ]);
    let mut sink = CollectingSink::default();

    let err = adapter
        .stream(&request(), &mut sink)
        .await
        .expect_err("the model said nothing");

    assert!(matches!(err, LlmError::EmptyResponse(_)));
    assert_eq!(err.code(), "EMPTY_RESPONSE");
    assert_eq!(
        err.to_string(),
        "EMPTY_RESPONSE: model \"deepseek-v4-flash\" returned a completed response with no content"
    );
    assert!(sink.chunks.is_empty(), "{:?}", sink.chunks);

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-EMPTY-2: a stop whose only output is reasoning is an answer.
///
/// Upstream: `convert.spec.ts`, "keeps a thinking-only stop successful (any
/// block counts as content)".
///
/// Input: one reasoning delta, a `stop`, and the sentinel.
/// Expected: the call succeeds, the reasoning is carried, and the sink saw it.
/// A model that thought out loud produced output, however little the user reads
/// of it.
#[tokio::test]
async fn a_stop_that_only_reasoned_is_still_an_answer() {
    let _environment = environment().await;
    let adapter = keyed_adapter(&[
        r#"{"choices":[{"delta":{"reasoning_content":"mull"},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ]);
    let mut sink = CollectingSink::default();

    let response = adapter.stream(&request(), &mut sink).await.expect("stream");

    assert_eq!(response.reasoning, "mull");
    assert!(response.content.is_empty());
    assert_eq!(sink.chunks.len(), 1, "{:?}", sink.chunks);

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-EMPTY-3: a stop whose only output is a tool call is an answer.
///
/// Upstream: the same rule - any content block counts - applied to the block
/// tetanus carries as a call rather than as text.
///
/// Input: one complete tool call, a `stop`, and the sentinel.
/// Expected: the call succeeds and carries the tool call. Answering by asking
/// for a tool is producing output, whatever finish reason the provider labelled
/// it with.
#[tokio::test]
async fn a_stop_that_only_called_a_tool_is_still_an_answer() {
    let _environment = environment().await;
    let adapter = keyed_adapter(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{\"text\":\"hi\"}"}}]},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ]);
    let mut sink = CollectingSink::default();

    let response = adapter.stream(&request(), &mut sink).await.expect("stream");

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "echo");
    assert!(response.content.is_empty());

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-CLOSE-3: a sentinel left in a half-arrived event does not count.
///
/// Upstream: `sse.spec.ts`, "treats a final DONE missing its blank-line
/// terminator as truncation". An event dispatches on its terminator, so a tail
/// at EOF is a cut connection and not a delivered event.
///
/// Input: a byte buffer whose last event is `data: [DONE]` with no blank line
/// after it, split by [`take_frames`] the way the transport splits it.
/// Expected: `take_frames` yields only the completed event and keeps the tail,
/// so the adapter never sees the sentinel and the stream fails. This is the
/// consequence TC-DS-SSE-1 pins the mechanism for.
#[tokio::test]
async fn a_sentinel_in_an_unterminated_tail_does_not_close_the_stream() {
    let _environment = environment().await;
    let mut buffer =
        String::from("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]");
    let frames = take_frames(&mut buffer);

    assert_eq!(frames.len(), 1, "the tail is not an event yet");
    assert_eq!(buffer, "data: [DONE]", "and it stays in the buffer");

    let adapter = keyed_adapter(&frames.iter().map(String::as_str).collect::<Vec<_>>());
    let err = adapter
        .stream(&request(), &mut CollectingSink::default())
        .await
        .expect_err("the sentinel never arrived");

    assert!(matches!(err, LlmError::Protocol(ref message) if message == STREAM_CLOSED));

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-DS-CLOSE-4: a stream cut in the middle of its first event fails.
///
/// Upstream: `sse.spec.ts`, "throws STREAM_CLOSED for a mid-event close".
///
/// Input: a buffer holding `data: {"a"` and nothing more.
/// Expected: `take_frames` yields nothing, so the adapter reads a stream that
/// said nothing and fails. Half a JSON object is not a frame to report as
/// malformed; it is a frame that never finished arriving.
#[tokio::test]
async fn a_stream_cut_inside_its_first_event_fails() {
    let _environment = environment().await;
    let mut buffer = String::from("data: {\"a\"");
    let frames = take_frames(&mut buffer);

    assert!(frames.is_empty(), "{frames:?}");

    let adapter = keyed_adapter(&[]);
    let err = adapter
        .stream(&request(), &mut CollectingSink::default())
        .await
        .expect_err("the event never arrived");

    assert!(matches!(err, LlmError::Protocol(ref message) if message == STREAM_CLOSED));

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// An adapter over canned frames, with the test credential in place. The caller
/// holds the environment guard while it uses the adapter, and removes the
/// variable when it is done.
fn keyed_adapter(frames: &[&str]) -> DeepSeekAdapter {
    std::env::set_var(TEST_API_KEY_ENV, "sk-test-key");
    let config = DeepSeekConfig {
        api_key_env: TEST_API_KEY_ENV.into(),
        ..DeepSeekConfig::default()
    };
    let transport = Arc::new(ReplayTransport::new(frames.iter().copied()));
    DeepSeekAdapter::new(config, transport)
}

/// TC-DS-CRED-1: with no key anywhere the call fails before any network I/O,
/// naming the configuration entry point and never any part of a key.
/// Expected: `LlmError::MissingCredential("TETANUS_TEST_ABSENT_KEY")`.
#[tokio::test]
async fn a_missing_key_fails_before_network_io() {
    let _environment = environment().await;
    let config = DeepSeekConfig {
        api_key_env: "TETANUS_TEST_ABSENT_KEY".into(),
        ..DeepSeekConfig::default()
    };
    let transport = Arc::new(ReplayTransport::new(Vec::<String>::new()));
    let adapter = DeepSeekAdapter::new(config, transport.clone());

    let err = adapter
        .stream(&request(), &mut CollectingSink::default())
        .await
        .expect_err("no credential");

    assert!(
        matches!(err, LlmError::MissingCredential(ref env) if env == "TETANUS_TEST_ABSENT_KEY")
    );
    assert!(transport.last_body().is_none(), "no request was made");
}

/// TC-DS-LIVE-1: one real call against the configured endpoint.
/// Environmental needs: `DEEPSEEK_API_KEY` and network access. Without the key
/// the case reports itself skipped and passes, so the suite never depends on a
/// credential.
/// Expected: a non-empty answer, or a named provider/transport error.
#[tokio::test]
async fn live_call_when_a_key_is_present() {
    // Held across the call: the key must still be there when the adapter reads
    // it, not only when this case decided not to skip.
    let _environment = environment().await;
    let Ok(key) = std::env::var(DEFAULT_API_KEY_ENV) else {
        eprintln!("TC-DS-LIVE-1 skipped: {DEFAULT_API_KEY_ENV} is not set");
        return;
    };
    if key.is_empty() {
        eprintln!("TC-DS-LIVE-1 skipped: {DEFAULT_API_KEY_ENV} is empty");
        return;
    }

    let adapter = DeepSeekAdapter::with_http(DeepSeekConfig::default());
    let model = adapter.models().first().cloned().expect("catalog");
    let mut sink = CollectingSink::default();
    let request = ModelRequest {
        provider: PROVIDER.into(),
        model,
        messages: vec![Message::user("Reply with the single word: pong")],
        tools: Vec::new(),
        max_tokens: Some(16),
    };

    let response = adapter
        .stream(&request, &mut sink)
        .await
        .expect("live call");

    assert!(!response.content.is_empty(), "the provider answered");
    assert!(!sink.chunks.is_empty(), "the answer arrived as a stream");
}

/// TC-DS-WAIT-1: the delta-seconds form of `Retry-After` is a wait in
/// milliseconds.
///
/// Upstream: `adapter.spec.ts`, "retains status, Retry-After seconds, and
/// provider request id as structured facts". Upstream also keeps the provider
/// request id; tetanus carries no request id on a failure, so the wait is the
/// whole assertion.
///
/// Input: `2`, and the same value with the whitespace a header may arrive with.
/// Expected: `Some(2000.0)` for both. The clock is not read: a delta is
/// measured from the answer, not from a date.
#[test]
fn a_wait_in_seconds_is_read_in_milliseconds() {
    assert_eq!(retry_after_ms("2", 0.0), Some(2_000.0));
    assert_eq!(retry_after_ms(" 5 ", 1_800_000_000_000.0), Some(5_000.0));
}

/// TC-DS-WAIT-2: the date form of `Retry-After` names an instant, so the wait
/// is the distance from now to it.
///
/// Upstream: `adapter.spec.ts`, "parses a future Retry-After HTTP date and the
/// DeepSeek request-id fallback". Upstream spies on the clock; tetanus passes
/// the instant in, so the case states both sides of the subtraction.
///
/// Input: RFC 9110's own IMF-fixdate example, measured from the epoch and from
/// three seconds before the instant it names.
/// Expected: 784_111_777_000 ms and 3_000 ms. The first pins the calendar
/// arithmetic against a published value; the second pins the subtraction.
#[test]
fn a_wait_as_a_date_is_the_distance_to_it() {
    let example = "Sun, 06 Nov 1994 08:49:37 GMT";
    assert_eq!(retry_after_ms(example, 0.0), Some(784_111_777_000.0));
    assert_eq!(
        retry_after_ms(example, 784_111_777_000.0 - 3_000.0),
        Some(3_000.0)
    );
}

/// TC-DS-WAIT-3: a date that has already passed asks for nothing.
///
/// Upstream: `adapter.spec.ts`, "omits zero, non-finite, invalid, and past
/// Retry-After values" (the past-date value).
///
/// Input: the epoch itself, measured from a day later.
/// Expected: `None`. A wait that ended before it was read is not a wait, and
/// the policy is left on its own backoff rather than on a negative number.
#[test]
fn a_wait_that_has_already_passed_asks_for_nothing() {
    assert_eq!(
        retry_after_ms("Thu, 01 Jan 1970 00:00:00 GMT", 86_400_000.0),
        None
    );
}

/// TC-DS-WAIT-4: a value that is not a wait asks for nothing.
///
/// Upstream: `adapter.spec.ts`, "omits zero, non-finite, invalid, and past
/// Retry-After values", extended with the malformed dates a fixed-width parser
/// has to refuse.
///
/// Input: zero, a negative delta, 400 digits, empty, prose, a date with no
/// zone, an impossible day, an unknown month, and a truncated clock.
/// Expected: `None` for every one. Refusing an uninterpretable value costs one
/// local backoff; obeying it could park a route for as long as the value says.
#[test]
fn a_value_that_is_not_a_wait_asks_for_nothing() {
    for value in [
        "0",
        "-5",
        &"9".repeat(400),
        "",
        "not-a-date",
        "Sun, 06 Nov 1994 08:49:37",
        "Sun, 32 Nov 1994 08:49:37 GMT",
        "Sun, 06 Foo 1994 08:49:37 GMT",
        "Sun, 06 Nov 1994 08:49 GMT",
    ] {
        assert_eq!(retry_after_ms(value, 0.0), None, "{value}");
    }
}

/// TC-DS-WAIT-5: the wait a provider asked for reaches the policy that acts on
/// it.
///
/// The recovery policy reads [`RequestFailure`], not the error, so a wait the
/// adapter parsed and nobody carried would be a wait nobody honours.
///
/// Input: a provider failure carrying two seconds, and a transport failure.
/// Expected: `Some(2000.0)` on the first, `None` on the second - a request
/// that never reached the service has no header to have asked in.
#[test]
fn the_asked_wait_reaches_the_failure_a_policy_reads() {
    let asked = LlmError::Provider {
        status: 429,
        message: "slow down".into(),
        retry_after_ms: Some(2_000.0),
    };
    assert_eq!(
        RequestFailure::from(&asked).provider_retry_after_ms,
        Some(2_000.0)
    );
    assert_eq!(RequestFailure::from(&asked).code, "RATE_LIMIT");

    let unreached = LlmError::Transport("connection refused".into());
    assert_eq!(
        RequestFailure::from(&unreached).provider_retry_after_ms,
        None
    );
}

/// TC-DS-WAIT-6: a throttled answer over a real socket carries its status, its
/// words and the wait it asked for.
///
/// Upstream: `adapter.spec.ts`, "retains status, Retry-After seconds ... as
/// structured facts", against upstream's mock server. This is the whole path:
/// the production transport, a real HTTP response, and the header read before
/// the body is taken.
///
/// Environmental needs: a loopback port. No network and no credential.
/// Input: `HTTP 429` with `retry-after: 2` and a JSON error body.
/// Expected: `LlmError::Provider` with status 429, the body as the message,
/// and `Some(2000.0)`.
#[tokio::test]
async fn a_throttled_answer_carries_the_wait_it_asked_for() {
    let body = r#"{"error":{"message":"slow down"}}"#;
    let url = one_shot("429 Too Many Requests", "retry-after: 2\r\n", body).await;

    // `expect_err` wants a `Debug` stream; a boxed one is not, so the refusal
    // is taken by match instead.
    let err = match ReqwestTransport::default()
        .post_sse(&url, "test-key", serde_json::json!({}))
        .await
    {
        Err(err) => err,
        Ok(_) => panic!("the provider refused, so no stream should have opened"),
    };

    let LlmError::Provider {
        status,
        message,
        retry_after_ms,
    } = err
    else {
        panic!("expected a provider failure, got {err}");
    };
    assert_eq!(status, 429);
    assert_eq!(message, body);
    assert_eq!(retry_after_ms, Some(2_000.0));
}

/// TC-DS-WAIT-7: an answer that asked for nothing leaves the policy on its own
/// backoff.
///
/// Input: `HTTP 500` with no `Retry-After` header.
/// Expected: `LlmError::Provider` with status 500 and no wait. The absent
/// header must read as "the provider said nothing", not as zero, or a server
/// error would be retried with no pause at all.
#[tokio::test]
async fn an_answer_with_no_header_asks_for_nothing() {
    let url = one_shot("500 Internal Server Error", "", "upstream is down").await;

    // `expect_err` wants a `Debug` stream; a boxed one is not, so the refusal
    // is taken by match instead.
    let err = match ReqwestTransport::default()
        .post_sse(&url, "test-key", serde_json::json!({}))
        .await
    {
        Err(err) => err,
        Ok(_) => panic!("the provider failed, so no stream should have opened"),
    };

    assert!(
        matches!(err, LlmError::Provider { status: 500, ref retry_after_ms, .. } if retry_after_ms.is_none()),
        "{err}"
    );
}

/// One HTTP answer on a loopback port, for the cases that need the production
/// transport rather than a replay. It answers the first connection and then
/// drains it, so the client's own close is graceful and the answer is never
/// lost to a reset.
async fn one_shot(status_line: &str, headers: &str, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let url = format!(
        "http://{}/chat/completions",
        listener.local_addr().expect("the bound address")
    );
    let answer = format!(
        "HTTP/1.1 {status_line}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("one connection");
        let mut buffer = [0u8; 2048];
        let _ = socket.read(&mut buffer).await;
        socket
            .write_all(answer.as_bytes())
            .await
            .expect("the answer");
        socket.flush().await.expect("flushed");
        while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
    });
    url
}

const TOOL_CALL_STREAM: &[&str] = &[
    r#"{"choices":[{"index":0,"delta":{"reasoning_content":"thinking"}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"content":"Let me "}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"content":"echo that."}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"echo","arguments":"{\"text\":"}}]}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]}"#,
    r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#,
    "[DONE]",
];
