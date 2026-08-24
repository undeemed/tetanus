//! Test Design Specification: searching a session's words.
//!
//! Feature under test: `tetanus_query::search` - term matching over the
//! journal's conversation text, the compaction surface a hit is labelled with,
//! snippets, and cursor paging. This is the search half of upstream's
//! `session-query/*`, which `docs/parity.md` marks phase ③ and which the query
//! slice's own note listed as the remaining clause.
//!
//! Approach: the cases that claim something about a real session run a real
//! turn on the offline mock adapter through the real `HarnessEngine` and read
//! the journal back through `session.events`, exactly as
//! `upstream_query.rs` does. The cases about compaction build their log by
//! hand, because a compacted session is a boundary the mock adapter will not
//! reach in one turn and the point of those cases is the label, not the
//! compactor.
//!
//! Features NOT tested here: the compaction fold itself, which
//! `crates/turn/tests` owns - this crate is *given* a surface and never derives
//! one, which is the property TC-PORT-QUERY-24 exists to pin.
//!
//! Environmental needs: a writable temp directory. No case reaches a network or
//! an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams};
use tetanus_protocol::types::SessionEvent;
use tetanus_query::{Cursor, EventFilter, Journal, QueryError, Role, SearchQuery, Surface};

/// A real session with `turns` mock turns, read back as a journal.
async fn session(dir: &TempDir, turns: usize) -> Journal {
    let engine: Arc<dyn Engine> = Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().join("sessions"),
        ..EngineConfig::default()
    }));
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");
    for turn in 0..turns {
        engine
            .agent_prompt(AgentPromptParams {
                session_id: info.session_id.clone(),
                content: format!("hello {turn}"),
            })
            .await
            .expect("prompt");
    }
    tetanus_query::source::load(&engine, &info.session_id)
        .await
        .expect("load")
}

fn event(seq: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.into(),
        seq,
        time: 1_000 + seq,
        data,
        source_event_seqs: None,
    }
}

/// A log of user messages, one per word given.
fn said(words: &[&str]) -> Vec<SessionEvent> {
    words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            event(
                index as u64,
                "user/message",
                serde_json::json!({ "content": word }),
            )
        })
        .collect()
}

/// TC-PORT-QUERY-20: a search finds the events whose words match, in sequence
/// order.
///
/// Input: one real mock turn, searched for `echo`.
/// Expected: the hits are the events that actually contain the word - the tool
/// call and the assistant's line about it - in ascending seq, each carrying its
/// turn, its step and the term it matched. Ordering is by seq and not by any
/// score: a scan has no relevance number, and inventing one would be this crate
/// making up a model no caller could tune.
#[tokio::test]
async fn a_search_finds_matching_events_in_sequence_order() {
    let dir = TempDir::new().expect("temp dir");
    let journal = session(&dir, 1).await;

    let found = journal
        .search(&SearchQuery::new(["echo"]), None)
        .expect("search");

    assert!(found.total >= 2, "the mock turn says `echo` more than once");
    assert_eq!(found.hits.len(), found.total, "one page holds them all");
    let seqs: Vec<u64> = found.hits.iter().map(|hit| hit.seq).collect();
    let mut ascending = seqs.clone();
    ascending.sort_unstable();
    assert_eq!(seqs, ascending, "sequence order, not score order");

    for hit in &found.hits {
        assert_eq!(hit.matched, vec!["echo".to_string()]);
        assert!(
            hit.snippet.to_lowercase().contains("echo"),
            "the snippet shows the match: {:?}",
            hit.snippet,
        );
        assert_eq!(hit.turn, Some(1), "positioned like every other read");
    }
}

/// TC-PORT-QUERY-21: `all` requires every term; the default requires any.
///
/// Input: a log of three one-word messages, searched for two of those words
/// both ways, then a two-word message searched with `all`.
/// Expected: any-of matches the two events; all-of matches neither, because no
/// single event holds both; and all-of does match the event that holds both.
/// The distinction is the whole difference between a filter and a search box: a
/// person typing two words means "an event about both".
#[test]
fn all_of_and_any_of_are_different_questions() {
    let journal = Journal::new("s", said(&["apples", "oranges", "apples and oranges"]));

    let any = journal
        .search(&SearchQuery::new(["apples", "oranges"]), None)
        .expect("search");
    assert_eq!(any.total, 3, "every event mentions one or the other");

    let all = journal
        .search(&SearchQuery::new(["apples", "oranges"]).all(), None)
        .expect("search");
    assert_eq!(all.total, 1, "only the event holding both");
    assert_eq!(all.hits[0].seq, 2);
    assert_eq!(all.hits[0].matched, vec!["apples", "oranges"]);
}

/// TC-PORT-QUERY-22: a hit from a compacted-away span is labelled, not hidden
/// and not silently shown.
///
/// Input: a log whose first two messages have been shadowed by a compaction,
/// with the surviving seqs supplied as the surface, searched for a word in both
/// halves.
/// Expected: every match comes back, and the ones inside the shadowed span say
/// `Shadowed` while the rest say `Current`. Hiding them would lose text that is
/// really on the log; showing them unlabelled would tell a person the model can
/// see something it has lost. Upstream carries the same field for the same
/// reason.
#[test]
fn a_hit_the_model_can_no_longer_see_says_so() {
    let journal = Journal::new(
        "s",
        said(&["timeout early", "timeout also early", "timeout later"]),
    )
    // Seqs 0 and 1 were replaced by a summary; only 2 survives.
    .with_surface([2]);

    let found = journal
        .search(&SearchQuery::new(["timeout"]), None)
        .expect("search");

    assert_eq!(found.total, 3, "nothing is hidden");
    let labelled: Vec<(u64, Surface)> = found
        .hits
        .iter()
        .map(|hit| (hit.seq, hit.surface))
        .collect();
    assert_eq!(
        labelled,
        vec![
            (0, Surface::Shadowed),
            (1, Surface::Shadowed),
            (2, Surface::Current),
        ],
    );
}

/// TC-PORT-QUERY-23: with no surface supplied, a hit says `Unknown` rather than
/// claiming to be visible.
///
/// Input: the same log, searched without `with_surface`.
/// Expected: every hit is `Unknown`. "We checked and it is visible" and "nobody
/// checked" are different facts. Defaulting to `Current` would be the more
/// convenient lie, and it would be wrong on exactly the sessions - long,
/// compacted ones - where a person is most likely to be searching.
#[test]
fn an_unchecked_surface_is_unknown_and_not_current() {
    let journal = Journal::new("s", said(&["timeout early", "timeout later"]));

    let found = journal
        .search(&SearchQuery::new(["timeout"]), None)
        .expect("search");

    assert_eq!(found.total, 2);
    assert!(
        found.hits.iter().all(|hit| hit.surface == Surface::Unknown),
        "{:?}",
        found.hits,
    );
}

/// TC-PORT-QUERY-24: this crate never derives the surface itself.
///
/// Input: a journal told that seq 0 is the only visible event, when in truth
/// nothing in the log says anything about compaction at all.
/// Expected: the labels follow what was supplied, exactly. This is a structural
/// case rather than a behavioural one: it passes only while the surface is an
/// input, and it fails the moment somebody adds a second fold here that
/// "corrects" the caller. `AGENTS.md` is explicit that one reader derives model
/// history, and that a second disagrees with it the first time a session
/// compacts.
#[test]
fn the_supplied_surface_is_obeyed_and_never_second_guessed() {
    let journal = Journal::new("s", said(&["alpha", "alpha", "alpha"])).with_surface([0]);

    let found = journal
        .search(&SearchQuery::new(["alpha"]), None)
        .expect("search");

    let labelled: Vec<Surface> = found.hits.iter().map(|hit| hit.surface).collect();
    assert_eq!(
        labelled,
        vec![Surface::Current, Surface::Shadowed, Surface::Shadowed],
        "no fold of its own overrode what the caller said",
    );
}

/// TC-PORT-QUERY-25: a search pages, and the total is the session's and not the
/// page's.
///
/// Input: five matching events, read two at a time by following the cursor.
/// Expected: three pages of 2, 2 and 1; every page reports `total: 5`; the last
/// page returns no cursor; and the five seqs arrive once each, in order. The
/// total staying 5 is the assertion that matters - a caller drawing "showing 2
/// of 5" would otherwise watch the 5 shrink as it paged.
#[test]
fn a_search_pages_and_the_total_is_the_whole_session() {
    let journal = Journal::new("s", said(&["hit a", "hit b", "hit c", "hit d", "hit e"]));
    let query = SearchQuery::new(["hit"]).limit(2);

    let mut seen = Vec::new();
    let mut pages = 0;
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = journal.search(&query, cursor.as_ref()).expect("search");
        assert_eq!(page.total, 5, "the total is a fact about the session");
        seen.extend(page.hits.iter().map(|hit| hit.seq));
        pages += 1;
        match page.cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(pages < 10, "the cursor must terminate");
    }

    assert_eq!(pages, 3, "2 + 2 + 1");
    assert_eq!(seen, vec![0, 1, 2, 3, 4], "each once, in order");
}

/// TC-PORT-QUERY-26: a cursor from one search is refused by another.
///
/// Input: a cursor issued by a search for `alpha`, presented to a search for
/// `beta`; then a token that was never a cursor.
/// Expected: both are refused. The seqs would line up and the answer would look
/// entirely plausible, which is exactly why this cannot be permitted: a wrong
/// page that reads as a right one is worse than an error. The bound is the
/// query's shape and not its page size, so re-paging the same search with a
/// different limit is still allowed.
#[test]
fn a_cursor_belongs_to_the_search_that_issued_it() {
    let journal = Journal::new(
        "s",
        said(&["alpha one", "alpha two", "beta one", "beta two"]),
    );

    let first = journal
        .search(&SearchQuery::new(["alpha"]).limit(1), None)
        .expect("search");
    let cursor = first.cursor.expect("more to come");

    let refused = journal
        .search(&SearchQuery::new(["beta"]).limit(1), Some(&cursor))
        .expect_err("a cursor from another search");
    assert!(
        matches!(refused, QueryError::InvalidWindow(_)),
        "{refused:?}",
    );

    // The same search with a different page size is the same question.
    let wider = journal.search(&SearchQuery::new(["alpha"]).limit(50), Some(&cursor));
    assert!(wider.is_ok(), "{wider:?}");

    let nonsense = Cursor::decode("not-a-cursor");
    assert!(matches!(nonsense, Err(QueryError::InvalidWindow(_))));
}

/// TC-PORT-QUERY-27: a cursor survives being written down.
///
/// Input: a cursor encoded to a token and read back.
/// Expected: the decoded cursor pages identically to the original. A carrier
/// has to put this in JSON and a browser in a URL, so a cursor that only works
/// as a Rust value is a cursor that does not work.
#[test]
fn a_cursor_round_trips_through_its_token() {
    let journal = Journal::new("s", said(&["hit a", "hit b", "hit c"]));
    let query = SearchQuery::new(["hit"]).limit(1);

    let first = journal.search(&query, None).expect("search");
    let token = first.cursor.expect("more").encode();
    let restored = Cursor::decode(&token).expect("decode");

    let next = journal.search(&query, Some(&restored)).expect("search");
    assert_eq!(next.hits.len(), 1);
    assert_eq!(next.hits[0].seq, 1, "resumed exactly where it left off");
}

/// TC-PORT-QUERY-28: a search can be narrowed by the same filter the rest of
/// the crate takes.
///
/// Input: one real turn, searched for `echo` restricted to tool events.
/// Expected: fewer hits than the unrestricted search, and every one of them is
/// about a tool. Search and filter compose rather than being two passes a
/// caller has to intersect by hand.
#[tokio::test]
async fn a_search_narrows_through_the_ordinary_filter() {
    let dir = TempDir::new().expect("temp dir");
    let journal = session(&dir, 1).await;

    let everywhere = journal
        .search(&SearchQuery::new(["echo"]), None)
        .expect("search");
    let only_tools = journal
        .search(
            &SearchQuery::new(["echo"]).filter(EventFilter::default().roles([Role::Tool])),
            None,
        )
        .expect("search");

    assert!(only_tools.total >= 1, "the tool call mentions echo");
    assert!(
        only_tools.total < everywhere.total,
        "the filter narrowed it: {} vs {}",
        only_tools.total,
        everywhere.total,
    );
    assert!(only_tools.hits.iter().all(|hit| hit.tool.is_some()));
}

/// TC-PORT-QUERY-29: a blank search is refused rather than answered with the
/// session.
///
/// Input: no terms, then one term of pure whitespace.
/// Expected: both refused as a malformed ask. A search box that submits empty
/// means "I have not typed anything yet", and returning the whole session is
/// the least useful possible reply - and the most expensive, on the long
/// sessions where somebody is actually searching.
#[test]
fn a_blank_search_is_refused() {
    let journal = Journal::new("s", said(&["anything"]));

    for terms in [vec![], vec!["   "]] {
        let refused = journal
            .search(&SearchQuery::new(terms.clone()), None)
            .expect_err("a blank search");
        assert!(
            matches!(refused, QueryError::InvalidFilter(_)),
            "{terms:?} gave {refused:?}",
        );
    }
}

/// TC-PORT-QUERY-30: a snippet is cut on characters, never on bytes.
///
/// Input: a long message padded with multi-byte characters, with the match near
/// the end.
/// Expected: a snippet that contains the match, is bounded, and is valid text.
/// Slicing a UTF-8 string at a byte offset panics mid-character, and a
/// transcript is exactly where a multi-byte character turns up - an emoji in a
/// commit message, a CJK path, an accented name.
#[test]
fn a_snippet_is_cut_on_characters_and_not_on_bytes() {
    let padding = "é🙂漢".repeat(200);
    let text = format!("{padding} needle");
    let journal = Journal::new("s", said(&[text.as_str()]));

    let found = journal
        .search(&SearchQuery::new(["needle"]), None)
        .expect("search");

    assert_eq!(found.total, 1);
    let snippet = &found.hits[0].snippet;
    assert!(snippet.contains("needle"), "{snippet:?}");
    assert!(
        snippet.chars().count() <= 162,
        "bounded: {} chars",
        snippet.chars().count(),
    );
    // The real assertion is that we got here at all: a byte-offset slice would
    // have panicked before returning.
    assert!(
        snippet.starts_with('…'),
        "the head was trimmed: {snippet:?}"
    );
}

/// TC-PORT-QUERY-31: paging is counted in matches, not in events.
///
/// Input: matches separated by events that do not match, paged one at a time.
/// Expected: every match arrives exactly once and the walk terminates on the
/// last one. This is the case that distinguishes a correct implementation from
/// one that decides "are there more" by counting *events* before the cursor:
/// with a match on every event the two agree, and they diverge the moment a
/// non-matching event sits in between - which is every real session. The first
/// cut of this module had that bug, and TC-PORT-QUERY-25 could not see it
/// because every event in its log was a hit.
#[test]
fn paging_counts_matches_and_not_events() {
    let journal = Journal::new(
        "s",
        // The fillers sit *before* the matches on purpose. With them after,
        // the count of events before the cursor never exceeds the number of
        // matches and a wrong implementation agrees with a right one by luck -
        // which is how the first draft of this case passed against the bug it
        // was written to catch.
        said(&[
            "filler",
            "filler",
            "filler",
            "filler",
            "filler",
            "needle one",
            "filler",
            "needle two",
            "needle three",
        ]),
    );
    let query = SearchQuery::new(["needle"]).limit(1);

    let mut seen = Vec::new();
    let mut cursor: Option<Cursor> = None;
    for _ in 0..10 {
        let page = journal.search(&query, cursor.as_ref()).expect("search");
        assert_eq!(page.total, 3, "three matches among nine events");
        seen.extend(page.hits.iter().map(|hit| hit.seq));
        match page.cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(seen, vec![5, 7, 8], "each match once, and none dropped");
}
