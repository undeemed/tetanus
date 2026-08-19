//! Engine failures to contract errors, and engine types to contract types.
//!
//! This module is the only place the two vocabularies meet. Keeping it in one
//! file is what lets `docs/interface-contract.md` promise that refactoring an
//! engine type is not a breaking change for a surface.

use tetanus_protocol::rpc::{ErrorCode, RpcError};

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
