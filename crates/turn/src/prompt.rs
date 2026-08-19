//! The named prompt-section registry: what the engine assembles before it asks
//! the model anything.
//!
//! Upstream keeps this registry on its `systemPrompt` service. A section has a
//! unique name, an explicit order, and text that is either fixed or produced
//! for each assembly. tetanus keeps that shape. What upstream also keeps there
//! and tetanus has no surface for - scopes, prompt variables, runtime-context
//! providers, and a "complete" section that replaces the assembly - stays a
//! row in `docs/parity.md`.
//!
//! The registry is the assembly's input, not its decision. It produces the
//! ordered sections the engine hands to the `system-prompt/assemble`
//! waterfall, which is still what has the last word.

use std::sync::{Arc, Mutex, Weak};

use tetanus_core::EffectHandle;

use crate::events::PromptSection;

/// The engine's own opening section. It is filled from
/// [`TurnConfig::base_prompt`](crate::TurnConfig::base_prompt), so
/// [`PromptRegistry::section`] refuses the name: a plugin that wants to speak
/// before the base registers a smaller order instead of fighting for the slot.
pub const BASE_SECTION: &str = "base";

/// Where the base section sits. Upstream puts its harness identity at the same
/// order, and its deployment persona at `0`.
pub const BASE_ORDER: i32 = -100;

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("prompt section \"{0}\" is already registered")]
    Duplicate(String),
    #[error("prompt section \"{0}\" is the engine's own slot, filled from the turn config")]
    Reserved(String),
}

/// What one assembly tells a section provider about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssembleAt {
    pub turn: u64,
    pub step: u32,
}

type Provider = Arc<dyn Fn(&AssembleAt) -> String + Send + Sync>;

/// A section's text: settled once, or asked for at every assembly.
#[derive(Clone)]
pub enum SectionText {
    Fixed(String),
    Provided(Provider),
}

impl SectionText {
    pub fn provided(text: impl Fn(&AssembleAt) -> String + Send + Sync + 'static) -> Self {
        Self::Provided(Arc::new(text))
    }

    fn resolve(&self, at: &AssembleAt) -> String {
        match self {
            Self::Fixed(text) => text.clone(),
            Self::Provided(provider) => provider(at),
        }
    }
}

impl From<String> for SectionText {
    fn from(text: String) -> Self {
        Self::Fixed(text)
    }
}

impl From<&str> for SectionText {
    fn from(text: &str) -> Self {
        Self::Fixed(text.to_string())
    }
}

/// One contribution to the system prompt.
pub struct Section {
    /// Unique among registered sections.
    pub id: String,
    /// Sections render in ascending order. Ties keep their registration order,
    /// so a plugin that contributes two sections at one order still reads in
    /// the order it wrote them.
    pub order: i32,
    pub text: SectionText,
}

impl Section {
    pub fn new(id: impl Into<String>, order: i32, text: impl Into<SectionText>) -> Self {
        Self {
            id: id.into(),
            order,
            text: text.into(),
        }
    }
}

struct Entry {
    seq: u64,
    id: String,
    order: i32,
    text: SectionText,
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    next: u64,
}

/// The registry itself. One per booted context, provided under the
/// `system-prompt` service key.
#[derive(Default)]
pub struct PromptRegistry {
    inner: Mutex<Inner>,
}

impl PromptRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a section. Dropping the returned handle unregisters it, so a
    /// plugin's contribution dies with the plugin's context.
    pub fn section(self: &Arc<Self>, section: Section) -> Result<EffectHandle, PromptError> {
        if section.id == BASE_SECTION {
            return Err(PromptError::Reserved(section.id));
        }
        let id = section.id.clone();
        self.insert(section).ok_or(PromptError::Duplicate(id))
    }

    /// Fill the engine's own slot. Private to the crate because the engine
    /// owns it: it cannot collide, because [`section`](Self::section) refuses
    /// the name.
    pub(crate) fn seed_base(self: &Arc<Self>, text: String) -> EffectHandle {
        self.insert(Section::new(BASE_SECTION, BASE_ORDER, text))
            .expect("the base slot is reserved, so it cannot be taken")
    }

    /// The registered sections, in render order, with every provider asked.
    ///
    /// Providers run with the lock released, so a provider that reads the
    /// registry back cannot deadlock the assembly that called it.
    pub fn assemble(&self, at: &AssembleAt) -> Vec<PromptSection> {
        let mut snapshot: Vec<(i32, u64, String, SectionText)> = {
            let inner = self.inner.lock().expect("prompt registry");
            inner
                .entries
                .iter()
                .map(|e| (e.order, e.seq, e.id.clone(), e.text.clone()))
                .collect()
        };
        snapshot.sort_by_key(|(order, seq, _, _)| (*order, *seq));
        snapshot
            .into_iter()
            .map(|(_, _, id, text)| PromptSection {
                id,
                text: text.resolve(at),
            })
            .collect()
    }

    fn insert(self: &Arc<Self>, section: Section) -> Option<EffectHandle> {
        let mut inner = self.inner.lock().expect("prompt registry");
        if inner.entries.iter().any(|e| e.id == section.id) {
            return None;
        }
        let seq = inner.next;
        inner.next += 1;
        inner.entries.push(Entry {
            seq,
            id: section.id,
            order: section.order,
            text: section.text,
        });
        drop(inner);

        let owner = Arc::downgrade(self);
        Some(EffectHandle::new(move || remove(&owner, seq)))
    }
}

fn remove(owner: &Weak<PromptRegistry>, seq: u64) {
    if let Some(registry) = owner.upgrade() {
        let mut inner = registry.inner.lock().expect("prompt registry");
        inner.entries.retain(|e| e.seq != seq);
    }
}
