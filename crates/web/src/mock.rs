//! A transport a case scripts, and a search provider that answers the same
//! thing every time.
//!
//! These are not test-only in the build sense - they ship - because they are
//! how a deployment runs the web tools offline: a demo, an air-gapped
//! evaluation, a reproduction of a bug that needs the same page every time.
//! The suite is the first caller, not the only intended one.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::fault::WebFault;
use crate::http::{HttpRequest, HttpResponse, HttpTransport};
use crate::search::{Availability, SearchAnswer, SearchProvider, SearchQuery, Source};

/// What a scripted transport answers for one URL.
pub type Scripted = Result<HttpResponse, WebFault>;

/// Anything a case can script an answer with: a response, a fault, or the
/// `Result` of either. It exists so a case reads as what it is scripting
/// rather than as a wrapper around it.
pub trait Answer {
    fn scripted(self) -> Scripted;
}

impl Answer for HttpResponse {
    fn scripted(self) -> Scripted {
        Ok(self)
    }
}

impl Answer for WebFault {
    fn scripted(self) -> Scripted {
        Err(self)
    }
}

impl Answer for Scripted {
    fn scripted(self) -> Scripted {
        self
    }
}

/// A transport that answers from a script and records what it was asked.
#[derive(Default)]
pub struct MockHttp {
    pages: Mutex<BTreeMap<String, Vec<Scripted>>>,
    /// The answer for a URL the script does not name.
    fallback: Mutex<Option<Scripted>>,
    asked: Mutex<Vec<HttpRequest>>,
}

impl MockHttp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `url` with `answer`. Repeated calls queue: the first answer is
    /// given once, then the next, and the last one repeats for ever - which is
    /// what a redirect chain and a retry both need.
    pub fn page(self, url: &str, answer: impl Answer) -> Self {
        self.pages
            .lock()
            .expect("pages")
            .entry(url.to_string())
            .or_default()
            .push(answer.scripted());
        self
    }

    /// Answer anything the script does not name.
    pub fn otherwise(self, answer: impl Answer) -> Self {
        *self.fallback.lock().expect("fallback") = Some(answer.scripted());
        self
    }

    /// Every request that was made, in order.
    pub fn asked(&self) -> Vec<HttpRequest> {
        self.asked.lock().expect("asked").clone()
    }
}

#[async_trait::async_trait]
impl HttpTransport for MockHttp {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, WebFault> {
        self.asked.lock().expect("asked").push(request.clone());
        let mut pages = self.pages.lock().expect("pages");
        match pages.get_mut(&request.url) {
            Some(queued) if queued.len() > 1 => queued.remove(0),
            Some(queued) => queued[0].clone(),
            None => self
                .fallback
                .lock()
                .expect("fallback")
                .clone()
                .unwrap_or_else(|| {
                    Err(WebFault::Provider(format!(
                        "nothing is scripted for {}",
                        request.url
                    )))
                }),
        }
    }
}

/// Build a plain 200 answer.
pub fn ok(content_type: &str, body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: BTreeMap::from([("content-type".to_string(), content_type.to_string())]),
        body: body.as_bytes().to_vec(),
        truncated: false,
    }
}

/// Build a redirect answer.
pub fn redirect(status: u16, location: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::from([("location".to_string(), location.to_string())]),
        body: Vec::new(),
        truncated: false,
    }
}

/// A search provider that answers the same results for every query.
///
/// Deterministic on purpose: the point of a search case is what the tool does
/// with results, and a provider that varied would make every such case a
/// question about the provider.
pub struct MockSearch {
    id: String,
    availability: Availability,
    answer: SearchAnswer,
    seen: Mutex<Vec<SearchQuery>>,
}

impl MockSearch {
    /// A usable provider answering with one source per name given.
    pub fn new(id: &str, sources: &[(&str, &str)]) -> Self {
        Self {
            id: id.to_string(),
            availability: Availability::Usable,
            answer: SearchAnswer {
                answer: Some(format!("what {id} found")),
                sources: sources
                    .iter()
                    .map(|(title, url)| Source {
                        title: (*title).to_string(),
                        url: (*url).to_string(),
                        snippet: Some(format!("about {title}")),
                        published: None,
                    })
                    .collect(),
                truncated: false,
            },
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The same provider, registered but not usable - a missing key, say.
    pub fn unusable(id: &str, why: &str) -> Self {
        let mut provider = Self::new(id, &[]);
        provider.availability = Availability::Unusable(why.to_string());
        provider
    }

    /// Every query this provider was asked.
    pub fn asked(&self) -> Vec<SearchQuery> {
        self.seen.lock().expect("seen").clone()
    }
}

#[async_trait::async_trait]
impl SearchProvider for MockSearch {
    fn id(&self) -> &str {
        &self.id
    }

    fn availability(&self) -> Availability {
        self.availability.clone()
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchAnswer, WebFault> {
        self.seen.lock().expect("seen").push(query.clone());
        Ok(self.answer.clone())
    }
}
