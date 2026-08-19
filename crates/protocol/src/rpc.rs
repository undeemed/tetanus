//! The JSON-RPC 2.0 envelope every tetanus surface speaks, and the error codes
//! it carries. Nothing here knows a harness concept: swapping the transport is
//! a change to the carrier, never to a payload in [`crate::methods`].

use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The literal `"2.0"` tag. A frame that omits it, or carries another value,
/// fails to deserialize instead of being answered as if it were valid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V2;

impl Serialize for V2 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for V2 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let tag = String::deserialize(d)?;
        match tag.as_str() {
            "2.0" => Ok(V2),
            other => Err(D::Error::invalid_value(Unexpected::Str(other), &"\"2.0\"")),
        }
    }
}

/// Request correlation id. Servers echo the exact value they received, so a
/// client that numbers its calls and one that names them both work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    Text(String),
    /// The id a server answers with when the frame it is refusing carried none
    /// it could read: a frame that is not JSON, or is JSON but not a request.
    /// JSON-RPC 2.0 requires `null` there, and an error with no id at all
    /// would be a frame a client cannot match to anything. A client never
    /// sends it.
    Null,
}

/// A call that expects exactly one [`Response`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: V2,
    pub id: Id,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A one-way frame. It is never answered, and never retried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: V2,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Exactly one of `result` or `error`, never both and never neither.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    #[serde(rename = "result")]
    Result(serde_json::Value),
    #[serde(rename = "error")]
    Error(RpcError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: V2,
    pub id: Id,
    #[serde(flatten)]
    pub payload: Payload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    /// Structured detail for the code. Shape is fixed per code; see the
    /// contract's error table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The code as a known variant, or `None` when the peer sent a code this
    /// build does not know. Unknown codes are surfaced, never remapped.
    pub fn kind(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(self.code)
    }
}

/// One frame in either direction. Both peers demultiplex with this, because
/// the server may also call the client (see `ui/ask`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

/// Every code the contract defines. `-32700..-32600` are the JSON-RPC
/// reserved codes; `-32000..-32099` is the implementation-defined band this
/// contract owns. A code's meaning is frozen for the life of a major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    Internal = -32603,
    /// The peer's protocol major version is not the one this build serves.
    UnsupportedProtocolVersion = -32000,
    /// The method is in the contract but this build does not serve it yet.
    NotImplemented = -32001,
    SessionNotFound = -32002,
    /// A turn is already running on this session.
    SessionBusy = -32003,
    /// The turn stopped because `agent.interrupt` asked it to.
    Cancelled = -32004,
    /// The provider credential is absent or unusable.
    MissingCredential = -32005,
    /// The provider answered, and the answer was a failure.
    ProviderError = -32006,
    ToolUnknown = -32007,
    /// The session journal on disk is not a faithful copy of a log.
    LogCorrupt = -32008,
    Io = -32009,
}

impl ErrorCode {
    pub fn code(self) -> i32 {
        self as i32
    }

    pub fn from_code(code: i32) -> Option<Self> {
        use ErrorCode::*;
        Some(match code {
            -32700 => ParseError,
            -32600 => InvalidRequest,
            -32601 => MethodNotFound,
            -32602 => InvalidParams,
            -32603 => Internal,
            -32000 => UnsupportedProtocolVersion,
            -32001 => NotImplemented,
            -32002 => SessionNotFound,
            -32003 => SessionBusy,
            -32004 => Cancelled,
            -32005 => MissingCredential,
            -32006 => ProviderError,
            -32007 => ToolUnknown,
            -32008 => LogCorrupt,
            -32009 => Io,
            _ => return None,
        })
    }

    /// The process exit status a CLI surface returns when a call fails with
    /// this code. Fixed here so no surface invents its own.
    pub fn exit_status(self) -> u8 {
        use ErrorCode::*;
        match self {
            ParseError | InvalidRequest | MethodNotFound | InvalidParams => 2,
            NotImplemented | UnsupportedProtocolVersion => 3,
            SessionNotFound | SessionBusy | ToolUnknown => 4,
            MissingCredential => 5,
            ProviderError => 6,
            Cancelled => 130,
            Internal | LogCorrupt | Io => 1,
        }
    }
}
