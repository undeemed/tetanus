//! The two tools that leave the machine: fetching a page, and searching.
//!
//! Both are built the same way, and the shape is the point:
//!
//! **The network is a seam, and the suite never crosses it.** Everything a
//! fetch decides - the scheme, the redirect, the size, the content type, the
//! charset - is decided above [`http::HttpTransport`], so every rule can be
//! asserted against a scripted transport with no socket in it. The live
//! transport is thin on purpose: what it does is send bytes, and what it must
//! not do is decide anything.
//!
//! **A search provider is a trait, and the model never learns which one
//! answered.** [`search::SearchProvider`] is the seam; a deployment registers
//! one or names one, and [`search::WebRuntime`] resolves. Two usable providers
//! and no choice made is a refusal rather than a coin toss, because a search
//! answered by a different engine each time is a harness nobody can debug.
//!
//! **A limit is refused before it is exceeded where that is possible, and
//! after it where it is not.** A declared `Content-Length` past the cap is
//! refused without reading a byte of body; a server that declares nothing is
//! read up to the cap and cut, and says it was cut. Truncation is never
//! silent: a model that reads half a page and is not told is a model that
//! answers confidently about the half it did not get.
//!
//! Parity: upstream `packages/web/*`. Its HTML-to-markdown conversion
//! (turndown), its spill files, its presentation metadata and its Anthropic
//! and Exa and Perplexity providers are named in `docs/parity.md`; what is
//! restated here is the fetch policy, the provider seam, one provider over it,
//! and the two model-facing tools.

pub mod fault;
pub mod fetch;
pub mod http;
pub mod live;
pub mod mock;
pub mod provider;
pub mod render;
pub mod search;
pub mod tools;

pub use fault::WebFault;
pub use fetch::{fetch, FetchLimits, Fetched, MediaKind};
pub use http::{HttpRequest, HttpResponse, HttpTransport};
pub use live::LiveHttp;
pub use provider::{DeepSeekSearch, DeepSeekSearchConfig};
pub use search::{Availability, SearchAnswer, SearchProvider, SearchQuery, Source, WebRuntime};
pub use tools::{WebFetchTool, WebSearchTool};
