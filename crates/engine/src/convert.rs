//! Engine types to contract types, and engine failures to contract errors.
//!
//! This module is the only place the two vocabularies meet. Keeping it in one
//! file is what lets `docs/interface-contract.md` promise that refactoring an
//! engine type is not a breaking change for a surface.

use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_session::SessionError;

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
