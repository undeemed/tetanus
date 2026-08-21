//! Who may open a WebSocket connection.
//!
//! Contract section 4.1.2: the trust boundary is the connection, because a
//! peer that opens one can do everything the engine can do - start turns that
//! run tools and spend money, read every journal in the server's directory,
//! read the resolved configuration. So the decision about who connects is the
//! whole decision, and it is made here.
//!
//! **Two attacks, and neither needs an exotic threat model.**
//!
//! A server bound off-box is reachable by anyone who can route to it. The
//! captain's standing rule is that served surfaces bind `0.0.0.0` for off-box
//! access, so this is the expected deployment rather than a misconfiguration
//! to warn about.
//!
//! And a browser can reach a loopback port from any page. The same-origin
//! policy does not restrict WebSocket connections the way it restricts
//! `fetch`, so a page the user merely visits can open
//! `ws://127.0.0.1:<port>` and drive the agent. That is why an `Origin` is
//! checked even when the peer is local: "local" and "trusted" stopped being
//! the same thing the moment a browser could be the local peer.
//!
//! **Default deny, and the default is not the absence of a decision.**
//! [`Auth::loopback_only`] is the weakest posture this module offers and it
//! still refuses every off-box peer and every browser origin. There is no
//! constructor that admits everyone.
//!
//! **Upstream leaves this open**, and that is a deliberate difference rather
//! than an omission. `packages/host/webserver` performs no authentication and
//! no origin check, and takes `'127.0.0.1' | '0.0.0.0'` as configuration. A
//! port that hands over the user's whole history and the ability to run
//! commands is not a surface to match bug for bug.

use std::net::IpAddr;

/// The header a browser can set on a WebSocket handshake, and the one this
/// carrier reads a token from.
///
/// A browser's WebSocket API cannot set request headers, so `Authorization` is
/// unavailable to the very client this carrier exists for. The subprotocol
/// field and the URL are the two things it *can* set, and of those the
/// subprotocol is the one that does not end up in access logs.
pub const TOKEN_PROTOCOL_PREFIX: &str = "tetanus.token.";

/// The query parameter a token may arrive in instead.
///
/// Offered because some clients cannot set a subprotocol either. It is the
/// weaker of the two: a URL is written to more logs than a header is, so a
/// deployment that can use the subprotocol should.
pub const TOKEN_QUERY: &str = "token";

/// Why a handshake was refused.
///
/// Each carries what the operator needs to fix it and nothing that helps a
/// prober: the peer is told a status, not which of these it earned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No token was presented and this posture requires one.
    TokenMissing,
    /// A token was presented and is not the one configured.
    TokenWrong,
    /// The peer is not on the loopback interface and this posture admits only
    /// those.
    NotLocal(IpAddr),
    /// The handshake carried an `Origin` that is not allowed. A browser sets
    /// this and cannot forge it.
    OriginNotAllowed(String),
}

impl Refusal {
    /// What the peer is told. Deliberately coarse.
    ///
    /// A prober that can tell "no token" from "wrong token" learns whether it
    /// has found a server expecting the token it is guessing, and a peer that
    /// can tell "wrong token" from "not local" learns the posture. Neither
    /// helps a legitimate client, which either has the token or does not.
    pub fn status(&self) -> u16 {
        401
    }

    /// What the operator is told, in the server's own log.
    pub fn reason(&self) -> String {
        match self {
            Refusal::TokenMissing => "no token was presented".into(),
            Refusal::TokenWrong => "the token presented is not the one configured".into(),
            Refusal::NotLocal(ip) => format!("peer {ip} is not on the loopback interface"),
            Refusal::OriginNotAllowed(origin) => format!("origin {origin:?} is not allowed"),
        }
    }
}

/// What one handshake presented.
///
/// Taken as a value rather than read from a request type, so the decision is
/// testable without building an HTTP request and cannot accidentally depend on
/// anything else the request carried.
#[derive(Debug, Clone, Default)]
pub struct Presented {
    pub token: Option<String>,
    pub origin: Option<String>,
}

/// Who may connect.
#[derive(Debug, Clone)]
pub struct Auth {
    token: Option<String>,
    allowed_origins: Vec<String>,
    loopback_only: bool,
}

impl Auth {
    /// Require this token from every peer, wherever it connects from.
    ///
    /// The posture contract section 4.1.2 describes, and the one a deployment
    /// bound off-box must use.
    pub fn require_token(token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
            allowed_origins: Vec::new(),
            loopback_only: false,
        }
    }

    /// Admit local peers without a token, and nobody else.
    ///
    /// The weakest posture offered, for a client on the same machine that has
    /// no way to be given a secret yet. It still refuses every off-box peer
    /// and every browser origin, so the two attacks in the module note are
    /// closed either way.
    ///
    /// It is not safe on a shared machine, where another local account is a
    /// local peer. A deployment that has one uses [`require_token`](Self::require_token).
    pub fn loopback_only() -> Self {
        Self {
            token: None,
            allowed_origins: Vec::new(),
            loopback_only: true,
        }
    }

    /// Also accept handshakes carrying this `Origin`.
    ///
    /// Needed only for a browser surface. Every origin is refused until one is
    /// named, which is what stops a page the user happens to be visiting from
    /// driving the agent.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }

    /// The token a client must present, when there is one.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Decide one handshake.
    pub fn admit(&self, peer: IpAddr, presented: &Presented) -> Result<(), Refusal> {
        // Origin first, and for every posture. A browser is the one peer that
        // cannot be trusted merely for being local, and it is the one peer
        // that always announces itself.
        if let Some(origin) = &presented.origin {
            if !self.allowed_origins.iter().any(|allowed| allowed == origin) {
                return Err(Refusal::OriginNotAllowed(origin.clone()));
            }
        }

        match &self.token {
            Some(expected) => match &presented.token {
                None => Err(Refusal::TokenMissing),
                // Constant time, because a token compared byte by byte with an
                // early return leaks its prefix to a peer that can time the
                // refusal - and a peer that can connect can time it precisely.
                Some(given) if constant_time_eq(given, expected) => Ok(()),
                Some(_) => Err(Refusal::TokenWrong),
            },
            None if self.loopback_only && !peer.is_loopback() => Err(Refusal::NotLocal(peer)),
            None => Ok(()),
        }
    }

    /// Pull a token out of what a handshake carried.
    ///
    /// The subprotocol is preferred over the query parameter, because a URL
    /// reaches more logs than a header does.
    pub fn present(protocols: Option<&str>, query: Option<&str>) -> Presented {
        let from_protocol = protocols.and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find_map(|entry| entry.strip_prefix(TOKEN_PROTOCOL_PREFIX))
                .map(str::to_string)
        });
        let from_query = query.and_then(|value| {
            value.split('&').find_map(|pair| {
                pair.split_once('=')
                    .filter(|(key, _)| *key == TOKEN_QUERY)
                    .map(|(_, token)| token.to_string())
            })
        });
        Presented {
            token: from_protocol.or(from_query),
            origin: None,
        }
    }
}

impl Default for Auth {
    /// Deny by default: the weakest posture, never an open one.
    fn default() -> Self {
        Self::loopback_only()
    }
}

/// Compare two secrets without returning early.
///
/// Length is not secret - a token's length is a property of how the server
/// mints them, not of the value - so it is compared first and directly. The
/// bytes are not: a comparison that stopped at the first difference would let
/// a peer recover the token one byte at a time by measuring how long the
/// refusal took.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut differing = 0u8;
    for (x, y) in a.iter().zip(b) {
        differing |= x ^ y;
    }
    differing == 0
}
