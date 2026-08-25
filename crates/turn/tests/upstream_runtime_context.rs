//! Test Design Specification: what a turn tells the model about the world
//! outside the conversation.
//!
//! Feature under test: [`tetanus_turn::context`], the `context/snapshot`
//! record the engine writes once per turn, and the rule by which that record
//! becomes a message. Contract section 4.4.8 fixes all three; the section has
//! been published, staged in `crates/protocol` and asserted at the boundary
//! (TC-PROTO-25) since before anything wrote one.
//!
//! Upstream builds each context as its own `agent/pre-step` plugin appending
//! its own user message (`packages/context/time-context`,
//! `packages/context/tmux-context`). tetanus gathers them into one record, for
//! the reason 4.4.8 gives: only the newest travels, and "newest" needs a
//! single record to be decidable.
//!
//! Approach: the clock is a parameter, so a case asserts the exact sentence a
//! model was shown rather than racing the system clock. The engine cases run
//! the offline harness end to end and read the journal back off the file,
//! because what a turn wrote and what a replay derives are the two halves of
//! this feature and only the file joins them.
//!
//! Features NOT tested here: the joining rule for prompt sections, which is
//! section 4.3's and has its own suite.
//!
//! Environmental needs: none. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use harness::Harness;
use tetanus_session::SessionEvent;
use tetanus_turn::context::{
    render, time_provider, ContextAt, ContextPart, ContextRegistry, TIME_ORDER, TIME_PART,
};
use tetanus_turn::llm::Role;
use tetanus_turn::log::{derive_messages, topic};

/// A clock stopped at one instant, so a case can assert the sentence.
fn frozen(secs: u64) -> tetanus_turn::context::Clock {
    Arc::new(move || UNIX_EPOCH + Duration::from_secs(secs))
}

/// 2026-08-22T09:41:07Z, an instant with no round numbers in it.
const AT: u64 = 1_787_391_667;

/// TC-PORT-RTCTX-1: the time a turn was prepared, in the words the model reads.
///
/// Upstream: `packages/context/time-context`, whose reading names the turn it
/// was sampled for. Restated rather than transcribed in one place, and the
/// difference is deliberate: upstream renders in a display time zone, resolved
/// from configuration, the process or the browser, and tetanus reports UTC,
/// because a display zone means a time-zone database and this workspace has no
/// such dependency. A reading nobody can misread beats a local time this build
/// cannot be sure it converted.
///
/// Input: the time provider on a clock stopped at a known instant, asked for
/// turn 7.
/// Expected: one sentence naming the turn and the instant, with the zone
/// stated rather than implied.
#[test]
fn the_time_reading_names_its_turn_and_its_zone() {
    let produce = time_provider(frozen(AT));

    let text = produce(&ContextAt { turn: 7 });

    assert_eq!(
        text,
        "Time sampled while preparing turn 7: 2026-08-22T09:41:07Z"
    );
}

/// TC-RTCTX-2: the parts are joined the way prompt sections are.
///
/// Section 4.4.8 spends a paragraph on this so that a reader of the journal
/// can reproduce exactly what the model saw. It is section 4.3's rule for
/// prompt sections on purpose: two joining rules would be one too many.
///
/// Input: three parts, one of them empty, then a set that is all empty.
/// Expected: the non-empty texts in list order, separated by a blank line; the
/// empty part contributes nothing at all, not a gap; an all-empty set renders
/// to nothing, which is what the engine reads as "write no snapshot".
#[test]
fn the_parts_join_the_way_prompt_sections_do() {
    let part = |name: &str, text: &str| ContextPart {
        name: name.into(),
        text: text.into(),
    };

    assert_eq!(
        render(&[
            part("time", "It is now"),
            part("tmux", ""),
            part("git", "On main")
        ]),
        "It is now\n\nOn main"
    );
    assert_eq!(render(&[part("time", ""), part("git", "")]), "");
    assert_eq!(render(&[]), "");
}

/// TC-RTCTX-3: providers are gathered in the order the deployment set.
///
/// There is no priority field on the durable record, and that is the design:
/// which provider comes first is configuration, settled before the snapshot is
/// written. An order on the wire would let two readers disagree about the text
/// the model actually saw.
///
/// Input: three providers registered out of order, then one dropped.
/// Expected: the snapshot is in ascending order and names each provider; a
/// dropped registration leaves the rest untouched, which is what makes a
/// provider a normal effect rather than a special case.
#[test]
fn providers_are_gathered_in_the_order_they_were_given() {
    let registry = ContextRegistry::new();
    let _last = registry.provider("git", 10, |_| "On main".to_string());
    let _first = registry.provider("time", -10, |at| format!("turn {}", at.turn));
    let middle = registry.provider("tmux", 0, |_| "pane 1".to_string());

    let gathered = registry.snapshot(&ContextAt { turn: 3 });

    assert_eq!(
        gathered
            .iter()
            .map(|part| (part.name.as_str(), part.text.as_str()))
            .collect::<Vec<_>>(),
        [("time", "turn 3"), ("tmux", "pane 1"), ("git", "On main")]
    );

    drop(middle);
    let after = registry.snapshot(&ContextAt { turn: 4 });
    assert_eq!(
        after.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["time", "git"],
        "dropping one registration removes exactly that one"
    );
}

/// TC-RTCTX-4: a turn records what it gathered, once, before its first step.
///
/// The position is the contract's: `turn/start`, then `context/snapshot`, then
/// `step/start`. It is once per turn and not once per step because a snapshot
/// is a fact about when the turn began - nothing re-reads it, so a step that
/// runs for ten minutes works from the time the turn started with.
///
/// Input: a session with the time provider registered, run for one turn that
/// takes two steps.
/// Expected: exactly one `context/snapshot` on the file, carrying the turn and
/// the parts, sitting between `turn/start` and the first `step/start`.
#[tokio::test]
async fn a_turn_records_its_context_once_before_the_first_step() {
    let h = Harness::new("rtctx-recorded").await;
    let _time = h
        .context()
        .provider(TIME_PART, TIME_ORDER, time_provider(frozen(AT)));

    h.engine.run_turn("call the tool").await.expect("the turn");

    let events = h.journal();
    let snapshots: Vec<&SessionEvent> = events
        .iter()
        .filter(|event| event.ty == topic::CONTEXT_SNAPSHOT)
        .collect();
    assert_eq!(snapshots.len(), 1, "one turn, one snapshot");
    assert_eq!(snapshots[0].data["turn"], 1);
    assert_eq!(snapshots[0].data["parts"][0]["name"], TIME_PART);
    assert_eq!(
        snapshots[0].data["parts"][0]["text"],
        "Time sampled while preparing turn 1: 2026-08-22T09:41:07Z"
    );

    let order: Vec<&str> = events
        .iter()
        .map(|event| event.ty.as_str())
        .filter(|ty| {
            matches!(
                *ty,
                topic::TURN_START | topic::CONTEXT_SNAPSHOT | topic::STEP_START
            )
        })
        .collect();
    assert_eq!(
        order[..3],
        [
            topic::TURN_START,
            topic::CONTEXT_SNAPSHOT,
            topic::STEP_START
        ],
        "the snapshot sits between the turn opening and its first step"
    );
}

/// TC-RTCTX-5: a deployment that configures no providers pays nothing.
///
/// Stated by the contract in those words, and worth a case because the cheap
/// implementation - write the record and let it be empty - costs a journal
/// line and a message on every turn of every session that never asked for one.
///
/// Input: the same harness with no provider registered, and then one whose
/// text is empty.
/// Expected: no `context/snapshot` at all in either run, and a derived history
/// identical to the one a build without this feature produced.
#[tokio::test]
async fn no_provider_and_an_empty_one_both_write_nothing() {
    let h = Harness::new("rtctx-silent").await;
    h.engine.run_turn("first").await.expect("the turn");

    let quiet = h.context().provider("quiet", 0, |_| String::new());
    h.engine.run_turn("second").await.expect("the turn");
    drop(quiet);

    let events = h.journal();
    assert!(
        !events
            .iter()
            .any(|event| event.ty == topic::CONTEXT_SNAPSHOT),
        "a snapshot of nothing was written anyway"
    );
    assert!(
        derive_messages(&events)
            .iter()
            .all(|message| message.role != Role::User || !message.content.is_empty()),
        "no empty user message reached the history"
    );
}

/// TC-RTCTX-6: only the newest snapshot travels, and the older ones stay.
///
/// The rule that makes this feature safe over a long session. A turn writes
/// one, so a hundred turns write a hundred, and yesterday's date is worse than
/// no date. They stay on the journal because the journal records what
/// happened: a reader may want to know what the model was told at the time.
///
/// Input: three turns with a clock that advances a day between them.
/// Expected: three snapshots on the file; exactly one derived user message
/// carrying a context reading, and it is the newest one.
#[tokio::test]
async fn only_the_newest_context_becomes_a_message() {
    let h = Harness::new("rtctx-newest").await;
    let clock = Arc::new(AtomicU64::new(AT));
    let ticking = Arc::clone(&clock);
    let _time = h.context().provider(
        TIME_PART,
        TIME_ORDER,
        time_provider(Arc::new(move || {
            UNIX_EPOCH + Duration::from_secs(ticking.load(Ordering::Relaxed))
        })),
    );

    for _ in 0..3 {
        h.engine.run_turn("again").await.expect("the turn");
        clock.fetch_add(86_400, Ordering::Relaxed);
    }

    let events = h.journal();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.ty == topic::CONTEXT_SNAPSHOT)
            .count(),
        3,
        "every turn recorded what it was told"
    );

    let readings: Vec<String> = derive_messages(&events)
        .into_iter()
        .filter(|message| message.content.starts_with("Time sampled"))
        .map(|message| message.content)
        .collect();
    assert_eq!(
        readings,
        ["Time sampled while preparing turn 3: 2026-08-24T09:41:07Z"],
        "one reading, and it is the last turn's"
    );
}

/// TC-RTCTX-7: the reading travels after the retained history and before the
/// message that opened the turn.
///
/// The first half is the whole design and not a detail of ordering. A provider
/// caches a prompt by its longest stable prefix; the system prompt is
/// identical on every turn of a session, so it caches, and a sentence saying
/// what time it is would invalidate that prefix on every request of every
/// session. So it is a user message, never a prompt section.
///
/// The second half is where this deliberately reads section 4.4.8's "after the
/// retained history" as the journal's own order rather than as "last".
/// Upstream appends after the messages a step entered, so its request ends in
/// a machine-written status block; a request whose last message is one is a
/// request that ends in something nobody asked for, and the thing that reads
/// the last message as the thing to answer is not only a model - this crate's
/// own mock adapter does, and with the block last it answered the block and
/// the turn never settled. The journal's order gives the caching property for
/// free and keeps a request ending in the user's ask or a tool's result.
///
/// Input: a prior turn, so there is retained history, then a turn with the
/// provider registered.
/// Expected: the reading appears exactly once, immediately before this turn's
/// user message, as a `user` message, with the earlier turn's history before
/// it and the answer after it.
#[tokio::test]
async fn the_reading_sits_between_the_history_and_this_turns_message() {
    let h = Harness::new("rtctx-position").await;
    h.engine.run_turn("earlier").await.expect("the first turn");
    let _time = h
        .context()
        .provider(TIME_PART, TIME_ORDER, time_provider(frozen(AT)));

    h.engine.run_turn("hello").await.expect("the turn");

    let history = derive_messages(&h.journal());
    let at = history
        .iter()
        .position(|message| message.content.starts_with("Time sampled"))
        .expect("the reading reached the history");

    assert_eq!(history[at].role, Role::User);
    assert_eq!(
        history
            .iter()
            .filter(|message| message.content.starts_with("Time sampled"))
            .count(),
        1,
        "the reading appears once"
    );
    assert_eq!(
        (history[at + 1].role, history[at + 1].content.as_str()),
        (Role::User, "hello"),
        "the turn's own message follows the reading"
    );
    assert!(
        history[..at]
            .iter()
            .any(|message| message.content == "earlier"),
        "the retained history comes first"
    );
    assert_ne!(
        history.last().expect("a history").role,
        Role::User,
        "the request does not end in a block nobody asked for"
    );
}

/// TC-RTCTX-8: a replay derives the same history the run did.
///
/// The claim behind carrying parts rather than rendered text: the rendering is
/// reproducible from the record, so nothing is lost by not storing it. A
/// resumed session that showed the model a different sentence from the one the
/// journal records would make the transcript a fiction.
///
/// Input: a finished session, read back off the file and derived again.
/// Expected: the derived history is identical, message for message.
#[tokio::test]
async fn a_replay_derives_the_same_reading() {
    let h = Harness::new("rtctx-replay").await;
    let _time = h
        .context()
        .provider(TIME_PART, TIME_ORDER, time_provider(frozen(AT)));

    h.engine.run_turn("hello").await.expect("the turn");

    let live = derive_messages(&h.engine.log().events());
    let replayed = derive_messages(&h.journal());

    assert_eq!(live.len(), replayed.len());
    for (from_memory, from_file) in live.iter().zip(replayed.iter()) {
        assert_eq!(from_memory.role, from_file.role);
        assert_eq!(from_memory.content, from_file.content);
    }
    assert_eq!(
        replayed
            .iter()
            .filter(|message| message.content.starts_with("Time sampled"))
            .count(),
        1,
        "the replay carries the reading the run showed, once"
    );
}

/// TC-RTCTX-9: a snapshot is a fact about when the turn began.
///
/// Nothing re-reads it, which is what lets a reader of the journal say what
/// the model believed at any point in the turn. A provider called again
/// mid-turn would make the record a lie about the second step.
///
/// Input: a counting provider, one turn that takes two steps.
/// Expected: the provider was asked exactly once, and the second step's
/// request carried the same reading as the first.
#[tokio::test]
async fn the_context_is_gathered_once_and_not_re_read() {
    let h = Harness::new("rtctx-once").await;
    let calls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&calls);
    let _counting = h.context().provider("counter", 0, move |at| {
        let nth = counted.fetch_add(1, Ordering::Relaxed) + 1;
        format!("gathering {nth} for turn {}", at.turn)
    });

    h.engine.run_turn("call the tool").await.expect("the turn");

    assert!(
        h.journal()
            .iter()
            .any(|event| event.ty == topic::STEP_START && event.data["step"] == 2),
        "the case needs a turn of more than one step to mean anything"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the context was gathered once for the turn, not once per step"
    );
}

/// TC-RTCTX-10: an unreadable clock does not fail the turn.
///
/// A context provider is a convenience, and a turn that dies because a clock
/// answered something strange is a worse outcome than a turn told a slightly
/// wrong time. The workspace has one hand-written date conversion and this is
/// the case that says what it does at the edges rather than leaving it to a
/// panic in production.
///
/// Input: a clock at the epoch and one before it.
/// Expected: both render, neither panics, and the pre-epoch instant reads as
/// the epoch rather than as a negative year.
#[test]
fn a_clock_at_the_edges_still_renders() {
    let at_epoch = time_provider(frozen(0));
    assert_eq!(
        at_epoch(&ContextAt { turn: 1 }),
        "Time sampled while preparing turn 1: 1970-01-01T00:00:00Z"
    );

    let before: tetanus_turn::context::Clock =
        Arc::new(|| UNIX_EPOCH - Duration::from_secs(86_400));
    let earlier = time_provider(before);
    assert_eq!(
        earlier(&ContextAt { turn: 1 }),
        "Time sampled while preparing turn 1: 1970-01-01T00:00:00Z",
        "a clock before the epoch reads as the epoch, not as a negative year"
    );
}

/// TC-RTCTX-11: a provider that panics contributes nothing, and the turn runs.
///
/// A runtime context is a decoration on the work, not the work. A provider
/// reads a clock, a branch, an environment variable - things that are absent
/// or malformed on somebody's machine - and the deployment that installed one
/// is rarely the person holding the conversation. Letting a panicking provider
/// end the turn trades a missing sentence for an agent that stops working,
/// which is the worse failure by a wide margin and the harder one to diagnose.
///
/// `crates/turn/src/tools.rs` contains a tool's classifier the same way and
/// for the same reason; this is that rule applied to the other plugin callback
/// the turn makes.
///
/// Input: two providers, the first of which panics.
/// Expected: the snapshot has both parts, the panicking one empty; the healthy
/// one is unaffected; and the reading the model sees is just the healthy text.
#[test]
fn a_panicking_provider_contributes_nothing_and_the_rest_still_speak() {
    static QUIET: std::sync::Once = std::sync::Once::new();
    QUIET.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info
                .to_string()
                .contains("deliberate context provider fault")
            {
                previous(info);
            }
        }));
    });

    let registry = ContextRegistry::new();
    let _faulty = registry.provider("faulty", 0, |_at| {
        panic!("deliberate context provider fault")
    });
    let _healthy = registry.provider("healthy", 1, |_at| "the branch is main".to_owned());

    let parts = registry.snapshot(&ContextAt { turn: 1 });

    assert_eq!(
        parts
            .iter()
            .map(|p| (p.name.as_str(), p.text.as_str()))
            .collect::<Vec<_>>(),
        [("faulty", ""), ("healthy", "the branch is main")],
        "the faulty provider is still named, with nothing to say"
    );
    assert_eq!(
        render(&parts),
        "the branch is main",
        "and an empty part leaves no gap in what the model reads"
    );
}
