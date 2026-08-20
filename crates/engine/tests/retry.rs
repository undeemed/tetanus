//! Test Design Specification: the retry policy a document configures.
//!
//! Features under test: [`tetanus_engine::retry::policy`], which turns the
//! settings document into the policy the engine will run on, and
//! [`tetanus_engine::retry::provider_policy`] with
//! [`tetanus_engine::retry::provider_policies`], which do the same for the
//! block one provider writes for itself. Upstream's `resolveRetryPolicy`
//! (`packages/llm/llm/src/retry-policy.ts`) is the rule set being ported.
//!
//! Approach: every case reads a real document off disk, because the key
//! constants are only correct if the reader's flattening produces them from
//! the nesting a reader would write.
//!
//! Features NOT tested here: what the executor does with a policy
//! (`crates/turn/tests/upstream_retry_executor.rs`), what the policy decides
//! (`upstream_retry_policy.rs`), and the installation of a resolved policy on
//! a live route - the general one is `crates/engine/tests/retry_route.rs`, and
//! a per-provider one is installed by the slice that reads these calls. None
//! is restated.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeMap;

use tempfile::TempDir;
use tetanus_config::ConfigError;
use tetanus_engine::retry::key;
use tetanus_engine::{boot, retry, EngineConfig, HarnessEngine};
use tetanus_protocol::methods::Engine;
use tetanus_protocol::types::ConfigLayer;
use tetanus_turn::llm::retry::{Backoff, RetryPolicy, DEFAULT_RETRYABLE_CODES};

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

/// TC-RETRY-8: a document with no provider block has no per-provider policy.
///
/// Input: a document that configures the general block only.
/// Expected: `provider_policies` is empty and `provider_policy` is `None` for
/// the provider that block would otherwise cover. A general block that also
/// counted as every provider's own would make the two indistinguishable, and
/// the route could never fall back.
#[test]
fn a_general_block_is_no_provider_s_own() {
    let (_dir, settings) = document(
        "llm:
  retry:
    mode: always",
    );

    assert!(retry::provider_policies(&settings)
        .expect("resolve")
        .is_empty());
    assert_eq!(
        retry::provider_policy(&settings, "deepseek").expect("resolve"),
        None
    );
}

/// TC-RETRY-9: each provider's block resolves to that provider's policy.
///
/// Input: two nested provider blocks, one unbounded and one bounded, written
/// as a reader would write them.
/// Expected: a map of exactly those two names, each carrying the policy its
/// own block states, and `None` for a provider neither block names.
#[test]
fn each_provider_block_resolves_to_its_own_policy() {
    let (_dir, settings) = document(
        "llm:
  providers:
    deepseek:
      retry:
        mode: always
        backoff:
          initial_delay_ms: 25
          max_delay_ms: 250
          jitter_ratio: 0
    mock:
      retry:
        max_retries: 1
        retryable_codes: [TIMEOUT]",
    );

    let deepseek = RetryPolicy::Always {
        backoff: Backoff {
            initial_delay_ms: 25.0,
            max_delay_ms: 250.0,
            jitter_ratio: 0.0,
        },
    };
    let mock = RetryPolicy::Normal {
        max_retries: 1,
        retryable_codes: vec!["TIMEOUT".to_string()],
        backoff: Backoff::default(),
    };

    assert_eq!(
        retry::provider_policies(&settings).expect("resolve"),
        BTreeMap::from([
            ("deepseek".to_string(), deepseek.clone()),
            ("mock".to_string(), mock.clone()),
        ])
    );
    assert_eq!(
        retry::provider_policy(&settings, "deepseek").expect("resolve"),
        Some(deepseek)
    );
    assert_eq!(
        retry::provider_policy(&settings, "mock").expect("resolve"),
        Some(mock)
    );
    assert_eq!(
        retry::provider_policy(&settings, "absent").expect("resolve"),
        None
    );
}

/// TC-RETRY-10: a provider block is the whole policy for its route.
///
/// Input: a general block with a bound and its own waits, beside a provider
/// block that only names the unbounded mode.
/// Expected: the provider's policy is unbounded with the compiled backoff, not
/// the general block's waits, and the general policy is unchanged. Layering
/// the two would hand `mode: always` a `max_retries` its author never wrote,
/// which TC-RETRY-5 refuses.
#[test]
fn a_provider_block_does_not_inherit_the_general_one() {
    let (_dir, settings) = document(
        "llm:
  retry:
    max_retries: 7
    backoff:
      initial_delay_ms: 20
      max_delay_ms: 99
  providers:
    deepseek:
      retry:
        mode: always",
    );

    assert_eq!(
        retry::provider_policy(&settings, "deepseek").expect("resolve"),
        Some(RetryPolicy::Always {
            backoff: Backoff::default(),
        })
    );
    assert_eq!(
        retry::policy(&settings).expect("resolve"),
        RetryPolicy::Normal {
            max_retries: 7,
            retryable_codes: DEFAULT_RETRYABLE_CODES
                .iter()
                .map(|code| code.to_string())
                .collect(),
            backoff: Backoff {
                initial_delay_ms: 20.0,
                max_delay_ms: 99.0,
                ..Backoff::default()
            },
        }
    );
}

/// TC-RETRY-11: a provider block is held to every rule the general one is.
///
/// Input: TC-RETRY-4's refusal table and TC-RETRY-5's unusable bound, written
/// under one provider instead of under `llm.retry`.
/// Expected: each is `BadValue` naming that provider's own key, so the message
/// points at the block the reader has to edit rather than at the general one.
#[test]
fn a_provider_block_is_refused_by_the_same_rules() {
    let block = retry::key::provider_retry("deepseek");
    let refused = [
        ("mode: sometimes", format!("{block}.mode")),
        ("max_retries: -1", format!("{block}.max_retries")),
        ("retryable_codes: []", format!("{block}.retryable_codes")),
        (
            "retryable_codes: [SERVER, SERVER]",
            format!("{block}.retryable_codes"),
        ),
        (
            "backoff: {initial_delay_ms: 0}",
            format!("{block}.backoff.initial_delay_ms"),
        ),
        (
            "backoff: {max_delay_ms: soon}",
            format!("{block}.backoff.max_delay_ms"),
        ),
        (
            "backoff: {jitter_ratio: 1.5}",
            format!("{block}.backoff.jitter_ratio"),
        ),
        (
            "backoff: {initial_delay_ms: 900, max_delay_ms: 100}",
            format!("{block}.backoff.initial_delay_ms"),
        ),
        (
            "mode: always\n        max_retries: 3",
            format!("{block}.max_retries"),
        ),
    ];

    for (setting, expected) in refused {
        let (_dir, settings) = document(&format!(
            "llm:\n  providers:\n    deepseek:\n      retry:\n        {setting}"
        ));
        let Err(ConfigError::BadValue { key, .. }) = retry::provider_policy(&settings, "deepseek")
        else {
            panic!("{setting} was accepted");
        };
        assert_eq!(key, expected, "{setting}");
        let Err(ConfigError::BadValue { key, .. }) = retry::provider_policies(&settings) else {
            panic!("{setting} was accepted for the whole document");
        };
        assert_eq!(key, expected, "{setting}");
    }
}

/// TC-RETRY-12: a block written under no provider name is refused.
///
/// Input: `llm.providers` with an empty name holding a retry block.
/// Expected: `BadValue` naming the flattened key. A nameless block matches no
/// route, so accepting it would silently configure nothing.
#[test]
fn a_block_under_no_name_is_refused() {
    let (_dir, settings) = document(
        "llm:
  providers:
    \"\":
      retry:
        mode: always",
    );

    let Err(ConfigError::BadValue { key, .. }) = retry::provider_policies(&settings) else {
        panic!("the nameless block was accepted");
    };
    assert_eq!(key, "llm.providers..retry.mode");
}

/// TC-RETRY-13: the general block's published keys are the prefixed six.
///
/// Input: the constants `crate::retry::key` publishes.
/// Expected: each is `llm.retry` plus its own suffix. The resolver builds both
/// blocks' keys from a prefix, so this is what pins the published names to the
/// ones it reads.
#[test]
fn the_published_keys_are_the_general_prefix_suffixed() {
    assert_eq!(key::RETRY, "llm.retry");
    for (published, suffix) in [
        (key::MODE, "mode"),
        (key::MAX_RETRIES, "max_retries"),
        (key::RETRYABLE_CODES, "retryable_codes"),
        (key::INITIAL_DELAY_MS, "backoff.initial_delay_ms"),
        (key::MAX_DELAY_MS, "backoff.max_delay_ms"),
        (key::JITTER_RATIO, "backoff.jitter_ratio"),
    ] {
        assert_eq!(published, format!("{}.{suffix}", key::RETRY));
    }
    assert_eq!(
        retry::key::provider_retry("deepseek"),
        "llm.providers.deepseek.retry"
    );
}
