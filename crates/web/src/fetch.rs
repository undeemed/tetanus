//! Fetching one page, under limits that are stated rather than hoped for.
//!
//! **A URL is judged before anything is sent.** A scheme that is not `http` or
//! `https` never reaches a socket, and neither does a URL carrying
//! credentials: `https://user:token@host/` in a model's output is a credential
//! about to be sent somewhere, and no fetch is worth that.
//!
//! **A redirect is followed, watched, and re-judged.** Each hop is checked
//! against the same rules as the first URL, and a hop that leaves the origin
//! is refused - a page that redirects to an internal address is how a fetch
//! tool becomes a request forgery. The hop count is capped separately, and the
//! two refusals are told apart in the message even though upstream gives them
//! one code.
//!
//! **A size limit is enforced twice.** A declared `Content-Length` past the
//! cap is refused before the body is read; a server that declares nothing, or
//! lies, is cut at the cap and the answer says it was cut.
//!
//! **A content type is required, and it is a short list.** A fetch that
//! decoded anything would hand a model a binary as text. No content type at
//! all is refused too, because guessing is what the list exists to avoid.

use std::collections::BTreeMap;
use std::time::Duration;

use url::Url;

use crate::fault::WebFault;
use crate::http::{HttpRequest, HttpResponse, HttpTransport};

/// What this fetch will read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Html,
    Text,
    Markdown,
    Json,
}

impl MediaKind {
    /// The media type a fetch accepts, or `None` for one it does not.
    pub fn of(media_type: &str) -> Option<Self> {
        match media_type {
            "text/html" | "application/xhtml+xml" => Some(Self::Html),
            "text/markdown" | "text/x-markdown" => Some(Self::Markdown),
            "application/json" | "text/json" => Some(Self::Json),
            "text/plain" | "text/csv" | "text/xml" | "application/xml" => Some(Self::Text),
            _ => None,
        }
    }
}

/// The bounds one fetch runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchLimits {
    /// Bytes of body read before the fetch is cut short.
    pub max_bytes: usize,
    /// Characters of decoded text kept. A byte cap is not a character cap:
    /// one is about the socket and the other about the model's context.
    pub max_chars: usize,
    /// Hops followed. Zero follows none and still fetches a direct answer.
    pub max_redirects: u8,
    pub timeout: Duration,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            // Upstream's defaults: enough for a long article, not enough to
            // matter to a process.
            max_bytes: 5 * 1024 * 1024,
            max_chars: 100_000,
            max_redirects: 5,
            timeout: Duration::from_secs(30),
        }
    }
}

/// What one fetch produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// Where the body actually came from, after redirects.
    pub final_url: String,
    pub status: u16,
    pub media_type: String,
    pub kind: MediaKind,
    /// The body, decoded and rendered to text.
    pub text: String,
    /// Whether anything was dropped - by the byte cap, by the character cap,
    /// or by a server that stopped early.
    pub truncated: bool,
    /// How many redirects were followed to get here.
    pub hops: u8,
}

/// The identity this fetch presents. Stated rather than blank, for the reason
/// `crates/turn/src/llm/attribution.rs` gives: anonymous traffic is traffic
/// nobody can attribute when it turns out to be a problem.
pub const USER_AGENT: &str = concat!("tetanus/", env!("CARGO_PKG_VERSION"), " (+web_fetch)");

/// Fetch one URL through `transport`, following redirects within the limits.
pub async fn fetch(
    transport: &dyn HttpTransport,
    url: &str,
    limits: FetchLimits,
) -> Result<Fetched, WebFault> {
    let mut target = judged(url)?;
    let origin = origin_of(&target);
    let mut hops = 0u8;

    loop {
        let response = transport.send(&request(target.as_str(), limits)).await?;

        if response.is_redirect() {
            if hops == limits.max_redirects {
                return Err(WebFault::RedirectBlocked(format!(
                    "the chain of redirects from {url} is longer than the {} this fetch follows",
                    limits.max_redirects
                )));
            }
            let location = response.header("location").ok_or_else(|| {
                WebFault::Provider(format!(
                    "{target} answered {} with no Location to follow",
                    response.status
                ))
            })?;
            // Relative locations are ordinary, so the next hop is resolved
            // against the one that sent it rather than parsed on its own.
            let next = target.join(location).map_err(|source| {
                WebFault::BadUrl(format!(
                    "{target} redirected to {location:?}, which is not a URL: {source}"
                ))
            })?;
            let next = judged(next.as_str())?;
            if origin_of(&next) != origin {
                return Err(WebFault::RedirectBlocked(format!(
                    "{url} redirected to {}, which is a different origin",
                    origin_of(&next)
                )));
            }
            target = next;
            hops += 1;
            continue;
        }

        return read(&response, &target, hops, limits);
    }
}

/// Turn one settled response into an answer, or say why it is not one.
fn read(
    response: &HttpResponse,
    target: &Url,
    hops: u8,
    limits: FetchLimits,
) -> Result<Fetched, WebFault> {
    let declared = response
        .header("content-length")
        .and_then(|value| value.trim().parse::<u64>().ok());
    if declared.is_some_and(|length| length > limits.max_bytes as u64) {
        return Err(WebFault::TooLarge {
            limit: limits.max_bytes,
            declared,
        });
    }

    let content_type = response
        .header("content-type")
        .ok_or_else(|| WebFault::UnsupportedType("nothing at all".to_string()))?;
    let (media_type, charset) = split_content_type(content_type);
    let kind = MediaKind::of(&media_type).ok_or(WebFault::UnsupportedType(media_type.clone()))?;

    let decoded = decode(&response.body, charset.as_deref())?;
    let rendered = match kind {
        MediaKind::Html => crate::render::html_to_text(&decoded),
        _ => decoded,
    };
    let (text, cut) = cut_to(&rendered, limits.max_chars);

    Ok(Fetched {
        final_url: target.to_string(),
        status: response.status,
        media_type,
        kind,
        text,
        truncated: response.truncated || cut,
        hops,
    })
}

/// A URL this fetch is willing to send, or the reason it is not.
fn judged(url: &str) -> Result<Url, WebFault> {
    let parsed = Url::parse(url.trim())
        .map_err(|source| WebFault::BadUrl(format!("{url:?} is not a URL: {source}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(WebFault::BadUrl(format!(
            "this fetch speaks http and https, not {:?}",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(WebFault::BadUrl(format!("{url:?} names no host")));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(WebFault::BadUrl(
            "a URL carrying credentials is not fetched: it would send them to whoever answers"
                .to_string(),
        ));
    }
    Ok(parsed)
}

/// Scheme, host and port - the three things that make two URLs the same
/// origin.
fn origin_of(url: &Url) -> String {
    format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    )
}

fn request(url: &str, limits: FetchLimits) -> HttpRequest {
    HttpRequest {
        method: crate::http::Method::Get,
        url: url.to_string(),
        body: None,
        headers: BTreeMap::from([
            ("user-agent".to_string(), USER_AGENT.to_string()),
            (
                "accept".to_string(),
                "text/html, text/plain, text/markdown, application/json;q=0.9, */*;q=0.1"
                    .to_string(),
            ),
        ]),
        max_bytes: limits.max_bytes,
        timeout: limits.timeout,
    }
}

/// The media type and the charset a `Content-Type` states.
fn split_content_type(header: &str) -> (String, Option<String>) {
    let mut parts = header.split(';');
    let media = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let charset = parts.find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key.trim().eq_ignore_ascii_case("charset"))
            .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
    });
    (media, charset)
}

/// Decode a body under the charset it declared.
///
/// Two encodings, and everything else refused: UTF-8, which is what the web
/// is, and ISO-8859-1, which is what the parts of it that are not UTF-8 mostly
/// are. Guessing at anything else - or decoding it as UTF-8 and hoping - hands
/// a model text that is subtly not what the page said, which is worse than a
/// refusal it can report.
fn decode(body: &[u8], charset: Option<&str>) -> Result<String, WebFault> {
    match charset.unwrap_or("utf-8") {
        "utf-8" | "utf8" | "us-ascii" | "ascii" => {
            // A body cut at the byte cap routinely ends mid-character, and
            // that is not a reason to refuse the page.
            Ok(String::from_utf8_lossy(body).into_owned())
        }
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" => {
            Ok(body.iter().map(|byte| char::from(*byte)).collect())
        }
        other => Err(WebFault::UnsupportedCharset(other.to_string())),
    }
}

/// The first `bound` characters, and whether anything was dropped.
fn cut_to(text: &str, bound: usize) -> (String, bool) {
    match text.char_indices().nth(bound) {
        None => (text.to_string(), false),
        Some((at, _)) => (text[..at].to_string(), true),
    }
}
