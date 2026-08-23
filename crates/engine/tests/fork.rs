//! Conformance for `session.fork`, contract section 4.4.6.
//!
//! Ported from upstream `packages/core/session/tests/fork.spec.ts`. A port
//! restates the upstream case against the tetanus seam that carries the same
//! decision, so upstream's live-object identity checks (a detached `Session`,
//! a stale one) have nothing to restate here: the wire names a session by id,
//! and the store resolves it.
//!
//! Test design: every case runs offline against a temporary journal root, on
//! the deterministic mock adapter, so none needs a key or a network.

use tempfile::TempDir;
use tetanus_engine::session::SESSION_START;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, Engine, SessionCreateParams, SessionEventsParams, SessionForkParams,
};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_session::SessionEvent;

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

async fn session(engine: &HarnessEngine, id: &str) -> String {
    engine
        .session_create(SessionCreateParams {
            session_id: Some(id.to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("session.create")
        .session_id
}

/// Append a durable fact straight to a session's journal. A case that needs a
/// particular shape of log says so directly, rather than driving a turn that
/// happens to produce it.
fn append(engine: &HarnessEngine, id: &str, ty: &str, data: serde_json::Value) -> u64 {
    engine
        .sessions()
        .live(id)
        .expect("live")
        .log
        .append(ty, data)
        .expect("append")
        .seq
}

fn events(engine: &HarnessEngine, id: &str) -> Vec<SessionEvent> {
    engine.sessions().live(id).expect("live").log.events()
}

fn header(engine: &HarnessEngine, id: &str) -> serde_json::Value {
    let events = events(engine, id);
    assert_eq!(events[0].ty, SESSION_START, "the first line is the header");
    events[0].data.clone()
}

/// One closed turn carrying one user message.
fn closed_turn(engine: &HarnessEngine, id: &str, turn: u64, text: &str) {
    append(
        engine,
        id,
        "turn/start",
        serde_json::json!({ "turn": turn }),
    );
    append(
        engine,
        id,
        "user/message",
        serde_json::json!({ "content": text }),
    );
    append(
        engine,
        id,
        "turn/end",
        serde_json::json!({ "turn": turn, "steps": 1, "stop_reason": "natural" }),
    );
}

fn fork(
    engine: &HarnessEngine,
    source: &str,
    through_seq: Option<u64>,
    child: Option<&str>,
) -> SessionForkParams {
    let _ = engine;
    SessionForkParams {
        session_id: source.to_string(),
        through_seq,
        child_session_id: child.map(str::to_string),
    }
}

/// TC-PORT-FORK-1: a parent that holds nothing but its header forks into a
/// child that inherits nothing and still records where it came from.
///
/// Input: a session created and never used, forked with no boundary.
/// Expected: the child's journal is one line - its own header - and that
/// header names the parent and a `fork_seq` of 0. Zero is the parent's header
/// line, so "inherited nothing" and "inherited through seq 0" are the same
/// fact, and the child's own work still begins at `fork_seq + 1`.
#[tokio::test]
async fn an_empty_parent_forks_into_an_empty_child_that_knows_its_parent() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "empty-parent").await;

    let child = engine
        .session_fork(fork(&engine, &parent, None, Some("empty-child")))
        .await
        .expect("session.fork");

    assert_eq!(child.session_id, "empty-child");
    assert_eq!(child.last_seq, 0, "the header is the only line");
    assert_eq!(
        header(&engine, &child.session_id),
        serde_json::json!({
            "session_id": "empty-child",
            "provider": child.provider,
            "model": child.model,
            "max_steps": 8,
            "parent_session": "empty-parent",
            "fork_seq": 0,
            // Inherited from the parent, which recorded it at creation
            // (contract section 4.4.9). Read from the parent rather than
            // written out, because what this asserts is that the child carries
            // the parent's value, not that either is a particular path.
            "cwd": header(&engine, "empty-parent")["cwd"],
            // No `spawned_by` and no `depth`: section 4.4.9 says a fork
            // inherits the origin facts it is a copy of, and a root parent has
            // none to inherit. A fork is not itself delegation.
        })
    );
}

/// TC-PORT-FORK-2: with no boundary named, the child inherits the parent's
/// whole journal, and the two are separate journals from that moment.
///
/// Input: a parent with one closed turn, forked with no boundary, then both
/// journals appended to.
/// Expected: the child's lines are the parent's with line 0 replaced by the
/// child's own header, every copied event keeping the seq and the payload it
/// was written under. Upstream freezes the seed objects so a child cannot
/// mutate a parent's event; a copy on disk is detached by construction, so
/// what is restated here is the consequence upstream's freeze exists for -
/// appending to either journal afterwards leaves the other exactly as it was.
#[tokio::test]
async fn the_default_boundary_is_the_parents_last_event_and_the_copy_is_detached() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "parent").await;
    closed_turn(&engine, &parent, 1, "hello");
    let before = events(&engine, &parent);

    let child = engine
        .session_fork(fork(&engine, &parent, None, Some("child")))
        .await
        .expect("session.fork");

    let inherited = events(&engine, &child.session_id);
    assert_eq!(inherited.len(), before.len());
    assert_eq!(inherited[1..], before[1..], "every copied line stands");
    assert_eq!(
        header(&engine, &child.session_id)["fork_seq"],
        serde_json::json!(before.len() as u64 - 1)
    );

    closed_turn(&engine, &parent, 2, "parent goes on");
    append(
        &engine,
        &child.session_id,
        "user/message",
        serde_json::json!({ "content": "child goes elsewhere" }),
    );

    assert_eq!(
        events(&engine, &child.session_id)[..inherited.len()],
        inherited[..],
        "the parent's second turn did not reach the child"
    );
    assert_eq!(
        events(&engine, &parent)[..before.len()],
        before[..],
        "the child's own work did not reach the parent"
    );
}

/// TC-PORT-FORK-3: an event appended after a turn closed is inherited like any
/// other. The default boundary is the parent's last event, not its last
/// `turn/end`.
///
/// Input: a parent with a closed turn and one further event that is not a turn
/// boundary - upstream's stand-in for a plugin's own durable record.
/// Expected: the child's last inherited line is that event.
#[tokio::test]
async fn an_event_after_a_closed_turn_is_inherited() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "log-only-parent").await;
    closed_turn(&engine, &parent, 1, "hello");
    append(
        &engine,
        &parent,
        "test/log-only",
        serde_json::json!({ "value": "after execution" }),
    );

    let child = engine
        .session_fork(fork(&engine, &parent, None, Some("log-only-child")))
        .await
        .expect("session.fork");

    let inherited = events(&engine, &child.session_id);
    assert_eq!(inherited.last().expect("last").ty, "test/log-only");
    assert_eq!(
        inherited.last().expect("last").data,
        serde_json::json!({ "value": "after execution" })
    );
}

/// TC-PORT-FORK-4: an earlier boundary is honoured while the parent's tail is
/// open, and the history the child derives is the prefix it kept.
///
/// Input: a parent with two closed turns and a third still open, forked
/// through the first turn's `turn/end`.
/// Expected: the child holds that prefix, `fork_seq` is that seq, and the
/// messages derived from the child's journal are the first turn's alone. A
/// journal is append-only, so a prefix of one is stable however busy its
/// writer is.
#[tokio::test]
async fn an_earlier_boundary_is_honoured_while_the_parents_tail_is_open() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "busy-parent").await;
    closed_turn(&engine, &parent, 1, "first");
    let boundary = events(&engine, &parent).last().expect("last").seq;
    closed_turn(&engine, &parent, 2, "second");
    append(
        &engine,
        &parent,
        "turn/start",
        serde_json::json!({ "turn": 3 }),
    );

    let child = engine
        .session_fork(fork(
            &engine,
            &parent,
            Some(boundary),
            Some("child-of-first"),
        ))
        .await
        .expect("session.fork");

    let inherited = events(&engine, &child.session_id);
    assert_eq!(inherited.len() as u64, boundary + 1);
    assert_eq!(
        header(&engine, &child.session_id)["fork_seq"],
        serde_json::json!(boundary)
    );
    let history = tetanus_turn::log::derive_messages(&inherited);
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].content, "first");
}

/// TC-PORT-FORK-5: every reason a turn can end for is a closed boundary.
///
/// Input: one parent per `stop_reason` tetanus writes, each with a single turn
/// that ended for that reason, forked through its `turn/end`.
/// Expected: each fork is accepted and inherits the whole journal. Which
/// reason closed a turn is a fact about that turn; the boundary rule asks only
/// whether the turn is closed.
#[tokio::test]
async fn every_stop_reason_closes_a_turn_for_the_purpose_of_a_boundary() {
    let (engine, _dir) = engine();

    for (n, reason) in [
        "natural",
        "pre-step-rejected",
        "max-steps",
        "cancelled",
        "interrupted",
        "max-tokens",
        "failed",
    ]
    .into_iter()
    .enumerate()
    {
        let parent = session(&engine, &format!("parent-{n}")).await;
        append(
            &engine,
            &parent,
            "turn/start",
            serde_json::json!({ "turn": 1 }),
        );
        append(
            &engine,
            &parent,
            "turn/end",
            serde_json::json!({ "turn": 1, "steps": 1, "stop_reason": reason }),
        );
        let boundary = events(&engine, &parent).last().expect("last").seq;

        let child = engine
            .session_fork(fork(
                &engine,
                &parent,
                Some(boundary),
                Some(&format!("child-{n}")),
            ))
            .await
            .unwrap_or_else(|e| panic!("fork through a `{reason}` turn: {e:?}"));

        assert_eq!(child.last_seq, boundary as i64);
        assert_eq!(
            events(&engine, &child.session_id).last().expect("last").ty,
            "turn/end"
        );
    }
}

/// TC-PORT-FORK-6: the child's own work begins after the seed, and the parent
/// carries no record that it was forked.
///
/// Upstream writes a `session/end-seed` marker into the child so a reader can
/// tell inherited events from the child's own, and its case turns on that
/// marker being in the child and not in the parent. tetanus states the same
/// boundary as `fork_seq` on the header, so what is restated is the property
/// the marker exists to give: the first event the child writes is at
/// `fork_seq + 1`, and nothing about the parent changed.
#[tokio::test]
async fn the_childs_own_work_begins_after_the_seed_and_the_parent_is_untouched() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "bracket-parent").await;
    closed_turn(&engine, &parent, 1, "work");
    let before = events(&engine, &parent);

    let child = engine
        .session_fork(fork(&engine, &parent, None, Some("bracket-child")))
        .await
        .expect("session.fork");
    let fork_seq = header(&engine, &child.session_id)["fork_seq"]
        .as_u64()
        .expect("fork_seq");

    let first = append(
        &engine,
        &child.session_id,
        "turn/start",
        serde_json::json!({ "turn": 2 }),
    );

    assert_eq!(first, fork_seq + 1);
    assert_eq!(events(&engine, &parent), before, "the parent is unmarked");
    assert!(
        !before.iter().any(|e| e.ty.contains("seed")),
        "a fork writes nothing to its source"
    );
}

/// TC-PORT-FORK-7: a boundary the parent never reached is refused, and no
/// child journal is left behind by the refusal.
///
/// Upstream also refuses a negative, a fractional and an unsafe-integer
/// boundary. Those are unrepresentable rather than unported: `through_seq` is
/// a `u64`, so a JSON value that is not a non-negative integer is refused by
/// the codec as `InvalidParams` before the engine is reached.
#[tokio::test]
async fn a_boundary_past_the_tail_is_refused_and_creates_nothing() {
    let (engine, dir) = engine();
    let parent = session(&engine, "short-parent").await;
    closed_turn(&engine, &parent, 1, "hello");
    let last = events(&engine, &parent).last().expect("last").seq;

    let error = engine
        .session_fork(fork(
            &engine,
            &parent,
            Some(last + 1),
            Some("no-such-child"),
        ))
        .await
        .expect_err("a boundary past the tail");

    assert_eq!(error.kind(), Some(ErrorCode::InvalidParams));
    assert_eq!(
        error.data.expect("data")["field"],
        serde_json::json!("through_seq")
    );
    assert!(
        !dir.path().join("no-such-child.jsonl").exists(),
        "a refused fork creates no journal"
    );
    assert!(engine.sessions().live("no-such-child").is_none());
}

/// TC-PORT-FORK-8: a source journal whose seqs are not contiguous has no fork
/// boundary to argue about.
///
/// Upstream reaches this by writing a wrong `seq` into a live session's array
/// and asserts a typed `INVALID_BOUNDARY`. A tetanus source is either the
/// in-memory log its writer built, where each seq is the log's length at
/// append time, or a journal read back by `replay`, which refuses a gap. So
/// the answer is the one any read of that journal gives - `LogCorrupt` naming
/// the line - and it arrives before the boundary is considered.
#[tokio::test]
async fn a_source_with_a_seq_gap_is_a_corrupt_journal_not_a_bad_boundary() {
    let (engine, dir) = engine();
    let path = dir.path().join("gappy.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"session/start","seq":0,"time":1,"data":{"session_id":"gappy","provider":"mock","model":"mock-model","max_steps":8}}"#,
            "\n",
            r#"{"type":"turn/start","seq":7,"time":2,"data":{"turn":1}}"#,
            "\n",
        ),
    )
    .expect("write");

    let error = engine
        .session_fork(fork(&engine, "gappy", Some(0), Some("gappy-child")))
        .await
        .expect_err("a journal with a gap");

    assert_eq!(error.kind(), Some(ErrorCode::LogCorrupt));
    assert_eq!(
        error.data.expect("data")["session_id"],
        serde_json::json!("gappy")
    );
}

/// TC-PORT-FORK-9: a source this server cannot open is `SessionNotFound`.
#[tokio::test]
async fn an_unknown_source_is_not_found() {
    let (engine, _dir) = engine();

    let error = engine
        .session_fork(fork(&engine, "missing", None, None))
        .await
        .expect_err("no such session");

    assert_eq!(error.kind(), Some(ErrorCode::SessionNotFound));
    assert_eq!(
        error.data.expect("data")["session_id"],
        serde_json::json!("missing")
    );
}

/// TC-PORT-FORK-10: a boundary inside an open turn is refused, wherever inside
/// it falls.
///
/// Input: five parents, each stopped at one of the places a turn can be
/// mid-flight - just opened, a step opened, the user's message written, the
/// model's answer written, a tool call dispatched - forked at that last line.
/// Expected: each is `InvalidParams` naming `through_seq`, and the message
/// names the turn that is still open. The rule reads the log alone: the last
/// `turn/start` or `turn/end` at or before the boundary decides.
#[tokio::test]
async fn a_boundary_inside_an_open_turn_is_refused_wherever_it_falls() {
    let (engine, _dir) = engine();

    let cases: Vec<(&str, Vec<(&str, serde_json::Value)>)> = vec![
        (
            "turn-start",
            vec![("turn/start", serde_json::json!({ "turn": 1 }))],
        ),
        (
            "step-start",
            vec![
                ("turn/start", serde_json::json!({ "turn": 1 })),
                ("step/start", serde_json::json!({ "turn": 1, "step": 1 })),
            ],
        ),
        (
            "user-message",
            vec![
                ("turn/start", serde_json::json!({ "turn": 1 })),
                ("user/message", serde_json::json!({ "content": "open" })),
            ],
        ),
        (
            "assistant-message",
            vec![
                ("turn/start", serde_json::json!({ "turn": 1 })),
                ("step/start", serde_json::json!({ "turn": 1, "step": 1 })),
                (
                    "assistant/message",
                    serde_json::json!({ "content": "partial", "tool_calls": [] }),
                ),
            ],
        ),
        (
            "tool-call",
            vec![
                ("turn/start", serde_json::json!({ "turn": 1 })),
                ("step/start", serde_json::json!({ "turn": 1, "step": 1 })),
                (
                    "tool/call",
                    serde_json::json!({ "id": "c1", "name": "echo", "arguments": {} }),
                ),
            ],
        ),
    ];

    for (name, writes) in cases {
        let parent = session(&engine, &format!("open-{name}")).await;
        let mut boundary = 0;
        for (ty, data) in writes {
            boundary = append(&engine, &parent, ty, data);
        }

        let error = engine
            .session_fork(fork(&engine, &parent, Some(boundary), None))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), Some(ErrorCode::InvalidParams), "{name}");
        assert_eq!(
            error.data.expect("data")["field"],
            serde_json::json!("through_seq"),
            "{name}"
        );
        assert!(
            error.message.contains("open turn 1"),
            "{name}: {}",
            error.message
        );
    }
}

/// TC-PORT-FORK-11: a child id that already has a journal is refused rather
/// than reopened, which is the one place this call differs from
/// `session.create`. A seed written onto a journal that already holds a
/// history would splice two histories into one file.
#[tokio::test]
async fn a_child_id_that_already_exists_is_refused_not_reopened() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "parent").await;
    closed_turn(&engine, &parent, 1, "hello");
    let taken = session(&engine, "taken").await;
    let before = events(&engine, &taken);

    let error = engine
        .session_fork(fork(&engine, &parent, None, Some("taken")))
        .await
        .expect_err("the child id is taken");

    assert_eq!(error.kind(), Some(ErrorCode::InvalidParams));
    assert_eq!(
        error.data.expect("data")["field"],
        serde_json::json!("child_session_id")
    );
    assert_eq!(
        events(&engine, &taken),
        before,
        "nothing was appended to it"
    );
}

/// TC-PORT-FORK-12: the child id is judged before the boundary is.
///
/// Input: a source whose only turn is open - so the default boundary is inside
/// it - and a child id that is already taken. Two things are wrong, and only
/// one answer can be given.
/// Expected: the taken id. A caller that reused an id learns that whatever
/// else is also true of the request, which is the order upstream settles them
/// in as well.
#[tokio::test]
async fn a_taken_child_id_is_answered_before_the_boundary_is_judged() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "open-parent").await;
    append(
        &engine,
        &parent,
        "turn/start",
        serde_json::json!({ "turn": 1 }),
    );
    session(&engine, "taken").await;

    let error = engine
        .session_fork(fork(&engine, &parent, None, Some("taken")))
        .await
        .expect_err("both are wrong");

    assert_eq!(
        error.data.expect("data")["field"],
        serde_json::json!("child_session_id")
    );
}

/// TC-FORK-1: a child continues the conversation it inherited.
///
/// This is the point of the call, and it is not one of upstream's cases: there
/// a fork produces a `Session` object, here it produces a journal the same
/// engine runs turns on. A turn on the child is numbered after the turns it
/// inherited and is answered in the light of them, which is the same rule a
/// resumed session follows - a fork is a resume of a prefix.
#[tokio::test]
async fn a_forked_session_continues_the_turns_it_inherited() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "conversation").await;
    engine
        .agent_prompt(AgentPromptParams {
            session_id: parent.clone(),
            content: "first question".into(),
        })
        .await
        .expect("agent.prompt");

    let child = engine
        .session_fork(fork(&engine, &parent, None, Some("second-try")))
        .await
        .expect("session.fork");
    let inherited = events(&engine, &child.session_id).len();

    let summary = engine
        .agent_prompt(AgentPromptParams {
            session_id: child.session_id.clone(),
            content: "second question".into(),
        })
        .await
        .expect("agent.prompt")
        .summary;

    assert_eq!(summary.turn, 2, "the inherited turn is counted");
    let history = tetanus_turn::log::derive_messages(&events(&engine, &child.session_id));
    let asked: Vec<&str> = history
        .iter()
        .filter(|m| m.role == tetanus_turn::llm::Role::User)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(asked, vec!["first question", "second question"]);
    assert!(
        events(&engine, &child.session_id).len() > inherited,
        "the child wrote its own turn"
    );
    assert_eq!(
        events(&engine, &parent).len(),
        inherited,
        "and the parent did not grow"
    );
}

/// TC-FORK-2: a forked session is an ordinary session from the outside.
///
/// It is listed, it pages, and its title is the first user message of the
/// history it inherited, because a title is read off the journal and the
/// child's journal starts with the parent's conversation.
#[tokio::test]
async fn a_forked_session_is_listed_paged_and_titled_like_any_other() {
    let (engine, _dir) = engine();
    let parent = session(&engine, "titled").await;
    closed_turn(&engine, &parent, 1, "what a title looks like");

    let child = engine
        .session_fork(fork(&engine, &parent, None, Some("titled-child")))
        .await
        .expect("session.fork");

    assert_eq!(child.title.as_deref(), Some("what a title looks like"));

    let listed = engine.session_list().await.expect("session.list").sessions;
    let ids: Vec<&str> = listed.iter().map(|s| s.session_id.as_str()).collect();
    assert!(ids.contains(&"titled-child"), "{ids:?}");

    let page = engine
        .session_events(SessionEventsParams {
            session_id: "titled-child".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("session.events");
    assert!(page.eof);
    assert_eq!(page.events[0].ty, SESSION_START);
    assert_eq!(page.events[0].data["parent_session"], "titled");
}
