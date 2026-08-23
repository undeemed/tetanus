//! Test Design Specification: the tool registration surface.
//!
//! Feature under test: `tetanus_toolset` - the one place that says which tools
//! this build offers, the rules that make adding a crate to it safe, and the
//! settings key that selects sources. This is not a port: upstream composes
//! tools through Cordis plugin loading, where a duplicate name is resolved by
//! load order. It is the seam each landed tool crate adds one line to.
//!
//! Approach: the assembly directly, with stand-in sources for the rules, and
//! against the shipped set for the properties that must hold of what actually
//! ships. A case that only used stand-ins would pass on a build whose real
//! sources collide.
//!
//! Features NOT tested here: that the *binary* offers what this composes,
//! which is `crates/cli/tests/toolset.rs`'s - a crate can be right and the
//! program people run can still not offer it, and only a case that execs the
//! binary can tell the difference.
//!
//! Environmental needs: a writable temp directory, because the shipped `fs`
//! source opens a workspace root. No case reaches a network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use serde_json::json;
use tetanus_config::{Config, Document, Layer};
use tetanus_toolset::{key, Assembly, AssemblyError, Composition, Source};
use tetanus_turn::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// A composition rooted somewhere writable, so the `fs` source composes the
/// same way it does under the binary.
fn shipped() -> (tempfile::TempDir, Composition) {
    let root = tempfile::tempdir().expect("temp dir");
    let cx = Composition::catalogue().workspace(root.path());
    (root, cx)
}

/// A tool that exists only to be named.
struct Named(&'static str);

#[async_trait::async_trait]
impl Tool for Named {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.0.into(),
            description: format!("the {} tool", self.0),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(self.0))
    }
}

fn source(name: &'static str, tools: &[&'static str]) -> Source {
    Source::new(
        name,
        "a stand-in source",
        tools
            .iter()
            .map(|tool| Arc::new(Named(tool)) as Arc<dyn Tool>)
            .collect(),
    )
}

fn settings(value: Option<serde_json::Value>) -> Config {
    let mut config = Config::default();
    if let Some(value) = value {
        config.load(
            Layer::File,
            Document::from([(key::SOURCES.to_string(), value)]),
        );
    }
    config
}

/// TC-TOOLSET-1: what this build actually ships composes, and every tool in it
/// has one owner.
///
/// The property that must hold of the shipped set rather than of a fixture:
/// `stock_registry` unwraps, so a duplicate among the real sources would be a
/// panic at startup rather than a test failure, and this is the case that keeps
/// that promise honest as crates land.
///
/// Input: the shipped assembly.
/// Expected: it builds; every tool the registry holds is claimed by exactly one
/// source; and the roster and the registry name the same tools. As each pending
/// crate adds its line, this case is what tells its author immediately that a
/// name collided.
#[test]
fn the_shipped_sources_compose_and_every_tool_has_one_owner() {
    let (_root, cx) = shipped();
    let assembly = Assembly::stock(&cx);
    let roster = assembly.roster();
    let named: Vec<String> = roster
        .iter()
        .flat_map(|(_, _, tools)| tools.clone())
        .collect();

    let registry = Assembly::stock(&cx)
        .build()
        .expect("the shipped set composes");

    let registered: Vec<String> = registry.names().cloned().collect();
    let mut sorted = named.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        named.len(),
        "two shipped sources offer one name: {named:?}"
    );
    assert_eq!(sorted, registered, "the roster and the registry agree");
    for tool in &registered {
        assert!(
            Assembly::stock(&cx).source_of(tool).is_some(),
            "{tool} has an owner"
        );
    }
}

/// TC-TOOLSET-1b: every landed tool crate is actually in the shipped set.
///
/// The case that says "a crate exists" is not the same as "the assembly offers
/// it". A crate can be in the workspace, tested, and reachable from nothing;
/// this names each landed source and one tool it must contribute, so deleting
/// a line from `sources()` fails here rather than going unnoticed until a user
/// asks why the model cannot read a file.
///
/// Input: the shipped roster.
/// Expected: every landed source present, each carrying the tool named. `web`,
/// `lsp` and `mcp` are declared but empty, because all three are opt-in: a
/// source that vanished when a deployment had not configured it would make
/// `tools.sources` mean something different on every host.
#[test]
fn every_landed_tool_crate_is_in_the_shipped_set() {
    let (_root, cx) = shipped();
    let roster = Assembly::stock(&cx).roster();
    let of = |name: &str| {
        roster
            .iter()
            .find(|(source, _, _)| *source == name)
            .unwrap_or_else(|| panic!("the {name} source is composed; roster: {roster:?}"))
            .2
            .clone()
    };

    assert!(of("builtin").contains(&"echo".to_string()));
    assert!(of("exec").contains(&"shell".to_string()));
    assert!(of("fs").contains(&"read".to_string()));
    assert!(of("features").contains(&"todo_write".to_string()));
    // Declared and empty until the document says otherwise.
    assert_eq!(of("web"), Vec::<String>::new());
    assert_eq!(of("lsp"), Vec::<String>::new());
    assert_eq!(of("mcp"), Vec::<String>::new());
}

/// TC-TOOLSET-11: a document that names a language server gets the `lsp` tool,
/// and one that does not gets a source with nothing in it.
///
/// The `lsp` client can drive any server that speaks the protocol, so which one
/// to run is a fact about the project rather than about the harness. A default
/// would answer a model's first question by starting the wrong program, and an
/// absent source would make `tools.sources` mean something different on a host
/// that had configured one.
///
/// Input: the shipped assembly, then the same with `lsp.server` set.
/// Expected: no tools, then exactly `lsp`.
#[test]
fn the_language_server_tool_appears_when_a_document_names_a_server() {
    let (_root, bare) = shipped();
    let (_root2, mut configured) = shipped();
    let mut document = Config::default();
    document.load(
        Layer::File,
        Document::from([(
            key::LSP_SERVER.to_string(),
            serde_json::Value::String("rust-analyzer".into()),
        )]),
    );
    configured.settings = Arc::new(document);

    let without = Assembly::stock(&bare).roster();
    let with = Assembly::stock(&configured).roster();

    let of = |roster: &Vec<(&'static str, &'static str, Vec<String>)>, name: &str| {
        roster
            .iter()
            .find(|(source, _, _)| *source == name)
            .map(|(_, _, tools)| tools.clone())
            .unwrap_or_default()
    };
    assert_eq!(of(&without, "lsp"), Vec::<String>::new());
    assert_eq!(of(&with, "lsp"), vec!["lsp".to_string()]);
}

/// TC-TOOLSET-1c: a session's registry offers what the catalogue advertised.
///
/// The drift the binary can actually suffer. Its listing is built against a
/// composition with no session and its turns are built against one per
/// session, so the two run different code paths through `sources()`; a source
/// that quietly contributed nothing without a journal would make `tetanus
/// tools` a lie.
///
/// Input: a catalogue composition and a session composition on one document.
/// Expected: the same tool names.
#[test]
fn a_sessions_registry_offers_what_the_catalogue_advertised() {
    let root = tempfile::tempdir().expect("temp dir");
    let listed: Vec<String> =
        tetanus_toolset::registry(&Composition::catalogue().workspace(root.path()))
            .expect("composes")
            .names()
            .cloned()
            .collect();

    let bus = tetanus_core::EventBus::new();
    let log = tetanus_session::JsonlSessionLog::create("s1", root.path().join("s.jsonl"), bus)
        .expect("journal");
    let session: Vec<String> = tetanus_toolset::registry(
        &Composition::for_session(
            tetanus_turn::interrupt::Interrupt::new(),
            log as Arc<dyn tetanus_session::SessionLog>,
            "s1",
        )
        .workspace(root.path()),
    )
    .expect("composes")
    .names()
    .cloned()
    .collect();

    assert_eq!(listed, session);
    assert!(listed.contains(&"read".to_string()), "{listed:?}");
}

/// TC-TOOLSET-2: the engine's offline default is the assembly's `builtin`
/// source and nothing private.
///
/// The engine deliberately does *not* compose the shipped set: it has no
/// session, so the file tools would key their observations on nobody and the
/// feature tools would fold over a journal that is not a session's, and
/// `crates/engine` would gain a dependency on every tool crate - which is the
/// line `ARCHITECTURE.md` §4.2 draws when it says nothing depends on
/// `tetanus-fs`. What it must not do is grow a *private* expression of its
/// own, which is the drift that started this crate.
///
/// Input: the engine's default tool registry, and the `builtin` source.
/// Expected: the same names. Adding a tool to the engine's default without
/// adding it to `builtin`, or renaming the source, fails here.
#[test]
fn the_engine_default_holds_the_builtin_source_and_nothing_private() {
    let (_root, cx) = shipped();
    let from_engine: Vec<String> = tetanus_engine::EngineConfig::default()
        .tools
        .names()
        .cloned()
        .collect();

    let builtin: Vec<String> = Assembly::stock(&cx)
        .only(["builtin"])
        .expect("the builtin source is shipped")
        .build()
        .expect("composes")
        .names()
        .cloned()
        .collect();

    assert_eq!(from_engine, builtin);
    assert!(
        !from_engine.is_empty(),
        "a build with no tools is not this one"
    );
}

/// TC-TOOLSET-3: two sources offering one name are refused, naming both.
///
/// The rule that makes adding a crate safe. `ToolRegistry::register` keys by
/// name and the last one wins, which is right for a registry and wrong for an
/// assembly: the model would be offered one tool's schema and run the other's
/// body, and nothing would say so.
///
/// Input: two sources that both offer `read`.
/// Expected: refused, naming the tool and both sources, with the message saying
/// what to do about it. Today this is unreachable with one source; the day
/// `crates/fs` and an MCP server both offer `read`, it is the first thing its
/// author sees.
#[test]
fn two_sources_offering_one_name_are_refused_and_both_are_named() {
    let assembly = Assembly::of(vec![
        source("fs", &["read", "write"]),
        source("mcp", &["read", "fetch"]),
    ]);

    let Err(refused) = assembly.build() else {
        panic!("one name offered by two sources must not compose")
    };

    assert_eq!(
        refused,
        AssemblyError::Duplicate {
            name: "read".into(),
            first: "fs",
            second: "mcp",
        }
    );
    let said = refused.to_string();
    assert!(
        said.contains("\"fs\"") && said.contains("\"mcp\""),
        "{said}"
    );
    assert!(said.contains("Rename one"), "it says what to do: {said}");
}

/// TC-TOOLSET-4: a source contributes all of its tools, and the registry holds
/// them in canonical order.
///
/// Input: two sources of two tools each.
/// Expected: all four in the registry, in name order whatever order the sources
/// declared them. The order the model reads them in is `tools.order`'s to
/// settle; what this fixes is that the set is reproducible.
#[test]
fn every_tool_of_every_composed_source_reaches_the_registry() {
    let assembly = Assembly::of(vec![
        source("second", &["zeta", "alpha"]),
        source("first", &["middle", "beta"]),
    ]);

    let registry = assembly.build().expect("composes");

    let names: Vec<String> = registry.names().cloned().collect();
    assert_eq!(names, ["alpha", "beta", "middle", "zeta"]);
}

/// TC-TOOLSET-5: `only` keeps what it names, in the order the build declares.
///
/// Input: three sources, two named in the other order.
/// Expected: those two, in declaration order, and the third gone. A document is
/// not a place to express a precedence nothing reads, so two deployments naming
/// the same sources get the same registry however they wrote the list.
#[test]
fn only_keeps_what_it_names_in_the_order_the_build_declares() {
    let assembly = Assembly::of(vec![
        source("fs", &["read"]),
        source("exec", &["run"]),
        source("web", &["fetch"]),
    ]);

    let kept = assembly.only(["web", "fs"]).expect("both exist");

    assert_eq!(kept.names(), ["fs", "web"]);
    let names: Vec<String> = kept.build().expect("composes").names().cloned().collect();
    assert_eq!(names, ["fetch", "read"]);
}

/// TC-TOOLSET-6: naming a source this build does not ship is refused, and says
/// what it does ship.
///
/// Input: `only` naming a source that is not there.
/// Expected: refused, listing the available names. A deployment that misspells
/// `fs` should not silently get a harness with fewer tools than it asked for -
/// that failure appears later, as a model that cannot read a file, and nothing
/// connects it back to the typo.
#[test]
fn an_unknown_source_is_refused_and_lists_what_there_is() {
    let assembly = Assembly::of(vec![source("fs", &["read"]), source("exec", &["run"])]);

    let refused = assembly.only(["fs", "fsx"]).expect_err("no such source");

    assert!(
        matches!(&refused, AssemblyError::UnknownSource { name, .. } if name == "fsx"),
        "{refused}"
    );
    let said = refused.to_string();
    assert!(said.contains("fs, exec"), "{said}");
}

/// TC-TOOLSET-7: `tools.sources` selects sources, and an absent key selects
/// all.
///
/// Input: no key; a list naming one source; and an empty list.
/// Expected: everything, then that one, then nothing. Naming none is a
/// legitimate deployment - a harness that only converses - and it is strange to
/// arrive at by accident, so it has to be written down rather than being what
/// an absent key means.
#[test]
fn the_settings_key_selects_sources_and_an_absent_key_selects_all() {
    let build = |value: Option<serde_json::Value>| {
        Assembly::of(vec![source("fs", &["read"]), source("exec", &["run"])])
            .configured(&settings(value))
            .expect("configured")
            .names()
    };

    assert_eq!(build(None), ["fs", "exec"]);
    assert_eq!(build(Some(json!(["exec"]))), ["exec"]);
    assert_eq!(build(Some(json!([]))), Vec::<&str>::new());
}

/// TC-TOOLSET-8: a `tools.sources` that is not a list of names is refused,
/// naming the key.
///
/// Input: the key written as a string, as a list holding a number, and naming a
/// source that does not exist.
/// Expected: `ConfigError::BadValue` naming `tools.sources` each time. The
/// engine's config faults all carry the key a reader has to edit, and this one
/// joins them rather than inventing its own shape.
#[test]
fn a_malformed_sources_setting_is_refused_and_names_the_key() {
    let assembly = || Assembly::of(vec![source("fs", &["read"])]);

    for value in [json!("fs"), json!(["fs", 7]), json!(["nope"])] {
        let refused = assembly()
            .configured(&settings(Some(value.clone())))
            .expect_err("refused");

        assert!(
            refused.to_string().starts_with(key::SOURCES),
            "{value}: {refused}"
        );
    }
}

/// TC-TOOLSET-9: the roster says which source a tool came from.
///
/// Input: two sources, and a lookup per tool.
/// Expected: each tool attributed to its own source, and a tool nobody offers
/// attributed to none. With forty tools on offer, "why is this here" is a
/// question a user asks, and the answer has to come from the same place the
/// tools do rather than from a table somebody maintains beside it.
#[test]
fn the_roster_says_where_each_tool_came_from() {
    let assembly = Assembly::of(vec![
        source("fs", &["read", "write"]),
        source("exec", &["run"]),
    ]);

    let roster = assembly.roster();

    assert_eq!(roster[0].0, "fs");
    assert_eq!(roster[0].2, ["read", "write"]);
    assert_eq!(assembly.source_of("run"), Some("exec"));
    assert_eq!(assembly.source_of("read"), Some("fs"));
    assert_eq!(assembly.source_of("nothing"), None);
}

/// TC-TOOLSET-10: a composed source's tools are callable, not just listed.
///
/// The listing and the dispatch come from one registry, so this is the case
/// that would fail if the assembly ever built a roster separately from what it
/// registered.
///
/// Input: a composed registry, dispatched through.
/// Expected: the tool runs and answers. A registry that listed a tool it could
/// not dispatch is the exact failure mode `crates/cli`'s comment about "what is
/// listed and what is callable" has always warned about.
#[tokio::test]
async fn a_composed_tool_is_callable_and_not_merely_listed() {
    let registry = Assembly::of(vec![source("fs", &["read"])])
        .build()
        .expect("composes");

    let outcome = registry
        .execute(&tetanus_turn::tools::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({}),
        })
        .await
        .expect("dispatched");

    assert!(outcome.ok);
    assert_eq!(outcome.content, "read");
}
