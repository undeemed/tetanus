//! `tetanus chat`: one session, many turns, typed one after another.
//!
//! `tetanus run` asks one question and exits. Everything a conversation needs
//! beyond that is already here - model history is derived from the journal, so
//! a second run against the same path continues the same conversation - and
//! what was missing was the loop that keeps typing into it. This module is
//! that loop, and nothing else: it opens the session `run` opens, boots the
//! engine `run` boots, and renders each turn through the view `run` renders.
//!
//! # Why one engine and one journal for the whole chat
//!
//! [`TurnEngine`] numbers the turns of the log it was built on, and derives
//! each request's history from that same log. So a chat is one engine over one
//! journal, and every exchange is an append to it. Nothing is held in memory
//! that the journal does not already hold, which is what makes leaving and
//! resuming the same thing as never leaving: `tetanus chat -s <path>` on a
//! journal that exists reads its turns back as history, and the next turn is
//! numbered after them.
//!
//! # Why the transcript is not reprinted on resume
//!
//! The view reads the journal, and a resumed journal is not empty. Each turn
//! is drawn from the events appended after it started, so a chat that resumes
//! a long session prints the banner and the next turn, not the afternoon
//! before it. The banner says how many turns are already there, and
//! `tetanus replay` is what reads them.
//!
//! # Ctrl-C is a way out, at the prompt and during a turn
//!
//! Once anything in this process has awaited [`crate::interrupt`], tokio owns
//! SIGINT for the rest of the process: the default disposition is gone, and a
//! Ctrl-C nobody is waiting on does nothing at all. A REPL that only waited on
//! it during a turn would therefore stop answering Ctrl-C at the prompt after
//! its first question. So the line reader waits on it too, and the two answers
//! agree: the turn is dropped where it stands, the journal keeps every event
//! it had already written, and the chat exits `130` (contract §4.5).

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::log::topic;
use tetanus_turn::{TurnConfig, TurnEngine};
use tetanus_ui::{tame_line, Policy, Ui};

use crate::render;
use crate::{AdapterChoice, Reported};

#[derive(clap::Args)]
pub struct ChatArgs {
    /// Which model provider to talk to. Defaults to `provider.default` in
    /// the settings document, and to DeepSeek when nothing sets it
    #[arg(short, long, value_enum)]
    pub adapter: Option<AdapterChoice>,
    /// Model id. Defaults to the adapter's first catalog entry.
    #[arg(short, long, value_name = "ID")]
    pub model: Option<String>,
    /// The journal this conversation is kept in. An existing one is resumed.
    /// Defaults to `chat.jsonl` under `sessions.root` in the settings
    /// document
    #[arg(short, long, value_name = "PATH")]
    pub session: Option<PathBuf>,
    /// Step budget for each turn. Defaults to `agent.max_steps` in the
    /// settings document
    #[arg(long, value_name = "N")]
    pub max_steps: Option<u32>,
    /// Print the model's thinking in full, not folded to its first line
    #[arg(long)]
    pub think: bool,
}

/// What a typed line asks for.
///
/// Parsed from plain text in one function, so every case is a unit test rather
/// than a session driven through a pty. A line is a message unless it opens
/// with a slash, and `//` is how a message that opens with one is still sent.
#[derive(Debug, PartialEq)]
pub enum Input<'a> {
    /// Nothing was typed. The prompt comes back, and no turn is spent.
    Blank,
    /// Ask the model this.
    Ask(&'a str),
    /// Leave the chat.
    Leave,
    /// Print the commands.
    Help,
    /// A slash command this build does not have, as it was typed.
    Unknown(&'a str),
}

/// Read one typed line as an [`Input`].
pub fn parse(line: &str) -> Input<'_> {
    let said = line.trim();
    // `//` is the escape, read before the commands: a line that opens with it
    // is a message whose first slash is dropped, so `//exit` asks the model
    // `/exit` instead of leaving. Without it there is a class of question -
    // anything opening with a path - that the chat cannot put at all.
    if let Some(escaped) = said.strip_prefix('/').filter(|rest| rest.starts_with('/')) {
        return match escaped {
            // The escape and nothing else. A lone slash is not a question.
            "/" => Input::Blank,
            asked => Input::Ask(asked),
        };
    }
    match said {
        "" => Input::Blank,
        "/exit" | "/quit" | "/q" => Input::Leave,
        "/help" | "/?" => Input::Help,
        // The command is the first word: `/reset now` is not a message, and
        // saying which command is missing is what tells the user it is not.
        _ => match said.starts_with('/') {
            true => Input::Unknown(said.split_whitespace().next().unwrap_or(said)),
            false => Input::Ask(said),
        },
    }
}

/// One line from the reader, or the way they left instead.
enum Typed {
    Line(String),
    /// End of input: Ctrl-D at a terminal, or a script whose input ran out.
    Eof,
    /// Ctrl-C, wherever it lands.
    Interrupted,
}

/// Hold a conversation on one session journal.
pub async fn chat<W: Write>(
    policy: &Policy,
    document: &std::path::Path,
    out: &mut Ui<W>,
    args: ChatArgs,
) -> Result<(), Reported> {
    // What every turn in this conversation runs on: the settings document,
    // with the flags over it. Before the journal exists, because the document
    // decides where the journal goes when `--session` did not.
    let settled = crate::settings::turn_settings(
        policy,
        document,
        crate::settings::TurnFlags {
            adapter: args.adapter,
            model: args.model.clone(),
            max_steps: args.max_steps,
            session: args.session.clone(),
        },
        // A conversation with the mock adapter is a demonstration rather than
        // a use, so an unconfigured chat is DeepSeek.
        AdapterChoice::Deepseek,
        "chat.jsonl",
    )?;

    // As `run` refuses a prompt it will not send: a chat that cannot reach a
    // model must say so at the point the adapter was named, not after it has
    // written a journal holding no turns.
    let (adapter, model) = crate::adapter(policy, settled.provider, settled.model.clone())?;

    let opened = crate::session(
        settled.settings.clone(),
        &settled.journal,
        settled.provider.route(),
        &model,
        settled.max_steps,
    )
    .await
    .map_err(|err| crate::fail(policy, &crate::about(err, &settled.journal)))?;

    let bus = EventBus::new();
    let log = JsonlSessionLog::create(&opened.session_id, &settled.journal, bus.clone())
        .map_err(|err| crate::fail(policy, &crate::journal_fault(&err, &settled.journal)))?;

    let ctx = boot(bus, adapter, Arc::new(crate::registry()), log.clone())
        .map_err(|err| crate::report(policy, &err.to_string(), None))?;
    let engine = TurnEngine::from_context(
        &ctx,
        TurnConfig {
            model: model.clone(),
            max_steps: settled.max_steps,
            ..TurnConfig::default()
        },
    )
    .map_err(|err| crate::report(policy, &err.to_string(), None))?;

    render::chat::banner(
        out,
        &render::chat::Opened {
            model: &model,
            journal: &log.path().display().to_string(),
            turns: turns_on(log.as_ref()),
        },
    )
    .ok();

    // A prompt marker is for a person at a keyboard. A script piping questions
    // in gets the transcript and nothing else, so its output is the journal
    // it just wrote and not a page with markers through it.
    let typing = std::io::stdin().is_terminal();
    // Painted once, and drawn on every keystroke by the editor that owns the
    // row. It is a string and not a call because the thread that draws it is
    // not the one holding the writer.
    let marker = render::chat::marker(out);
    // The same name as the banner drew, tamed for the line that says it -
    // `tetanus run` words its phase line from a tamed name for the same
    // reason. What was given still selects the adapter; what is drawn is
    // drawn.
    let phase = format!("running the turn on {}", tame_line(&model));

    loop {
        // The blank line that separates the prompt from the turn above it goes
        // through the writer, before the terminal is taken: it is an ordinary
        // row of the transcript, and only the row under it is the editor's.
        if typing {
            render::chat::space(out).ok();
        }
        let asked = match typed(typing, &marker, policy.width).await {
            Ok(Typed::Line(line)) => line,
            // Both ways out land here: the journal is named, and the status
            // says which of them it was.
            Ok(Typed::Eof) => break,
            Ok(Typed::Interrupted) => {
                crate::journal(out, &log);
                return Err(crate::stopped(policy));
            }
            Err(err) => {
                return Err(crate::fail(
                    policy,
                    &tetanus_protocol::rpc::RpcError::new(
                        tetanus_protocol::rpc::ErrorCode::Io,
                        format!("standard input: {err}"),
                    ),
                ))
            }
        };
        match parse(&asked) {
            Input::Blank => continue,
            Input::Leave => break,
            Input::Help => {
                render::chat::help(out).ok();
                continue;
            }
            Input::Unknown(command) => {
                policy
                    .stderr()
                    .note(&format!(
                        "{} is not a command; /help lists them",
                        tame_line(command)
                    ))
                    .ok();
                continue;
            }
            Input::Ask(asked) => {
                // Where this turn starts on the journal. The view draws what
                // is appended from here, so a resumed session's history stays
                // where it is and every turn is drawn once.
                let from = log.events().len();
                let running = engine.run_turn(asked);
                let Some(outcome) =
                    crate::with_live(policy, out, &log, from, &phase, args.think, running).await
                else {
                    crate::journal(out, &log);
                    return Err(crate::stopped(policy));
                };
                // A turn that failed ends the chat with the status §4.5 gives
                // its code, the same as `tetanus run`: the conversation is on
                // the journal, and `tetanus chat -s <path>` picks it up again.
                outcome.map_err(|err| {
                    crate::fail(
                        policy,
                        &crate::turn_fault(
                            &err,
                            &opened.session_id,
                            settled.provider.route(),
                            &settled.journal,
                        ),
                    )
                })?;
                engine
                    .flush()
                    .await
                    .map_err(|err| crate::report(policy, &err.to_string(), None))?;
            }
        }
    }

    crate::journal(out, &log);
    Ok(())
}

/// Turns already on a journal, which is what a resumed chat remembers.
fn turns_on(log: &JsonlSessionLog) -> usize {
    log.events()
        .iter()
        .filter(|event| event.ty == topic::TURN_START)
        .count()
}

/// Wait for one typed line, or for the reader to leave.
///
/// The read is a blocking one on a pool thread, because standard input has no
/// async form that a piped script and a terminal both answer. The wait is the
/// select around it: see this module's header for why Ctrl-C has to be waited
/// on here and not only during a turn.
///
/// The blocking read outlives an interrupt - nothing can cancel a thread
/// parked in `read(2)` - so the caller ends the process rather than dropping
/// the runtime, which would wait for a line nobody is going to type.
async fn typed(typing: bool, marker: &str, width: usize) -> std::io::Result<Typed> {
    if !typing {
        return piped().await;
    }
    let marker = marker.to_string();
    tokio::task::spawn_blocking(move || edited(&marker, width))
        .await
        .map_err(std::io::Error::other)?
}

/// One line from a terminal, with the editing keys a shell has.
///
/// It runs on a blocking thread because that is what it is: a person typing.
/// Raw mode is held for exactly as long as the line takes, and given back
/// before anything else is printed - the transcript above the prompt is the
/// reader's own scrollback, and a turn that drew into a raw terminal would
/// leave every row of it starting where the last one ended.
///
/// Ctrl-C is a keystroke here rather than a signal, because raw mode is what
/// turns the one into the other. It comes back as [`Typed::Interrupted`], which
/// is the same answer the signal gives on the other path, so the loop above
/// does not know which kind of terminal it is talking to.
fn edited(marker: &str, width: usize) -> std::io::Result<Typed> {
    // Armed before the terminal is taken and dropped after it is given back,
    // so there is no moment in which the terminal is raw and nothing would
    // undo it.
    let killed = tetanus_ui::when_killed(tetanus_ui::Typing).ok();
    let mut held = tetanus_ui::Held::take(tetanus_ui::Typing).map_err(|(_, err)| err)?;
    let read = tetanus_ui::read(held.console(), &mut std::io::stdout(), marker, width);
    let given = held.release();
    drop(killed);
    given.and(read).map(|typed| match typed {
        tetanus_ui::Typed::Asked(line) => Typed::Line(line),
        tetanus_ui::Typed::Interrupted => Typed::Interrupted,
        // `read` returns on the three keys that end a line and on nothing
        // else. If a later one ever arrives here, ending the chat is the
        // answer that loses nothing: the journal holds every turn already.
        _ => Typed::Eof,
    })
}

/// One line from something that is not a terminal: a pipe, a here-document, a
/// test. There is nobody to draw a row for, so the line is read as a line and
/// Ctrl-C is a signal again.
async fn piped() -> std::io::Result<Typed> {
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map(|read| (read, line))
    });
    tokio::select! {
        read = line => match read {
            Ok(Ok((0, _))) => Ok(Typed::Eof),
            Ok(Ok((_, line))) => Ok(Typed::Line(line)),
            Ok(Err(err)) => Err(err),
            Err(joined) => Err(std::io::Error::other(joined)),
        },
        _ = crate::interrupt() => Ok(Typed::Interrupted),
    }
}

/// Test Design Specification: what a typed line means.
///
/// Features tested: every command the chat answers and each spelling of it, a
/// message, the escape that sends a message opening with a slash, what an
/// empty line does, and a command this build does not have.
///
/// Features NOT tested here: the loop itself and every way out of it (end to
/// end, in `tests/chat.rs`, where a real binary reads a real pipe), and the
/// wording of the page (owned by `render::chat`).
///
/// Environmental needs: none. Each case is a string in and a value out.
#[cfg(test)]
mod tests {
    use super::*;

    /// TC-CLI-CHAT-IN-1: an ordinary message.
    /// Expected: asked as typed, with the newline the terminal added and any
    /// spaces around it taken off. A line typed at a prompt is not quoted, so
    /// the whitespace around it is a slip and never part of the question.
    #[test]
    fn a_typed_line_is_the_question() {
        assert_eq!(
            parse("what did I just say?\n"),
            Input::Ask("what did I just say?")
        );
        assert_eq!(parse("  spaced out  \n"), Input::Ask("spaced out"));
    }

    /// TC-CLI-CHAT-IN-2: nothing typed.
    /// Expected: blank, so Enter on an empty prompt brings the prompt back
    /// rather than spending a provider call on a question nobody asked - the
    /// rule `prompt::resolve` already holds `tetanus run` to.
    #[test]
    fn an_empty_line_asks_nothing() {
        for line in ["\n", "", "   \n", "\t\n"] {
            assert_eq!(parse(line), Input::Blank, "{line:?}");
        }
    }

    /// TC-CLI-CHAT-IN-3: leaving.
    /// Expected: every spelling of it leaves. `/q` is there because it is what
    /// a reader who has just used the journal viewer will type.
    #[test]
    fn the_leaving_commands_all_leave() {
        for line in ["/exit\n", "/quit\n", "/q\n", "  /exit  \n"] {
            assert_eq!(parse(line), Input::Leave, "{line:?}");
        }
    }

    /// TC-CLI-CHAT-IN-4: asking what the commands are.
    /// Expected: the card, under either spelling. `?` is the key every
    /// full-screen view in this binary already answers with its key map.
    #[test]
    fn help_is_asked_for_two_ways() {
        for line in ["/help\n", "/?\n"] {
            assert_eq!(parse(line), Input::Help, "{line:?}");
        }
    }

    /// TC-CLI-CHAT-IN-5: a message that opens with a slash.
    /// Expected: the first slash is dropped and the rest is asked as typed, so
    /// `//exit` is a question and not the way out. Without the escape there is
    /// a class of question - anything opening with a path - that the chat
    /// cannot put at all.
    #[test]
    fn a_double_slash_asks_what_would_have_been_a_command() {
        assert_eq!(parse("//exit\n"), Input::Ask("/exit"));
        assert_eq!(
            parse("//usr/bin holds what?\n"),
            Input::Ask("/usr/bin holds what?")
        );
        // The escape with nothing after it is one slash, which is not a
        // question worth a turn.
        assert_eq!(parse("//\n"), Input::Blank);
    }

    /// TC-CLI-CHAT-IN-6: a command this build does not have.
    /// Expected: named as the first word, and not sent to the model. Sending
    /// it would answer a typo with a paid-for turn, and the reply would be the
    /// model guessing at what the command was meant to do.
    #[test]
    fn an_unknown_command_is_not_a_question() {
        assert_eq!(parse("/reset\n"), Input::Unknown("/reset"));
        assert_eq!(parse("/model deepseek-chat\n"), Input::Unknown("/model"));
    }
}
