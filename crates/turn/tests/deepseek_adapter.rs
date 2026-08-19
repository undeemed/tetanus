//! Test Design Specification: the DeepSeek chat-completions adapter.
//!
//! Feature under test: wire serialization and SSE stream decoding for the
//! provider route `deepseek-official`, exercised through a replaying transport.
//! Features NOT tested here: TLS, retry policy, thinking-mode configuration and
//! the credential seam - Phase ② concerns.
//!
//! Environmental needs: none. TC-DS-LIVE-1 additionally needs `DEEPSEEK_API_KEY`
//! and network, and reports itself skipped without them.

use std::sync::Arc;

use tetanus_turn::llm::deepseek::{
    take_frames, wire_request, DeepSeekAdapter, DeepSeekConfig, ReplayTransport, StreamDecoder,
    DEFAULT_API_KEY_ENV, PROVIDER, PUBLIC_BASE_URL,
};
use tetanus_turn::llm::{CollectingSink, LlmAdapter, LlmError, Message, ModelRequest, StreamChunk};
use tetanus_turn::tools::{ToolCall, ToolSchema};

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

/// TC-DS-ADAPTER-1: end to end through the replaying transport, the adapter
/// streams chunks into the sink and returns the assembled response.
/// Expected: the sink sees four chunks in stream order; the transport received
/// the serialized body; the response carries the tool call.
#[tokio::test]
async fn streams_through_the_transport_seam() {
    // Safety: single-threaded test process section; the adapter reads the key
    // from the environment exactly as it does in production.
    std::env::set_var(DEFAULT_API_KEY_ENV, "sk-test-key");

    let transport = Arc::new(ReplayTransport::new(TOOL_CALL_STREAM.iter().copied()));
    let adapter = DeepSeekAdapter::new(DeepSeekConfig::default(), transport.clone());
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

    std::env::remove_var(DEFAULT_API_KEY_ENV);
}

/// TC-DS-CRED-1: with no key anywhere the call fails before any network I/O,
/// naming the configuration entry point and never any part of a key.
/// Expected: `LlmError::MissingCredential("TETANUS_TEST_ABSENT_KEY")`.
#[tokio::test]
async fn a_missing_key_fails_before_network_io() {
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

const TOOL_CALL_STREAM: &[&str] = &[
    r#"{"choices":[{"index":0,"delta":{"reasoning_content":"thinking"}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"content":"Let me "}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"content":"echo that."}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"echo","arguments":"{\"text\":"}}]}}]}"#,
    r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]}"#,
    r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#,
    "[DONE]",
];
