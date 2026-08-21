//! What a route is: a pattern, a path, and who answers.

use std::collections::HashMap;
use std::sync::Arc;

use super::Response;

/// How a named route's path is matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// The whole path, exactly.
    Exact,
    /// The start of the path. The longest one that matches wins, which is what
    /// lets `/api/v2` be somebody else's while `/api` is registered.
    Prefix,
}

/// Whoever answers a request.
///
/// A function rather than a trait object with state: a route that needs state
/// captures it, which is the same thing with less to say about it. `Send` and
/// `Sync` because the accept loop hands requests to tasks.
pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

/// One registration in the table.
#[derive(Clone)]
pub struct Route {
    pub(crate) path: String,
    pub(crate) handler: Handler,
}

impl Route {
    pub(crate) fn new(path: &str, handler: Handler) -> Self {
        Self {
            path: path.to_string(),
            handler,
        }
    }

    /// The path this route was registered under.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// A request, as a route needs to read it.
///
/// The head only. A body belongs to the route that wants one, and a carrier
/// that read every body would hold memory for requests nobody asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path with any query string taken off, which is what a route table
    /// matches on. `?ws=...` is the page's business, not the table's.
    pub path: String,
    pub query: Option<String>,
    pub headers: HashMap<String, String>,
    /// Whether this is a protocol upgrade, which is matched in its own table.
    pub upgrade: bool,
}

impl Request {
    /// Read a parsed head, or `None` if it is not one this carrier serves.
    pub(crate) fn of(parsed: &httparse::Request<'_, '_>) -> Option<Self> {
        let target = parsed.path?;
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path, Some(query.to_string())),
            None => (target, None),
        };
        let headers: HashMap<String, String> = parsed
            .headers
            .iter()
            .map(|header| {
                (
                    header.name.to_ascii_lowercase(),
                    String::from_utf8_lossy(header.value).to_string(),
                )
            })
            .collect();
        // An upgrade is the header, not the path: a route table that guessed
        // from the pathname would send an ordinary GET of `/ws` to the socket
        // handler and hang the reader's browser.
        let upgrade = headers
            .get("upgrade")
            .is_some_and(|said| said.eq_ignore_ascii_case("websocket"));
        Some(Self {
            method: parsed.method?.to_string(),
            path: path.to_string(),
            query,
            headers,
            upgrade,
        })
    }

    /// A header, by a name compared without case, as HTTP means them.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}
