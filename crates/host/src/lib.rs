//! The HTTP route carrier the web GUI rides on.
//!
//! Upstream's `host/webserver` is "a `node:http` server that listens on
//! activation and provides `ctx.webServer`". This is that server, and it is
//! deliberately as small: it knows no harness concepts, serves no files, and
//! prints nothing. What it owns is the route table and the order requests are
//! matched in; everything a request actually does belongs to whoever
//! registered the route.
//!
//! # The match order is a contract, not an implementation detail
//!
//! Exact over the whole table, then the longest prefix, then the one fallback.
//! It is fixed, and registration order carries no request-facing meaning,
//! because a carrier whose answer depends on the order plugins happened to
//! start in is a carrier nobody can compose against.
//!
//! For the same reason a duplicate path is refused rather than shadowed: two
//! owners of one path is a misconfiguration of the assembly, and the moment to
//! say so is composition, not the first request that goes to the wrong one.
//!
//! # What it will not bind
//!
//! `127.0.0.1` and `0.0.0.0`, and nothing else. Loopback is the posture; the
//! wildcard is a deliberate exposure a person has to type. An address in
//! between reads as a third option this server has thought about, and it has
//! not: there is no TLS here, no authentication and no origin policy.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod respond;
mod route;

pub use respond::{Response, Status};
pub use route::{Handler, Pattern, Request, Route};

/// The addresses this server will bind, and nothing else.
const BINDABLE: [&str; 2] = ["127.0.0.1", "0.0.0.0"];

/// How much of a request head this carrier will read before refusing it.
///
/// A head is a request line and its headers. Sixteen kilobytes is more than
/// any browser sends and less than a socket can use to hold memory open.
const HEAD_LIMIT: usize = 16 * 1024;

/// How long a refused connection is drained before it is shut down.
const LINGER: std::time::Duration = std::time::Duration::from_millis(250);

/// The HTTP server, and the table of who answers what.
///
/// Cloneable, and every clone is the same table: a plugin registers a route
/// through its own handle while the accept loop reads that same table.
#[derive(Clone)]
pub struct WebServer {
    table: Arc<Mutex<Table>>,
    address: SocketAddr,
}

/// Who answers what, and the one who answers everything else.
#[derive(Default)]
struct Table {
    routes: HashMap<(Pattern, String), Route>,
    upgrades: HashMap<String, Route>,
    fallback: Option<Route>,
}

/// A registration, undone by dropping it.
///
/// Upstream returns a disposer and ties the seat to the plugin's fiber. The
/// same idea in Rust is a guard: a plugin that goes away takes its routes with
/// it, and a seat nobody holds is a seat the next owner can claim.
pub struct Registered {
    table: Arc<Mutex<Table>>,
    what: What,
}

enum What {
    Route(Pattern, String),
    Upgrade(String),
    Fallback,
}

/// What a registration can go wrong by.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken {
    /// Two owners for one path. The assembly is wrong, and this is the moment
    /// to say so rather than the first request that goes to the wrong one.
    Route(String),
    /// A second claim on the single fallback seat.
    Fallback,
}

impl std::fmt::Display for Taken {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Taken::Route(path) => write!(out, "{path} already has an owner"),
            Taken::Fallback => write!(out, "the fallback seat already has an owner"),
        }
    }
}

impl std::error::Error for Taken {}

impl WebServer {
    /// Bind, and answer nothing yet.
    ///
    /// The listener is bound here rather than on the first request so that a
    /// port already in use is a failure of starting up, with the address in
    /// the message, and not a surprise the first reader meets.
    pub async fn bind(host: &str, port: u16) -> io::Result<(Self, TcpListener)> {
        if !BINDABLE.contains(&host) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{host} is not an address this server binds; use 127.0.0.1 or 0.0.0.0"),
            ));
        }
        let listener = TcpListener::bind((host, port)).await?;
        let address = listener.local_addr()?;
        let server = Self {
            table: Arc::new(Mutex::new(Table::default())),
            address,
        };
        Ok((server, listener))
    }

    /// The address this server is on, with the port the operating system
    /// picked when the caller asked for none.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Add a named route. The guard removes it.
    pub fn register(
        &self,
        pattern: Pattern,
        path: &str,
        handler: Handler,
    ) -> Result<Registered, Taken> {
        let mut table = self.table.lock().expect("the route table");
        let key = (pattern, path.to_string());
        if table.routes.contains_key(&key) {
            return Err(Taken::Route(path.to_string()));
        }
        table.routes.insert(key.clone(), Route::new(path, handler));
        Ok(Registered {
            table: Arc::clone(&self.table),
            what: What::Route(key.0, key.1),
        })
    }

    /// Add an upgrade route, matched on the exact pathname.
    pub fn register_upgrade(&self, path: &str, handler: Handler) -> Result<Registered, Taken> {
        let mut table = self.table.lock().expect("the route table");
        if table.upgrades.contains_key(path) {
            return Err(Taken::Route(path.to_string()));
        }
        table
            .upgrades
            .insert(path.to_string(), Route::new(path, handler));
        Ok(Registered {
            table: Arc::clone(&self.table),
            what: What::Upgrade(path.to_string()),
        })
    }

    /// Claim the one seat for everything no named route matched.
    ///
    /// Single owner: the shipped one is the static frontend, and while nobody
    /// holds it this server answers 404. A second claim is refused rather than
    /// queued, because two owners of "everything else" is not a thing an
    /// assembly can mean.
    pub fn register_fallback(&self, handler: Handler) -> Result<Registered, Taken> {
        let mut table = self.table.lock().expect("the route table");
        if table.fallback.is_some() {
            return Err(Taken::Fallback);
        }
        table.fallback = Some(Route::new("", handler));
        Ok(Registered {
            table: Arc::clone(&self.table),
            what: What::Fallback,
        })
    }

    /// Answer requests until the listener fails.
    ///
    /// One task per connection, and a task that panics takes its own socket
    /// with it: a carrier that let one bad request end the process would make
    /// every route's bug everybody's outage.
    pub async fn serve(self, listener: TcpListener) -> io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                if let Err(err) = server.answer(stream).await {
                    // A request that could not be answered is this connection's
                    // problem and nobody else's.
                    tracing::warn!(%err, "the request was not answered");
                }
            });
        }
    }

    /// Read one request, find who owns it, and write what they said.
    async fn answer(&self, mut stream: TcpStream) -> io::Result<()> {
        let Some(request) = read_head(&mut stream).await? else {
            respond::write(&mut stream, &Response::status(Status::BadRequest)).await?;
            return lingering_close(stream).await;
        };
        let found = {
            let table = self.table.lock().expect("the route table");
            table.find(&request)
        };
        let Some(route) = found else {
            respond::write(&mut stream, &Response::status(Status::NotFound)).await?;
            return lingering_close(stream).await;
        };
        let answer = (route.handler)(&request);
        respond::write(&mut stream, &answer).await?;
        lingering_close(stream).await
    }
}

impl Table {
    /// The route that owns a request: exact, then longest prefix, then the
    /// fallback. The order is the contract; nothing here reads registration
    /// order.
    fn find(&self, request: &Request) -> Option<Route> {
        if request.upgrade {
            return self.upgrades.get(&request.path).cloned();
        }
        if let Some(route) = self.routes.get(&(Pattern::Exact, request.path.clone())) {
            return Some(route.clone());
        }
        let longest = self
            .routes
            .iter()
            .filter(|((pattern, path), _)| {
                *pattern == Pattern::Prefix && request.path.starts_with(path.as_str())
            })
            .max_by_key(|((_, path), _)| path.len())
            .map(|(_, route)| route.clone());
        longest.or_else(|| self.fallback.clone())
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        let Ok(mut table) = self.table.lock() else {
            return;
        };
        match &self.what {
            What::Route(pattern, path) => {
                table.routes.remove(&(*pattern, path.clone()));
            }
            What::Upgrade(path) => {
                table.upgrades.remove(path);
            }
            What::Fallback => table.fallback = None,
        }
    }
}

/// Close a connection the way a client can still read the answer on.
///
/// A refusal is written while the client may still be sending - a request too
/// long for this carrier is exactly that case - and a socket closed with bytes
/// still unread sends RST, which throws away the reply the client had not read
/// yet. So the rest is drained, briefly, and then the write side is shut down:
/// the client reads its 400 and sees the end of it.
async fn lingering_close(mut stream: TcpStream) -> io::Result<()> {
    let drain = async {
        let mut bin = [0_u8; 4096];
        loop {
            match stream.read(&mut bin).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
    };
    // Bounded, because a client that keeps writing forever must not keep a
    // task here forever.
    let _ = tokio::time::timeout(LINGER, drain).await;
    stream.shutdown().await
}

/// Read a request's head, and stop reading at [`HEAD_LIMIT`].
///
/// `None` is a head this carrier will not parse: a client that dropped, a
/// head longer than the limit, or bytes that are not a request. Every one of
/// them is answered 400 rather than logged and left, because a socket held
/// open for a reply that never comes is worse than a refusal.
async fn read_head(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1024];
    loop {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            return Ok(None);
        }
        head.extend_from_slice(&byte[..read]);
        if head.len() > HEAD_LIMIT {
            return Ok(None);
        }
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut parsed = httparse::Request::new(&mut headers);
        match parsed.parse(&head) {
            Ok(httparse::Status::Complete(_)) => return Ok(Request::of(&parsed)),
            Ok(httparse::Status::Partial) => continue,
            Err(_) => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
