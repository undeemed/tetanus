//! Test Design Specification: the product identity a model request carries.
//!
//! Features under test: upstream `packages/llm/llm/tests/attribution.spec.ts` -
//! the static identity, the `User-Agent` it renders to, and the header set a
//! request is attributed with. TC-PORT-ATTR-5 goes one step further than
//! upstream and pins that the header reaches a provider, because a rendered
//! string nothing sends attributes nothing.
//!
//! Approach: the first four cases are pure, against
//! `tetanus_turn::llm::attribution`. The fifth drives the real
//! [`DeepSeekAdapter`] over HTTP against a fake provider on a loopback port
//! and reads the request head that arrived.
//!
//! Features NOT tested here: what the adapter puts in the request *body*
//! (`upstream_deepseek_wire.rs`) and how a failed request is retried
//! (`upstream_transport_recovery.rs`). Upstream's identity is configurable per
//! call site through an options argument; tetanus has no configuration surface
//! for one, so `AppIdentity` is a parameter and the default is the only
//! identity anything constructs. Upstream also has no case for the header on
//! the wire, because its transport is not the unit under test there.
//!
//! Environmental needs: a loopback socket. No case reaches the network, and
//! none reads a real API key: the adapter is pointed at a made-up variable.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_turn::llm::attribution::{
    attribution_headers, user_agent, AppIdentity, PRODUCT, URL, USER_AGENT,
};
use tetanus_turn::llm::deepseek::{DeepSeekAdapter, DeepSeekConfig, PROVIDER};
use tetanus_turn::llm::{CollectingSink, LlmAdapter, Message, ModelRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The credential the fake provider is addressed with. A case never reads the
/// real one, and never leaves the real one's name holding a made-up value.
const TEST_API_KEY_ENV: &str = "TETANUS_TEST_ATTR_KEY";
const MODEL: &str = "mock-model";

/// A white-label identity exercising every field, as upstream's does.
fn fork() -> AppIdentity {
    AppIdentity {
        product: "fork-agent".to_string(),
        version: "9.9.9".to_string(),
        url: "https://example.com/fork-agent".to_string(),
    }
}

/// TC-PORT-ATTR-1: the identity is the product's public facts, and its version
/// comes from the manifest.
///
/// Upstream: "sources the version from the package manifest, never a
/// hand-copied constant" and "carries only static public product facts".
///
/// Input: `AppIdentity::default()`.
/// Expected: exactly the product name, the crate's own version and the project
/// URL. The version assertion is the one that matters: a hand-copied constant
/// passes on the day it is written and misreports the build for ever after.
#[test]
fn the_default_identity_is_the_product_read_off_the_manifest() {
    assert_eq!(
        AppIdentity::default(),
        AppIdentity {
            product: PRODUCT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            url: URL.to_string(),
        }
    );
}

/// TC-PORT-ATTR-2: the rendered form is a product token with a URL comment.
///
/// Upstream: "renders product/version with the +url comment".
///
/// Input: the default identity.
/// Expected: `tetanus/<version> (+https://github.com/undeemed/tetanus)`,
/// spelled out here rather than rebuilt from the same helper, so a change to
/// the format is a change to this case.
#[test]
fn the_default_identity_renders_as_a_product_token() {
    assert_eq!(
        user_agent(&AppIdentity::default()),
        format!(
            "tetanus/{} (+https://github.com/undeemed/tetanus)",
            env!("CARGO_PKG_VERSION")
        )
    );
}

/// TC-PORT-ATTR-3: a fork renders as itself.
///
/// Upstream: "renders a custom identity".
///
/// Input: an identity naming another product, version and URL.
/// Expected: `fork-agent/9.9.9 (+https://example.com/fork-agent)`. Nothing of
/// this product leaks into a request a fork sends.
#[test]
fn a_fork_renders_as_its_own_product() {
    assert_eq!(
        user_agent(&fork()),
        "fork-agent/9.9.9 (+https://example.com/fork-agent)"
    );
}

/// TC-PORT-ATTR-4: attribution is one header, whoever is asking.
///
/// Upstream: "defaults to the provider-neutral baseline: User-Agent and
/// nothing else" and "maps a custom identity onto the User-Agent header only".
///
/// Input: the default identity, then the fork's.
/// Expected: a map of exactly one entry, `user-agent`, holding that identity's
/// rendered form. The count is the assertion: a second header would be a fact
/// about the caller that nobody agreed to send.
#[test]
fn attribution_is_the_user_agent_and_nothing_else() {
    for identity in [AppIdentity::default(), fork()] {
        let headers = attribution_headers(&identity);
        assert_eq!(
            headers.keys().collect::<Vec<_>>(),
            vec![USER_AGENT],
            "{identity:?} sent more than the baseline"
        );
        assert_eq!(headers[USER_AGENT], user_agent(&identity));
    }
}

/// TC-PORT-ATTR-5: the header is on the request a provider receives.
///
/// Not upstream: its transport is not the unit under test in that file. It is
/// here because the four cases above pass whether or not anything sends what
/// they render.
///
/// Input: the real `DeepSeekAdapter` over HTTP against a fake provider on a
/// loopback port, answering one complete stream.
/// Expected: the request head carries `user-agent:` with the default
/// identity's rendered form, and carries it once.
#[tokio::test]
async fn the_provider_receives_the_user_agent() {
    std::env::set_var(TEST_API_KEY_ENV, "mock-key");
    let (port, head) = provider().await;

    let adapter = DeepSeekAdapter::with_http(DeepSeekConfig {
        api_key_env: TEST_API_KEY_ENV.to_string(),
        base_url: format!("http://127.0.0.1:{port}"),
        models: vec![MODEL.to_string()],
        max_tokens: None,
        ..DeepSeekConfig::default()
    });
    let mut sink = CollectingSink::default();
    let answered = adapter
        .stream(&request(), &mut sink)
        .await
        .expect("the fake provider answered");
    assert_eq!(answered.content, "hi");

    let sent = head.await.expect("the request head was read");
    let carried: Vec<&str> = sent
        .lines()
        .filter(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
        .collect();
    assert_eq!(
        carried,
        vec![format!(
            "user-agent: {}",
            user_agent(&AppIdentity::default())
        )],
        "the request head was:\n{sent}"
    );
}

fn request() -> ModelRequest {
    ModelRequest {
        provider: PROVIDER.to_string(),
        model: MODEL.to_string(),
        messages: vec![Message::user("say hi")],
        tools: Vec::new(),
        max_tokens: None,
    }
}

/// A fake provider that answers one complete stream and hands back the request
/// head it was sent. It reads only the head, because that is what this case
/// asserts on and the body is another file's business.
async fn provider() -> (u16, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let served = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut raw = Vec::new();
        let mut buffer = [0u8; 1024];
        while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.expect("read");
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..read]);
        }
        let body = format!(
            "{}{}{}",
            frame(r#"{"choices":[{"delta":{"content":"hi"}}]}"#),
            frame(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            "data: [DONE]\n\n"
        );
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .expect("write");
        socket.flush().await.expect("flush");
        String::from_utf8_lossy(&raw).to_string()
    });
    (port, served)
}

fn frame(data: &str) -> String {
    format!("data: {data}\n\n")
}
