//! Test Design Specification: the binary runs a real shell command.
//!
//! Feature under test: `tetanus run` with the shell tools registered - the
//! whole path from a prompt, through the turn, into a process, and back into
//! the next request. Features NOT tested here: the seam itself (owned by
//! `tetanus-exec`, asserted in its own suites), the tool pipeline (owned by
//! `tetanus-turn`), and rendering (owned by the presentation lane).
//!
//! Why the binary and not the library: a tool can be correct in a library and
//! unreachable from the program people run, and this lane's acceptance is that
//! `tetanus` runs a real command end to end. The offline adapter treats a
//! prompt opening with `!` as "run the rest", which is what lets that be true
//! with no API key.
//!
//! Environmental needs: a bash on PATH, a writable temp directory. No case
//! reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        .args(args)
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .expect("the binary runs")
}

/// Every record of one kind on a journal.
fn records(journal: &str, kind: &str) -> Vec<serde_json::Value> {
    journal
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["type"] == kind)
        .map(|event| event["data"].clone())
        .collect()
}

/// TC-CLI-SHELL-1: one real command, end to end, through the binary.
///
/// Expected: exit 0; the command really ran, so the file it was asked to write
/// is on disk; the journal records a `tool/call` naming `shell` and a
/// successful `tool/result` holding what the command printed; and the turn's
/// answer quotes that output, which is only possible if the result reached the
/// second request.
#[test]
fn the_binary_runs_a_real_command_end_to_end() {
    let dir = tempfile::tempdir().expect("temp dir");

    let out = run(
        dir.path(),
        &[
            "run",
            "--prompt",
            "!echo printed-by-a-real-shell > witness.txt; cat witness.txt",
            "--session",
            "j.jsonl",
            "--color",
            "never",
        ],
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("witness.txt")).expect("the command wrote it"),
        "printed-by-a-real-shell\n"
    );

    let journal = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");
    let calls = records(&journal, "tool/call");
    assert_eq!(calls.len(), 1, "one command, one call:\n{journal}");
    assert_eq!(calls[0]["name"], serde_json::json!("shell"));

    let results = records(&journal, "tool/result");
    assert_eq!(results[0]["ok"], serde_json::json!(true));
    assert_eq!(
        results[0]["content"],
        serde_json::json!("printed-by-a-real-shell\n")
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains("printed-by-a-real-shell"),
        "the answer does not carry the command's output, so it never reached the next request:\n{stdout}"
    );
}

/// TC-CLI-SHELL-2: a command that fails is reported to the model and does not
/// fail the run.
///
/// Expected: exit 0 - the harness worked, the command did not - with the exit
/// marker on the result the journal recorded, and `ok` false.
#[test]
fn a_failing_command_is_reported_and_the_run_still_succeeds() {
    let dir = tempfile::tempdir().expect("temp dir");

    let out = run(
        dir.path(),
        &[
            "run",
            "--prompt",
            "!echo to-stderr 1>&2; exit 5",
            "--session",
            "j.jsonl",
            "--color",
            "never",
        ],
    );

    assert!(
        out.status.success(),
        "a command that failed is not a run that failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let journal = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");
    let results = records(&journal, "tool/result");
    assert_eq!(results[0]["ok"], serde_json::json!(false));
    let content = results[0]["content"].as_str().expect("text");
    assert!(
        content.contains("[stderr]") && content.contains("[exit code: 5]"),
        "the model reads what happened: {content:?}"
    );
}

/// TC-CLI-SHELL-3: the tools page lists the shell tools the binary can call.
///
/// A tool a run can call and the page does not list is a tool nobody can
/// discover; the page is built from the same registry the run boots with, and
/// this is the case that keeps that true for the shell family.
///
/// Expected: `shell` and the four session tools are on the page, each with the
/// arguments a model has to send.
#[test]
fn the_tools_page_lists_the_shell_family() {
    let dir = tempfile::tempdir().expect("temp dir");

    let page =
        String::from_utf8(run(dir.path(), &["tools", "--color", "never"]).stdout).expect("utf-8");

    for tool in [
        "shell",
        "shell_open",
        "shell_run",
        "shell_close",
        "shell_list",
    ] {
        assert!(page.contains(tool), "`{tool}` is not on the page:\n{page}");
    }
    assert!(
        page.contains("session_id"),
        "the session tools advertise the id a model has to pass back:\n{page}"
    );
}
