//! Test Design Specification: which settings keys name a credential.
//!
//! Feature under test: `tetanus_config::secret::names_a_secret`, the rule
//! section 4.3 of `docs/interface-contract.md` states for `config.dump`.
//!
//! Approach: the rule is a name test, so the cases are names. Each case states
//! a class of name rather than one example, because the rule is applied to
//! whatever a user writes and a single example would pass a rule that only
//! matched it. The two directions are tested apart: what must be withheld, and
//! what must still be shown.
//!
//! Features NOT tested here: what a caller does with the answer. The engine
//! withholding the value and keeping the entry is `crates/engine/tests/catalog.rs`
//! (TC-CFG-SECRET-1..4).
//!
//! Environmental needs: none. No case reads a file, a network or an
//! environment variable.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_config::secret::names_a_secret;

/// TC-SECRET-1: a key whose last word is one of the five holds a credential.
///
/// Input: the five words, each written the ways a document writes a key -
/// snake case, camel case, kebab case, and on its own.
/// Expected: every one is a secret. A separator and a capital both start a
/// word, so `api_key`, `apiKey` and `APIKey` are one name to this rule and a
/// document is not made safe by its spelling.
#[test]
fn a_key_that_names_a_credential_is_a_secret() {
    for key in [
        "llm.providers.deepseek.api_key",
        "llm.providers.deepseek.apiKey",
        "llm.providers.deepseek.APIKey",
        "llm.providers.deepseek.API-KEY",
        "llm.providers.deepseek.key",
        "llm.providers.acme.client_secret",
        "llm.providers.acme.auth_token",
        "host.password",
        "host.credential",
        "token",
    ] {
        assert!(names_a_secret(key), "`{key}` holds a credential");
    }
}

/// TC-SECRET-2: a key that only mentions a credential is not one.
///
/// Input: the near misses a real document has - the name of the environment
/// variable a key is read from, a token budget, a path - and a word that only
/// ends in one of the five.
/// Expected: none is a secret. This is the direction that costs a user
/// something when it is wrong: a withheld value is a setting they wrote and
/// can no longer read back, so the rule reads the last word rather than
/// looking for one anywhere in the name.
#[test]
fn a_key_that_only_mentions_a_credential_is_not_one() {
    for key in [
        "llm.providers.deepseek.api_key_env",
        "llm.providers.deepseek.apiKeyEnv",
        "agent.max_tokens",
        "llm.token_budget",
        "secret_path",
        "ui.monkey",
        "sessions.root",
        "provider.default",
    ] {
        assert!(!names_a_secret(key), "`{key}` holds no credential");
    }
}

/// TC-SECRET-3: only the last word decides.
///
/// Input: keys whose credential word is a section name rather than the leaf.
/// Expected: none is a secret. A rule that looked anywhere in the key would
/// hide every setting under a section called `credentials`, which is where a
/// document is most likely to also hold the settings a user has to see.
#[test]
fn only_the_last_word_decides() {
    assert!(!names_a_secret("credentials.deepseek.base_url"));
    assert!(!names_a_secret("secret.provider"));
    assert!(!names_a_secret("token.model"));
    assert!(names_a_secret("credentials.deepseek.token"));
}

/// TC-SECRET-4: a key that is not a key answers, and answers no.
///
/// Input: the empty key, a key that is one dot, and a key that ends in one.
/// Expected: none is a secret, and none panics. The rule is applied to
/// whatever a document holds, and a document is not required to hold a
/// well-formed key for a dump to be answerable.
#[test]
fn a_malformed_key_is_not_a_secret() {
    for key in ["", ".", "llm.", "..."] {
        assert!(!names_a_secret(key), "`{key}` holds no credential");
    }
}
