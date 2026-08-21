//! Test Design Specification: the HTTP route carrier.
//!
//! Features tested: the fixed match order (exact, then longest prefix, then
//! the one fallback); that a duplicate path is refused at registration rather
//! than shadowed at request time; that dropping a registration frees the seat;
//! that the fallback seat is single-owner and that an unclaimed carrier
//! answers 404; that an upgrade is matched on its own table and on the header
//! rather than the pathname; and the two addresses this server will bind.
//!
//! Features NOT tested here: what any route answers with - that belongs to
//! whoever registered it - and the static frontend's own semantics, which are
//! its package's cases.
//!
//! Environmental needs: a loopback port the operating system picks. No case
//! binds a fixed port, so the suite runs beside anything.

use std::sync::Arc;

use tokio::io::AsyncWriteExt;

use super::*;

/// A handler that says which route answered.
fn says(what: &'static str) -> Handler {
    Arc::new(move |_| Response::text(Status::Ok, what))
}

/// A carrier bound on a port nobody chose.
async fn carrier() -> (WebServer, TcpListener) {
    WebServer::bind("127.0.0.1", 0).await.expect("it binds")
}

/// Ask a running carrier for a path, and read the answer whole.
async fn ask(address: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).await.expect("it connects");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request is written");
    let mut said = Vec::new();
    stream
        .read_to_end(&mut said)
        .await
        .expect("the answer is read");
    String::from_utf8_lossy(&said).to_string()
}

/// TC-HOST-WEB-1: an exact route and a prefix route that both match.
/// Expected: the exact one answers. The order is the contract: a table where
/// the answer depended on which plugin started first is a table nobody can
/// compose against.
#[tokio::test]
async fn an_exact_route_beats_a_prefix_that_also_matches() {
    let (server, listener) = carrier().await;
    let address = server.address();
    let _exact = server
        .register(Pattern::Exact, "/api", says("exact"))
        .expect("the seat is free");
    let _prefix = server
        .register(Pattern::Prefix, "/api", says("prefix"))
        .expect("a prefix is a different seat");
    tokio::spawn(server.serve(listener));

    let said = ask(address, "GET /api HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.contains("exact"), "{said}");
}

/// TC-HOST-WEB-2: two prefixes that both match one path.
/// Expected: the longer one answers, so `/api/v2` can be somebody else's while
/// `/api` is registered.
#[tokio::test]
async fn the_longest_prefix_answers() {
    let (server, listener) = carrier().await;
    let address = server.address();
    let _short = server
        .register(Pattern::Prefix, "/api", says("short"))
        .expect("free");
    let _long = server
        .register(Pattern::Prefix, "/api/v2", says("long"))
        .expect("free");
    tokio::spawn(server.serve(listener));

    let said = ask(address, "GET /api/v2/models HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.contains("long"), "{said}");
}

/// TC-HOST-WEB-3: a path that matches no named route.
/// Expected: the fallback answers if one is claimed, and 404 while none is.
/// An unclaimed carrier answering 404 is what makes the static frontend's
/// absence readable rather than a hang.
#[tokio::test]
async fn what_matches_nothing_goes_to_the_fallback_or_to_404() {
    let (server, listener) = carrier().await;
    let address = server.address();
    tokio::spawn(server.clone().serve(listener));

    let said = ask(address, "GET /whatever HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.starts_with("HTTP/1.1 404"), "{said}");

    let _seat = server
        .register_fallback(says("the page"))
        .expect("the seat is free");
    let said = ask(address, "GET /whatever HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.contains("the page"), "{said}");
}

/// TC-HOST-WEB-4: two owners for one path, and two claims on the fallback.
/// Expected: both refused, at registration. Two owners of one path is a
/// misconfiguration of the assembly, and the moment to say so is composition
/// rather than the first request that goes to the wrong one.
#[tokio::test]
async fn one_path_has_one_owner() {
    let (server, _listener) = carrier().await;
    let _first = server
        .register(Pattern::Exact, "/api", says("first"))
        .expect("free");

    assert_eq!(
        server
            .register(Pattern::Exact, "/api", says("second"))
            .err(),
        Some(Taken::Route("/api".into()))
    );

    let _seat = server.register_fallback(says("page")).expect("free");
    assert_eq!(
        server.register_fallback(says("another")).err(),
        Some(Taken::Fallback)
    );
}

/// TC-HOST-WEB-5: a registration dropped.
/// Expected: the seat is free again, and the path answers 404. A plugin that
/// goes away takes its routes with it; a seat nobody holds is one the next
/// owner can claim.
#[tokio::test]
async fn dropping_a_registration_frees_the_seat() {
    let (server, listener) = carrier().await;
    let address = server.address();
    tokio::spawn(server.clone().serve(listener));

    let seat = server
        .register(Pattern::Exact, "/gone", says("here"))
        .expect("free");
    assert!(ask(address, "GET /gone HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .contains("here"));

    drop(seat);
    let said = ask(address, "GET /gone HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.starts_with("HTTP/1.1 404"), "{said}");

    // And the path can be claimed again.
    let _again = server
        .register(Pattern::Exact, "/gone", says("again"))
        .expect("the seat came back");
}

/// TC-HOST-WEB-6: an upgrade, and an ordinary GET of the same path.
/// Expected: the upgrade table answers the upgrade; the ordinary GET does not
/// reach it. A table that guessed from the pathname would hand a browser's
/// plain GET of `/ws` to a socket handler and hang it.
#[tokio::test]
async fn an_upgrade_is_told_by_its_header_and_not_by_its_path() {
    let (server, listener) = carrier().await;
    let address = server.address();
    let _socket = server
        .register_upgrade("/ws", says("the socket"))
        .expect("free");
    tokio::spawn(server.serve(listener));

    let upgraded = ask(
        address,
        "GET /ws HTTP/1.1\r\nhost: x\r\nupgrade: websocket\r\nconnection: Upgrade\r\n\r\n",
    )
    .await;
    assert!(upgraded.contains("the socket"), "{upgraded}");

    let plain = ask(address, "GET /ws HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(plain.starts_with("HTTP/1.1 404"), "{plain}");
}

/// TC-HOST-WEB-7: the addresses this server will bind.
/// Expected: loopback and the wildcard, and nothing else. There is no TLS
/// here, no authentication and no origin policy, so a third address would read
/// as an option this server has thought about.
#[tokio::test]
async fn it_binds_loopback_and_the_wildcard_and_nothing_else() {
    assert!(WebServer::bind("127.0.0.1", 0).await.is_ok());
    assert!(WebServer::bind("0.0.0.0", 0).await.is_ok());

    let refused = match WebServer::bind("192.168.1.10", 0).await {
        Err(refused) => refused,
        Ok(_) => panic!("192.168.1.10 is not an address this server binds"),
    };
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert!(
        refused.to_string().contains("127.0.0.1 or 0.0.0.0"),
        "{refused}"
    );
}

/// TC-HOST-WEB-8: bytes that are not a request, and a head that will not end.
/// Expected: 400 in both cases, and the process still serving. A socket held
/// open for a reply that never comes is worse than a refusal.
#[tokio::test]
async fn a_head_this_carrier_will_not_parse_is_refused() {
    let (server, listener) = carrier().await;
    let address = server.address();
    tokio::spawn(server.serve(listener));

    let said = ask(address, "this is not HTTP\r\n\r\n").await;
    assert!(said.starts_with("HTTP/1.1 400"), "{said}");

    let long = format!("GET /{} HTTP/1.1\r\nhost: x\r\n\r\n", "a".repeat(20_000));
    let said = ask(address, &long).await;
    assert!(said.starts_with("HTTP/1.1 400"), "{said}");
}
