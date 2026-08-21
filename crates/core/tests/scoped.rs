//! Test Design Specification: scoped stores, ported.
//!
//! Feature under test: `tetanus_core::scoped` - working state that belongs to
//! one scope, cannot be read from another, and goes when the scope does.
//! Upstream answers the same question with Cordis scopes, whose per-scope
//! stores are pinned by `packages/core/scope/tests/scope.spec.ts`.
//!
//! Approach: the type directly. This is memory rather than durability, so
//! there is no file to read back; what a case can observe is exactly what a
//! caller can - what a scope reads, what another scope reads, and what is left
//! after disposal.
//!
//! What is not restated, and why. Upstream's scope *keys*, and its parent
//! chain (a lookup that falls through to an ancestor scope), have no
//! counterpart here, deliberately: an inheriting lookup is how a child ends up
//! acting on a belief its parent established, which is the borrowed-knowledge
//! failure the filesystem observation policy exists to stop one layer up. Its scoped event
//! dispatch is answered structurally by one `EventBus` per session, which
//! `crates/turn/tests/upstream_scoped.rs` already records.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tetanus_core::scoped::ScopedStores;

/// TC-PORT-SCOPED-1: a scope reads what it wrote.
///
/// Upstream: a scope's store is a store.
///
/// Input: values written and read back through one scope.
/// Expected: what was written, and `None` for a key nothing set. The floor
/// every other case stands on.
#[test]
fn a_scope_reads_what_it_wrote() {
    let stores = ScopedStores::new();
    let scope = stores.open("session-a");

    scope.set("root", json!("/srv/project"));
    scope.set("warned", json!(true));

    assert_eq!(scope.get("root"), Some(json!("/srv/project")));
    assert_eq!(scope.get("warned"), Some(json!(true)));
    assert_eq!(scope.get("never-set"), None);
    assert_eq!(scope.keys(), ["root", "warned"]);
}

/// TC-PORT-SCOPED-2: one scope cannot read another's state.
///
/// Upstream keeps per-scope state per scope; here it is the whole point.
///
/// Input: two scopes writing the same key.
/// Expected: each reads its own value, and neither sees the other's. Two
/// sessions in one process must not act on facts they did not establish, and
/// the key is not a parameter of `Scope`'s methods precisely so that reaching
/// across is not something a caller can do by accident.
#[test]
fn one_scope_cannot_read_another() {
    let stores = ScopedStores::new();
    let first = stores.open("session-a");
    let second = stores.open("session-b");

    first.set("root", json!("/srv/first"));
    second.set("root", json!("/srv/second"));

    assert_eq!(first.get("root"), Some(json!("/srv/first")));
    assert_eq!(second.get("root"), Some(json!("/srv/second")));
    assert_eq!(stores.open_scopes(), 2);
}

/// TC-PORT-SCOPED-3: disposing a scope takes its state with it.
///
/// Upstream's scope disposal, restated on the effect handle that is tetanus's
/// equivalent.
///
/// Input: a scope with state, whose handle is dropped, then the same key
/// opened again.
/// Expected: nothing is left, and the reopened scope starts empty. A long-lived
/// process that kept a map per session it ever ran would grow without bound,
/// and a session id that came back would otherwise inherit what a previous run
/// of it believed.
#[test]
fn disposing_a_scope_takes_its_state() {
    let stores = ScopedStores::new();
    let scope = stores.open("session-a");
    scope.set("root", json!("/srv/project"));

    let handle = scope.into_handle();
    drop(handle);

    assert_eq!(stores.open_scopes(), 0);
    assert_eq!(stores.get("session-a", "root"), None);
    let reopened = stores.open("session-a");
    assert_eq!(reopened.keys(), Vec::<String>::new());
}

/// TC-PORT-SCOPED-4: reopening a live scope continues it rather than clearing
/// it.
///
/// Upstream's scopes are created once; this states what a second `open` of the
/// same key does, because nothing stops a caller doing it.
///
/// Input: a scope with state, opened a second time by the same key.
/// Expected: the second view reads what the first wrote. Clearing on reopen
/// would make a holder's first read depend on whether anybody had opened the
/// scope before, which is a fact no caller can know.
#[test]
fn reopening_a_live_scope_continues_it() {
    let stores = ScopedStores::new();
    let first = stores.open("session-a");
    first.set("root", json!("/srv/project"));

    let second = stores.open("session-a");

    assert_eq!(second.get("root"), Some(json!("/srv/project")));
    assert_eq!(stores.open_scopes(), 1);
}

/// TC-PORT-SCOPED-5: typed reads recompute rather than fail.
///
/// The difference from `tetanus_core::storage`, stated as behaviour.
///
/// Input: a value read as the type it is, and as a type it is not.
/// Expected: the value, then `None`. Everything in a scoped store is
/// recomputable by definition, so a caller meeting a value it cannot use should
/// recompute; a store whose values must be right is the durable one, and that
/// refuses instead.
#[test]
fn a_typed_read_of_the_wrong_type_answers_nothing() {
    let stores = ScopedStores::new();
    let scope = stores.open("session-a");
    scope.write("steps", &7u32);

    assert_eq!(scope.read::<u32>("steps"), Some(7));
    assert_eq!(scope.read::<String>("steps"), None);
    assert_eq!(scope.read::<u32>("absent"), None);
}

/// TC-PORT-SCOPED-6: a value that cannot be serialized is not written, and
/// says so.
///
/// The one caller mistake this type can catch.
///
/// Input: a map whose keys are not strings, which JSON cannot represent.
/// Expected: `false`, and nothing stored. Dropping it silently would produce a
/// read answering `None` for a value the caller believes it wrote, which is the
/// hardest kind of bug to find because both halves look correct.
#[test]
fn a_value_that_cannot_be_serialized_is_refused() {
    use std::collections::BTreeMap;

    let stores = ScopedStores::new();
    let scope = stores.open("session-a");
    let unserializable: BTreeMap<(u8, u8), &str> = BTreeMap::from([((1, 2), "pair")]);

    let wrote = scope.write("pairs", &unserializable);

    assert!(!wrote);
    assert_eq!(scope.get("pairs"), None);
}

/// TC-PORT-SCOPED-7: removing a key leaves the rest of the scope alone.
///
/// Upstream's store is a map.
///
/// Input: three keys, one removed.
/// Expected: the removed key gone with its old value answered, the other two
/// untouched, and removing something absent answering nothing rather than
/// failing.
#[test]
fn removing_a_key_leaves_the_rest_of_the_scope() {
    let stores = ScopedStores::new();
    let scope = stores.open("session-a");
    scope.set("a", json!(1));
    scope.set("b", json!(2));
    scope.set("c", json!(3));

    let removed = scope.remove("b");
    let absent = scope.remove("never-there");

    assert_eq!(removed, Some(json!(2)));
    assert_eq!(absent, None);
    assert_eq!(scope.keys(), ["a", "c"]);
}

/// TC-PORT-SCOPED-8: disposing one scope leaves every other scope standing.
///
/// The property that makes disposal safe to do at all.
///
/// Input: three scopes with state; the middle one disposed.
/// Expected: the other two read exactly what they wrote. A disposal that
/// touched a neighbour would make ending one session corrupt another, which is
/// worse than never freeing anything.
#[test]
fn disposing_one_scope_leaves_the_others_standing() {
    let stores = ScopedStores::new();
    let first = stores.open("session-a");
    let second = stores.open("session-b");
    let third = stores.open("session-c");
    first.set("value", json!("first"));
    second.set("value", json!("second"));
    third.set("value", json!("third"));

    drop(second.into_handle());

    assert_eq!(first.get("value"), Some(json!("first")));
    assert_eq!(third.get("value"), Some(json!("third")));
    assert_eq!(stores.get("session-b", "value"), None);
    assert_eq!(stores.open_scopes(), 2);
}
