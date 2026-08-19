//! Test Design Specification: the retry policy a document configures.
//!
//! Feature under test: [`tetanus_engine::retry::policy`], which turns the
//! settings document into the policy the engine will run on. Upstream's
//! `resolveRetryPolicy` (`packages/llm/llm/src/retry-policy.ts`) is the rule
//! set being ported.
//!
//! Approach: every case reads a real document off disk, because the key
//! constants are only correct if the reader's flattening produces them from
//! the nesting a reader would write.
//!
//! Features NOT tested here: what the executor does with a policy
//! (`crates/turn/tests/upstream_retry_executor.rs`), what the policy decides
//! (`upstream_retry_policy.rs`), and the installation of the resolved policy
//! on a live route, which lands with the executor's caller. Neither is
//! restated.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_config::ConfigError;
use tetanus_engine::retry::key;
use tetanus_engine::{boot, retry, EngineConfig, HarnessEngine};
use tetanus_protocol::methods::Engine;
use tetanus_protocol::types::ConfigLayer;
use tetanus_turn::llm::retry::{Backoff, RetryPolicy};

/// TC-RETRY-1: a build with no document runs the compiled policy, and says so.
///
/// Input: an engine booted on a document that does not exist.
/// Expected: the policy is `RetryPolicy::default()`, and every key of the
/// policy appears in `config.dump` at the `Default` layer. A key the engine
/// reads but never publishes would be a setting nobody could discover.
#[tokio::test]
async fn no_document_is_the_compiled_policy_and_it_is_visible() {
    let dir = TempDir::new().expect("temp dir");
    let settings = boot::document(&dir.path().join("absent.yaml")).expect("no document");

    assert_eq!(
        retry::policy(&settings).expect("defaults"),
        RetryPolicy::default()
    );

    let engine = HarnessEngine::new(EngineConfig::from_settings(settings).expect("settings"));
    let dumped = engine.config_dump().await.expect("dump");
    for published in [
        key::MODE,
        key::MAX_RETRIES,
        key::RETRYABLE_CODES,
        key::INITIAL_DELAY_MS,
        key::MAX_DELAY_MS,
        key::JITTER_RATIO,
    ] {
        let entry = dumped
            .entries
            .iter()
            .find(|entry| entry.key == published)
            .unwrap_or_else(|| panic!("{published} is not published"));
        assert_eq!(entry.layer, ConfigLayer::Default, "{published}");
    }
}

/// TC-RETRY-2: the document's own nesting resolves to an unbounded policy.
///
/// Input: `llm: {retry: {mode: always, backoff: {...}}}`, written as a reader
/// would write it rather than as flat dotted keys.
/// Expected: `RetryPolicy::Always` carrying the three backoff numbers. The
/// case is written nested on purpose: the key constants are only correct if
/// the document reader flattens that shape into them.
#[test]
fn a_nested_document_resolves_an_unbounded_policy() {
    let (_dir, settings) = document(
        "llm:
  retry:
    mode: always
    backoff:
      initial_delay_ms: 25
      max_delay_ms: 250
      jitter_ratio: 0",
    );

    assert_eq!(
        retry::policy(&settings).expect("resolve"),
        RetryPolicy::Always {
            backoff: Backoff {
                initial_delay_ms: 25.0,
                max_delay_ms: 250.0,
                jitter_ratio: 0.0,
            },
        }
    );
}

/// TC-RETRY-3: a bounded policy takes the retries and the codes it was given.
///
/// Input: `mode: normal` with `max_retries: 0` and one failure code.
/// Expected: exactly those, with the compiled backoff. Zero is a policy and
/// not a mistake: it says this route never retries.
#[test]
fn a_bounded_policy_may_allow_no_retries_at_all() {
    let (_dir, settings) = document(
        "llm:
  retry:
    mode: normal
    max_retries: 0
    retryable_codes: [TIMEOUT]",
    );

    assert_eq!(
        retry::policy(&settings).expect("resolve"),
        RetryPolicy::Normal {
            max_retries: 0,
            retryable_codes: vec!["TIMEOUT".to_string()],
            backoff: Backoff::default(),
        }
    );
}

/// TC-RETRY-4: every value the policy refuses names the key that holds it.
///
/// Input: one document per rule upstream states - an unknown mode, a negative
/// retry count, an empty code list, a repeated code, a code that is not text,
/// a wait of zero, a wait that is not a number, a spread above one, and an
/// initial wait longer than the ceiling.
/// Expected: each is `ConfigError::BadValue` naming the stated key, so the
/// published mapping reports the line the reader has to edit. A rule that
/// refused nothing would let a document configure a policy the engine does not
/// run.
#[test]
fn every_refused_value_names_its_own_key() {
    let refused = [
        ("mode: sometimes", key::MODE),
        ("mode: normal\n    max_retries: -1", key::MAX_RETRIES),
        ("retryable_codes: []", key::RETRYABLE_CODES),
        ("retryable_codes: [SERVER, SERVER]", key::RETRYABLE_CODES),
        ("retryable_codes: [SERVER, 7]", key::RETRYABLE_CODES),
        ("backoff: {initial_delay_ms: 0}", key::INITIAL_DELAY_MS),
        ("backoff: {max_delay_ms: soon}", key::MAX_DELAY_MS),
        ("backoff: {jitter_ratio: 1.5}", key::JITTER_RATIO),
        (
            "backoff: {initial_delay_ms: 900, max_delay_ms: 100}",
            key::INITIAL_DELAY_MS,
        ),
    ];

    for (setting, expected) in refused {
        let (_dir, settings) = document(&format!("llm:\n  retry:\n    {setting}"));
        let Err(ConfigError::BadValue { key, .. }) = retry::policy(&settings) else {
            panic!("{setting} was accepted");
        };
        assert_eq!(key, expected, "{setting}");
    }
}

/// TC-RETRY-5: a bound written beside the unbounded mode is refused.
///
/// Input: `mode: always` with a `max_retries` beside it.
/// Expected: `BadValue` naming `max_retries`, not a policy that ignores it.
/// Its author expected a bound, and unbounded mode does not have one; the
/// compiled default sets the same key, so only a value a document set counts.
#[test]
fn a_bound_beside_the_unbounded_mode_is_refused() {
    let (_dir, settings) = document(
        "llm:
  retry:
    mode: always
    max_retries: 3",
    );

    let Err(ConfigError::BadValue { key, .. }) = retry::policy(&settings) else {
        panic!("the unusable bound was accepted");
    };
    assert_eq!(key, key::MAX_RETRIES);
}

/// A document holding `settings` under `llm.retry`, and the config it resolves
/// to over the engine's own defaults.
fn document(text: &str) -> (TempDir, tetanus_config::Config) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, text).expect("write");
    let settings = boot::document(&path).expect("read");
    (dir, settings)
}
