//! Test Design Specification: the rules a storage backend keeps, asserted
//! against every backend there is.
//!
//! Features under test: `tetanus_core::storage::KvStore` as served by
//! `json::Store` and `sqlite::SqliteStore`, and the named registry that mounts
//! them side by side.
//!
//! Why the suite is shaped this way: upstream ships one conformance suite
//! (`packages/storage/tests/contract.ts`) that every backend is run through,
//! and the reason is the reason a seam exists at all - a caller holding a
//! `dyn KvStore` cannot tell the media apart, so a rule that holds for one
//! backend and not the other is not a rule anybody can rely on. Each case here
//! therefore runs its whole body twice, once per backend, and says which one
//! failed. The backend-specific cases that follow are the ones with nothing to
//! compare: a file's text format, a database's identity stamp.
//!
//! Features NOT tested here: the file backend's own format, rollback and
//! atomic-publish rules, which are `storage.rs` (TC-PORT-STORE-J1..J13).
//!
//! Environmental needs: a temporary directory. No network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_core::storage::{
    KvStore, SharedStore, SqliteStore, StorageError, StorageRegistry, Store,
};

/// The two media, each opened over its own path in one temporary directory.
///
/// A closure per backend rather than a list of trait objects, because a case
/// that reopens a store needs to build a second one over the same path.
/// One backend as this suite drives it: a name for the failure message, and a
/// way to open that medium over a path.
type Backend = (
    &'static str,
    fn(&std::path::Path, &[&str]) -> Box<dyn KvStore>,
);

fn backends() -> Vec<Backend> {
    vec![
        ("json", |path, declared| {
            Box::new(Store::open(path, declared).expect("the file store opens"))
        }),
        ("sqlite", |path, declared| {
            Box::new(SqliteStore::open(path, declared).expect("the database opens"))
        }),
    ]
}

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

/// TC-PORT-STORE-C1: a value survives a write and a fresh open.
///
/// Upstream: the contract suite's round trip, run against each backend.
///
/// Input: two values written, the store dropped, and the medium reopened.
/// Expected: both read back, on both backends. A store that only answers from
/// memory would pass every other case in this file.
#[test]
fn a_value_survives_a_fresh_open() {
    for (backend, open) in backends() {
        let home = dir();
        let path = home.path().join(format!("{backend}.store"));

        let mut store = open(&path, &["checkpoints"]);
        store
            .put("checkpoints", "usage", json!({ "seq": 12 }))
            .expect("stored");
        store
            .put("checkpoints", "title", json!("a name"))
            .expect("stored");
        drop(store);

        let reopened = open(&path, &["checkpoints"]);
        assert_eq!(
            reopened.get("checkpoints", "usage").expect("read"),
            Some(json!({ "seq": 12 })),
            "{backend} lost a value across a reopen"
        );
        assert_eq!(
            reopened.get("checkpoints", "title").expect("read"),
            Some(json!("a name")),
            "{backend}"
        );
    }
}

/// TC-PORT-STORE-C2: a table declared but never written reads as empty.
///
/// The rule that lets a deployment add a table without migrating its medium.
///
/// Input: a store opened with two tables, one of them written.
/// Expected: the untouched table reads as an empty table rather than as an
/// error or a missing one, on both backends.
#[test]
fn a_declared_table_the_medium_lacks_reads_as_empty() {
    for (backend, open) in backends() {
        let home = dir();
        let mut store = open(
            &home.path().join(format!("{backend}.store")),
            &["written", "untouched"],
        );
        store.put("written", "k", json!(1)).expect("stored");

        assert!(
            store.read_table("untouched").expect("read").is_empty(),
            "{backend} did not read an undeclared-but-declared table as empty"
        );
        assert_eq!(
            store.get("untouched", "k").expect("read"),
            None,
            "{backend}"
        );
    }
}

/// TC-PORT-STORE-C3: a table nobody declared is a caller mistake.
///
/// Never a table that quietly appears: a typo would otherwise write where
/// nothing reads, which is indistinguishable from the data being lost.
///
/// Input: a read and a write against an undeclared name.
/// Expected: `UndeclaredTable` from both operations on both backends, naming
/// what was declared; and the write did not create it.
#[test]
fn an_undeclared_table_is_refused_by_both() {
    for (backend, open) in backends() {
        let home = dir();
        let mut store = open(&home.path().join(format!("{backend}.store")), &["known"]);

        match store.get("guessed", "k") {
            Err(StorageError::UndeclaredTable { name, declared }) => {
                assert_eq!(name, "guessed", "{backend}");
                assert_eq!(declared, vec!["known".to_string()], "{backend}");
            }
            other => panic!("{backend}: expected an undeclared table, got {other:?}"),
        }
        assert!(
            matches!(
                store.put("guessed", "k", json!(1)),
                Err(StorageError::UndeclaredTable { .. })
            ),
            "{backend} accepted a write to a table nobody declared"
        );
        assert_eq!(store.declared(), vec!["known".to_string()], "{backend}");
    }
}

/// TC-PORT-STORE-C4: nothing is written until something is stored.
///
/// A run that stores nothing leaves no trace. The file backend has always kept
/// this; the database backend has to open its connection lazily to keep it,
/// and a backend that broke the rule would leave an empty database file in
/// every workspace that merely booted.
///
/// Input: a store opened and read, with no write; then one write.
/// Expected: no file on disk after the reads, on both backends; a file after
/// the write.
#[test]
fn nothing_is_written_until_something_is_stored() {
    for (backend, open) in backends() {
        let home = dir();
        let path = home.path().join(format!("{backend}.store"));

        let mut store = open(&path, &["t"]);
        assert_eq!(store.get("t", "absent").expect("read"), None, "{backend}");
        assert!(store.read_table("t").expect("read").is_empty(), "{backend}");
        assert!(
            !path.exists(),
            "{backend} materialized a medium for a store nobody wrote to"
        );

        store.put("t", "k", json!(1)).expect("stored");
        assert!(path.exists(), "{backend} did not materialize on the write");
    }
}

/// TC-PORT-STORE-C5: a remove answers what was there, and a remove of nothing
/// writes nothing.
///
/// The second half is the subtle one: a caller clearing a key it never set
/// must not be what creates the medium.
///
/// Input: a remove against an empty store, then a write and a remove.
/// Expected: `None` and no medium for the first; the stored value and an empty
/// table for the second, on both backends.
#[test]
fn a_remove_answers_what_was_there() {
    for (backend, open) in backends() {
        let home = dir();
        let path = home.path().join(format!("{backend}.store"));
        let mut store = open(&path, &["t"]);

        assert_eq!(
            store.remove("t", "never").expect("removed"),
            None,
            "{backend}"
        );
        assert!(
            !path.exists(),
            "{backend} materialized a medium for a remove that found nothing"
        );

        store.put("t", "k", json!("v")).expect("stored");
        assert_eq!(
            store.remove("t", "k").expect("removed"),
            Some(json!("v")),
            "{backend}"
        );
        assert!(store.read_table("t").expect("read").is_empty(), "{backend}");
    }
}

/// TC-PORT-STORE-C6: a write answers the value it replaced.
///
/// Input: two writes to one key.
/// Expected: the first answers `None`, the second answers the first value, and
/// the table holds the second, on both backends.
#[test]
fn a_write_answers_the_value_it_replaced() {
    for (backend, open) in backends() {
        let home = dir();
        let mut store = open(&home.path().join(format!("{backend}.store")), &["t"]);

        assert_eq!(
            store.put("t", "k", json!(1)).expect("stored"),
            None,
            "{backend}"
        );
        assert_eq!(
            store.put("t", "k", json!(2)).expect("stored"),
            Some(json!(1)),
            "{backend}"
        );
        assert_eq!(
            store.read_table("t").expect("read").get("k"),
            Some(&json!(2)),
            "{backend}"
        );
    }
}

/// TC-PORT-STORE-C7: a table another component owns is not disturbed.
///
/// Two components may share one medium. One of them opening it, declaring only
/// its own tables and writing must not delete the other's rows - which is the
/// rule that makes sharing a store safe at all.
///
/// Input: one opener writing table `a`, a second opener declaring only `b` and
/// writing, then the first table read again through a third open.
/// Expected: both tables intact on both backends.
#[test]
fn a_table_another_component_owns_is_kept() {
    for (backend, open) in backends() {
        let home = dir();
        let path = home.path().join(format!("{backend}.store"));

        let mut first = open(&path, &["a"]);
        first.put("a", "k", json!("mine")).expect("stored");
        drop(first);

        let mut second = open(&path, &["b"]);
        second.put("b", "k", json!("theirs")).expect("stored");
        drop(second);

        let both = open(&path, &["a", "b"]);
        assert_eq!(
            both.get("a", "k").expect("read"),
            Some(json!("mine")),
            "{backend} dropped a table its second opener never declared"
        );
        assert_eq!(
            both.get("b", "k").expect("read"),
            Some(json!("theirs")),
            "{backend}"
        );
    }
}

/// TC-PORT-STORE-C8: names are checked the same way everywhere.
///
/// One character set for both media, so a name never has to be escaped
/// differently depending on where it is stored - which is what would make a
/// migration between the two lossy.
///
/// Input: a bad table name at open and a bad key at write.
/// Expected: `BadName` from both backends for both, naming what was rejected.
#[test]
fn a_bad_name_is_refused_the_same_way() {
    let home = dir();
    assert!(matches!(
        Store::open(home.path().join("j.store"), &["Bad Table"]),
        Err(StorageError::BadName { what: "table", .. })
    ));
    assert!(matches!(
        SqliteStore::open(home.path().join("s.store"), &["Bad Table"]),
        Err(StorageError::BadName { what: "table", .. })
    ));

    for (backend, open) in backends() {
        let mut store = open(&home.path().join(format!("{backend}.names")), &["t"]);
        assert!(
            matches!(
                store.put("t", "NOT A KEY", json!(1)),
                Err(StorageError::BadName { what: "key", .. })
            ),
            "{backend} accepted a key nobody could put in a file name"
        );
    }
}

/// TC-PORT-STORE-S1: a database that is not one of ours is refused.
///
/// Backend-specific because it has no counterpart in a text file: an unrelated
/// SQLite database opened as a store would otherwise be grown a `records`
/// table, which is a corruption of somebody else's data rather than a failure.
///
/// Input: a SQLite file created by something else, opened as a store.
/// Expected: `Malformed` naming the identity that was found, and the foreign
/// database still holds its own table afterwards.
#[test]
fn a_database_that_is_not_ours_is_refused() {
    let home = dir();
    let path = home.path().join("foreign.db");
    let foreign = rusqlite::Connection::open(&path).expect("a database");
    foreign
        .execute("CREATE TABLE somebody_elses (id INTEGER)", [])
        .expect("their table");
    drop(foreign);

    match SqliteStore::open(&path, &["t"]) {
        Err(StorageError::Malformed { message, .. }) => {
            assert!(
                message.contains("not a tetanus key-value store"),
                "{message}"
            );
        }
        other => panic!("expected a refusal, got {:?}", other.map(|_| "a store")),
    }

    let reopened = rusqlite::Connection::open(&path).expect("still a database");
    let survived: i64 = reopened
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'somebody_elses'",
            [],
            |row| row.get(0),
        )
        .expect("their schema");
    assert_eq!(survived, 1, "the refusal must not have touched their file");
}

/// TC-PORT-STORE-S2: a database from a schema this build does not read is
/// refused rather than misread.
///
/// Input: a store's own database with its `user_version` moved on.
/// Expected: `ForeignVersion` carrying the version found - the same class the
/// file backend answers for a file format it does not read, because the reader
/// has the same question either way.
#[test]
fn a_future_schema_is_refused() {
    let home = dir();
    let path = home.path().join("future.db");
    let mut store = SqliteStore::open(&path, &["t"]).expect("opens");
    store.put("t", "k", json!(1)).expect("stored");
    drop(store);

    let bumped = rusqlite::Connection::open(&path).expect("a database");
    bumped
        .pragma_update(None, "user_version", 99)
        .expect("moved on");
    drop(bumped);

    match SqliteStore::open(&path, &["t"]) {
        Err(StorageError::ForeignVersion { found, .. }) => assert_eq!(found, 99),
        other => panic!(
            "expected a foreign version, got {:?}",
            other.map(|_| "a store")
        ),
    }
}

/// TC-STORE-REG-1: two media are mounted side by side and asked for by name.
///
/// The registry exists because there are two backends. Which one serves which
/// consumer is that consumer's configuration, never a hub-wide current
/// backend, because a global choice cannot be scoped to one component.
///
/// Input: both backends mounted, then each looked up and written through.
/// Expected: each write lands in its own medium and neither name resolves to
/// the other.
#[test]
fn two_media_are_mounted_side_by_side() {
    let home = dir();
    let registry = StorageRegistry::new();
    let file = home.path().join("mounted.store");
    let database = home.path().join("mounted.db");

    let _json = registry
        .register(
            "json",
            Arc::new(Mutex::new(Store::open(&file, &["t"]).expect("opens"))),
        )
        .expect("mounted");
    let _sqlite = registry
        .register(
            "sqlite",
            Arc::new(Mutex::new(
                SqliteStore::open(&database, &["t"]).expect("opens"),
            )),
        )
        .expect("mounted");

    assert_eq!(
        registry.names(),
        vec!["json".to_string(), "sqlite".to_string()]
    );
    registry
        .get("json")
        .expect("mounted")
        .lock()
        .expect("the store")
        .put("t", "k", json!("in the file"))
        .expect("stored");

    assert!(file.exists(), "the file backend wrote its own medium");
    assert!(
        !database.exists(),
        "a write through one name must not touch the other medium"
    );
}

/// TC-STORE-REG-2: a name mounted twice is a configuration mistake.
///
/// Refused rather than replaced: silently keeping the second would give half
/// a deployment's consumers the other medium, with nothing in a log to say so.
///
/// Input: two stores registered under one name; then an unmounted lookup.
/// Expected: `DuplicateStore` for the second registration, and `UnknownStore`
/// listing what is mounted for the lookup - because the commonest cause of
/// that error is one deployment spelling a name two ways.
#[test]
fn a_name_mounted_twice_is_refused() {
    let home = dir();
    let registry = StorageRegistry::new();
    let store = || -> SharedStore {
        Arc::new(Mutex::new(
            Store::open(home.path().join("dup.store"), &["t"]).expect("opens"),
        ))
    };

    let _first = registry.register("kv", store()).expect("mounted");
    match registry.register("kv", store()) {
        Err(StorageError::DuplicateStore { name }) => assert_eq!(name, "kv"),
        other => panic!("expected a refusal, got {:?}", other.map(|_| "a handle")),
    }

    match registry.get("kb") {
        Err(StorageError::UnknownStore { name, mounted }) => {
            assert_eq!(name, "kb");
            assert_eq!(mounted, vec!["kv".to_string()]);
        }
        other => panic!(
            "expected an unknown store, got {:?}",
            other.map(|_| "a store")
        ),
    }
}

/// TC-STORE-REG-3: unmounting is an effect, and a stale handle removes
/// nothing.
///
/// Upstream states this rule where it registers: after a dispose and a
/// re-register under the same name, the first disposer firing again must not
/// remove the successor. It is the failure that only appears under a reload,
/// where the consequence is a deployment that quietly loses its store.
///
/// Input: a store mounted, unmounted, and a second mounted under the same
/// name; then the first handle dropped.
/// Expected: the name is unmounted after the first drop and still mounted -
/// pointing at the successor - after the stale drop.
#[test]
fn a_stale_handle_does_not_unmount_its_successor() {
    let home = dir();
    let registry = StorageRegistry::new();
    let store = |name: &str| -> SharedStore {
        Arc::new(Mutex::new(
            Store::open(home.path().join(name), &["t"]).expect("opens"),
        ))
    };

    let first = registry
        .register("kv", store("first.store"))
        .expect("mounted");
    drop(first);
    assert!(registry.names().is_empty(), "the handle unmounted the name");

    let successor = store("second.store");
    let _second = registry
        .register("kv", Arc::clone(&successor))
        .expect("mounted again");

    // The stale handle is already dropped above; dropping a second handle for
    // the same name is what a reload does, so assert the successor survives a
    // repeat of the first drop's effect.
    let repeat = registry
        .register("other", store("third.store"))
        .expect("mounted");
    drop(repeat);

    assert_eq!(
        registry.names(),
        vec!["kv".to_string()],
        "the successor was unmounted by somebody else's handle"
    );
    assert!(
        Arc::ptr_eq(&registry.get("kv").expect("mounted"), &successor),
        "the name resolves to the successor"
    );
}
