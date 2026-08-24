//! Test Design Specification: the provider's own id for a refused request.
//!
//! Feature under test: `LlmError::Provider.request_id`, the header rule that
//! fills it, and the path it takes to a listener through `RequestFailure`.
//! Upstream keeps the same fact on its `LlmFailure`
//! (`packages/llm/llm/src/types.ts`), reads it off a refusal in its DeepSeek
//! adapter and carries it as far as its durable retry record;
//! `docs/parity.md` has named it as the `llm/*` row's remaining gap since the
//! row was written.
//!
//! Why it matters, stated once: the status is on the response and the message
//! is in the body, but the request id exists only in the provider's own logs.
//! It is the single thing a user can quote to a provider's support, and
//! discarding it makes "my request failed and nobody can tell me why"
//! unanswerable by anyone.
//!
//! Approach: the header rule is a pure function of a lookup and is tested as
//! one, so the preference order and the trimming are pinned without a socket.
//! The transport half is in `deepseek_adapter.rs`, beside the rest of the
//! cases that read a real response off a loopback endpoint
//! (TC-PORT-REQID-1 and -2).
//!
//! Features NOT tested here: what the boundary publishes about a refusal,
//! which is `crates/engine/tests/faults.rs`, and what the journal records for
//! a retried one, which is `upstream_retry_executor.rs`.
//!
//! Environmental needs: none. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_turn::events::RequestFailure;
use tetanus_turn::llm::{request_id_from, LlmError, REQUEST_ID_HEADERS};

/// TC-REQID-1: the id is read from the headers a provider actually uses.
///
/// There is no standard header for this and each provider picked its own, so
/// the rule is a list rather than a name.
///
/// Input: a lookup that answers one known spelling, then one that answers a
/// header no provider in the list sends.
/// Expected: each known spelling is found, and the unknown one is not guessed
/// at.
#[test]
fn the_id_is_read_from_the_headers_providers_use() {
    for name in REQUEST_ID_HEADERS {
        let found = request_id_from(|asked| (asked == name).then_some("req-abc123"));
        assert_eq!(found.as_deref(), Some("req-abc123"), "{name} is not read");
    }
    assert_eq!(
        request_id_from(|asked| (asked == "x-correlation-id").then_some("nope")),
        None,
        "a header nobody sends is not guessed at"
    );
}

/// TC-REQID-2: the most specific header wins when a provider sends several.
///
/// A response behind a CDN carries the CDN's ray id as well as the provider's
/// own. Quoting the CDN's to the provider's support is useless, so the order
/// in the list is load-bearing rather than incidental - and the ray id is
/// still worth keeping as the last resort, because a refusal generated at the
/// edge carries nothing else.
///
/// Input: a lookup answering both `x-request-id` and `cf-ray`, then one
/// answering `cf-ray` alone.
/// Expected: the provider's own id when both are there, the ray id when it is
/// all there is.
#[test]
fn the_most_specific_header_wins() {
    let both = |name: &str| match name {
        "x-request-id" => Some("the-provider-id"),
        "cf-ray" => Some("the-cdn-id"),
        _ => None,
    };
    assert_eq!(request_id_from(both).as_deref(), Some("the-provider-id"));

    let edge = |name: &str| (name == "cf-ray").then_some("the-cdn-id");
    assert_eq!(
        request_id_from(edge).as_deref(),
        Some("the-cdn-id"),
        "an edge refusal reached no provider, so its ray id is the only id there is"
    );
}

/// TC-REQID-3: a header present and empty is no id at all, and does not hide
/// one that follows it.
///
/// Upstream refuses an empty `requestId` where the failure is constructed
/// (`packages/llm/llm/src/index.ts`) and again where the retry record is
/// validated (`llm-retry/src/invariant.ts`). tetanus has no constructor to
/// refuse in, so the rule is enforced at the one place ids are made: an empty
/// string quoted to support is worse than saying there was no id, because it
/// looks like a value that was recorded.
///
/// Input: empty, whitespace-only and padded headers, then an empty preferred
/// header beside a real one further down the list.
/// Expected: `None` for the blank forms, the trimmed value for the padded one,
/// and the real id for the mixed case rather than the blank that outranks it.
#[test]
fn an_empty_header_is_no_id_and_hides_none() {
    assert_eq!(request_id_from(|_| Some("")), None);
    assert_eq!(request_id_from(|_| Some("   \t ")), None);
    assert_eq!(
        request_id_from(|_| Some("  req-padded  ")).as_deref(),
        Some("req-padded")
    );

    let blank_first = |name: &str| match name {
        "x-request-id" => Some(""),
        "cf-ray" => Some("the-cdn-id"),
        _ => None,
    };
    assert_eq!(
        request_id_from(blank_first).as_deref(),
        Some("the-cdn-id"),
        "a blank header is skipped, not taken as the answer"
    );
}

/// TC-REQID-4: the id is read through an accessor, and only a refusal has one.
///
/// The shape `retry_after_ms` already has: a caller that wants the id does not
/// have to know which variants can carry one.
///
/// Input: a refusal with an id, one without, and two failures that never
/// reached a provider.
/// Expected: the id for the first, `None` for the rest, with no match on the
/// enum required of the caller.
#[test]
fn only_a_provider_refusal_carries_an_id() {
    let refused = LlmError::Provider {
        status: 429,
        message: "slow down".into(),
        retry_after_ms: None,
        request_id: Some("req-42".into()),
    };
    assert_eq!(refused.request_id(), Some("req-42"));

    let anonymous = LlmError::Provider {
        status: 500,
        message: "oh dear".into(),
        retry_after_ms: None,
        request_id: None,
    };
    assert_eq!(anonymous.request_id(), None, "a provider may name none");

    for other in [
        LlmError::Transport("socket".into()),
        LlmError::Protocol("garbage".into()),
    ] {
        assert_eq!(other.request_id(), None, "{other} should carry no id");
    }
}

/// TC-PORT-REQID-3: the structured facts of a refusal survive to what the
/// caller is handed.
///
/// Upstream: `packages/llm/llm/tests/service.spec.ts`, "preserves structured
/// LlmError facts in the terminal failure" - there the facts arrive on the
/// terminal `finish` chunk, here on the `RequestFailure` that
/// `agent/request-error` hands a listener, which is the same seam: by the time
/// a failure reaches a log line the response it came on is long gone, so if it
/// is not on this struct it is not anywhere.
///
/// Input: a 429 carrying a wait and an id, then a 500 carrying neither.
/// Expected: the id, the wait and the classification all cross the conversion
/// together; a refusal with no id carries none rather than an empty string.
#[test]
fn the_structured_facts_reach_the_listener_together() {
    let carried = RequestFailure::from(&LlmError::Provider {
        status: 429,
        message: "rate limited".into(),
        retry_after_ms: Some(1_500.0),
        request_id: Some("req-listener".into()),
    });
    assert_eq!(carried.provider_request_id.as_deref(), Some("req-listener"));
    assert_eq!(carried.provider_retry_after_ms, Some(1_500.0));
    assert_eq!(carried.code, "RATE_LIMIT");

    let anonymous = RequestFailure::from(&LlmError::Provider {
        status: 500,
        message: "oh dear".into(),
        retry_after_ms: None,
        request_id: None,
    });
    assert_eq!(anonymous.provider_request_id, None);
}

/// TC-REQID-5: classification does not depend on the id, in either direction.
///
/// A new field on an error is a chance to change what that error means by
/// accident. The id is provenance and nothing else: it must not make a
/// refusal more or less retryable, or turn a rate limit into a quota failure.
///
/// Input: the statuses and messages whose classification differs, each built
/// with an id and without one.
/// Expected: the code is identical in every pair.
#[test]
fn the_id_changes_no_classification() {
    for (status, message) in [
        (429, "slow down"),
        (429, "insufficient quota"),
        (400, "bad request"),
        (500, "server error"),
        (408, "timeout"),
    ] {
        let without = LlmError::Provider {
            status,
            message: message.into(),
            retry_after_ms: None,
            request_id: None,
        };
        let with = LlmError::Provider {
            status,
            message: message.into(),
            retry_after_ms: None,
            request_id: Some("req-1".into()),
        };
        assert_eq!(
            without.code(),
            with.code(),
            "{status} {message:?} classified differently with an id"
        );
    }
}
