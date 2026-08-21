//! Test Design Specification: who may open a WebSocket connection.
//!
//! Feature under test: `tetanus_rpc::auth` and the handshake gate in
//! `websocket::serve_as`. Contract section 4.1.2 says the trust boundary is
//! the connection, because a peer that opens one can start turns, read every
//! journal and read the resolved configuration. This is the enforcement of
//! that.
//!
//! There is no upstream suite to port, and that is itself a finding:
//! `packages/host/webserver` performs no authentication and no origin check
//! and takes `'127.0.0.1' | '0.0.0.0'` as configuration. This is a deliberate
//! difference, recorded in `docs/parity.md`.
//!
//! Approach: the decision is a pure function of the peer address and what the
//! handshake presented, so most cases state those directly. Two drive a real
//! loopback socket end to end, because "refused before the upgrade" is a claim
//! about HTTP that only a real handshake can settle.
//!
//! Environmental needs: a loopback port. No case reaches a network or an API
//! key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::net::IpAddr;

use tetanus_rpc::auth::{Auth, Presented, Refusal, TOKEN_PROTOCOL_PREFIX, TOKEN_QUERY};

/// TC-WSAUTH-1: there is no posture that admits everyone.
///
/// The point of the module. `Auth::default()` is what a caller gets by
/// omission, and getting an open server by omission is exactly the defect this
/// closes - so the weakest posture available still refuses every off-box peer
/// and every browser origin.
#[test]
fn there_is_no_posture_that_admits_everyone() {
    let weakest = Auth::default();

    assert!(
        weakest.admit(local(), &nothing()).is_ok(),
        "a local tool works"
    );
    assert_eq!(
        weakest.admit(off_box(), &nothing()),
        Err(Refusal::NotLocal(off_box())),
        "and an off-box peer does not, by default"
    );
    assert_eq!(
        weakest.admit(local(), &from_origin("https://evil.example")),
        Err(Refusal::OriginNotAllowed("https://evil.example".into())),
        "and neither does a browser, even a local one"
    );
}

/// TC-WSAUTH-2: an off-box peer needs a token, which is the captain's
/// deployment.
///
/// The standing rule is that served surfaces bind `0.0.0.0` for off-box
/// access, so this is the expected deployment rather than a misconfiguration.
/// Under `require_token` the peer's address stops mattering: the token is the
/// whole decision, which is what makes binding off-box safe rather than
/// merely possible.
#[test]
fn an_off_box_peer_needs_a_token() {
    let auth = Auth::require_token("s3cret");

    assert!(auth.admit(off_box(), &with_token("s3cret")).is_ok());
    assert!(auth.admit(local(), &with_token("s3cret")).is_ok());

    assert_eq!(
        auth.admit(off_box(), &nothing()),
        Err(Refusal::TokenMissing)
    );
    assert_eq!(
        auth.admit(off_box(), &with_token("guess")),
        Err(Refusal::TokenWrong)
    );
    assert_eq!(
        auth.admit(local(), &nothing()),
        Err(Refusal::TokenMissing),
        "being local earns nothing once a token is required"
    );
}

/// TC-WSAUTH-3: a browser origin is refused until it is named, even on
/// loopback.
///
/// This is the attack that surprises people. The same-origin policy does not
/// restrict WebSocket connections the way it restricts `fetch`, so a page the
/// user merely visits can open `ws://127.0.0.1:<port>`. A rule that trusted
/// local peers would admit it, which is why `Origin` is checked in every
/// posture rather than only in the strict one.
///
/// A non-browser client sends no `Origin` and is unaffected; a browser cannot
/// forge one.
#[test]
fn a_browser_origin_is_refused_until_it_is_named() {
    for auth in [Auth::default(), Auth::require_token("s3cret")] {
        let presented = Presented {
            token: Some("s3cret".into()),
            origin: Some("https://evil.example".into()),
        };
        assert_eq!(
            auth.admit(local(), &presented),
            Err(Refusal::OriginNotAllowed("https://evil.example".into())),
            "a correct token does not excuse an unknown origin"
        );
    }

    let ui = Auth::default().allow_origin("https://ui.tetanus.local");
    assert!(ui
        .admit(local(), &from_origin("https://ui.tetanus.local"))
        .is_ok());
    assert_eq!(
        ui.admit(
            local(),
            &from_origin("https://ui.tetanus.local.evil.example")
        ),
        Err(Refusal::OriginNotAllowed(
            "https://ui.tetanus.local.evil.example".into()
        )),
        "an allowed origin is the whole string, never a prefix"
    );
}

/// TC-WSAUTH-4: a token arrives where a browser can put one.
///
/// A browser's WebSocket API cannot set request headers, so `Authorization` is
/// unavailable to the very client this carrier exists for. The subprotocol and
/// the URL are what it can set, and the subprotocol is preferred because a URL
/// reaches more logs than a header does.
#[test]
fn a_token_arrives_where_a_browser_can_put_one() {
    let from_protocol = Auth::present(Some(&format!("{TOKEN_PROTOCOL_PREFIX}abc123")), None);
    assert_eq!(from_protocol.token.as_deref(), Some("abc123"));

    let from_query = Auth::present(None, Some(&format!("{TOKEN_QUERY}=abc123")));
    assert_eq!(from_query.token.as_deref(), Some("abc123"));

    // Preferred, when a client sends both.
    let both = Auth::present(
        Some(&format!("{TOKEN_PROTOCOL_PREFIX}from-header")),
        Some(&format!("{TOKEN_QUERY}=from-url")),
    );
    assert_eq!(
        both.token.as_deref(),
        Some("from-header"),
        "the one that does not end up in an access log wins"
    );

    // A subprotocol list, which is what a browser actually sends.
    let listed = Auth::present(
        Some(&format!("chat, {TOKEN_PROTOCOL_PREFIX}abc123, other")),
        None,
    );
    assert_eq!(listed.token.as_deref(), Some("abc123"));

    // Nothing presented is nothing found, rather than an empty token that
    // might compare equal to an empty configured one.
    assert_eq!(Auth::present(None, None).token, None);
    assert_eq!(Auth::present(Some("chat"), Some("other=1")).token, None);
}

/// TC-WSAUTH-5: a wrong token is compared without returning early.
///
/// A peer that can connect can time a refusal precisely, and a comparison that
/// stopped at the first differing byte would let it recover the token one byte
/// at a time.
///
/// **This case does not catch that mutation, and the honest thing is to say
/// so.** Replacing the constant-time comparison with `==` leaves every
/// assertion here passing, because the difference is in how long a refusal
/// takes and not in what it answers. A timing assertion would be flaky on a
/// loaded runner and would still not prove the property. What is pinned is the
/// observable half - every wrong token earns the same refusal whatever it
/// shares with the right one - and the timing half is carried by the
/// implementation and its comment. `docs/parity.md` records the gap.
#[test]
fn a_wrong_token_is_refused_whatever_it_shares_with_the_right_one() {
    let auth = Auth::require_token("abcdefgh");

    // Sharing every byte but the last, or none at all, earns the same answer.
    for wrong in ["abcdefgX", "Xbcdefgh", "XXXXXXXX", "", "abcdefghi", "abc"] {
        assert_eq!(
            auth.admit(local(), &with_token(wrong)),
            Err(Refusal::TokenWrong),
            "{wrong:?}"
        );
    }
}

/// TC-WSAUTH-6: a refusal tells the operator what to fix and the peer almost
/// nothing.
///
/// The asymmetry is deliberate. A prober that could tell "no token" from
/// "wrong token" would learn whether it had found a server expecting the token
/// it was guessing, and one that could tell "wrong token" from "not local"
/// would learn the posture. A legitimate client learns nothing it needs: it
/// either has the token or it does not.
#[test]
fn a_refusal_tells_the_operator_more_than_the_peer() {
    let refusals = [
        Refusal::TokenMissing,
        Refusal::TokenWrong,
        Refusal::NotLocal(off_box()),
        Refusal::OriginNotAllowed("https://evil.example".into()),
    ];

    let statuses: Vec<u16> = refusals.iter().map(Refusal::status).collect();
    assert_eq!(
        statuses,
        [401, 401, 401, 401],
        "one status, so the peer cannot tell them apart"
    );

    // The operator's log does distinguish them, and names the specific thing.
    assert!(Refusal::NotLocal(off_box())
        .reason()
        .contains("203.0.113.7"));
    assert!(Refusal::OriginNotAllowed("https://evil.example".into())
        .reason()
        .contains("evil.example"));
    let mut reasons: Vec<String> = refusals.iter().map(Refusal::reason).collect();
    reasons.sort();
    reasons.dedup();
    assert_eq!(reasons.len(), 4, "four distinguishable reasons for the log");
}

fn local() -> IpAddr {
    IpAddr::from([127, 0, 0, 1])
}

fn off_box() -> IpAddr {
    IpAddr::from([203, 0, 113, 7])
}

fn nothing() -> Presented {
    Presented::default()
}

fn with_token(token: &str) -> Presented {
    Presented {
        token: Some(token.to_string()),
        origin: None,
    }
}

fn from_origin(origin: &str) -> Presented {
    Presented {
        token: None,
        origin: Some(origin.to_string()),
    }
}

// ------------------------------------------------- over a real handshake

/// TC-WSAUTH-7: a refused peer never reaches the JSON-RPC layer.
///
/// The placement is the point of the whole design, and it is a claim about
/// HTTP that only a real handshake can settle. Refusing at the upgrade means
/// an unauthenticated peer never sends a frame, which is why contract section
/// 4.1.2 adds no error code: there is nothing to answer.
///
/// Input: a server requiring a token, connected to with none, then with the
/// wrong one, then with the right one.
/// Expected: the first two fail during `connect_async` - before any frame is
/// written - and the third opens and answers a call.
#[tokio::test]
async fn a_refused_peer_never_reaches_the_json_rpc_layer() {
    let address = serve_with(Auth::require_token("s3cret")).await;

    for attempt in [address.clone(), format!("{address}/?{TOKEN_QUERY}=wrong")] {
        let refused = tokio_tungstenite::connect_async(&attempt).await;
        assert!(
            refused.is_err(),
            "the handshake must fail before the socket exists: {attempt}"
        );
    }

    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("{address}/?{TOKEN_QUERY}=s3cret"))
            .await
            .expect("the right token opens the connection");

    use futures_util::{SinkExt, StreamExt};
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"jsonrpc":"2.0","id":1,"method":"session.list","params":{}}"#.into(),
        ))
        .await
        .expect("send");
    let answered = socket.next().await.expect("a frame").expect("a message");
    assert!(
        answered.to_text().expect("text").contains("\"id\":1"),
        "and the engine is reachable once the peer is admitted"
    );
}

/// TC-WSAUTH-8: the default posture serves a local tool and refuses a browser.
///
/// The two halves of "default deny" that matter in practice: the developer
/// running a local client is not blocked, and the page they happen to have
/// open cannot drive their agent. The second is checked by sending the
/// `Origin` a browser would send, since a browser cannot be scripted here and
/// cannot forge that header there.
#[tokio::test]
async fn the_default_posture_serves_a_local_tool_and_refuses_a_browser() {
    let address = serve_with(Auth::default()).await;

    let (_socket, _) = tokio_tungstenite::connect_async(&address)
        .await
        .expect("a local tool sends no Origin and is admitted");

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = address.as_str().into_client_request().expect("a request");
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().expect("a header"));
    assert!(
        tokio_tungstenite::connect_async(request).await.is_err(),
        "a page the user is merely visiting must not drive the agent"
    );
}

/// A server on a loopback port under `auth`, and its `ws://` address.
async fn serve_with(auth: Auth) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let address = listener.local_addr().expect("the bound address");
    tokio::spawn(tetanus_rpc::websocket::serve_as(
        std::sync::Arc::new(Silent),
        listener,
        auth,
    ));
    format!("ws://{address}")
}

/// An engine that answers the one call these cases make.
struct Silent;

#[async_trait::async_trait]
impl tetanus_protocol::methods::Engine for Silent {
    async fn hello(
        &self,
        _: tetanus_protocol::methods::HelloParams,
    ) -> Result<tetanus_protocol::methods::HelloResult, tetanus_protocol::rpc::RpcError> {
        unreachable!("these cases do not greet")
    }
    async fn session_create(
        &self,
        _: tetanus_protocol::methods::SessionCreateParams,
    ) -> Result<tetanus_protocol::types::SessionInfo, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
    async fn session_list(
        &self,
    ) -> Result<tetanus_protocol::methods::SessionListResult, tetanus_protocol::rpc::RpcError> {
        Ok(tetanus_protocol::methods::SessionListResult {
            sessions: Vec::new(),
        })
    }
    async fn session_events(
        &self,
        _: tetanus_protocol::methods::SessionEventsParams,
    ) -> Result<tetanus_protocol::methods::SessionEventsResult, tetanus_protocol::rpc::RpcError>
    {
        unreachable!()
    }
    async fn session_subscribe(
        &self,
        _: tetanus_protocol::methods::SessionSubscribeParams,
        _: std::sync::Arc<dyn tetanus_protocol::methods::EventSink>,
    ) -> Result<tetanus_protocol::methods::SessionSubscribeResult, tetanus_protocol::rpc::RpcError>
    {
        unreachable!()
    }
    async fn session_unsubscribe(
        &self,
        _: tetanus_protocol::methods::SessionUnsubscribeParams,
    ) -> Result<tetanus_protocol::methods::Ack, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
    async fn agent_prompt(
        &self,
        _: tetanus_protocol::methods::AgentPromptParams,
    ) -> Result<tetanus_protocol::methods::AgentPromptResult, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
    async fn agent_status(
        &self,
        _: tetanus_protocol::methods::SessionRef,
    ) -> Result<tetanus_protocol::methods::AgentStatusResult, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
    async fn agent_interrupt(
        &self,
        _: tetanus_protocol::methods::SessionRef,
    ) -> Result<tetanus_protocol::methods::Ack, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
    async fn catalog_tools(
        &self,
    ) -> Result<tetanus_protocol::methods::ToolCatalogResult, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
    async fn catalog_models(
        &self,
    ) -> Result<tetanus_protocol::methods::ModelCatalogResult, tetanus_protocol::rpc::RpcError>
    {
        unreachable!()
    }
    async fn config_dump(
        &self,
    ) -> Result<tetanus_protocol::methods::ConfigDumpResult, tetanus_protocol::rpc::RpcError> {
        unreachable!()
    }
}
