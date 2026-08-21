//! The built frontend, served on the carrier's one fallback seat.
//!
//! Upstream's `host/frontend-static` is "a function plugin that claims the
//! webserver's single fallback seat and serves the built frontend directory
//! with the shell's locked semantics". The semantics are locked because a
//! single-page application depends on them: a path that is not a file has to
//! come back as the page, or every deep link a reader bookmarks is a 404.
//!
//! # The four rules
//!
//! - A path that escapes the dist root is **403**, whatever it spells itself
//!   as. The check is on the resolved path, not on the text, because `%2e%2e`
//!   and a symlink both spell `..` without writing it.
//! - A miss is **`index.html` with 200**. Not 404, not a redirect: the router
//!   in the page is what decides whether `/sessions/17` means anything, and it
//!   cannot decide if the server answered first.
//! - An extension this table does not know ships as
//!   `application/octet-stream`. A guess is worse than a download.
//! - Anything but GET or HEAD that reached the fallback is **405**. A POST to a
//!   path no named route claimed is a caller talking to an API that is not
//!   there, and answering it with the page would say the opposite.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{answered, Registered, Request, Response, Status, Taken, WebServer};

/// The types this shell actually ships, and nothing aspirational.
///
/// Upstream calls its own table minimal and says so on purpose: it covers what
/// the bundler emits plus the manifest, and an extension that has never been
/// served has no business being guessed at.
const TYPES: &[(&str, &str)] = &[
    ("html", "text/html; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("mjs", "text/javascript; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("json", "application/json; charset=utf-8"),
    ("webmanifest", "application/manifest+json; charset=utf-8"),
    ("map", "application/json; charset=utf-8"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("ico", "image/x-icon"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("wasm", "application/wasm"),
    ("txt", "text/plain; charset=utf-8"),
];

/// What an unknown extension ships as.
const UNKNOWN: &str = "application/octet-stream";

/// The built frontend directory, and the index inside it.
pub struct Frontend {
    root: PathBuf,
    index: PathBuf,
}

impl Frontend {
    /// Claim the fallback seat for the directory `dist_index` sits in.
    ///
    /// The index file is named rather than the directory, because that is the
    /// fact a composing assembly actually holds: it resolves the frontend
    /// package's export and gets a file. The root is where that file lives.
    ///
    /// The seat is single-owner, so this fails when something already holds
    /// it; dropping the returned guard gives it back, after which the carrier
    /// answers 404 again.
    pub fn mount(server: &WebServer, dist_index: &Path) -> Result<Registered, Taken> {
        let root = dist_index
            .parent()
            .unwrap_or(Path::new("."))
            .canonicalize()
            .unwrap_or_else(|_| dist_index.parent().unwrap_or(Path::new(".")).to_path_buf());
        let frontend = Arc::new(Frontend {
            index: root.join(
                dist_index
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("index.html")),
            ),
            root,
        });
        let carrier = server.clone();
        server.register_fallback(answered(move |request| frontend.answer(&carrier, &request)))
    }

    /// What one request to the frontend gets.
    fn answer(&self, server: &WebServer, request: &Request) -> Response {
        if !matches!(request.method.as_str(), "GET" | "HEAD") {
            return Response::text(
                Status::MethodNotAllowed,
                "only GET and HEAD are served here",
            )
            .with("allow", "GET, HEAD");
        }
        match self.resolve(&request.path) {
            Asked::Outside => Response::text(Status::Forbidden, "that is outside the frontend"),
            Asked::File(path) => match std::fs::read(&path) {
                Ok(body) => Response::body(Status::Ok, kind(&path), body),
                // A file that resolved and then would not be read is the disk
                // saying something the reader cannot act on. It is the page's
                // absence, not a server fault: the SPA rule covers it.
                Err(_) => self.page(server),
            },
            Asked::Page => self.page(server),
        }
    }

    /// The index, with every registered tap run over it.
    fn page(&self, server: &WebServer) -> Response {
        let Ok(html) = std::fs::read_to_string(&self.index) else {
            return Response::text(Status::NotFound, "the frontend has no index.html");
        };
        Response::body(
            Status::Ok,
            "text/html; charset=utf-8",
            server.apply_index_taps(html),
        )
    }

    /// Where a request path lands: a file, the page, or off the property.
    fn resolve(&self, path: &str) -> Asked {
        let Ok(decoded) = decode(path) else {
            // A malformed escape is not a path this server will guess at, and
            // §the carrier answers a bad request rather than serving the page
            // to a caller who asked for something unreadable.
            return Asked::Outside;
        };
        // A path is joined component by component and `..` is refused outright
        // rather than resolved, because a `..` that resolves inside the root
        // today resolves outside it the moment a directory moves.
        let mut walked = self.root.clone();
        for part in decoded.split('/') {
            match part {
                "" | "." => continue,
                ".." => return Asked::Outside,
                part => walked.push(part),
            }
        }
        let Ok(real) = walked.canonicalize() else {
            return Asked::Page;
        };
        // The resolved path, not the written one: a symlink out of the root
        // spells no `..` at all.
        if !real.starts_with(&self.root) {
            return Asked::Outside;
        }
        match real.is_file() {
            true => Asked::File(real),
            false => Asked::Page,
        }
    }
}

/// What a request path turned out to be.
enum Asked {
    File(PathBuf),
    Page,
    Outside,
}

/// The type a file ships as, by extension, and octet-stream for the rest.
fn kind(path: &Path) -> &'static str {
    let Some(extension) = path.extension().and_then(|end| end.to_str()) else {
        return UNKNOWN;
    };
    let lower = extension.to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(known, _)| *known == lower)
        .map(|(_, kind)| *kind)
        .unwrap_or(UNKNOWN)
}

/// Percent-decode a request path, refusing an escape that is not one.
fn decode(path: &str) -> Result<String, ()> {
    let mut out = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = path.get(at + 1..at + 3).ok_or(())?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| ())?);
                at += 3;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| ())
}
