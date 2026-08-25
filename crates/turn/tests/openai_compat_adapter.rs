//! Test Design Specification: the route-named OpenAI-compatible adapter.
//!
//! Feature under test: [`OpenAiCompatAdapter`], the wrapper that answers to a
//! route a settings document named while the wire format, the credential
//! lookup and the two timeouts stay the wrapped DeepSeek adapter's. The claim
//! being defended is a narrow one and it is the whole point of the wrapper:
//! **only the route changes**. Everything else has to be the same call the
//! DeepSeek adapter would have made, byte for byte, or a deployment
//! configuring `llm.providers.<name>` is running a second, quietly different
//! transport.
//!
//! Features NOT tested here: the SSE decoder, the wire body shape, the retry
//! classification of a failure and the `Retry-After` reading - all of them
//! belong to the wrapped adapter and are covered by `deepseek_adapter.rs`. A
//! second copy of those cases here would assert the wrapper's delegation twice
//! and the behaviour once.
//!
//! Environmental needs: none. No case opens a socket: the production
//! constructor is exercised for what it builds, never for a call it would
//! make.
//!
//! The environment is process-wide, so every case that reads or writes it
//! holds [`environment`] while it does, and writes a variable no other case
//! reads - the discipline `deepseek_adapter.rs` states and for the same
//! reason.

use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard};

use tetanus_turn::llm::deepseek::{DeepSeekConfig, FrameStream, SseTransport, PROVIDER};
use tetanus_turn::llm::openai_compat::OpenAiCompatAdapter;
use tetanus_turn::llm::{CollectingSink, LlmAdapter, LlmError, Message, ModelRequest};

/// A credential variable only this file reads.
const TEST_API_KEY_ENV: &str = "TETANUS_TEST_OPENAI_COMPAT_KEY";

/// The route a document names, spelled so nothing could confuse it with the
/// wrapped adapter's own.
const ROUTE: &str = "acme-gateway";

static ENVIRONMENT: Mutex<()> = Mutex::const_new(());

async fn environment() -> MutexGuard<'static, ()> {
    ENVIRONMENT.lock().await
}

/// One complete completion, in the provider's own frames.
const STREAM: [&str; 3] = [
    r#"{"choices":[{"delta":{"content":"hello from "}}]}"#,
    r#"{"choices":[{"delta":{"content":"the gateway"},"finish_reason":"stop"}]}"#,
    "[DONE]",
];

/// A transport that answers with [`STREAM`] and remembers everything it was
/// asked with.
///
/// `ReplayTransport` in the crate keeps the body and drops the url and the
/// key, and those two are exactly what this file has to see: the wrapper must
/// not have moved the endpoint or the credential while it renamed the route.
struct Watched {
    seen: std::sync::Mutex<Option<(String, String, serde_json::Value)>>,
}

impl Watched {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: std::sync::Mutex::new(None),
        })
    }

    fn asked(&self) -> (String, String, serde_json::Value) {
        self.seen
            .lock()
            .expect("seen lock")
            .clone()
            .expect("a call")
    }
}

#[async_trait::async_trait]
impl SseTransport for Watched {
    async fn post_sse(
        &self,
        url: &str,
        api_key: &str,
        body: serde_json::Value,
    ) -> Result<FrameStream, LlmError> {
        *self.seen.lock().expect("seen lock") = Some((url.into(), api_key.into(), body));
        let frames: Vec<Result<String, LlmError>> =
            STREAM.iter().map(|frame| Ok(frame.to_string())).collect();
        Ok(Box::pin(futures_util::stream::iter(frames)))
    }
}

fn config() -> DeepSeekConfig {
    DeepSeekConfig {
        api_key_env: TEST_API_KEY_ENV.into(),
        base_url: "https://gateway.example/v1".into(),
        models: vec!["acme/small".into(), "acme/large".into()],
        max_tokens: Some(64),
        stream_idle_timeout_ms: 1_000,
        request_deadline_ms: 2_000,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        provider: ROUTE.into(),
        model: "acme/small".into(),
        messages: vec![Message::user("say hello")],
        tools: Vec::new(),
        max_tokens: None,
    }
}

/// TC-OAI-1: the wrapper answers to the route it was named with, and not to
/// the wrapped adapter's.
///
/// Expected: `provider()` is `acme-gateway`; the wrapped constant
/// `deepseek-official` appears nowhere. This is the one behavioural difference
/// the type exists to make, so it is asserted on its own rather than as a
/// clause of a longer case.
#[test]
fn the_route_is_the_name_it_was_built_with() {
    let adapter = OpenAiCompatAdapter::new(ROUTE.into(), config(), Watched::new());

    assert_eq!(adapter.provider(), ROUTE);
    assert_eq!(adapter.route(), ROUTE);
    assert_ne!(adapter.provider(), PROVIDER);
}

/// TC-OAI-2: the catalogue and the credential reference are the wrapped
/// adapter's, unchanged.
///
/// Expected: `models()` is the configured list in order, `credential_env()` is
/// the configured variable, and `config()` reads back what it was built with -
/// including the two bounds, which a wrapper that rebuilt the config rather
/// than holding it would silently reset to the compiled defaults.
#[test]
fn the_catalogue_and_the_credential_pass_through() {
    let adapter = OpenAiCompatAdapter::new(ROUTE.into(), config(), Watched::new());

    assert_eq!(adapter.models(), vec!["acme/small", "acme/large"]);
    assert_eq!(adapter.credential_env(), Some(TEST_API_KEY_ENV));
    assert_eq!(adapter.config().base_url, "https://gateway.example/v1");
    assert_eq!(adapter.config().max_tokens, Some(64));
    assert_eq!(adapter.config().idle_window().as_millis(), 1_000);
    assert_eq!(adapter.config().deadline().as_millis(), 2_000);
}

/// TC-OAI-3: a stream through the wrapper is the wrapped adapter's own call.
///
/// Expected: the transport is asked for `{base_url}/chat/completions` with the
/// key the configured variable holds and the wrapped adapter's own body, the
/// replayed chunks reach the sink, and the assembled response carries the text
/// and the finish reason. The endpoint assertion is the one that matters
/// most: a wrapper that had built its own url would work against every
/// provider that happens to share DeepSeek's path and fail against the rest.
#[tokio::test]
async fn a_stream_delegates_to_the_wrapped_transport() {
    let _environment = environment().await;
    std::env::set_var(TEST_API_KEY_ENV, "sk-gateway-key");

    let transport = Watched::new();
    let adapter = OpenAiCompatAdapter::new(ROUTE.into(), config(), transport.clone());
    let mut sink = CollectingSink::default();

    let response = adapter.stream(&request(), &mut sink).await.expect("stream");

    let (url, key, body) = transport.asked();
    assert_eq!(url, "https://gateway.example/v1/chat/completions");
    assert_eq!(key, "sk-gateway-key");
    assert_eq!(body["model"], "acme/small");
    // The adapter-configured cap reaches the wire, which is the wrapped
    // adapter reading the config this wrapper handed it.
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["stream"], true);

    assert_eq!(response.content, "hello from the gateway");
    assert_eq!(response.finish_reason, "stop");
    assert!(
        !sink.chunks.is_empty(),
        "the replayed chunks reached a sink"
    );

    std::env::remove_var(TEST_API_KEY_ENV);
}

/// TC-OAI-4: a route whose credential variable is empty fails before the
/// transport is reached.
///
/// Expected: `MissingCredential` naming the variable, and the transport was
/// never called. The wrapper adds no credential path of its own, so what this
/// pins is that it did not accidentally bypass the wrapped adapter's.
#[tokio::test]
async fn a_route_with_no_credential_never_reaches_the_transport() {
    let _environment = environment().await;
    std::env::remove_var(TEST_API_KEY_ENV);

    let transport = Watched::new();
    let adapter = OpenAiCompatAdapter::new(ROUTE.into(), config(), transport.clone());
    let mut sink = CollectingSink::default();

    let error = adapter
        .stream(&request(), &mut sink)
        .await
        .expect_err("no key, no call");

    assert!(
        matches!(&error, LlmError::MissingCredential(named) if named == TEST_API_KEY_ENV),
        "{error:?}"
    );
    assert!(
        transport.seen.lock().expect("seen lock").is_none(),
        "nothing should have been sent"
    );
}

/// TC-OAI-5: the production constructor builds the same route over a real HTTP
/// transport.
///
/// Expected: `with_http` answers the same route, catalogue and credential
/// reference as the test constructor, with the configured bounds intact. No
/// request is made: what is being checked is that the one line composing the
/// production wiring is reached and composes the config it was given, which is
/// the difference between a constructor that works and one that is never
/// compiled against.
#[test]
fn the_production_constructor_carries_the_same_route_and_config() {
    let adapter = OpenAiCompatAdapter::with_http(ROUTE.into(), config());

    assert_eq!(adapter.provider(), ROUTE);
    assert_eq!(adapter.models(), vec!["acme/small", "acme/large"]);
    assert_eq!(adapter.credential_env(), Some(TEST_API_KEY_ENV));
    assert_eq!(adapter.config().base_url, "https://gateway.example/v1");
    assert_eq!(adapter.config().idle_window().as_millis(), 1_000);
    assert_eq!(adapter.config().deadline().as_millis(), 2_000);
}
