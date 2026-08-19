//! The journal as the file spells it: one line in, one row out.
//!
//! `tetanus replay --raw` is the view for a journal the cooked view will not
//! read. The timeline is assembled from events that follow each other, so one
//! line out of place stops it; this view assembles nothing, and therefore has
//! nothing to be stopped by.
//!
//! # Why this view parses its own lines
//!
//! A journal read as a log is a sequence, and a sequence with a hole in it is
//! not the log that produced it - refusing it is the right answer, and
//! `tetanus_session::replay` gives it. This view is not reading a log. It is
//! showing a file, so it takes each line on its own and reports the ones it
//! cannot read rather than refusing the file they are in.
//!
//! [`parse`] is here and not in the caller for that reason: the fallback is
//! part of what the view shows, not part of how the file is opened. The line
//! it does parse is the contract's own [`SessionEvent`], so the columns below
//! carry no second copy of the journal's field names.

use std::io::{self, Write};

use tetanus_protocol::types::SessionEvent;
use tetanus_ui::{Role, Ui};

/// Width of the `seq` column, wide enough for a journal no one will scroll.
const SEQ: usize = 4;

/// Width of the topic column. A longer topic pushes its data right rather
/// than being cut: this view never hides what the file says.
const TOPIC: usize = 20;

/// One line of a journal, as this view sees it.
pub enum Line {
    /// A line that reads as an event, whatever the rest of the file does.
    Event(SessionEvent),
    /// A line that does not. The file is the record, so the view still shows
    /// it, verbatim, under the number a repair has to find it by.
    Unreadable { number: usize, text: String },
}

/// Read one line of a journal without judging the log it belongs to.
///
/// `number` is the line's position in the file, counted from 1, which is the
/// number `render::fault` reports and the number an editor jumps to.
pub fn parse(number: usize, text: &str) -> Line {
    match serde_json::from_str::<SessionEvent>(text) {
        Ok(event) => Line::Event(event),
        Err(_) => Line::Unreadable {
            number,
            text: text.to_string(),
        },
    }
}

/// The first line this view could not read, if there was one. The caller
/// turns it into the contract's `LogCorrupt`, so a script still learns the
/// file is broken even though the view printed it.
pub fn unreadable(lines: &[Line]) -> Option<usize> {
    lines.iter().find_map(|line| match line {
        Line::Unreadable { number, .. } => Some(*number),
        Line::Event(_) => None,
    })
}

/// Print every line, readable or not, in the order the file holds them.
pub fn render<W: Write>(ui: &mut Ui<W>, lines: &[Line]) -> io::Result<()> {
    if lines.is_empty() {
        let empty = ui.paint(Role::Muted, "the journal is empty").to_string();
        return ui.line(&empty);
    }
    for line in lines {
        let row = match line {
            Line::Event(event) => format!(
                "{:>SEQ$}  {:<TOPIC$} {}",
                ui.paint(Role::Seq, &event.seq.to_string()),
                ui.paint(Role::Topic, &event.ty),
                event.data
            ),
            // The number goes beside the text and not in the `seq` column:
            // `seq` is what a line claims about itself, and a line this view
            // could not read claims nothing.
            Line::Unreadable { number, text } => format!(
                "{:>SEQ$}  {:<TOPIC$} {}",
                ui.paint(Role::Error, "?"),
                ui.paint(Role::Error, "unreadable"),
                ui.paint(Role::Muted, &format!("line {number}: {text}"))
            ),
        };
        ui.line(&row)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn shown(lines: &[Line], color: bool) -> String {
        let mut ui = buffered(Theme::new(color, Charset::Ascii), 80);
        render(&mut ui, lines).expect("render");
        ui.contents()
    }

    /// TC-CLI-RAW-1: a line the contract's own type accepts.
    /// Expected: an `Event`, carrying the `seq` and the topic the file states,
    /// with no `sourceEventSeqs` needed - a journal line omits it.
    #[test]
    fn a_journal_line_parses_as_the_contracts_event() {
        let text = r#"{"type":"turn/start","seq":7,"time":1,"data":{"n":1}}"#;
        match parse(1, text) {
            Line::Event(event) => {
                assert_eq!(event.seq, 7);
                assert_eq!(event.ty, "turn/start");
                assert_eq!(event.source_event_seqs, None);
            }
            Line::Unreadable { .. } => panic!("a valid line was refused: {text}"),
        }
    }

    /// TC-CLI-RAW-2: a line that is not JSON at all.
    /// Expected: `Unreadable`, keeping the text and the line number, because
    /// the user has to find that line to repair it.
    #[test]
    fn a_line_that_is_not_an_event_keeps_its_place() {
        match parse(3, "half a line") {
            Line::Unreadable { number, text } => {
                assert_eq!(number, 3);
                assert_eq!(text, "half a line");
            }
            Line::Event(_) => panic!("nonsense parsed as an event"),
        }
    }

    /// TC-CLI-RAW-3: a file with a hole in its sequence.
    /// Expected: every row is printed and none is marked, because `seq` is
    /// the file's claim and this view repeats it. The cooked view is the one
    /// that refuses a log whose sequence does not follow.
    #[test]
    fn a_sequence_with_a_hole_is_still_printed() {
        let lines = vec![
            parse(1, r#"{"type":"turn/start","seq":0,"time":1,"data":{}}"#),
            parse(2, r#"{"type":"turn/end","seq":9,"time":2,"data":{}}"#),
        ];
        assert_eq!(unreadable(&lines), None);

        assert_eq!(
            shown(&lines, false),
            "   0  turn/start           {}\n   9  turn/end             {}\n"
        );
    }

    /// TC-CLI-RAW-4: a readable prefix followed by a line that is not.
    /// Expected: the prefix prints as itself, the bad line prints as itself
    /// under its number, and `unreadable` reports the first one - which is
    /// what turns the page into a non-zero exit.
    #[test]
    fn a_broken_line_is_shown_where_it_is() {
        let lines = vec![
            parse(1, r#"{"type":"turn/start","seq":0,"time":1,"data":{}}"#),
            parse(2, "{oh no"),
        ];
        assert_eq!(unreadable(&lines), Some(2));

        assert_eq!(
            shown(&lines, false),
            "   0  turn/start           {}\n   ?  unreadable           line 2: {oh no\n"
        );
    }

    /// TC-CLI-RAW-6: a file with no lines in it.
    /// Expected: the same sentence the timeline prints. The two views of one
    /// journal must not disagree about whether it is empty.
    #[test]
    fn an_empty_file_says_it_is_empty() {
        assert_eq!(shown(&[], false), "the journal is empty\n");
    }

    /// TC-CLI-RAW-5: the same page, coloured.
    /// Expected: the columns land in the same places. `Painted` pads through
    /// `Display`, so a themed run must not shift a column by the width of an
    /// escape sequence.
    #[test]
    fn colour_does_not_move_a_column() {
        let lines = vec![parse(
            1,
            r#"{"type":"turn/start","seq":0,"time":1,"data":{}}"#,
        )];

        let painted = shown(&lines, true);
        assert!(painted.contains("\u{1b}["), "nothing was painted");
        assert_eq!(strip(&painted), shown(&lines, false));
    }

    /// Drop every escape sequence, so two renderings can be compared as text.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                out.push(c);
                continue;
            }
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        }
        out
    }
}
