//! A named group of tools, and the checked composition of several.
//!
//! Nothing here knows which crates exist: `sources` supplies the groups and
//! this decides what composing them means. That is what lets the duplicate
//! rule and the selection rule be asserted against stand-in sources, on a
//! build whose real ones are still arriving.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tetanus_config::{Config, ConfigError};
use tetanus_turn::tools::{Tool, ToolRegistry};

use crate::key;
use crate::Composition;

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

    /// A source from a crate that publishes `register(&mut ToolRegistry)` and
    /// no list.
    ///
    /// Most of them do, because registering is what a composer wanted before
    /// this crate existed. Draining a throwaway registry is what lets a crate
    /// land here without being asked for an accessor first - five lanes, five
    /// pull requests, all of them waiting on each other - and the assembly
    /// still sees each source's names separately, which is what the duplicate
    /// check needs.
    pub fn registered(
        name: &'static str,
        description: &'static str,
        build: impl FnOnce(&mut ToolRegistry),
    ) -> Self {
        let mut registry = ToolRegistry::new();
        build(&mut registry);
        let names: Vec<String> = registry.names().cloned().collect();
        let tools = names
            .iter()
            .filter_map(|name| registry.get(name))
            .collect::<Vec<_>>();
        Self::new(name, description, tools)
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
}

/// The sources this build ships, composed and checked.
pub struct Assembly {
    sources: Vec<Source>,
}

impl Assembly {
    /// Everything this build ships, built against one composition.
    pub fn stock(cx: &Composition) -> Self {
        Self {
            sources: crate::sources(cx),
        }
    }

    /// An assembly of exactly the sources given, for a composer that is not
    /// taking the shipped set - a test, or an embedder with its own tools.
    pub fn of(sources: Vec<Source>) -> Self {
        Self { sources }
    }

    /// Add one source.
    #[must_use]
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
        // Reported as the one name that is wrong, not as the whole list: a
        // reader fixing a document needs the word to change, and "must be a
        // list of names, not <the whole error sentence>" is what a nested
        // message reads like.
        self.only(names).map_err(|error| match error {
            AssemblyError::UnknownSource { name, available } => ConfigError::BadValue {
                key: key::SOURCES.to_string(),
                expected: format!("a source this build ships: {available}"),
                found: name,
            },
            clash => ConfigError::BadValue {
                key: key::SOURCES.to_string(),
                expected: "sources whose tools have distinct names".to_string(),
                found: clash.to_string(),
            },
        })
    }

    /// The source names, in declaration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.sources.iter().map(|source| source.name).collect()
    }

    /// What each source contributes: its name, its line, and its tools.
    ///
    /// Published because "why is this tool on offer" is a question a user asks
    /// of a harness with twenty of them, and the answer has to come from the
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
