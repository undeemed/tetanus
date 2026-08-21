//! `web_fetch` and `web_search`, as the model sees them.
//!
//! **Both are parallel-safe.** They read; they change nothing out there and
//! nothing here, so two of them in one step may overlap. That is the opt-in
//! `ToolMode` asks for, made deliberately and for stated arguments rather than
//! by silence.
//!
//! **A failure is a failed result with its code in it.** Everything a fetch or
//! a search can refuse is a [`WebFault`] with an upstream code, and the tool's
//! text leads with that code, so a journal can be searched for
//! `WEB_FETCH_TOO_LARGE` and the model reads a bounded sentence instead of a
//! stack of prose.
//!
//! **Truncation is stated in the text the model reads.** A page cut at the cap
//! that did not say so would be a page the model answers about as if it had
//! read the end.
//!
//! **A search result carries its sources, and says to cite them.** That last
//! line is upstream's, and it is there for the reason upstream has it: a model
//! given search results without one tends to state them as its own knowledge.

use std::sync::Arc;

use serde_json::Value;
use tetanus_turn::tools::{Tool, ToolError, ToolMode, ToolOutcome, ToolSchema};

use crate::fault::WebFault;
use crate::fetch::{fetch, FetchLimits, Fetched};
use crate::http::HttpTransport;
use crate::search::{SearchAnswer, WebRuntime};

/// Fetch one page and hand the model its text.
pub struct WebFetchTool {
    transport: Arc<dyn HttpTransport>,
    limits: FetchLimits,
    /// The most characters the rendered result may run to, cap and framing
    /// together. Separate from [`FetchLimits::max_chars`] because one bounds
    /// the page and the other bounds what a step spends on it.
    max_output: usize,
}

impl WebFetchTool {
    pub const NAME: &'static str = "web_fetch";

    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport,
            limits: FetchLimits::default(),
            max_output: 40_000,
        }
    }

    pub fn limits(mut self, limits: FetchLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn max_output(mut self, max_output: usize) -> Self {
        self.max_output = max_output;
        self
    }
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.to_string(),
            description: "Fetch a web page or a JSON document over http or https and return its \
                          text. Follows same-origin redirects; refuses anything that is not text, \
                          HTML, markdown or JSON."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The absolute http or https URL to fetch.",
                    },
                },
                "required": ["url"],
            }),
        }
    }

    /// Reading a page changes nothing, so any number of fetches may overlap.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let url = text_argument(arguments, "url").map_err(|fault| failed(Self::NAME, &fault))?;
        match fetch(self.transport.as_ref(), &url, self.limits).await {
            Ok(fetched) => Ok(ToolOutcome::ok(render_fetch(&fetched, self.max_output))),
            Err(fault) => Err(failed(Self::NAME, &fault)),
        }
    }
}

/// Search the web through whichever provider the runtime resolves.
pub struct WebSearchTool {
    runtime: Arc<WebRuntime>,
}

impl WebSearchTool {
    pub const NAME: &'static str = "web_search";

    pub fn new(runtime: Arc<WebRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.to_string(),
            description: "Search the web and return the pages found, with their URLs, so they can \
                          be cited or fetched."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to search for.",
                    },
                },
                "required": ["query"],
            }),
        }
    }

    /// A search reads and changes nothing, so searches may overlap.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let query =
            text_argument(arguments, "query").map_err(|fault| failed(Self::NAME, &fault))?;
        match self.runtime.search(&query).await {
            Ok(answer) => Ok(ToolOutcome::ok(render_search(&query, &answer))),
            Err(fault) => Err(failed(Self::NAME, &fault)),
        }
    }
}

/// A required string argument, or the refusal that names it.
fn text_argument(arguments: &Value, name: &str) -> Result<String, WebFault> {
    match arguments.get(name).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        Some(_) => Err(WebFault::InvalidArguments(format!(
            "{name} was given, and it is empty"
        ))),
        None => Err(WebFault::InvalidArguments(format!("{name} is required"))),
    }
}

fn failed(tool: &str, fault: &WebFault) -> ToolError {
    ToolError::Failed(tool.to_string(), format!("[{}] {fault}", fault.code()))
}

/// One fetched page, as the model reads it.
pub fn render_fetch(fetched: &Fetched, max_output: usize) -> String {
    let mut out = format!(
        "{} ({} {})\n\n",
        fetched.final_url, fetched.status, fetched.media_type
    );
    out.push_str(&fetched.text);

    let cut_here = match out.char_indices().nth(max_output) {
        None => false,
        Some((at, _)) => {
            out.truncate(at);
            true
        }
    };
    if fetched.truncated || cut_here {
        out.push_str(
            "\n\n[the page was longer than this tool returns; this is the beginning of it]",
        );
    }
    out
}

/// One search, as the model reads it.
pub fn render_search(query: &str, answer: &SearchAnswer) -> String {
    if answer.answer.is_none() && answer.sources.is_empty() {
        return format!("No results for {query:?}.");
    }
    let mut out = String::new();
    if let Some(prose) = &answer.answer {
        out.push_str(prose);
        out.push_str("\n\n");
    }
    if !answer.sources.is_empty() {
        out.push_str("Sources:\n");
        for (index, source) in answer.sources.iter().enumerate() {
            out.push_str(&format!(
                "[{}] {} - {}\n    {}\n",
                index + 1,
                source.title,
                hostname(&source.url),
                source.url,
            ));
            if let Some(snippet) = &source.snippet {
                out.push_str(&format!("    {}\n", one_line(snippet)));
            }
        }
    }
    if answer.truncated {
        out.push_str("\n[more results were found than this tool returns]\n");
    }
    out.push_str("\nCite the sources you use by their URL.");
    out
}

/// The host a URL names, for a reader scanning a list. A URL that will not
/// parse is shown whole rather than dropped - upstream's "falls back to the
/// raw URL as a source label".
fn hostname(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}
