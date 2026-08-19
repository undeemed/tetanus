//! Where a run's prompt comes from.
//!
//! Contract §4.7 says `tetanus run` is `session.create`, `session.subscribe`
//! and `agent.prompt`, and says nothing about how the text reaches
//! `agent.prompt`. That is argv, and argv is this lane's. So the three sources
//! are decided here, in one function a test can call with plain data, rather
//! than in the middle of a turn where the only way to assert them is to run
//! the binary.
//!
//! # Three sources, one rule
//!
//! - **Typed.** `tetanus run "list the files"`, or the same text after `-p`.
//!   Upstream reads its task the same way - `dsh --profile headless "run the
//!   tests"` - and a prompt is the argument of the command, not a setting of
//!   it, so the positional form is the one the examples show.
//! - **Standard input**, named by `-`. A prompt worth writing down is longer
//!   than a shell quotes comfortably: `tetanus run - < task.md` and a heredoc
//!   both become one turn, with the newlines the file had.
//! - **Neither**, which is [`DEFAULT`]: a first run needs nothing typed.
//!
//! `-` is the convention every tool that reads a file already uses, and it
//! costs no flag. Reading a piped stdin *without* being asked was the
//! alternative, and it is rejected: `tetanus run` inside a script whose stdin
//! is a closed pipe would then silently ask nothing, and inside a terminal
//! pipeline it would hang waiting for a prompt nobody is typing.
//!
//! # An empty prompt is a usage error, not an empty turn
//!
//! `-p ""`, and a `-` whose file turned out to be empty, both stop before the
//! journal is opened. The turn would otherwise run, cost a provider call, and
//! write a journal recording that the agent was asked nothing - which is a
//! mistake to report at the point it was made, with the exit status §4.5 gives
//! a bad argument.

use std::io::Read;

use tetanus_protocol::rpc::{ErrorCode, RpcError};

/// What a bare `tetanus run` asks.
pub const DEFAULT: &str = "run one full turn";

/// The prompt that means "read the prompt from standard input".
pub const STDIN: &str = "-";

/// Resolve what to ask the agent.
///
/// `said` is whichever of the positional prompt and `-p` was given; clap
/// rejects both at once, so there is nothing to prefer here.
pub fn resolve(said: Option<String>, stdin: impl Read) -> Result<String, RpcError> {
    let Some(said) = said else {
        return Ok(DEFAULT.to_string());
    };
    let (prompt, source) = if said == STDIN {
        (read(stdin)?, "standard input was empty")
    } else {
        (said, "it is empty")
    };
    if prompt.trim().is_empty() {
        return Err(unusable(source));
    }
    Ok(prompt)
}

/// Read standard input to its end.
fn read(mut stdin: impl Read) -> Result<String, RpcError> {
    let mut bytes = Vec::new();
    // §4.5 gives `Io` a `path` when a path is at fault. Nothing here has one,
    // so what failed is named in the message instead of invented as data.
    stdin
        .read_to_end(&mut bytes)
        .map_err(|err| RpcError::new(ErrorCode::Io, format!("standard input: {err}")))?;
    let text = String::from_utf8(bytes).map_err(|_| unusable("standard input is not UTF-8"))?;
    // A shell ends every line it pipes, and a heredoc ends its last one. That
    // newline belongs to the plumbing, not to the question.
    Ok(text.trim_end().to_string())
}

/// A prompt this build will not send, named as the argument it came from so
/// `fault::wording` can say which one to fix.
fn unusable(why: &str) -> RpcError {
    RpcError::new(ErrorCode::InvalidParams, why).with_data(serde_json::json!({ "field": "prompt" }))
}

/// Test Design Specification: resolving a run's prompt.
///
/// Features tested: each of the three sources; that `-` reads standard input
/// to its end and keeps the newlines inside it; that the newline the plumbing
/// added is dropped; and that an empty or unreadable prompt is refused with
/// the code whose exit status says "bad argument".
///
/// Features NOT tested here: that clap refuses the positional form and `-p`
/// together, and that nothing is written when a prompt is refused - both are
/// end to end, in `tests/run_offline.rs`. The wording a user reads is owned by
/// `render::fault`.
///
/// Environmental needs: none. Standard input is a slice of bytes.
#[cfg(test)]
mod tests {
    use super::*;

    /// The reader a case gives `resolve` when the prompt did not come from it.
    /// A run that never says `-` must not read standard input at all, so this
    /// fails the test rather than returning nothing.
    struct Untouched;

    impl Read for Untouched {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("standard input was read for a prompt that was typed");
        }
    }

    fn resolved(said: Option<&str>, stdin: &str) -> Result<String, RpcError> {
        resolve(said.map(str::to_string), stdin.as_bytes())
    }

    /// TC-CLI-ASK-1: nothing typed.
    /// Expected: the documented default, and standard input untouched. A bare
    /// `tetanus run` is the first thing anybody types, and it must not block
    /// on a stdin nobody is going to write to.
    #[test]
    fn a_bare_run_asks_the_default() {
        assert_eq!(resolve(None, Untouched).expect("resolves"), DEFAULT);
    }

    /// TC-CLI-ASK-2: a prompt typed on the command line.
    /// Expected: exactly what was typed, standard input untouched. Leading and
    /// trailing spaces are kept: the user quoted them.
    #[test]
    fn a_typed_prompt_is_asked_verbatim() {
        let said = Some("  list the files  ".to_string());
        assert_eq!(
            resolve(said, Untouched).expect("resolves"),
            "  list the files  "
        );
    }

    /// TC-CLI-ASK-3: `-`, with a prompt on standard input.
    /// Expected: the whole of it, the newlines inside it kept and the one at
    /// the end dropped. Keeping the interior newlines is the point of the
    /// source: a prompt worth writing to a file has paragraphs in it.
    #[test]
    fn a_dash_reads_the_prompt_from_standard_input() {
        assert_eq!(
            resolved(Some(STDIN), "first line\n\nlast line\n").expect("resolves"),
            "first line\n\nlast line"
        );
    }

    /// TC-CLI-ASK-4: a prompt with nothing in it, from either source.
    /// Expected: `InvalidParams` naming the `prompt` field, so §4.5 exits 2
    /// and `render::fault` can say which argument to fix. Running the turn
    /// instead spends a provider call on a question nobody asked.
    #[test]
    fn an_empty_prompt_is_refused_before_anything_runs() {
        for (said, stdin) in [(Some(""), ""), (Some("   "), ""), (Some(STDIN), " \n\n")] {
            let refused = resolved(said, stdin).expect_err("refused");
            assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
            assert_eq!(
                refused
                    .data
                    .as_ref()
                    .and_then(|data| data["field"].as_str()),
                Some("prompt"),
                "{refused:?}"
            );
        }
    }

    /// TC-CLI-ASK-5: standard input that is not text.
    /// Expected: refused as a bad argument, not passed through lossily. A
    /// prompt half-converted to replacement characters is a turn that asks
    /// something nobody wrote.
    #[test]
    fn standard_input_that_is_not_utf8_is_refused() {
        let refused = resolve(Some(STDIN.to_string()), &[0xff, 0xfe][..]).expect_err("refused");

        assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
        assert!(refused.message.contains("UTF-8"), "{refused:?}");
    }

    /// TC-CLI-ASK-6: standard input that cannot be read.
    /// Expected: `Io`, carrying what the operating system said. No `path`,
    /// because §4.5 reserves that for a path, and standard input is not one.
    #[test]
    fn standard_input_that_fails_is_reported_as_io() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("the pipe broke"))
            }
        }

        let refused = resolve(Some(STDIN.to_string()), Broken).expect_err("refused");

        assert_eq!(refused.kind(), Some(ErrorCode::Io));
        assert!(refused.message.contains("the pipe broke"), "{refused:?}");
        assert!(refused.data.is_none(), "{refused:?}");
    }
}
