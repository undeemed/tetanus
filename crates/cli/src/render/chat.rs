//! The three things a chat prints that a turn does not: the page it opens on,
//! the marker it waits at, and the card of commands.
//!
//! Everything between them is the turn, drawn by the same `Reader` every other
//! surface draws a turn with, so a conversation on the screen reads like the
//! same conversation replayed tomorrow.
//!
//! # Why the banner says where the journal is, first
//!
//! It is the one fact about a chat that is not on the screen anywhere else,
//! and it is the fact a reader needs to resume the conversation, replay it, or
//! find out later what was said. `tetanus run` prints it when a turn ends,
//! because that is when a run has one; a chat has one from the moment it
//! opens, and it is what the next `tetanus chat -s <path>` is typed from.
//!
//! # Why the marker is one glyph
//!
//! The turn under it labels the question `you`, in the same words a replay
//! will. A prompt that also said `you` would put that word on the screen twice
//! for one question. So the marker says only "type here", and the transcript
//! keeps the labels.

use std::io::{self, Write};

use tetanus_ui::{tame_line, Role, Ui};

/// Width of the label column on the opening page. `journal` is the longest
/// label, and `run` prints its own journal line at this width, so the two land
/// in the same column when a chat and a run are read one after the other.
const LABEL: usize = 7;

/// What a chat opened on.
pub struct Opened<'a> {
    /// The model every turn of this chat will run on.
    pub model: &'a str,
    /// Where the conversation is being written.
    pub journal: &'a str,
    /// Turns already on that journal. Zero for a chat that starts one.
    pub turns: usize,
}

/// The page a chat opens on.
///
/// The two values that came from outside - the model a flag or a config file
/// named, and the path a `-s` gave - are drawn through `tame_line` on the way
/// in. A writer draws what it is handed, so making them safe is this page's
/// job, and both are drawn as one row each: a sequence in either would clear
/// the screen it is opening on, or rename the window, before a single question
/// has been typed.
pub fn banner<W: Write>(ui: &mut Ui<W>, opened: &Opened) -> io::Result<()> {
    let model = ui.paint(Role::Accent, &tame_line(opened.model)).to_string();
    ui.heading(&format!("chat on {model}"))?;
    ui.field("journal", LABEL, &tame_line(opened.journal))?;
    // Only when there are some. A chat that starts a journal has no history to
    // report, and a row saying `0 turns` would read as one that lost some.
    if opened.turns > 0 {
        ui.field("resumed", LABEL, &turns(opened.turns))?;
    }
    let dot = ui.theme().glyph(" · ", " - ");
    ui.field(
        "keys",
        LABEL,
        &format!("/help for the commands{dot}ctrl-d to leave"),
    )
}

/// How much of the conversation a resumed journal is carrying.
fn turns(turns: usize) -> String {
    match turns {
        1 => "1 turn already, and this chat remembers it".into(),
        many => format!("{many} turns already, and this chat remembers them"),
    }
}

/// The marker a chat waits at. No newline: the reader types on this line, and
/// it is flushed because nothing else will flush it before they do.
pub fn prompt<W: Write>(ui: &mut Ui<W>) -> io::Result<()> {
    let glyph = ui.theme().glyph("›", ">").to_string();
    let marker = ui.paint(Role::Accent, &glyph).to_string();
    write!(ui.out(), "\n{marker} ")?;
    ui.flush()
}

/// Every command the chat answers, in the order a reader meets them.
pub fn help<W: Write>(ui: &mut Ui<W>) -> io::Result<()> {
    let rows = [
        ("/help", "this card; `/?` does the same"),
        (
            "/exit",
            "leave the chat; `/quit`, `/q` and ctrl-d do the same",
        ),
        ("//text", "ask `/text`, rather than run it as a command"),
    ];
    let label = rows
        .iter()
        .map(|(command, _)| command.chars().count())
        .max()
        .unwrap_or(0);

    ui.heading("commands")?;
    for (command, said) in rows {
        ui.field(command, label, said)?;
    }
    Ok(())
}

/// Test Design Specification: the page a chat prints around its turns.
///
/// Features tested: that the opening page names the model, the journal and the
/// way out; that it reports a resumed journal's turns and says nothing at all
/// about a new one's; that the marker is one glyph and is not a line; and that
/// the card lists every command the parser answers.
///
/// Features NOT tested here: what a turn draws (owned by `render::timeline`
/// and `render::live`), what a typed line means (owned by `chat`, asserted in
/// its own module), and the colour policy (owned by `tetanus-ui`).
///
/// Environmental needs: none. Each case renders into a buffer.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn rendered(charset: Charset, draw: impl Fn(&mut Ui<Vec<u8>>)) -> String {
        let mut ui = buffered(Theme::new(false, charset), 80);
        draw(&mut ui);
        ui.contents()
    }

    /// TC-CLI-CHAT-PAGE-1: a chat that starts a journal.
    /// Expected: the model in the heading, the journal path, the way out, and
    /// no row about turns. The path is the row a reader copies into the next
    /// `tetanus chat -s`, so it is on the page before the first question, not
    /// after the last answer.
    #[test]
    fn the_opening_page_names_the_model_and_the_journal() {
        let page = rendered(Charset::Unicode, |ui| {
            banner(
                ui,
                &Opened {
                    model: "deepseek-chat",
                    journal: "sessions/chat.jsonl",
                    turns: 0,
                },
            )
            .expect("banner");
        });

        assert_eq!(
            page,
            "\nchat on deepseek-chat\n\
             journal  sessions/chat.jsonl\n\
             keys     /help for the commands · ctrl-d to leave\n"
        );
    }

    /// TC-CLI-CHAT-PAGE-2: a chat that resumes one.
    /// Expected: a row saying how many turns it is carrying, singular when
    /// there is one. It answers the question a resumed chat opens with -
    /// whether this is the conversation the reader meant to continue.
    #[test]
    fn a_resumed_chat_says_what_it_remembers() {
        for (turns, expected) in [(1, "1 turn already"), (4, "4 turns already")] {
            let page = rendered(Charset::Unicode, |ui| {
                banner(
                    ui,
                    &Opened {
                        model: "mock-echo-1",
                        journal: "sessions/chat.jsonl",
                        turns,
                    },
                )
                .expect("banner");
            });
            assert!(page.contains(expected), "{page}");
        }
    }

    /// TC-CLI-CHAT-PAGE-5: the two values the page took from outside.
    /// Expected: what is left of each one is the readable text it held, on the
    /// row it arrived on, and the page is otherwise the page above. A name off
    /// a flag and a path off `-s` are the whole of what a caller puts on this
    /// page, and it is drawn before the reader has typed anything.
    #[test]
    fn a_name_or_a_path_from_outside_is_drawn_not_obeyed() {
        let page = rendered(Charset::Unicode, |ui| {
            banner(
                ui,
                &Opened {
                    model: "mo\u{1b}[2Jck",
                    journal: "se\u{1b}]0;pwned\u{7}ssions/chat.jsonl",
                    turns: 0,
                },
            )
            .expect("banner");
        });

        assert_eq!(
            page,
            "\nchat on mock\n\
             journal  sessions/chat.jsonl\n\
             keys     /help for the commands \u{b7} ctrl-d to leave\n"
        );
    }

    /// TC-CLI-CHAT-PAGE-3: the marker.
    /// Expected: a blank line, the glyph and one space, with no newline after
    /// it, so the reader types on that line. In the ASCII charset it is `>`,
    /// because a terminal that cannot draw `›` draws a replacement glyph in a
    /// column the marker was measured without.
    #[test]
    fn the_marker_leaves_the_cursor_on_its_own_line() {
        assert_eq!(rendered(Charset::Unicode, |ui| prompt(ui).unwrap()), "\n› ");
        assert_eq!(rendered(Charset::Ascii, |ui| prompt(ui).unwrap()), "\n> ");
    }

    /// TC-CLI-CHAT-PAGE-4: the card.
    /// Expected: every command the parser answers is on it, in one column. A
    /// card that listed a command the parser does not have, or missed one it
    /// does, is worse than no card: it is the only place a reader looks.
    #[test]
    fn the_card_lists_every_command() {
        let card = rendered(Charset::Unicode, |ui| help(ui).expect("card"));

        for command in ["/help", "/?", "/exit", "/quit", "/q", "//text"] {
            assert!(card.contains(command), "{command} is missing:\n{card}");
        }
        assert!(card.contains("ctrl-d"), "{card}");
    }
}
