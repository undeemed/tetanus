//! Test Design Specification: a turn that runs a program and uses what it
//! produced.
//!
//! Feature under test: `tetanus_coderuntime::tool::CodeTool` through the
//! ordinary tool pipeline - registered like any other tool, dispatched by the
//! turn engine, its failures contained the way every tool failure is.
//! Upstream's equivalent is Code Mode driving `ctx.codeRuntime` through its
//! tool runtime; the shape restated here is the tetanus one, because the
//! pipeline is what the claim is about.
//!
//! Approach: a real turn engine over a temporary journal and a scripted model,
//! exactly as `crates/mcp/tests/bridge_turn.rs` does for a server's tools. A
//! tool asserted only against its own `execute` would not show that the turn
//! survives, which is the half that matters.
//!
//! Environmental needs: a writable temp directory. No network, no key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_coderuntime::tool::CodeTool;
use tetanus_coderuntime::types::Namespace;
use tetanus_coderuntime::{Budget, LocalRuntime};
use tetanus_turn::events::StopReason;
use tetanus_turn::tools::{EchoTool, Tool, ToolRegistry};

mod harness;
use harness::{ModelAsking, TurnFixture};

fn runtime() -> Arc<LocalRuntime> {
    Arc::new(LocalRuntime::new(Budget {
        fuel: 200_000,
        wall: Duration::from_millis(500),
        max_output_bytes: 4096,
        reap_grace: Duration::from_millis(200),
    }))
}

/// TC-PORT-CODERT-25: a turn runs a program and reads its value in the next
/// step.
///
/// Upstream: Code Mode's whole purpose - the model writes the control flow and
/// the harness runs it once, instead of the model driving three tool calls
/// across three steps.
///
/// Input: a registry holding `run_code` beside `echo`, a model that asks for a
/// program calling a host binding in a loop, and one turn.
/// Expected: the turn ends naturally; the journal holds one successful
/// `tool/result` under `run_code` carrying the program's value and its logs;
/// and the model's closing message repeats the value, which is the proof it
/// reached the next step.
#[tokio::test]
async fn a_turn_runs_a_program_and_reads_its_value_in_the_next_step() {
    let counted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&counted);
    let tools = Namespace::new("host").with("double", move |argument| {
        seen.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let number = argument.as_f64().unwrap_or_default();
        Ok(json!(number * 2.0))
    });

    let registry = ToolRegistry::new()
        .with(Arc::new(EchoTool))
        .with(Arc::new(CodeTool::new(runtime()).binding(tools)));

    let turn = TurnFixture::new(
        "code-turn",
        registry,
        Arc::new(ModelAsking {
            tool: CodeTool::NAME.to_string(),
            arguments: json!({
                "program": r#"let total = 0;
                              let i = 1;
                              while (i <= 4) { total = total + host.double(i); i = i + 1; }
                              log("added " + (i - 1) + " numbers");
                              return { total: total };"#,
            }),
        }),
    );

    let outcome = turn
        .engine
        .run_turn("add the first four doubled numbers")
        .await
        .expect("the turn runs");

    assert_eq!(outcome.reason, StopReason::Natural);
    let results = turn.tool_results();
    assert_eq!(results.len(), 1);
    let (name, ok, content) = &results[0];
    assert_eq!(name, CodeTool::NAME);
    assert!(ok, "the program failed: {content}");
    assert!(
        content.contains("\"total\":20"),
        "the value is in the result: {content}"
    );
    assert!(
        content.contains("added 4 numbers"),
        "the logs are in the result: {content}"
    );
    assert_eq!(
        counted.load(std::sync::atomic::Ordering::Acquire),
        4,
        "the binding was called once per iteration"
    );
    assert!(
        outcome.content.contains("\"total\":20"),
        "the model read the value in its next step: {}",
        outcome.content
    );
}

/// TC-PORT-CODERT-26: a program that will not stop fails its call, and the
/// turn goes on.
///
/// Upstream: the containment its tool layer gives a failed `run` - an error
/// result, not a thrown turn.
///
/// This is the acceptance criterion stated as behaviour: the infinite loop is
/// stopped, the model is told which class of thing went wrong, and the turn
/// reaches its next step.
///
/// Input: a model asking for `while (true) { }` under a small budget.
/// Expected: the turn ends naturally; the one `tool/result` is a failure whose
/// text opens with `[timeout]`; the model's closing message repeats it; and no
/// worker is left running.
#[tokio::test]
async fn a_program_that_will_not_stop_fails_its_call_and_the_turn_goes_on() {
    let runtime = runtime();
    let registry = ToolRegistry::new()
        .with(Arc::new(EchoTool))
        .with(Arc::new(CodeTool::new(
            Arc::clone(&runtime) as Arc<dyn tetanus_coderuntime::types::CodeRuntime>
        )));

    let turn = TurnFixture::new(
        "code-runaway",
        registry,
        Arc::new(ModelAsking {
            tool: CodeTool::NAME.to_string(),
            arguments: json!({ "program": "while (true) { }" }),
        }),
    );

    let outcome = turn
        .engine
        .run_turn("loop for ever")
        .await
        .expect("the turn survives the program");

    assert_eq!(
        outcome.reason,
        StopReason::Natural,
        "the turn was not ended"
    );
    let (name, ok, content) = turn.tool_results().remove(0);
    assert_eq!(name, CodeTool::NAME);
    assert!(!ok, "the call is recorded as failed");
    assert!(
        content.contains("[timeout]"),
        "the class is what the model reads first: {content}"
    );
    assert!(
        outcome.content.contains("[timeout]"),
        "the model read the failure: {}",
        outcome.content
    );

    for _ in 0..200 {
        if runtime.live_workers() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "a worker outlived the turn: {} live",
        runtime.live_workers()
    );
}

/// TC-PORT-CODERT-27: the tool describes the bindings it was given.
///
/// Upstream: its Code Mode presentation generates typed stubs from the same
/// descriptors, for the same reason - a model offered a binding it was never
/// told about will not call it, and one told about a binding that is not there
/// will.
///
/// Input: a tool composed with two namespaces.
/// Expected: the schema names the tool, requires `program`, and its
/// description lists every binding member as the program would call it.
#[test]
fn the_tool_describes_the_bindings_it_was_given() {
    let tool = CodeTool::new(runtime())
        .binding(Namespace::new("host").with("double", |v| Ok(v.clone())))
        .binding(
            Namespace::new("files")
                .with("read", |v| Ok(v.clone()))
                .with("write", |v| Ok(v.clone())),
        );
    let schema = tool.schema();

    assert_eq!(schema.name, "run_code");
    assert_eq!(
        schema.parameters.pointer("/required/0"),
        Some(&json!("program"))
    );
    for member in [
        "host.double(argument)",
        "files.read(argument)",
        "files.write(argument)",
    ] {
        assert!(
            schema.description.contains(member),
            "the model is told about {member}: {}",
            schema.description
        );
    }
}

/// TC-PORT-CODERT-28: a call with nothing to run is refused before a worker
/// exists.
///
/// Upstream: its tool layer validates arguments before reaching the runtime.
///
/// Input: `run_code` with no program, and with a blank one.
/// Expected: `InvalidArguments` naming the field in both cases, and no run
/// started.
#[tokio::test]
async fn a_call_with_nothing_to_run_is_refused_before_a_worker_exists() {
    let runtime = runtime();
    let tool =
        CodeTool::new(Arc::clone(&runtime) as Arc<dyn tetanus_coderuntime::types::CodeRuntime>);

    for arguments in [json!({}), json!({ "program": "   " })] {
        let refused = tool
            .execute(&arguments)
            .await
            .expect_err("there is nothing to run");
        assert!(
            refused.to_string().contains("program"),
            "the field is named: {refused}"
        );
    }
    assert_eq!(runtime.live_workers(), 0);
}

/// TC-PORT-CODERT-29: a program calls the harness's own tools, and the turn
/// spends one step on all of them.
///
/// Upstream: Code Mode itself - its bindings are async because a program
/// calling a tool is the thing the seam exists for, and its worker awaits
/// across a port to do it.
///
/// This is the gap the first pass named and left open: bindings were
/// synchronous, so nothing could wire the tool registry in, and `run_code`
/// could only call closures a composer wrote by hand.
///
/// Input: a registry holding `echo`, a `tools` namespace built from it, and a
/// program that calls `echo` three times in a loop and returns what came back.
/// Expected: one successful `tool/result` for the whole program; the three
/// results inside its value; and the turn still one turn - three tool calls
/// that cost one step rather than three.
#[tokio::test]
async fn a_program_calls_the_harnesss_own_tools_in_one_step() {
    let registry = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));
    let namespace = tetanus_coderuntime::tool::tools_namespace(
        "tools",
        Arc::clone(&registry),
        &["echo".to_string()],
    )
    .expect("echo is registered");

    let outer = ToolRegistry::new()
        .with(Arc::new(EchoTool))
        .with(Arc::new(CodeTool::new(runtime()).binding(namespace)));

    let turn = TurnFixture::new(
        "code-tools",
        outer,
        Arc::new(ModelAsking {
            tool: CodeTool::NAME.to_string(),
            arguments: json!({
                "program": r#"let out = [];
                              let i = 0;
                              while (i < 3) {
                                let said = tools.echo({ text: "line " + i });
                                out = push(out, said.content);
                                i = i + 1;
                              }
                              return out;"#,
            }),
        }),
    );

    let outcome = turn.engine.run_turn("echo three lines").await.expect("ran");
    assert_eq!(outcome.reason, StopReason::Natural);

    let results = turn.tool_results();
    assert_eq!(
        results.len(),
        1,
        "three tool calls inside one program are one call to the turn: {results:?}"
    );
    let (name, ok, content) = &results[0];
    assert_eq!(name, CodeTool::NAME);
    assert!(ok, "{content}");
    for line in ["line 0", "line 1", "line 2"] {
        assert!(content.contains(line), "{line} is missing from {content}");
    }
}

/// TC-PORT-CODERT-30: a tool that says no is a value the program reads, and a
/// tool that cannot run is the program's failure.
///
/// Upstream: "bridges binding calls both ways and rejects the program-side
/// call on a host rejection" - with the distinction this crate adds, because
/// a tetanus tool has two ways of not working and a program should branch on
/// one and stop on the other.
///
/// Input: a program calling `echo` with no `text` - which the tool refuses as
/// invalid arguments - and one calling a member the namespace does not carry.
/// Expected: the first ends the program with the tool's own words; the second
/// names the member. Both are failures the model reads, and neither ends the
/// turn.
#[tokio::test]
async fn a_tool_that_says_no_and_one_that_cannot_run_are_told_apart() {
    let registry = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));
    let namespace =
        tetanus_coderuntime::tool::tools_namespace("tools", registry, &["echo".to_string()])
            .expect("echo is registered");
    let tool = CodeTool::new(runtime()).binding(namespace);

    let refused = tool
        .execute(&json!({ "program": "return tools.echo({ wrong: 1 });" }))
        .await
        .expect_err("the tool refused the arguments");
    assert!(
        refused.to_string().contains("missing `text`"),
        "the tool's own words reach the model: {refused}"
    );

    let absent = tool
        .execute(&json!({ "program": "return tools.write({ text: 1 });" }))
        .await
        .expect_err("no such member");
    assert!(
        absent.to_string().contains("write"),
        "the member is named: {absent}"
    );
}

/// TC-PORT-CODERT-31: the tool that runs programs is not offered to them.
///
/// No upstream equivalent: its Code Mode composes the binding list from a
/// tool set that never contains the code runtime itself, so the question does
/// not arise there. It arises here because the namespace is built from this
/// harness's own registry, and a program that can call `run_code` can nest
/// runs until the machine gives - each one holding a worker thread while it
/// waits for the next.
///
/// Input: a namespace asked to offer `run_code`, and one asked for a tool
/// nobody registered.
/// Expected: both refused, before any program exists, each saying why.
#[test]
fn the_tool_that_runs_programs_is_not_offered_to_them() {
    let registry = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));

    let nested = tetanus_coderuntime::tool::tools_namespace(
        "tools",
        Arc::clone(&registry),
        &[CodeTool::NAME.to_string()],
    )
    .expect_err("a program cannot be given the tool running it");
    assert!(
        nested.to_string().contains("nest runs"),
        "the refusal says why: {nested}"
    );

    let missing =
        tetanus_coderuntime::tool::tools_namespace("tools", registry, &["ghost".to_string()])
            .expect_err("no such tool");
    assert!(missing.to_string().contains("ghost"), "{missing}");
    assert!(
        missing.to_string().contains("echo"),
        "the refusal lists what there is: {missing}"
    );
}
