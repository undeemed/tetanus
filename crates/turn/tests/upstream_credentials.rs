//! Test Design Specification: judging a stored credential.
//!
//! Feature under test: [`tetanus_turn::llm::deepseek::normalize_api_key`], the
//! decision upstream pins in `packages/llm/llm/tests/api-key.spec.ts` against
//! `normalizeApiKey` and `assertUsableApiKey`: whether what the environment
//! holds can be carried on the wire, and what to say when it cannot.
//!
//! Approach: the judgement is a pure function of the stored string, so most
//! cases state a string and the verdict it earns. TC-PORT-KEY-6 goes through
//! the adapter as well, because a judgement the adapter does not apply is not
//! a judgement.
//!
//! Environmental needs: none. TC-PORT-KEY-6 writes one variable it owns, and
//! is the only case in this file that touches the environment, so no case here
//! can observe another mid-write.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use tetanus_turn::llm::deepseek::{
    normalize_api_key, DeepSeekAdapter, DeepSeekConfig, ReplayTransport,
};
use tetanus_turn::llm::{CollectingSink, LlmAdapter, LlmError, Message, ModelRequest};

const REFERENCE: &str = "DEEPSEEK_API_KEY";

/// A variable only TC-PORT-KEY-6 reads. The real name is left alone so that
/// nothing here decides whether the live case in `deepseek_adapter.rs` skips.
const TEST_API_KEY_ENV: &str = "TETANUS_TEST_BLANK_KEY";

/// TC-PORT-KEY-1: a printable-ASCII key is carried as it was stored.
///
/// Upstream: `api-key.spec.ts`, "accepts a printable-ASCII key unchanged" and
/// "accepts the printable-ASCII boundary characters".
///
/// Input: an ordinary key, then the two ends of the allowed range.
/// Expected: each comes back unchanged. The boundaries are asserted because an
/// off-by-one in the range check would refuse real keys and only show up in
/// production.
#[test]
fn a_printable_ascii_key_is_carried_unchanged() {
    assert_eq!(usable("sk-0123456789abcdef"), "sk-0123456789abcdef");
    assert_eq!(usable("!~"), "!~");
}

/// TC-PORT-KEY-2: surrounding whitespace is removed before judging.
///
/// Upstream: `api-key.spec.ts`, "trims surrounding whitespace before judging"
/// and "returns the trimmed key when it is usable".
///
/// Input: a key with leading spaces and a trailing tab and newline.
/// Expected: the bare key. A key read from a file or pasted into a shell
/// arrives with a newline often enough that refusing it would be a support
/// burden, and sending it would be a header the provider rejects.
#[test]
fn surrounding_whitespace_is_removed_before_judging() {
    assert_eq!(usable("  sk-abc\t\n"), "sk-abc");
}

/// TC-PORT-KEY-3: a key of nothing but whitespace reads as absent.
///
/// Upstream: `api-key.spec.ts`, "rejects an empty string / spaces only / a tab
/// only as empty", and "refuses a blank stored credential, naming the
/// reference".
///
/// Input: the empty string, spaces, and a tab.
/// Expected: `MissingCredential` for each, naming the reference. Blank is not
/// a wrong key, it is no key: reporting it as invalid would send the reader
/// looking for a typo in a value that was never set.
#[test]
fn a_blank_credential_reads_as_absent_not_as_wrong() {
    for raw in ["", "   ", "\t", "\n "] {
        let err = refused(raw);
        assert!(
            matches!(err, LlmError::MissingCredential(ref r) if r == REFERENCE),
            "{raw:?} gave {err}"
        );
    }
}

/// TC-PORT-KEY-4: a key an HTTP header cannot carry is refused as invalid.
///
/// Upstream: `api-key.spec.ts`, "rejects an emoji / CJK text / full-width
/// punctuation / an interior space / a C0 control character / a latin-1
/// character as illegal characters".
///
/// Input: each of those six shapes.
/// Expected: `InvalidCredential` for each. An interior space is in the list
/// because it does not fail a byte check: it silently makes a second header
/// token, and the provider then reports a key that does not match anything.
#[test]
fn a_key_no_header_can_carry_is_refused_as_invalid() {
    for raw in [
        "sk-\u{1F600}abc",
        "sk-你好",
        "sk-abc，",
        "sk-abc def",
        "sk-abc\u{01}",
        "sk-café",
    ] {
        let err = refused(raw);
        assert!(
            matches!(err, LlmError::InvalidCredential(ref r) if r == REFERENCE),
            "{raw:?} gave {err}"
        );
    }
}

/// TC-PORT-KEY-5: a refusal names the reference and never the key.
///
/// Upstream: `api-key.spec.ts`, "never echoes the key it refuses", and the
/// two cases asserting the message names the reference.
///
/// Input: an illegal key carrying a recognisable secret.
/// Expected: the message names `DEEPSEEK_API_KEY` and holds no part of the
/// key. An error is copied into a log, a bug report and a screenshot, so a key
/// that reaches the message is a key that has leaked.
#[test]
fn a_refusal_names_the_reference_and_never_the_key() {
    let message = refused("sk-\u{1F600}supersecret").to_string();

    assert!(message.contains(REFERENCE), "{message}");
    assert!(!message.contains("supersecret"), "{message}");
    assert!(!message.contains("sk-"), "{message}");
}

/// TC-PORT-KEY-6: the adapter applies the same judgement to the environment.
///
/// Upstream: `assertUsableApiKey`, which the adapters call on the credential
/// they resolved rather than judging it themselves.
///
/// Input: the credential variable set to a single space, then a request.
/// Expected: `MissingCredential`, and the transport never called. A blank
/// value that reached the wire would spend a real request to be told what was
/// knowable before it: previously it did.
#[tokio::test]
async fn the_adapter_judges_what_the_environment_holds() {
    std::env::set_var(TEST_API_KEY_ENV, " ");
    let config = DeepSeekConfig {
        api_key_env: TEST_API_KEY_ENV.to_string(),
        ..DeepSeekConfig::default()
    };
    let transport = Arc::new(ReplayTransport::new(["[DONE]"]));
    let adapter = DeepSeekAdapter::new(config, transport.clone());

    let err = adapter
        .stream(&request(), &mut CollectingSink::default())
        .await
        .expect_err("a blank key is no key");

    assert!(
        matches!(err, LlmError::MissingCredential(ref r) if r == TEST_API_KEY_ENV),
        "{err}"
    );
    assert!(transport.last_body().is_none(), "no request may be sent");

    std::env::remove_var(TEST_API_KEY_ENV);
}

fn usable(raw: &str) -> String {
    normalize_api_key(raw, REFERENCE).expect("a usable key")
}

fn refused(raw: &str) -> LlmError {
    normalize_api_key(raw, REFERENCE).expect_err("an unusable key")
}

fn request() -> ModelRequest {
    ModelRequest {
        provider: "deepseek-official".to_string(),
        model: "deepseek-v4-flash".to_string(),
        messages: vec![Message::user("hi")],
        tools: Vec::new(),
        max_tokens: None,
    }
}
