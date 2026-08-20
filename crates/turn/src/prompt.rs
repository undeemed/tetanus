//! The named prompt-section registry: what the engine assembles before it asks
//! the model anything.
//!
//! Upstream keeps this registry on its `systemPrompt` service. A section has a
//! unique name, an explicit order, and text that is either fixed or produced
//! for each assembly, and one section may declare itself the whole prompt.
//! Section text may name prompt variables, which [`interpolate`] substitutes.
//! tetanus keeps that shape. What upstream also keeps there and tetanus has no
//! surface for - scopes, the variable registry itself and runtime-context
//! providers - stays a row in `docs/parity.md`.
//!
//! The registry is the assembly's input, not its decision. It produces the
//! ordered sections the engine hands to the `system-prompt/assemble`
//! waterfall, which is still what has the last word.

use std::collections::BTreeMap;
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

/// How a prompt variable's name is written, both where it is registered and
/// where a section names it between braces.
pub const VARIABLE_NAME: &str = "^[a-z][a-z0-9_]*$";

/// What one assembly knows about its variables: every registered name, and the
/// value it has for this assembly, if it has one.
///
/// A name that is absent is not registered at all, and a section that names it
/// is a mistake; a name present with no value is registered but has nothing to
/// say this time, which is a different mistake. [`interpolate`] refuses both,
/// in those words.
pub type Variables = BTreeMap<String, Option<String>>;

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("prompt section \"{0}\" is already registered")]
    Duplicate(String),
    #[error("prompt section \"{0}\" is the engine's own slot, filled from the turn config")]
    Reserved(String),
    #[error("prompt section \"{refused}\" cannot be the whole prompt: \"{held}\" already is")]
    Complete { held: String, refused: String },
    /// The text opened a reference that never became one complete group, and a
    /// later `}}` says the author meant it as a reference.
    #[error("malformed prompt variable reference at {at:?} in section {section:?} (references are complete simple {{{{name}}}} groups)")]
    MalformedReference { section: String, at: String },
    /// A complete group whose name no reference could carry.
    #[error("malformed prompt variable reference {:?} in section {section:?} (variable names match {VARIABLE_NAME})", reference(.name))]
    BadReference { section: String, name: String },
    #[error("unknown prompt variable {:?} in section {section:?}; registered variables: {}", reference(.name), listed(.registered))]
    UnknownVariable {
        section: String,
        name: String,
        registered: Vec<String>,
    },
    #[error("prompt variable {:?} has no value for this assembly (section {section:?})", reference(.name))]
    NoValue { section: String, name: String },
}

/// A variable's name as a section writes it.
fn reference(name: &str) -> String {
    ["{{", name, "}}"].concat()
}

fn listed(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
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
    /// Set by [`Section::complete`]. Private, so the only way to claim the
    /// whole prompt is to say so in words at the construction site.
    complete: bool,
}

impl Section {
    pub fn new(id: impl Into<String>, order: i32, text: impl Into<SectionText>) -> Self {
        Self {
            id: id.into(),
            order,
            text: text.into(),
            complete: false,
        }
    }

    /// This section is the whole prompt: what the model reads is its text and
    /// nothing else.
    ///
    /// The assembly still runs in full, so tool schemas and every other
    /// contribution still resolve and every listener still sees them; the
    /// engine restores this section as the sole prompt section afterwards.
    /// A registry holds one such section at a time, so the second registration
    /// is refused rather than silently shadowing the first.
    pub fn complete(mut self) -> Self {
        self.complete = true;
        self
    }
}

struct Entry {
    seq: u64,
    id: String,
    order: i32,
    text: SectionText,
    complete: bool,
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
        if section.complete {
            let held = self.complete_id();
            if let Some(held) = held {
                return Err(PromptError::Complete {
                    held,
                    refused: section.id,
                });
            }
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

    /// The name of the section that is the whole prompt, if one is registered.
    ///
    /// The engine asks before it assembles, and keeps that section aside: a
    /// complete prompt is restored after `system-prompt/assemble` has run, so
    /// a listener sees the whole assembly but cannot edit what the model
    /// finally reads.
    pub fn complete_id(&self) -> Option<String> {
        let inner = self.inner.lock().expect("prompt registry");
        inner
            .entries
            .iter()
            .find(|e| e.complete)
            .map(|e| e.id.clone())
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
            complete: section.complete,
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

fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Substitute one section's `{{name}}` references from this assembly's
/// variables.
///
/// Strict, in three ways that are all mistakes worth a failed turn rather than
/// prose the model would have read as an instruction: a reference to a name
/// nothing registered, a reference to a registered name with no value this
/// time, and text that opened a reference it never closed properly.
///
/// The one thing that is not a mistake: a lone `{{` with no `}}` after it
/// anywhere is prose, and stays exactly as written - a shell default like
/// `${X:-{{fallback}` is not a prompt variable. A substituted value is never
/// scanned again either, so a value that itself contains braces is text, not a
/// second reference.
///
/// `section` names the section the text came from, so a message says where to
/// go and fix it.
pub fn interpolate(
    text: &str,
    section: &str,
    variables: &Variables,
) -> Result<String, PromptError> {
    let mut out = String::new();
    let mut last = 0;
    while let Some(offset) = text[last..].find("{{") {
        let open = last + offset;
        let Some(name) = group_at(&text[open..]) else {
            // A later closing brace says a reference was meant; without one
            // the braces are prose.
            if text[open + 2..].contains("}}") {
                return Err(PromptError::MalformedReference {
                    section: section.to_string(),
                    at: head(&text[open..]),
                });
            }
            out.push_str(&text[last..open + 2]);
            last = open + 2;
            continue;
        };
        if !is_variable_name(name) {
            return Err(PromptError::BadReference {
                section: section.to_string(),
                name: name.to_string(),
            });
        }
        let value = match variables.get(name) {
            None => {
                return Err(PromptError::UnknownVariable {
                    section: section.to_string(),
                    name: name.to_string(),
                    registered: variables.keys().cloned().collect(),
                })
            }
            Some(None) => {
                return Err(PromptError::NoValue {
                    section: section.to_string(),
                    name: name.to_string(),
                })
            }
            Some(Some(value)) => value,
        };
        out.push_str(&text[last..open]);
        out.push_str(value);
        last = open + name.len() + 4;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// The name in the complete `{{name}}` group this text opens with, if it opens
/// with one. A brace anywhere inside is not part of a group.
fn group_at(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("{{")?;
    let end = rest.find(['{', '}'])?;
    rest[end..].starts_with("}}").then(|| &rest[..end])
}

/// As much of a malformed reference as a message needs to point at it.
fn head(text: &str) -> String {
    let mut out: String = text.chars().take(16).collect();
    out.push('\u{2026}');
    out
}
