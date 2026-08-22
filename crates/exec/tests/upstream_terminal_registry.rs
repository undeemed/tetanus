//! Test Design Specification: the owner-scoped terminal registry, ported.
//!
//! Feature under test: `tetanus_exec::terminals` - minting ids, publishing a
//! session only once its shell answers, which owner may touch which session,
//! names inside one owner, listing, and closing. Upstream pins the same
//! decisions in `packages/terminal/terminal/tests/service.spec.ts`
//! (`TerminalSessionService`).
//!
//! Approach: the real registry over real terminals, because every claim here
//! is about a live session - an id that outlives two tool calls, a close that
//! waits for the shell to be gone, a name that is taken while another open is
//! still starting. The two owners are two [`Owner`] values, which is what
//! stands in for upstream's exact-`Agent` comparison until tetanus sessions
//! have an agent identity; the comparison the registry makes is the same one.
//!
//! What upstream has here that this does not, and why. Its service disposes
//! sessions when the owning agent's effect scope goes away, and refuses a
//! spawn for an owner that is no longer live: both need an agent lifecycle
//! this workspace has not built, so a composition closes its own registry
//! (`close_all`) on the way down. Its `TerminalBackendCleanupError` and the
//! aggregate rollback around a partially started session have nothing to
//! restate, because an open here either publishes or leaves nothing behind.
//!
//! Environmental needs: Linux with `/dev/ptmx`, a bash on PATH, a writable
//! temp directory. Cases report themselves skipped where a terminal cannot be
//! allocated rather than passing for the wrong reason. No case reaches a
//! network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use tetanus_exec::backend::{Bash, PowerShell};
use tetanus_exec::terminal::{TerminalConfig, TerminalError};
use tetanus_exec::terminals::{Closed, OpenRequest, Owner, Terminals};

/// TC-PORT-TERM-28: an id names one session for as long as it lives, and a
/// list shows an owner what it opened.
///
/// Upstream: "spawn publishes an identity", "list returns owner-visible
/// snapshots in publication order".
///
/// A model opens a terminal in one tool call and types into it in the next, so
/// the id is the whole handle: if it were re-used, or if listing showed them
/// in an order that changed, the model's second call would reach a shell it
/// did not open.
///
/// Input: two sessions opened by one owner, one of them named.
/// Expected: the ids differ, each `get` answers the session that was opened,
/// the list is in the order they were opened, and the name and backend type
/// come back with them.
#[tokio::test]
async fn an_id_names_one_session_and_a_list_shows_what_an_owner_opened() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let terminals = registry(workspace.path());
    let owner = Owner::new("session-a");

    let Some(first) = open(&terminals, &owner, OpenRequest::default()).await else {
        return;
    };
    let second = open(
        &terminals,
        &owner,
        OpenRequest {
            name: Some("builder".into()),
            ..OpenRequest::default()
        },
    )
    .await
    .expect("a second terminal");

    assert_ne!(first.id(), second.id(), "an id is never re-used");
    assert_eq!(first.kind(), "bash");
    assert_eq!(second.name(), Some("builder"));

    let listed: Vec<String> = terminals
        .list(&owner)
        .iter()
        .map(|session| session.id().to_string())
        .collect();
    assert_eq!(
        listed,
        vec![first.id().to_string(), second.id().to_string()],
        "the list is in the order they were opened"
    );
    assert_eq!(
        terminals.get(&owner, second.id()).expect("by id").id(),
        second.id()
    );
    terminals.close_all().await;
}

/// TC-PORT-TERM-29: another owner cannot see, use or close a session, and is
/// told which of the two it is.
///
/// Upstream: `FOREIGN_SESSION` and `NO_SESSION` (`service.spec.ts`, "rejects
/// foreign operations").
///
/// Two owners sharing one registry without this means one of them typing into
/// a shell the other is halfway through using, with neither able to tell. The
/// two errors are kept apart because they are different bugs: reaching for
/// somebody else's session is a wiring mistake, and reaching for one that
/// never existed is a lost id.
///
/// Input: a session opened by one owner, then asked for, listed and closed by
/// another; and an id nobody ever minted.
/// Expected: the foreign owner is refused as foreign on every call and sees an
/// empty list, the unknown id is refused as unknown, and the session is still
/// running afterwards.
#[tokio::test]
async fn another_owner_cannot_reach_a_session_and_is_told_which_it_is() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let terminals = registry(workspace.path());
    let mine = Owner::new("session-a");
    let theirs = Owner::new("session-b");

    let Some(session) = open(&terminals, &mine, OpenRequest::default()).await else {
        return;
    };

    match terminals.get(&theirs, session.id()) {
        Err(TerminalError::Foreign(id)) => assert_eq!(id, session.id()),
        other => panic!("a foreign session must be refused as foreign, got {other:?}"),
    }
    match terminals.kill(&theirs, session.id()).await {
        Err(TerminalError::Foreign(id)) => assert_eq!(id, session.id()),
        other => panic!("a foreign close must be refused, got {other:?}"),
    }
    assert!(
        terminals.list(&theirs).is_empty(),
        "an owner sees only its own sessions"
    );
    match terminals.get(&mine, "pty-never-minted") {
        Err(TerminalError::NoSession(id)) => assert_eq!(id, "pty-never-minted"),
        other => panic!("an unknown id must be refused as unknown, got {other:?}"),
    }

    assert!(
        session.send("echo mine", true, None).await.is_ok(),
        "the session survived being asked for by somebody else"
    );
    terminals.close_all().await;
}

/// TC-PORT-TERM-30: one owner cannot have two sessions by one name, and two
/// owners can.
///
/// Upstream: `DUPLICATE_NAME`, including the reservation that holds while a
/// session is still starting.
///
/// A name is how a model keeps two terminals apart - "build", "server" - so
/// two sessions answering to one name is a model typing into whichever the
/// registry happened to find first. The reservation matters because opening a
/// terminal takes long enough for a second open to arrive while the first is
/// still starting.
///
/// Input: two opens with the same name by one owner, then the same name by a
/// second owner, then the name re-used after the first session is closed.
/// Expected: the second open is refused as a duplicate, the other owner's is
/// allowed, and the name is free again once the session holding it is gone.
#[tokio::test]
async fn a_name_belongs_to_one_session_within_one_owner() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let terminals = registry(workspace.path());
    let mine = Owner::new("session-a");
    let theirs = Owner::new("session-b");
    let named = || OpenRequest {
        name: Some("build".into()),
        ..OpenRequest::default()
    };

    let Some(first) = open(&terminals, &mine, named()).await else {
        return;
    };
    match terminals.open(&mine, named()).await {
        Err(TerminalError::DuplicateName { owner, name }) => {
            assert_eq!(owner, mine.id());
            assert_eq!(name, "build");
        }
        other => panic!("a duplicate name must be refused, got {other:?}"),
    }
    let elsewhere = terminals
        .open(&theirs, named())
        .await
        .expect("another owner's names are its own");

    terminals
        .kill(&mine, first.id())
        .await
        .expect("closed the one holding the name");
    let reused = terminals
        .open(&mine, named())
        .await
        .expect("the name is free once nothing holds it");

    assert_eq!(reused.name(), Some("build"));
    assert_ne!(reused.id(), first.id(), "a fresh session, not the old one");
    assert_ne!(elsewhere.id(), reused.id());
    terminals.close_all().await;
}

/// TC-PORT-TERM-31: closing says whether this call was the one that closed it,
/// and a closed session is forgotten.
///
/// Upstream: `kill` returning true for a newly closed session and false for
/// one already closing.
///
/// A tool reporting "closed" for a session somebody else closed a moment ago
/// is a tool telling the model something it did not do. Forgetting the session
/// afterwards is the other half: a list that still showed it would have the
/// model typing into a shell that is gone.
///
/// Input: a session closed twice, and one closed directly before the registry
/// is asked to.
/// Expected: the first close answers `Now`, a second refuses with the id
/// unknown, the session already closed answers `Already`, and neither is in
/// the list.
#[tokio::test]
async fn closing_says_whether_this_call_did_it_and_the_session_is_forgotten() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let terminals = registry(workspace.path());
    let owner = Owner::new("session-a");

    let Some(session) = open(&terminals, &owner, OpenRequest::default()).await else {
        return;
    };
    assert_eq!(
        terminals.kill(&owner, session.id()).await.expect("closed"),
        Closed::Now
    );
    match terminals.kill(&owner, session.id()).await {
        Err(TerminalError::NoSession(_)) => {}
        other => panic!("a session closed and forgotten has nothing left to close, got {other:?}"),
    }
    assert!(terminals.list(&owner).is_empty());

    let second = terminals
        .open(&owner, OpenRequest::default())
        .await
        .expect("another terminal");
    second.close().await;
    assert_eq!(
        terminals.kill(&owner, second.id()).await.expect("closed"),
        Closed::Already,
        "the registry says who did it"
    );
    terminals.close_all().await;
}

/// TC-PORT-TERM-32: a request names the backend type it wants, and one that
/// names an absent type is refused with what is on offer.
///
/// Upstream: `registerBackend`, `listBackends`, `NO_BACKEND`, and
/// `DUPLICATE_BACKEND`.
///
/// The type is what keeps Windows from being designed out: a deployment
/// registers `pwsh` and every later call is the same call. A refusal that
/// names the registered types is what stops a model retrying the same wrong
/// one, and refusing a duplicate registration is what stops two backends
/// answering to one name with the first-registered silently winning.
///
/// Input: a registry with bash and PowerShell registered; a request for each,
/// one for a type nobody registered, and a second registration of bash.
/// Expected: the types list both in registration order; `bash` opens; the
/// unknown type is refused naming both; the duplicate registration is refused;
/// and asking for `pwsh` on a host without one is refused as a missing binary
/// rather than as a missing backend.
#[tokio::test]
async fn a_request_names_its_backend_type_and_an_absent_one_is_refused() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let terminals = registry(workspace.path());
    terminals
        .register_backend(Arc::new(PowerShell::new()))
        .expect("a second backend");
    let owner = Owner::new("session-a");

    assert_eq!(terminals.backends(), vec!["bash", "pwsh"]);
    match terminals.register_backend(Arc::new(Bash::new())) {
        Err(TerminalError::DuplicateBackend(name)) => assert_eq!(name, "bash"),
        other => panic!("two backends must not share a type, got {other:?}"),
    }

    match terminals
        .open(
            &owner,
            OpenRequest {
                kind: Some("fish".into()),
                ..OpenRequest::default()
            },
        )
        .await
    {
        Err(TerminalError::NoBackend { asked, registered }) => {
            assert_eq!(asked, "fish");
            assert_eq!(registered, vec!["bash".to_string(), "pwsh".to_string()]);
        }
        other => panic!("an unregistered type must be refused, got {other:?}"),
    }

    // A host with no PowerShell is the ordinary case here, and the refusal has
    // to be the backend's own - "this host has no pwsh" - rather than "no such
    // backend", because the two are fixed by different people.
    match terminals
        .open(
            &owner,
            OpenRequest {
                kind: Some("pwsh".into()),
                ..OpenRequest::default()
            },
        )
        .await
    {
        Err(TerminalError::Backend(refused)) => assert!(
            refused.to_string().contains("pwsh"),
            "the refusal should name the program: {refused}"
        ),
        Ok(session) => {
            // A host that does have one: then it opened, and that is the
            // claim.
            assert_eq!(session.kind(), "pwsh");
        }
        other => panic!("expected a backend refusal or a session, got {other:?}"),
    }
    terminals.close_all().await;
}

/// TC-PORT-TERM-33: a session starts where the request said, and a relative
/// path is resolved against the deployment's workspace.
///
/// Upstream: `TerminalSpawnRequest.cwd`, defaulting to the workspace root.
///
/// A model that writes `cwd: "crates/exec"` means the one inside the
/// workspace, and a harness that resolved it against its own process
/// directory would start the shell somewhere neither of them chose.
///
/// Input: one session with no `cwd`, one with a relative path, and one with an
/// absolute path outside the workspace.
/// Expected: each shell's own `pwd` is the directory that was meant.
#[tokio::test]
async fn a_session_starts_where_the_request_said() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let inner = workspace.path().join("inner");
    std::fs::create_dir(&inner).expect("a directory to start in");
    let elsewhere = tempfile::tempdir().expect("temp dir");
    let terminals = registry(workspace.path());
    let owner = Owner::new("session-a");

    let Some(default) = open(&terminals, &owner, OpenRequest::default()).await else {
        return;
    };
    let relative = open(
        &terminals,
        &owner,
        OpenRequest {
            cwd: Some("inner".into()),
            ..OpenRequest::default()
        },
    )
    .await
    .expect("a terminal in a relative directory");
    let absolute = open(
        &terminals,
        &owner,
        OpenRequest {
            cwd: Some(elsewhere.path().to_path_buf()),
            ..OpenRequest::default()
        },
    )
    .await
    .expect("a terminal somewhere else entirely");

    for (session, expected) in [
        (&default, workspace.path().to_path_buf()),
        (&relative, inner.clone()),
        (&absolute, elsewhere.path().to_path_buf()),
    ] {
        let seen = session.send("pwd", true, None).await.expect("sent");
        let expected = std::fs::canonicalize(&expected).expect("a real directory");
        assert!(
            seen.viewport.contains(&expected.display().to_string()),
            "the shell started somewhere else: {:?} is not {}",
            seen.viewport,
            expected.display()
        );
    }
    terminals.close_all().await;
}

// ---------------------------------------------------------------- fixtures

/// A registry over a bash backend, rooted at `workspace`, with budgets short
/// enough that a case waiting for one waits for milliseconds.
fn registry(workspace: &std::path::Path) -> Terminals {
    Terminals::with(
        TerminalConfig {
            cwd: workspace.to_path_buf(),
            idle_silence: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            grace: Duration::from_millis(200),
            ..TerminalConfig::default()
        },
        Arc::new(Bash::new()),
    )
    .expect("one backend registers")
}

/// The first session a case opens, or `None` after reporting the case skipped
/// where this host has no terminal to allocate.
async fn open(
    terminals: &Terminals,
    owner: &Owner,
    request: OpenRequest,
) -> Option<Arc<tetanus_exec::terminal::TerminalSession>> {
    match terminals.open(owner, request).await {
        Ok(session) => Some(session),
        Err(TerminalError::Pty(tetanus_exec::pty::PtyError::Allocate(why))) => {
            eprintln!("skipped: this host cannot allocate a pseudo-terminal ({why})");
            None
        }
        Err(TerminalError::Backend(why)) => {
            eprintln!("skipped: this host has no bash ({why})");
            None
        }
        Err(other) => panic!("the terminal could not be opened: {other}"),
    }
}
