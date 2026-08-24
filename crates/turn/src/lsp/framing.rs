//! The LSP base protocol's framing: `Content-Length`-delimited JSON-RPC over a
//! byte stream.
//!
//! **Both bounds are deliberate.** A server that never sends the header
//! terminator would grow the buffer for ever, and one that announces a body
//! larger than memory would be believed. A language server is a program the
//! deployment chose, but it is also a program that crashes, and a decoder that
//! trusts a length field is one a corrupt stream can take the harness down
//! with.
//!
//! **A length is bytes, and the body is decoded as UTF-8 afterwards.** The
//! header counts octets, so slicing on the announced length is the one place
//! this may work in bytes; the text is validated after the cut rather than
//! assumed.
//!
//! Parity: upstream `packages/lsp/lsp-stdio/src/framing.ts`, pinned by its
//! `framing.spec.ts`.

/// The header/body separator in the base protocol.
const SEPARATOR: &[u8] = b"\r\n\r\n";

/// Cap on the header section, so a server that never terminates one cannot
/// grow the buffer without limit.
pub const MAX_HEADER_BYTES: usize = 1 << 16;

/// The default cap on one message body.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 32 << 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FramingError {
    #[error("the LSP header passed {MAX_HEADER_BYTES} bytes with no terminator")]
    HeaderTooLong,
    #[error("the LSP header has no Content-Length")]
    NoContentLength,
    #[error("the LSP Content-Length {0:?} is not a byte count")]
    BadContentLength(String),
    #[error("an LSP message of {announced} bytes exceeds the {limit}-byte limit")]
    MessageTooLong { announced: usize, limit: usize },
    #[error("an LSP message body was not UTF-8")]
    NotText,
    #[error("an LSP message body was not JSON")]
    NotJson,
}

/// Frame one message for a server's standard input.
pub fn encode(message: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_else(|_| b"{}".to_vec());
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(&body);
    framed
}

/// A streaming decoder: fed stdout chunks, it yields whole message bodies.
#[derive(Debug)]
pub struct MessageDecoder {
    buffer: Vec<u8>,
    limit: usize,
}

impl Default for MessageDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MESSAGE_BYTES)
    }
}

impl MessageDecoder {
    pub fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
        }
    }

    /// Append a chunk and answer every message that is now complete.
    ///
    /// A partial message is not an error and not an answer: it is bytes the
    /// decoder keeps until the rest arrives, which is what makes this usable
    /// against a pipe that splits messages wherever it likes.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<serde_json::Value>, FramingError> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        while let Some(message) = self.next()? {
            messages.push(message);
        }
        Ok(messages)
    }

    fn next(&mut self) -> Result<Option<serde_json::Value>, FramingError> {
        let Some(at) = find(&self.buffer, SEPARATOR) else {
            // No terminator yet. That is only patience up to a point: past the
            // cap it is a server that is never going to send one.
            if self.buffer.len() > MAX_HEADER_BYTES {
                return Err(FramingError::HeaderTooLong);
            }
            return Ok(None);
        };
        if at > MAX_HEADER_BYTES {
            return Err(FramingError::HeaderTooLong);
        }

        let header = std::str::from_utf8(&self.buffer[..at]).map_err(|_| FramingError::NotText)?;
        let length = content_length(header)?;
        if length > self.limit {
            return Err(FramingError::MessageTooLong {
                announced: length,
                limit: self.limit,
            });
        }

        let start = at + SEPARATOR.len();
        let end = start + length;
        if self.buffer.len() < end {
            return Ok(None);
        }
        let body =
            std::str::from_utf8(&self.buffer[start..end]).map_err(|_| FramingError::NotText)?;
        let message = serde_json::from_str(body).map_err(|_| FramingError::NotJson)?;
        self.buffer.drain(..end);
        Ok(Some(message))
    }
}

/// The `Content-Length` value, case-insensitively, ignoring every other header
/// as the base protocol says to.
fn content_length(header: &str) -> Result<usize, FramingError> {
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim();
        return value
            .parse()
            .map_err(|_| FramingError::BadContentLength(value.to_string()));
    }
    Err(FramingError::NoContentLength)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
