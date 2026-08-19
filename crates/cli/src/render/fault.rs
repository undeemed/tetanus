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
//! # Why the match is exhaustive
//!
//! Every code is spelled out, with no catch-all arm. A code added to the
//! contract stops this crate from compiling until someone decides how it
//! reads, which is the cheapest moment to decide it.

use tetanus_protocol::rpc::{ErrorCode, RpcError};

/// The sentence to print, and the way out when there is one.
pub fn wording(error: &RpcError) -> (String, Option<String>) {
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
                Some(path) => format!("{path}: {}", error.message),
                None => error.message.clone(),
            },
            None,
        ),
        ErrorCode::SessionNotFound => (
            match field(error, "session_id") {
                Some(id) => format!("no session {id}"),
                None => error.message.clone(),
            },
            Some("name the journal by path to open it again".into()),
        ),
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
/// blank; and that an unknown code is reported raw and exits 1.
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
}
