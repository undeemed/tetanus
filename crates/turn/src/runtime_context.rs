//! What a turn tells the model about the world outside the conversation.
//!
//! Today's date, the working directory, the branch, whether the sandbox is on.
//! None of it is in the conversation and none of it is stable, so it is
//! gathered once per turn and written to the journal as `context/snapshot`.
//!
//! Contract section 4.4.8 settled the shape before this was built, and every
//! rule below is the contract's rather than this module's.
//!
//! # It is a user message, not part of the system prompt
//!
//! This is the whole design and it is a caching decision. A provider caches a
//! prompt by its longest stable prefix. The system prompt is the same on every
//! turn of a session, so it caches; a sentence saying what time it is changes
//! every turn, and putting it there would invalidate the cached prefix on
//! every request of every session. Carrying it after the retained history
//! leaves the prefix untouched and costs one message.
//!
//! # Only the newest snapshot is history
//!
//! A turn writes one, so a long session accumulates them, and yesterday's date
//! is worse than no date. When history is derived, the last `context/snapshot`
//! becomes a `user` message and every earlier one is skipped. They stay on the
//! journal, because the journal records what happened and a reader may want to
//! know what the model was told at the time; they simply do not travel again.
//!
//! # The parts are the record, not the rendered text
//!
//! `parts` is an ordered list of `name` and `text`, and the message the model
//! reads is the non-empty ones joined with a blank line, in list order - the
//! same rule section 4.3 fixes for prompt sections, because two joining rules
//! would be one too many. Carrying the parts is deliberate: the rendering is
//! reproducible from them, so nothing is lost, and a surface that wants to
//! show which provider said what has it.
//!
//! # A deployment that configures nothing pays nothing
//!
//! A snapshot whose parts are all empty is not written at all. Not an empty
//! array, not a record with no parts - no event. A journal from a deployment
//! with no providers is byte-identical to the journal it had before this
//! existed.
//!
//! Parity: upstream's runtime-context providers, `ctx.runtimeContext`.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// One provider's contribution to a turn's runtime context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPart {
    /// Who contributed it. On the journal so a surface can say which provider
    /// said what, which is the reason the parts are recorded rather than the
    /// rendered text.
    pub name: String,
    /// What it said, already rendered. An empty string is a provider that had
    /// nothing to say this turn, which is not the same as a provider that is
    /// not installed - both are invisible to the model, and only the journal
    /// tells them apart.
    pub text: String,
}

impl ContextPart {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
        }
    }
}

/// Something that tells the model where it is, asked once per turn.
///
/// Synchronous and infallible by design. A provider that needs to fail has
/// nothing useful to say, and the honest expression of that is an empty
/// string: a turn must not be held up, or failed, because the clock could not
/// be read. Anything that has to await belongs in the composition that
/// installs the provider, not in the ask.
pub trait ContextProvider: Send + Sync {
    /// The name recorded on the journal beside this provider's text.
    fn name(&self) -> &str;
    /// What to tell the model this turn, or an empty string for nothing.
    fn text(&self) -> String;
}

/// A provider whose text is fixed at composition time.
///
/// For the parts of the world that do not change while the process runs - the
/// workspace root, the platform - and for tests.
pub struct StaticContext {
    name: String,
    text: String,
}

impl StaticContext {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
        }
    }
}

impl ContextProvider for StaticContext {
    fn name(&self) -> &str {
        &self.name
    }
    fn text(&self) -> String {
        self.text.clone()
    }
}

/// The providers a composition installed, in the order they will be asked.
///
/// Order is registration order and not sorted: a deployment that puts the
/// workspace before the date meant that, and a registry that sorted by name
/// would quietly rewrite the paragraph the model reads.
#[derive(Default)]
pub struct ContextRegistry {
    providers: Mutex<Vec<Arc<dyn ContextProvider>>>,
}

impl ContextRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install one provider at the end of the order.
    pub fn register(&self, provider: Arc<dyn ContextProvider>) {
        self.providers.lock().expect("providers").push(provider);
    }

    /// Convenience for the common case of a fixed sentence.
    pub fn register_static(&self, name: impl Into<String>, text: impl Into<String>) {
        self.register(Arc::new(StaticContext::new(name, text)));
    }

    /// How many providers are installed.
    pub fn len(&self) -> usize {
        self.providers.lock().expect("providers").len()
    }

    /// Whether nothing is installed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Ask every provider, in order, and collect what they said.
    ///
    /// A provider that panics is contained and contributes nothing: it is a
    /// bug in what a composition installed, and failing the turn over the
    /// clock would make an optional decoration able to stop the work. The
    /// panic is logged, because a provider silently contributing nothing
    /// forever is the failure a reader needs told about.
    pub fn gather(&self) -> Vec<ContextPart> {
        let providers = {
            let held = self.providers.lock().expect("providers");
            held.clone()
        };
        let mut parts = Vec::with_capacity(providers.len());
        for provider in providers {
            let name = provider.name().to_owned();
            let text =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| provider.text())) {
                    Ok(text) => text,
                    Err(_) => {
                        tracing::error!(
                            provider = name,
                            "a runtime-context provider panicked; it contributes nothing this turn"
                        );
                        String::new()
                    }
                };
            parts.push(ContextPart { name, text });
        }
        parts
    }
}

/// The message the model reads, from the parts on the journal.
///
/// Non-empty parts joined with a blank line, in list order. `None` when every
/// part is empty, which is the same condition under which no snapshot is
/// written at all - so a reader folding a journal and a writer deciding
/// whether to write one cannot disagree about what "nothing to say" means.
pub fn render(parts: &[ContextPart]) -> Option<String> {
    let rendered: Vec<&str> = parts
        .iter()
        .map(|part| part.text.as_str())
        .filter(|text| !text.is_empty())
        .collect();
    if rendered.is_empty() {
        None
    } else {
        Some(rendered.join("\n\n"))
    }
}
