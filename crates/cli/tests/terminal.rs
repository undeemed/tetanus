//! Test Design Specification: the binary offers the terminal family.
//!
//! Feature under test: the composition in `crates/cli` - whether the six
//! `terminal_*` tools reach the registry a run boots with, and therefore
//! whether a model talking to this binary can call them. Features NOT tested
//! here: the terminal seam itself (owned by `tetanus-exec`, asserted in its
//! own suites) and the tool pipeline (owned by `tetanus-turn`).
//!
//! Why the binary and not the library: a tool family can be complete in a
//! crate and never registered by the program people run, and nothing inside
//! that crate can notice. The tools page is built from the same registry a run
//! boots with, so a family on the page is a family a model can call.
//!
//! Environmental needs: Linux with `/dev/ptmx` and a bash on PATH; the tools
//! page needs neither, because a schema is not a session. No case reaches a
//! network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::process::Command;

/// TC-CLI-TERM-1: the tools page lists the terminal family, with what a model
/// has to send and what it will read back.
///
/// Expected: the six names are on the page, the send tool advertises the
/// session id, the text and the `submit` flag, and the signal tool advertises
/// the signal it needs. The page abbreviates a description and does not print
/// an enum, so what a marker says and which signals are allowed are asserted
/// against the schemas in `tetanus-exec`'s own suite rather than here.
#[test]
fn the_tools_page_lists_the_terminal_family() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir.path())
        .args(["tools", "--color", "never"])
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .expect("the binary runs");
    let page = String::from_utf8(out.stdout).expect("utf-8");

    for tool in [
        "terminal_open",
        "terminal_send",
        "terminal_read",
        "terminal_signal",
        "terminal_close",
        "terminal_list",
    ] {
        assert!(page.contains(tool), "`{tool}` is not on the page:\n{page}");
    }
    for argument in [
        "session_id (string, required)",
        "text (string, required)",
        "submit (boolean)",
        "signal (string, required)",
    ] {
        assert!(
            page.contains(argument),
            "`{argument}` is not advertised, so no model will send it:\n{page}"
        );
    }
}
