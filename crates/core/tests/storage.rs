//! Test Design Specification: the durable key-value store, ported.
//!
//! Feature under test: `tetanus_core::storage::Store` - named tables of JSON
//! in one file that is replaced whole and atomically. Upstream pins the same
//! backend in `packages/storage/storage-json/tests/json-backend.spec.ts`; each
//! case names the upstream case it comes from.
//!
//! Approach: real files in a temp directory, and the failure cases are made to
//! fail for real - the publish failure is produced by removing the directory
//! the store lives in, not by injecting a fake error. A store's whole promise
//! is about what is on disk after a crash or a refusal, so a suite that
//! mocked the disk would be asserting the mock.
//!
//! What is not restated, and why. Upstream's backend registry exists to let a
//! deployment choose between the JSON and SQLite backends; tetanus has one, so
//! there is nothing to choose and its `registry.spec.ts` has nothing to
//! restate until a second lands. Its `storage-domain` layer, its SQLite
//! backend, and the Cordis mount/dispose lifecycle around both are separate
//! packages. Its "close drains in-flight writes" case needs an async close
//! this store does not have: `put` publishes before it returns, so there is
//! never a write in flight to drain - which is the same guarantee reached by
//! not needing it.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key. One case removes a directory to force a write failure, and
//! restores nothing, because the temp directory is discarded either way.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tempfile::TempDir;
use tetanus_core::storage::{StorageError, Store, FORMAT_VERSION};

/// TC-PORT-STORE-J1: nothing is written until something is stored.
///
/// Upstream: "defers materialization until the first write".
///
/// A run that stores nothing should leave no trace. Creating the file at open
/// would also make "the store exists" stop meaning "something was stored",
/// which is the question an operator actually asks of a directory.
///
/// Input: a store opened at a path with no file, read from, then written to.
/// Expected: no file after opening or reading; a file after the first write;
/// and the declared table reads as empty before it, rather than failing.
#[test]
fn nothing_is_written_until_something_is_stored() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");

    let mut store = Store::open(&path, &["checkpoints"]).expect("open");
    assert!(store.table("checkpoints").expect("declared").is_empty());
    assert!(!path.exists(), "opening a store creates nothing");

    store.put("checkpoints", "a", json!(1)).expect("put");
    assert!(path.exists(), "the first write materializes it");
}

/// TC-PORT-STORE-J2: the file is human-readable, and says what it is.
///
/// Upstream: "publishes a human-readable pretty-printed file".
///
/// A store is small and is read by a person during an incident, so it is
/// pretty-printed rather than minified. It carries its format version so a
/// reader - human or otherwise - is never guessing which rules it was written
/// under.
///
/// Input: a store with two values written.
/// Expected: the file parses as JSON, carries the version and the tables, and
/// spans several lines rather than one.
#[test]
fn the_published_file_is_readable_and_versioned() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");
    let mut store = Store::open(&path, &["titles"]).expect("open");

    store.put("titles", "s1", json!("a session")).expect("put");
    store.put("titles", "s2", json!("another")).expect("put");

    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.lines().count() > 3, "pretty-printed: {text}");

    let document: Value = serde_json::from_str(&text).expect("parses");
    assert_eq!(document["version"], json!(FORMAT_VERSION));
    assert_eq!(document["tables"]["titles"]["s1"], json!("a session"));
    assert_eq!(document["tables"]["titles"]["s2"], json!("another"));
}

/// TC-PORT-STORE-J3: what was stored is what a fresh open reads.
///
/// The round trip is the whole point, and it is worth asserting through a
/// second `Store` rather than through the first one's memory - reading back
/// what is still in RAM would pass even if nothing had been written.
///
/// Input: values written, then a new store opened on the same path.
/// Expected: identical values, and a removal is durable too.
#[test]
fn what_was_stored_is_what_a_fresh_open_reads() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");

    {
        let mut store = Store::open(&path, &["kv"]).expect("open");
        store.put("kv", "a", json!({ "n": 1 })).expect("put");
        store.put("kv", "b", json!([1, 2, 3])).expect("put");
        assert_eq!(
            store.remove("kv", "a").expect("remove"),
            Some(json!({ "n": 1 }))
        );
    }

    let reopened = Store::open(&path, &["kv"]).expect("reopen");
    assert_eq!(reopened.get("kv", "a").expect("declared"), None);
    assert_eq!(
        reopened.get("kv", "b").expect("declared"),
        Some(&json!([1, 2, 3]))
    );
}

/// TC-PORT-STORE-J4: a table nobody declared is a caller error, not a new
/// table.
///
/// Upstream: "rejects undeclared table and global access as caller errors".
///
/// Creating it on demand would turn a typo into a table nobody reads, which
/// looks exactly like the data having been lost - and only at the point
/// something tries to read it back, long after the write that went astray.
///
/// Input: reads and writes against a table that was not declared.
/// Expected: `UndeclaredTable` naming what was asked for and what is
/// available, from every entry point, and no table created as a side effect.
#[test]
fn an_undeclared_table_is_a_caller_error() {
    let dir = TempDir::new().expect("temp dir");
    let mut store = Store::open(dir.path().join("store.json"), &["known"]).expect("open");

    match store.put("typo", "a", json!(1)) {
        Err(StorageError::UndeclaredTable { name, declared }) => {
            assert_eq!(name, "typo");
            assert_eq!(
                declared,
                vec!["known".to_string()],
                "and says what is there"
            );
        }
        other => panic!("expected an undeclared-table error, got {other:?}"),
    }
    assert!(matches!(
        store.table("typo"),
        Err(StorageError::UndeclaredTable { .. })
    ));
    assert!(matches!(
        store.remove("typo", "a"),
        Err(StorageError::UndeclaredTable { .. })
    ));
    assert!(
        store.table("typo").is_err(),
        "a refused access created nothing"
    );
}

/// TC-PORT-STORE-J5: a declared table the file does not hold reads as empty.
///
/// Upstream: "opens a file missing a declared table as that table empty".
///
/// This is what lets a deployment add a table without migrating its file. The
/// alternative - failing to open because a table is absent - would make every
/// new feature a breaking change to everyone's stored data.
///
/// Input: a file written with one table, reopened declaring two.
/// Expected: the existing table keeps its values, the new one is empty, and
/// nothing failed.
#[test]
fn a_declared_table_the_file_lacks_reads_as_empty() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");

    let mut first = Store::open(&path, &["old"]).expect("open");
    first.put("old", "a", json!(1)).expect("put");

    let grown = Store::open(&path, &["old", "new"]).expect("reopen with a new table");
    assert_eq!(grown.get("old", "a").expect("declared"), Some(&json!(1)));
    assert!(grown.table("new").expect("declared").is_empty());
}

/// TC-PORT-STORE-J6: a table this caller did not declare survives being
/// rewritten around.
///
/// Not an upstream case in this shape - upstream scopes a unit to one owner -
/// but it is the direct consequence of replacing the file whole, and getting
/// it wrong destroys another component's data silently. Two components sharing
/// one store is the ordinary arrangement, and the one that writes must not
/// delete what the other keeps.
///
/// Input: two tables written; a store reopened declaring only one, which then
/// writes.
/// Expected: the undeclared table is still in the file afterwards, with its
/// values intact.
#[test]
fn a_table_this_caller_did_not_declare_is_not_destroyed() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");

    let mut both = Store::open(&path, &["mine", "theirs"]).expect("open");
    both.put("mine", "a", json!(1)).expect("put");
    both.put("theirs", "b", json!(2)).expect("put");
    drop(both);

    let mut only_mine = Store::open(&path, &["mine"]).expect("reopen");
    only_mine.put("mine", "a", json!(99)).expect("put");

    let everything = Store::open(&path, &["mine", "theirs"]).expect("reopen");
    assert_eq!(
        everything.get("mine", "a").expect("declared"),
        Some(&json!(99))
    );
    assert_eq!(
        everything.get("theirs", "b").expect("declared"),
        Some(&json!(2)),
        "the other component's table survived a write it did not make"
    );
}

/// TC-PORT-STORE-J7: a file from another format version is refused, and said
/// so distinctly from a corrupt one.
///
/// Upstream: "rejects a foreign unit header", and "rejects malformed table
/// shapes and foreign versions distinctly".
///
/// The two need different answers. A corrupt file is damage; a foreign version
/// may have been written by a build that is still running, and the right move
/// is to stop rather than to overwrite it with this build's idea of the
/// format.
///
/// Input: a file at a later version, a file with no version, a file that is
/// not JSON, a file whose root is not an object, and one whose table is not an
/// object.
/// Expected: `ForeignVersion` carrying the version found for the first, and
/// `Malformed` for the rest - so a caller can tell "too new" from "broken".
#[test]
fn a_foreign_version_is_refused_distinctly_from_a_corrupt_file() {
    let dir = TempDir::new().expect("temp dir");

    let future = dir.path().join("future.json");
    std::fs::write(&future, r#"{"version": 99, "tables": {}}"#).expect("write");
    match Store::open(&future, &["kv"]) {
        Err(StorageError::ForeignVersion { found, .. }) => assert_eq!(found, 99),
        other => panic!("expected a foreign version, got {other:?}"),
    }

    for (name, contents) in [
        ("no-version.json", r#"{"tables": {}}"#),
        ("not-json.json", "{oh dear"),
        ("not-an-object.json", "[]"),
        ("bad-table.json", r#"{"version": 1, "tables": {"kv": 7}}"#),
        ("no-tables.json", r#"{"version": 1}"#),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("write");
        assert!(
            matches!(
                Store::open(&path, &["kv"]),
                Err(StorageError::Malformed { .. })
            ),
            "{name} should be malformed"
        );
    }
}

/// TC-PORT-STORE-J8: a read failure that is not "no such file" is propagated.
///
/// Upstream: "propagates non-ENOENT read failures".
///
/// Absent means empty; anything else means the store may exist and could not
/// be read, and starting from empty there would silently discard it - then
/// overwrite it on the first write.
///
/// Input: a path that is a directory, which the filesystem refuses to read as
/// a file for a reason that is not absence.
/// Expected: `Unreadable`, carrying the path, rather than an empty store.
#[test]
fn a_read_failure_that_is_not_absence_is_propagated() {
    let dir = TempDir::new().expect("temp dir");
    let as_directory = dir.path().join("store.json");
    std::fs::create_dir(&as_directory).expect("mkdir");

    match Store::open(&as_directory, &["kv"]) {
        Err(StorageError::Unreadable { path, .. }) => assert_eq!(path, as_directory),
        other => panic!("expected the read failure to propagate, got {other:?}"),
    }
}

/// TC-PORT-STORE-J9: a publish that fails rolls memory back to what is stored.
///
/// Upstream: "rolls back memory when a publish fails".
///
/// This is the invariant that matters after a restart. A store that remembered
/// a value it could not write would answer from memory for the rest of the
/// process and then lose it, so the bug would surface later and somewhere
/// else. Rolling back means a caller that ignores the error still reads what a
/// fresh open would read.
///
/// Input: a store with a stored value, whose directory is then removed so the
/// next publish cannot succeed.
/// Expected: the write fails as `Unwritable`; the in-memory value is the one
/// that was last successfully stored, not the one that failed; and a later
/// successful write - once the directory is back - publishes the rolled-back
/// state plus that write, never the lost one.
#[test]
fn a_failed_publish_rolls_memory_back() {
    let dir = TempDir::new().expect("temp dir");
    let home = dir.path().join("home");
    std::fs::create_dir(&home).expect("mkdir");
    let path = home.join("store.json");

    let mut store = Store::open(&path, &["kv"]).expect("open");
    store.put("kv", "kept", json!("first")).expect("put");

    std::fs::remove_dir_all(&home).expect("remove the directory under it");

    let refused = store
        .put("kv", "lost", json!("second"))
        .expect_err("nowhere to write");
    assert!(
        matches!(refused, StorageError::Unwritable { .. }),
        "{refused:?}"
    );
    assert_eq!(
        store.get("kv", "lost").expect("declared"),
        None,
        "a value that was not written is not remembered"
    );
    assert_eq!(
        store.get("kv", "kept").expect("declared"),
        Some(&json!("first")),
        "and the value that was written is still there"
    );

    std::fs::create_dir(&home).expect("mkdir");
    store.put("kv", "third", json!(3)).expect("put");
    let reopened = Store::open(&path, &["kv"]).expect("reopen");
    assert_eq!(
        reopened.get("kv", "kept").expect("declared"),
        Some(&json!("first"))
    );
    assert_eq!(
        reopened.get("kv", "third").expect("declared"),
        Some(&json!(3))
    );
    assert_eq!(
        reopened.get("kv", "lost").expect("declared"),
        None,
        "the failed write never reaches the file either"
    );
}

/// TC-PORT-STORE-J10: a publish leaves no temporary behind, whether it worked
/// or not.
///
/// A leftover temporary accumulates, and a reader that found one might take it
/// for the store. The failing half matters more than the succeeding half,
/// because that is the path where cleanup is easy to forget.
///
/// Input: several successful writes, then a write that fails because the
/// target path is a directory.
/// Expected: after both, the directory holds the store and nothing else.
#[test]
fn a_publish_leaves_no_temporary_behind() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");
    let mut store = Store::open(&path, &["kv"]).expect("open");

    for n in 0..3 {
        store.put("kv", &format!("k{n}"), json!(n)).expect("put");
    }
    assert_eq!(entries(dir.path()), vec!["store.json".to_string()]);

    // A target that is a directory makes the rename fail after the temporary
    // has already been written, which is the path that leaks if cleanup is
    // only on the happy side.
    let blocked = dir.path().join("blocked.json");
    std::fs::create_dir(&blocked).expect("mkdir");
    let mut awkward = Store::open(dir.path().join("other.json"), &["kv"]).expect("open");
    awkward.put("kv", "a", json!(1)).expect("put");
    std::fs::remove_file(dir.path().join("other.json")).expect("remove");
    std::fs::create_dir(dir.path().join("other.json")).expect("put a directory in its place");
    let _ = awkward.put("kv", "b", json!(2));

    let left: Vec<String> = entries(dir.path())
        .into_iter()
        .filter(|name| name.ends_with(".tmp") || name.starts_with('.'))
        .collect();
    assert!(left.is_empty(), "temporaries left behind: {left:?}");
}

/// TC-PORT-STORE-J11: a name that is not a name is refused.
///
/// Upstream: "rejects invalid unit and table names".
///
/// The character set is the one that is safe in a file name, a JSON key and a
/// log line at once, so a name never needs escaping differently depending on
/// where it is shown.
///
/// Input: table names and key names that are empty, too long, uppercase, or
/// carry a separator or a control character.
/// Expected: `BadName` in every case, naming which of the two it was, and the
/// good names beside them still accepted.
#[test]
fn a_name_that_is_not_a_name_is_refused() {
    let dir = TempDir::new().expect("temp dir");

    for bad in [
        "",
        "Upper",
        "has space",
        "has/slash",
        "has\nnewline",
        &"x".repeat(65),
    ] {
        match Store::open(dir.path().join("s.json"), &[bad]) {
            Err(StorageError::BadName { what, .. }) => assert_eq!(what, "table"),
            other => panic!("table name {bad:?} should be refused, got {other:?}"),
        }
    }

    let mut store = Store::open(dir.path().join("s.json"), &["ok.table-1_2"]).expect("good names");
    for bad in ["", "Upper", "has space", &"x".repeat(65)] {
        match store.put("ok.table-1_2", bad, json!(1)) {
            Err(StorageError::BadName { what, .. }) => assert_eq!(what, "key"),
            other => panic!("key {bad:?} should be refused, got {other:?}"),
        }
    }
    store
        .put("ok.table-1_2", "good.key-1_2", json!(1))
        .expect("put");
}

/// TC-PORT-STORE-J12: a checkpoint-shaped value survives the round trip
/// unchanged.
///
/// The store's first real caller is the projection checkpoint, whose whole
/// safety argument rests on state being plain JSON that persists exactly. A
/// value that came back subtly different - a number retyped, a key reordered
/// into something a comparison rejects - would break that quietly.
///
/// Input: a nested value covering every JSON kind, including an empty object,
/// an empty array, a null, a negative number and a non-ASCII string.
/// Expected: it reads back equal, through the file rather than from memory.
#[test]
fn a_nested_value_survives_the_round_trip_exactly() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");

    let value = json!({
        "ver": 7,
        "seq": -1,
        "val": {
            "counts": [0, 1, 2],
            "nested": { "deep": { "empty": {}, "list": [] } },
            "absent": null,
            "text": "a\u{1F600}b \" \\ \n",
            "flag": false,
        },
    });

    let mut store = Store::open(&path, &["checkpoints"]).expect("open");
    store.put("checkpoints", "s1", value.clone()).expect("put");

    let reopened = Store::open(&path, &["checkpoints"]).expect("reopen");
    assert_eq!(
        reopened.get("checkpoints", "s1").expect("declared"),
        Some(&value)
    );

    let whole: BTreeMap<String, Value> = reopened.table("checkpoints").expect("declared").clone();
    assert_eq!(whole.len(), 1);
}

/// TC-PORT-STORE-J13: a publish replaces the file rather than rewriting it in
/// place.
///
/// Upstream: the publish protocol in its `atomic.ts` - write a same-directory
/// temporary, fsync it, rename over the target.
///
/// Atomicity is the store's headline promise and the one a suite can most
/// easily fail to check: a copy, or a truncate-and-write, passes every case
/// about *content* while leaving a window in which a reader sees half a file.
/// A mutation to `std::fs::copy` did pass the rest of this suite, which is why
/// this case exists.
///
/// The observable difference is identity. A rename swaps in a new directory
/// entry, so the file's inode changes and anything holding the old one keeps
/// reading the old bytes. A copy or truncate keeps the inode and mutates the
/// bytes underneath every reader at once - which is precisely the torn read
/// the protocol exists to prevent.
///
/// Input: a store written, its inode recorded, a reader opened on the old
/// file, then a second write.
/// Expected: the inode changes across the publish, and the still-open reader
/// sees exactly the bytes that were there when it opened.
#[cfg(unix)]
#[test]
fn a_publish_replaces_the_file_rather_than_rewriting_it() {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.json");
    let mut store = Store::open(&path, &["kv"]).expect("open");

    store.put("kv", "a", json!("first")).expect("put");
    let before = std::fs::metadata(&path).expect("stat").ino();

    // Held across the publish. Under a rename this handle keeps the old file
    // alive and unchanged; under a copy it would see the new bytes.
    let mut held = std::fs::File::open(&path).expect("open the published file");

    store.put("kv", "a", json!("second")).expect("put");
    let after = std::fs::metadata(&path).expect("stat").ino();

    assert_ne!(
        before, after,
        "a publish swaps in a new file; the same inode means it was written in place"
    );

    let mut seen = String::new();
    held.read_to_string(&mut seen).expect("read the held file");
    assert!(
        seen.contains("first") && !seen.contains("second"),
        "a reader that opened before the publish still reads what it opened: {seen}"
    );

    // And the new file is the new content, so the swap published rather than
    // merely replaced.
    let published = std::fs::read_to_string(&path).expect("read");
    assert!(published.contains("second"), "{published}");
}

/// The file names in a directory, sorted, so a case can say what is there.
fn entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}
