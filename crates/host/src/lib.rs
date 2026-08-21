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

mod frontend;
mod picker;
mod respond;
mod route;

pub use frontend::Frontend;
pub use picker::{Browse, Capability, Entry, Listing, PickerError, MAX_ENTRIES};
pub use respond::{Response, Status};
pub use route::{answered, Answering, Handler, Pattern, Request, Route};

/// The addresses this server will bind, and nothing else.
const BINDABLE: [&str; 2] = ["127.0.0.1", "0.0.0.0"];

/// How much of a request head this carrier will read before refusing it.
///
/// A head is a request line and its headers. Sixteen kilobytes is more than
/// any browser sends and less than a socket can use to hold memory open.
const HEAD_LIMIT: usize = 16 * 1024;

/// How long a client has to finish sending a request head.
///
/// A peek returns what has arrived, so a head that stops arriving would
/// otherwise be waited on forever - one socket, one task, held by a client
/// that need not even be malicious to do it.
const HEAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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

/// Who answers what, the one who answers everything else, and what every
/// index response is run through on its way out.
#[derive(Default)]
struct Table {
    routes: HashMap<(Pattern, String), Route>,
    upgrades: HashMap<String, Upgrade>,
    fallback: Option<Route>,
    taps: Vec<(usize, Tap)>,
    /// The number the next tap is filed under, so that a tap removed and one
    /// added do not collide and the order stays the order they were added in.
    next_tap: usize,
}

/// The facts a page is handed before it runs.
///
/// Upstream calls this the boot manifest and sends it through an index tap,
/// which is the arrangement worth keeping: the assembly knows what was bound,
/// the frontend knows nothing about the assembly, and the index is the one
/// document both of them touch. A page that had to be patched by whoever
/// served it - a string replaced in a file on the way past - is a page that
/// only works when served by that one program.
pub struct Manifest {
    pub carrier: String,
    pub protocol: String,
    /// The secret a reader of this page may dial the carrier with, when the
    /// deployment's posture is to admit every reader of the page. `None` when
    /// the secret is the reader's own and belongs in their URL instead.
    pub token: Option<String>,
}

impl Manifest {
    /// The tap that writes this manifest into a page.
    ///
    /// Written as JSON into a script the page reads before its own, and
    /// `</` is escaped inside it: a value containing `</script>` would
    /// otherwise close the tag and the rest of the manifest would be markup.
    pub fn tap(self) -> Tap {
        let said = match &self.token {
            Some(token) => format!(
                "{{\"carrier\":{},\"protocol\":{},\"token\":{}}}",
                quoted(&self.carrier),
                quoted(&self.protocol),
                quoted(token)
            ),
            None => format!(
                "{{\"carrier\":{},\"protocol\":{}}}",
                quoted(&self.carrier),
                quoted(&self.protocol)
            ),
        };
        let script = format!("<script>window.TETANUS_BOOT = {said};</script>");
        Arc::new(move |html| match html.find("</head>") {
            Some(at) => {
                let mut out = String::with_capacity(html.len() + script.len());
                out.push_str(&html[..at]);
                out.push_str(&script);
                out.push_str(&html[at..]);
                out
            }
            // A page with no head is not this tap's to repair, and a manifest
            // appended anywhere else would run after the script that reads it.
            None => html,
        })
    }
}

/// A JSON string, with the one escape that matters inside a script tag.
fn quoted(value: &str) -> String {
    let escaped: String = value
        .chars()
        .flat_map(|char| match char {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '<' => "\\u003c".chars().collect(),
            char => vec![char],
        })
        .collect();
    format!("\"{escaped}\"")
}

/// A transform every index response passes through.
///
/// This is how the boot manifest reaches the page: the assembly knows what the
/// browser has to be told, the frontend knows nothing about the assembly, and
/// the index is the one document both of them touch.
pub type Tap = Arc<dyn Fn(String) -> String + Send + Sync>;

/// Who takes over a connection that asked to stop being HTTP.
///
/// Handed the socket with nothing read off it, so the handshake belongs
/// entirely to the protocol taking over: this carrier delivers the raw socket
/// and the request that came with it, and has no opinion about what is said
/// next. That is upstream's line - "the upgrade handler owns the protocol
/// handshake and connection contents; the webserver only delivers the raw
/// socket and request" - and it is the only arrangement under which
/// `crates/rpc` can do its own WebSocket handshake, origin check and token
/// check unchanged (contract §4.1.2).
pub type Upgrade = Arc<dyn Fn(TcpStream, Request) + Send + Sync>;

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
    Tap(usize),
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
    pub fn register_upgrade(&self, path: &str, handler: Upgrade) -> Result<Registered, Taken> {
        let mut table = self.table.lock().expect("the route table");
        if table.upgrades.contains_key(path) {
            return Err(Taken::Route(path.to_string()));
        }
        table.upgrades.insert(path.to_string(), handler);
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

    /// Add a transform to every index response, in the order taps are added.
    ///
    /// Unlike a route, a tap is not a seat: several plugins each have
    /// something to tell the page, and they compose rather than exclude. The
    /// order is the order they were added in, which is the assembly's, and the
    /// guard removes exactly the one it holds.
    pub fn tap_index(&self, tap: Tap) -> Registered {
        let mut table = self.table.lock().expect("the route table");
        let at = table.next_tap;
        table.next_tap += 1;
        table.taps.push((at, tap));
        Registered {
            table: Arc::clone(&self.table),
            what: What::Tap(at),
        }
    }

    /// Run a body through the registered taps, in order.
    ///
    /// Called by whoever owns the fallback seat on every index response. It
    /// lives here rather than there because the taps are the assembly's and
    /// the frontend is only the document they are applied to.
    pub fn apply_index_taps(&self, html: String) -> String {
        let taps: Vec<Tap> = {
            let table = self.table.lock().expect("the route table");
            table.taps.iter().map(|(_, tap)| Arc::clone(tap)).collect()
        };
        taps.into_iter().fold(html, |html, tap| tap(html))
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
        let head = tokio::time::timeout(HEAD_TIMEOUT, read_head(&stream))
            .await
            .unwrap_or(Ok(None))?;
        let Some((request, head)) = head else {
            respond::write(&mut stream, &Response::status(Status::BadRequest)).await?;
            return lingering_close(stream).await;
        };
        // An upgrade leaves with the socket exactly as it arrived: the head was
        // read by peeking, so the protocol taking over reads its own request
        // and performs its own handshake.
        if request.upgrade {
            let taken = {
                let table = self.table.lock().expect("the route table");
                table.upgrades.get(&request.path).cloned()
            };
            return match taken {
                Some(handler) => {
                    handler(stream, request);
                    Ok(())
                }
                None => {
                    respond::write(&mut stream, &Response::status(Status::NotFound)).await?;
                    lingering_close(stream).await
                }
            };
        }
        // Everything else is answered here, so the head this carrier peeked at
        // is taken off the socket, and then the body a route may want.
        take(&mut stream, head).await?;
        let mut request = request;
        if let Ok(peer) = stream.peer_addr() {
            request.peer = peer.ip();
        }
        match body(&mut stream, &request).await? {
            Body::Read(body) => request.body = body,
            // A body bigger than this carrier will hold is refused before it
            // is read, not after: reading it to say no is the attack working.
            Body::TooLarge => {
                respond::write(&mut stream, &Response::status(Status::TooLarge)).await?;
                return lingering_close(stream).await;
            }
        }
        let found = {
            let table = self.table.lock().expect("the route table");
            table.find(&request)
        };
        let Some(route) = found else {
            respond::write(&mut stream, &Response::status(Status::NotFound)).await?;
            return lingering_close(stream).await;
        };
        let answer = (route.handler)(request).await;
        respond::write(&mut stream, &answer).await?;
        lingering_close(stream).await
    }
}

impl Table {
    /// The route that owns a request: exact, then longest prefix, then the
    /// fallback. The order is the contract; nothing here reads registration
    /// order.
    fn find(&self, request: &Request) -> Option<Route> {
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
            What::Tap(at) => table.taps.retain(|(filed, _)| filed != at),
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
async fn read_head(stream: &TcpStream) -> io::Result<Option<(Request, usize)>> {
    let mut head = vec![0_u8; HEAD_LIMIT];
    let mut waited = 0;
    loop {
        // Peeked rather than read, because an upgrade leaves with a socket
        // nobody has taken bytes off: the protocol that takes over reads its
        // own request. The kernel keeps the bytes until they are taken, so
        // each peek returns everything that has arrived so far, and a head
        // that arrived in pieces is re-read whole rather than reassembled.
        let seen = match stream.peek(&mut head).await? {
            0 => return Ok(None),
            seen => seen,
        };
        // A peek answers with what is already buffered, so a head still
        // arriving would spin this loop. Nothing new means wait a moment; the
        // whole read is bounded by `HEAD_TIMEOUT` either way.
        if seen == waited {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        waited = seen;
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut parsed = httparse::Request::new(&mut headers);
        match parsed.parse(&head[..seen]) {
            Ok(httparse::Status::Complete(length)) => {
                return Ok(Request::of(&parsed).map(|request| (request, length)))
            }
            // A head that fills the buffer and is still not a head is one this
            // carrier will not wait any longer for.
            Ok(httparse::Status::Partial) if seen >= HEAD_LIMIT => return Ok(None),
            Ok(httparse::Status::Partial) => continue,
            Err(_) => return Ok(None),
        }
    }
}

/// The most this carrier will read after a head.
///
/// A prompt with an image in it is the big one, and it is nowhere near this.
const BODY_LIMIT: usize = 8 * 1024 * 1024;

/// What came after the head.
enum Body {
    Read(Vec<u8>),
    TooLarge,
}

/// Read the body a request declared, if it declared one.
///
/// Only `content-length`. A chunked body is not refused here so much as never
/// arranged: this carrier serves a page and an API bridge, both of which send
/// a length, and a transfer encoding nobody sends is a parser nobody has
/// tested.
async fn body(stream: &mut TcpStream, request: &Request) -> io::Result<Body> {
    let Some(length) = request.header("content-length") else {
        return Ok(Body::Read(Vec::new()));
    };
    let Ok(length) = length.trim().parse::<usize>() else {
        return Ok(Body::Read(Vec::new()));
    };
    if length > BODY_LIMIT {
        return Ok(Body::TooLarge);
    }
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).await?;
    Ok(Body::Read(body))
}

/// Take a peeked head off the socket, so the body starts where a reader of it
/// would expect.
async fn take(stream: &mut TcpStream, head: usize) -> io::Result<()> {
    let mut taken = vec![0_u8; head];
    stream.read_exact(&mut taken).await.map(|_| ())
}

#[cfg(test)]
mod tests;
