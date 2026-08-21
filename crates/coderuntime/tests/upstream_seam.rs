//! Test Design Specification: the code-runtime seam and its portable name
//! rules, ported.
//!
//! Features under test: `tetanus_coderuntime::types` - what a run answers with
//! and what counts as misuse rather than failure - and
//! `tetanus_coderuntime::reserved`, the names every backend refuses. Upstream
//! pins these in `packages/code-runtime/code-runtime/tests/service.spec.ts`
//! and `reserved.spec.ts`.
//!
//! Approach: the local runtime, because a seam asserted against a fake
//! implementation of itself asserts the fake. The cases here are about the
//! *contract* - a failure is a field, misuse is an error, a shut-down runtime
//! refuses - so they use the smallest programs that reach each answer.
//!
//! What is not restated, and why. Upstream's service registration cases
//! (`ctx.codeRuntime`, removal on fiber disposal, refusing a second
//! implementation) are Cordis lifecycle: a tetanus runtime is a value a
//! composer holds, so there is no registry to remove it from and no second
//! registration to refuse. Its typed rejection classes have no counterpart
//! yet, since a binding failure here is a message the program reads, so
//! `RESERVED_ERROR_MEMBERS` is pinned as the settled half of that contract and
//! `docs/parity.md` carries the rest.
//!
//! Environmental needs: none. No network, no key, no filesystem.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use serde_json::json;
use tetanus_coderuntime::reserved::{
    check_error_member, check_global, is_dunder, is_portable_identifier, PORTABLE_RESERVED_WORDS,
    RESERVED_BINDING_GLOBALS, RESERVED_ERROR_MEMBERS,
};
use tetanus_coderuntime::types::{CodeRuntime, FailureKind, Namespace, SeamError};
use tetanus_coderuntime::{Budget, LocalRuntime, RunRequest};

fn runtime() -> LocalRuntime {
    LocalRuntime::new(Budget {
        fuel: 200_000,
        wall: std::time::Duration::from_secs(2),
        max_output_bytes: 4096,
        reap_grace: std::time::Duration::from_millis(200),
    })
}

/// TC-PORT-CODERT-1: a runtime says what it runs and what it runs it in.
///
/// Upstream: "registers as ctx.codeRuntime and serves the abstract API",
/// "registers with the seam descriptors".
///
/// The two descriptors are informational and nothing gates on them, which is
/// exactly why they have to be honest: a surface that presents usage
/// instructions switches on the language, and this one is not JavaScript.
///
/// Input: the local runtime.
/// Expected: a lowercase language identifier that does not claim to be
/// JavaScript or TypeScript, and `worker-thread` as the substrate.
#[test]
fn a_runtime_says_what_it_runs_and_what_it_runs_it_in() {
    let runtime = runtime();
    assert_eq!(runtime.isolation(), "worker-thread");
    let language = runtime.language();
    assert_eq!(language, language.to_lowercase());
    assert!(
        !["javascript", "typescript", "python"].contains(&language),
        "the descriptor must not claim a language this backend does not run: {language}"
    );
}

/// TC-PORT-CODERT-2: a failed program is a field on a result, never an error
/// of `run`.
///
/// Upstream: "reports a failed run as an error field on a resolved result,
/// never a rejection".
///
/// The distinction is the seam's whole shape: reporting a failed program is
/// the caller's job, so it must arrive as data rather than as an exception
/// path that a caller can forget to handle.
///
/// Input: a program that fails at run time, and one that does not parse.
/// Expected: both are `Ok`, both carry `exception`, and the message is the
/// one a model could correct itself from.
#[tokio::test]
async fn a_failed_program_is_a_field_on_a_result_never_an_error_of_run() {
    let runtime = runtime();

    let threw = runtime
        .run(RunRequest::new("return missing_name;"))
        .await
        .expect("a failing program is not an error of run");
    assert_eq!(threw.kind(), Some(FailureKind::Exception));
    assert!(
        threw
            .error
            .as_ref()
            .is_some_and(|e| e.message.contains("missing_name")),
        "{:?}",
        threw.error
    );
    assert!(threw.value.is_none());

    let unparsable = runtime
        .run(RunRequest::new("return ;;; ("))
        .await
        .expect("a program that does not parse is not an error of run either");
    assert_eq!(unparsable.kind(), Some(FailureKind::Exception));
    assert!(
        unparsable
            .error
            .as_ref()
            .is_some_and(|e| e.message.contains("does not parse")),
        "{:?}",
        unparsable.error
    );
}

/// TC-PORT-CODERT-3: a request that is already aborted never starts a worker.
///
/// Upstream: "reports a pre-aborted signal as an abort failure", "reports a
/// pre-aborted signal without spawning".
///
/// Input: a request carrying an abort that has already fired, holding a
/// program that would otherwise run for ever.
/// Expected: an `abort` failure, no worker ever alive, and it returns at once.
#[tokio::test]
async fn a_request_that_is_already_aborted_never_starts_a_worker() {
    let runtime = runtime();
    let started = std::time::Instant::now();
    let result = runtime
        .run(
            RunRequest::new("while (true) { }")
                .abort_with(tetanus_coderuntime::types::Abort::fired()),
        )
        .await
        .expect("not an error of run");

    assert_eq!(result.kind(), Some(FailureKind::Abort));
    assert_eq!(runtime.live_workers(), 0);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "it waited: {:?}",
        started.elapsed()
    );
}

/// TC-PORT-CODERT-4: a namespace no backend can expose is misuse, refused
/// before anything runs.
///
/// Upstream: "rejects invalid and duplicate binding globals loudly", and the
/// `RESERVED_BINDING_GLOBALS` half of `reserved.spec.ts`.
///
/// The line between misuse and failure is what makes the seam usable: a caller
/// cannot fix a program that threw by changing its own code, and it can always
/// fix a namespace it named badly.
///
/// Input: a namespace called `console`, one called `lambda`, one spelled
/// `$tools`, and two namespaces of the same name.
/// Expected: `SeamError` in each case, naming the global and saying why, and
/// no result at all.
#[tokio::test]
async fn a_namespace_no_backend_can_expose_is_misuse_refused_before_anything_runs() {
    let runtime = runtime();

    for (global, expected) in [
        ("console", "slot a backend owns"),
        ("lambda", "reserved word"),
        ("$tools", "portable identifier"),
        ("", "needs a name"),
    ] {
        let refused = runtime
            .run(RunRequest::new("return 1;").binding(Namespace::new(global)))
            .await
            .expect_err("this is misuse, not a failed run");
        let SeamError::BadNamespace { global: named, why } = &refused else {
            panic!("expected a bad namespace, got {refused:?}");
        };
        assert_eq!(named, global);
        assert!(why.contains(expected), "{global:?}: {why}");
    }

    let duplicated = runtime
        .run(
            RunRequest::new("return 1;")
                .binding(Namespace::new("tools"))
                .binding(Namespace::new("tools")),
        )
        .await
        .expect_err("two namespaces of one name");
    assert!(matches!(duplicated, SeamError::DuplicateNamespace(name) if name == "tools"));
}

/// TC-PORT-CODERT-5: the portable name rules are the union, not this
/// backend's own.
///
/// Upstream: "RESERVED_BINDING_GLOBALS covers each backend-owned slot",
/// "PORTABLE_RESERVED_WORDS is the union of ECMAScript and Python reserved
/// words", "RESERVED_ERROR_MEMBERS covers the JS Error and Python
/// exception-protocol members", "DUNDER_MEMBER matches dunder-form names
/// only".
///
/// Narrowing the set to what this backend needs would be the same mistake one
/// level down: `lambda` would work today and break the day a Python backend
/// lands.
///
/// Input: the sets themselves, the identifier rule, and the dunder rule
/// against upstream's own table of names.
/// Expected: the backend-owned slots and both languages' reserved words are
/// present; the identifier rule accepts portable names and refuses the rest;
/// `__x__` is dunder and `_private`, `name`, `__mid`, `__` and `____` are not.
#[test]
fn the_portable_name_rules_are_the_union_not_this_backends_own() {
    for slot in [
        "console",
        "__dsh_main__",
        "__builtins__",
        "__name__",
        "__debug__",
    ] {
        assert!(RESERVED_BINDING_GLOBALS.contains(&slot), "{slot}");
        assert!(check_global(slot).is_err(), "{slot} was accepted");
    }
    for word in [
        "await", "class", "function", "lambda", "None", "nonlocal", "match", "_",
    ] {
        assert!(PORTABLE_RESERVED_WORDS.contains(&word), "{word}");
    }
    for member in [
        "name",
        "message",
        "stack",
        "args",
        "with_traceback",
        "add_note",
    ] {
        assert!(RESERVED_ERROR_MEMBERS.contains(&member), "{member}");
        assert!(check_error_member(member).is_err(), "{member} was accepted");
    }

    for good in ["tools", "_private", "T1", "__mid"] {
        assert!(is_portable_identifier(good), "{good}");
    }
    for bad in ["$tools", "1tool", "a-b", "", "tools!"] {
        assert!(!is_portable_identifier(bad), "{bad}");
    }

    // Upstream's own table, name for name.
    for dunder in ["__dict__", "__init__", "__x__"] {
        assert!(is_dunder(dunder), "{dunder}");
        assert!(check_error_member(dunder).is_err(), "{dunder} was accepted");
    }
    for ordinary in ["_private", "name", "__mid", "__", "____"] {
        assert!(!is_dunder(ordinary), "{ordinary} was read as dunder");
    }
    // `name` is refused as an error member for the other reason, and `____`
    // is not dunder because its middle is empty - both are upstream's rules.
    assert!(check_error_member("____").is_ok());
    assert!(check_error_member("__mid").is_ok());
}

/// TC-PORT-CODERT-6: a shut-down runtime refuses later runs.
///
/// Upstream: "disposal aborts in-flight runs, awaits worker exit, and rejects
/// later runs".
///
/// Input: a runtime that has been shut down, asked to run a trivial program.
/// Expected: `SeamError::Unsupported`, saying it was shut down - misuse,
/// because the caller is holding a runtime it was told to stop using.
#[tokio::test]
async fn a_shut_down_runtime_refuses_later_runs() {
    let runtime = runtime();
    runtime.shutdown().await;
    let refused = runtime
        .run(RunRequest::new("return 1;"))
        .await
        .expect_err("a shut-down runtime runs nothing");
    assert!(
        matches!(&refused, SeamError::Unsupported { why, .. } if why.contains("shut down")),
        "{refused:?}"
    );
}

/// TC-PORT-CODERT-7: a binding is called with what the program passed and
/// answers into the program.
///
/// Upstream: "bridges binding calls both ways and rejects the program-side
/// call on a host rejection", "bridges a deeply nested lossless JSON argument,
/// resolution, and completion".
///
/// Input: a namespace whose function echoes a nested object back, and one
/// whose function refuses; a program that calls both.
/// Expected: the nested value survives both crossings unchanged; the refusal
/// becomes the program's own failure, carrying the host's words and the member
/// name.
#[tokio::test]
async fn a_binding_is_called_with_what_the_program_passed_and_answers_into_the_program() {
    let runtime = runtime();
    let nested = json!({ "a": [1, { "b": ["c", true, null] }], "n": 2.5 });
    let echoed = nested.clone();

    let tools = Namespace::new("tools")
        .with("echo", move |argument| Ok(argument.clone()))
        .with("refuse", |_| Err("the host said no".to_string()));

    let result = runtime
        .run(
            RunRequest::new(
                r#"let back = tools.echo({ a: [1, { b: ["c", true, null] }], n: 2.5 });
                   return back;"#,
            )
            .binding(tools.clone()),
        )
        .await
        .expect("ran");
    assert_eq!(result.error, None, "{:?}", result.error);
    assert_eq!(result.value, Some(echoed));

    let refused = runtime
        .run(RunRequest::new("return tools.refuse(1);").binding(tools))
        .await
        .expect("ran");
    assert_eq!(refused.kind(), Some(FailureKind::Exception));
    let message = refused.error.expect("a failure").message;
    assert!(message.contains("refuse"), "{message}");
    assert!(message.contains("the host said no"), "{message}");
}

/// TC-PORT-CODERT-8: a binding name that is not there is the program's
/// mistake, and the message says what is.
///
/// Upstream: "survives forged port traffic: unknown binding names ...".
///
/// A model that called the wrong name can correct itself from a list; one told
/// only "undefined is not a function" cannot.
///
/// Input: a program calling a member the namespace does not have.
/// Expected: an `exception` naming the member and listing the ones that exist.
#[tokio::test]
async fn a_binding_name_that_is_not_there_is_the_programs_mistake() {
    let runtime = runtime();
    let tools = Namespace::new("tools")
        .with("read", |_| Ok(json!("contents")))
        .with("write", |_| Ok(json!(true)));

    let result = runtime
        .run(RunRequest::new("return tools.delete(1);").binding(tools))
        .await
        .expect("ran");

    assert_eq!(result.kind(), Some(FailureKind::Exception));
    let message = result.error.expect("a failure").message;
    assert!(message.contains("delete"), "{message}");
    assert!(
        message.contains("read") && message.contains("write"),
        "the message lists what there is: {message}"
    );
}

/// TC-PORT-CODERT-9: nothing survives from one run to the next.
///
/// Upstream: "keeps runs isolated: no state survives from one run to the
/// next", "gives the program an EMPTY environment".
///
/// Input: one run that declares a name and one that reads it; a program asking
/// for a host global by every name this process might have.
/// Expected: the second run does not see the first's name, and no ambient
/// global is reachable.
#[tokio::test]
async fn nothing_survives_from_one_run_to_the_next() {
    let runtime = runtime();
    let first = runtime
        .run(RunRequest::new("let secret = 42; return secret;"))
        .await
        .expect("ran");
    assert_eq!(first.value, Some(json!(42)));

    let second = runtime
        .run(RunRequest::new("return secret;"))
        .await
        .expect("ran");
    assert_eq!(second.kind(), Some(FailureKind::Exception));

    for ambient in ["process", "env", "globalThis", "std", "require"] {
        let reached = runtime
            .run(RunRequest::new(format!("return {ambient};")))
            .await
            .expect("ran");
        assert_eq!(
            reached.kind(),
            Some(FailureKind::Exception),
            "{ambient} was reachable"
        );
    }
}

/// TC-PORT-CODERT-10: a completion that is not lossless JSON fails the run
/// rather than being rendered into something else.
///
/// Upstream: "rejects a non-lossless completion instead of replacing it with
/// rendered text", "completes a program that returns nothing with no value at
/// all".
///
/// Substituting a string would hand the caller a value the program never
/// produced, and the caller has no way to tell.
///
/// Input: a program returning a number that overflows to infinity; a program
/// returning a binding; and a program that returns nothing.
/// Expected: `invalid-output` for the first two, and a clean result with no
/// value at all for the third.
#[tokio::test]
async fn a_completion_that_is_not_lossless_json_fails_the_run() {
    let runtime = runtime();

    let infinite = runtime
        .run(RunRequest::new("return 1e308 * 10;"))
        .await
        .expect("ran");
    assert_eq!(infinite.kind(), Some(FailureKind::InvalidOutput));
    assert!(infinite.value.is_none(), "no value was substituted");

    let a_binding = runtime
        .run(
            RunRequest::new("return tools.echo;")
                .binding(Namespace::new("tools").with("echo", |v| Ok(v.clone()))),
        )
        .await
        .expect("ran");
    assert_eq!(a_binding.kind(), Some(FailureKind::InvalidOutput));

    let silent = runtime
        .run(RunRequest::new("log(\"done\");"))
        .await
        .expect("ran");
    assert!(silent.is_ok(), "{:?}", silent.error);
    assert_eq!(
        silent.value, None,
        "a program that returns nothing has no value"
    );
    assert_eq!(silent.logs, vec!["done".to_string()]);
}

/// TC-PORT-CODERT-11: logs are kept in order, and they survive a failure.
///
/// Upstream: "captures output in order, returns the value", "keeps logs
/// streamed before a failure".
///
/// What the program printed before it broke is usually the most useful thing
/// anyone gets, so a failure must not throw it away.
///
/// Input: a program that logs three lines and then fails.
/// Expected: the failure, and all three lines in the order they were written.
#[tokio::test]
async fn logs_are_kept_in_order_and_they_survive_a_failure() {
    let runtime = runtime();
    let result = runtime
        .run(RunRequest::new(
            r#"log("first"); log("second", 3); log("third"); return nope;"#,
        ))
        .await
        .expect("ran");

    assert_eq!(result.kind(), Some(FailureKind::Exception));
    assert_eq!(
        result.logs,
        vec![
            "first".to_string(),
            "second 3".to_string(),
            "third".to_string()
        ]
    );
}

/// TC-PORT-CODERT-12: a run reports how long it took.
///
/// No upstream equivalent: its result carries logs, value and error, and the
/// duration is the caller's to measure. tetanus puts it on the result because
/// the tool that renders one has no other clock, and a code result without a
/// duration is the one number every operator asks for first.
///
/// Input: a program that does a measurable amount of arithmetic.
/// Expected: a duration that is not zero and is well under the budget.
#[tokio::test]
async fn a_run_reports_how_long_it_took() {
    let runtime = runtime();
    let result = runtime
        .run(RunRequest::new(
            "let i = 0; while (i < 2000) { i = i + 1; } return i;",
        ))
        .await
        .expect("ran");

    assert_eq!(result.value, Some(json!(2000)));
    assert!(result.duration > std::time::Duration::ZERO);
    assert!(
        result.duration < std::time::Duration::from_secs(2),
        "{:?}",
        result.duration
    );
}

/// TC-PORT-CODERT-13: a namespace member whose name is awkward is an ordinary
/// member.
///
/// Upstream: "exposes binding names that collide with Object.prototype as
/// ordinary functions", "a runtime must treat names like `__proto__` or
/// `constructor` as ordinary own properties".
///
/// The rule is about the *member* names, which are arbitrary strings, as
/// against the *global*, which is a portable identifier. A backend that
/// confused the two would refuse a perfectly good tool name.
///
/// Input: a namespace with members called `constructor`, `__proto__` and
/// `toString`.
/// Expected: each is callable and answers its own value.
#[tokio::test]
async fn a_namespace_member_whose_name_is_awkward_is_an_ordinary_member() {
    let runtime = runtime();
    let tools = Namespace::new("tools")
        .with("constructor", |_| Ok(json!("built")))
        .with("__proto__", |_| Ok(json!("proto")))
        .with("toString", |_| Ok(json!("stringed")));

    let result = runtime
        .run(
            RunRequest::new(
                r#"return [tools.constructor(1), tools.__proto__(1), tools.toString(1)];"#,
            )
            .binding(tools),
        )
        .await
        .expect("ran");

    assert_eq!(result.error, None, "{:?}", result.error);
    assert_eq!(result.value, Some(json!(["built", "proto", "stringed"])));
}

/// TC-PORT-CODERT-14: the seam's own check is shared, so a request valid for
/// one backend is valid for the other.
///
/// Upstream: the whole reason `RESERVED_BINDING_GLOBALS` lives on the seam
/// rather than in a backend.
///
/// Input: `check_bindings` directly, with a good list and a bad one.
/// Expected: the same answers the runtime gave in TC-PORT-CODERT-4, from the
/// function both backends call.
#[test]
fn the_seams_own_check_is_shared_by_every_backend() {
    let good = vec![Namespace::new("tools"), Namespace::new("files")];
    assert!(tetanus_coderuntime::types::check_bindings(&good).is_ok());

    let bad = vec![Namespace::new("console")];
    assert!(matches!(
        tetanus_coderuntime::types::check_bindings(&bad),
        Err(SeamError::BadNamespace { .. })
    ));

    // And the binding itself is just a closure the host owns.
    let counted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&counted);
    let namespace = Namespace::new("tools").with("tick", move |_| {
        seen.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(json!(null))
    });
    assert_eq!(namespace.functions.len(), 1);
    assert_eq!(counted.load(std::sync::atomic::Ordering::Acquire), 0);
}

/// TC-PORT-CODERT-32: a program can survive one call failing, and cannot
/// survive its own budget.
///
/// Upstream: its typed rejection contract - "materializes a typed rejection
/// from a generic namespace descriptor", "rejects the program-side call on a
/// host rejection". A rejection nothing can catch is a rejection a program
/// cannot act on, which is why this and the error shape landed together.
///
/// The second half is the one with teeth: a program that could catch its own
/// timeout could write `while (true) { try { } catch (e) { } }` and never
/// end, which is the containment story undone by two keywords. Only a
/// program-level failure is catchable; a budget, an abort and a full ledger
/// pass straight through.
///
/// Input: a program that catches a failing binding and carries on; then one
/// that tries to catch its own compute budget.
/// Expected: the first completes, with the caught value carrying the message
/// and the failed member's name under the declared property; the second still
/// fails with `timeout`.
#[tokio::test]
async fn a_program_survives_a_failed_call_and_never_its_own_budget() {
    let runtime = runtime();
    let tools = Namespace::new("tools")
        .with("read", |_| Err("no such file".to_string()))
        .with("write", |_| Ok(json!(true)))
        .failing_as("ToolError", "member");

    let caught = runtime
        .run(
            RunRequest::new(
                r#"let outcome = "not reached";
                   try {
                     tools.read({ path: "/nowhere" });
                   } catch (e) {
                     outcome = { message: e.message, failed: e.member, kind: e.kind };
                   }
                   tools.write({ done: true });
                   return outcome;"#,
            )
            .binding(tools),
        )
        .await
        .expect("ran");

    assert_eq!(caught.error, None, "{:?}", caught.error);
    let value = caught.value.expect("a value");
    assert_eq!(
        value.get("failed"),
        Some(&json!("read")),
        "the failed member is carried under the declared property: {value}"
    );
    assert_eq!(value.get("kind"), Some(&json!("exception")));
    assert!(
        value["message"]
            .as_str()
            .is_some_and(|m| m.contains("no such file")),
        "the host's own words survive: {value}"
    );

    // And the budget is not the program's to catch.
    let starved = LocalRuntime::new(Budget {
        fuel: 5_000,
        ..runtime.budget()
    });
    let result = starved
        .run(RunRequest::new(
            "while (true) { try { let x = 1; } catch (e) { let y = 2; } }",
        ))
        .await
        .expect("ran");
    assert_eq!(
        result.kind(),
        Some(FailureKind::Timeout),
        "a program caught its own budget: {:?}",
        result.error
    );
}

/// TC-PORT-CODERT-33: an error shape no backend could serve is misuse.
///
/// Upstream: "rejects malformed or colliding binding error-class
/// declarations", and the `RESERVED_ERROR_MEMBERS` set that says which names
/// those are.
///
/// This is what that set was implemented for. Until the error shape existed it
/// was tested and unused; now a namespace that declares `message` as its
/// member property - which would overwrite the message - or a dunder-form
/// name, or a reserved word as its failure name, is refused before a program
/// runs.
///
/// Input: namespaces declaring `message`, `__init__`, `with_traceback` and a
/// failure name that is a reserved word.
/// Expected: `SeamError::BadNamespace` for each, naming the namespace and
/// saying which rule it broke.
#[tokio::test]
async fn an_error_shape_no_backend_could_serve_is_misuse() {
    let runtime = runtime();

    for (property, expected) in [
        ("message", "error protocol"),
        ("__init__", "dunder-form"),
        ("with_traceback", "error protocol"),
        ("", "needs a name"),
    ] {
        let refused = runtime
            .run(
                RunRequest::new("return 1;").binding(
                    Namespace::new("tools")
                        .with("read", |v| Ok(v.clone()))
                        .failing_as("ToolError", property),
                ),
            )
            .await
            .expect_err("this shape cannot be served");
        let SeamError::BadNamespace { global, why } = &refused else {
            panic!("expected a bad namespace for {property:?}, got {refused:?}");
        };
        assert_eq!(global, "tools");
        assert!(why.contains(expected), "{property:?}: {why}");
    }

    let bad_name = runtime
        .run(
            RunRequest::new("return 1;").binding(
                Namespace::new("tools")
                    .with("read", |v| Ok(v.clone()))
                    .failing_as("lambda", "member"),
            ),
        )
        .await
        .expect_err("a reserved word cannot name a failure");
    assert!(bad_name.to_string().contains("reserved word"), "{bad_name}");
}

/// TC-PORT-CODERT-34: a `try` with no `catch` is refused where it is written.
///
/// No upstream equivalent - its language is TypeScript and the grammar is
/// Node's. The rule is this language's, and it exists because the one thing
/// `try` must never be usable for is swallowing a failure silently: a body
/// whose failure went nowhere would leave a program running on values it
/// never got.
///
/// Input: a program with a bare `try { }`.
/// Expected: an `exception` at parse time, saying what is missing.
#[tokio::test]
async fn a_try_with_no_catch_is_refused_where_it_is_written() {
    let result = runtime()
        .run(RunRequest::new("try { tools.read(1); } return 1;"))
        .await
        .expect("ran");
    assert_eq!(result.kind(), Some(FailureKind::Exception));
    assert!(
        result
            .error
            .as_ref()
            .is_some_and(|failure| failure.message.contains("catch")),
        "{:?}",
        result.error
    );
}
