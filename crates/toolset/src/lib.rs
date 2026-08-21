//! The one place that says which tools this build offers.
//!
//! **Why this exists before the tools do.** Five lanes are building tool
//! crates - a filesystem service, a command runner, the feature tools, an MCP
//! client, web fetch - and none of them can compose itself: the registry the
//! binary uses lives in `crates/cli`, which the presentation lane owns, and the
//! one the engine defaults to lives in `crates/engine`. Today those are two
//! separate expressions that both happen to say `EchoTool`, which is already a
//! drift waiting to happen and becomes a certainty at five crates. So the
//! assembly moves here first, shaped so that a landed crate is one line, and
//! the line is written down before the crate arrives rather than negotiated
//! after it.
//!
//! **A tool comes from a source, and the source is named.** Grouping is not
//! decoration: it is what lets a deployment say `tools.sources: [fs]` instead
//! of naming fifteen tools, what lets a duplicate be reported as "these two
//! crates both offer `read`" rather than one silently winning, and what lets
//! `tetanus tools` say where a tool came from when a user asks why it is there.
//!
//! **A duplicate name is refused, not overwritten.**
//! [`tetanus_turn::tools::ToolRegistry::register`] keys by name and the last
//! registration wins, which is right for a registry and wrong for an assembly:
//! when two crates offer `read`, one of them is being silently dropped, and the
//! model is offered a schema belonging to a tool that is not the one that runs.
//! That failure is invisible today because there is one source, and inevitable
//! the day there are five.
//!
//! **What a deployment enables, it enables by source.** An absent
//! `tools.sources` is every source this build ships; a list names exactly the
//! sources to use, and naming none is a harness with no tools - which is a
//! legitimate thing to want and a strange thing to arrive at by accident, so it
//! has to be written explicitly.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tetanus_config::{Config, ConfigError};
use tetanus_turn::tools::{EchoTool, Tool, ToolRegistry};

/// The settings key a deployment names sources with.
pub mod key {
    /// Which sources to compose. Absent is all of them.
    pub const SOURCES: &str = "tools.sources";
}

/// One crate's worth of tools, under the name a deployment uses for it.
///
/// `Debug` reports the names rather than the tools: a tool is a trait object
/// with no useful rendering, and what a reader of a failed assembly needs is
/// which source held what.
pub struct Source {
    /// What a deployment writes in `tools.sources`, and what a duplicate
    /// report names. Stable: renaming one breaks a document that named it.
    pub name: &'static str,
    /// One line for `tetanus tools` and for the note that explains why a tool
    /// is on offer.
    pub description: &'static str,
    pub tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("name", &self.name)
            .field("tools", &self.tool_names())
            .finish()
    }
}

impl std::fmt::Debug for Assembly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.sources.iter()).finish()
    }
}

impl Source {
    pub fn new(name: &'static str, description: &'static str, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            name,
            description,
            tools,
        }
    }

    /// The names this source offers, in the order the registry will hold them.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.iter().map(|tool| tool.schema().name).collect();
        names.sort();
        names
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssemblyError {
    /// Two sources offer one name. Refused rather than resolved, because
    /// either answer is wrong: the model would be offered one tool's schema
    /// and run the other's body, and nothing would say so.
    #[error(
        "the tool {name:?} is offered by both {first:?} and {second:?}. Two tools cannot share a \
         name: the model is offered one schema and would run the other body. Rename one, or \
         compose only one of the two sources"
    )]
    Duplicate {
        name: String,
        first: &'static str,
        second: &'static str,
    },
    #[error("no tool source is named {name:?}; this build ships {available}")]
    UnknownSource { name: String, available: String },
    #[error("{key}: must be a list of source names", key = key::SOURCES)]
    BadSetting,
}

/// Every source this build ships, in the order a reader meets them.
///
/// **This function is the registration surface.** A tool crate that lands adds
/// exactly one entry here and changes nothing else in the workspace; the binary
/// and the engine both read this, so a tool added here is a tool the model can
/// call and a tool `tetanus tools` lists, and those cannot disagree.
/// `docs/parity-updates/` names the one line each pending crate will add.
pub fn sources() -> Vec<Source> {
    vec![Source::new(
        "builtin",
        "The tools the engine ships with, which need no capability beyond the turn itself.",
        vec![Arc::new(EchoTool)],
    )]
    // Each landed crate appends one line here. The pending ones, with the exact
    // line each will add, are listed in `docs/parity-updates/toolset.md`.
}

/// The sources this build ships, composed and checked.
pub struct Assembly {
    sources: Vec<Source>,
}

impl Default for Assembly {
    fn default() -> Self {
        Self::stock()
    }
}

impl Assembly {
    /// Everything this build ships.
    pub fn stock() -> Self {
        Self { sources: sources() }
    }

    /// An assembly of exactly the sources given, for a composer that is not
    /// taking the shipped set - a test, or an embedder with its own tools.
    pub fn of(sources: Vec<Source>) -> Self {
        Self { sources }
    }

    /// Add one source.
    pub fn with(mut self, source: Source) -> Self {
        self.sources.push(source);
        self
    }

    /// Keep only the named sources, in the order this build declares them
    /// rather than the order they were named.
    ///
    /// The declared order is what makes an assembly reproducible: two
    /// deployments naming the same sources in different orders get the same
    /// registry, and a document is not a place to express precedence that
    /// nothing reads. `tools.order` is where the order the model sees is set.
    pub fn only<I, S>(mut self, names: I) -> Result<Self, AssemblyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let wanted: Vec<String> = names.into_iter().map(|n| n.as_ref().to_string()).collect();
        for name in &wanted {
            if !self.sources.iter().any(|source| source.name == name) {
                return Err(AssemblyError::UnknownSource {
                    name: name.clone(),
                    available: self.names().join(", "),
                });
            }
        }
        self.sources
            .retain(|source| wanted.iter().any(|name| name == source.name));
        Ok(self)
    }

    /// Apply `tools.sources` from a settings document.
    ///
    /// An absent key leaves every source composed; a list keeps exactly what it
    /// names, and an empty list keeps none. A deployment that wants a harness
    /// with no tools has to write that down, which is the difference between
    /// choosing it and arriving at it.
    pub fn configured(self, settings: &Config) -> Result<Self, ConfigError> {
        let Some(resolved) = settings.get(key::SOURCES) else {
            return Ok(self);
        };
        let Value::Array(items) = &resolved.value else {
            return Err(bad(&resolved.value));
        };
        let mut names = Vec::with_capacity(items.len());
        for item in items {
            match item.as_str() {
                Some(name) => names.push(name.to_string()),
                None => return Err(bad(&resolved.value)),
            }
        }
        self.only(names).map_err(|error| ConfigError::BadValue {
            key: key::SOURCES.to_string(),
            expected: "a list of source names this build ships".to_string(),
            found: error.to_string(),
        })
    }

    /// The source names, in declaration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.sources.iter().map(|source| source.name).collect()
    }

    /// What each source contributes: its name, its line, and its tools.
    ///
    /// Published because "why is this tool on offer" is a question a user asks
    /// of a harness with forty of them, and the answer has to come from the
    /// same place the tools do.
    pub fn roster(&self) -> Vec<(&'static str, &'static str, Vec<String>)> {
        self.sources
            .iter()
            .map(|source| (source.name, source.description, source.tool_names()))
            .collect()
    }

    /// Which source offers a given tool, if any.
    pub fn source_of(&self, tool: &str) -> Option<&'static str> {
        self.sources
            .iter()
            .find(|source| source.tool_names().iter().any(|name| name == tool))
            .map(|source| source.name)
    }

    /// Compose the registry, refusing a name two sources share.
    ///
    /// The check is here rather than in the registry because the registry is
    /// right to key by name - one name, one tool - and what is wrong is a
    /// *composition* that produced two. Refusing at the seam that built it
    /// names both crates, which is the only form of the message anybody can
    /// act on.
    pub fn build(self) -> Result<ToolRegistry, AssemblyError> {
        let mut owner: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut registry = ToolRegistry::new();
        for source in &self.sources {
            for tool in &source.tools {
                let name = tool.schema().name;
                if let Some(first) = owner.get(&name) {
                    return Err(AssemblyError::Duplicate {
                        name,
                        first,
                        second: source.name,
                    });
                }
                owner.insert(name, source.name);
                registry.register(Arc::clone(tool));
            }
        }
        Ok(registry)
    }
}

fn bad(found: &Value) -> ConfigError {
    ConfigError::BadValue {
        key: key::SOURCES.to_string(),
        expected: "a list of source names".to_string(),
        found: found.to_string(),
    }
}

/// The registry this build offers, with nothing configured.
///
/// What `crates/cli` and `crates/engine` both call, so what the binary lists
/// and what a turn can dispatch are one thing rather than two expressions that
/// agree today.
///
/// It cannot fail: the shipped sources are this build's own, so a duplicate
/// among them is a mistake in [`sources`] rather than anything a deployment
/// did, and it is caught by TC-TOOLSET-1 before it can ship.
pub fn stock_registry() -> ToolRegistry {
    Assembly::stock()
        .build()
        .expect("the shipped sources are this build's own; TC-TOOLSET-1 holds them unique")
}
