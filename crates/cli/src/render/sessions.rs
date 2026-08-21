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
//! # Why a narrow window stacks the row instead
//!
//! An id long enough to leave no room for a title leaves no room for the
//! counters either, and a printed row wider than the window is folded by the
//! terminal at column zero, where the second half reads as another session.
//! So a window that cannot hold the table gets the id on a line of its own
//! and the rest indented under it: the same bargain the help page's examples
//! make, and for the same reason - two lines that say which is which beat one
//! line folded into nonsense.
//!
//! The picker does not stack. Its rows are a cursor's rows, one session each,
//! and its frame cuts what overruns rather than folding it; a row that became
//! two there would be a cursor that selects the journal above the one it is
//! pointing at.
//!
//! # Why the newest is first
//!
//! `session.list` answers in the engine's own order, which is by id. That is
//! the right order for a machine, and the wrong one for a person: the session
//! you want is almost always the one you just wrote. So this view sorts by
//! creation time, newest first, and leaves the call's own order to `--json`,
//! which prints the result verbatim.
//!
//! # Why the root is on the heading
//!
//! Which directory was listed is a fact about the answer, not about any row
//! in it: `--dir`, the settings document and the harness home can each name a
//! different one, and an empty page under the wrong root reads exactly like
//! an empty page under the right one. So the root is drawn beside the
//! heading, on the full page and the empty one alike.
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
use tetanus_ui::{tame, truncate, visible_width, Role, Theme, Ui};

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
    // An id is a file's name and a state is a word the engine chose, so both
    // are tamed once and then measured, drawn and padded as what they became.
    let ids: Vec<String> = sessions.iter().map(|row| tame(&row.session_id)).collect();
    let states: Vec<String> = sessions.iter().map(|row| named_state(row)).collect();
    let id = width(ids.iter().map(|cell| visible_width(cell)));
    let digits = width(sessions.iter().map(|row| count(row).to_string().len()));
    let size = width(
        sessions
            .iter()
            .map(|row| visible_width(&measure(row, digits))),
    );
    let state = width(states.iter().map(|cell| visible_width(cell)));
    let room = cols.saturating_sub(id + size + state + GAP * 3).max(1);
    let gap = " ".repeat(GAP);

    sessions
        .iter()
        .zip(&ids)
        .zip(&states)
        .map(|((row, named), word)| {
            let taken = measure(row, digits);
            format!(
                "{}{}{gap}{}{}{gap}{}{}{gap}{}",
                theme.paint(Role::Accent, named),
                pad(named, id),
                theme.paint(Role::Muted, &taken),
                pad(&taken, size),
                theme.paint(role(row), word),
                pad(word, state),
                title(theme, row, room, charset),
            )
        })
        .collect()
}

/// The spaces that carry a cell out to `width` columns.
///
/// A format width would count characters instead, so an id or a state in a
/// script a terminal draws twice as wide would push every column after it.
fn pad(cell: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(visible_width(cell)))
}

/// Print the list under a heading that names the root it was read from.
pub fn render<W: Write>(ui: &mut Ui<W>, list: &SessionListResult, root: &str) -> io::Result<()> {
    ui.heading_at("sessions", root)?;
    if list.sessions.is_empty() {
        let empty = ui
            .paint(Role::Muted, "no sessions yet - tetanus run writes one")
            .to_string();
        return ui.line(&empty);
    }

    let composed = page(ui.theme(), ui.width(), &ordered(list));
    for row in composed {
        ui.line(&row)?;
    }
    Ok(())
}

/// The list as a page prints it: the table where the window holds it, and the
/// stacked form where it does not.
///
/// Only the printed page chooses. The picker composes [`rows`] itself, and
/// wants one row per session whatever the width.
pub fn page(theme: &Theme, cols: usize, sessions: &[&SessionInfo]) -> Vec<String> {
    match beside(cols, sessions) < LEAST {
        true => stacked(theme, cols, sessions),
        false => rows(theme, cols, sessions),
    }
}

/// The columns a title would have beside the fixed ones.
fn beside(cols: usize, sessions: &[&SessionInfo]) -> usize {
    let ids = sessions
        .iter()
        .map(|row| visible_width(&tame(&row.session_id)));
    let digits = width(sessions.iter().map(|row| count(row).to_string().len()));
    let size = width(
        sessions
            .iter()
            .map(|row| visible_width(&measure(row, digits))),
    );
    let state = width(sessions.iter().map(|row| visible_width(&named_state(row))));
    cols.saturating_sub(width(ids) + size + state + GAP * 3)
}

/// Two lines per session: the id whole, and everything else under it.
fn stacked(theme: &Theme, cols: usize, sessions: &[&SessionInfo]) -> Vec<String> {
    let charset = theme.charset();
    let digits = width(sessions.iter().map(|row| count(row).to_string().len()));
    let room = cols.saturating_sub(STACK).max(1);
    let gap = " ".repeat(GAP);

    sessions
        .iter()
        .flat_map(|row| {
            let taken = measure(row, digits);
            let named = tame(&row.session_id);
            [
                theme.paint(Role::Accent, &named).to_string(),
                format!(
                    "{}{}{gap}{}{gap}{}",
                    " ".repeat(STACK),
                    theme.paint(Role::Muted, &taken),
                    theme.paint(role(row), &named_state(row)),
                    title(theme, row, room, charset),
                ),
            ]
        })
        .collect()
}

/// The narrowest title worth a column of its own. Under this the page stacks:
/// a title cut to two characters says nothing a reader can use, and the row it
/// is on has already overrun the window.
const LEAST: usize = 12;

/// How far the second line of a stacked row is indented. Deep enough to tell
/// the two lines apart, and the same indent the help page's examples use when
/// they stack.
const STACK: usize = 6;

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
fn named_state(row: &SessionInfo) -> String {
    match &row.state {
        AgentState::Idle => "idle".to_string(),
        AgentState::Running => "running".to_string(),
        AgentState::Other(other) => tame(other),
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

/// The widest of a set of cells, in the columns a terminal draws them in.
fn width(cells: impl Iterator<Item = usize>) -> usize {
    cells.max().unwrap_or(0)
}

/// Test Design Specification: the session list.
///
/// Features tested: the four columns and their alignment; that the newest
/// session is first and a tie is still settled; that an empty journal reads
/// `0 events` and a one-event journal is singular; that a session with no
/// prompt yet says so; that a title too long for the terminal is cut while
/// the id is not, and that a window too narrow for the table stacks the row
/// instead while the picker's rows stay one per session; the empty list, which
/// carries the root like the full page does; an id and a state that carry
/// escape sequences; and an id a terminal draws twice as wide.
///
/// Features NOT tested here: which sessions this build finds and which root it
/// looked in (owned by `tetanus-engine` and the binary, and asserted end to
/// end in `tests/presentation.rs`), the JSON form (owned by `render::json`),
/// and the colour policy (owned by `tetanus-ui`).
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
        under(sessions, width, "/srv/j")
    }

    fn under(sessions: Vec<SessionInfo>, width: usize, root: &str) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        render(&mut ui, &SessionListResult { sessions }, root).expect("render");
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
            "\nsessions  /srv/j\n\
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

    /// TC-CLI-SESS-9: an id long enough to leave a title no room.
    /// Expected: the page stacks - the id whole on a line of its own, the
    /// counters, the state and the title indented under it - and no line
    /// overruns the window. Printed as a table, that row is folded by the
    /// terminal at column zero, and the half that lands there reads as
    /// another session.
    #[test]
    fn a_window_too_narrow_for_the_table_stacks_the_row() {
        let id = "s".repeat(40);
        let out = shown(vec![session(&id, 1, 3, Some("fix the parser"))], 50);
        let lines: Vec<&str> = out.lines().collect();

        for row in &lines {
            assert!(visible_width(row) <= 50, "`{row}` overruns 50");
        }
        assert!(
            lines.iter().any(|row| row.trim() == id),
            "the id is not on a line of its own:\n{out}"
        );
        let under = lines
            .iter()
            .find(|row| row.contains("fix the parser"))
            .unwrap_or_else(|| panic!("the title is on no line:\n{out}"));
        assert!(under.starts_with("      "), "not indented: `{under}`");
        assert!(under.contains("4 events"), "no counters: `{under}`");
        assert!(under.contains("idle"), "no state: `{under}`");
    }

    /// TC-CLI-SESS-10: the same list in a window that holds the table.
    /// Expected: one line per session, the columns as TC-CLI-SESS-1 has them.
    /// The stacked form is for the window that cannot hold a table, and a
    /// reader who widened their terminal gets their columns back.
    #[test]
    fn a_window_that_holds_the_table_still_gets_one() {
        let out = shown(
            vec![
                session("first", 2, 3, Some("fix the parser")),
                session("second", 1, 1, Some("read the log")),
            ],
            100,
        );

        let rows: Vec<&str> = out
            .lines()
            .filter(|row| row.contains("events") || row.contains("event "))
            .collect();
        assert_eq!(rows.len(), 2, "not one line per session:\n{out}");
        assert!(
            rows[0].starts_with("first"),
            "not a table row: `{}`",
            rows[0]
        );
    }

    /// TC-CLI-SESS-11: what the picker composes, at the width that stacks.
    /// Expected: still one row per session. The picker moves a cursor down
    /// these rows and opens the journal under it, so a row that became two
    /// would be a cursor pointing at one journal and opening another; its
    /// frame cuts what overruns instead of folding it.
    #[test]
    fn the_picker_gets_one_row_per_session_at_any_width() {
        let list = SessionListResult {
            sessions: vec![
                session(&"s".repeat(40), 2, 3, Some("fix the parser")),
                session(&"t".repeat(40), 1, 1, Some("read the log")),
            ],
        };
        let composed = rows(&Theme::new(false, Charset::Unicode), 50, &ordered(&list));

        assert_eq!(composed.len(), 2, "the picker's rows: {composed:?}");
    }

    /// TC-CLI-SESS-7: an id and a state that carry escape sequences.
    /// Expected: no sequence reaches the page and both are still read. An id
    /// is a file's name, which a user chose with `--session`, and a state
    /// this build does not know is a word the engine chose (contract §2).
    #[test]
    fn an_id_and_a_state_off_the_wire_are_drawn_and_not_obeyed() {
        let clear = "\u{1b}[2J";
        let out = shown(
            vec![SessionInfo {
                state: AgentState::Other(format!("re{clear}trying")),
                ..session(&format!("s{clear}1"), 1, 2, Some("hi"))
            }],
            80,
        );

        assert!(!out.contains('\u{1b}'), "{out:?}");
        assert!(out.contains("s1"), "{out}");
        assert!(out.contains("retrying"), "{out}");
    }

    /// TC-CLI-SESS-8: an id in a script a terminal draws twice as wide.
    /// Expected: both titles start at the same column. Every column on this
    /// row is padded in what the terminal draws, so one wide id does not push
    /// the three columns after it out of place on that row alone.
    #[test]
    fn every_cell_is_padded_in_the_columns_it_draws() {
        let out = shown(
            vec![
                session("\u{65e5}\u{672c}\u{8a9e}", 2, 2, Some("wide")),
                session("session-2", 1, 2, Some("plain")),
            ],
            80,
        );

        let starts: Vec<usize> = ["wide", "plain"]
            .iter()
            .map(|title| {
                let line = out.lines().find(|line| line.contains(title)).expect(title);
                tetanus_ui::visible_width(&line[..line.find(title).expect(title)])
            })
            .collect();
        assert_eq!(
            starts[0], starts[1],
            "the titles are not in one column:\n{out}"
        );
    }

    /// TC-CLI-SESS-6: nothing written yet.
    /// Expected: the view says so, says what writes one, and names the root it
    /// looked in. This is the page a first-time user reaches before they have
    /// run anything, and the one page whose whole content is a claim about a
    /// directory - a reader who cannot see which directory cannot tell an
    /// empty root from the wrong root.
    #[test]
    fn an_empty_list_says_what_writes_one() {
        assert_eq!(
            under(Vec::new(), 80, "/srv/j"),
            "\nsessions  /srv/j\nno sessions yet - tetanus run writes one\n"
        );
    }
}
