//! Searching: the seam, the providers registered on it, and the rule for
//! choosing between them.
//!
//! **A provider says whether it is usable, and the runtime never guesses.** A
//! provider with no credential is registered and unusable, which is a
//! different thing from not being there: the first is a deployment that meant
//! to have search and has not finished configuring it, and the message should
//! say so.
//!
//! **Two usable providers and no choice made is a refusal.** Picking by
//! registration order would make the same query answered by a different engine
//! depending on which plugin loaded first, which is the kind of thing nobody
//! can debug from a journal.
//!
//! **The result cap is the runtime's, not the provider's.** A provider that
//! over-returns is truncated here and the answer says it was, so a model is
//! never told that ten sources are all of them when the cap cut thirty.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::fault::WebFault;

/// The capability these providers serve. Named, because the fetch seam is the
/// other one and the messages have to say which is missing.
pub const CAPABILITY: &str = "search";

/// One query, as a provider takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    /// The most results the caller wants. A provider may ignore it; the
    /// runtime enforces it either way.
    pub max_results: Option<usize>,
}

/// One thing a search found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// However the provider stated the age of the page, unparsed: a date this
    /// crate re-formatted would be a date it could get wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

/// What a search produced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchAnswer {
    /// The provider's own prose answer, when it wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub sources: Vec<Source>,
    /// Whether the result cap cut anything.
    pub truncated: bool,
}

/// Whether a provider can be asked anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Usable,
    /// Registered, and cannot serve: no credential, a base URL that is not a
    /// URL, a limit that is not a number.
    Unusable(String),
}

impl Availability {
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Usable)
    }

    pub fn why(&self) -> String {
        match self {
            Self::Usable => String::new(),
            Self::Unusable(why) => why.clone(),
        }
    }
}

#[async_trait::async_trait]
pub trait SearchProvider: Send + Sync {
    /// The name a deployment configures this provider by.
    fn id(&self) -> &str;

    /// Whether it can serve right now. The default is yes: a provider with
    /// nothing to be missing does not have to say so.
    fn availability(&self) -> Availability {
        Availability::Usable
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchAnswer, WebFault>;
}

/// The registered providers, and the rule for choosing between them.
#[derive(Default)]
pub struct WebRuntime {
    providers: Vec<Arc<dyn SearchProvider>>,
    /// The provider a deployment named, if it named one.
    configured: Option<String>,
    /// The most results any search returns, whatever a provider sends.
    max_results: Option<usize>,
}

impl WebRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider. A second one under the same id is refused: the
    /// configuration names providers by id, and two of them would make that
    /// name mean whichever loaded last.
    pub fn register(&mut self, provider: Arc<dyn SearchProvider>) -> Result<(), WebFault> {
        if self.providers.iter().any(|held| held.id() == provider.id()) {
            return Err(WebFault::DuplicateProvider {
                capability: CAPABILITY.to_string(),
                id: provider.id().to_string(),
            });
        }
        self.providers.push(provider);
        Ok(())
    }

    /// Builder form, for a composer that is assembling a runtime in one
    /// expression. Panics on a duplicate id, which is a wiring mistake rather
    /// than a run-time condition.
    pub fn with(mut self, provider: Arc<dyn SearchProvider>) -> Self {
        self.register(provider).expect("a duplicate provider id");
        self
    }

    /// Name the provider every search uses.
    pub fn configure(mut self, id: Option<String>) -> Self {
        self.configured = id;
        self
    }

    /// Cap how many sources a search answers with.
    pub fn cap(mut self, max_results: Option<usize>) -> Self {
        self.max_results = max_results;
        self
    }

    /// The ids registered, in registration order.
    pub fn registered(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|provider| provider.id().to_string())
            .collect()
    }

    /// Which provider a search would run on, or why none would.
    pub fn resolve(&self) -> Result<Arc<dyn SearchProvider>, WebFault> {
        if let Some(id) = &self.configured {
            let named = self
                .providers
                .iter()
                .find(|provider| provider.id() == id)
                .ok_or_else(|| WebFault::ConfiguredMissing {
                    capability: CAPABILITY.to_string(),
                    id: id.clone(),
                    registered: self.registered(),
                })?;
            return match named.availability() {
                Availability::Usable => Ok(Arc::clone(named)),
                Availability::Unusable(why) => Err(WebFault::ConfiguredUnavailable {
                    capability: CAPABILITY.to_string(),
                    id: id.clone(),
                    why,
                }),
            };
        }

        let usable: Vec<&Arc<dyn SearchProvider>> = self
            .providers
            .iter()
            .filter(|provider| provider.availability().is_usable())
            .collect();
        match usable.len() {
            0 => Err(WebFault::ProviderUnavailable {
                capability: CAPABILITY.to_string(),
            }),
            1 => Ok(Arc::clone(usable[0])),
            // Sorted, so the message is the same whichever order the providers
            // were registered in - the very thing the refusal is about.
            _ => Err(WebFault::ProviderAmbiguous {
                capability: CAPABILITY.to_string(),
                candidates: usable
                    .iter()
                    .map(|provider| provider.id().to_string())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect(),
            }),
        }
    }

    /// Run one search on the resolved provider, under the runtime's cap.
    pub async fn search(&self, text: &str) -> Result<SearchAnswer, WebFault> {
        let text = text.trim();
        if text.is_empty() {
            return Err(WebFault::InvalidArguments(
                "a search needs a query with something in it".to_string(),
            ));
        }
        let provider = self.resolve()?;
        let mut answer = provider
            .search(&SearchQuery {
                text: text.to_string(),
                max_results: self.max_results,
            })
            .await?;
        if let Some(cap) = self.max_results {
            if answer.sources.len() > cap {
                answer.sources.truncate(cap);
                answer.truncated = true;
            }
        }
        Ok(answer)
    }
}
