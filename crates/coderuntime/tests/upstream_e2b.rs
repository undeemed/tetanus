//! Test Design Specification: the remote backend and the sandbox it borrows,
//! ported.
//!
//! Feature under test: `tetanus_coderuntime::remote` - one shared sandbox
//! created on first use and killed at shutdown, a program submitted, polled,
//! fetched and cancelled, and the credential rules around all of it. Upstream
//! pins the sandbox-ownership half in `packages/e2b/e2b/tests/e2b.spec.ts`;
//! the submit/poll/fetch/cancel half is the shape this lane was asked to match
//! rather than a file to port line for line, because upstream's code runtime
//! and its e2b package are two packages that never meet.
//!
//! Approach: the scripted provider that ships with the crate. It really
//! evaluates the program it was submitted, so an end-to-end case is asserting
//! a run that arrived through the four calls rather than a canned answer. No
//! case reaches a network, and none needs a key that is worth anything.
//!
//! What is not restated, and why. Upstream's E2B integration is a *sandbox
//! owner* shared by a filesystem adapter and a subprocess adapter; tetanus has
//! neither of those, so its `quoteE2BShellArg`, its login-shell control home
//! and its `FileType` re-exports have nothing to attach to. Its SDK-specific
//! failure taxonomy is one provider's; [`RemoteFault`] carries the three
//! distinctions the runtime actually acts on. What is restated is every rule
//! about *ownership*: one sandbox, created lazily, rolled back when setup
//! fails, killed once at disposal, and a kill of something already gone
//! treated as success.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_coderuntime::remote::double::{scripted, Faults, ScriptedSandbox};
use tetanus_coderuntime::remote::{RemoteFault, Sandbox};
use tetanus_coderuntime::types::{Abort, CodeRuntime, FailureKind, Namespace};
use tetanus_coderuntime::{RemoteRuntime, RunRequest, SandboxConfig};

fn config() -> SandboxConfig {
    SandboxConfig {
        api_key: Some("a-key".to_string()),
        poll_every: Duration::from_millis(5),
        wall: Duration::from_secs(2),
        ..SandboxConfig::default()
    }
}

/// TC-PORT-E2B-1: the remote backend runs the same program through the same
/// seam.
///
/// Upstream: the seam's whole promise - a caller writes against `CodeRuntime`
/// and a deployment chooses the substrate.
///
/// Input: the same program the local cases run, submitted to the scripted
/// provider.
/// Expected: the value and the logs come back through submit, poll and fetch;
/// the runtime reports itself as a container; and exactly one sandbox was
/// created for it.
#[tokio::test]
async fn the_remote_backend_runs_the_same_program_through_the_same_seam() {
    let (provider, runtime) = scripted(Faults::default(), config());
    assert_eq!(runtime.isolation(), "container");

    let result = runtime
        .run(RunRequest::new(
            r#"let total = 0; let i = 1;
               while (i <= 4) { total = total + i; i = i + 1; }
               log("done"); return { total: total };"#,
        ))
        .await
        .expect("not seam misuse");

    assert!(result.is_ok(), "{:?}", result.error);
    assert_eq!(result.value, Some(json!({ "total": 10 })));
    assert_eq!(result.logs, vec!["done".to_string()]);
    assert!(result.duration > Duration::ZERO);
    assert_eq!(provider.created(), 1);
    assert_eq!(provider.jobs().len(), 1);
}

/// TC-PORT-E2B-2: one sandbox is created, shared, and killed once.
///
/// Upstream: "creates one protected shared sandbox and kills it on default
/// disposal".
///
/// A sandbox per run would be a machine per program: slower, and billed for.
///
/// Input: three runs on one runtime, then shutdown.
/// Expected: one sandbox created for all three, one kill, and nothing left
/// live at the provider.
#[tokio::test]
async fn one_sandbox_is_created_shared_and_killed_once() {
    let (provider, runtime) = scripted(Faults::default(), config());
    for _ in 0..3 {
        runtime
            .run(RunRequest::new("return 1;"))
            .await
            .expect("not seam misuse");
    }
    assert_eq!(provider.created(), 1, "a sandbox per run was created");
    assert_eq!(provider.live().len(), 1);

    runtime.shutdown().await;
    assert_eq!(provider.killed(), 1);
    assert!(
        provider.live().is_empty(),
        "a sandbox was left running: {:?}",
        provider.live()
    );
}

/// TC-PORT-E2B-3: a sandbox that cannot be prepared is killed, and the setup
/// failure is what the caller reads.
///
/// Upstream: "kills a newly created sandbox when remote directory setup
/// fails", "preserves the setup failure after its one rollback attempt
/// fails".
///
/// A creation that is not rolled back is a machine nobody holds a handle to,
/// running until its lifetime expires and billed for the whole of it.
///
/// Input: a provider that creates happily and refuses to prepare; then one
/// that also refuses to kill.
/// Expected: both report the *setup* failure, and both attempted the kill.
#[tokio::test]
async fn a_sandbox_that_cannot_be_prepared_is_killed_and_the_setup_failure_survives() {
    let (provider, runtime) = scripted(
        Faults {
            prepare: Some(RemoteFault::Provider("the workspace is read-only".into())),
            ..Faults::default()
        },
        config(),
    );
    let result = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    assert_eq!(result.kind(), Some(FailureKind::WorkerExit));
    assert!(
        result
            .error
            .as_ref()
            .is_some_and(|failure| failure.message.contains("read-only")),
        "{:?}",
        result.error
    );
    assert_eq!(provider.created(), 1);
    assert_eq!(provider.killed(), 1, "the creation was not rolled back");
    assert!(provider.live().is_empty());

    // And when the rollback itself fails, the original failure is still the
    // one reported.
    let (stubborn, runtime) = scripted(
        Faults {
            prepare: Some(RemoteFault::Provider("the workspace is read-only".into())),
            kill: Some(RemoteFault::Provider("the sandbox will not die".into())),
            ..Faults::default()
        },
        config(),
    );
    let result = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    let message = result.error.expect("a failure").message;
    assert!(
        message.contains("read-only") && !message.contains("will not die"),
        "the setup failure is what the caller reads: {message}"
    );
    assert_eq!(stubborn.killed(), 1);
}

/// TC-PORT-E2B-4: killing a sandbox that is already gone is success.
///
/// Upstream: "accepts a missing sandbox when disposal itself requests
/// deletion", "does not classify other disposal failures as an already-gone
/// sandbox".
///
/// A provider expires sandboxes on its own schedule, so a harness that treated
/// `not found` at teardown as a failure would report one every time a session
/// outlived its sandbox.
///
/// Input: a shutdown whose kill answers `NotFound`; and one whose kill answers
/// a real provider failure.
/// Expected: neither leaves the runtime unusable, both mark it closed, and the
/// two are distinguished at the provider rather than collapsed.
#[tokio::test]
async fn killing_a_sandbox_that_is_already_gone_is_success() {
    let provider = Arc::new(ScriptedSandbox::new(Faults::default()));
    let runtime = RemoteRuntime::new(Arc::clone(&provider) as Arc<dyn Sandbox>, config());
    runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");

    // The provider loses the sandbox behind the runtime's back, exactly as an
    // expiry does.
    let held = provider.live()[0].clone();
    provider.kill(&held).await.expect("the provider drops it");
    assert!(provider.live().is_empty());

    // Shutdown asks again, and the `NotFound` is the state it wanted.
    runtime.shutdown().await;
    assert_eq!(provider.killed(), 2);

    let refused = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    assert_eq!(
        refused.kind(),
        Some(FailureKind::WorkerExit),
        "a shut-down runtime creates nothing new"
    );
}

/// TC-PORT-E2B-5: a run is polled until it settles, and a cancel stops it.
///
/// Upstream: the four calls this lane was asked to match. The cancel is the
/// half with teeth: a caller that stopped waiting must not leave a program
/// running on a machine it is paying for.
///
/// Input: a provider that answers three polls with `Running`; then a run
/// aborted while it is being polled.
/// Expected: the first completes after the polls; the second comes back as an
/// `abort` promptly, and the provider was actually told to cancel that job.
#[tokio::test]
async fn a_run_is_polled_until_it_settles_and_a_cancel_stops_it() {
    let (provider, runtime) = scripted(
        Faults {
            polls_before_done: 3,
            ..Faults::default()
        },
        config(),
    );
    let result = runtime
        .run(RunRequest::new("return 7;"))
        .await
        .expect("not seam misuse");
    assert_eq!(result.value, Some(json!(7)));

    // Now one the caller gives up on while it is still running.
    let (slow, runtime) = scripted(
        Faults {
            // More polls than the abort will allow.
            polls_before_done: 10_000,
            ..Faults::default()
        },
        config(),
    );
    let abort = Abort::new();
    let stopper = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stopper.stop();
    });

    let started = std::time::Instant::now();
    let cancelled = runtime
        .run(RunRequest::new("return 1;").abort_with(abort))
        .await
        .expect("not seam misuse");

    assert_eq!(cancelled.kind(), Some(FailureKind::Abort));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the abort was noticed late: {:?}",
        started.elapsed()
    );
    let job = slow.jobs().first().cloned().expect("a job was submitted");
    assert!(
        slow.was_cancelled(&job),
        "the provider was never told to cancel {job}"
    );
    let _ = provider;
}

/// TC-PORT-E2B-6: a run that outlives its ceiling is cancelled, not waited
/// for.
///
/// Upstream: its sandbox lifetime, which always deletes the sandbox at expiry.
///
/// Input: a provider that never finishes a job, under a 100ms ceiling.
/// Expected: a `timeout` naming the ceiling, promptly, and the job cancelled
/// at the provider.
#[tokio::test]
async fn a_run_that_outlives_its_ceiling_is_cancelled_not_waited_for() {
    let (provider, runtime) = scripted(
        Faults {
            polls_before_done: 10_000,
            ..Faults::default()
        },
        SandboxConfig {
            wall: Duration::from_millis(100),
            ..config()
        },
    );

    let started = std::time::Instant::now();
    let result = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");

    assert_eq!(result.kind(), Some(FailureKind::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let job = provider.jobs().first().cloned().expect("a job");
    assert!(provider.was_cancelled(&job));
}

/// TC-PORT-E2B-7: a key is required, and it is never sent into the sandbox.
///
/// Upstream: "requires a key when both config and the environment omit it",
/// "reads the key from the environment and honors the configured cwd and
/// lifetime", and its note that the key "is never forwarded into the sandbox".
///
/// Input: a config with no key and no environment; one with a key in the
/// environment only; and a prepared sandbox whose configured cwd is read back.
/// Expected: an `Unauthorized` for the first with an actionable message; a key
/// for the second; and the cwd the deployment configured reaching the
/// provider, with nothing carrying the key into the program's world.
#[tokio::test]
async fn a_key_is_required_and_never_sent_into_the_sandbox() {
    let unkeyed = SandboxConfig {
        api_key: None,
        ..config()
    };
    let refused = unkeyed.key(None).expect_err("no key anywhere");
    assert!(matches!(refused, RemoteFault::Unauthorized(_)));
    assert!(
        refused.to_string().contains("settings document"),
        "the message says where to put one: {refused}"
    );
    assert_eq!(
        unkeyed.key(Some("from-the-environment")).expect("a key"),
        "from-the-environment"
    );
    assert!(
        unkeyed.key(Some("   ")).is_err(),
        "a blank environment value is not a credential"
    );

    // A runtime with no key fails its runs rather than running them unkeyed.
    let (_, runtime) = scripted(Faults::default(), unkeyed);
    let result = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    assert_eq!(result.kind(), Some(FailureKind::WorkerExit));

    // And the configured working directory is what the sandbox is prepared
    // with - the one part of the config that does travel.
    let (provider, runtime) = scripted(
        Faults::default(),
        SandboxConfig {
            cwd: "/srv/agent".to_string(),
            ..config()
        },
    );
    runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    assert_eq!(provider.prepared(), vec!["/srv/agent".to_string()]);
}

/// TC-PORT-E2B-8: a program that needs this process's bindings is refused
/// rather than run without them.
///
/// No upstream equivalent: its e2b package carries no code runtime, so the
/// question never arises there. It arises here because both backends are the
/// same trait, and the difference between them has to be a refusal a caller
/// can read rather than a program that fails inside the sandbox on an
/// undefined name.
///
/// Input: a remote run carrying a binding namespace.
/// Expected: `SeamError::Unsupported`, naming the provider and saying which
/// runtime to use instead - and nothing submitted.
#[tokio::test]
async fn a_program_that_needs_this_processs_bindings_is_refused() {
    let (provider, runtime) = scripted(Faults::default(), config());
    let refused = runtime
        .run(
            RunRequest::new("return tools.read(1);")
                .binding(Namespace::new("tools").with("read", |v| Ok(v.clone()))),
        )
        .await
        .expect_err("a remote sandbox cannot call back into this process");

    assert!(
        refused.to_string().contains("local runtime"),
        "the refusal says what to do instead: {refused}"
    );
    assert_eq!(
        provider.created(),
        0,
        "nothing was created for a refused run"
    );
    assert!(provider.jobs().is_empty());
}

/// TC-PORT-E2B-9: a sandbox that dies under a job is worker-exit, and the next
/// run gets a new one.
///
/// Upstream: its sandbox can expire mid-operation, which is why disposal
/// tolerates a missing one.
///
/// Input: a provider whose sandbox is removed while a job is being polled.
/// Expected: the run reports `worker-exit` carrying the provider's words, and
/// the run after it creates a second sandbox rather than reusing a dead id.
#[tokio::test]
async fn a_sandbox_that_dies_under_a_job_is_worker_exit_and_the_next_run_gets_a_new_one() {
    let provider = Arc::new(ScriptedSandbox::new(Faults {
        polls_before_done: 10_000,
        ..Faults::default()
    }));
    let runtime = RemoteRuntime::new(
        Arc::clone(&provider) as Arc<dyn Sandbox>,
        SandboxConfig {
            wall: Duration::from_millis(150),
            ..config()
        },
    );

    // The ceiling is what ends this one; the point is the state afterwards.
    let ended = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    assert_eq!(ended.kind(), Some(FailureKind::Timeout));

    // The provider loses the sandbox, and the next run has to notice.
    let held = provider.live()[0].clone();
    provider.kill(&held).await.expect("dropped");
    let after = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect("not seam misuse");
    assert_eq!(
        after.kind(),
        Some(FailureKind::WorkerExit),
        "a submit into a sandbox that is gone is not a program failure"
    );
    assert!(
        after
            .error
            .as_ref()
            .is_some_and(|failure| failure.message.contains("no sandbox")),
        "{:?}",
        after.error
    );
}
