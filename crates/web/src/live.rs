//! The transport that actually leaves the machine.
//!
//! It is deliberately the thinnest thing in this crate: it sends, it reads up
//! to the cap, and it decides nothing. Redirects are off at this layer -
//! following one is a policy decision about origins, and
//! [`crate::fetch`] is where that is made and where it can be tested.
//!
//! Nothing in the offline suite constructs one of these.

use std::collections::BTreeMap;

use futures_util::StreamExt;

use crate::fault::WebFault;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};

/// A live HTTP transport over `reqwest`.
pub struct LiveHttp {
    client: reqwest::Client,
}

impl Default for LiveHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                // The fetcher follows redirects itself, because whether a hop
                // is allowed is a question about origins that this layer has
                // no business answering.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                // The same failure `reqwest::Client::default()` panics on: a
                // TLS backend that will not initialise, which is a broken
                // build rather than a runtime condition.
                .expect("a reqwest client"),
        }
    }
}

#[async_trait::async_trait]
impl HttpTransport for LiveHttp {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, WebFault> {
        let mut sending = match request.method {
            Method::Get => self.client.get(&request.url),
            Method::Post => self.client.post(&request.url),
        }
        .timeout(request.timeout);
        for (name, value) in &request.headers {
            sending = sending.header(name, value);
        }
        if let Some(body) = &request.body {
            sending = sending.body(body.clone());
        }
        let response = sending.send().await.map_err(|source| {
            if source.is_timeout() {
                WebFault::Timeout
            } else {
                WebFault::Provider(format!("{} could not be fetched: {source}", request.url))
            }
        })?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect::<BTreeMap<String, String>>();

        // Read to the cap and stop: a body larger than the cap must cost the
        // cap, not the body.
        let mut body: Vec<u8> = Vec::new();
        let mut truncated = false;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| {
                if source.is_timeout() {
                    WebFault::Timeout
                } else {
                    WebFault::Provider(format!("the body of {} ended early: {source}", request.url))
                }
            })?;
            let room = request.max_bytes.saturating_sub(body.len());
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        Ok(HttpResponse {
            status,
            headers,
            body,
            truncated,
        })
    }
}
