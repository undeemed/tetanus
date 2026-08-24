//! Test Design Specification: the spill store and its policy, ported.
//!
//! Features under test: `tetanus_core::spill` - where an oversized payload
//! goes, and the bounded replacement the model reads instead. Upstream pins
//! the same behaviour in `packages/spill/spill-local/tests/spill-local.spec.ts`
//! and `spill-policy/tests/spill-policy.spec.ts`.
//!
//! Approach: a spill root in a temporary directory, driven through the public
//! seam. The cap is stated in bytes in every case, because that is the unit
//! the policy is about and the unit the hazards live in.
//!
//! What is not restated, and why. Upstream's policy is a `tools/post-execute`
//! listener that composes with other listeners through `next()`; tetanus has
//! no post-execute projection seam yet, so the decision and the storage are
//! published for the pipeline to call and the composition cases have nothing
//! to restate. Its content-block handling - leaving a result carrying any
//! non-text block untouched - is unrepresentable: a tetanus tool result
//! carries a `String`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_core::spill::{SpillPolicy, SpillSource, SpillStore};

fn source() -> SpillSource {
    SpillSource {
        session_id: "s-1".into(),
        tool: "read".into(),
        call_id: "call-1".into(),
    }
}

/// TC-PORT-SPILL-1: a payload within the cap is left exactly alone.
///
/// Expected: no decision, and nothing written - a policy that filed away every
/// result would cost a write per tool call for no benefit.
#[test]
fn a_payload_within_the_cap_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());
    let policy = SpillPolicy {
        max_inline_bytes: 1000,
    };

    assert!(policy.apply(&store, &source(), "short output").is_none());
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "nothing was written"
    );
}

/// TC-PORT-SPILL-2: an oversized payload is stored whole and replaced bounded.
///
/// Upstream: `spill-policy.spec.ts`, "spills an oversized result and replaces
/// it with a preview plus the locator".
///
/// Expected: the file holds the original exactly; the replacement is inside
/// the cap, names the locator, and carries both ends of the content.
#[test]
fn an_oversized_payload_is_stored_whole_and_replaced_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());
    let policy = SpillPolicy {
        max_inline_bytes: 400,
    };
    let content = format!("HEAD{}TAIL", "x".repeat(5_000));

    let spilled = policy
        .apply(&store, &source(), &content)
        .expect("over the cap");

    assert_eq!(
        std::fs::read_to_string(&spilled.reference.locator).unwrap(),
        content
    );
    assert_eq!(spilled.reference.bytes, content.len());
    assert!(
        spilled.replacement.len() <= 400,
        "the replacement is {} bytes",
        spilled.replacement.len()
    );
    assert!(spilled.replacement.contains(&spilled.reference.locator));
    assert!(spilled.replacement.starts_with("HEAD"));
    assert!(spilled.replacement.contains("TAIL"));
}

/// TC-PORT-SPILL-3: the replacement never exceeds the cap, at any size.
///
/// Upstream reserves the notice's cost inside the cap before cutting the
/// preview, for the defect this case is built to catch: a naive policy spends
/// the whole budget on the preview and then appends the notice, so for a
/// marginally over-cap payload the replacement comes out *larger than the
/// original* - the one outcome a size policy must never have.
///
/// Expected: over a spread of caps and lengths, every replacement is inside
/// its cap and smaller than what it replaced.
#[test]
fn a_replacement_is_never_larger_than_its_cap_or_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());

    for cap in [200_usize, 256, 400, 1000, 4096] {
        for over in [1_usize, 2, 17, 500, 10_000] {
            let content = "y".repeat(cap + over);
            let policy = SpillPolicy {
                max_inline_bytes: cap,
            };
            let Some(spilled) = policy.apply(&store, &source(), &content) else {
                // Declining is allowed; serving something over the cap is not.
                continue;
            };
            assert!(
                spilled.replacement.len() <= cap,
                "cap {cap}, input {}: replacement {}",
                content.len(),
                spilled.replacement.len()
            );
            assert!(
                spilled.replacement.len() < content.len(),
                "cap {cap}: the replacement did not shrink anything"
            );
        }
    }
}

/// TC-PORT-SPILL-4: a cap too small for a notice keeps the original.
///
/// Upstream: `spill-policy.spec.ts`, "keeps the inline content when the notice
/// alone exceeds maxInlineBytes". Serving a truncated locator would be worse
/// than serving the content, because the content is at least usable.
///
/// Expected: no decision, whatever the payload's size.
#[test]
fn a_cap_too_small_for_a_notice_keeps_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());
    let policy = SpillPolicy {
        max_inline_bytes: 8,
    };

    assert!(policy
        .apply(&store, &source(), &"z".repeat(9_000))
        .is_none());
}

/// TC-PORT-SPILL-5: a preview never splits a character.
///
/// The budget is in bytes because that is what a size cap means, but a cut at
/// a byte offset lands mid-character routinely. A preview that is not text is
/// not a preview - and in Rust the naive version does not merely look wrong,
/// it panics.
///
/// Expected: the replacement is valid text at every cap across a multi-byte
/// payload, and still inside its cap.
#[test]
fn a_preview_never_splits_a_character() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());
    // Three bytes a character, so almost every byte offset is mid-character.
    let content = "\u{4f60}\u{597d}".repeat(3_000);

    for cap in [200_usize, 201, 202, 203, 300, 512, 1023] {
        let policy = SpillPolicy {
            max_inline_bytes: cap,
        };
        let Some(spilled) = policy.apply(&store, &source(), &content) else {
            continue;
        };
        assert!(spilled.replacement.len() <= cap, "cap {cap}");
        // Reaching here at all means no cut panicked; this states the rest.
        assert!(
            spilled.replacement.chars().count() > 0,
            "cap {cap}: the preview is text"
        );
    }
}

/// TC-PORT-SPILL-6: artifacts are scoped to their session and named safely.
///
/// A session id, a tool name and a call id are all untrusted to some degree -
/// a call id is minted by a model - so a name that traverses must not reach
/// the filesystem.
///
/// Expected: the artifact lands under the spill root, in a directory named for
/// its session, whatever the source fields say.
#[test]
fn artifacts_are_scoped_to_their_session_and_named_safely() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());

    let hostile = SpillSource {
        session_id: "../../escape".into(),
        tool: "/etc/passwd".into(),
        call_id: "..".into(),
    };
    let saved = store.save(&hostile, "content").unwrap();

    let path = std::path::Path::new(&saved.locator);
    assert!(path.exists());
    // Containment is stated after resolution, which is the only form of it
    // that means anything: a `..` left as literal characters inside one
    // segment cannot traverse, and asserting on the spelling would fail on a
    // safe name while passing a crafted one that resolved out.
    assert!(
        path.canonicalize()
            .unwrap()
            .starts_with(dir.path().canonicalize().unwrap()),
        "escaped the spill root: {}",
        saved.locator
    );
    // And no component is a traversal token in its own right, which is what
    // the encoding exists to prevent.
    assert!(
        !path
            .components()
            .any(|part| part.as_os_str() == ".." || part.as_os_str() == "."),
        "a traversal component reached the path: {}",
        saved.locator
    );
}

/// TC-PORT-SPILL-7: two results of one call do not overwrite each other.
///
/// Expected: two distinct paths, each holding its own content.
#[test]
fn two_results_of_one_call_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());

    let first = store.save(&source(), "the first").unwrap();
    let second = store.save(&source(), "the second").unwrap();

    assert_ne!(first.locator, second.locator);
    assert_eq!(
        std::fs::read_to_string(&first.locator).unwrap(),
        "the first"
    );
    assert_eq!(
        std::fs::read_to_string(&second.locator).unwrap(),
        "the second"
    );
}

/// TC-PORT-SPILL-8: a storage failure keeps the inline content.
///
/// Upstream: `spill-policy.spec.ts`, "keeps the inline content when saveText
/// fails". A successful tool call must never become an error, or lose its
/// output, because the harness could not file it away.
///
/// Input: a spill root that is a regular file, so every write under it fails.
/// Expected: no decision, and no panic.
#[test]
fn a_storage_failure_keeps_the_inline_content() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, b"in the way").unwrap();
    let store = SpillStore::at(&blocked);
    let policy = SpillPolicy {
        max_inline_bytes: 100,
    };

    assert!(policy
        .apply(&store, &source(), &"q".repeat(5_000))
        .is_none());
}

/// TC-PORT-SPILL-9: a spilled artifact is owner-only.
///
/// Spilled output is whatever a tool read, which routinely includes source,
/// configuration and output that was never meant to leave the process. A
/// world-readable copy in a shared root is a leak the original never was.
///
/// Expected: mode `0600`.
#[cfg(unix)]
#[test]
fn a_spilled_artifact_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let store = SpillStore::at(dir.path());
    let saved = store.save(&source(), "sensitive output").unwrap();

    let mode = std::fs::metadata(&saved.locator)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "got {mode:o}");
}
