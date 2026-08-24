//! Test Design Specification: the observation policy, ported.
//!
//! Feature under test: `tetanus_fs::observation` and the tools that drive it -
//! a session may not overwrite what it has not read, may not edit what it has
//! not read, and may not write back over a file that moved underneath it.
//! Upstream pins the same three decisions in
//! `packages/fs/fs-observation-policy/tests/policy.spec.ts`.
//!
//! Approach: the tools rather than the policy object alone, because the rule is
//! only worth anything if the tools actually consult it. A case that asserted
//! `write_intent` in isolation would pass with the tools wired to nothing.
//!
//! What is not restated, and why. Upstream derives the intent through three
//! `fs/*` waterfall events so a deployment can omit the policy plugin and get
//! unconditional mutation; tetanus makes the policy a value the tool layer
//! holds, and the deployment that wants the bare provider composes
//! `FsTools::unobserved` - which TC-PORT-FS-34 pins as the same behaviour.
//! Upstream keys its state on a `WeakMap` so a collected session frees it; a
//! tetanus owner is a session id, so the release is explicit and
//! TC-PORT-FS-33 pins it instead.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Fixture;
use tetanus_fs::observation::{Observation, ObservedState};
use tetanus_fs::service::WriteIntent;
use tetanus_fs::FsTools;
use tetanus_turn::tools::ToolRegistry;

/// Run one tool by name against a registry, and answer what the model would
/// read.
async fn call(registry: &ToolRegistry, name: &str, arguments: serde_json::Value) -> String {
    let outcome = registry
        .execute(&tetanus_turn::tools::ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments,
        })
        .await
        .expect("the tool answered");
    outcome.content
}

/// Whether the tool refused, as a caller reading the result would decide.
async fn refused(registry: &ToolRegistry, name: &str, arguments: serde_json::Value) -> bool {
    !registry
        .execute(&tetanus_turn::tools::ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments,
        })
        .await
        .expect("the tool answered")
        .ok
}

fn registry(fixture: &Fixture, observed: &Arc<ObservedState>, owner: &str) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    FsTools::new(fixture.sandboxed(), Arc::clone(observed), owner).register(&mut registry);
    registry
}

/// TC-PORT-FS-29: writing a file the session has not read is refused.
///
/// Upstream: "unseen ⇒ createIfAbsent", which an existing file fails.
///
/// Input: a write to an existing file, with no read before it.
/// Expected: refused as `FS_NOT_OBSERVED`, the file untouched, and the message
/// says to read it first. This is the case the whole policy exists for: the
/// model would otherwise have replaced content nobody looked at.
#[tokio::test]
async fn a_write_to_an_unread_file_is_refused_and_says_to_read_it_first() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let observed = Arc::new(ObservedState::new());
    let tools = registry(&fixture, &observed, "session-a");

    let answer = call(
        &tools,
        "write",
        json!({ "path": "kept.txt", "content": "clobbered\n" }),
    )
    .await;

    assert!(answer.starts_with("FS_NOT_OBSERVED"), "{answer}");
    assert!(answer.contains("Read it first"), "{answer}");
    assert_eq!(fixture.read("kept.txt"), "original\n");
}

/// TC-PORT-FS-30: writing a file the session has read goes through.
///
/// Upstream: "confirmed present ⇒ replaceIfVersion at the observed version".
///
/// Input: a read, then a write of the same path.
/// Expected: the write lands. The guard is a precondition the read satisfied,
/// not a prohibition on writing.
#[tokio::test]
async fn a_write_after_a_read_lands() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let observed = Arc::new(ObservedState::new());
    let tools = registry(&fixture, &observed, "session-a");

    call(&tools, "read", json!({ "path": "kept.txt" })).await;
    let answer = call(
        &tools,
        "write",
        json!({ "path": "kept.txt", "content": "revised\n" }),
    )
    .await;

    assert!(answer.starts_with("updated kept.txt"), "{answer}");
    assert_eq!(fixture.read("kept.txt"), "revised\n");
}

/// TC-PORT-FS-31: a file that changed after it was read is not written over.
///
/// Upstream: the observed version is the CAS basis.
///
/// Input: a read, an outside change, then a write.
/// Expected: `FS_STALE_VERSION`, the outside change intact, and a message
/// telling the model to read it again. Without this, a model that thought for
/// three steps reverts whatever landed while it was thinking.
#[tokio::test]
async fn a_file_that_moved_after_the_read_is_not_written_over() {
    let fixture = Fixture::new();
    fixture.write("shared.txt", "v1\n");
    let observed = Arc::new(ObservedState::new());
    let tools = registry(&fixture, &observed, "session-a");

    call(&tools, "read", json!({ "path": "shared.txt" })).await;
    fixture.write("shared.txt", "somebody else's work\n");
    let answer = call(
        &tools,
        "write",
        json!({ "path": "shared.txt", "content": "v2\n" }),
    )
    .await;

    assert!(answer.starts_with("FS_STALE_VERSION"), "{answer}");
    assert!(answer.contains("Read it again"), "{answer}");
    assert_eq!(fixture.read("shared.txt"), "somebody else's work\n");
}

/// TC-PORT-FS-32: an edit of an unread file is refused, and a confirmed
/// absence is refused differently.
///
/// Upstream: "unseen rejects with `FS_NOT_OBSERVED`, confirmed absence rejects
/// with `FS_NOT_FOUND`".
///
/// Input: an edit with no prior read; then a stat of a missing path followed by
/// an edit of it.
/// Expected: the two distinct classes. They differ because the useful next move
/// differs: read the file, versus create it.
#[tokio::test]
async fn an_edit_needs_a_read_and_a_confirmed_absence_says_so_differently() {
    let fixture = Fixture::new();
    fixture.write("code.rs", "let x = 1;\n");
    let observed = Arc::new(ObservedState::new());
    let tools = registry(&fixture, &observed, "session-a");
    let edit = |path: &str| json!({ "path": path, "old_string": "let x = 1;", "new_string": "let x = 2;" });

    let unseen = call(&tools, "edit", edit("code.rs")).await;
    call(&tools, "stat", json!({ "path": "missing.rs" })).await;
    let absent = call(&tools, "edit", edit("missing.rs")).await;

    assert!(unseen.starts_with("FS_NOT_OBSERVED"), "{unseen}");
    assert!(absent.starts_with("FS_NOT_FOUND"), "{absent}");
    assert_eq!(fixture.read("code.rs"), "let x = 1;\n");
}

/// TC-PORT-FS-33: one session's reads do not authorize another's writes, and a
/// forgotten session keeps none of them.
///
/// Upstream: state is keyed per owner in a `WeakMap`, so a collected session
/// frees its state.
///
/// Input: session A reads a file; session B writes it; then A's state is
/// dropped and A writes it.
/// Expected: B is refused, and A is refused after `forget`. Borrowed knowledge
/// is exactly the blind write the policy exists to stop, and a resumed session
/// id inheriting observations made before it was resumed would be the same
/// mistake with a longer fuse.
#[tokio::test]
async fn observations_belong_to_one_session_and_are_dropped_with_it() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let observed = Arc::new(ObservedState::new());
    let session_a = registry(&fixture, &observed, "session-a");
    let session_b = registry(&fixture, &observed, "session-b");

    call(&session_a, "read", json!({ "path": "kept.txt" })).await;
    let borrowed = refused(
        &session_b,
        "write",
        json!({ "path": "kept.txt", "content": "from b\n" }),
    )
    .await;
    observed.forget("session-a");
    let forgotten = refused(
        &session_a,
        "write",
        json!({ "path": "kept.txt", "content": "from a\n" }),
    )
    .await;

    assert!(borrowed, "session B never read the file");
    assert!(forgotten, "session A's observations were dropped");
    assert_eq!(fixture.read("kept.txt"), "original\n");
}

/// TC-PORT-FS-34: composed without the policy, mutation is unconditional.
///
/// Upstream: "without this plugin, tools retain the bare provider's
/// unconditional mutation behavior".
///
/// Input: the same unread write and unread edit, through `FsTools::unobserved`.
/// Expected: both go through. The rule is a composition, not a property of the
/// backend, and a deployment that opts out gets exactly upstream's bare
/// behaviour rather than a half-applied guard.
#[tokio::test]
async fn without_the_policy_a_write_and_an_edit_are_unconditional() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let mut registry = ToolRegistry::new();
    FsTools::unobserved(fixture.sandboxed(), "session-a").register(&mut registry);

    let wrote = call(
        &registry,
        "write",
        json!({ "path": "kept.txt", "content": "let x = 1;\n" }),
    )
    .await;
    let edited = call(
        &registry,
        "edit",
        json!({ "path": "kept.txt", "old_string": "1", "new_string": "2" }),
    )
    .await;

    assert!(wrote.starts_with("updated"), "{wrote}");
    assert!(edited.starts_with("edited"), "{edited}");
    assert_eq!(fixture.read("kept.txt"), "let x = 2;\n");
}

/// TC-PORT-FS-35: a read that found nothing authorizes a create.
///
/// Upstream: a confirmed absence is recorded, and `createIfAbsent` follows from
/// it as it does from never having looked.
///
/// Input: a read of a missing path, then a write to it.
/// Expected: the read refuses with `FS_NOT_FOUND`, the write then creates the
/// file. A model that checked whether a file existed before writing it has done
/// the right thing and must not be punished for it.
#[tokio::test]
async fn a_read_that_found_nothing_still_authorizes_the_create() {
    let fixture = Fixture::new();
    let observed = Arc::new(ObservedState::new());
    let tools = registry(&fixture, &observed, "session-a");

    let missing = call(&tools, "read", json!({ "path": "new.txt" })).await;
    let created = call(
        &tools,
        "write",
        json!({ "path": "new.txt", "content": "fresh\n" }),
    )
    .await;

    assert!(missing.starts_with("FS_NOT_FOUND"), "{missing}");
    assert!(created.starts_with("created new.txt"), "{created}");
    assert_eq!(fixture.read("new.txt"), "fresh\n");
}

/// TC-PORT-FS-36: the intents the policy derives, stated directly.
///
/// Upstream: the three-way derivation is the policy's whole surface.
///
/// Input: the three states - unseen, confirmed absent, confirmed present -
/// asked for a write intent.
/// Expected: create-if-absent, create-if-absent, replace-at-that-version. This
/// is the one case that reads the derivation itself rather than through a tool,
/// so a future tool that forgets to consult it fails the others and not this
/// one - which is how a reader tells a wiring bug from a policy bug.
#[test]
fn the_policy_derives_one_intent_per_state() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("kept.txt").expect("resolve");
    let (_, version) = fs.read(&target).expect("read");
    let observed = ObservedState::new();

    let unseen = observed.write_intent("session-a", &target);
    observed.observe("session-a", &target, Observation::Absent);
    let absent = observed.write_intent("session-a", &target);
    observed.observe("session-a", &target, Observation::Present(version.clone()));
    let present = observed.write_intent("session-a", &target);

    assert_eq!(unseen, WriteIntent::CreateIfAbsent);
    assert_eq!(absent, WriteIntent::CreateIfAbsent);
    assert_eq!(present, WriteIntent::ReplaceIfVersion(version));
}
