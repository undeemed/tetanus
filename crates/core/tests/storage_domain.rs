//! Test Design Specification: the layer between a component and a medium.
//!
//! Feature under test: `tetanus_core::storage::domain` - declared tables with
//! a validator, a version stamped on the medium, a change event per durable
//! write, and the routing that says which store serves which domain.
//!
//! Upstream: `packages/storage/storage-domain`. Its spec vocabulary, its
//! `domain-changed` event, its per-domain routing and its open-time refusals
//! all restate here; its zod schemas do not, because this workspace has no
//! schema language at this layer and a table carries a predicate its owner
//! wrote instead.
//!
//! Approach: real stores on both media where the case is about durability, and
//! a bus with a recording listener where it is about announcement. The
//! validation cases are asserted in *both* directions - what cannot be written
//! and what will not be served - because a layer that only checks writes is a
//! layer that hands a component data it does not understand the first time two
//! builds share a store.
//!
//! Features NOT tested here: what each medium does with a table of JSON, which
//! is `storage_backends.rs`.
//!
//! Environmental needs: a temporary directory. No network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_core::storage::domain::{tables_for, DomainError, Operation};
use tetanus_core::storage::{
    Domain, DomainChanged, DomainRouter, DomainSpec, GlobalSpec, SharedStore, SqliteStore,
    StorageRegistry, Store, TableSpec,
};
use tetanus_core::EventBus;

/// A spec that accepts an object with a numeric `count`, which is enough shape
/// for a validator to have an opinion about.
fn spec(version: u32) -> DomainSpec {
    DomainSpec::new("sessions", version).table(
        "state",
        TableSpec::new(
            |value| match value.get("count").and_then(serde_json::Value::as_u64) {
                Some(_) => Ok(()),
                None => Err("a state record needs a numeric `count`".to_string()),
            },
        ),
    )
}

fn file_store(dir: &std::path::Path, spec: &DomainSpec) -> SharedStore {
    let names = tables_for(spec);
    let declared: Vec<&str> = names.iter().map(String::as_str).collect();
    Arc::new(Mutex::new(
        Store::open(dir.join("domains.store"), &declared).expect("the store opens"),
    ))
}

/// TC-PORT-DOM-1: a record round-trips through the domain onto the medium.
///
/// Both media, because a domain that only worked over one would be a domain
/// that had quietly chosen a medium for its component.
///
/// Input: a record written through a domain on each backend, then read back
/// through a second domain over a freshly opened store.
/// Expected: the value, on both.
#[test]
fn a_record_round_trips_on_either_medium() {
    let home = tempfile::tempdir().expect("temp dir");
    let names = tables_for(&spec(1));
    let declared: Vec<&str> = names.iter().map(String::as_str).collect();

    let stores: Vec<(&str, SharedStore)> = vec![
        (
            "json",
            Arc::new(Mutex::new(
                Store::open(home.path().join("d.store"), &declared).expect("opens"),
            )),
        ),
        (
            "sqlite",
            Arc::new(Mutex::new(
                SqliteStore::open(home.path().join("d.db"), &declared).expect("opens"),
            )),
        ),
    ];

    for (medium, store) in stores {
        let domain = Domain::open(spec(1), Arc::clone(&store)).expect("opens");
        domain
            .put("state", "s-1", json!({ "count": 3 }))
            .expect("stored");

        let reopened = Domain::open(spec(1), store).expect("opens again");
        assert_eq!(
            reopened.get("state", "s-1").expect("read"),
            Some(json!({ "count": 3 })),
            "{medium} lost the record"
        );
    }
}

/// TC-PORT-DOM-2: a record the declaration refuses is never written.
///
/// The write half of validation at the durable boundary. The medium must be
/// untouched afterwards, not merely the read: a store that holds a record its
/// own domain would refuse is a store nobody can trust to read back.
///
/// Input: a record with no `count`.
/// Expected: `Invalid` naming the domain, table, key and the validator's own
/// words; nothing stored under that key.
#[test]
fn a_record_the_declaration_refuses_is_not_written() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    let domain = Domain::open(spec(1), Arc::clone(&store)).expect("opens");

    match domain.put("state", "s-1", json!({ "nope": true })) {
        Err(DomainError::Invalid {
            domain: name,
            table,
            key,
            message,
        }) => {
            assert_eq!(
                (name.as_str(), table.as_str(), key.as_str()),
                ("sessions", "state", "s-1")
            );
            assert!(message.contains("numeric `count`"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(domain.get("state", "s-1").expect("read"), None);
    assert!(domain.all("state").expect("read").is_empty());
}

/// TC-PORT-DOM-3: a stored record the declaration would now refuse is
/// reported, not served.
///
/// The read half, and the one that matters more. A component reading a record
/// it would have refused to write is acting on data it does not understand -
/// which is what happens when two builds share a store and only one of them
/// knows the new shape.
///
/// Input: a value written past the domain, straight onto the medium.
/// Expected: `Invalid` from the read, naming the record.
#[test]
fn a_stored_record_that_no_longer_validates_is_reported() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    store
        .lock()
        .expect("the store")
        .put("sessions.state", "s-1", json!("not even an object"))
        .expect("stored behind the domain's back");

    let domain = Domain::open(spec(1), store).expect("opens");

    match domain.get("state", "s-1") {
        Err(DomainError::Invalid { key, .. }) => assert_eq!(key, "s-1"),
        other => panic!("a record nobody would write was served: {other:?}"),
    }
}

/// TC-PORT-DOM-4: a domain opened against another version's data refuses.
///
/// Migrating would mean converting a record whose meaning changed, which is
/// guessing, and the guess is silent. Refusing at open is loud and early.
///
/// Input: data written at version 1, opened at version 2.
/// Expected: `ForeignVersion` carrying both numbers; and a store nobody has
/// written carries no stamp, so a first open at any version is fine.
#[test]
fn another_versions_data_refuses_at_open() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));

    Domain::open(spec(2), Arc::clone(&store)).expect("an unwritten store stamps nothing");

    let first = Domain::open(spec(1), Arc::clone(&store)).expect("opens");
    first
        .put("state", "s-1", json!({ "count": 1 }))
        .expect("stored");

    match Domain::open(spec(2), store) {
        Err(DomainError::ForeignVersion {
            declared, found, ..
        }) => assert_eq!((declared, found), (2, 1)),
        other => panic!(
            "expected a version refusal, got {}",
            other.map(|_| "a domain").unwrap_or("an error")
        ),
    }
}

/// TC-PORT-DOM-5: every durable write is announced once, after it is durable.
///
/// After, not before: an event for a write that then failed would have a
/// consumer acting on a record the medium never took. The event carries the
/// new value and no old one, because a consumer wanting a diff keeps its own
/// previous copy and shipping both doubles what an event costs everyone else.
///
/// Input: a put, a delete, and a delete of a key nobody stored.
/// Expected: two events in order, the put carrying the value and the delete
/// carrying none; the delete that found nothing announces nothing.
#[test]
fn every_durable_write_is_announced_once() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::<DomainChanged>::new()));
    let recorder = Arc::clone(&seen);
    let _watch = bus.on_emit::<DomainChanged>(move |change| {
        recorder.lock().expect("seen").push(change.clone());
    });

    let domain = Domain::watched(spec(1), store, bus).expect("opens");
    domain
        .put("state", "s-1", json!({ "count": 7 }))
        .expect("stored");
    assert!(domain.delete("state", "s-1").expect("removed"));
    assert!(!domain.delete("state", "never").expect("nothing to remove"));

    let changes = seen.lock().expect("seen").clone();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert_eq!(changes[0].operation, Operation::Put);
    assert_eq!(changes[0].value, Some(json!({ "count": 7 })));
    assert_eq!(
        (
            changes[0].domain.as_str(),
            changes[0].table.as_str(),
            changes[0].key.as_str()
        ),
        ("sessions", "state", "s-1")
    );
    assert_eq!(changes[1].operation, Operation::Deleted);
    assert_eq!(changes[1].value, None, "a tombstone carries nothing");
}

/// TC-PORT-DOM-6: a write that was refused announces nothing.
///
/// The other half of "after it is durable". A validator's refusal is the
/// commonest way a write does not happen, and an event for it would be a
/// consumer told about a record that does not exist.
///
/// Input: a record the declaration refuses, on a watched domain.
/// Expected: no events at all.
#[test]
fn a_refused_write_announces_nothing() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::<DomainChanged>::new()));
    let recorder = Arc::clone(&seen);
    let _watch = bus.on_emit::<DomainChanged>(move |change| {
        recorder.lock().expect("seen").push(change.clone());
    });

    let domain = Domain::watched(spec(1), store, bus).expect("opens");
    assert!(domain.put("state", "s-1", json!({})).is_err());

    assert!(
        seen.lock().expect("seen").is_empty(),
        "a refused write was announced"
    );
}

/// TC-PORT-DOM-7: the global slot serves its declared value until something
/// writes one, and writing one leaves a trace only then.
///
/// The same promise both media make about a store nobody wrote to, kept one
/// layer up: a domain that is opened and read leaves no file.
///
/// Input: a global read before any write, then a write, then a read.
/// Expected: the initial value, no medium on disk, then the written value.
#[test]
fn the_global_slot_serves_its_initial_value_first() {
    let home = tempfile::tempdir().expect("temp dir");
    let with_global = spec(1).global(GlobalSpec {
        validate: Arc::new(|value| match value.is_object() {
            true => Ok(()),
            false => Err("the global is an object".into()),
        }),
        initial: json!({ "theme": "dark" }),
    });
    let path = home.path().join("domains.store");
    let names = tables_for(&with_global);
    let declared: Vec<&str> = names.iter().map(String::as_str).collect();
    let store: SharedStore = Arc::new(Mutex::new(
        Store::open(&path, &declared).expect("the store opens"),
    ));

    let domain = Domain::open(with_global, store).expect("opens");
    assert_eq!(domain.global().expect("read"), json!({ "theme": "dark" }));
    assert!(!path.exists(), "reading a global materialized a medium");

    domain
        .set_global(json!({ "theme": "light" }))
        .expect("stored");
    assert_eq!(domain.global().expect("read"), json!({ "theme": "light" }));
    assert!(path.exists());
}

/// TC-PORT-DOM-8: a domain with no global says so rather than inventing one.
///
/// Input: `global` and `set_global` on a spec that declares none.
/// Expected: `NoGlobal` from both, naming the domain.
#[test]
fn a_domain_with_no_global_refuses_the_slot() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    let domain = Domain::open(spec(1), store).expect("opens");

    assert!(matches!(domain.global(), Err(DomainError::NoGlobal { .. })));
    assert!(matches!(
        domain.set_global(json!({})),
        Err(DomainError::NoGlobal { .. })
    ));
}

/// TC-PORT-DOM-9: routing sends a domain to the store a deployment chose, and
/// a route naming nothing fails at open.
///
/// At open and not at the first write, which is the whole point of resolving
/// early: a deployment that boots, runs and then fails on a write has already
/// told somebody the thing worked.
///
/// Input: two stores mounted, one domain routed to the second, and a third
/// domain routed to a name nobody mounted.
/// Expected: the record lands in the routed medium and not the default one;
/// the bad route fails at open naming what is mounted.
#[test]
fn routing_decides_the_medium_and_fails_early() {
    let home = tempfile::tempdir().expect("temp dir");
    let registry = StorageRegistry::new();
    let names = tables_for(&spec(1));
    let declared: Vec<&str> = names.iter().map(String::as_str).collect();

    let default_path = home.path().join("default.store");
    let other_path = home.path().join("other.store");
    let _a = registry
        .register(
            "default",
            Arc::new(Mutex::new(
                Store::open(&default_path, &declared).expect("opens"),
            )) as SharedStore,
        )
        .expect("mounted");
    let _b = registry
        .register(
            "other",
            Arc::new(Mutex::new(
                Store::open(&other_path, &declared).expect("opens"),
            )) as SharedStore,
        )
        .expect("mounted");

    let router = DomainRouter::new(registry, "default").route("sessions", "other");
    assert_eq!(router.store_for("sessions"), "other");
    assert_eq!(router.store_for("anything-else"), "default");

    let domain = router.open(spec(1)).expect("opens on the routed store");
    domain
        .put("state", "s-1", json!({ "count": 1 }))
        .expect("stored");
    assert!(other_path.exists(), "the routed medium took the write");
    assert!(
        !default_path.exists(),
        "the default medium was written to instead of the routed one"
    );

    let elsewhere = DomainRouter::new(StorageRegistry::new(), "nowhere");
    assert!(
        elsewhere.open(spec(1)).is_err(),
        "a route naming an unmounted store must fail at open, not at the first write"
    );
}

/// TC-PORT-DOM-10: two domains in one store do not collide.
///
/// One store holds several components' data, and two of them may each have a
/// `state` table without meaning the same thing. The namespacing is what makes
/// a shared medium safe; without it the second domain silently reads the
/// first's records.
///
/// Input: two domains with the same table name in one store, each writing the
/// same key.
/// Expected: each reads its own value.
#[test]
fn two_domains_in_one_store_keep_their_own_tables() {
    let home = tempfile::tempdir().expect("temp dir");
    let mine = spec(1);
    let theirs = DomainSpec::new("features", 1).table("state", TableSpec::any());
    let mut names = tables_for(&mine);
    names.extend(tables_for(&theirs));
    let declared: Vec<&str> = names.iter().map(String::as_str).collect();
    let store: SharedStore = Arc::new(Mutex::new(
        Store::open(home.path().join("shared.store"), &declared).expect("opens"),
    ));

    let sessions = Domain::open(mine, Arc::clone(&store)).expect("opens");
    let features = Domain::open(theirs, store).expect("opens");

    sessions
        .put("state", "k", json!({ "count": 1 }))
        .expect("stored");
    features.put("state", "k", json!("theirs")).expect("stored");

    assert_eq!(
        sessions.get("state", "k").expect("read"),
        Some(json!({ "count": 1 }))
    );
    assert_eq!(
        features.get("state", "k").expect("read"),
        Some(json!("theirs"))
    );
}

/// TC-PORT-DOM-11: a table nobody declared is a caller mistake, at every door.
///
/// The same rule the medium keeps one layer down, restated here because the
/// domain is what a component actually calls: a typo must not read as an empty
/// table, which is indistinguishable from data that is gone.
///
/// Input: a read, a write and a delete against an undeclared table.
/// Expected: `UnknownTable` from all three, naming what the domain declares.
#[test]
fn an_undeclared_table_is_refused_at_every_door() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    let domain = Domain::open(spec(1), store).expect("opens");

    for outcome in [
        domain.get("guessed", "k").err(),
        domain.put("guessed", "k", json!({ "count": 1 })).err(),
        domain.delete("guessed", "k").err(),
    ] {
        match outcome {
            Some(DomainError::UnknownTable {
                table, declared, ..
            }) => {
                assert_eq!(table, "guessed");
                assert_eq!(declared, vec!["state".to_string()]);
            }
            other => panic!("expected an undeclared table, got {other:?}"),
        }
    }
}

/// TC-PORT-DOM-12: a domain may not declare the table its own bookkeeping
/// uses.
///
/// The version stamp and the global value live in one table named for the
/// domain, so a component declaring that name would find its records beside a
/// number it did not write. The first draft prevented this with a character
/// the medium's name rule forbids, and the medium refused the name - so the
/// rule is enforced here instead, at open, where it can say what to do about
/// it.
///
/// Input: a spec declaring `meta`.
/// Expected: `ReservedTable` naming the domain, before any medium is touched.
#[test]
fn a_domain_may_not_declare_the_reserved_table() {
    let home = tempfile::tempdir().expect("temp dir");
    let store = file_store(home.path(), &spec(1));
    let clashing = DomainSpec::new("sessions", 1).table("meta", TableSpec::any());

    match Domain::open(clashing, store) {
        Err(DomainError::ReservedTable { domain }) => assert_eq!(domain, "sessions"),
        other => panic!(
            "expected a refusal, got {}",
            other.map(|_| "a domain").unwrap_or("an error")
        ),
    }
}
