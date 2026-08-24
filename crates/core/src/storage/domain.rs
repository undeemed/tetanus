//! The layer between a component and a medium: declared tables with a schema,
//! a version stamped on what they wrote, a change event per durable write, and
//! a route saying which store serves which domain.
//!
//! [`KvStore`](super::KvStore) is deliberately dumb - tables of JSON, no
//! opinion about what is in them. That is the right seam for a medium and the
//! wrong surface for a component, which wants to say "this is my data, this is
//! its shape, tell me when it changes". Everything here is that, and none of
//! it knows which medium answered.
//!
//! **A domain declares itself once.** Name, version, its tables, each table's
//! validator, and optionally one global singleton. The declaration is the
//! source of what is checked and of what is opened, so a table nobody declared
//! cannot be written by a typo and a record nobody would accept cannot be
//! stored.
//!
//! **Validation happens at the durable boundary, in both directions.** A value
//! the schema refuses is never written; a stored value that no longer
//! validates is *reported* rather than served, because a component that reads
//! a record it would have refused to write is a component acting on data it
//! does not understand. That is the case a version bump exists to prevent, and
//! it is the case that happens anyway when two builds share a store.
//!
//! **The version is stamped on the medium.** A domain opened against data
//! another version wrote refuses at open rather than migrating: converting a
//! record whose meaning changed is guessing, and the guess is silent.
//!
//! **A change is announced after it is durable, never before.** One event per
//! write, carrying the new value and nothing else - no old value, because a
//! consumer that wants a diff keeps its own previous copy, and shipping both
//! doubles what an event costs for a reader that does not.
//!
//! **Which medium serves which domain is a deployment's routing, not a
//! component's import.** A [`DomainRouter`] holds a default store name and
//! per-domain overrides; a route naming a store nobody mounted fails at open,
//! loudly, rather than at the first write.
//!
//! Parity: upstream `packages/storage/storage-domain` - its spec vocabulary,
//! its `domain-changed` event, its routing config and its open-time refusals.
//! Upstream validates with zod and projects the same schemas to RPC later;
//! this workspace has no schema language at this layer, so a table carries a
//! predicate its owner wrote, which is the same decision `crates/turn`'s tool
//! schemas make one layer up.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::events::{DispatchMode, Event, EventBus};

use super::{valid_name, SharedStore, StorageError, StorageRegistry, Table};

/// The table a domain stamps its version in, and holds its global value in.
///
/// A name a domain may not declare for itself. The first draft spelled it
/// `@meta` so that a collision was unrepresentable, and the medium refused the
/// name: that character set is deliberately one that is safe as a file name, a
/// JSON key and a SQL identifier at once. So the collision is prevented by a
/// rule instead of by a character, and the rule is enforced at open rather than
/// left as a warning nobody reads.
pub const META_TABLE: &str = "meta";

/// The key the version lives under in [`META_TABLE`].
pub const VERSION_KEY: &str = "version";

/// What a record has to satisfy to be stored.
///
/// A predicate rather than a schema document, because this workspace has no
/// schema language at this layer and inventing one here would be inventing it
/// for everybody. The message is the owner's own words: it reaches a person
/// who has to work out which record was wrong.
pub type Validator = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;

/// One declared table.
#[derive(Clone)]
pub struct TableSpec {
    pub validate: Validator,
}

impl TableSpec {
    /// A table whose records must satisfy `check`.
    pub fn new<F>(check: F) -> Self
    where
        F: Fn(&Value) -> Result<(), String> + Send + Sync + 'static,
    {
        Self {
            validate: Arc::new(check),
        }
    }

    /// A table that stores anything. Honest for a cache whose shape is its
    /// owner's business and nobody else's.
    pub fn any() -> Self {
        Self::new(|_| Ok(()))
    }
}

/// The single-value slot a domain may declare beside its tables.
#[derive(Clone)]
pub struct GlobalSpec {
    pub validate: Validator,
    /// What a reader is served before anything has been written. It is not
    /// written at open: a domain nobody has used leaves no trace, the same
    /// promise both media make.
    pub initial: Value,
}

/// One domain's declaration: identity, version and layout.
#[derive(Clone)]
pub struct DomainSpec {
    pub name: String,
    pub version: u32,
    pub tables: BTreeMap<String, TableSpec>,
    pub global: Option<GlobalSpec>,
}

impl DomainSpec {
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            name: name.into(),
            version,
            tables: BTreeMap::new(),
            global: None,
        }
    }

    pub fn table(mut self, name: impl Into<String>, spec: TableSpec) -> Self {
        self.tables.insert(name.into(), spec);
        self
    }

    pub fn global(mut self, spec: GlobalSpec) -> Self {
        self.global = Some(spec);
        self
    }

    /// The names this domain occupies in a store: its tables, namespaced by
    /// the domain, plus the meta table.
    ///
    /// Namespaced because one store holds several domains and two of them may
    /// each have a `state` table without meaning the same thing.
    fn declared(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tables
            .keys()
            .map(|table| qualified(&self.name, table))
            .collect();
        names.push(qualified(&self.name, META_TABLE));
        names
    }
}

/// How a domain's table is named on the medium.
fn qualified(domain: &str, table: &str) -> String {
    format!("{domain}.{table}")
}

/// What happened to one record, once it was durable.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainChanged {
    pub domain: String,
    /// The table, or empty for the global singleton - which is what upstream
    /// carries, and it costs a reader one comparison rather than a second
    /// event type.
    pub table: String,
    pub key: String,
    pub operation: Operation,
    /// The new value. Absent on a delete: there is nothing to carry, and a
    /// tombstone with a value in it would read as a write.
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Put,
    Deleted,
}

impl Event for DomainChanged {
    const TOPIC: &'static str = "storage/changed";
    const MODE: DispatchMode = DispatchMode::Emit;
    type Output = ();
}

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("the store this domain is routed to refused: {0}")]
    Store(#[from] StorageError),
    #[error("domain {domain:?} declares no table {table:?} (declared: {declared:?})")]
    UnknownTable {
        domain: String,
        table: String,
        declared: Vec<String>,
    },
    #[error("domain {domain:?} declares no global value")]
    NoGlobal { domain: String },
    /// A domain that declared the name its own bookkeeping uses. Refused
    /// rather than shadowed: a component silently sharing a table with the
    /// version stamp would find its records beside a number it did not write.
    #[error("domain {domain:?} may not declare a table named {META_TABLE:?}: that name is the domain's own")]
    ReservedTable { domain: String },
    #[error("{domain}.{table}: record {key:?} is not one this domain accepts: {message}")]
    Invalid {
        domain: String,
        table: String,
        key: String,
        message: String,
    },
    #[error(
        "domain {domain:?} is version {declared} and the store holds version {found}: \
         a record whose meaning changed is not one to guess at"
    )]
    ForeignVersion {
        domain: String,
        declared: u32,
        found: u32,
    },
}

/// One domain, open on the store it is routed to.
pub struct Domain {
    spec: DomainSpec,
    store: SharedStore,
    bus: Option<EventBus>,
}

impl Domain {
    /// Open `spec` on `store`, checking the version stamped there.
    ///
    /// A store holding no stamp is one this domain has never written, and the
    /// stamp is not written here either: the first durable write writes it,
    /// so opening a domain and doing nothing still leaves no trace.
    pub fn open(spec: DomainSpec, store: SharedStore) -> Result<Self, DomainError> {
        valid_name("domain", &spec.name)?;
        for table in spec.tables.keys() {
            valid_name("table", table)?;
            if table == META_TABLE {
                return Err(DomainError::ReservedTable {
                    domain: spec.name.clone(),
                });
            }
        }
        let domain = Self {
            spec,
            store,
            bus: None,
        };
        domain.check_version()?;
        Ok(domain)
    }

    /// The same, announcing every durable change on `bus`.
    pub fn watched(
        spec: DomainSpec,
        store: SharedStore,
        bus: EventBus,
    ) -> Result<Self, DomainError> {
        let mut domain = Self::open(spec, store)?;
        domain.bus = Some(bus);
        Ok(domain)
    }

    pub fn name(&self) -> &str {
        &self.spec.name
    }

    fn check_version(&self) -> Result<(), DomainError> {
        let meta = qualified(&self.spec.name, META_TABLE);
        let stored = self
            .store
            .lock()
            .expect("the domain's store")
            .get(&meta, VERSION_KEY)?;
        match stored.as_ref().and_then(Value::as_u64) {
            None => Ok(()),
            Some(found) if found == u64::from(self.spec.version) => Ok(()),
            Some(found) => Err(DomainError::ForeignVersion {
                domain: self.spec.name.clone(),
                declared: self.spec.version,
                found: found as u32,
            }),
        }
    }

    fn table_spec(&self, table: &str) -> Result<&TableSpec, DomainError> {
        self.spec
            .tables
            .get(table)
            .ok_or_else(|| DomainError::UnknownTable {
                domain: self.spec.name.clone(),
                table: table.to_string(),
                declared: self.spec.tables.keys().cloned().collect(),
            })
    }

    /// Read one record, checking it still satisfies the declaration.
    ///
    /// A stored value the schema would now refuse is an error and not a value:
    /// serving it would hand a component data it does not understand, which is
    /// exactly what the version stamp exists to prevent and exactly what
    /// happens anyway when two builds share a store.
    pub fn get(&self, table: &str, key: &str) -> Result<Option<Value>, DomainError> {
        let spec = self.table_spec(table)?;
        let stored = self
            .store
            .lock()
            .expect("the domain's store")
            .get(&qualified(&self.spec.name, table), key)?;
        match stored {
            None => Ok(None),
            Some(value) => match (spec.validate)(&value) {
                Ok(()) => Ok(Some(value)),
                Err(message) => Err(DomainError::Invalid {
                    domain: self.spec.name.clone(),
                    table: table.to_string(),
                    key: key.to_string(),
                    message,
                }),
            },
        }
    }

    /// Every record of one table, in key order.
    pub fn all(&self, table: &str) -> Result<Table, DomainError> {
        self.table_spec(table)?;
        Ok(self
            .store
            .lock()
            .expect("the domain's store")
            .read_table(&qualified(&self.spec.name, table))?)
    }

    /// Store one record, and announce it once it is durable.
    pub fn put(&self, table: &str, key: &str, value: Value) -> Result<(), DomainError> {
        let spec = self.table_spec(table)?;
        if let Err(message) = (spec.validate)(&value) {
            return Err(DomainError::Invalid {
                domain: self.spec.name.clone(),
                table: table.to_string(),
                key: key.to_string(),
                message,
            });
        }
        self.write(&qualified(&self.spec.name, table), key, value.clone())?;
        self.announce(DomainChanged {
            domain: self.spec.name.clone(),
            table: table.to_string(),
            key: key.to_string(),
            operation: Operation::Put,
            value: Some(value),
        });
        Ok(())
    }

    /// Remove one record. Answers whether there was one, and announces only
    /// when there was: an event for a key nobody stored describes a change
    /// that did not happen.
    pub fn delete(&self, table: &str, key: &str) -> Result<bool, DomainError> {
        self.table_spec(table)?;
        let removed = self
            .store
            .lock()
            .expect("the domain's store")
            .remove(&qualified(&self.spec.name, table), key)?;
        if removed.is_some() {
            self.announce(DomainChanged {
                domain: self.spec.name.clone(),
                table: table.to_string(),
                key: key.to_string(),
                operation: Operation::Deleted,
                value: None,
            });
        }
        Ok(removed.is_some())
    }

    /// The global value, or the declared initial one before anything wrote it.
    pub fn global(&self) -> Result<Value, DomainError> {
        let spec = self.spec.global.as_ref().ok_or(DomainError::NoGlobal {
            domain: self.spec.name.clone(),
        })?;
        let stored = self
            .store
            .lock()
            .expect("the domain's store")
            .get(&qualified(&self.spec.name, META_TABLE), "global")?;
        match stored {
            None => Ok(spec.initial.clone()),
            Some(value) => match (spec.validate)(&value) {
                Ok(()) => Ok(value),
                Err(message) => Err(DomainError::Invalid {
                    domain: self.spec.name.clone(),
                    table: String::new(),
                    key: "global".into(),
                    message,
                }),
            },
        }
    }

    /// Replace the global value.
    pub fn set_global(&self, value: Value) -> Result<(), DomainError> {
        let spec = self.spec.global.as_ref().ok_or(DomainError::NoGlobal {
            domain: self.spec.name.clone(),
        })?;
        if let Err(message) = (spec.validate)(&value) {
            return Err(DomainError::Invalid {
                domain: self.spec.name.clone(),
                table: String::new(),
                key: "global".into(),
                message,
            });
        }
        self.write(
            &qualified(&self.spec.name, META_TABLE),
            "global",
            value.clone(),
        )?;
        self.announce(DomainChanged {
            domain: self.spec.name.clone(),
            table: String::new(),
            key: "global".into(),
            operation: Operation::Put,
            value: Some(value),
        });
        Ok(())
    }

    /// One durable write, with the version stamped beside it.
    ///
    /// The stamp is written before the record and not after: a crash between
    /// the two leaves a store that says which version wrote what is in it,
    /// where the other order leaves records no reader can date.
    fn write(&self, table: &str, key: &str, value: Value) -> Result<(), DomainError> {
        let mut store = self.store.lock().expect("the domain's store");
        let meta = qualified(&self.spec.name, META_TABLE);
        if store.get(&meta, VERSION_KEY)?.is_none() {
            store.put(&meta, VERSION_KEY, Value::from(self.spec.version))?;
        }
        store.put(table, key, value)?;
        Ok(())
    }

    /// Announce a change that has already happened.
    fn announce(&self, change: DomainChanged) {
        if let Some(bus) = &self.bus {
            bus.emit(&change);
        }
    }
}

/// Which store serves which domain.
///
/// A deployment's decision rather than a component's: the component names its
/// domain, and the routing says where that lives. There is no universally
/// correct medium, which is why the default is required rather than guessed.
#[derive(Clone)]
pub struct DomainRouter {
    registry: StorageRegistry,
    default_store: String,
    routes: BTreeMap<String, String>,
}

impl DomainRouter {
    pub fn new(registry: StorageRegistry, default_store: impl Into<String>) -> Self {
        Self {
            registry,
            default_store: default_store.into(),
            routes: BTreeMap::new(),
        }
    }

    /// Send one domain somewhere other than the default.
    pub fn route(mut self, domain: impl Into<String>, store: impl Into<String>) -> Self {
        self.routes.insert(domain.into(), store.into());
        self
    }

    /// The store a domain is routed to, by name.
    pub fn store_for(&self, domain: &str) -> &str {
        self.routes
            .get(domain)
            .map(String::as_str)
            .unwrap_or(&self.default_store)
    }

    /// Open a domain on the store it is routed to.
    ///
    /// A route naming a store nobody mounted fails here, which is the point of
    /// resolving at open: the alternative is a deployment that boots, runs,
    /// and fails at the first write - by which time it has told a user the
    /// thing worked.
    pub fn open(&self, spec: DomainSpec) -> Result<Domain, DomainError> {
        let name = self.store_for(&spec.name).to_string();
        let store = self.registry.get(&name)?;
        declare_missing(&store, &spec)?;
        Domain::open(spec, store)
    }

    /// The same, with changes announced on `bus`.
    pub fn open_watched(&self, spec: DomainSpec, bus: EventBus) -> Result<Domain, DomainError> {
        let name = self.store_for(&spec.name).to_string();
        let store = self.registry.get(&name)?;
        declare_missing(&store, &spec)?;
        Domain::watched(spec, store, bus)
    }
}

/// Check the routed store was opened with the tables this domain needs.
///
/// A store declares its tables when it is opened, and a domain cannot reopen
/// somebody else's store, so this reports the mismatch rather than papering
/// over it: the deployment that opened the store is the one that has to name
/// the tables.
fn declare_missing(store: &SharedStore, spec: &DomainSpec) -> Result<(), DomainError> {
    let declared = store.lock().expect("the routed store").declared();
    let missing: Vec<String> = spec
        .declared()
        .into_iter()
        .filter(|name| !declared.contains(name))
        .collect();
    match missing.is_empty() {
        true => Ok(()),
        false => Err(DomainError::Store(StorageError::UndeclaredTable {
            name: missing.join(", "),
            declared,
        })),
    }
}

/// The table names a store must declare to serve `spec`.
///
/// Published so a composition opening the store and the component declaring
/// the domain agree without copying a format string.
pub fn tables_for(spec: &DomainSpec) -> Vec<String> {
    spec.declared()
}
