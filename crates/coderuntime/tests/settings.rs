//! Test Design Specification: turning the code runtime on from a document.
//!
//! Feature under test: `tetanus_coderuntime::settings` - whether a deployment
//! gets a `run_code` tool at all, on which backend, under which budgets, and
//! with which of its own tools offered to programs.
//!
//! This is completeness rather than an upstream port, and the cases say so:
//! upstream's runtime is a Cordis plugin, so loading it is already the
//! deployment's choice and its configuration is the plugin's. A tetanus
//! registry is compiled in, so without this nothing could turn the crate on
//! and every case in the other files would be exercising a capability no
//! deployment could reach.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;
use std::time::Duration;

use tetanus_coderuntime::remote::double::ScriptedSandbox;
use tetanus_coderuntime::settings::{self, key};
use tetanus_config::{Config, Layer};
use tetanus_turn::tools::{EchoTool, ToolRegistry};

fn document(pairs: &[(&str, serde_json::Value)]) -> Config {
    let mut config = Config::default();
    for (key, value) in pairs {
        config.set(key, value.clone(), Layer::File);
    }
    config
}

/// The refusal a call produced. `expect_err` needs the success type to be
/// printable, and a registered tool is a trait object that is not.
fn refusal<T>(result: Result<T, tetanus_config::ConfigError>) -> tetanus_config::ConfigError {
    match result {
        Err(fault) => fault,
        Ok(_) => panic!("expected a refusal, and the call succeeded"),
    }
}

fn registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)))
}

/// TC-CODE-SET-1: no document, no code runtime.
///
/// A capability a deployment did not ask for should not appear because a crate
/// was compiled in - the same rule `crates/web` follows about the network, and
/// for a stronger reason: this one runs code a model wrote.
///
/// Input: an empty document, and one that turns it on.
/// Expected: no tool, then `run_code`.
#[test]
fn no_document_no_code_runtime() {
    assert!(settings::tool(&Config::default(), registry(), None, None)
        .expect("read")
        .is_none());

    let on = document(&[(key::ENABLED, serde_json::json!(true))]);
    let tool = settings::tool(&on, registry(), None, None)
        .expect("read")
        .expect("a tool");
    assert_eq!(tool.schema().name, "run_code");
}

/// TC-CODE-SET-2: the budgets are the document's.
///
/// Input: a document naming each budget, and one naming a fuel of zero.
/// Expected: the values as written with the defaults under them, and a refusal
/// naming the key for the impossible one.
#[test]
fn the_budgets_are_the_documents() {
    let written = document(&[
        (key::FUEL, serde_json::json!(1234)),
        (key::WALL, serde_json::json!(2500)),
        (key::MAX_OUTPUT, serde_json::json!(4096)),
    ]);
    let budget = settings::budget(&written).expect("read");
    assert_eq!(budget.fuel, 1234);
    assert_eq!(budget.wall, Duration::from_millis(2500));
    assert_eq!(budget.max_output_bytes, 4096);
    assert_eq!(
        budget.reap_grace,
        tetanus_coderuntime::Budget::default().reap_grace,
        "what the document did not say keeps the default"
    );

    let impossible = document(&[(key::FUEL, serde_json::json!(0))]);
    let refused = settings::budget(&impossible).expect_err("a program with no fuel runs nothing");
    assert!(refused.to_string().contains(key::FUEL), "{refused}");
}

/// TC-CODE-SET-3: the tools a program may call are named one by one.
///
/// A list that said "all of them" would grow a member every time a plugin
/// registered one, which is a decision nobody made.
///
/// Input: a document offering `echo`; then one offering a tool nobody
/// registered; then one offering `run_code` itself.
/// Expected: the first describes `tools.echo` to the model; the other two are
/// refused, naming the key and the reason.
#[test]
fn the_tools_a_program_may_call_are_named_one_by_one() {
    let offered = document(&[
        (key::ENABLED, serde_json::json!(true)),
        (key::TOOLS, serde_json::json!(["echo"])),
    ]);
    let tool = settings::tool(&offered, registry(), None, None)
        .expect("read")
        .expect("a tool");
    assert!(
        tool.schema().description.contains("tools.echo(argument)"),
        "the model is told what it can call: {}",
        tool.schema().description
    );

    for (list, expected) in [
        (serde_json::json!(["ghost"]), "ghost"),
        (serde_json::json!(["run_code"]), "nest runs"),
    ] {
        let bad = document(&[(key::ENABLED, serde_json::json!(true)), (key::TOOLS, list)]);
        let refused = refusal(settings::tool(&bad, registry(), None, None));
        assert!(refused.to_string().contains(key::TOOLS), "{refused}");
        assert!(refused.to_string().contains(expected), "{refused}");
    }
}

/// TC-CODE-SET-4: asking for the remote backend without wiring one is a
/// mistake, not a quiet fallback.
///
/// A document that said "run this somewhere else" and got a local run would be
/// running a model's program on the harness's own machine, which is the one
/// substitution nobody would want made silently.
///
/// Input: `code.remote.enabled` with no provider; then the same with one.
/// Expected: a refusal naming the key; then a tool whose runtime is a
/// container.
#[test]
fn asking_for_the_remote_backend_without_wiring_one_is_a_mistake() {
    let asked = document(&[
        (key::ENABLED, serde_json::json!(true)),
        (key::REMOTE_ENABLED, serde_json::json!(true)),
        (key::REMOTE_KEY, serde_json::json!("a-key")),
    ]);

    let refused = refusal(settings::tool(&asked, registry(), None, None));
    assert!(
        refused.to_string().contains(key::REMOTE_ENABLED),
        "{refused}"
    );

    let provider = Arc::new(ScriptedSandbox::default());
    let tool = settings::tool(&asked, registry(), Some(provider), None)
        .expect("read")
        .expect("a tool");
    assert_eq!(tool.schema().name, "run_code");

    // And the key falls back to the environment, as every other credential in
    // this workspace does.
    let unkeyed = document(&[
        (key::ENABLED, serde_json::json!(true)),
        (key::REMOTE_ENABLED, serde_json::json!(true)),
    ]);
    let config = settings::remote_config(&unkeyed, Some("from-the-environment")).expect("read");
    assert_eq!(config.api_key.as_deref(), Some("from-the-environment"));
}
