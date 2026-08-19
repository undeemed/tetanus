//! The journals this build has written, as a page to pick one from.
//!
//! `tetanus sessions` answers `session.list`. A session is a file on disk and
//! an id every other call takes, so this view is read for one reason: to find
//! the id of the turn you want next.
//!
//! # Why the id is never cut
//!
//! Every other column here is something to read; the id is something to
//! retype. A truncated id is worse than no id, so the id column takes the
//! width it needs and the title - the one column that is only ever read -
//! takes whatever the terminal has left.
//!
//! # Why the newest is first
//!
//! `session.list` answers in the engine's own order, which is by id. That is
//! the right order for a machine, and the wrong one for a person: the session
//! you want is almost always the one you just wrote. So this view sorts by
//! creation time, newest first, and leaves the call's own order to `--json`,
//! which prints the result verbatim.
//!
//! # Why there is no date column
//!
//! `created_time` is milliseconds since the epoch. Rendering it as a date
//! needs a calendar this build does not carry, and rendering it as an age
//! ("2m ago") would make the same journal print differently every run, which
//! the surface promises it never does. The order carries the recency instead.

use std::io::{self, Write};

use tetanus_protocol::methods::SessionListResult;
use tetanus_protocol::types::{AgentState, SessionInfo};
use tetanus_ui::{truncate, Role, Theme, Ui};

/// Space between two columns of a row.
const GAP: usize = 2;

/// The sessions in the order a person reads them, newest first.
///
/// Separate from [`rows`] because the picker needs both halves and needs them
/// apart: which session a row stands for is what Enter opens, and it would
/// have to find that out again from a string if this returned only the text.
pub fn ordered(list: &SessionListResult) -> Vec<&SessionInfo> {
    let mut rows: Vec<&SessionInfo> = list.sessions.iter().collect();
    // Reverse by time, then forward by id, so two sessions created inside the
    // same millisecond still print in one settled order every run.
    rows.sort_by(|a, b| {
        b.created_time
            .cmp(&a.created_time)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    rows
}

/// One row per session, composed for a terminal `cols` wide: the id to
/// retype, its size, its state, and what it was about.
pub fn rows(theme: &Theme, cols: usize, sessions: &[&SessionInfo]) -> Vec<String> {
    let charset = theme.charset();
    let id = width(sessions.iter().map(|row| row.session_id.chars().count()));
    let digits = width(sessions.iter().map(|row| count(row).to_string().len()));
    let size = width(
        sessions
            .iter()
            .map(|row| measure(row, digits).chars().count()),
    );
    let state = width(sessions.iter().map(|row| named(row).chars().count()));
    let room = cols.saturating_sub(id + size + state + GAP * 3).max(1);
    let gap = " ".repeat(GAP);

    sessions
        .iter()
        .map(|row| {
            format!(
                "{:<id$}{gap}{:<size$}{gap}{:<state$}{gap}{}",
                theme.paint(Role::Accent, &row.session_id),
                theme.paint(Role::Muted, &measure(row, digits)),
                theme.paint(role(row), &named(row)),
                title(theme, row, room, charset),
            )
        })
        .collect()
}

/// Print the list under a heading.
pub fn render<W: Write>(ui: &mut Ui<W>, list: &SessionListResult) -> io::Result<()> {
    ui.heading("sessions")?;
    if list.sessions.is_empty() {
        let empty = ui
            .paint(Role::Muted, "no sessions yet - tetanus run writes one")
            .to_string();
        return ui.line(&empty);
    }

    let composed = rows(ui.theme(), ui.width(), &ordered(list));
    for row in composed {
        ui.line(&row)?;
    }
    Ok(())
}

/// How many events the journal holds. `last_seq` is `-1` for an empty log, so
/// a session created and never prompted honestly reads `0 events`.
fn count(row: &SessionInfo) -> i64 {
    row.last_seq + 1
}

/// The size cell, with the number right-aligned inside it so the digits form
/// a column even when the word beside them changes.
fn measure(row: &SessionInfo, digits: usize) -> String {
    let held = count(row);
    let word = if held == 1 { "event" } else { "events" };
    format!("{held:>digits$} {word}")
}

/// What the session is doing, in the contract's own word for it.
fn named(row: &SessionInfo) -> String {
    match &row.state {
        AgentState::Idle => "idle".to_string(),
        AgentState::Running => "running".to_string(),
        AgentState::Other(other) => other.clone(),
    }
}

/// A running session is the one a reader is looking for, so it is the one
/// that carries the colour. An unknown state is a warning: this build cannot
/// say whether that session will take a prompt.
fn role(row: &SessionInfo) -> Role {
    match &row.state {
        AgentState::Idle => Role::Muted,
        AgentState::Running => Role::Accent,
        AgentState::Other(_) => Role::Warn,
    }
}

/// The session's first prompt, cut to the room the fixed columns left.
fn title(theme: &Theme, row: &SessionInfo, room: usize, charset: tetanus_ui::Charset) -> String {
    match &row.title {
        Some(title) => truncate(title, room, charset),
        None => theme
            .paint(Role::Muted, &truncate("no prompt yet", room, charset))
            .to_string(),
    }
}

/// The widest of a set of cells, in characters a terminal draws.
fn width(cells: impl Iterator<Item = usize>) -> usize {
    cells.max().unwrap_or(0)
}

/// Test Design Specification: the session list.
///
/// Features tested: the four columns and their alignment; that the newest
/// session is first and a tie is still settled; that an empty journal reads
/// `0 events` and a one-event journal is singular; that a session with no
/// prompt yet says so; that a title too long for the terminal is cut while
/// the id is not; and the empty list.
///
/// Features NOT tested here: which sessions this build finds (owned by
/// `tetanus-engine`, and asserted end to end in `tests/presentation.rs`), the
/// JSON form (owned by `render::json`), and the colour policy (owned by
/// `tetanus-ui`).
///
/// Environmental needs: none. Every case renders into a `Vec<u8>`, so no case
/// touches the filesystem or reads an environment variable.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn session(id: &str, created: u64, last_seq: i64, title: Option<&str>) -> SessionInfo {
        SessionInfo {
            session_id: id.into(),
            path: format!("sessions/{id}.jsonl"),
            provider: "mock".into(),
            model: "mock-echo-1".into(),
            created_time: created,
            last_seq,
            title: title.map(Into::into),
            state: AgentState::Idle,
        }
    }

    fn shown(sessions: Vec<SessionInfo>, width: usize) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        render(&mut ui, &SessionListResult { sessions }).expect("render");
        ui.contents()
    }

    /// TC-CLI-SESS-1: two sessions of different sizes and ages.
    /// Expected: the newest first, ids in one column, the digits of the size
    /// right-aligned against each other, and the title last. This is the row
    /// a user reads an id out of, so every column it carries is asserted.
    #[test]
    fn the_newest_session_is_first_and_the_columns_line_up() {
        let out = shown(
            vec![
                session("turn", 1_000, 5, Some("echo this")),
                session("s1755", 2_000, 17, Some("summarise the release notes")),
            ],
            80,
        );

        assert_eq!(
            out,
            "\nsessions\n\
             s1755  18 events  idle  summarise the release notes\n\
             turn    6 events  idle  echo this\n"
        );
    }

    /// TC-CLI-SESS-2: two sessions created in the same millisecond.
    /// Expected: a settled order, by id. Two runs of the same command print
    /// the same bytes, which a sort by time alone would not guarantee.
    #[test]
    fn a_tie_on_time_is_still_settled() {
        let rows = vec![
            session("b", 7, 0, Some("second")),
            session("a", 7, 0, Some("first")),
        ];
        let out = shown(rows.clone(), 80);

        assert!(
            out.find("first") < out.find("second"),
            "the tie was not settled:\n{out}"
        );
        assert_eq!(out, shown(rows.into_iter().rev().collect(), 80));
    }

    /// TC-CLI-SESS-3: an empty journal and a one-event journal.
    /// Expected: `0 events` for the empty one - `last_seq` is `-1` there, and
    /// a row reading `-1 events` would be nonsense - and the singular for the
    /// one that holds exactly one.
    #[test]
    fn a_journal_is_counted_in_events_and_the_singular_is_spelled() {
        let out = shown(
            vec![
                session("fresh", 2, -1, None),
                session("one", 1, 0, Some("hi")),
            ],
            80,
        );

        assert!(out.contains("0 events"), "{out}");
        assert!(out.contains("1 event  "), "{out}");
        assert!(!out.contains("-1"), "an empty journal counted down:\n{out}");
    }

    /// TC-CLI-SESS-4: a session created but never prompted.
    /// Expected: the title column says so rather than leaving the row to end
    /// in whitespace, which reads as a rendering fault.
    #[test]
    fn a_session_with_no_prompt_yet_says_so() {
        let out = shown(vec![session("fresh", 1, -1, None)], 80);
        assert!(out.contains("no prompt yet"), "{out}");
    }

    /// TC-CLI-SESS-5: a title wider than the terminal, beside a long id.
    /// Expected: the row fits the terminal, the title is what gave way, and
    /// the id is printed whole - it is the one thing on the page a user has
    /// to retype, and half an id is worth nothing.
    #[test]
    fn the_title_gives_way_before_the_id_does() {
        let id = "s".repeat(30);
        let out = shown(vec![session(&id, 1, 2, Some(&"y".repeat(90)))], 60);

        for row in out.lines() {
            assert!(row.chars().count() <= 60, "`{row}` overruns 60");
        }
        assert!(out.contains(&id), "the id was cut:\n{out}");
        assert!(out.contains('…'), "the title was not cut:\n{out}");
    }

    /// TC-CLI-SESS-6: nothing written yet.
    /// Expected: the view says so, and says what writes one. This is the page
    /// a first-time user reaches before they have run anything.
    #[test]
    fn an_empty_list_says_what_writes_one() {
        assert_eq!(
            shown(Vec::new(), 80),
            "\nsessions\nno sessions yet - tetanus run writes one\n"
        );
    }
}
