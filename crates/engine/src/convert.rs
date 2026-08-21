//! Engine types to contract types, and engine failures to contract errors.
//!
//! This module is the only place the two vocabularies meet. Keeping it in one
//! file is what lets `docs/interface-contract.md` promise that refactoring an
//! engine type is not a breaking change for a surface.

use std::path::Path;

use tetanus_config::ConfigError;
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types as wire;
use tetanus_session::SessionError;
use tetanus_turn::llm::LlmError;
use tetanus_turn::TurnError;

/// One durable fact, as the contract carries it. The two structs are field for
/// field identical, which TC-PAGE-1 asserts against literal JSON rather than
/// against this function.
pub fn session_event(event: tetanus_session::SessionEvent) -> wire::SessionEvent {
    wire::SessionEvent {
        ty: event.ty,
        seq: event.seq,
        time: event.time,
        data: event.data,
        source_event_seqs: event.source_event_seqs,
    }
}

/// Why a turn closed, in the contract's vocabulary. The engine's enum and the
/// wire's are separate types on purpose (contract section 7.6), so this is the
/// one place a new stop reason has to be named twice.
pub fn stop_reason(reason: tetanus_turn::StopReason) -> wire::StopReason {
    match reason {
        tetanus_turn::StopReason::Natural => wire::StopReason::Natural,
        tetanus_turn::StopReason::PreStepRejected => wire::StopReason::PreStepRejected,
        tetanus_turn::StopReason::MaxSteps => wire::StopReason::MaxSteps,
        tetanus_turn::StopReason::Cancelled => wire::StopReason::Cancelled,
        // Neither of these has a named wire variant: both arrived after
        // contract 1.0, and section 7.5 is what lets them travel as the
        // fallback. The word is the engine's own, which is also the word the
        // journal carries, so a surface reading either sees one spelling.
        reason @ (tetanus_turn::StopReason::MaxTokens | tetanus_turn::StopReason::Interrupted) => {
            wire::StopReason::Other(reason.as_str().to_string())
        }
    }
}

/// Contract section 4.5: a journal that is not a faithful copy of a log is
/// `LogCorrupt`, anything else the filesystem refused is `Io`.
///
/// `journal` is the file the caller was reading, when it knows it. The `Io`
/// row of the contract's table carries the path, and a caller that has none
/// reports the failure without inventing one.
pub fn journal_error(session_id: &str, journal: Option<&Path>, error: &SessionError) -> RpcError {
    match error {
        SessionError::Corrupt(line) => RpcError::new(
            ErrorCode::LogCorrupt,
            format!("journal for `{session_id}` is corrupt at line {line}"),
        )
        .with_data(serde_json::json!({ "session_id": session_id, "line": line })),
        SessionError::Io(io) => match journal {
            Some(path) => io_error(path, io),
            None => RpcError::new(ErrorCode::Io, io.to_string()),
        },
        other => RpcError::new(ErrorCode::Internal, other.to_string()),
    }
}

/// Contract section 4.5: something the filesystem refused, carrying the path
/// at fault so a surface can name it. A bare `Not a directory` tells the
/// reader that a path was wrong but not which one, and only the engine knows
/// which one it was reading.
pub(crate) fn io_error(path: &Path, error: &std::io::Error) -> RpcError {
    RpcError::new(ErrorCode::Io, error.to_string())
        .with_data(serde_json::json!({ "path": path.display().to_string() }))
}

/// [`journal_error`] for a caller that owns its error and knows no path.
pub fn session_error(session_id: &str, error: SessionError) -> RpcError {
    journal_error(session_id, None, &error)
}

/// Contract section 4.5: why a turn failed, in the code a surface acts on.
///
/// This is the published mapping, and the only one. A surface calls it and
/// renders what it returns; it does not match on an engine error type to
/// derive a code of its own. The engine's error enums are internal types with
/// no fallback variant, so a match outside this crate stops compiling the day
/// the engine names a new failure, and the code that failure deserves is the
/// engine's decision rather than each surface's.
///
/// The distinction the codes carry is what the reader does next: a credential
/// is fixed by a human, a provider error is worth retrying, and an internal
/// fault is this build's bug. That is why a sink failure is `Internal` even
/// though it surfaced during a provider call: nothing about the provider was
/// wrong.
pub fn turn_error(
    session_id: &str,
    provider: &str,
    journal: Option<&Path>,
    error: &TurnError,
) -> RpcError {
    match error {
        TurnError::Session(e) => journal_error(session_id, journal, e),
        TurnError::Service(e) => internal(e),
        // A section that names a variable the assembly cannot give it is a
        // mistake in what this build registered, so the reader's next move is
        // the one every internal fault asks for: report it. Retrying sends the
        // same sections through the same registry.
        TurnError::Prompt(e) => internal(format!("the system prompt could not be assembled: {e}")),
        // A listener with a bug is this build's fault, not the caller's and not
        // the provider's, so it takes the code every internal fault takes.
        // Retrying would run the same listener over the same input, so there is
        // nothing for the reader to do but report it - which is exactly what
        // `Internal` tells a surface.
        TurnError::Plugin(fault) => internal(format!("a plugin listener panicked: {fault}")),
        TurnError::Llm(LlmError::MissingCredential(env) | LlmError::InvalidCredential(env)) => {
            RpcError::new(
                ErrorCode::MissingCredential,
                format!("provider `{provider}` has no usable credential at {env}"),
            )
            .with_data(serde_json::json!({ "provider": provider, "env": env }))
        }
        // The wait the provider asked for is not published: section 4.5 fixes
        // the fields this error's `data` carries, so adding one is a change to
        // the contract and not to this table.
        TurnError::Llm(LlmError::Provider {
            status, message, ..
        }) => RpcError::new(
            ErrorCode::ProviderError,
            format!("provider `{provider}` answered {status}: {message}"),
        )
        .with_data(serde_json::json!({ "provider": provider, "status": status })),
        TurnError::Llm(LlmError::Sink(e)) => internal(format!("session log refused a chunk: {e}")),
        // No status: the provider never answered, so the field the table
        // names is absent rather than invented.
        TurnError::Llm(other) => RpcError::new(
            ErrorCode::ProviderError,
            format!("provider `{provider}` failed: {other}"),
        )
        .with_data(serde_json::json!({ "provider": provider })),
    }
}

/// Contract section 4.5: why the settings document could not be booted on, in
/// the code a surface acts on.
///
/// This is the third published mapping, and it exists for the reason the other
/// two do: `ConfigError` is an internal enum with no fallback variant, so a
/// surface that matched it to pick a code would stop compiling the day the
/// engine names a new fault - as it just did, for a value that is wrong rather
/// than a document that is.
///
/// The split is what the reader has to fix. A document that could not be
/// turned into settings is `Io` with its path, whether the filesystem refused
/// the file or its own text did; a value the key does not take is
/// `InvalidParams` with the key, because the key is the thing to edit. The
/// message drops the key that [`ConfigError`]'s own wording repeats, since
/// `field` already carries it and a surface prints both.
pub fn config_error(error: &ConfigError) -> RpcError {
    match error {
        ConfigError::BadValue {
            key,
            expected,
            found,
        } => RpcError::new(
            ErrorCode::InvalidParams,
            format!("must be {expected}, not {found}"),
        )
        .with_data(serde_json::json!({ "field": key })),
        ConfigError::UnsupportedExtension { path, .. }
        | ConfigError::IsADirectory { path }
        | ConfigError::Unreadable { path, .. }
        | ConfigError::Malformed { path, .. }
        | ConfigError::NotAMap { path } => RpcError::new(ErrorCode::Io, error.to_string())
            .with_data(serde_json::json!({ "path": path.display().to_string() })),
    }
}

pub fn session_not_found(session_id: &str) -> RpcError {
    RpcError::new(
        ErrorCode::SessionNotFound,
        format!("no session `{session_id}`"),
    )
    .with_data(serde_json::json!({ "session_id": session_id }))
}

pub fn internal(message: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, message.to_string())
}
