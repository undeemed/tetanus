//! Test Design Specification: context compaction, ported.
//!
//! Features under test: `tetanus_turn::compaction` - the surface a compaction
//! rewrites, the boundaries it may cut at, the durable transaction it records,
//! and the derivation a replay reproduces from that record. Upstream pins the
//! same behaviour in `packages/compaction/compaction/tests/{compaction,
//! tool-pairing}.spec.ts` and `compaction-basic/tests/compaction-basic.spec.ts`;
//! the content transform half is already ported as `upstream_prune.rs`.
//!
//! Approach: journals built event by event, so a case states the surface it is
//! about rather than arranging for a turn to produce one, and one case that
//! drives a real turn end to end. The summarizer is deterministic
//! (`OutlineSummarizer`), which is what lets the whole transaction be asserted
//! offline: a model-written checkpoint would make every assertion about the
//! replacement a probabilistic one.
//!
//! What is not restated, and why. Upstream's manual `/compact` command, its
//! per-model policy table and its `sourceCommandId` provenance are surfaces
//! tetanus has not built. Its surface-changed retries guard an asynchronous
//! summarizer racing a concurrently appending session; a tetanus session has
//! one writer and a turn compacts inside its own step, so there is no second
//! writer to race. Its `session/end-seed` boundary, which lets a fork's
//! inherited `compaction/start` be ignored, has no counterpart: `fork_seq` on
//! the header states the same boundary.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::Arc;

use harness::Harness;
use tetanus_core::EventBus;
use tetanus_session::{replay, JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::compaction::{
    self, compact, prune_results, select_range, surface, tool_pairing_balanced_before,
    CompactionBudget, OutlineSummarizer, Summarizer,
};
use tetanus_turn::engine::AutoCompaction;
use tetanus_turn::llm::Role;
use tetanus_turn::log::derive_messages;
use tetanus_turn::prune::PruneBudget;
use tetanus_turn::TurnConfig;

/// A journal on disk, so every case can replay what it wrote.
fn journal(name: &str) -> (Arc<JsonlSessionLog>, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(format!("{name}.jsonl"));
    let log = JsonlSessionLog::create(name, &path, EventBus::new()).expect("journal");
    (log, path, dir)
}

/// One exchange: a user message, an assistant answer, and nothing owed.
fn exchange(log: &dyn SessionLog, user: &str, assistant: &str) {
    log.append_with_sources(
        "user/message",
        serde_json::json!({ "content": user }),
        vec![],
    )
    .unwrap();
    log.append_with_sources(
        "assistant/message",
        serde_json::json!({ "content": assistant, "tool_calls": [] }),
        vec![],
    )
    .unwrap();
}

/// One exchange that calls a tool and reads its result.
fn tool_exchange(log: &dyn SessionLog, id: &str, output: &str) {
    log.append_with_sources(
        "assistant/message",
        serde_json::json!({
            "content": "",
            "tool_calls": [{ "id": id, "name": "read", "arguments": {} }],
        }),
        vec![],
    )
    .unwrap();
    log.append("tool/call", serde_json::json!({ "id": id, "name": "read" }))
        .unwrap();
    log.append_with_sources(
        "tool/result",
        serde_json::json!({ "call_id": id, "content": output }),
        vec![],
    )
    .unwrap();
}

fn types(events: &[SessionEvent]) -> Vec<&str> {
    events.iter().map(|e| e.ty.as_str()).collect()
}

/// TC-PORT-COMPACT-1: with no compaction on the log, the surface is every
/// surface event in log order.
///
/// The claim that makes one derivation serve both cases: an uncompacted
/// journal must derive exactly as it did before compaction existed.
///
/// Expected: the surface names the three surface events and nothing else, and
/// the derived history is the same three messages.
#[test]
fn an_uncompacted_log_derives_exactly_as_it_did() {
    let (log, _path, _dir) = journal("plain");
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();
    exchange(log.as_ref(), "first", "answer");
    log.append("assistant/chunk", serde_json::json!({ "delta": "an" }))
        .unwrap();
    log.append_with_sources(
        "user/message",
        serde_json::json!({ "content": "second" }),
        vec![],
    )
    .unwrap();

    let events = log.events();
    assert_eq!(surface(&events), vec![1, 2, 4]);
    let history = derive_messages(&events);
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].content, "first");
    assert_eq!(history[2].content, "second");
}

/// TC-PORT-COMPACT-2: a replacement takes the position of the range it
/// shadows, not the end of the conversation.
///
/// Upstream: `compaction.spec.ts`, the `replace` surface op. It is the rule
/// that makes a checkpoint readable at all: a summary of the first twenty
/// messages belongs where those twenty were, in front of the tail that was
/// kept verbatim, not after it.
///
/// Expected: the summary is the first message of the derived history and the
/// untouched tail follows it in order.
#[test]
fn a_replacement_stands_where_the_range_it_replaced_stood() {
    let (log, _path, _dir) = journal("position");
    exchange(log.as_ref(), "old one", "old answer");
    exchange(log.as_ref(), "recent", "recent answer");

    // The record names the two oldest surface events; the very next surface
    // event is the replacement.
    log.append(
        compaction::topic::COMPACTION_SUMMARY,
        serde_json::json!({
            "shadowed_seqs": [0, 1],
            "shadowed_token_count": 42,
        }),
    )
    .unwrap();
    log.append_with_sources(
        "user/message",
        serde_json::json!({ "content": "SUMMARY" }),
        vec![0, 1],
    )
    .unwrap();

    let history = derive_messages(&log.events());
    let said: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(said, vec!["SUMMARY", "recent", "recent answer"]);
}

/// TC-PORT-COMPACT-3: a record whose next event is not a replacement shadows
/// nothing.
///
/// The adjacency is the protocol. A record followed by something else
/// described a replacement that never landed, and honouring it later would
/// shadow a range that the replacement it finally met never named.
///
/// Expected: the surface is untouched and every message survives.
#[test]
fn a_record_with_no_adjacent_replacement_shadows_nothing() {
    let (log, _path, _dir) = journal("orphan");
    exchange(log.as_ref(), "one", "two");
    log.append(
        compaction::topic::COMPACTION_SUMMARY,
        serde_json::json!({ "shadowed_seqs": [0, 1], "shadowed_token_count": 9 }),
    )
    .unwrap();
    // Anything at all between the record and a surface event expires it.
    log.append("turn/end", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.append_with_sources(
        "user/message",
        serde_json::json!({ "content": "later" }),
        vec![],
    )
    .unwrap();

    let said: Vec<String> = derive_messages(&log.events())
        .iter()
        .map(|m| m.content.clone())
        .collect();
    assert_eq!(said, vec!["one", "two", "later"]);
}

/// TC-PORT-COMPACT-4: a cut may not separate a tool call from its result.
///
/// Upstream: `tool-pairing.spec.ts`. A request whose assistant message asks
/// for a tool that nothing answers is one a provider refuses, so this decides
/// every boundary a compaction may use.
///
/// Expected: the cut before the tool result is unbalanced, the cuts before the
/// assistant message that asked and after the result are balanced.
#[test]
fn a_cut_between_a_call_and_its_result_is_refused() {
    let (log, _path, _dir) = journal("pairing");
    exchange(log.as_ref(), "do it", "starting");
    tool_exchange(log.as_ref(), "c1", "output");

    let events = log.events();
    let nodes = surface(&events);
    // nodes: user, assistant, assistant(call), tool/result
    assert_eq!(nodes.len(), 4);
    assert!(tool_pairing_balanced_before(&events, &nodes, 2));
    assert!(
        !tool_pairing_balanced_before(&events, &nodes, 3),
        "the cut before the result would orphan the call"
    );
    assert!(tool_pairing_balanced_before(&events, &nodes, 4));
}

/// TC-PORT-COMPACT-5: the selected range stops at a balanced boundary.
///
/// Upstream: `compaction-basic.spec.ts`, "never splits an assistant
/// tool-call/result pair". The tail is walked back until the retain budget is
/// covered, and the cut is then moved *earlier* until it is balanced - earlier
/// rather than later, because moving it later would compact a message the
/// retention budget promised to keep.
///
/// Expected: the range ends before the assistant message that opened the call,
/// so the call and its result stay together on the retained side.
#[test]
fn the_selected_range_ends_on_a_balanced_boundary() {
    let (log, _path, _dir) = journal("select");
    exchange(log.as_ref(), "one", "two");
    tool_exchange(log.as_ref(), "c1", "output");

    let events = log.events();
    let nodes = surface(&events);
    // A budget that would otherwise cut between the call and its result.
    let (from, to) = select_range(&events, &nodes, 30).expect("something to compact");
    assert_eq!(from, 0);
    assert!(
        to < 2,
        "the range must end before the assistant message that opened the call, got {to}"
    );
}

/// TC-PORT-COMPACT-6: nothing is compacted when the whole surface is the tail
/// worth keeping.
///
/// Expected: `select_range` answers `None`, and `compact` reports
/// `NothingToCompact` rather than committing an empty transaction.
#[tokio::test]
async fn a_short_conversation_is_left_alone() {
    let (log, _path, _dir) = journal("short");
    exchange(log.as_ref(), "hello", "hi");

    let events = log.events();
    let nodes = surface(&events);
    assert!(select_range(&events, &nodes, 100_000).is_none());

    let refused = compact(
        log.as_ref(),
        &OutlineSummarizer,
        "system",
        CompactionBudget {
            threshold_tokens: 1000,
            retain_tokens: 100_000,
        },
    )
    .await
    .expect_err("nothing to compact");
    assert!(matches!(
        refused,
        compaction::CompactionError::NothingToCompact
    ));
    assert_eq!(log.events().len(), 2, "no records were written");
}

/// TC-PORT-COMPACT-7: the whole transaction lands on the journal, in order,
/// with the record adjacent to its replacement.
///
/// Upstream: `compaction.spec.ts`, "appends start, summary, replacement and
/// end". The adjacency is what lets a bounded consumer price a replacement
/// without keeping a price per message.
///
/// Expected: `compaction/start`, `compaction/summary`, the replacement
/// `user/message`, `compaction/end` - contiguously, in that order - and the
/// replacement cites the start, the record and every shadowed event.
#[tokio::test]
async fn the_transaction_lands_in_order_and_adjacent() {
    let (log, _path, _dir) = journal("transaction");
    for n in 0..8 {
        exchange(
            log.as_ref(),
            &format!("question {n} {}", "x".repeat(200)),
            &format!("answer {n} {}", "y".repeat(200)),
        );
    }
    let before = log.events().len();

    let done = compact(
        log.as_ref(),
        &OutlineSummarizer,
        "system",
        CompactionBudget {
            threshold_tokens: 400,
            retain_tokens: 120,
        },
    )
    .await
    .expect("compacted");

    let events = log.events();
    assert_eq!(
        types(&events[before..]),
        vec![
            "compaction/start",
            "compaction/summary",
            "user/message",
            "compaction/end",
        ]
    );
    assert_eq!(done.summary_seq + 1, done.replacement_seq, "adjacent");
    let cites = events[done.replacement_seq as usize]
        .source_event_seqs
        .clone()
        .expect("the replacement cites what it replaced");
    assert_eq!(cites[0], done.start_seq);
    assert_eq!(cites[1], done.summary_seq);
    assert_eq!(cites[2..].to_vec(), done.shadowed_seqs);
}

/// TC-PORT-COMPACT-8: the compacted history is smaller, keeps the recent tail
/// verbatim, and replays identically.
///
/// This is the acceptance claim: the request the provider sees is inside the
/// budget, the compaction is on the journal, and a replay of that journal
/// derives the same history.
///
/// Expected: fewer messages and fewer tokens than before; the last exchange is
/// still there word for word; and the history derived from the replayed
/// journal equals the history derived from the live log.
#[tokio::test]
async fn a_compacted_session_is_smaller_and_replays_the_same() {
    let (log, path, _dir) = journal("replay");
    for n in 0..8 {
        exchange(
            log.as_ref(),
            &format!("question {n} {}", "x".repeat(200)),
            &format!("answer {n} {}", "y".repeat(200)),
        );
    }
    let before = derive_messages(&log.events());
    let before_tokens = tetanus_turn::tokens::TokenSurface::of(&log.events()).total_tokens();

    compact(
        log.as_ref(),
        &OutlineSummarizer,
        "system",
        CompactionBudget {
            threshold_tokens: 400,
            retain_tokens: 120,
        },
    )
    .await
    .expect("compacted");
    log.flush().unwrap();

    let after = derive_messages(&log.events());
    let after_tokens = tetanus_turn::tokens::TokenSurface::of(&log.events()).total_tokens();
    assert!(
        after.len() < before.len(),
        "{} messages became {}",
        before.len(),
        after.len()
    );
    assert!(
        after_tokens < before_tokens,
        "{before_tokens} tokens became {after_tokens}"
    );
    assert_eq!(
        after.last().expect("a tail").content,
        before.last().expect("a tail").content,
        "the most recent message is kept verbatim"
    );
    assert_eq!(after[0].role, Role::User, "the checkpoint stands first");
    assert!(after[0]
        .content
        .contains(tetanus_turn::compaction::SUMMARY_OPEN));

    let replayed = replay(&path).expect("replay");
    assert_eq!(
        derive_messages(&replayed),
        after,
        "a replay of the journal derives the compacted history exactly"
    );
}

/// TC-PORT-COMPACT-9: a summary that is not smaller is refused, and the
/// journal says why.
///
/// Upstream: `compaction-basic.spec.ts`, "rejects a summary that is not
/// smaller than the shadowed content". Committing one would leave the session
/// over budget with a replacement that gets compacted again on the next step,
/// for ever.
///
/// Expected: `NotSmaller`; the bracket is closed with the reason recorded; and
/// the derived history is exactly what it was before the attempt.
#[tokio::test]
async fn a_summary_that_is_not_smaller_is_refused_and_recorded() {
    struct Verbose;
    #[async_trait::async_trait]
    impl Summarizer for Verbose {
        async fn summarize(
            &self,
            _input: compaction::SummarizationInput,
        ) -> Result<compaction::Summary, compaction::CompactionError> {
            Ok(compaction::Summary {
                text: "z".repeat(50_000),
                provider: "test".into(),
                model: "test".into(),
            })
        }
    }

    let (log, _path, _dir) = journal("bloated");
    for n in 0..8 {
        exchange(log.as_ref(), &format!("q{n}"), &format!("a{n}"));
    }
    let before = derive_messages(&log.events());

    let refused = compact(
        log.as_ref(),
        &Verbose,
        "system",
        CompactionBudget {
            threshold_tokens: 100,
            retain_tokens: 10,
        },
    )
    .await
    .expect_err("not smaller");
    assert!(matches!(
        refused,
        compaction::CompactionError::NotSmaller { .. }
    ));

    let events = log.events();
    assert_eq!(types(&events[before.len()..]).len(), 2);
    let closer = events.last().expect("a closer");
    assert_eq!(closer.ty, "compaction/end");
    assert!(
        closer.data.get("error").is_some(),
        "the close records why: {}",
        closer.data
    );
    assert_eq!(
        derive_messages(&log.events()),
        before,
        "a refused compaction changes no history"
    );
}

/// TC-PORT-COMPACT-10: a compaction left open holds the lock.
///
/// Upstream: `compaction.spec.ts`, "rejects a second compaction while one is
/// in progress". The summarizer is a provider call, so a second compaction
/// entering during it would shadow a range the first is still holding.
///
/// Expected: `AlreadyOpen` naming the seq of the start that was never closed.
#[tokio::test]
async fn a_compaction_left_open_refuses_the_next_one() {
    let (log, _path, _dir) = journal("locked");
    for n in 0..8 {
        exchange(log.as_ref(), &format!("q{n} {}", "x".repeat(200)), "a");
    }
    let start = log
        .append(compaction::topic::COMPACTION_START, serde_json::json!({}))
        .unwrap();

    let refused = compact(
        log.as_ref(),
        &OutlineSummarizer,
        "system",
        CompactionBudget {
            threshold_tokens: 400,
            retain_tokens: 120,
        },
    )
    .await
    .expect_err("the lock is held");
    assert!(
        matches!(refused, compaction::CompactionError::AlreadyOpen(seq) if seq == start.seq),
        "got {refused:?}"
    );
}

/// TC-PORT-COMPACT-11: the pruner's session transaction shrinks a tool result
/// on the log and derives the shortened one.
///
/// Upstream: `tool-result-pruner.spec.ts`, its `pruneSession` half - the part
/// `docs/parity.md` recorded as needing a durable event type that had not been
/// published. It is model-free, so it is the remedy worth trying first.
///
/// Expected: one `compaction/prune` immediately followed by the shortened
/// result; the derived history carries the shortened text and not the long
/// one; the replacement still answers the same call.
#[test]
fn pruning_a_result_replaces_it_on_the_surface() {
    let (log, path, _dir) = journal("pruned");
    exchange(log.as_ref(), "read it", "reading");
    tool_exchange(log.as_ref(), "c1", &"L".repeat(5_000));
    let before = log.events().len();

    let done = prune_results(
        log.as_ref(),
        PruneBudget {
            threshold: 200,
            head: 50,
            tail: 20,
        },
    )
    .expect("pruned");
    log.flush().unwrap();

    assert_eq!(done.replacements.len(), 1);
    assert!(done.chars_removed > 4_000);
    let events = log.events();
    assert_eq!(
        types(&events[before..]),
        vec!["compaction/prune", "tool/result"]
    );

    let history = derive_messages(&events);
    let result = history.last().expect("the result");
    assert_eq!(result.role, Role::Tool);
    assert_eq!(result.tool_call_id.as_deref(), Some("c1"));
    assert!(
        result.content.len() < 500,
        "the long result is off the surface: {} chars",
        result.content.len()
    );
    assert!(result.content.contains(tetanus_turn::prune::MARKER));
    assert_eq!(
        derive_messages(&replay(&path).unwrap()),
        history,
        "the pruned history replays"
    );
}

/// TC-PORT-COMPACT-12: a result already within budget is left exactly alone.
///
/// Expected: no records at all, so a pruning pass over a healthy session costs
/// the journal nothing.
#[test]
fn pruning_leaves_a_short_result_alone() {
    let (log, _path, _dir) = journal("short-result");
    tool_exchange(log.as_ref(), "c1", "small");
    let before = log.events();

    let done = prune_results(log.as_ref(), PruneBudget::default()).expect("pruned");

    assert!(done.replacements.is_empty());
    assert_eq!(log.events(), before);
}

/// TC-PORT-COMPACT-13: a turn over its budget compacts itself and continues.
///
/// The end-to-end claim, over a real turn: a conversation that has outgrown
/// its window keeps going, the request the provider is handed is inside the
/// budget, and the compaction is on the journal that turn wrote.
///
/// Expected: the turn succeeds; the journal carries the transaction; and the
/// history the last request derived is smaller than the surface that went into
/// the turn.
#[tokio::test]
async fn a_turn_over_its_budget_compacts_and_carries_on() {
    let mut config = TurnConfig {
        context_window: Some(4_000),
        compaction: Some(AutoCompaction {
            budget: CompactionBudget {
                threshold_tokens: 300,
                retain_tokens: 80,
            },
            prune: Some(PruneBudget {
                threshold: 400,
                head: 100,
                tail: 50,
            }),
        }),
        ..TurnConfig::default()
    };
    config.max_steps = 4;
    let h = Harness::with_config(
        "auto-compact",
        tetanus_turn::tools::ToolRegistry::new().with(Arc::new(tetanus_turn::tools::EchoTool)),
        config,
    )
    .await;

    // A history big enough that the first step is already over the threshold.
    for n in 0..10 {
        exchange(
            h.engine.log().as_ref(),
            &format!("earlier {n} {}", "x".repeat(300)),
            &format!("earlier answer {n} {}", "y".repeat(300)),
        );
    }
    let before = tetanus_turn::tokens::TokenSurface::of(&h.engine.log().events()).total_tokens();

    h.engine
        .run_turn("carry on from where we left off")
        .await
        .expect("the turn continues");

    let events = h.engine.log().events();
    assert!(
        events
            .iter()
            .any(|e| e.ty == compaction::topic::COMPACTION_SUMMARY),
        "the compaction is on the journal"
    );
    assert!(
        events
            .iter()
            .any(|e| e.ty == compaction::topic::COMPACTION_END),
        "and it was closed"
    );

    let after = tetanus_turn::tokens::TokenSurface::of(&events).total_tokens();
    assert!(
        after < before,
        "the surface shrank: {before} tokens became {after}"
    );
    assert_eq!(
        derive_messages(&replay(&h.log_path).unwrap()),
        derive_messages(&events),
        "the turn's journal replays to the compacted history"
    );
}

/// TC-PORT-COMPACT-14: a turn inside its budget compacts nothing.
///
/// The negative half, asserted as hard as the positive: a policy that fired
/// when it was not needed would rewrite a user's history for no reason.
///
/// Expected: the journal holds no compaction record at all.
#[tokio::test]
async fn a_turn_inside_its_budget_compacts_nothing() {
    let h = Harness::with_config(
        "no-compact",
        tetanus_turn::tools::ToolRegistry::new().with(Arc::new(tetanus_turn::tools::EchoTool)),
        TurnConfig {
            context_window: Some(200_000),
            compaction: Some(AutoCompaction {
                budget: CompactionBudget::for_window(200_000).unwrap(),
                prune: Some(PruneBudget::default()),
            }),
            ..TurnConfig::default()
        },
    )
    .await;

    h.engine.run_turn("a short question").await.expect("turn");

    let events = h.engine.log().events();
    assert!(
        !events.iter().any(|e| e.ty.starts_with("compaction/")),
        "nothing was compacted: {:?}",
        types(&events)
    );
}

/// TC-PORT-COMPACT-15: a budget whose retained tail reaches the threshold is
/// refused where it is set.
///
/// Upstream: `compaction-basic/config.ts`, `validateRatioRetention`. A tail
/// bigger than the whole budget can never be compacted down to it, so every
/// step would try, fail and try again - the same non-convergence
/// `PruneBudget::validate` refuses for the same reason.
///
/// Expected: `RetainExceedsThreshold`, and a zero window is refused too.
#[test]
fn a_budget_that_cannot_converge_is_refused() {
    let refused = CompactionBudget::scaled(1000, 0.5, 0.5).expect_err("cannot converge");
    assert!(matches!(
        refused,
        compaction::CompactionError::RetainExceedsThreshold { .. }
    ));
    assert!(matches!(
        CompactionBudget::for_window(0).expect_err("no window"),
        compaction::CompactionError::EmptyWindow
    ));
    let ok = CompactionBudget::for_window(100_000).expect("scales");
    assert_eq!(ok.threshold_tokens, 80_000);
    assert_eq!(ok.retain_tokens, 16_000);
}
