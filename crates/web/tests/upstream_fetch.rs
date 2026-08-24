//! Test Design Specification: fetching one page under stated limits, ported.
//!
//! Feature under test: `tetanus_web::fetch` - the URL policy, the redirect
//! rules, the size caps, the content-type list and the charset list. Upstream
//! pins the same behaviour in
//! `packages/web/web-fetch-http/tests/fetch-http.spec.ts`.
//!
//! Approach: a scripted transport. Every rule here is a decision made above
//! the socket, and asserting them against a real server would be asserting a
//! server. The one thing a scripted transport cannot pin - that the live
//! transport stops reading at the cap rather than after it - is a property of
//! `crates/web/src/live.rs`, which is deliberately thin enough to read.
//!
//! What is not restated, and why. Upstream converts HTML to markdown with
//! turndown, and a large part of its suite is that library's behaviour -
//! tables, entity edge cases, a depth preflight against pathological nesting.
//! This strips markup instead, so those cases have nothing to restate and the
//! difference is a `docs/parity.md` row; TC-PORT-WEB-13 pins what the stripper
//! does promise. Its abort-signal cases have no counterpart: a tetanus tool
//! call is bounded by a timeout, not by a caller's signal. Its spill files and
//! presentation metadata serve surfaces this build does not have.
//!
//! Environmental needs: none. No socket is opened by any case in this file.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeMap;
use std::time::Duration;

use tetanus_web::fault::code;
use tetanus_web::fetch::{fetch, FetchLimits, MediaKind};
use tetanus_web::http::HttpResponse;
use tetanus_web::mock::{ok, redirect, MockHttp};
use tetanus_web::WebFault;

fn limits() -> FetchLimits {
    FetchLimits {
        max_bytes: 1024,
        max_chars: 500,
        max_redirects: 2,
        timeout: Duration::from_secs(1),
    }
}

/// TC-PORT-WEB-1: a page is fetched, classified and handed over as text.
///
/// Upstream: "fetches a text body", "fetches an html body and classifies it as
/// html", "sends the configured user agent".
///
/// Input: a plain-text page and an HTML page.
/// Expected: each comes back with its kind, its final URL and its status; the
/// HTML is stripped to its text; and the request carried a `user-agent`.
#[tokio::test]
async fn a_page_is_fetched_classified_and_handed_over_as_text() {
    let transport = MockHttp::new()
        .page(
            "https://example.test/notes",
            ok("text/plain", "plain words"),
        )
        .page(
            "https://example.test/page",
            ok(
                "text/html; charset=utf-8",
                "<html><body><h1>Title</h1><p>Body text</p><script>alert(1)</script></body></html>",
            ),
        );

    let plain = fetch(&transport, "https://example.test/notes", limits())
        .await
        .expect("fetched");
    assert_eq!(plain.kind, MediaKind::Text);
    assert_eq!(plain.text, "plain words");
    assert_eq!(plain.status, 200);
    assert_eq!(plain.final_url, "https://example.test/notes");
    assert!(!plain.truncated);

    let page = fetch(&transport, "https://example.test/page", limits())
        .await
        .expect("fetched");
    assert_eq!(page.kind, MediaKind::Html);
    assert_eq!(page.text, "Title\nBody text");
    assert!(
        !page.text.contains("alert"),
        "a script is not text: {:?}",
        page.text
    );

    let sent = transport.asked();
    assert!(
        sent[0]
            .headers
            .get("user-agent")
            .is_some_and(|agent| agent.starts_with("tetanus/")),
        "the fetch says who it is: {:?}",
        sent[0].headers
    );
}

/// TC-PORT-WEB-2: a declared length past the cap is refused before the body is
/// read.
///
/// Upstream: "rejects an over-cap Content-Length with WEB_FETCH_TOO_LARGE".
///
/// Input: a response declaring ten times the cap.
/// Expected: `WEB_FETCH_TOO_LARGE`, naming both the cap and what was declared.
#[tokio::test]
async fn a_declared_length_past_the_cap_is_refused_before_the_body_is_read() {
    let transport = MockHttp::new().page(
        "https://example.test/big",
        HttpResponse {
            status: 200,
            headers: BTreeMap::from([
                ("content-type".to_string(), "text/plain".to_string()),
                ("content-length".to_string(), "10240".to_string()),
            ]),
            body: b"the first bytes".to_vec(),
            truncated: false,
        },
    );

    let fault = fetch(&transport, "https://example.test/big", limits())
        .await
        .expect_err("refused");
    assert_eq!(fault.code(), code::TOO_LARGE);
    assert!(
        fault.to_string().contains("1024") && fault.to_string().contains("10240"),
        "the message names the cap and the claim: {fault}"
    );
}

/// TC-PORT-WEB-3: a body cut at the cap says it was cut.
///
/// Upstream: "truncates a stream that grows past the byte cap", "does not flag
/// a body that exactly fills the byte cap as truncated", "truncates a decoded
/// body past the character cap".
///
/// A model reading half a page and not being told is a model that answers
/// confidently about the half it did not get.
///
/// Input: a transport reporting a truncated read, then a page longer than the
/// character cap, then one exactly at it.
/// Expected: truncated, truncated, and not truncated.
#[tokio::test]
async fn a_body_cut_at_the_cap_says_it_was_cut() {
    let cut = HttpResponse {
        truncated: true,
        ..ok("text/plain", "as much as fitted")
    };
    let bound = limits().max_chars;
    let transport = MockHttp::new()
        .page("https://example.test/stream", cut)
        .page(
            "https://example.test/long",
            ok("text/plain", &"x".repeat(bound + 1)),
        )
        .page(
            "https://example.test/exact",
            ok("text/plain", &"x".repeat(bound)),
        );

    assert!(
        fetch(&transport, "https://example.test/stream", limits())
            .await
            .expect("fetched")
            .truncated,
        "a transport that stopped reading is a truncated page"
    );
    let long = fetch(&transport, "https://example.test/long", limits())
        .await
        .expect("fetched");
    assert!(long.truncated);
    assert_eq!(long.text.chars().count(), bound);
    assert!(
        !fetch(&transport, "https://example.test/exact", limits())
            .await
            .expect("fetched")
            .truncated,
        "a page that exactly fills the cap was not cut"
    );
}

/// TC-PORT-WEB-4: a type this fetch does not read is refused, and so is no
/// type at all.
///
/// Upstream: "rejects an unsupported content type", "rejects a response with
/// no content type at all".
///
/// Input: an image, and a response with no `Content-Type` header.
/// Expected: `WEB_UNSUPPORTED_CONTENT_TYPE` for both, naming what arrived.
#[tokio::test]
async fn a_type_this_fetch_does_not_read_is_refused_and_so_is_no_type_at_all() {
    let transport = MockHttp::new()
        .page(
            "https://example.test/cat.png",
            ok("image/png", "\u{fffd}PNG"),
        )
        .page(
            "https://example.test/mystery",
            Ok(HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: b"who knows".to_vec(),
                truncated: false,
            }),
        );

    let image = fetch(&transport, "https://example.test/cat.png", limits())
        .await
        .expect_err("refused");
    assert_eq!(image.code(), code::UNSUPPORTED_TYPE);
    assert!(image.to_string().contains("image/png"), "{image}");

    let mystery = fetch(&transport, "https://example.test/mystery", limits())
        .await
        .expect_err("refused");
    assert_eq!(mystery.code(), code::UNSUPPORTED_TYPE);
    assert!(
        mystery.to_string().contains("nothing at all"),
        "an absent type is named as absent: {mystery}"
    );
}

/// TC-PORT-WEB-5: a declared charset is decoded, and an unknown one is
/// refused.
///
/// Upstream: "decodes a non-UTF-8 declared charset", "rejects an unsupported
/// declared charset".
///
/// Input: an ISO-8859-1 body, and a body declaring Shift_JIS.
/// Expected: the first decodes to the character those bytes mean in that
/// encoding; the second is `WEB_UNSUPPORTED_CHARSET`.
#[tokio::test]
async fn a_declared_charset_is_decoded_and_an_unknown_one_is_refused() {
    let latin = HttpResponse {
        status: 200,
        headers: BTreeMap::from([(
            "content-type".to_string(),
            "text/plain; charset=iso-8859-1".to_string(),
        )]),
        // 0xe9 is 'é' in ISO-8859-1 and not valid UTF-8 on its own.
        body: vec![b'c', b'a', b'f', 0xe9],
        truncated: false,
    };
    let transport = MockHttp::new()
        .page("https://example.test/latin", latin)
        .page(
            "https://example.test/jp",
            Ok(HttpResponse {
                status: 200,
                headers: BTreeMap::from([(
                    "content-type".to_string(),
                    "text/plain; charset=Shift_JIS".to_string(),
                )]),
                body: b"nihongo".to_vec(),
                truncated: false,
            }),
        );

    assert_eq!(
        fetch(&transport, "https://example.test/latin", limits())
            .await
            .expect("fetched")
            .text,
        "café"
    );
    let refused = fetch(&transport, "https://example.test/jp", limits())
        .await
        .expect_err("refused");
    assert_eq!(refused.code(), code::UNSUPPORTED_CHARSET);
    assert!(refused.to_string().contains("shift_jis"), "{refused}");
}

/// TC-PORT-WEB-6: a same-origin redirect is followed and the final URL is
/// reported.
///
/// Upstream: "follows a same-origin redirect and reports the final URL",
/// "follows a relative same-origin redirect".
///
/// Input: an absolute same-origin redirect, then a relative one.
/// Expected: the body of the destination, the destination as the final URL,
/// and the hop counted.
#[tokio::test]
async fn a_same_origin_redirect_is_followed_and_the_final_url_is_reported() {
    let transport = MockHttp::new()
        .page(
            "https://example.test/old",
            redirect(301, "https://example.test/new"),
        )
        .page("https://example.test/new", ok("text/plain", "the new page"))
        .page("https://example.test/rel", redirect(302, "/new"));

    let absolute = fetch(&transport, "https://example.test/old", limits())
        .await
        .expect("followed");
    assert_eq!(absolute.text, "the new page");
    assert_eq!(absolute.final_url, "https://example.test/new");
    assert_eq!(absolute.hops, 1);

    let relative = fetch(&transport, "https://example.test/rel", limits())
        .await
        .expect("followed");
    assert_eq!(relative.final_url, "https://example.test/new");
}

/// TC-PORT-WEB-7: a redirect that leaves the origin is blocked.
///
/// Upstream: "blocks a cross-origin redirect with WEB_REDIRECT_BLOCKED",
/// "re-validates a redirect target, rejecting same-origin credentials in the
/// Location".
///
/// This is the case that makes a fetch tool safe to give a model: a page that
/// redirects to an internal address is how a fetch becomes a request forgery,
/// and a hop carrying credentials is a credential about to be sent.
///
/// Input: a redirect to another host, and one to a credentialled URL on the
/// same host.
/// Expected: `WEB_REDIRECT_BLOCKED` for the first and `WEB_BAD_URL` for the
/// second, with neither destination requested.
#[tokio::test]
async fn a_redirect_that_leaves_the_origin_is_blocked() {
    let transport = MockHttp::new()
        .page(
            "https://example.test/out",
            redirect(302, "http://169.254.169.254/latest/meta-data/"),
        )
        .page(
            "https://example.test/creds",
            redirect(302, "https://user:secret@example.test/inner"),
        )
        .otherwise(ok("text/plain", "should never be read"));

    let out = fetch(&transport, "https://example.test/out", limits())
        .await
        .expect_err("blocked");
    assert_eq!(out.code(), code::REDIRECT_BLOCKED);
    assert!(out.to_string().contains("different origin"), "{out}");

    let creds = fetch(&transport, "https://example.test/creds", limits())
        .await
        .expect_err("blocked");
    assert_eq!(creds.code(), code::BAD_URL);

    let asked: Vec<String> = transport.asked().into_iter().map(|r| r.url).collect();
    assert_eq!(
        asked,
        vec![
            "https://example.test/out".to_string(),
            "https://example.test/creds".to_string()
        ],
        "no blocked destination was requested"
    );
}

/// TC-PORT-WEB-8: the hop cap is exact.
///
/// Upstream: "rejects exceeding the redirect hop cap", "follows exactly
/// maxRedirects hops", "makes exactly maxRedirects+1 requests before blocking
/// an over-long chain", "maxRedirects: 0 follows no redirect but still fetches
/// a direct 200".
///
/// Input: a chain of two hops under a cap of two, a chain of three under the
/// same cap, and a direct page under a cap of zero.
/// Expected: the two-hop chain lands; the three-hop chain is blocked after
/// exactly three requests; the direct page under a cap of zero still arrives.
#[tokio::test]
async fn the_hop_cap_is_exact() {
    let transport = MockHttp::new()
        .page("https://example.test/1", redirect(302, "/2"))
        .page("https://example.test/2", redirect(302, "/3"))
        .page("https://example.test/3", ok("text/plain", "arrived"))
        .page("https://example.test/a", redirect(302, "/b"))
        .page("https://example.test/b", redirect(302, "/c"))
        .page("https://example.test/c", redirect(302, "/d"))
        .page("https://example.test/d", ok("text/plain", "too far"))
        .page("https://example.test/direct", ok("text/plain", "here"));

    assert_eq!(
        fetch(&transport, "https://example.test/1", limits())
            .await
            .expect("two hops is within a cap of two")
            .text,
        "arrived"
    );

    let blocked = fetch(&transport, "https://example.test/a", limits())
        .await
        .expect_err("blocked");
    assert_eq!(blocked.code(), code::REDIRECT_BLOCKED);
    let over_long: Vec<String> = transport
        .asked()
        .into_iter()
        .map(|request| request.url)
        .filter(|url| url.contains("/a") || url.contains("/b") || url.contains("/c"))
        .collect();
    assert_eq!(
        over_long.len(),
        3,
        "the cap is reached, not exceeded: {over_long:?}"
    );

    let none_followed = FetchLimits {
        max_redirects: 0,
        ..limits()
    };
    assert_eq!(
        fetch(&transport, "https://example.test/direct", none_followed)
            .await
            .expect("a direct answer needs no hop")
            .text,
        "here"
    );
    assert!(
        fetch(&transport, "https://example.test/1", none_followed)
            .await
            .is_err(),
        "a cap of zero follows nothing"
    );
}

/// TC-PORT-WEB-9: a redirect with nowhere to go is the server's fault, said so.
///
/// Upstream: "treats a redirect without a Location header as a provider
/// error".
///
/// Input: a 302 with no `Location`.
/// Expected: `WEB_PROVIDER_ERROR` naming the status.
#[tokio::test]
async fn a_redirect_with_nowhere_to_go_is_the_servers_fault() {
    let transport = MockHttp::new().page(
        "https://example.test/nowhere",
        HttpResponse {
            status: 302,
            headers: BTreeMap::new(),
            body: Vec::new(),
            truncated: false,
        },
    );
    let fault = fetch(&transport, "https://example.test/nowhere", limits())
        .await
        .expect_err("refused");
    assert_eq!(fault.code(), code::PROVIDER_ERROR);
    assert!(fault.to_string().contains("302"), "{fault}");
}

/// TC-PORT-WEB-10: a URL this tool will not send never reaches the transport.
///
/// Upstream: "rejects a non-http scheme before any network access", "rejects
/// credentials in the URL", "validates scheme, credentials, and length".
///
/// Input: a `file://` URL, a `javascript:` URL, a URL with a password in it,
/// and a string that is not a URL.
/// Expected: `WEB_BAD_URL` for each, and no request made at all.
#[tokio::test]
async fn a_url_this_tool_will_not_send_never_reaches_the_transport() {
    let transport = MockHttp::new().otherwise(ok("text/plain", "should never be read"));
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:token@example.test/x",
        "not a url at all",
    ] {
        let fault = fetch(&transport, url, limits()).await.unwrap_err_for(url);
        assert_eq!(fault.code(), code::BAD_URL, "{url}: {fault}");
    }
    assert!(
        transport.asked().is_empty(),
        "something was sent: {:?}",
        transport.asked()
    );
}

/// TC-PORT-WEB-11: a non-2xx page is a result, not a failure.
///
/// Upstream: "returns a non-2xx response as a result, not an error".
///
/// A 404's body is often the useful part - a message saying what to ask for
/// instead - and a model that is told only "it failed" cannot use it.
///
/// Input: a 404 with a text body.
/// Expected: a result carrying the status and the body.
#[tokio::test]
async fn a_non_2xx_page_is_a_result_not_a_failure() {
    let transport = MockHttp::new().page(
        "https://example.test/gone",
        Ok(HttpResponse {
            status: 404,
            ..ok("text/plain", "no such document; try /index")
        }),
    );
    let fetched = fetch(&transport, "https://example.test/gone", limits())
        .await
        .expect("a 404 is an answer");
    assert_eq!(fetched.status, 404);
    assert!(fetched.text.contains("try /index"));
}

/// TC-PORT-WEB-12: a transport failure keeps the transport's own words.
///
/// Upstream: "maps a connection failure to WEB_PROVIDER_ERROR", "times out a
/// slow response with WEB_FETCH_TIMEOUT".
///
/// Input: a transport answering with a timeout, and one answering with a
/// connection failure.
/// Expected: the codes pass through unchanged.
#[tokio::test]
async fn a_transport_failure_keeps_the_transports_own_words() {
    let transport = MockHttp::new()
        .page("https://example.test/slow", WebFault::Timeout)
        .page(
            "https://example.test/refused",
            WebFault::Provider("connection refused".to_string()),
        );

    assert_eq!(
        fetch(&transport, "https://example.test/slow", limits())
            .await
            .expect_err("timed out")
            .code(),
        code::TIMEOUT
    );
    let refused = fetch(&transport, "https://example.test/refused", limits())
        .await
        .expect_err("refused");
    assert_eq!(refused.code(), code::PROVIDER_ERROR);
    assert!(refused.to_string().contains("connection refused"));
}

/// TC-PORT-WEB-13: markup becomes text, and what is not text is dropped.
///
/// Upstream: "converts html via turndown: entities, links, tables, nesting;
/// drops script/style/noscript", "comments and mismatched closing tags cannot
/// hide deep nesting from the preflight", "scans malformed unterminated tags
/// in bounded time".
///
/// tetanus strips rather than converting, so what is restated is the part that
/// is a promise rather than a library's output: scripts and styles do not
/// reach the model, comments do not, entities are readable, and a document
/// that is nothing but unterminated tags terminates.
///
/// Input: a page with a script, a style, a comment, entities and a tag that is
/// never closed.
/// Expected: none of the dropped content, the entities decoded, and the whole
/// thing finished promptly.
#[tokio::test]
async fn markup_becomes_text_and_what_is_not_text_is_dropped() {
    let page = concat!(
        "<html><head><style>body{color:red}</style></head><body>",
        "<!-- a note nobody should read -->",
        "<p>five &lt; six &amp; seven</p>",
        "<script>steal()</script>",
        "<div>last line</div>",
        "<span class=\"unterminated",
    );
    let transport = MockHttp::new().page("https://example.test/mess", ok("text/html", page));

    let started = std::time::Instant::now();
    let fetched = fetch(&transport, "https://example.test/mess", limits())
        .await
        .expect("fetched");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "stripping took {:?}",
        started.elapsed()
    );
    assert_eq!(fetched.text, "five < six & seven\nlast line");
}

/// A small readability helper: `expect_err` cannot say which input failed.
trait UnwrapErrFor {
    fn unwrap_err_for(self, what: &str) -> WebFault;
}

impl<T: std::fmt::Debug> UnwrapErrFor for Result<T, WebFault> {
    fn unwrap_err_for(self, what: &str) -> WebFault {
        match self {
            Err(fault) => fault,
            Ok(value) => panic!("{what} was accepted: {value:?}"),
        }
    }
}
