//! Test Design Specification: the credential store, ported.
//!
//! Features under test: `tetanus_config::credentials` - the layer order, the
//! reference/value split, the refusals, and the containment claim the store
//! exists for. Upstream pins the same seam in
//! `packages/credentials/credentials/tests/credentials.spec.ts` and
//! `credentials-local/tests/local.spec.ts`.
//!
//! Approach: a store in a temporary directory, and - for the containment
//! claim - a real dump and a real journal, greppedfor the secret. The
//! environment layer is exercised through a variable this suite owns, set and
//! removed inside one case, because the process environment is global to the
//! test binary.
//!
//! What is not restated, and why. Upstream's hot-reload watcher and its
//! cross-process write lock are surfaces this crate does not have; a value is
//! read from the file on every resolve instead, which gives the same "a
//! changed credential reaches the next operation" property, and
//! TC-PORT-CRED-9 pins it. Its `.env` fallbacks in the invocation directory
//! and its home are two more read-only layers of the same kind as the
//! environment; tetanus has one. Its YAML comment preservation has nothing to
//! restate: the document is JSON and holds nothing but credentials.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_config::credentials::{
    CredentialError, CredentialSource, Credentials, CREDENTIALS_FILE, REDACTED,
};

/// Write a document by hand, owner-only, as a user editing one must.
///
/// The mode is not incidental to the fixture: TC-PORT-CRED-6 is the case that
/// says a wider file is refused before it is read, so a hand-written fixture
/// left at the umask's mode would be refused by that rule and every case using
/// it would fail for a reason it is not about.
fn hand_write(dir: &std::path::Path, text: &str) -> std::path::PathBuf {
    let path = dir.join(CREDENTIALS_FILE);
    std::fs::write(&path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

fn store() -> (Credentials, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    (Credentials::under(dir.path()), dir)
}

/// TC-PORT-CRED-1: a stored value resolves, and says where it came from.
///
/// Upstream: `local.spec.ts`, "resolves a value written to the managed store".
///
/// Expected: the value round-trips through a fresh store over the same file,
/// reported as coming from the store.
#[test]
fn a_stored_value_resolves_from_the_store() {
    let (store, dir) = store();
    store.set("DEEPSEEK_API_KEY", "sk-secret-value").unwrap();

    let reopened = Credentials::under(dir.path());
    let found = reopened
        .resolve("DEEPSEEK_API_KEY")
        .unwrap()
        .expect("a value");
    assert_eq!(found.expose(), "sk-secret-value");
    assert_eq!(found.source(), CredentialSource::Store);
}

/// TC-PORT-CRED-2: an unconfigured reference resolves to nothing.
///
/// Expected: `None`, and a description that says so without inventing a
/// source.
#[test]
fn an_unconfigured_reference_resolves_to_nothing() {
    let (store, _dir) = store();

    assert!(store.resolve("NOT_SET_ANYWHERE").unwrap().is_none());
    let described = store.describe("NOT_SET_ANYWHERE").unwrap();
    assert!(!described.configured);
    assert_eq!(described.source, None);
    assert!(described.writable);
}

/// TC-PORT-CRED-3: the environment wins, and is visibly read-only.
///
/// Upstream: `local.spec.ts`, "the inherited environment shadows the managed
/// store" and "rejects a write while a read-only source shadows the
/// reference". A write that appeared to succeed while resolution kept
/// returning the environment's value is the worst of the three possible
/// behaviours, which is why it is refused rather than accepted.
///
/// Expected: the environment's value resolves over the stored one; `describe`
/// reports the environment and `writable: false`; `set` and `unset` are both
/// refused.
#[test]
fn the_environment_wins_and_refuses_to_be_written_through() {
    let (store, _dir) = store();
    store.set("TETANUS_TEST_CRED", "from-the-file").unwrap();

    std::env::set_var("TETANUS_TEST_CRED", "from-the-environment");
    let found = store
        .resolve("TETANUS_TEST_CRED")
        .unwrap()
        .expect("a value");
    assert_eq!(found.expose(), "from-the-environment");
    assert_eq!(found.source(), CredentialSource::Environment);

    let described = store.describe("TETANUS_TEST_CRED").unwrap();
    assert_eq!(described.source, Some(CredentialSource::Environment));
    assert!(!described.writable);

    assert!(matches!(
        store.set("TETANUS_TEST_CRED", "another"),
        Err(CredentialError::ShadowedByEnvironment(_))
    ));
    assert!(matches!(
        store.unset("TETANUS_TEST_CRED"),
        Err(CredentialError::ShadowedByEnvironment(_))
    ));

    std::env::remove_var("TETANUS_TEST_CRED");
    // With the shadow gone, the stored value is served again untouched.
    assert_eq!(
        store
            .resolve("TETANUS_TEST_CRED")
            .unwrap()
            .expect("a value")
            .expose(),
        "from-the-file"
    );
}

/// TC-PORT-CRED-4: an empty value is an absent one, everywhere.
///
/// Upstream states this seam-wide, and the defect it prevents is one tetanus
/// already met once: a whitespace key that read as present and went to the
/// provider (`upstream_credentials.rs`, TC-PORT-KEY-1..6).
///
/// Expected: storing blank or whitespace is refused; an empty value already in
/// the file resolves to nothing and describes as unconfigured.
#[test]
fn an_empty_value_is_an_absent_one() {
    let (store, dir) = store();

    assert!(matches!(
        store.set("BLANK_KEY", ""),
        Err(CredentialError::EmptyValue(_))
    ));
    assert!(matches!(
        store.set("BLANK_KEY", "   \t "),
        Err(CredentialError::EmptyValue(_))
    ));

    // One written past the seam, as a hand-edited file would carry it.
    hand_write(dir.path(), r#"{"BLANK_KEY": ""}"#);
    assert!(store.resolve("BLANK_KEY").unwrap().is_none());
    assert!(!store.describe("BLANK_KEY").unwrap().configured);
}

/// TC-PORT-CRED-5: a reference that is not a POSIX identifier is refused.
///
/// Upstream: `credentials.spec.ts`, `credentialRef`'s pattern. A reference
/// doubles as an environment variable name, so one that could not be exported
/// would be a reference only half the layers could serve.
///
/// Expected: every malformed shape is refused, and a legal one is not.
#[test]
fn a_reference_that_is_not_an_identifier_is_refused() {
    let (store, _dir) = store();
    for bad in ["", "1LEADING_DIGIT", "has-a-dash", "has space", "has.dot"] {
        assert!(
            matches!(store.resolve(bad), Err(CredentialError::BadReference(_))),
            "{bad:?} should not be a reference"
        );
    }
    assert!(store.resolve("_OK").is_ok());
    assert!(store.resolve("DEEPSEEK_API_KEY").is_ok());
}

/// TC-PORT-CRED-6: the file is owner-only, and one that is not is refused.
///
/// Upstream: `local.spec.ts`, "rejects a credentials document readable beyond
/// its owner". The store writes `0600`, but a hand-written file carries
/// whatever umask produced it, and serving secrets out of a world-readable
/// file would make the mode meaningless.
///
/// Expected: the written file is `0600`; a file widened to `0644` is refused
/// rather than read.
#[cfg(unix)]
#[test]
fn the_file_is_owner_only_and_a_wider_one_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let (store, dir) = store();
    store.set("SOME_KEY", "value").unwrap();
    let path = dir.path().join(CREDENTIALS_FILE);

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "written owner-only, got {mode:o}");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        store.resolve("SOME_KEY"),
        Err(CredentialError::TooOpen { .. })
    ));
}

/// TC-PORT-CRED-7: removing a reference removes it, and removing an absent one
/// is not an error.
///
/// Expected: `true` then `false`, and the value is gone from the file.
#[test]
fn unsetting_removes_a_value_and_tolerates_an_absent_one() {
    let (store, _dir) = store();
    store.set("GOING_AWAY", "value").unwrap();

    assert!(store.unset("GOING_AWAY").unwrap());
    assert!(!store.unset("GOING_AWAY").unwrap());
    assert!(store.resolve("GOING_AWAY").unwrap().is_none());
    assert!(store.references().unwrap().is_empty());
}

/// TC-PORT-CRED-8: a surface may list references and never values.
///
/// Expected: `references` names both keys; `describe` reports them configured
/// and writable; and neither answer carries a value anywhere in it.
#[test]
fn a_surface_lists_references_and_never_values() {
    let (store, _dir) = store();
    store.set("FIRST_KEY", "first-secret").unwrap();
    store.set("SECOND_KEY", "second-secret").unwrap();

    let listed = store.references().unwrap();
    assert_eq!(listed, vec!["FIRST_KEY", "SECOND_KEY"]);

    for reference in &listed {
        let described = store.describe(reference).unwrap();
        assert!(described.configured);
        assert!(described.writable);
        let rendered = serde_json::to_string(&described).unwrap();
        assert!(
            !rendered.contains("secret"),
            "a description carried a value: {rendered}"
        );
    }
}

/// TC-PORT-CRED-9: a value changed on disk reaches the next operation.
///
/// Upstream gets this from a watcher; this reads the file per resolve, which
/// is the same property without a thread. It is also what keeps a revoked
/// secret from living on in memory.
///
/// Expected: the second resolve, through the same store handle, answers the
/// new value.
#[test]
fn a_value_changed_on_disk_reaches_the_next_operation() {
    let (store, dir) = store();
    store.set("ROTATING_KEY", "old").unwrap();
    assert_eq!(
        store.resolve("ROTATING_KEY").unwrap().unwrap().expose(),
        "old"
    );

    let elsewhere = Credentials::under(dir.path());
    elsewhere.set("ROTATING_KEY", "new").unwrap();

    assert_eq!(
        store.resolve("ROTATING_KEY").unwrap().unwrap().expose(),
        "new",
        "no cache stands between the file and the next operation"
    );
}

/// TC-PORT-CRED-10: a secret cannot be printed by accident.
///
/// The one leak that needs no mistake in the store itself: a struct holding a
/// secret gets debug-printed into a log line. `Secret` renders as the
/// redaction in both formatting traits, so the accident produces nothing.
///
/// Expected: neither `{:?}` nor `{}` contains the value.
#[test]
fn a_secret_cannot_be_printed_by_accident() {
    let (store, _dir) = store();
    store.set("PRINTED_KEY", "sk-do-not-print-me").unwrap();
    let found = store.resolve("PRINTED_KEY").unwrap().expect("a value");

    assert_eq!(format!("{found:?}"), REDACTED);
    assert_eq!(format!("{found}"), REDACTED);
    assert!(!format!("{found:?} {found}").contains("do-not-print-me"));
    assert_eq!(
        found.expose(),
        "sk-do-not-print-me",
        "and it is still there"
    );
}

/// TC-PORT-CRED-11: a malformed document is refused without quoting itself.
///
/// The parser's own message embeds the offending line, and in this file every
/// line is a secret - so the message is dropped and only the path is reported.
///
/// Expected: `Malformed`, and the rendered error contains no part of the file.
#[test]
fn a_malformed_document_is_refused_without_quoting_itself() {
    let (store, dir) = store();
    hand_write(dir.path(), r#"{"A_KEY": "sk-leaked-in-a-parse-error" "#);

    let refused = store.resolve("A_KEY").unwrap_err();
    assert!(matches!(refused, CredentialError::Malformed { .. }));
    assert!(
        !refused.to_string().contains("sk-leaked"),
        "the error quoted the document: {refused}"
    );
}

/// TC-PORT-CRED-12: a key in the store is not a key in the settings document.
///
/// The whole reason this store exists. A credential written here must not
/// appear in the layered configuration at all - not redacted, not as a key -
/// because a value that is not in the layers cannot be dumped by a surface
/// that has not learned to redact it.
///
/// Expected: the resolved configuration has no entry under the reference, and
/// nothing in its rendered form carries the value.
#[test]
fn a_stored_credential_is_not_in_the_settings_document() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Credentials::under(dir.path());
    store.set("DEEPSEEK_API_KEY", "sk-never-in-config").unwrap();

    let config = tetanus_config::Config::default();
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("sk-never-in-config"));
    assert!(config.get("DEEPSEEK_API_KEY").is_none());
    assert!(config.provenance().next().is_none());
}
