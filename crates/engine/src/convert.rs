//! Engine types to contract types, and engine failures to contract errors.
//!
//! This module is the only place the two vocabularies meet. Keeping it in one
//! file is what lets `docs/interface-contract.md` promise that refactoring an
//! engine type is not a breaking change for a surface.

use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types as wire;
use tetanus_session::SessionError;

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
    }
}

/// Contract section 4.5: a journal that is not a faithful copy of a log is
/// `LogCorrupt`, anything else the filesystem refused is `Io`.
pub fn session_error(session_id: &str, error: SessionError) -> RpcError {
    match error {
        SessionError::Corrupt(line) => RpcError::new(
            ErrorCode::LogCorrupt,
            format!("journal for `{session_id}` is corrupt at line {line}"),
        )
        .with_data(serde_json::json!({ "session_id": session_id, "line": line })),
        SessionError::Io(io) => RpcError::new(ErrorCode::Io, io.to_string()),
        other => RpcError::new(ErrorCode::Internal, other.to_string()),
    }
}

pub fn session_not_found(session_id: &str) -> RpcError {
    RpcError::new(
        ErrorCode::SessionNotFound,
        format!("no session `{session_id}`"),
    )
    .with_data(serde_json::json!({ "session_id": session_id }))
}

pub fn not_implemented(method: &str) -> RpcError {
    RpcError::new(
        ErrorCode::NotImplemented,
        format!("`{method}` is in the contract but this build does not serve it yet"),
    )
    .with_data(serde_json::json!({ "method": method }))
}

pub fn internal(message: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, message.to_string())
}
