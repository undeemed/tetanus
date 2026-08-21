//! Test Design Specification: named agent presets, ported.
//!
//! Feature under test: `tetanus_engine::preset` - the roster read out of the
//! settings document and out of preset directories - and what applying one
//! does to a session: the model it runs on, the tools it may call, the prompt
//! it opens with, and the persona it carries. Upstream pins these in
//! `packages/preset/agent-presets/tests/{settings,session,mount}.spec.ts` and
//! `packages/preset/persona/tests/persona.spec.ts`.
//!
//! Approach: a real engine over a temporary sessions root and the mock
//! provider, driven through `session.create` and `agent.prompt` - the calls a
//! surface makes. A preset that was asserted by reading the roster alone would
//! be a preset nobody had applied to anything.
//!
//! What is not restated, and why. Upstream composes a preset by mounting a
//! Cordis plugin tree per session, so most of its `mount.spec.ts` is about
//! that machinery - a row that fails to load, a service published
//! process-globally, an isolate realm, a subtree torn down. A tetanus preset
//! names settings, not plugins, and the registry it narrows is settled at
//! boot, so those cases have nothing to restate; `docs/parity.md` carries the
//! difference. Its authoring half - copying a shipped preset into a writable
//! root, tightening modes, deleting - needs a write path `crates/config` does
//! not have. Its live switch of a running composition is deliberately not
//! served: this reads the preset once, at creation, and TC-PORT-PRESET-5 pins
//! that.
//!
//! Environmental needs: a writable temp directory. No network, no key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_config::preset::AgentPreset;
use tetanus_config::{Config, Layer};
use tetanus_core::EffectHandle;
use tetanus_engine::preset::{roster, PresetError, Roster};
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_turn::events::AssemblePrompt;
use tetanus_turn::llm::mock;
use tetanus_turn::tools::{EchoTool, Tool, ToolMode, ToolOutcome, ToolRegistry, ToolSchema};

/// A second tool, so a preset has something to leave out.
struct NoteTool;

#[async_trait::async_trait]
impl Tool for NoteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "note".into(),
            description: "Write a note nobody reads.".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn mode(&self, _arguments: &serde_json::Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(
        &self,
        _arguments: &serde_json::Value,
    ) -> Result<ToolOutcome, tetanus_turn::tools::ToolError> {
        Ok(ToolOutcome::ok("noted"))
    }
}

/// A document holding two inline presets: one that narrows the tools and names
/// a model, one that only speaks.
fn document() -> Config {
    let mut config = Config::default();
    for (key, value) in [
        (
            "presets.fast.model.default",
            serde_json::json!("fast-model"),
        ),
        (
            "presets.fast.agent.tools",
            serde_json::json!(["echo".to_string()]),
        ),
        (
            "presets.fast.agent.prompt",
            serde_json::json!("Answer in one line."),
        ),
        (
            "presets.fast.agent.persona",
            serde_json::json!("You are Quick, who never elaborates."),
        ),
        (
            "presets.thorough.model.default",
            serde_json::json!("thorough-model"),
        ),
        ("presets.thorough.agent.max_steps", serde_json::json!(20)),
    ] {
        config.set(key, value, Layer::File);
    }
    config
}

fn engine(presets: Roster, root: &TempDir) -> HarnessEngine {
    HarnessEngine::new(EngineConfig {
        sessions_root: root.path().to_path_buf(),
        presets,
        tools: Arc::new(
            ToolRegistry::new()
                .with(Arc::new(EchoTool))
                .with(Arc::new(NoteTool)),
        ),
        ..EngineConfig::default()
    })
}

fn created(preset: Option<&str>) -> SessionCreateParams {
    SessionCreateParams {
        session_id: None,
        path: None,
        provider: None,
        model: None,
        max_steps: None,
        preset: preset.map(str::to_string),
    }
}

/// TC-PORT-PRESET-1: a preset written in the settings document is a preset the
/// engine composes.
///
/// Upstream: "the preset roster lists every root's presets", "exposes the
/// configured default id".
///
/// Input: a document with two inline presets and a default.
/// Expected: both ids in the roster, each carrying what it said, and the
/// default reported.
#[test]
fn a_preset_written_in_the_settings_document_is_one_the_engine_composes() {
    let mut config = document();
    config.set(
        "presets.default",
        serde_json::json!("thorough"),
        Layer::File,
    );
    let roster = roster(&config).expect("the document reads");

    assert_eq!(
        roster.ids(),
        vec!["fast".to_string(), "thorough".to_string()]
    );
    assert_eq!(roster.default_id(), Some("thorough"));
    assert_eq!(
        roster.get("fast"),
        Some(&AgentPreset {
            model: Some("fast-model".to_string()),
            provider: None,
            max_steps: None,
            tools: Some(vec!["echo".to_string()]),
            prompt: Some("Answer in one line.".to_string()),
            persona: Some("You are Quick, who never elaborates.".to_string()),
        })
    );
    assert_eq!(
        roster.get("thorough").and_then(|p| p.max_steps),
        Some(20),
        "a preset that names only some keys is a preset"
    );
}

/// TC-PORT-PRESET-2: a preset directory is read with the same vocabulary as an
/// inline one, and the document wins.
///
/// Upstream: "lists every root's presets", "takes the user default over the
/// composition default".
///
/// A deployment ships preset directories and a user overrides one in their own
/// document; the nearer definition has to win, exactly as the trust order of
/// the roots works inside the roster itself.
///
/// Input: a root holding `fast` and `shipped`, and a document redefining
/// `fast`.
/// Expected: `shipped` as the directory wrote it, and `fast` as the document
/// wrote it.
#[test]
fn a_preset_directory_is_read_with_the_same_vocabulary_and_the_document_wins() {
    let root = tempfile::tempdir().expect("temp dir");
    for (id, model) in [("fast", "directory-model"), ("shipped", "shipped-model")] {
        let dir = root.path().join(id);
        std::fs::create_dir_all(&dir).expect("preset dir");
        std::fs::write(
            dir.join("settings.json"),
            serde_json::to_string(&serde_json::json!({ "model.default": model })).expect("json"),
        )
        .expect("preset document");
    }

    let mut config = document();
    config.set(
        "presets.roots",
        serde_json::json!([root.path().display().to_string()]),
        Layer::File,
    );
    let roster = roster(&config).expect("the roster reads");

    assert_eq!(
        roster.get("shipped").and_then(|p| p.model.clone()),
        Some("shipped-model".to_string())
    );
    assert_eq!(
        roster.get("fast").and_then(|p| p.model.clone()),
        Some("fast-model".to_string()),
        "the document's own definition wins over the directory's"
    );
}

/// TC-PORT-PRESET-3: selecting a preset changes the model and the tools that
/// session may call.
///
/// Upstream: "composes an agent from a preset", "gives each session only its
/// own preset", "lets two sessions share one preset without colliding".
///
/// This is the acceptance the feature exists for, and both halves are asserted
/// on the same two sessions: a preset that changed the model but not the tools
/// would be a setting, not an agent.
///
/// Input: one engine, two sessions, one composed from `fast` and one from
/// nothing.
/// Expected: the first runs on `fast-model` and may call only `echo`; the
/// second runs on the engine default and may call both tools.
#[tokio::test]
async fn selecting_a_preset_changes_the_model_and_the_tools_that_session_may_call() {
    let root = tempfile::tempdir().expect("temp dir");
    let engine = engine(roster(&document()).expect("roster"), &root);

    let fast = engine
        .session_create(created(Some("fast")))
        .await
        .expect("created");
    let plain = engine.session_create(created(None)).await.expect("created");

    assert_eq!(fast.model, "fast-model");
    assert_eq!(plain.model, mock::MODEL);

    assert_eq!(
        engine.session_tools(&fast.session_id).expect("tools"),
        vec!["echo".to_string()],
        "the preset's subset is what this session may call"
    );
    assert_eq!(
        engine.session_tools(&plain.session_id).expect("tools"),
        vec!["echo".to_string(), "note".to_string()],
        "a session composed from no preset sees the whole registry"
    );
}

/// TC-PORT-PRESET-4: what the caller wrote wins over what the preset says.
///
/// Upstream: its preset settings are defaults a session may override.
///
/// A caller that named both a preset and a model asked for that model on that
/// agent; a preset that overrode it would make the explicit argument a lie.
///
/// Input: a session created with `fast` and an explicit model and step budget.
/// Expected: the caller's model, the caller's budget, and still the preset's
/// tool subset.
#[tokio::test]
async fn what_the_caller_wrote_wins_over_what_the_preset_says() {
    let root = tempfile::tempdir().expect("temp dir");
    let engine = engine(roster(&document()).expect("roster"), &root);

    let session = engine
        .session_create(SessionCreateParams {
            model: Some("a-model-the-caller-named".to_string()),
            max_steps: Some(3),
            ..created(Some("fast"))
        })
        .await
        .expect("created");

    assert_eq!(session.model, "a-model-the-caller-named");
    assert_eq!(
        engine.session_tools(&session.session_id).expect("tools"),
        vec!["echo".to_string()]
    );
}

/// TC-PORT-PRESET-5: the preset a session was composed from is a fact of its
/// journal.
///
/// Upstream: "reads the creation-time value when nothing was switched",
/// "leaves a running session on the preset it was composed from".
///
/// A session whose agent changed under it half way through a conversation
/// would make its journal a record of two different agents, and nothing in it
/// would say where one ended.
///
/// Input: a session created from `fast`, then an engine built from a document
/// where `fast` names a different model, reopening the same journal.
/// Expected: the header still names `fast` and the model it was created with.
#[tokio::test]
async fn the_preset_a_session_was_composed_from_is_a_fact_of_its_journal() {
    let root = tempfile::tempdir().expect("temp dir");
    let first = engine(roster(&document()).expect("roster"), &root);
    let session = first
        .session_create(created(Some("fast")))
        .await
        .expect("created");
    let id = session.session_id.clone();
    assert_eq!(session.model, "fast-model");

    let mut changed = document();
    changed.set(
        "presets.fast.model.default",
        serde_json::json!("a-different-model"),
        Layer::File,
    );
    let second = engine(roster(&changed).expect("roster"), &root);
    let reopened = second
        .session_create(SessionCreateParams {
            session_id: Some(id.clone()),
            ..created(None)
        })
        .await
        .expect("reopened");

    assert_eq!(
        reopened.model, "fast-model",
        "the model a turn already ran under is a fact of the log"
    );
    let events = second
        .session_events(tetanus_protocol::methods::SessionEventsParams {
            session_id: id,
            from_seq: 0,
            limit: Some(1),
        })
        .await
        .expect("events");
    assert_eq!(
        events.events[0].data.get("preset"),
        Some(&serde_json::json!("fast")),
        "the journal says which agent it was: {:?}",
        events.events[0].data
    );
}

/// TC-PORT-PRESET-6: a preset nobody wrote is refused, and it names the ones
/// that exist.
///
/// Upstream: "reports the known ids when a preset is unknown", "reports an
/// unknown user default only when a session tries to use it".
///
/// A session asked for by name is a session somebody meant; running it on the
/// harness defaults would silently hand a model the tools the preset was
/// written to keep away from it.
///
/// Input: `session.create` naming a preset that is not in the roster, and a
/// roster resolving an unknown default.
/// Expected: `InvalidParams` carrying the field, the id and the known ids; and
/// [`PresetError::Unknown`] from the roster, naming what it has.
#[tokio::test]
async fn a_preset_nobody_wrote_is_refused_and_it_names_the_ones_that_exist() {
    let root = tempfile::tempdir().expect("temp dir");
    let engine = engine(roster(&document()).expect("roster"), &root);

    let refused = engine
        .session_create(created(Some("quickest")))
        .await
        .expect_err("no such preset");
    assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
    assert!(refused.message.contains("quickest"), "{refused:?}");
    assert!(
        refused.message.contains("fast") && refused.message.contains("thorough"),
        "the refusal offers what there is: {refused:?}"
    );
    assert_eq!(
        refused.data.as_ref().and_then(|data| data.get("field")),
        Some(&serde_json::json!("preset"))
    );

    let unknown = Roster::new()
        .defaulting_to(Some("nobody".to_string()))
        .resolve(None)
        .expect_err("the default names nothing");
    assert!(matches!(unknown, PresetError::Unknown { .. }));
}

/// TC-PORT-PRESET-7: the default preset composes a session that named none.
///
/// Upstream: "mounts the default preset when the caller names none", "composes
/// a new session from the user default", "falls back to the composition
/// default while the user set none".
///
/// Input: a roster whose default is `thorough`, and a `session.create` naming
/// no preset.
/// Expected: the session runs on `thorough-model` with that preset's step
/// budget, and its header names it.
#[tokio::test]
async fn the_default_preset_composes_a_session_that_named_none() {
    let root = tempfile::tempdir().expect("temp dir");
    let mut config = document();
    config.set(
        "presets.default",
        serde_json::json!("thorough"),
        Layer::File,
    );
    let engine = engine(roster(&config).expect("roster"), &root);

    let session = engine.session_create(created(None)).await.expect("created");
    assert_eq!(session.model, "thorough-model");
    assert_eq!(
        engine.session_tools(&session.session_id).expect("tools"),
        vec!["echo".to_string(), "note".to_string()],
        "a preset that names no subset narrows nothing"
    );
}

/// TC-PORT-PRESET-8: the persona and the prompt shape reach the turn.
///
/// Upstream: "makes a complete persona the exact prompt after every other
/// contribution", "shadows the deployment default for one scope only",
/// "scopes prompt sections and assembled schemas to the same session".
///
/// tetanus has no prompt scopes, so the restatement is per session: the
/// persona is a section on that session's own registry, and the preset's
/// prompt replaces the engine's opening section rather than the whole
/// assembly.
///
/// Input: a turn run on a session composed from `fast`, and one on a session
/// composed from nothing, each watched on its own bus.
/// Expected: the first session's assembly carries the preset's opening section
/// and its persona, in that order; the second carries neither.
#[tokio::test]
async fn the_persona_and_the_prompt_shape_reach_the_turn() {
    let root = tempfile::tempdir().expect("temp dir");
    let engine = engine(roster(&document()).expect("roster"), &root);

    let composed = engine
        .session_create(created(Some("fast")))
        .await
        .expect("created");
    let plain = engine.session_create(created(None)).await.expect("created");

    // One bus per session is what makes a persona per session observable: the
    // sections are read off the assembly of the turn that ran on it.
    /// One assembly's sections, as (id, text) in the order the engine read
    /// them.
    type Assembly = Vec<(String, String)>;
    /// One watched session: its id, every assembly its turns produced, and the
    /// listener that records them - which must outlive the turn.
    type Watched = (String, Arc<Mutex<Vec<Assembly>>>, EffectHandle);
    let mut watched: Vec<Watched> = Vec::new();
    for session in [&composed, &plain] {
        let live = engine
            .sessions()
            .open(&session.session_id)
            .expect("the session is open");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let handle = live.bus.on_waterfall::<AssemblePrompt, _>(move |ev, next| {
            sink.lock().expect("seen").push(
                ev.sections
                    .iter()
                    .map(|section| (section.id.clone(), section.text.clone()))
                    .collect::<Assembly>(),
            );
            Box::pin(next.run(ev))
        });
        watched.push((session.session_id.clone(), seen, handle));
    }

    for session in [&composed, &plain] {
        engine
            .agent_prompt(AgentPromptParams {
                session_id: session.session_id.clone(),
                content: "say something".to_string(),
            })
            .await
            .expect("the turn runs");
    }

    let sections_of = |id: &str| -> Assembly {
        watched
            .iter()
            .find(|(watched_id, _, _)| watched_id == id)
            .map(|(_, seen, _)| {
                seen.lock()
                    .expect("seen")
                    .first()
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    };

    let composed_sections = sections_of(&composed.session_id);
    assert_eq!(
        composed_sections
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<&str>>(),
        vec!["base", "persona"],
        "the persona sits after the opening section: {composed_sections:?}"
    );
    assert_eq!(composed_sections[0].1, "Answer in one line.");
    assert_eq!(
        composed_sections[1].1,
        "You are Quick, who never elaborates."
    );

    let plain_sections = sections_of(&plain.session_id);
    assert_eq!(
        plain_sections
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<&str>>(),
        vec!["base"],
        "one session's persona does not reach another: {plain_sections:?}"
    );
    assert!(
        !plain_sections[0].1.contains("one line"),
        "nor does its prompt shape: {plain_sections:?}"
    );
}

/// TC-PORT-PRESET-9: a preset naming a tool the harness does not have is
/// refused, not quietly narrowed.
///
/// Upstream: "refuses the mount up front with the discovery-reported reason".
///
/// A preset whose typo silently produced a smaller tool set would be an agent
/// missing a capability nobody took away.
///
/// Input: a preset naming `echo` and `ghost`.
/// Expected: the session is created - the journal is not the problem - and the
/// first thing that needs its agent refuses, naming the tool and the preset.
#[tokio::test]
async fn a_preset_naming_a_tool_the_harness_does_not_have_is_refused() {
    let root = tempfile::tempdir().expect("temp dir");
    let mut config = document();
    config.set(
        "presets.fast.agent.tools",
        serde_json::json!(["echo", "ghost"]),
        Layer::File,
    );
    let engine = engine(roster(&config).expect("roster"), &root);

    let session = engine
        .session_create(created(Some("fast")))
        .await
        .expect("created");
    let refused = engine
        .session_tools(&session.session_id)
        .expect_err("the subset cannot be built");
    assert!(refused.message.contains("ghost"), "{refused:?}");
    assert!(refused.message.contains("fast"), "{refused:?}");

    let refused = engine
        .agent_prompt(AgentPromptParams {
            session_id: session.session_id.clone(),
            content: "anything".to_string(),
        })
        .await
        .expect_err("the turn cannot be composed");
    assert!(refused.message.contains("ghost"), "{refused:?}");
}

/// TC-PORT-PRESET-10: a fork continues as the agent it forked from.
///
/// Upstream: "gives the child its parent's preset", "reports the preset id the
/// child joined, for the durable header", "composes nothing when the parent
/// joined no preset".
///
/// Input: a session composed from `fast`, prompted once, then forked.
/// Expected: the child's header names `fast` and its model, and its tool set
/// is the preset's.
#[tokio::test]
async fn a_fork_continues_as_the_agent_it_forked_from() {
    let root = tempfile::tempdir().expect("temp dir");
    let engine = engine(roster(&document()).expect("roster"), &root);
    let parent = engine
        .session_create(created(Some("fast")))
        .await
        .expect("created");
    engine
        .agent_prompt(AgentPromptParams {
            session_id: parent.session_id.clone(),
            content: "hello".to_string(),
        })
        .await
        .expect("the turn runs");

    let child = engine
        .session_fork(tetanus_protocol::methods::SessionForkParams {
            session_id: parent.session_id.clone(),
            through_seq: None,
            child_session_id: None,
        })
        .await
        .expect("forked");

    assert_eq!(child.model, "fast-model");
    assert_eq!(
        engine.session_tools(&child.session_id).expect("tools"),
        vec!["echo".to_string()]
    );
}
