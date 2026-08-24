//! Test Design Specification: the budgets a program cannot outlive, ported.
//!
//! Feature under test: `tetanus_coderuntime::local` under load it is meant to
//! survive - a loop that never ends, a binding that never returns, output
//! without limit, a worker that dies. Upstream pins the same in
//! `packages/code-runtime/code-runtime-worker-thread/tests/runtime.spec.ts`,
//! under "budgets and containment".
//!
//! Approach: real worker threads and real budgets, set small. Every case here
//! is about something that must *end*, so each one is bounded by a test-level
//! timeout as well: a lost containment property produces no value rather than
//! a wrong one, and without the bound a regression would wedge a CI run
//! instead of failing it. That is the same reason
//! `crates/turn/tests/deepseek_deadline.rs` gives.
//!
//! What is not restated, and why. Upstream's OOM containment needs a heap cap
//! per worker, which a Rust thread cannot have without an allocator this crate
//! has no business installing: the memory bound is the output ledger and the
//! fuel, and `docs/parity.md` says so. Its forged-port cases are about a
//! serialization boundary between host and worker that this backend does not
//! have - the evaluator runs in the host's address space and the host owns
//! every value - so a program cannot forge a message; TC-PORT-CODERT-24 pins
//! the property those cases protect, which is that the ledger cannot be
//! talked past.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, a panic, or a case that does not
//! finish inside its bound.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_coderuntime::types::{Abort, CodeRuntime, FailureKind, Namespace};
use tetanus_coderuntime::{Budget, LocalRuntime, RunRequest};

/// Budgets small enough that every case here finishes in well under a second.
fn tight() -> Budget {
    Budget {
        fuel: 200_000,
        wall: Duration::from_millis(300),
        max_output_bytes: 512,
        reap_grace: Duration::from_millis(200),
    }
}

/// Wait for the workers to be reclaimed, bounded. The count falls when the
/// thread returns, which is a moment after the result does.
async fn workers_reclaimed(runtime: &LocalRuntime) -> bool {
    for _ in 0..200 {
        if runtime.live_workers() == 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

/// TC-PORT-CODERT-15: a program that never stops is stopped, and its worker
/// comes back.
///
/// Upstream: "ends a hot loop at the compute budget".
///
/// This is the acceptance the local backend exists for. A Rust thread cannot
/// be killed, so the claim being made is stronger than "the call returned": it
/// is that the evaluator noticed and the thread was reclaimed, which is what
/// stops a harness accumulating one wedged thread per runaway program.
///
/// Input: `while (true) { }` under a small fuel budget.
/// Expected: a `timeout` naming the compute budget, inside the test's bound,
/// and no live worker afterwards.
#[tokio::test]
async fn a_program_that_never_stops_is_stopped_and_its_worker_comes_back() {
    let runtime = LocalRuntime::new(tight());
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.run(RunRequest::new("while (true) { }")),
    )
    .await
    .expect("the runaway program ended")
    .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::Timeout));
    assert!(
        result
            .error
            .as_ref()
            .is_some_and(|failure| failure.message.contains("compute budget")),
        "{:?}",
        result.error
    );
    assert!(
        workers_reclaimed(&runtime).await,
        "the worker was not reclaimed: {} still live",
        runtime.live_workers()
    );
}

/// TC-PORT-CODERT-16: a run that mostly waits ends at the wall clock, and
/// waiting costs it no fuel.
///
/// Upstream: "ends an idle-forever run at the wall-clock ceiling", "does not
/// charge time spent awaiting a slow binding against the compute budget".
///
/// The two halves are one design: the compute budget is fuel, which a host
/// binding never spends, so the ceiling is what bounds a program that spends
/// its life waiting - and a program that legitimately waits is not failed for
/// being slow when the host was.
///
/// Input: a loop calling a binding that sleeps 20ms, under a 300ms ceiling and
/// a fuel budget far larger than the loop can spend.
/// Expected: a `timeout` naming the wall-clock ceiling rather than the compute
/// budget, and the binding was entered several times - so fuel was not what
/// ran out.
#[tokio::test]
async fn a_run_that_mostly_waits_ends_at_the_wall_clock_and_waiting_costs_no_fuel() {
    let runtime = LocalRuntime::new(tight());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let slow = Namespace::new("host").with("wait", move |_| {
        counted.fetch_add(1, Ordering::AcqRel);
        std::thread::sleep(Duration::from_millis(20));
        Ok(json!(null))
    });

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.run(RunRequest::new("while (true) { host.wait(1); }").binding(slow)),
    )
    .await
    .expect("the run ended")
    .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::Timeout));
    let message = result.error.expect("a failure").message;
    assert!(
        message.contains("wall-clock"),
        "the ceiling that fired is named: {message}"
    );
    let entered = calls.load(Ordering::Acquire);
    assert!(
        entered > 2,
        "the loop should have spent its time in the binding, not in fuel: {entered} calls"
    );
    assert!(workers_reclaimed(&runtime).await);
}

/// TC-PORT-CODERT-17: an abort mid-run stops the program.
///
/// Upstream: "reports an abort mid-run and stops the worker".
///
/// An abort is not a timeout: the run was fine and somebody changed their
/// mind, and a caller that cannot tell the two apart cannot tell a user's
/// interrupt from a budget it should raise.
///
/// Input: a long-running loop, aborted from outside after it has started.
/// Expected: an `abort` failure, promptly, with the worker reclaimed.
#[tokio::test]
async fn an_abort_mid_run_stops_the_program() {
    let runtime = LocalRuntime::new(Budget {
        // Enough fuel and time that nothing but the abort can end this.
        fuel: 100_000_000,
        wall: Duration::from_secs(30),
        ..tight()
    });
    let abort = Abort::new();
    let stopper = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stopper.stop();
    });

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.run(RunRequest::new("while (true) { }").abort_with(abort)),
    )
    .await
    .expect("the abort ended the run")
    .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::Abort));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the abort was noticed late: {:?}",
        started.elapsed()
    );
    assert!(workers_reclaimed(&runtime).await);
}

/// TC-PORT-CODERT-18: runaway output fails at the cap, and what fitted is
/// kept.
///
/// Upstream: "fails runaway log output explicitly while retaining a bounded
/// prefix", "retains a fitting prefix when one oversized log is the first
/// output".
///
/// The prefix is the point: a program that failed by logging too much is
/// usually explained by what it logged first, and a failure that threw the
/// logs away would take the explanation with it.
///
/// Input: a loop logging a line at a time under a 512-byte cap, then a single
/// log longer than the whole cap.
/// Expected: `output-limit` in both cases, with logs kept in the first and a
/// bounded prefix kept in the second, and nothing kept past the cap.
#[tokio::test]
async fn runaway_output_fails_at_the_cap_and_what_fitted_is_kept() {
    let runtime = LocalRuntime::new(tight());

    let flood = runtime
        .run(RunRequest::new(
            r#"let i = 0; while (i < 1000) { log("line of output number " + i); i = i + 1; }"#,
        ))
        .await
        .expect("not an error of run");
    assert_eq!(flood.kind(), Some(FailureKind::OutputLimit));
    assert!(!flood.logs.is_empty(), "the prefix was thrown away");
    let kept: usize = flood.logs.iter().map(String::len).sum();
    assert!(kept <= 512, "{kept} bytes were kept past a 512 byte cap");
    assert!(
        flood.logs[0].contains("number 0"),
        "the beginning is what is kept: {:?}",
        flood.logs[0]
    );

    let one_big = runtime
        .run(RunRequest::new(
            r#"let pad = "0123456789"; let i = 0; while (i < 8) { pad = pad + pad; i = i + 1; }
               log("head:" + pad);"#,
        ))
        .await
        .expect("not an error of run");
    assert_eq!(one_big.kind(), Some(FailureKind::OutputLimit));
    let kept: usize = one_big.logs.iter().map(String::len).sum();
    assert!(kept <= 512, "{kept} bytes kept");
    assert!(
        one_big
            .logs
            .first()
            .is_some_and(|line| line.starts_with("head:")),
        "the fitting prefix of the one oversized line is kept: {:?}",
        one_big.logs.first()
    );
}

/// TC-PORT-CODERT-19: an oversized value fails the run, and logs and value are
/// one ledger.
///
/// Upstream: "fails an oversized return value without substituting a string",
/// "accounts logs and completion in one exact combined ledger".
///
/// Two caps that each pass is not a cap: a program can always split its output
/// between the two.
///
/// Input: a program returning a string larger than the cap; then one whose
/// logs and value each fit but together do not.
/// Expected: `output-limit` for both, no value substituted, and the ledger's
/// message naming the combined size.
#[tokio::test]
async fn an_oversized_value_fails_the_run_and_logs_and_value_are_one_ledger() {
    let runtime = LocalRuntime::new(tight());

    let big_value = runtime
        .run(RunRequest::new(
            r#"let pad = "0123456789"; let i = 0; while (i < 7) { pad = pad + pad; i = i + 1; }
               return pad;"#,
        ))
        .await
        .expect("not an error of run");
    assert_eq!(big_value.kind(), Some(FailureKind::OutputLimit));
    assert!(
        big_value.value.is_none(),
        "a value was substituted: {:?}",
        big_value.value
    );

    // 300 bytes of logs and 300 of value: each fits under 512, together they
    // do not.
    let combined = runtime
        .run(RunRequest::new(
            r#"let pad = "0123456789"; let i = 0; while (i < 5) { pad = pad + pad; i = i + 1; }
               log(pad); return pad;"#,
        ))
        .await
        .expect("not an error of run");
    assert_eq!(combined.kind(), Some(FailureKind::OutputLimit));
    assert!(
        combined
            .error
            .as_ref()
            .is_some_and(|failure| failure.message.contains("together")),
        "the ledger says the two were counted as one: {:?}",
        combined.error
    );
}

/// TC-PORT-CODERT-20: a worker that dies is contained, and the host is
/// healthy afterwards.
///
/// Upstream: "reports a worker that exits before publishing a completion",
/// "contains an OOM under resourceLimits as worker-exit, host process
/// healthy".
///
/// A panic inside a host binding is somebody else's bug arriving on this
/// crate's thread. Letting it unwind would take out the turn; reporting it as
/// an exception would blame the program. `worker-exit` is the honest class.
///
/// Input: a binding that panics, called by a program.
/// Expected: a `worker-exit` failure, the runtime still usable for the next
/// run, and no leaked worker.
#[tokio::test]
async fn a_worker_that_dies_is_contained_and_the_host_is_healthy_afterwards() {
    let runtime = LocalRuntime::new(tight());
    let hostile = Namespace::new("host").with("explode", |_| {
        panic!("a host binding with a bug in it");
    });

    let died = runtime
        .run(RunRequest::new("return host.explode(1);").binding(hostile))
        .await
        .expect("not an error of run");
    assert_eq!(died.kind(), Some(FailureKind::WorkerExit));

    let after = runtime
        .run(RunRequest::new("return 1 + 1;"))
        .await
        .expect("the runtime still works");
    assert_eq!(after.value, Some(json!(2)));
    assert!(workers_reclaimed(&runtime).await);
}

/// TC-PORT-CODERT-21: shutting the runtime down ends a run that is in flight.
///
/// Upstream: "disposal aborts in-flight runs, awaits worker exit, and rejects
/// later runs".
///
/// A harness stopping must not wait on a program that was going to run for
/// another minute, and must not leave the thread behind either.
///
/// Input: a long run, with `shutdown` called while it is going.
/// Expected: the run ends as an abort, and every worker is reclaimed.
#[tokio::test]
async fn shutting_the_runtime_down_ends_a_run_that_is_in_flight() {
    let runtime = Arc::new(LocalRuntime::new(Budget {
        fuel: 100_000_000,
        wall: Duration::from_secs(30),
        ..tight()
    }));
    let closing = Arc::clone(&runtime);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        closing.shutdown().await;
    });

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.run(RunRequest::new("while (true) { }")),
    )
    .await
    .expect("the shutdown ended the run")
    .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::Abort));
    assert!(workers_reclaimed(&runtime).await);
}

/// TC-PORT-CODERT-22: a binding that never returns is bounded by the reap
/// grace, and named as the reason.
///
/// Upstream has no equivalent: a Node worker can be terminated whatever it is
/// doing, so a stuck host call is its caller's problem and not the runtime's.
/// Here it is the one case the stop flag cannot reach - the worker is inside
/// the caller's code, not the evaluator's - so the runtime bounds it from
/// outside and says which of the two it is blaming.
///
/// Input: a binding that blocks for far longer than the ceiling plus the reap
/// grace.
/// Expected: a `timeout` whose message names the binding rather than the
/// program's own budgets, returned inside the bound.
#[tokio::test]
async fn a_binding_that_never_returns_is_bounded_and_named_as_the_reason() {
    let runtime = LocalRuntime::new(tight());
    let stuck = Namespace::new("host").with("block", |_| {
        std::thread::sleep(Duration::from_secs(30));
        Ok(json!(null))
    });

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.run(RunRequest::new("return host.block(1);").binding(stuck)),
    )
    .await
    .expect("the runtime did not wait for the binding")
    .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::Timeout));
    assert!(
        result
            .error
            .as_ref()
            .is_some_and(|failure| failure.message.contains("binding")),
        "the message blames the binding, not the program: {:?}",
        result.error
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the bound did not hold: {:?}",
        started.elapsed()
    );
    // The thread is still inside the caller's binding and cannot be
    // reclaimed - that is the honest state, and the count says so rather than
    // the runtime pretending otherwise.
    assert_eq!(
        runtime.live_workers(),
        1,
        "a worker stuck in a host binding is still a worker"
    );
}

/// TC-PORT-CODERT-23: the budgets are what the caller set, not what the
/// program asked for.
///
/// Upstream: "rejects config values that are not positive numbers", and the
/// convention its request type states - defaulting is the implementation's
/// validated config, and a request carries no tuning knobs.
///
/// Input: two runtimes with different fuel, running the same program.
/// Expected: the smaller budget fails where the larger completes, and no part
/// of the program can change either.
#[tokio::test]
async fn the_budgets_are_what_the_caller_set_not_what_the_program_asked_for() {
    let program = "let i = 0; while (i < 5000) { i = i + 1; } return i;";

    let generous = LocalRuntime::new(Budget {
        fuel: 200_000,
        ..tight()
    });
    assert_eq!(
        generous
            .run(RunRequest::new(program))
            .await
            .expect("ran")
            .value,
        Some(json!(5000))
    );

    let mean = LocalRuntime::new(Budget {
        fuel: 500,
        ..tight()
    });
    let starved = mean.run(RunRequest::new(program)).await.expect("ran");
    assert_eq!(starved.kind(), Some(FailureKind::Timeout));
    assert_eq!(mean.budget().fuel, 500, "the budget is the runtime's");
}

/// TC-PORT-CODERT-24: the output ledger cannot be talked past.
///
/// Upstream: its forged-port family - "fails forged log floods and forged done
/// values through the same outer cap", "re-caps an oversized forged done value
/// at the host boundary", "honors a forged worker-side output-limit signal".
/// Those cases exist because upstream's worker is a separate program that
/// could lie about its own accounting. Here the evaluator and the meter are
/// the same code in one address space, so nothing can be forged - which makes
/// the property they protect assertable directly.
///
/// Input: a program that logs a bounded prefix, then tries to return a value
/// larger than the whole cap.
/// Expected: `output-limit`, the logs bounded, and the value absent - the two
/// counted against one ledger with no path that skips it.
#[tokio::test]
async fn the_output_ledger_cannot_be_talked_past() {
    let runtime = LocalRuntime::new(tight());
    let result = runtime
        .run(RunRequest::new(
            r#"log("small");
               let pad = "0123456789"; let i = 0; while (i < 7) { pad = pad + pad; i = i + 1; }
               return { big: pad };"#,
        ))
        .await
        .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::OutputLimit));
    assert_eq!(result.logs, vec!["small".to_string()]);
    assert!(result.value.is_none());
}
