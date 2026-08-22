//! Test Design Specification: the published failure mapping.
//!
//! Feature under test: [`tetanus_engine::convert::turn_error`],
//! [`tetanus_engine::convert::journal_error`] and
//! [`tetanus_engine::convert::config_error`], the one implementation of
//! section 4.5 of `docs/interface-contract.md`. The contract now says a
//! surface calls this mapping rather than deriving a code from an engine error
//! type, so what it returns is a boundary promise and not an internal detail.
//!
//! Approach: one case per row of the error table that a turn can reach, each
//! stating the code, the `data` fields, and the exit status the table gives
//! that code. A case asserts the absent fields too: a `status` invented for a
//! provider that never answered would read as a real HTTP answer.
//!
//! Features NOT tested here: the codes no turn produces (`SessionBusy`,
//! `ToolUnknown`, the envelope codes), which their own suites cover.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;

use tetanus_config::ConfigError;
use tetanus_engine::convert::{config_error, journal_error, turn_error};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_session::SessionError;
use tetanus_turn::llm::LlmError;
use tetanus_turn::prompt::PromptError;
use tetanus_turn::TurnError;

const SESSION: &str = "s-1";
const PROVIDER: &str = "deepseek-official";

/// TC-FAULT-1: a credential fault names the variable to set.
///
/// Input: a missing credential, then an unusable one.
/// Expected: both are `MissingCredential` carrying `provider` and `env`, and
/// exit 5. The two are one code because the reader's next move is the same:
/// go and set that variable. The message never carries the key itself.
#[test]
fn a_credential_fault_names_the_variable_and_exits_five() {
    for error in [
        LlmError::MissingCredential("DEEPSEEK_API_KEY".to_string()),
        LlmError::InvalidCredential("DEEPSEEK_API_KEY".to_string()),
    ] {
        let fault = mapped(TurnError::Llm(error));

        assert_eq!(fault.kind(), Some(ErrorCode::MissingCredential));
        assert_eq!(data(&fault)["provider"], PROVIDER);
        assert_eq!(data(&fault)["env"], "DEEPSEEK_API_KEY");
        assert_eq!(ErrorCode::MissingCredential.exit_status(), 5);
    }
}

/// TC-FAULT-2: a provider that answered reports what it answered.
///
/// Input: an HTTP 429 from the provider.
/// Expected: `ProviderError` carrying `provider` and `status`, exit 6, with
/// the provider's own message in the sentence. A script retries a 6 and
/// reports a 1, so the status is what makes the failure actionable.
#[test]
fn a_provider_answer_carries_its_status_and_exits_six() {
    let fault = mapped(TurnError::Llm(LlmError::Provider {
        status: 429,
        message: "rate limited".to_string(),
        retry_after_ms: None,
        request_id: None,
    }));

    assert_eq!(fault.kind(), Some(ErrorCode::ProviderError));
    assert_eq!(data(&fault)["provider"], PROVIDER);
    assert_eq!(data(&fault)["status"], 429);
    assert!(fault.message.contains("rate limited"), "{}", fault.message);
    assert_eq!(ErrorCode::ProviderError.exit_status(), 6);
}

/// TC-FAULT-3: a provider that never answered reports no status.
///
/// Input: a transport failure, then a protocol failure.
/// Expected: `ProviderError` with `provider` and no `status` key at all. A
/// zero would read as an HTTP answer of zero, and an absent key is the only
/// honest way to say the call never got one.
#[test]
fn a_call_that_never_reached_an_answer_reports_no_status() {
    for error in [
        LlmError::Transport("connection reset".to_string()),
        LlmError::Protocol("frame is not JSON".to_string()),
    ] {
        let fault = mapped(TurnError::Llm(error));

        assert_eq!(fault.kind(), Some(ErrorCode::ProviderError));
        assert_eq!(data(&fault)["provider"], PROVIDER);
        assert_eq!(data(&fault).get("status"), None, "{fault:?}");
    }
}

/// TC-FAULT-4: a log that refused a chunk is this build's fault.
///
/// Input: a sink failure, which reaches the caller from inside a provider
/// call.
/// Expected: `Internal`, exit 1, not `ProviderError`. Nothing about the
/// provider was wrong, and telling a script to retry the provider would send
/// it round a loop that cannot succeed.
#[test]
fn a_sink_failure_is_internal_not_the_providers_fault() {
    let fault = mapped(TurnError::Llm(LlmError::Sink("disk full".to_string())));

    assert_eq!(fault.kind(), Some(ErrorCode::Internal));
    assert_eq!(ErrorCode::Internal.exit_status(), 1);
    assert!(fault.message.contains("disk full"), "{}", fault.message);
}

/// TC-FAULT-5: a torn journal names the line to look at.
///
/// Input: a corrupt journal at line 12, reached through a turn.
/// Expected: `LogCorrupt` carrying `session_id` and `line`, exit 1. The line
/// is what makes the journal repairable by hand.
#[test]
fn a_corrupt_journal_names_the_session_and_the_line() {
    let fault = mapped(TurnError::Session(SessionError::Corrupt(12)));

    assert_eq!(fault.kind(), Some(ErrorCode::LogCorrupt));
    assert_eq!(data(&fault)["session_id"], SESSION);
    assert_eq!(data(&fault)["line"], 12);
    assert_eq!(ErrorCode::LogCorrupt.exit_status(), 1);
}

/// TC-FAULT-6: an I/O fault carries the path only when there is one to carry.
///
/// Input: the same I/O error mapped with a journal path, then without one.
/// Expected: `Io` with `path` in the first, and no `data` at all in the
/// second. The contract's table asks for the path "when a path is at fault",
/// and a caller that does not know one must not guess.
#[test]
fn an_io_fault_carries_the_path_the_caller_knew() {
    let refused = || SessionError::Io(std::io::Error::other("refused"));

    let known = journal_error(SESSION, Some(Path::new("/tmp/s-1.jsonl")), &refused());
    let unknown = journal_error(SESSION, None, &refused());

    assert_eq!(known.kind(), Some(ErrorCode::Io));
    assert_eq!(data(&known)["path"], "/tmp/s-1.jsonl");
    assert_eq!(unknown.kind(), Some(ErrorCode::Io));
    assert_eq!(unknown.data, None, "{unknown:?}");
}

/// TC-FAULT-7: every mapped failure carries a code this build knows.
///
/// Input: one failure of each `TurnError` shape, including a service fault.
/// Expected: `kind()` resolves for all of them, so no surface ever meets a
/// raw number from this mapping. A new engine failure that reached a surface
/// unmapped would be reported as an unknown code and exit 1, which is why the
/// mapping ends in a catch-all rather than a match a new variant would break.
#[test]
fn every_shape_a_turn_can_fail_in_has_a_known_code() {
    let shapes = [
        TurnError::Llm(LlmError::Transport("x".to_string())),
        TurnError::Session(SessionError::NotSerializable("event".to_string())),
        TurnError::Session(SessionError::Corrupt(1)),
        TurnError::Prompt(PromptError::UnknownVariable {
            section: "persona".to_string(),
            name: "modle".to_string(),
            registered: vec!["model".to_string()],
        }),
        TurnError::Plugin("a listener with a bug".to_string()),
    ];

    for shape in shapes {
        let fault = mapped(shape);
        assert!(fault.kind().is_some(), "{fault:?}");
        assert!(!fault.message.is_empty(), "{fault:?}");
    }
}

/// TC-FAULT-10: a contained plugin panic is this build's fault, and says so.
///
/// A listener with a bug is not the caller's mistake and not the provider's,
/// and retrying would run the same listener over the same input, so the
/// reader's only move is to report it - which is what `Internal` means in
/// section 4.5, and what its exit status of 1 tells a script.
///
/// Input: the failure a contained decision-listener panic produces.
/// Expected: `Internal`, exit 1, carrying the panic's own words so the report
/// names the bug rather than only its category.
#[test]
fn a_contained_plugin_panic_is_internal_and_carries_its_message() {
    let fault = mapped(TurnError::Plugin("assembling went wrong".to_string()));

    assert_eq!(fault.kind(), Some(ErrorCode::Internal));
    assert_eq!(ErrorCode::Internal.exit_status(), 1);
    assert!(
        fault.message.contains("assembling went wrong"),
        "the panic's own words reach the report: {}",
        fault.message
    );
}

/// TC-FAULT-8: a settings document that cannot be booted on names its path.
///
/// Input: each `ConfigError` whose subject is the document - an extension the
/// reader does not take, a directory where a file was named, a file the
/// filesystem refused, text that does not parse, and a root that is not a map.
/// Expected: all five are `Io` carrying `path`, and exit 1. They are one code
/// because the reader's next move is the same for all five: go and look at
/// that file. No case invents a `field`, because no key is at fault when the
/// document as a whole could not be read.
#[test]
fn a_document_that_cannot_be_booted_on_names_its_path_and_exits_one() {
    let path = std::path::PathBuf::from("/tmp/settings.toml");
    let documents = [
        ConfigError::UnsupportedExtension {
            path: path.clone(),
            extension: "toml".to_string(),
        },
        ConfigError::IsADirectory { path: path.clone() },
        ConfigError::Unreadable {
            path: path.clone(),
            source: std::io::Error::other("refused"),
        },
        ConfigError::Malformed {
            path: path.clone(),
            message: "expected value".to_string(),
        },
        ConfigError::NotAMap { path: path.clone() },
    ];

    for document in documents {
        let fault = config_error(&document);

        assert_eq!(fault.kind(), Some(ErrorCode::Io), "{fault:?}");
        assert_eq!(data(&fault)["path"], "/tmp/settings.toml", "{fault:?}");
        assert_eq!(data(&fault).get("field"), None, "{fault:?}");
    }
    assert_eq!(ErrorCode::Io.exit_status(), 1);
}

/// TC-FAULT-9: a value the key does not take names the key.
///
/// Input: a `BadValue` for `agent.max_steps`.
/// Expected: `InvalidParams` carrying the dotted key as `field`, and exit 2 -
/// not `Io`, because the document was read and one line in it is wrong. The
/// message states what the key takes without repeating the key, which the
/// enum's own `Display` does and `field` already carries.
#[test]
fn a_value_the_key_does_not_take_names_the_key_and_exits_two() {
    let fault = config_error(&ConfigError::BadValue {
        key: "agent.max_steps".to_string(),
        expected: "a whole number of steps, at least 1".to_string(),
        found: "0".to_string(),
    });

    assert_eq!(fault.kind(), Some(ErrorCode::InvalidParams));
    assert_eq!(data(&fault)["field"], "agent.max_steps");
    assert_eq!(data(&fault).get("path"), None, "{fault:?}");
    assert_eq!(
        fault.message,
        "must be a whole number of steps, at least 1, not 0"
    );
    assert_eq!(ErrorCode::InvalidParams.exit_status(), 2);
}

/// TC-FAULT-10: a prompt that could not be assembled is this build's fault.
///
/// Input: a section that named a variable the assembly had no value for.
/// Expected: `Internal`, no `data`, and exit 1. Not `ProviderError`, because
/// the request never left and retrying sends the same sections through the
/// same registry; what the reader does next is report the build, which is what
/// the table's `Internal` row means.
#[test]
fn a_prompt_that_could_not_be_assembled_is_internal_and_exits_one() {
    let fault = mapped(TurnError::Prompt(PromptError::NoValue {
        section: "persona".to_string(),
        name: "cwd".to_string(),
    }));

    assert_eq!(fault.kind(), Some(ErrorCode::Internal));
    assert_eq!(fault.data, None, "{fault:?}");
    assert!(fault.message.contains("persona"), "{fault:?}");
    assert_eq!(ErrorCode::Internal.exit_status(), 1);
}

fn mapped(error: TurnError) -> RpcError {
    turn_error(SESSION, PROVIDER, Some(Path::new("/tmp/s-1.jsonl")), &error)
}

fn data(error: &RpcError) -> &serde_json::Value {
    error.data.as_ref().expect("the table names data fields")
}
