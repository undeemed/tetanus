//! Test Design Specification: the HTTP route carrier.
//!
//! Features tested: the fixed match order (exact, then longest prefix, then
//! the one fallback); that a duplicate path is refused at registration rather
//! than shadowed at request time; that dropping a registration frees the seat;
//! that the fallback seat is single-owner and that an unclaimed carrier
//! answers 404; that an upgrade is matched on its own table and on the header
//! rather than the pathname; and the two addresses this server will bind.
//!
//! For the frontend on that seat: that a file is served as what it is and an
//! unknown extension as bytes; that a miss is the page with 200 rather than a
//! 404; that nothing outside the dist root is served, written, escaped or
//! symlinked; that a write which reached the fallback is 405; that every index
//! response runs through the taps in order; and that the seat is given back.
//!
//! Features NOT tested here: what any other route answers with - that belongs
//! to whoever registered it.
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

/// A dist directory with an index, an asset and a file of an unknown kind.
fn dist() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("index.html"), "<html>the page</html>").expect("index");
    std::fs::create_dir_all(dir.path().join("assets")).expect("assets");
    std::fs::write(dir.path().join("assets/app.js"), "console.log(1)").expect("js");
    std::fs::write(dir.path().join("assets/thing.bin"), [0_u8, 1, 2]).expect("bin");
    dir
}

/// TC-HOST-STATIC-1: a file that is there, and one whose extension this table
/// does not know.
/// Expected: the asset with its own type; the unknown one as
/// `application/octet-stream`. A guess is worse than a download.
#[tokio::test]
async fn a_file_is_served_as_what_it_is_or_as_bytes() {
    let dir = dist();
    let (server, listener) = carrier().await;
    let address = server.address();
    let _seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("the seat is free");
    tokio::spawn(server.serve(listener));

    let js = ask(address, "GET /assets/app.js HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(js.contains("text/javascript"), "{js}");
    assert!(js.contains("console.log(1)"), "{js}");

    let bin = ask(address, "GET /assets/thing.bin HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(bin.contains("application/octet-stream"), "{bin}");
}

/// TC-HOST-STATIC-2: a path that is no file at all.
/// Expected: `index.html`, with 200. Not 404 and not a redirect: the router in
/// the page decides whether `/sessions/17` means anything, and it cannot
/// decide if the server answered first.
#[tokio::test]
async fn a_miss_is_the_page_and_not_a_404() {
    let dir = dist();
    let (server, listener) = carrier().await;
    let address = server.address();
    let _seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("free");
    tokio::spawn(server.serve(listener));

    let said = ask(address, "GET /sessions/17 HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.starts_with("HTTP/1.1 200"), "{said}");
    assert!(said.contains("the page"), "{said}");
}

/// TC-HOST-STATIC-3: three ways of asking for something outside the root -
/// written, escaped, and through a symlink.
/// Expected: 403 for each. The check is on the resolved path, because `%2e%2e`
/// and a symlink both spell `..` without writing it.
#[tokio::test]
async fn nothing_outside_the_frontend_is_served() {
    let dir = dist();
    let secret = dir.path().parent().expect("a parent").join("secret.txt");
    std::fs::write(&secret, "not yours").expect("the file outside");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, dir.path().join("link.txt")).expect("the symlink");

    let (server, listener) = carrier().await;
    let address = server.address();
    let _seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("free");
    tokio::spawn(server.serve(listener));

    for path in ["/../secret.txt", "/%2e%2e/secret.txt", "/link.txt"] {
        let said = ask(address, &format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n")).await;
        assert!(said.starts_with("HTTP/1.1 403"), "{path}: {said}");
        assert!(
            !said.contains("not yours"),
            "{path} leaked the file: {said}"
        );
    }
    let _ = std::fs::remove_file(secret);
}

/// TC-HOST-STATIC-4: a POST to a path no named route claimed.
/// Expected: 405 with an `allow` header, not the page. A caller posting to an
/// API that is not there must not be told it worked.
#[tokio::test]
async fn a_write_that_reached_the_fallback_is_refused() {
    let dir = dist();
    let (server, listener) = carrier().await;
    let address = server.address();
    let _seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("free");
    tokio::spawn(server.serve(listener));

    let said = ask(
        address,
        "POST /api/models HTTP/1.1\r\nhost: x\r\ncontent-length: 0\r\n\r\n",
    )
    .await;
    assert!(said.starts_with("HTTP/1.1 405"), "{said}");
    assert!(said.to_lowercase().contains("allow: get, head"), "{said}");
}

/// TC-HOST-STATIC-5: two taps registered, then one dropped.
/// Expected: every index response carries both transforms, in the order they
/// were added; after the drop, only the other one. This is how the boot
/// manifest reaches a page that knows nothing about the assembly.
#[tokio::test]
async fn every_index_response_runs_through_the_taps() {
    let dir = dist();
    let (server, listener) = carrier().await;
    let address = server.address();
    let _seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("free");
    let first = server.tap_index(Arc::new(|html| {
        html.replace("</html>", "<b>one</b></html>")
    }));
    let _second = server.tap_index(Arc::new(|html| {
        html.replace("</html>", "<b>two</b></html>")
    }));
    tokio::spawn(server.clone().serve(listener));

    let said = ask(address, "GET / HTTP/1.1\r\nhost: x\r\n\r\n").await;
    let one = said.find("one").expect("the first tap ran");
    let two = said.find("two").expect("the second tap ran");
    assert!(one < two, "the taps ran out of order: {said}");

    drop(first);
    let said = ask(address, "GET / HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(!said.contains("one"), "a dropped tap still ran: {said}");
    assert!(said.contains("two"), "{said}");
}

/// TC-HOST-STATIC-6: the seat, claimed and released.
/// Expected: a second mount is refused while the first holds it, and the
/// carrier answers 404 again once the guard is dropped. The seat is
/// effect-scoped, so a frontend that goes away does not leave a page behind.
#[tokio::test]
async fn the_frontend_holds_one_seat_and_gives_it_back() {
    let dir = dist();
    let (server, listener) = carrier().await;
    let address = server.address();
    let seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("free");
    tokio::spawn(server.clone().serve(listener));

    assert_eq!(
        Frontend::mount(&server, &dir.path().join("index.html")).err(),
        Some(Taken::Fallback)
    );

    drop(seat);
    let said = ask(address, "GET /whatever HTTP/1.1\r\nhost: x\r\n\r\n").await;
    assert!(said.starts_with("HTTP/1.1 404"), "{said}");
}
