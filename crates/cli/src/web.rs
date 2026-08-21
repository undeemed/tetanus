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
//! # Why two ports, for now
//!
//! The carrier binds its own port and the manifest names it. Upstream serves
//! both from one server, with the socket on an upgrade route, and so will we -
//! that needs the raw socket handed to the upgrade handler, which is the next
//! slice. Until then the page is told an address rather than assuming one,
//! which is why it was told an address in the first place.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tetanus_host::{Frontend, Manifest, WebServer};
use tetanus_protocol::rpc::{ErrorCode, RpcError};

use crate::{fail, render, settings, Policy, Reported};

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
    // The carrier takes a port of its own from the operating system. Nobody
    // types it: the manifest carries it to the page, and the page dials it.
    let socket = runtime
        .block_on(tokio::net::TcpListener::bind((host.as_str(), 0)))
        .map_err(|refused| {
            fail(
                policy,
                &RpcError::new(ErrorCode::Io, format!("the carrier: {refused}")),
            )
        })?;
    let carrier = runtime
        .block_on(async { socket.local_addr() })
        .map_err(|err| fail(policy, &RpcError::new(ErrorCode::Io, err.to_string())))?;

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
    let _manifest = page.tap_index(
        Manifest {
            carrier: format!("ws://{}", reachable(carrier.port())),
            protocol: tetanus_protocol::PROTOCOL_VERSION.to_string(),
        }
        .tap(),
    );

    render::web::banner(
        &mut err,
        &render::web::Serving {
            page: &format!("http://{}", reachable(page.address().port())),
            carrier: &format!("ws://{}", reachable(carrier.port())),
            sessions: &booted.sessions_root,
            frontend: &frontend.display().to_string(),
        },
    )
    .ok();

    let engine: Arc<dyn tetanus_protocol::methods::Engine> =
        Arc::new(tetanus_engine::HarnessEngine::new(booted));
    let served = runtime.block_on(async {
        tokio::select! {
            served = tetanus_rpc::websocket::serve(engine, socket) => served,
            served = page.clone().serve(pages) => served,
            // The key the banner named. Neither server ends on its own: they
            // accept until the accept fails, so the interrupt is the shutdown
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

/// The name a reader off this machine reaches it by.
fn hostname() -> String {
    std::env::var("TETANUS_PUBLIC_HOST").unwrap_or_else(|_| "localhost".into())
}
