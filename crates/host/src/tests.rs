//! Test Design Specification: the HTTP route carrier.
//!
//! Features tested: the fixed match order (exact, then longest prefix, then
//! the one fallback); that a duplicate path is refused at registration rather
//! than shadowed at request time; that dropping a registration frees the seat;
//! that the fallback seat is single-owner and that an unclaimed carrier
//! answers 404; that an upgrade is matched on its own table and on the header
//! rather than the pathname; that an upgrade handler is handed the socket
//! with nothing read off it; that a server asked to stop stops accepting and
//! tells the handlers holding sockets; and the two addresses this server will
//! bind.
//!
//! For the directory picker: that a listing is directories only, name-sorted,
//! with dead links left out and hidden reported rather than applied; that the
//! crumbs are the chain from the root; that a level past the bound is cut and
//! says so; that creation is one directory and never a tree; and that a path
//! which is not fully qualified is refused.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;

use super::*;

/// A handler that says which route answered.
fn says(what: &'static str) -> Handler {
    answered(move |_| Response::text(Status::Ok, what))
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
    // The handler is handed the socket with nothing read off it, so what it
    // sees first is the request line the client sent: this one reads it back
    // to prove it, then says so.
    let _socket = server
        .register_upgrade(
            "/ws",
            Arc::new(|mut stream, _| {
                tokio::spawn(async move {
                    // Read the whole head the way a handshake would, so the
                    // socket is empty before it is closed: a close with bytes
                    // still unread sends RST and throws away the reply.
                    let mut seen = Vec::new();
                    let mut byte = [0_u8; 256];
                    while !seen.windows(4).any(|four| four == b"\r\n\r\n") {
                        match stream.read(&mut byte).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => seen.extend_from_slice(&byte[..read]),
                        }
                    }
                    let said = match String::from_utf8_lossy(&seen).starts_with("GET /ws") {
                        true => "the socket, with its own head",
                        false => "the head was eaten",
                    };
                    let _ = stream.write_all(said.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }),
        )
        .expect("free");
    tokio::spawn(server.serve(listener));

    let upgraded = ask(
        address,
        "GET /ws HTTP/1.1\r\nhost: x\r\nupgrade: websocket\r\nconnection: Upgrade\r\n\r\n",
    )
    .await;
    assert!(
        upgraded.contains("the socket, with its own head"),
        "{upgraded}"
    );

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

/// TC-HOST-STATIC-7: the boot manifest, in a page, with a value that would
/// close the script tag.
/// Expected: the manifest lands inside the head, before the page's own script,
/// and `<` is escaped so a carrier address containing `</script>` arrives as
/// data rather than as markup.
#[tokio::test]
async fn the_manifest_reaches_the_page_as_data() {
    let dir = dist();
    std::fs::write(
        dir.path().join("index.html"),
        "<html><head><title>x</title></head><body></body></html>",
    )
    .expect("index");
    let (server, listener) = carrier().await;
    let address = server.address();
    let _seat = Frontend::mount(&server, &dir.path().join("index.html")).expect("free");
    let _manifest = server.tap_index(
        Manifest {
            carrier: "ws://127.0.0.1:9/</script><b>".into(),
            protocol: "1.0".into(),
            token: None,
        }
        .tap(),
    );
    tokio::spawn(server.serve(listener));

    let said = ask(address, "GET / HTTP/1.1\r\nhost: x\r\n\r\n").await;
    let head = said.find("</head>").expect("a head");
    let boot = said.find("TETANUS_BOOT").expect("the manifest");
    assert!(boot < head, "the manifest is outside the head: {said}");
    assert!(!said.contains("/</script><b>"), "unescaped: {said}");
    assert!(said.contains("\\u003c/script>"), "{said}");
}

/// A tree with a hidden directory, a file, a good link and a broken one.
fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in ["beta", "alpha", ".hidden"] {
        std::fs::create_dir(dir.path().join(name)).expect("a directory");
    }
    std::fs::write(dir.path().join("a-file.txt"), "not a directory").expect("a file");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path().join("alpha"), dir.path().join("link-good"))
            .expect("a good link");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("link-dead"))
            .expect("a dead link");
    }
    dir
}

/// TC-HOST-PICK-1: one level of a real tree.
/// Expected: directories only, name-sorted, a link to a directory followed and
/// a link to nothing left out - the probe failing is what "not enterable"
/// means. Hidden is reported and not applied, because whether to show a dot
/// directory is the reader's choice and not the host's.
#[test]
fn a_listing_is_directories_only_and_says_which_are_hidden() {
    let dir = tree();
    let listed = Browse::default().list(Some(dir.path())).expect("it lists");

    let names: Vec<&str> = listed.entries.iter().map(|row| row.name.as_str()).collect();
    assert!(!names.contains(&"a-file.txt"), "a file got in: {names:?}");
    assert!(
        !names.contains(&"link-dead"),
        "a dead link got in: {names:?}"
    );
    #[cfg(unix)]
    assert!(
        names.contains(&"link-good"),
        "a good link was dropped: {names:?}"
    );
    assert!(names.contains(&".hidden"), "{names:?}");

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the level is not name-sorted");

    let hidden = listed
        .entries
        .iter()
        .find(|row| row.name == ".hidden")
        .expect("the dot directory");
    assert!(hidden.hidden, "the dot convention was not reported");
    assert!(!listed.truncated);
}

/// TC-HOST-PICK-2: the ancestor chain.
/// Expected: root first, every crumb a path that can be listed, and the root
/// labelled by its own path rather than by an empty name - a crumb with no
/// text is a target nobody can click.
#[test]
fn the_crumbs_are_the_chain_from_the_root() {
    let dir = tree();
    let listed = Browse::default()
        .list(Some(&dir.path().join("alpha")))
        .expect("it lists");

    let first = listed.crumbs.first().expect("a root crumb");
    assert!(!first.name.is_empty(), "the root crumb has no label");
    assert_eq!(
        listed.crumbs.last().map(|crumb| crumb.path.clone()),
        Some(dir.path().join("alpha"))
    );
    for crumb in &listed.crumbs {
        assert!(crumb.path.is_absolute(), "{crumb:?}");
    }
}

/// TC-HOST-PICK-3: a level with more children than the bound.
/// Expected: the name-sorted head, cut to the bound, and `truncated` set so
/// the client can say the level is incomplete. Memory stays with the bound
/// rather than with the directory.
#[test]
fn a_level_bigger_than_the_bound_is_cut_and_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    for at in 0..40 {
        std::fs::create_dir(dir.path().join(format!("d{at:03}"))).expect("a directory");
    }
    let browse = Browse {
        max_entries: Some(10),
    };

    let listed = browse.list(Some(dir.path())).expect("it lists");

    assert_eq!(listed.entries.len(), 10, "the bound was not applied");
    assert!(listed.truncated, "a cut level did not say so");
    assert_eq!(listed.entries[0].name, "d000", "not the sorted head");
}

/// TC-HOST-PICK-4: making a directory - the ordinary case, one already there,
/// a missing parent, and a name that is not one segment.
/// Expected: the three failures upstream's wire codes name, and no recursion:
/// a missing parent is a real failure and not a level to invent, because a
/// reader who mistyped a segment should be told rather than handed a tree.
#[test]
fn creation_is_one_directory_and_never_a_tree() {
    let dir = tree();
    let browse = Browse::default();

    let made = browse.create(dir.path(), "new-one").expect("it is made");
    assert!(made.path.is_dir());

    assert_eq!(
        browse.create(dir.path(), "alpha"),
        Err(PickerError::Exists(dir.path().join("alpha")))
    );
    assert!(matches!(
        browse.create(&dir.path().join("no-such-parent"), "child"),
        Err(PickerError::CreateFailed(_))
    ));
    for bad in ["", "  ", "a/b", "..", "."] {
        assert!(
            matches!(
                browse.create(dir.path(), bad),
                Err(PickerError::CreateFailed(_))
            ),
            "{bad:?} was accepted as a name"
        );
    }
}

/// TC-HOST-PICK-5: a path that is not fully qualified.
/// Expected: refused, both for listing and for creation. A relative path would
/// be rebased under whatever directory the host process happens to be in,
/// which is a different place from the one the reader named.
#[test]
fn a_path_that_is_not_fully_qualified_is_refused() {
    let browse = Browse::default();

    assert_eq!(
        browse.list(Some(Path::new("relative/thing"))),
        Err(PickerError::Unreadable(PathBuf::from("relative/thing")))
    );
    assert!(matches!(
        browse.create(Path::new("relative/thing"), "child"),
        Err(PickerError::Unreadable(_))
    ));
}

/// TC-HOST-PICK-6: the capability a consumer asks about first.
/// Expected: `browse`. The seam is worth keeping with one backend behind it,
/// because a consumer that switches on the kind hides the feature for a host
/// it does not understand rather than failing against it.
#[test]
fn the_backend_says_what_it_can_do() {
    assert_eq!(Browse::default().capability(), Capability::Browse);
}

/// TC-HOST-WEB-9: a server asked to stop.
/// Expected: `serve` returns, nothing more is accepted, and a handler holding
/// a socket of its own has been told - the carrier gave that socket away and
/// cannot close it, so a stream holding a response open has to end it itself.
#[tokio::test]
async fn a_server_asked_to_stop_stops_accepting_and_says_so() {
    let (server, listener) = carrier().await;
    let address = server.address();
    let told = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watched = Arc::clone(&told);
    let mut stopping = server.stopping();
    tokio::spawn(async move {
        let _ = stopping.changed().await;
        watched.store(true, std::sync::atomic::Ordering::Release);
    });
    let _route = server
        .register(Pattern::Exact, "/here", says("here"))
        .expect("free");
    let serving = tokio::spawn(server.clone().serve(listener));

    // Up, and answering.
    assert!(ask(address, "GET /here HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .contains("here"));

    server.stop();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), serving).await;
    assert!(ended.is_ok(), "serve did not return");
    assert!(
        told.load(std::sync::atomic::Ordering::Acquire),
        "a handler holding a socket was not told"
    );

    // And the address is no longer answering.
    let after = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        TcpStream::connect(address),
    )
    .await;
    let refused = match after {
        Ok(Ok(mut open)) => {
            use tokio::io::AsyncReadExt;
            let mut byte = [0_u8; 1];
            // A listener that is gone either refuses the connect or accepts
            // nothing and reads zero; both are "not serving".
            open.write_all(b"GET /here HTTP/1.1\r\nhost: x\r\n\r\n")
                .await
                .ok();
            matches!(open.read(&mut byte).await, Ok(0) | Err(_))
        }
        _ => true,
    };
    assert!(refused, "the server kept answering after it was stopped");
}
