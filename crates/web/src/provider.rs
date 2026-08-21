//! One real search provider, over the same transport seam as a fetch.
//!
//! DeepSeek's search is a Messages request carrying the `web_search` server
//! tool, and the answer is a list of content blocks: `web_search_tool_result`
//! blocks holding the pages found, and `text` blocks whose citations quote
//! them. This maps that to [`SearchAnswer`], and it maps every way the request
//! can fail to a [`WebFault`] with the code upstream gives it.
//!
//! **It goes through [`HttpTransport`] like everything else here**, so the
//! whole mapping - the request that goes out, the shapes that come back, the
//! failures - is asserted offline. A provider that reached for a socket of its
//! own would be a provider nobody could test.
//!
//! **A key is a reason to be unusable, not a reason to fail a search.** A
//! provider with no credential answers [`Availability::Unusable`], so
//! [`crate::search::WebRuntime`] leaves it out of the choice and says why
//! rather than dispatching to it and reporting an authentication failure.
//!
//! **A prose-only answer is a provider error.** Upstream calls this strict
//! mode: a search that came back with no result block found nothing it can
//! cite, and handing a model an uncited paragraph as search results is how a
//! citation becomes a hallucination.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::fault::WebFault;
use crate::http::{HttpRequest, HttpTransport, Method};
use crate::search::{Availability, SearchAnswer, SearchProvider, SearchQuery, Source};

/// The id a deployment configures this provider by.
pub const ID: &str = "deepseek";

/// Where the request goes when a deployment names nothing.
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// The model the search request runs on.
pub const DEFAULT_MODEL: &str = "deepseek-chat";

/// How this provider is set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekSearchConfig {
    pub base_url: String,
    pub model: String,
    /// The credential. Absent or blank makes the provider unusable, which is
    /// the ordinary state of a deployment that has not configured search.
    pub api_key: Option<String>,
    /// How many searches the model may run per request.
    pub max_uses: u32,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl Default for DeepSeekSearchConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: None,
            max_uses: 5,
            max_tokens: 2048,
            timeout: Duration::from_secs(30),
        }
    }
}

/// DeepSeek's search, over the transport seam.
pub struct DeepSeekSearch<T: HttpTransport> {
    transport: T,
    config: DeepSeekSearchConfig,
}

impl<T: HttpTransport> DeepSeekSearch<T> {
    pub fn new(transport: T, config: DeepSeekSearchConfig) -> Self {
        Self { transport, config }
    }

    /// The transport this provider sends through. A caller that scripted one
    /// reads back what was sent through this.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }
}

#[async_trait::async_trait]
impl<T: HttpTransport> SearchProvider for DeepSeekSearch<T> {
    fn id(&self) -> &str {
        ID
    }

    fn availability(&self) -> Availability {
        let Some(key) = self.config.api_key.as_deref() else {
            return Availability::Unusable(
                "no API key is configured for the DeepSeek search provider".to_string(),
            );
        };
        if key.trim().is_empty() {
            // The same judgement `crates/turn` makes about a provider key: a
            // value of nothing but whitespace is not a credential, and sending
            // it would be an authentication failure with a confusing message.
            return Availability::Unusable(
                "the DeepSeek search key is blank, which is not a credential".to_string(),
            );
        }
        if url::Url::parse(&self.config.base_url).is_err() {
            return Availability::Unusable(format!(
                "the DeepSeek search base URL {:?} is not a URL",
                self.config.base_url
            ));
        }
        if self.config.max_uses == 0 || self.config.max_tokens == 0 {
            return Availability::Unusable(
                "the DeepSeek search limits must both be at least one".to_string(),
            );
        }
        Availability::Usable
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchAnswer, WebFault> {
        if let Availability::Unusable(why) = self.availability() {
            return Err(WebFault::Provider(why));
        }
        let key = self.config.api_key.clone().unwrap_or_default();
        let body = json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "messages": [{ "role": "user", "content": query.text }],
            "tools": [{
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": query.max_results.map_or(self.config.max_uses, |cap| cap.max(1) as u32),
            }],
        });

        let response = self
            .transport
            .send(&HttpRequest {
                method: Method::Post,
                url: self.endpoint(),
                body: Some(serde_json::to_vec(&body).expect("a JSON value serializes")),
                headers: BTreeMap::from([
                    ("content-type".to_string(), "application/json".to_string()),
                    ("x-api-key".to_string(), key),
                    ("anthropic-version".to_string(), "2023-06-01".to_string()),
                    (
                        "user-agent".to_string(),
                        crate::fetch::USER_AGENT.to_string(),
                    ),
                ]),
                // A search answer is small; a provider sending more than this
                // is not sending an answer.
                max_bytes: 2 * 1024 * 1024,
                timeout: self.config.timeout,
            })
            .await?;

        let text = String::from_utf8_lossy(&response.body).into_owned();
        if !(200..300).contains(&response.status) {
            return Err(WebFault::Provider(format!(
                "the DeepSeek search API answered {}: {}",
                response.status,
                message_in(&text)
            )));
        }
        let answer: Value = serde_json::from_str(&text).map_err(|source| {
            WebFault::Provider(format!(
                "the DeepSeek search answer does not parse: {source}"
            ))
        })?;
        map_answer(&answer)
    }
}

/// Turn one Messages answer into a search result.
pub fn map_answer(answer: &Value) -> Result<SearchAnswer, WebFault> {
    let blocks = answer
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            WebFault::Provider("the DeepSeek search answer carries no content".to_string())
        })?;

    // Every citation the prose made, by URL: the quoted text is the best
    // snippet available, and it is the one the model actually read.
    let mut snippets: BTreeMap<String, String> = BTreeMap::new();
    for block in blocks {
        let Some(citations) = block.get("citations").and_then(Value::as_array) else {
            continue;
        };
        for citation in citations {
            let (Some(url), Some(quoted)) = (
                citation.get("url").and_then(Value::as_str),
                citation.get("cited_text").and_then(Value::as_str),
            ) else {
                continue;
            };
            // First one wins: the same page cited twice was read once.
            snippets
                .entry(url.to_string())
                .or_insert_with(|| quoted.to_string());
        }
    }

    let mut sources: Vec<Source> = Vec::new();
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut found_results = false;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("web_search_tool_result") {
            continue;
        }
        found_results = true;
        let items = block
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in items {
            let Some(url) = item
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.trim().is_empty())
            else {
                continue;
            };
            if seen.insert(url.to_string(), ()).is_some() {
                continue;
            }
            sources.push(Source {
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(url)
                    .to_string(),
                url: url.to_string(),
                snippet: snippets.get(url).cloned(),
                published: item
                    .get("page_age")
                    .and_then(Value::as_str)
                    .filter(|age| !age.trim().is_empty())
                    .map(str::to_string),
            });
        }
    }

    if !found_results {
        return Err(WebFault::Provider(
            "the DeepSeek search answer carries no search results, only prose: a citation nobody \
             can check is worse than no answer"
                .to_string(),
        ));
    }

    let prose: String = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<&str>>()
        .join("\n")
        .trim()
        .to_string();

    Ok(SearchAnswer {
        answer: (!prose.is_empty()).then_some(prose),
        sources,
        truncated: false,
    })
}

/// The provider's own words in an error body, whatever shape it took.
fn message_in(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        // Not JSON at all: an HTML error page or a proxy's plain text. The
        // status line above already said what happened.
        return first_line(body);
    };
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .or_else(|| value.get("error").and_then(Value::as_str))
        .map_or_else(|| first_line(body), str::to_string)
}

fn first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(200) {
        None => line.to_string(),
        Some((at, _)) => format!("{}...", &line[..at]),
    }
}
