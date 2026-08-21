//! The seam every request goes through, and nothing else.
//!
//! A transport sends one request and answers with one response. It follows no
//! redirect, judges no content type, and applies no policy beyond the byte cap
//! it is given - the cap is here rather than above because stopping a download
//! needs the reader, and a policy that could only refuse a body after reading
//! it would not be a cap at all.
//!
//! Everything else lives in [`crate::fetch`], which is why the whole fetch
//! policy can be asserted with no socket in the suite.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::fault::WebFault;

/// What a request asks the server to do. Two, because this crate fetches
/// pages and posts to one search API, and a transport that accepted any verb
/// would be a general HTTP client nothing here needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One request, as a transport takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    /// The body, for a request that has one.
    pub body: Option<Vec<u8>>,
    pub headers: BTreeMap<String, String>,
    /// Stop reading the body after this many bytes, and say so.
    pub max_bytes: usize,
    pub timeout: Duration,
}

/// One response, as a transport answers with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names lowercased, because a caller reading `content-type` must
    /// not have to know what case the server chose.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    /// Whether the body was cut at [`HttpRequest::max_bytes`].
    pub truncated: bool,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// Whether this is a redirect the fetcher should follow.
    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308)
    }
}

#[async_trait::async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, WebFault>;
}
