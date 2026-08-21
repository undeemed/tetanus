//! `tetanus web`: the browser panel, with a carrier behind it.
//!
//! This is the composition upstream calls the web shell: an HTTP server for
//! the page, a WebSocket carrier for the protocol, and a boot manifest that
//! tells the first what the second bound. Every piece of it already existed -
//! `crates/host` is the carrier of routes, `crates/rpc` is the protocol's own
//! WebSocket, `web/app` is the page - and what this file adds is the wiring
//! and the sentence a person reads when it comes up.
//!
//! # Why the page is not patched on its way out
//!
//! The development server this replaces read the carrier's port out of a
//! banner and string-replaced a global into the HTML as it served it. That
//! works exactly once: the page only runs when served by that one program, and
//! anybody opening the file another way gets a page with a hole in it. The
//! manifest goes through the host's index tap instead, which is a published
//! seam any assembly can write to, and the page reads it as data.
//!
//! # One port
//!
//! The page and the protocol are the same server: the socket lives on an
//! upgrade route at `/api/ws`, which is upstream's arrangement and is worth
//! copying for a reason that has nothing to do with tidiness. A page served
//! from one origin and dialling another is a cross-origin WebSocket, which is
//! the case `crates/rpc`'s own origin check exists to refuse (contract
//! §4.1.2). Same origin, and the check protects the deployment instead of
//! fighting the page.
//!
//! The manifest still names the address rather than the page assuming it,
//! because a page opened through a proxy, or with `?ws=`, is told where to go
//! by whoever put it there.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tetanus_host::{Frontend, Manifest, WebServer};
use tetanus_protocol::rpc::{ErrorCode, RpcError};

use crate::{fail, render, settings, Policy, Reported};

/// Where the protocol lives on the page's own server.
///
/// Under `/api` because that is the prefix upstream's connection plugin owns,
/// and named rather than guessed at because the manifest tells the page.
const CARRIER: &str = "/api/ws";

/// Serve the page and the protocol until the reader stops it.
pub fn web(
    policy: &Policy,
    document: &Path,
    dir: Option<PathBuf>,
    listen: &str,
    frontend: &Path,
) -> Result<(), Reported> {
    let mut err = policy.stderr();
    // The frontend is checked before anything is bound. A server that came up
    // on the address a person is about to open, and then answered every
    // request with "no index.html", is a worse failure than not coming up.
    let index = frontend.join("index.html");
    if !index.is_file() {
        return Err(fail(
            policy,
            &RpcError::new(
                ErrorCode::Io,
                format!("{}: there is no index.html to serve", frontend.display()),
            )
            .with_data(serde_json::json!({ "path": frontend.display().to_string() })),
        ));
    }
    let booted = settings::booted(policy, document, &settings::root(dir.as_deref()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| crate::report(policy, &err.to_string(), None))?;

    let (host, page, pages) = bound(policy, &runtime, listen)?;

    // The address a reader can reach, which is not always the one that was
    // bound: `0.0.0.0` is every interface and nobody's hostname.
    let reachable = |port: u16| match host.as_str() {
        "0.0.0.0" => format!("{}:{port}", hostname()),
        host => format!("{host}:{port}"),
    };
    let _seat = Frontend::mount(&page, &index).map_err(|taken| {
        fail(
            policy,
            &RpcError::new(ErrorCode::Internal, taken.to_string()),
        )
    })?;
    let address = format!("ws://{}{CARRIER}", reachable(page.address().port()));
    let _manifest = page.tap_index(
        Manifest {
            carrier: address.clone(),
            protocol: tetanus_protocol::PROTOCOL_VERSION.to_string(),
        }
        .tap(),
    );

    render::web::banner(
        &mut err,
        &render::web::Serving {
            page: &format!("http://{}", reachable(page.address().port())),
            carrier: &address,
            sessions: &booted.sessions_root,
            frontend: &frontend.display().to_string(),
        },
    )
    .ok();

    let engine: Arc<dyn tetanus_protocol::methods::Engine> =
        Arc::new(tetanus_engine::HarnessEngine::new(booted));
    // The origins this server is willing to be dialled from: its own, in the
    // spellings a browser sends. §4.1.2 refuses every browser origin until one
    // is named, and the point of serving the socket on the page's own port is
    // that the page's origin is one we know rather than one we guess.
    // `localhost` and `127.0.0.1` are different origins to a browser and the
    // same machine to everybody else, so both are named when the bind is
    // loopback.
    let auth = origins(&host, page.address().port());

    // The socket seat. `crates/rpc` is handed the connection with nothing read
    // off it, so its handshake, its origin check and its token check all run
    // exactly as they do on `tetanus serve --listen`: this composition adds a
    // door, not a second doorman.
    let _socket = page
        .register_upgrade(
            CARRIER,
            Arc::new(move |stream, _| {
                let engine = Arc::clone(&engine);
                let auth = Arc::clone(&auth);
                tokio::spawn(async move {
                    let peer = stream
                        .peer_addr()
                        .map(|peer| peer.ip())
                        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
                    let _ =
                        tetanus_rpc::websocket::connection_as(engine, stream, &auth, peer).await;
                });
            }),
        )
        .map_err(|taken| {
            fail(
                policy,
                &RpcError::new(ErrorCode::Internal, taken.to_string()),
            )
        })?;

    let served = runtime.block_on(async {
        tokio::select! {
            served = page.clone().serve(pages) => served,
            // The key the banner named. The server has no end of its own: it
            // accepts until the accept fails, so the interrupt is the shutdown
            // and this exits 0, the way `tetanus serve` does for the same key.
            _ = tokio::signal::ctrl_c() => Ok(()),
        }
    });
    served.map_err(|broken| fail(policy, &RpcError::new(ErrorCode::Io, broken.to_string())))?;
    render::web::stopped(&mut err).ok();
    Ok(())
}

/// The host, bound, and the host part of the address it was given.
type Bound = (String, WebServer, tokio::net::TcpListener);

fn bound(
    policy: &Policy,
    runtime: &tokio::runtime::Runtime,
    listen: &str,
) -> Result<Bound, Reported> {
    let (host, port) = listen.rsplit_once(':').unwrap_or((listen, "5300"));
    let port: u16 = port.parse().map_err(|_| {
        fail(
            policy,
            &RpcError::new(
                ErrorCode::InvalidParams,
                format!("{listen}: that is not an address with a port"),
            ),
        )
    })?;
    let (server, listener) = runtime
        .block_on(WebServer::bind(host, port))
        .map_err(|refused| {
            fail(
                policy,
                &RpcError::new(ErrorCode::Io, format!("{listen}: {refused}")),
            )
        })?;
    Ok((host.to_string(), server, listener))
}

/// The origins a page this server served is allowed to dial it from.
///
/// A browser sends the origin it loaded the page from, so the set is exactly
/// the addresses this server can be opened on. Nothing else is added: an
/// origin that is not one of ours is the cross-site case §4.1.2 exists to
/// refuse, and a wildcard here would be that refusal deleted.
fn origins(host: &str, port: u16) -> Arc<tetanus_rpc::auth::Auth> {
    let mut auth = tetanus_rpc::auth::Auth::default();
    let names = match host {
        "0.0.0.0" => vec![hostname(), "localhost".into(), "127.0.0.1".into()],
        host => vec![host.to_string(), "localhost".into()],
    };
    for name in names {
        // Both schemes, because a deployment behind a proxy terminating TLS
        // sends `https` while the server behind it is plain.
        auth = auth.allow_origin(format!("http://{name}:{port}"));
        auth = auth.allow_origin(format!("https://{name}:{port}"));
    }
    Arc::new(auth)
}

/// The name a reader off this machine reaches it by.
fn hostname() -> String {
    std::env::var("TETANUS_PUBLIC_HOST").unwrap_or_else(|_| "localhost".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TC-CLI-WEB-3: the origins a loopback panel and a wildcard panel allow.
    /// Expected: exactly the addresses this server can be opened on, in both
    /// schemes, and nothing else. §4.1.2 refuses every browser origin until
    /// one is named; the point of serving the socket on the page's own port is
    /// that the page's origin is one we know rather than one we guess, and a
    /// wildcard here would be that refusal deleted.
    #[test]
    fn only_the_origins_this_server_serves_are_allowed() {
        let loopback = origins("127.0.0.1", 5300);
        let allows = |auth: &tetanus_rpc::auth::Auth, origin: &str| {
            auth.admit(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                &tetanus_rpc::auth::Presented {
                    origin: Some(origin.to_string()),
                    ..Default::default()
                },
            )
            .is_ok()
        };
        assert!(allows(&loopback, "http://127.0.0.1:5300"));
        // A browser calls these two different origins; a person calls them one
        // machine.
        assert!(allows(&loopback, "http://localhost:5300"));
        // Behind a proxy that terminates TLS, the page is https and the server
        // behind it is not.
        assert!(allows(&loopback, "https://localhost:5300"));

        assert!(!allows(&loopback, "http://127.0.0.1:5301"), "another port");
        assert!(!allows(&loopback, "http://evil.example"), "another site");
        assert!(!allows(&loopback, "null"), "a sandboxed frame");
    }

    /// TC-CLI-WEB-4: the wildcard bind, which is every interface and nobody's
    /// hostname.
    /// Expected: the public name is included, so a reader off the machine can
    /// open the page at all, and the loopback names stay for the person
    /// sitting at it.
    #[test]
    fn a_wildcard_bind_allows_the_name_a_reader_reaches_it_by() {
        std::env::set_var("TETANUS_PUBLIC_HOST", "example.test");
        let wildcard = origins("0.0.0.0", 5300);
        std::env::remove_var("TETANUS_PUBLIC_HOST");

        let allows = |auth: &tetanus_rpc::auth::Auth, origin: &str| {
            auth.admit(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                &tetanus_rpc::auth::Presented {
                    origin: Some(origin.to_string()),
                    ..Default::default()
                },
            )
            .is_ok()
        };
        assert!(allows(&wildcard, "http://example.test:5300"));
        assert!(allows(&wildcard, "http://127.0.0.1:5300"));
        assert!(!allows(&wildcard, "http://elsewhere.test:5300"));
    }
}
