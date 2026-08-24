//! Test Design Specification: turning the settings document into web tools.
//!
//! Feature under test: `tetanus_web::settings` - which tools a document turns
//! on, the limits a fetch runs under, and the provider a search resolves to.
//! Upstream's equivalents are its plugin configs: `tool-web`'s enablement and
//! caps, and `web-search-deepseek`'s key and base URL
//! (`tool-web/tests/tool-web.spec.ts`, `web-search-deepseek/tests/settings.spec.ts`).
//!
//! Approach: a config built key by key. No case reaches the network: the live
//! transport is constructed, which opens nothing, and every assertion is about
//! what was registered and with what.
//!
//! What is not restated, and why. Upstream refuses an out-of-range limit at
//! plugin construction through Schemastery; tetanus refuses it here, in the
//! same place it refuses every other malformed setting, and TC-PORT-WEB-29
//! pins that. Its per-tool timeout budget belongs to a tool-call scheduler
//! this build does not have.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::time::Duration;

use tetanus_config::{Config, Layer};
use tetanus_turn::tools::Tool;
use tetanus_web::settings::{self, key};

fn document(pairs: &[(&str, serde_json::Value)]) -> Config {
    let mut config = Config::default();
    for (key, value) in pairs {
        config.set(key, value.clone(), Layer::File);
    }
    config
}

fn names(tools: &[std::sync::Arc<dyn Tool>]) -> Vec<String> {
    tools.iter().map(|tool| tool.schema().name).collect()
}

/// TC-PORT-WEB-27: nothing reaches the network unless the document says so.
///
/// Upstream: "registers both tools by default", "registers only enabled
/// tools", "registers only web_fetch when search is disabled".
///
/// tetanus inverts the default deliberately, and this is the case that states
/// it: a harness whose first run in a sandbox quietly fetched a URL a model
/// invented would be a surprise nobody asked for. Upstream's plugin is only
/// loaded by a deployment that wanted it; a tetanus registry is compiled in,
/// so the switch has to be the document.
///
/// Input: an empty document; one enabling fetch; one enabling both.
/// Expected: no tools, then `web_fetch`, then both.
#[test]
fn nothing_reaches_the_network_unless_the_document_says_so() {
    assert!(settings::tools(&Config::default(), None)
        .expect("read")
        .is_empty());

    let fetch_only = document(&[(key::FETCH_ENABLED, serde_json::json!(true))]);
    assert_eq!(
        names(&settings::tools(&fetch_only, None).expect("read")),
        vec!["web_fetch".to_string()]
    );

    let both = document(&[
        (key::FETCH_ENABLED, serde_json::json!(true)),
        (key::SEARCH_ENABLED, serde_json::json!(true)),
    ]);
    assert_eq!(
        names(&settings::tools(&both, None).expect("read")),
        vec!["web_fetch".to_string(), "web_search".to_string()]
    );
}

/// TC-PORT-WEB-28: `web_search` is registered whether or not a provider can
/// serve.
///
/// Upstream: "registers web_search even when no provider is available (schema
/// follows enablement, not availability)".
///
/// A tool that vanished when a key expired would change the model's behaviour
/// for a reason it can neither see nor report; a call that fails with
/// `WEB_PROVIDER_UNAVAILABLE` is a sentence somebody can act on.
///
/// Input: search enabled with no key anywhere.
/// Expected: the tool is registered, and its runtime resolves to nothing.
#[test]
fn web_search_is_registered_whether_or_not_a_provider_can_serve() {
    let enabled = document(&[(key::SEARCH_ENABLED, serde_json::json!(true))]);
    assert_eq!(
        names(&settings::tools(&enabled, None).expect("read")),
        vec!["web_search".to_string()]
    );

    let runtime = settings::runtime(&enabled, None).expect("read");
    assert_eq!(
        runtime.registered(),
        vec!["deepseek".to_string()],
        "the provider is registered; it is simply not usable"
    );
    assert!(
        runtime.resolve().is_err(),
        "and resolving says so rather than dispatching"
    );
}

/// TC-PORT-WEB-29: the limits are the document's, and a limit that is not one
/// is refused.
///
/// Upstream: "rejects a non-positive resource limit at construction", "rejects
/// a zero timeout at construction", "rejects a fractional redirect cap",
/// "accepts maxRedirects: 0 (follow no redirects) as valid config".
///
/// Input: a document setting each limit; one setting a zero byte cap; one
/// setting a negative hop cap; and one setting no redirects at all.
/// Expected: the values as written; refusals naming the two impossible keys;
/// and a hop cap of zero accepted, because following no redirect is a thing to
/// ask for.
#[test]
fn the_limits_are_the_documents_and_a_limit_that_is_not_one_is_refused() {
    let written = document(&[
        (key::MAX_BYTES, serde_json::json!(2048)),
        (key::MAX_CHARS, serde_json::json!(512)),
        (key::MAX_REDIRECTS, serde_json::json!(1)),
        (key::TIMEOUT, serde_json::json!(1500)),
    ]);
    let limits = settings::limits(&written).expect("read");
    assert_eq!(limits.max_bytes, 2048);
    assert_eq!(limits.max_chars, 512);
    assert_eq!(limits.max_redirects, 1);
    assert_eq!(limits.timeout, Duration::from_millis(1500));

    for (key, value) in [
        (key::MAX_BYTES, serde_json::json!(0)),
        (key::TIMEOUT, serde_json::json!(0)),
        (key::MAX_REDIRECTS, serde_json::json!(-1)),
    ] {
        let refused = settings::limits(&document(&[(key, value)]))
            .expect_err("an impossible limit is refused where it is written");
        assert!(refused.to_string().contains(key), "{refused}");
    }

    let none = document(&[(key::MAX_REDIRECTS, serde_json::json!(0))]);
    assert_eq!(
        settings::limits(&none)
            .expect("zero is a cap")
            .max_redirects,
        0
    );
}

/// TC-PORT-WEB-30: the key comes from the document, or from the environment
/// behind it.
///
/// Upstream: "falls back to the env key and defaults when config omits them",
/// "resolves the credential for each search so a stored or rotated key needs
/// no restart" - the first half, which is the half a document can serve.
///
/// Input: a document with a key; a document with none and an environment key;
/// neither.
/// Expected: usable, usable, unusable - read through the runtime, since the
/// key itself is never handed back out.
#[test]
fn the_key_comes_from_the_document_or_from_the_environment_behind_it() {
    let written = document(&[(key::DEEPSEEK_KEY, serde_json::json!("from-the-document"))]);
    assert!(settings::runtime(&written, None)
        .expect("read")
        .resolve()
        .is_ok());

    assert!(
        settings::runtime(&Config::default(), Some("from-the-environment"))
            .expect("read")
            .resolve()
            .is_ok()
    );

    assert!(
        settings::runtime(&Config::default(), Some("   "))
            .expect("read")
            .resolve()
            .is_err(),
        "a blank environment value is not a credential"
    );
}

/// TC-PORT-WEB-31: a configured provider that is not registered is refused
/// when a search is resolved.
///
/// Upstream: "throws WEB_PROVIDER_CONFIGURED_MISSING for an unregistered
/// configured id" - reached here through the document rather than through a
/// composer.
///
/// Input: a document naming `exa`, which this build does not carry.
/// Expected: the runtime builds, and resolving names both what was asked for
/// and what there is.
#[test]
fn a_configured_provider_that_is_not_registered_is_refused_when_a_search_resolves() {
    let settings = document(&[
        (key::SEARCH_ENABLED, serde_json::json!(true)),
        (key::PROVIDER, serde_json::json!("exa")),
        (key::DEEPSEEK_KEY, serde_json::json!("a-key")),
    ]);
    let runtime = settings::runtime(&settings, None).expect("read");
    let refused = match runtime.resolve() {
        Err(fault) => fault,
        Ok(_) => panic!("a provider this build does not carry was resolved"),
    };
    assert_eq!(refused.code(), tetanus_web::fault::code::CONFIGURED_MISSING);
    assert!(refused.to_string().contains("exa"), "{refused}");
    assert!(refused.to_string().contains("deepseek"), "{refused}");
}
