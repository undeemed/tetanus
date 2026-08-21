//! What a route says back, and the bytes that go on the wire.

use std::io;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// The statuses this carrier and its routes answer with.
///
/// A closed list rather than a number: every one of these is a decision some
/// spec made, and a route that wanted a status nobody has argued about yet
/// should have to add it here and say why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NoContent,
    BadRequest,
    Forbidden,
    NotFound,
    MethodNotAllowed,
    TooLarge,
    UnsupportedMedia,
    Error,
}

impl Status {
    /// The code and the reason phrase, as one line of a response.
    fn line(self) -> (u16, &'static str) {
        match self {
            Status::Ok => (200, "OK"),
            Status::NoContent => (204, "No Content"),
            Status::BadRequest => (400, "Bad Request"),
            Status::Forbidden => (403, "Forbidden"),
            Status::NotFound => (404, "Not Found"),
            Status::MethodNotAllowed => (405, "Method Not Allowed"),
            Status::TooLarge => (413, "Payload Too Large"),
            Status::UnsupportedMedia => (415, "Unsupported Media Type"),
            Status::Error => (500, "Internal Server Error"),
        }
    }
}

/// What a route answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: Status,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// A status and nothing else.
    pub fn status(status: Status) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A body, and the type it is to be read as.
    pub fn body(status: Status, kind: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), kind.into())],
            body: body.into(),
        }
    }

    /// The same, for text this build wrote.
    pub fn text(status: Status, said: &str) -> Self {
        Self::body(
            status,
            "text/plain; charset=utf-8",
            said.as_bytes().to_vec(),
        )
    }

    /// Add a header. Returns itself so a route reads as one expression.
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// Write a response, head and body.
///
/// `content-length` is always written and the connection is always closed:
/// this is a dev-facing carrier for one page, and a keep-alive that got the
/// length wrong is a browser that hangs rather than an error anybody can read.
pub(crate) async fn write(stream: &mut TcpStream, answer: &Response) -> io::Result<()> {
    let (code, reason) = answer.status.line();
    let mut head = format!("HTTP/1.1 {code} {reason}\r\n");
    for (name, value) in &answer.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\n", answer.body.len()));
    head.push_str("connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&answer.body).await?;
    stream.flush().await
}
