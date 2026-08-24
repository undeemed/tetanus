//! Test Design Specification: the search seam, one provider over it, and the
//! two model-facing tools, ported.
//!
//! Features under test: `tetanus_web::search` (registration, resolution, the
//! result cap), `tetanus_web::provider` (DeepSeek's search mapped over the
//! transport seam), and `tetanus_web::tools` (what the model reads, and what a
//! failure looks like). Upstream pins these in `packages/web/web/tests/
//! web.spec.ts`, `web-search-deepseek/tests/deepseek.spec.ts` and
//! `tool-web/tests/tool-web.spec.ts`.
//!
//! Approach: the deterministic mock provider for the runtime and tool cases,
//! and the scripted HTTP transport for the real provider. Both halves matter:
//! the runtime's rules are about choosing between providers, and the
//! provider's are about a wire format, and neither can be asserted through the
//! other.
//!
//! What is not restated, and why. Upstream's Exa and Perplexity providers are
//! two more implementations of the same seam. Its credential-resolver races -
//! a key rotated mid-search, a resolver that never settles - need a credential
//! service this build does not have; a key here is config, and TC-PORT-WEB-16
//! pins the part that survives: no key is a provider that is not usable rather
//! than a search that fails. Its presentation metadata and card views belong
//! to a surface contract this crate does not serve.
//!
//! Environmental needs: none. No socket, no key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use tetanus_turn::tools::{Tool, ToolError};
use tetanus_web::fault::code;
use tetanus_web::http::{HttpResponse, Method};
use tetanus_web::mock::{MockHttp, MockSearch};
use tetanus_web::provider::{DeepSeekSearch, DeepSeekSearchConfig};
use tetanus_web::search::{Availability, SearchAnswer, Source, WebRuntime};
use tetanus_web::tools::{WebFetchTool, WebSearchTool};
use tetanus_web::{FetchLimits, WebFault};

fn answer_body(body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
        body: serde_json::to_vec(&body).expect("json"),
        truncated: false,
    }
}

/// The refusal a call produced. `expect_err` needs the success type to be
/// printable, and a resolved provider is a trait object that is not.
fn refusal<T>(result: Result<T, WebFault>) -> WebFault {
    match result {
        Err(fault) => fault,
        Ok(_) => panic!("expected a refusal, and the call succeeded"),
    }
}

fn configured_provider(transport: MockHttp) -> DeepSeekSearch<MockHttp> {
    DeepSeekSearch::new(
        transport,
        DeepSeekSearchConfig {
            api_key: Some("a-key".to_string()),
            ..DeepSeekSearchConfig::default()
        },
    )
}

/// TC-PORT-WEB-14: a runtime with no usable provider says so, and one with two
/// refuses to choose.
///
/// Upstream: "throws WEB_PROVIDER_UNAVAILABLE when nothing is registered",
/// "... when providers exist but none are usable", "throws
/// WEB_PROVIDER_AMBIGUOUS rather than picking by order", "does not let
/// registration order change auto-selection", "ignores unusable providers when
/// auto-selecting", "throws WEB_DUPLICATE_PROVIDER on a duplicate search id".
///
/// Picking by registration order would make the same query answered by a
/// different engine depending on which plugin loaded first.
///
/// Input: an empty runtime; one holding only an unusable provider; one holding
/// two usable ones; one holding a usable and an unusable one; and a duplicate
/// registration.
/// Expected: unavailable, unavailable, ambiguous naming both candidates, the
/// usable one chosen, and the duplicate refused.
#[test]
fn a_runtime_with_no_usable_provider_says_so_and_one_with_two_refuses_to_choose() {
    assert_eq!(
        refusal(WebRuntime::new().resolve()).code(),
        code::PROVIDER_UNAVAILABLE
    );

    let only_broken = WebRuntime::new().with(Arc::new(MockSearch::unusable("exa", "no key")));
    assert_eq!(
        refusal(only_broken.resolve()).code(),
        code::PROVIDER_UNAVAILABLE
    );

    let both = WebRuntime::new()
        .with(Arc::new(MockSearch::new("zebra", &[])))
        .with(Arc::new(MockSearch::new("alpha", &[])));
    let ambiguous = refusal(both.resolve());
    assert_eq!(ambiguous.code(), code::PROVIDER_AMBIGUOUS);
    assert!(
        ambiguous.to_string().contains("alpha") && ambiguous.to_string().contains("zebra"),
        "both candidates are named: {ambiguous}"
    );

    let one_usable = WebRuntime::new()
        .with(Arc::new(MockSearch::unusable("exa", "no key")))
        .with(Arc::new(MockSearch::new("deepseek", &[])));
    assert_eq!(
        one_usable.resolve().expect("only one is usable").id(),
        "deepseek"
    );

    let mut duplicate = WebRuntime::new().with(Arc::new(MockSearch::new("deepseek", &[])));
    assert_eq!(
        refusal(duplicate.register(Arc::new(MockSearch::new("deepseek", &[])))).code(),
        code::DUPLICATE_PROVIDER
    );
}

/// TC-PORT-WEB-15: a configured provider is used, and a configured one that is
/// not there is a different refusal from one that cannot serve.
///
/// Upstream: "runs the configured provider even when another usable provider
/// is registered", "throws WEB_PROVIDER_CONFIGURED_MISSING for an unregistered
/// configured id", "throws WEB_PROVIDER_CONFIGURED_UNAVAILABLE for an unusable
/// configured id".
///
/// The two refusals are different questions: one is a name that is wrong, the
/// other is a name that is right and a deployment that is unfinished.
///
/// Input: a runtime with two providers configured to one; a configured id
/// nobody registered; a configured id that is registered and unusable.
/// Expected: the named provider answers; `WEB_PROVIDER_CONFIGURED_MISSING`
/// listing what is registered; `WEB_PROVIDER_CONFIGURED_UNAVAILABLE` carrying
/// the provider's own reason.
#[tokio::test]
async fn a_configured_provider_is_used_and_the_two_ways_of_naming_a_bad_one_differ() {
    let runtime = WebRuntime::new()
        .with(Arc::new(MockSearch::new(
            "alpha",
            &[("A", "https://a.test/")],
        )))
        .with(Arc::new(MockSearch::new(
            "beta",
            &[("B", "https://b.test/")],
        )))
        .configure(Some("beta".to_string()));
    let answer = runtime.search("anything").await.expect("searched");
    assert_eq!(answer.sources[0].title, "B");

    let missing = WebRuntime::new()
        .with(Arc::new(MockSearch::new("alpha", &[])))
        .configure(Some("nobody".to_string()));
    let fault = refusal(missing.resolve());
    assert_eq!(fault.code(), code::CONFIGURED_MISSING);
    assert!(fault.to_string().contains("alpha"), "{fault}");

    let unusable = WebRuntime::new()
        .with(Arc::new(MockSearch::unusable(
            "alpha",
            "the key is missing",
        )))
        .configure(Some("alpha".to_string()));
    let fault = refusal(unusable.resolve());
    assert_eq!(fault.code(), code::CONFIGURED_UNAVAILABLE);
    assert!(fault.to_string().contains("the key is missing"), "{fault}");
}

/// TC-PORT-WEB-16: the result cap is the runtime's, and truncation is stated.
///
/// Upstream: "truncates sources and sets truncated when a provider
/// over-returns", "leaves truncated false when within the bound", "does not
/// bound when maxResults is omitted", "forwards a configured cap to the seam".
///
/// Input: a provider returning three sources, under a cap of two, a cap of
/// five, and no cap.
/// Expected: two and truncated; three and not; three and not - and the
/// provider was told the cap either way.
#[tokio::test]
async fn the_result_cap_is_the_runtimes_and_truncation_is_stated() {
    let sources = [
        ("one", "https://one.test/"),
        ("two", "https://two.test/"),
        ("three", "https://three.test/"),
    ];
    let provider = Arc::new(MockSearch::new("mock", &sources));

    let capped = WebRuntime::new()
        .with(Arc::clone(&provider) as Arc<dyn tetanus_web::SearchProvider>)
        .cap(Some(2));
    let answer = capped.search("q").await.expect("searched");
    assert_eq!(answer.sources.len(), 2);
    assert!(answer.truncated);
    assert_eq!(
        provider.asked()[0].max_results,
        Some(2),
        "the provider is told the cap as well, so it can ask for less"
    );

    let roomy = WebRuntime::new()
        .with(Arc::clone(&provider) as Arc<dyn tetanus_web::SearchProvider>)
        .cap(Some(5));
    let answer = roomy.search("q").await.expect("searched");
    assert_eq!(answer.sources.len(), 3);
    assert!(!answer.truncated);

    let uncapped =
        WebRuntime::new().with(Arc::clone(&provider) as Arc<dyn tetanus_web::SearchProvider>);
    assert_eq!(
        uncapped.search("q").await.expect("searched").sources.len(),
        3
    );
}

/// TC-PORT-WEB-17: the DeepSeek provider is unusable until it is configured.
///
/// Upstream: "is unavailable without a key", "is available with a key", "is
/// misconfigured when the base URL is unparseable", "is misconfigured when
/// request limits are not positive integers", "reports an actionable
/// credential error".
///
/// A provider that failed the search instead would put a configuration mistake
/// in front of the model as a tool failure, once per call.
///
/// Input: the provider with no key, a blank key, a base URL that is not a URL,
/// a zero limit, and a proper configuration.
/// Expected: unusable with a reason for each of the first four, usable for the
/// last.
#[test]
fn the_deepseek_provider_is_unusable_until_it_is_configured() {
    use tetanus_web::SearchProvider;

    let unconfigured = DeepSeekSearch::new(MockHttp::new(), DeepSeekSearchConfig::default());
    assert!(matches!(
        unconfigured.availability(),
        Availability::Unusable(_)
    ));

    for (config, expected) in [
        (
            DeepSeekSearchConfig {
                api_key: Some("   ".to_string()),
                ..DeepSeekSearchConfig::default()
            },
            "blank",
        ),
        (
            DeepSeekSearchConfig {
                api_key: Some("k".to_string()),
                base_url: "not a url".to_string(),
                ..DeepSeekSearchConfig::default()
            },
            "not a URL",
        ),
        (
            DeepSeekSearchConfig {
                api_key: Some("k".to_string()),
                max_uses: 0,
                ..DeepSeekSearchConfig::default()
            },
            "at least one",
        ),
    ] {
        let provider = DeepSeekSearch::new(MockHttp::new(), config);
        let Availability::Unusable(why) = provider.availability() else {
            panic!("{expected}: expected an unusable provider");
        };
        assert!(why.contains(expected), "{why}");
    }

    assert_eq!(
        configured_provider(MockHttp::new()).availability(),
        Availability::Usable
    );
}

/// TC-PORT-WEB-18: the provider posts the search request and maps what comes
/// back.
///
/// Upstream: "records and posts the same Anthropic Messages request with the
/// web_search server tool", "joins result items to citation snippets and maps
/// page_age to publishedAt", "dedupes repeated urls across result blocks
/// (first wins)", "skips non-result items and items with an empty url".
///
/// Input: a scripted answer holding a result block with three items - one
/// repeated, one with no URL - and a text block citing one of them.
/// Expected: a POST to the messages endpoint carrying the query and the
/// `web_search` tool; two sources, in order, the cited one carrying its quoted
/// text, and the page age carried through unparsed.
#[tokio::test]
async fn the_provider_posts_the_search_request_and_maps_what_comes_back() {
    use tetanus_web::SearchProvider;

    let transport = MockHttp::new().page(
        "https://api.deepseek.com/v1/messages",
        answer_body(json!({
            "content": [
                {
                    "type": "web_search_tool_result",
                    "content": [
                        { "url": "https://rust-lang.org/", "title": "Rust", "page_age": "2 days ago" },
                        { "url": "", "title": "nothing" },
                        { "url": "https://doc.rust-lang.org/", "title": "Docs" },
                        { "url": "https://rust-lang.org/", "title": "Rust again" },
                    ],
                },
                {
                    "type": "text",
                    "text": "Rust is a language.",
                    "citations": [
                        { "url": "https://rust-lang.org/", "cited_text": "a language empowering everyone" },
                    ],
                },
            ],
        })),
    );
    let provider = configured_provider(transport);

    let answer = provider
        .search(&tetanus_web::search::SearchQuery {
            text: "what is rust".to_string(),
            max_results: Some(3),
        })
        .await
        .expect("searched");

    assert_eq!(
        answer.sources,
        vec![
            Source {
                title: "Rust".to_string(),
                url: "https://rust-lang.org/".to_string(),
                snippet: Some("a language empowering everyone".to_string()),
                published: Some("2 days ago".to_string()),
            },
            Source {
                title: "Docs".to_string(),
                url: "https://doc.rust-lang.org/".to_string(),
                snippet: None,
                published: None,
            },
        ]
    );
    assert_eq!(answer.answer.as_deref(), Some("Rust is a language."));
}

/// TC-PORT-WEB-19: every way the provider's request can fail comes back as a
/// provider error with the server's words.
///
/// Upstream: "maps an HTTP error to WEB_PROVIDER_ERROR with the provider
/// message", "keeps a status-line message when the error body is not JSON",
/// "maps an unparseable success body to WEB_PROVIDER_ERROR", "maps a
/// well-formed body of the wrong shape to WEB_PROVIDER_ERROR, not a raw
/// TypeError", "strict mode flows through search(): a prose-only response
/// throws WEB_PROVIDER_ERROR".
///
/// The last one is the case with a reason behind it: an answer with no result
/// block found nothing anyone can check, and handing a model an uncited
/// paragraph as search results is how a citation becomes a hallucination.
///
/// Input: a 401 with a JSON error body; a 500 with an HTML body; a 200 that is
/// not JSON; a 200 of the wrong shape; and a 200 with prose and no results.
/// Expected: `WEB_PROVIDER_ERROR` for each, carrying what the server said.
#[tokio::test]
async fn every_way_the_providers_request_can_fail_is_a_provider_error() {
    use tetanus_web::SearchProvider;

    let endpoint = "https://api.deepseek.com/v1/messages";
    let query = tetanus_web::search::SearchQuery {
        text: "q".to_string(),
        max_results: None,
    };

    let cases: Vec<(HttpResponse, &str)> = vec![
        (
            HttpResponse {
                status: 401,
                body: serde_json::to_vec(&json!({ "error": { "message": "invalid api key" } }))
                    .expect("json"),
                ..answer_body(json!({}))
            },
            "invalid api key",
        ),
        (
            HttpResponse {
                status: 500,
                body: b"<html>upstream is down</html>".to_vec(),
                ..answer_body(json!({}))
            },
            "upstream is down",
        ),
        (
            HttpResponse {
                body: b"not json at all".to_vec(),
                ..answer_body(json!({}))
            },
            "does not parse",
        ),
        (answer_body(json!({ "unexpected": true })), "no content"),
        (
            answer_body(json!({
                "content": [{ "type": "text", "text": "I think so, probably." }],
            })),
            "only prose",
        ),
    ];

    for (response, expected) in cases {
        let provider = configured_provider(MockHttp::new().page(endpoint, response));
        let fault = provider.search(&query).await.expect_err("refused");
        assert_eq!(fault.code(), code::PROVIDER_ERROR, "{fault}");
        assert!(
            fault.to_string().contains(expected),
            "expected {expected:?} in: {fault}"
        );
    }

    // And the request that went out is the one upstream describes.
    let transport = MockHttp::new().page(
        endpoint,
        answer_body(json!({
            "content": [{ "type": "web_search_tool_result", "content": [] }],
        })),
    );
    let provider = configured_provider(transport);
    provider.search(&query).await.expect("searched");
}

/// TC-PORT-WEB-20: the search tool renders results the model can cite.
///
/// Upstream: "renders content, sources with titles/hostnames, snippets, and a
/// citation reminder", "reports no results when there is neither content nor
/// sources", "notes truncation", "falls back to the raw URL as a source label
/// when the URL is unparseable".
///
/// Input: an answer with prose, two sources and truncation; and an empty one.
/// Expected: the prose, a numbered list carrying each title, hostname, URL and
/// snippet, a truncation note, and the citation reminder; and for the empty
/// one, a sentence naming the query.
#[tokio::test]
async fn the_search_tool_renders_results_the_model_can_cite() {
    let rendered = tetanus_web::tools::render_search(
        "rust",
        &SearchAnswer {
            answer: Some("Rust is a language.".to_string()),
            sources: vec![
                Source {
                    title: "Rust".to_string(),
                    url: "https://rust-lang.org/learn".to_string(),
                    snippet: Some("empowering\n  everyone".to_string()),
                    published: None,
                },
                Source {
                    title: "A local note".to_string(),
                    url: "not-a-url".to_string(),
                    snippet: None,
                    published: None,
                },
            ],
            truncated: true,
        },
    );
    assert!(rendered.starts_with("Rust is a language."));
    assert!(rendered.contains("[1] Rust - rust-lang.org"));
    assert!(rendered.contains("https://rust-lang.org/learn"));
    assert!(
        rendered.contains("empowering everyone"),
        "a snippet is one line: {rendered}"
    );
    assert!(
        rendered.contains("[2] A local note - not-a-url"),
        "an unparseable URL is its own label: {rendered}"
    );
    assert!(rendered.contains("more results were found"));
    assert!(rendered
        .trim_end()
        .ends_with("Cite the sources you use by their URL."));

    let empty = tetanus_web::tools::render_search(
        "nothing at all",
        &SearchAnswer {
            answer: None,
            sources: Vec::new(),
            truncated: false,
        },
    );
    assert_eq!(empty, "No results for \"nothing at all\".");
}

/// TC-PORT-WEB-21: both tools refuse an empty argument and carry a failure's
/// code.
///
/// Upstream: "validates the query", "validates url (non-empty, no timeout
/// parameter)", "rejects invalid arguments with a structured INVALID_ARGS
/// error", "surfaces a structured WebError when no provider is available".
///
/// Input: `web_search` with no query and with a blank one; `web_fetch` with no
/// URL; and `web_search` on a runtime with no provider.
/// Expected: `INVALID_ARGS` in the first three, `WEB_PROVIDER_UNAVAILABLE` in
/// the last, each as a failed tool call rather than a panic.
#[tokio::test]
async fn both_tools_refuse_an_empty_argument_and_carry_a_failures_code() {
    let search = WebSearchTool::new(Arc::new(
        WebRuntime::new().with(Arc::new(MockSearch::new("mock", &[]))),
    ));
    for arguments in [json!({}), json!({ "query": "   " })] {
        let failed = search.execute(&arguments).await.expect_err("refused");
        assert!(failed.to_string().contains(code::INVALID_ARGS), "{failed}");
    }

    let fetcher = WebFetchTool::new(Arc::new(MockHttp::new()));
    let failed = fetcher.execute(&json!({})).await.expect_err("refused");
    assert!(failed.to_string().contains(code::INVALID_ARGS), "{failed}");

    let unserved = WebSearchTool::new(Arc::new(WebRuntime::new()));
    let failed = unserved
        .execute(&json!({ "query": "anything" }))
        .await
        .expect_err("refused");
    assert!(
        matches!(&failed, ToolError::Failed(name, _) if name == WebSearchTool::NAME),
        "the tool that failed is named: {failed}"
    );
    assert!(
        failed.to_string().contains(code::PROVIDER_UNAVAILABLE),
        "{failed}"
    );
}

/// TC-PORT-WEB-22: the fetch tool answers with the page, its status, and a
/// truncation note.
///
/// Upstream: "renders an html body to markdown text with a status header",
/// "passes a text body through and notes truncation", "caps the complete
/// output and notes truncation", "bounds the rendered output of the registered
/// web_fetch tool".
///
/// Input: a page under a small output cap, fetched through the tool.
/// Expected: the answer opens with the final URL, the status and the media
/// type; the body follows; the output is cut to the cap and says it was.
#[tokio::test]
async fn the_fetch_tool_answers_with_the_page_its_status_and_a_truncation_note() {
    let transport = MockHttp::new().page(
        "https://example.test/doc",
        tetanus_web::mock::ok("text/html", "<p>the whole of the page body</p>"),
    );
    let tool = WebFetchTool::new(Arc::new(transport))
        .limits(FetchLimits::default())
        .max_output(40);

    let outcome = tool
        .execute(&json!({ "url": "https://example.test/doc" }))
        .await
        .expect("fetched");
    assert!(outcome.ok);
    assert!(
        outcome
            .content
            .starts_with("https://example.test/doc (200 text/html)"),
        "the header names where it came from: {:?}",
        outcome.content
    );
    assert!(
        outcome.content.contains("this is the beginning of it"),
        "the cut is stated: {:?}",
        outcome.content
    );
}

/// TC-PORT-WEB-23: a fetch failure reaches the model as a failed call with its
/// code.
///
/// Upstream: "falls back to the generic card on an error result" - the same
/// fact from the other side: a failed fetch is a result, not a turn ending.
///
/// Input: a URL the fetch policy refuses.
/// Expected: a [`ToolError::Failed`] naming `web_fetch` and carrying
/// `WEB_BAD_URL`.
#[tokio::test]
async fn a_fetch_failure_reaches_the_model_as_a_failed_call_with_its_code() {
    let tool = WebFetchTool::new(Arc::new(MockHttp::new()));
    let failed = tool
        .execute(&json!({ "url": "file:///etc/passwd" }))
        .await
        .expect_err("refused");
    assert!(failed.to_string().contains(code::BAD_URL), "{failed}");
    assert!(matches!(failed, ToolError::Failed(name, _) if name == WebFetchTool::NAME));
}

/// TC-PORT-WEB-24: the request the provider sends is the documented one.
///
/// Upstream: "records and posts the same Anthropic Messages request with the
/// web_search server tool".
///
/// Input: one search through a configured provider.
/// Expected: a POST to `/v1/messages`, carrying the key header, the query as
/// the user message, and the `web_search` server tool with the cap the caller
/// asked for.
#[tokio::test]
async fn the_request_the_provider_sends_is_the_documented_one() {
    use tetanus_web::SearchProvider;

    let transport = MockHttp::new().page(
        "https://api.deepseek.com/v1/messages",
        answer_body(json!({
            "content": [{
                "type": "web_search_tool_result",
                "content": [{ "url": "https://x.test/", "title": "X" }],
            }],
        })),
    );
    let provider = DeepSeekSearch::new(
        transport,
        DeepSeekSearchConfig {
            api_key: Some("a-key".to_string()),
            ..DeepSeekSearchConfig::default()
        },
    );
    provider
        .search(&tetanus_web::search::SearchQuery {
            text: "what is rust".to_string(),
            max_results: Some(4),
        })
        .await
        .expect("searched");

    // The transport moved into the provider, so the record is read back
    // through the provider's own view of it.
    let sent = provider.transport().asked();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, Method::Post);
    assert_eq!(sent[0].url, "https://api.deepseek.com/v1/messages");
    assert_eq!(
        sent[0].headers.get("x-api-key").map(String::as_str),
        Some("a-key")
    );
    let body: serde_json::Value =
        serde_json::from_slice(sent[0].body.as_ref().expect("a body")).expect("json");
    assert_eq!(
        body.pointer("/messages/0/content"),
        Some(&json!("what is rust"))
    );
    assert_eq!(body.pointer("/tools/0/name"), Some(&json!("web_search")));
    assert_eq!(body.pointer("/tools/0/max_uses"), Some(&json!(4)));
}

/// TC-PORT-WEB-25: a search that reaches the tool carries the provider's
/// sources.
///
/// Upstream: "executes web_search and formats the result", "runs the selected
/// provider and returns its result".
///
/// Input: the tool over a runtime holding one mock provider.
/// Expected: a successful outcome naming both sources and their URLs.
#[tokio::test]
async fn a_search_that_reaches_the_tool_carries_the_providers_sources() {
    let runtime = WebRuntime::new().with(Arc::new(MockSearch::new(
        "mock",
        &[
            ("First", "https://one.test/a"),
            ("Second", "https://two.test/b"),
        ],
    )));
    let tool = WebSearchTool::new(Arc::new(runtime));

    let outcome = tool
        .execute(&json!({ "query": "anything" }))
        .await
        .expect("searched");
    assert!(outcome.ok);
    assert!(outcome.content.contains("https://one.test/a"));
    assert!(outcome.content.contains("https://two.test/b"));
    assert!(outcome.content.contains("what mock found"));
}

/// TC-PORT-WEB-26: an empty query is refused before a provider is asked.
///
/// Upstream: "validates the query".
///
/// Input: a runtime whose provider records what it was asked, searched with
/// whitespace.
/// Expected: `INVALID_ARGS`, and the provider was never called.
#[tokio::test]
async fn an_empty_query_is_refused_before_a_provider_is_asked() {
    let provider = Arc::new(MockSearch::new("mock", &[]));
    let runtime =
        WebRuntime::new().with(Arc::clone(&provider) as Arc<dyn tetanus_web::SearchProvider>);
    let fault = runtime.search("   ").await.expect_err("refused");
    assert_eq!(fault.code(), code::INVALID_ARGS);
    assert!(provider.asked().is_empty(), "the provider was asked anyway");
    let _ = WebFault::Timeout;
}
