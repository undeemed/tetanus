//! Test Design Specification: telling a terminal provider refusal from a
//! transient one, ported.
//!
//! Feature under test: `LlmError::code`, and the two classifiers behind it -
//! `names_exhausted_quota` and `names_context_overflow`. Upstream pins the
//! same wordings in `packages/llm/llm/src/error.ts`
//! (`isQuotaExceededError`, `isContextWindowExceededError`), used by its
//! DeepSeek adapter; `docs/parity.md` names both as `llm/*` gaps.
//!
//! Why it matters: an exhausted account arrives as a 429, the same status a
//! provider uses to say "slow down". Before this distinction the two were one
//! code, so a dead key spent the whole retry budget - every attempt and every
//! backoff - and then reported `RATE_LIMIT`, sending the reader after a
//! throughput problem instead of a billing one.
//!
//! Approach: the classifiers are pure, so the wording cases are literal
//! strings in and a decision out, taken from the phrasings providers actually
//! use. The routing cases go through `LlmError::code` so the classification is
//! asserted where a caller meets it, and the retry cases go through the
//! default retryable set so "terminal" is asserted as behaviour rather than as
//! a name.
//!
//! What is not restated: upstream matches with regular expressions and this
//! normalizes then matches phrases, because the workspace has no regex
//! dependency. The accepted wordings are the same, and TC-PORT-QUOTA-2 pins
//! the normalization that makes them so. Upstream's provider request id is a
//! separate gap and is not part of this.
//!
//! Environmental needs: none. No case touches a filesystem, a network or an
//! API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_turn::llm::retry::DEFAULT_RETRYABLE_CODES;
use tetanus_turn::llm::{
    names_context_overflow, names_exhausted_quota, LlmError, CONTEXT_WINDOW_EXCEEDED, QUOTA,
};

/// TC-PORT-QUOTA-1: the wordings providers use for an exhausted account are
/// recognised, and a rate limit is not.
///
/// Upstream: `isQuotaExceededError`.
///
/// The negative half is the one that keeps this safe. A classifier that leaned
/// towards "terminal" would turn an ordinary rate limit into a failed turn,
/// which is a worse outcome than the retry-budget waste it was written to
/// prevent.
///
/// Input: the quota and balance phrasings providers send, then the throttling
/// phrasings they send for a 429 that a backoff fixes.
/// Expected: every quota wording recognised; not one throttling wording
/// recognised.
#[test]
fn exhausted_account_wordings_are_recognised_and_throttling_is_not() {
    for terminal in [
        "insufficient_quota",
        "Insufficient Quota",
        "insufficient-balance",
        "You have insufficient credits to complete this request.",
        "Your quota has been exceeded.",
        "quota_exhausted",
        "You exceeded your current quota, please check your plan and billing details.",
        "monthly limit reached",
        "usage limit exceeded",
        "Your balance is exhausted.",
        "You are out of credits.",
        "out_of_budget",
    ] {
        assert!(
            names_exhausted_quota(terminal),
            "should be terminal: {terminal:?}"
        );
    }

    for transient in [
        "Rate limit reached for requests",
        "Too many requests, please slow down.",
        "rate_limit_exceeded",
        "Request rate exceeded, retry after 2s",
        "429 Too Many Requests",
        "The server had an error processing your request.",
        "Service temporarily unavailable.",
        "",
    ] {
        assert!(
            !names_exhausted_quota(transient),
            "should not be terminal: {transient:?}"
        );
    }
}

/// TC-PORT-QUOTA-2: the same fact spelled three ways is one fact.
///
/// Providers send `insufficient_quota`, `insufficient-quota` and `Insufficient
/// Quota`, and a classifier written against one of them works for one of them.
/// Normalizing first is what makes the phrase list short enough to read and
/// wide enough to be useful.
///
/// Input: one wording in several separators and cases, and the same phrase
/// broken across extra whitespace.
/// Expected: recognised every way.
#[test]
fn separators_and_case_do_not_change_the_answer() {
    for spelling in [
        "insufficient_quota",
        "insufficient-quota",
        "Insufficient Quota",
        "INSUFFICIENT_QUOTA",
        "  insufficient   quota  ",
        "error: insufficient_quota (code 429)",
    ] {
        assert!(names_exhausted_quota(spelling), "{spelling:?}");
    }
}

/// TC-PORT-QUOTA-3: a request too big for the model's context is recognised.
///
/// Upstream: `isContextWindowExceededError`.
///
/// Terminal for a plainer reason than quota: the same request will not fit
/// next time either. What fixes it is sending less, which is a decision above
/// this seam.
///
/// Input: the context-overflow phrasings providers send, then messages that
/// mention length or limits without meaning the context window.
/// Expected: the overflow wordings recognised, and the near-misses not - a
/// provider whose own queue is too long has not said this.
#[test]
fn a_request_too_big_for_the_context_is_recognised() {
    for overflow in [
        "context_length_exceeded",
        "This model's maximum context length is 65536 tokens.",
        "context window exceeded",
        "Your prompt is too long for this model.",
        "The request is too large for the context window.",
        "input exceeds the model's context length",
    ] {
        assert!(names_context_overflow(overflow), "{overflow:?}");
    }

    for other in [
        "rate_limit_exceeded",
        "insufficient_quota",
        "The queue is too long, try again later.",
        "Value too long for column name.",
        "maximum retries exceeded",
        "",
    ] {
        assert!(!names_context_overflow(other), "{other:?}");
    }
}

/// TC-PORT-QUOTA-4: what the provider said is read before the status it said
/// it under.
///
/// The status is the coarser fact: 429 is the same number for an empty account
/// and for a caller going too fast. Reading the detail first is what lets one
/// status produce two answers, and it is the whole mechanism.
///
/// Input: two 429s differing only in their message, a 400 carrying context
/// overflow, and a plain 429 and 500 carrying nothing distinctive.
/// Expected: `QUOTA`, `RATE_LIMIT`, `CONTEXT_WINDOW_EXCEEDED`, then the
/// status-derived codes unchanged - so the classification adds answers without
/// disturbing the ones that were already right.
#[test]
fn the_detail_is_read_before_the_status() {
    assert_eq!(provider(429, "insufficient_quota").code(), QUOTA);
    assert_eq!(provider(429, "Rate limit reached").code(), "RATE_LIMIT");
    assert_eq!(
        provider(400, "context_length_exceeded").code(),
        CONTEXT_WINDOW_EXCEEDED
    );

    assert_eq!(provider(429, "").code(), "RATE_LIMIT");
    assert_eq!(provider(500, "").code(), "SERVER");
    assert_eq!(provider(408, "").code(), "TIMEOUT");
    assert_eq!(provider(418, "").code(), "PROVIDER");
}

/// TC-PORT-QUOTA-5: a terminal refusal is one the retry policy will not
/// repeat.
///
/// This asserts "terminal" as behaviour rather than as a name. A code that
/// sounded final but sat in the retryable set would waste exactly the budget
/// this distinction exists to save, and nothing else in the suite would catch
/// it.
///
/// Expected: neither new code is in the default retryable set, while the codes
/// that describe a passing condition still are.
#[test]
fn a_terminal_refusal_is_not_retried() {
    for terminal in [QUOTA, CONTEXT_WINDOW_EXCEEDED] {
        assert!(
            !DEFAULT_RETRYABLE_CODES.contains(&terminal),
            "{terminal} must not be retried: asking again cannot change the answer"
        );
    }

    for transient in ["RATE_LIMIT", "SERVER", "TIMEOUT", "TRANSPORT"] {
        assert!(
            DEFAULT_RETRYABLE_CODES.contains(&transient),
            "{transient} is still worth another attempt"
        );
    }
}

/// TC-PORT-QUOTA-6: a quota refusal keeps the rest of what it carried.
///
/// The classification changes what the failure is called, and must change
/// nothing else. A provider that sent a `Retry-After` with its 429 still sent
/// it, and a reader showing the provider's own words should see them whatever
/// this build decided the code was.
///
/// Input: a quota refusal carrying a provider-asked wait.
/// Expected: the code is `QUOTA`, the wait is still readable, and the
/// provider's message is still in the rendered error.
#[test]
fn classification_changes_the_code_and_nothing_else() {
    let refused = LlmError::Provider {
        status: 429,
        message: "insufficient_quota: add credits to continue".to_string(),
        retry_after_ms: Some(1_500.0),
        request_id: None,
    };

    assert_eq!(refused.code(), QUOTA);
    assert_eq!(
        refused.retry_after_ms(),
        Some(1_500.0),
        "what the provider asked for is still what it asked for"
    );
    assert!(
        refused.to_string().contains("insufficient_quota"),
        "and its own words still reach a reader: {refused}"
    );
}

fn provider(status: u16, message: &str) -> LlmError {
    LlmError::Provider {
        status,
        message: message.to_string(),
        retry_after_ms: None,
        request_id: None,
    }
}
