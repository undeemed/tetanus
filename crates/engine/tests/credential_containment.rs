//! Test Design Specification: a credential set through the store never
//! appears in an artifact.
//!
//! Feature under test: the containment claim the credential store exists for,
//! stated end to end rather than per component. `crates/config` proves the
//! store's own rules (TC-PORT-CRED-1..12); this proves the consequence a user
//! actually cares about - that the secret is in none of the things a harness
//! produces and a person shares.
//!
//! Approach: set a credential with a value chosen to be unmistakable, run a
//! real turn, then read back every artifact that run produced - the resolved
//! configuration dump, the session journal on disk, the events the boundary
//! serves, and the store's own descriptions - and grep each one for the value.
//! Greping is the right instrument here precisely because it is
//! indiscriminate: a typed assertion would only check the fields someone
//! thought of.
//!
//! The negative half is guarded: each case also asserts the secret is
//! genuinely resolvable, so a bug that stored nothing at all would fail rather
//! than pass every containment claim by vacuity.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_config::credentials::Credentials;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, Engine, SessionCreateParams, SessionEventsParams,
};

/// A value no other part of the workspace could produce by chance, so a hit
/// anywhere is this credential and not a coincidence.
const SECRET: &str = "sk-tetanus-canary-8f3a1d2b-never-print-me";

/// The reference it is stored under: the real one an adapter resolves, so the
/// case covers the credential the product actually has.
const REFERENCE: &str = "DEEPSEEK_API_KEY";

fn engine_over(dir: &TempDir) -> HarnessEngine {
    HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    })
}

/// Every file under a directory, as text, for a search that cannot be fooled
/// by which file someone remembered to check.
fn all_text_under(root: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                found.push((path, text));
            }
        }
    }
    found
}

/// TC-PORT-CRED-13: a stored credential is in no artifact a run produces.
///
/// The acceptance claim. A secret set through the store must not appear in
/// `config.dump`, in the journal on disk, or in the events the boundary
/// serves - and it must still be resolvable, or the case proves nothing.
///
/// Expected: the value is resolvable; and no dump entry, no journal byte and
/// no served event contains it.
#[tokio::test]
async fn a_stored_credential_reaches_no_artifact_of_a_run() {
    let home = TempDir::new().expect("temp dir");
    let sessions = TempDir::new().expect("temp dir");

    let store = Credentials::under(home.path());
    store.set(REFERENCE, SECRET).expect("store the credential");

    let engine = engine_over(&sessions);
    engine
        .session_create(SessionCreateParams {
            session_id: Some("canary".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    engine
        .agent_prompt(AgentPromptParams {
            session_id: "canary".into(),
            content: "run one full turn".into(),
        })
        .await
        .expect("a turn");

    // The guard: a store that silently held nothing would pass every claim
    // below without the store working at all.
    assert_eq!(
        store
            .resolve(REFERENCE)
            .unwrap()
            .expect("resolvable")
            .expose(),
        SECRET,
        "the credential is genuinely stored, so the containment claims mean something"
    );

    // 1. The resolved configuration a surface may publish.
    let dump = engine.config_dump().await.expect("dump");
    let rendered = serde_json::to_string(&dump).expect("serialize");
    assert!(
        !rendered.contains(SECRET),
        "the credential reached config.dump: {rendered}"
    );
    assert!(
        !dump.entries.iter().any(|entry| entry.key == REFERENCE),
        "the credential is not even a key in the resolved configuration"
    );

    // 2. Every byte of every journal the run wrote.
    for (path, text) in all_text_under(sessions.path()) {
        assert!(
            !text.contains(SECRET),
            "the credential reached {}",
            path.display()
        );
    }

    // 3. The events the boundary serves to whoever is on the carrier.
    let events = engine
        .session_events(SessionEventsParams {
            session_id: "canary".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");
    let served = serde_json::to_string(&events).expect("serialize");
    assert!(
        !served.contains(SECRET),
        "the credential reached session.events"
    );
    assert!(
        events.events.len() > 5,
        "the turn really ran, so the search had something to search"
    );
}

/// TC-PORT-CRED-14: the credential's own file is the only place it is.
///
/// The complement of the last case: having proved the secret is nowhere a run
/// puts things, prove it *is* where the store says it is - otherwise "nowhere"
/// could mean the write never happened.
///
/// Expected: exactly one file under the harness home contains the value, and
/// it is the credentials document.
#[test]
fn the_credentials_document_is_the_only_file_holding_it() {
    let home = TempDir::new().expect("temp dir");
    let store = Credentials::under(home.path());
    store.set(REFERENCE, SECRET).expect("store");

    let holding: Vec<_> = all_text_under(home.path())
        .into_iter()
        .filter(|(_, text)| text.contains(SECRET))
        .map(|(path, _)| path)
        .collect();

    assert_eq!(holding.len(), 1, "found in {holding:?}");
    assert_eq!(holding[0], store.path());
}

/// TC-PORT-CRED-15: describing every reference never renders a value.
///
/// A settings page lists what is configured. That listing is the surface most
/// likely to be screenshotted into an issue, so it is asserted separately from
/// the dump.
///
/// Expected: the listing names the reference, reports it configured, and
/// contains no part of the value.
#[test]
fn a_listing_of_references_never_renders_a_value() {
    let home = TempDir::new().expect("temp dir");
    let store = Credentials::under(home.path());
    store.set(REFERENCE, SECRET).expect("store");

    let listing: Vec<_> = store
        .references()
        .expect("references")
        .into_iter()
        .map(|reference| {
            let described = store.describe(&reference).expect("describe");
            (reference, described)
        })
        .collect();

    let rendered = serde_json::to_string(&listing).expect("serialize");
    assert!(rendered.contains(REFERENCE), "the reference is listed");
    assert!(listing[0].1.configured);
    assert!(
        !rendered.contains(SECRET),
        "the listing carried the value: {rendered}"
    );
    // Not even a prefix of it: a partially rendered secret is still a leak.
    assert!(!rendered.contains("sk-tetanus"), "{rendered}");
}
