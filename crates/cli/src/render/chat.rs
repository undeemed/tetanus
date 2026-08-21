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

use tetanus_ui::{or_empty, tame_line, truncate, Role, Theme, Ui};

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
/// in, and through `or_empty` if that left nothing of them: a heading ending
/// in `chat on ` says less than a heading that says the name was empty. A writer draws what it is handed, so making them safe is this page's
/// job, and both are drawn as one row each: a sequence in either would clear
/// the screen it is opening on, or rename the window, before a single question
/// has been typed.
pub fn banner<W: Write>(ui: &mut Ui<W>, opened: &Opened) -> io::Result<()> {
    let named = tame_line(opened.model);
    let model = ui.paint(Role::Accent, or_empty(&named)).to_string();
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

/// The marker a chat waits at, painted, with the space the reader types after.
///
/// Returned rather than written, because the row it opens is redrawn on every
/// keystroke by [`tetanus_ui::read`], which runs on a thread of its own and
/// has no writer to paint with.
pub fn marker<W: Write>(ui: &mut Ui<W>) -> String {
    let glyph = ui.theme().glyph("›", ">").to_string();
    format!("{} ", ui.paint(Role::Accent, &glyph))
}

/// The blank row between the turn above and the marker under it.
///
/// It is written through the writer and flushed, because the editor draws the
/// row after it and the two must not arrive out of order.
pub fn space<W: Write>(ui: &mut Ui<W>) -> io::Result<()> {
    ui.blank()?;
    ui.flush()
}

/// Every command the chat answers, in the order a reader meets them.
/// Every command the chat answers, and what each one does.
///
/// One list, read by the page a chat prints and by the card the full-screen
/// view settles onto its transcript. Two lists would be two answers to
/// `/help`, and the one a reader met would depend on which chat they were in.
pub const COMMANDS: [(&str, &str); 5] = [
    ("/help", "this card; `/?` does the same"),
    (
        "/keys",
        "on a screen of its own: every key it answers, editing keys included",
    ),
    (
        "/exit",
        "leave the chat; `/quit`, `/q` and ctrl-d do the same",
    ),
    (
        "/find word",
        "on a screen of its own: mark it, and walk the marks with ctrl-n and ctrl-p; `/find` alone takes the marks off",
    ),
    ("//text", "ask `/text`, rather than run it as a command"),
];

/// The same card, as lines to settle onto a transcript.
///
/// The full-screen view has no writer to hand: its rows are composed and then
/// painted as one frame, so the card is composed the same way. The heading is
/// drawn as a heading rather than written as one, for the same reason.
pub fn card(theme: &Theme, cols: usize) -> Vec<String> {
    let label = COMMANDS
        .iter()
        .map(|(command, _)| command.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = vec![theme.paint(Role::Heading, "commands").to_string()];
    lines.extend(COMMANDS.iter().map(|(command, said)| {
        let pad = " ".repeat(label - command.chars().count() + 2);
        let said = truncate(said, cols.saturating_sub(label + 4), theme.charset());
        format!(
            "  {}{pad}{}",
            theme.paint(Role::Accent, command),
            theme.paint(Role::Muted, &said)
        )
    }));
    lines
}

pub fn help<W: Write>(ui: &mut Ui<W>) -> io::Result<()> {
    let rows = COMMANDS;
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

    /// TC-CLI-CHAT-PAGE-6: a path and a name holding nothing that can be
    /// drawn.
    /// Expected: each row says `(empty)` rather than stopping after its label.
    /// A file whose whole name is an escape sequence is a file a reader can
    /// make, and after `tame_line` there is nothing of it left to print; a row
    /// that went blank there would read as this build having lost the journal
    /// it is writing.
    #[test]
    fn a_row_with_nothing_left_to_draw_says_so() {
        let page = rendered(Charset::Unicode, |ui| {
            banner(
                ui,
                &Opened {
                    model: "\u{1b}[2J",
                    journal: "\u{1b}]0;pwned\u{7}",
                    turns: 0,
                },
            )
            .expect("banner");
        });

        assert_eq!(
            page,
            "\nchat on (empty)\n\
             journal  (empty)\n\
             keys     /help for the commands \u{b7} ctrl-d to leave\n"
        );
    }

    /// TC-CLI-CHAT-PAGE-3: the marker, and the row it sits on.
    /// Expected: the glyph and one space, with nothing around it - the editor
    /// puts it at the start of a row it redraws, so a newline inside it would
    /// be redrawn too. In the ASCII charset it is `>`, because a terminal that
    /// cannot draw `›` draws a replacement glyph in a column the marker was
    /// measured without. The blank row above it is written separately, because
    /// it belongs to the transcript and not to the line being typed.
    #[test]
    fn the_marker_is_a_glyph_and_the_room_to_type_after_it() {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), 80);
        assert_eq!(marker(&mut ui), "› ");
        let mut ui = buffered(Theme::new(false, Charset::Ascii), 80);
        assert_eq!(marker(&mut ui), "> ");

        assert_eq!(rendered(Charset::Unicode, |ui| space(ui).unwrap()), "\n");
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
