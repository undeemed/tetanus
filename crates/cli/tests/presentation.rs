//! Test Design Specification: the binary's colour policy, progress line and
//! diagnostics.
//!
//! Features tested: that the resolved palette reaches clap's own rendering as
//! well as the lines this crate writes itself; that a colour-hostile
//! environment cannot change the bytes of plain output; the shape and exit
//! status of a reported failure; that a bad `--color` value is a usage error;
//! and which of the two views - the turn, or the raw sequence - a command
//! prints. NOT tested here: the resolution rules themselves (owned by
//! `tetanus-ui`'s `color_policy.rs`) and the turn flow (owned by the
//! conformance suite in `tetanus-turn`).
//!
//! Environmental needs: none. Every case runs offline, and every case states
//! the colour-related variables it depends on rather than inheriting them.

use std::path::Path;
use std::process::{Command, Output};

const ESC: &str = "\u{1b}";

/// Run the binary with the colour environment stated, never inherited.
fn run(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir).args(args);
    for name in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "DEEPSEEK_API_KEY"] {
        cmd.env_remove(name);
    }
    cmd.env("TERM", "xterm-256color").env("COLUMNS", "100");
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().expect("the binary runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("utf-8")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("utf-8")
}

/// TC-CLI-UI-1: `tetanus --help` into a pipe.
/// Expected: exit 0, a page that names its usage and its `--color` values, and
/// not one escape byte, because a pipe is not a terminal.
#[test]
fn a_piped_help_page_is_plain() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["--help"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    let help = stdout(&out);
    for expected in [
        "Usage: tetanus",
        "Commands:",
        "Options:",
        "--color <WHEN>",
        "[possible values: auto, always, never]",
    ] {
        assert!(
            help.contains(expected),
            "`{expected}` missing from:\n{help}"
        );
    }
    assert!(!help.contains(ESC), "a pipe got escape codes:\n{help:?}");
}

/// TC-CLI-UI-2: `--color always --help` into a pipe.
/// Expected: clap's own help rendering is coloured too, so one policy governs
/// the whole surface and not only the parts this crate writes itself.
#[test]
fn forcing_colour_reaches_claps_own_rendering() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["--color", "always", "--help"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    let help = stdout(&out);
    assert!(help.contains(ESC), "no styling was emitted:\n{help:?}");
    // clap styles its own headers, so `Usage:` and the binary name are no
    // longer adjacent bytes; the flag literal survives as one run of text.
    assert!(help.contains("--color"), "{help}");
}

/// TC-CLI-UI-3: the environment overrides, on a real turn.
/// Expected: `CLICOLOR_FORCE` colours a piped run, `NO_COLOR` keeps it plain,
/// and `--color never` beats `CLICOLOR_FORCE`.
#[test]
fn the_environment_decides_a_piped_run() {
    let dir = tempfile::tempdir().expect("temp dir");
    let args = &["run", "--session", "journal.jsonl"];

    let forced = stdout(&run(dir.path(), args, &[("CLICOLOR_FORCE", "1")]));
    assert!(forced.contains(ESC), "CLICOLOR_FORCE was ignored");

    let refused = stdout(&run(dir.path(), args, &[("NO_COLOR", "1")]));
    assert!(!refused.contains(ESC), "NO_COLOR was ignored:\n{refused:?}");

    let flagged = stdout(&run(
        dir.path(),
        &["run", "--color", "never", "--session", "journal.jsonl"],
        &[("CLICOLOR_FORCE", "1")],
    ));
    assert!(!flagged.contains(ESC), "the flag lost to the environment");
}

/// TC-CLI-UI-4: plain output is one string, however it was asked for.
/// Expected: the bytes of a piped run are identical whether colour was
/// declined by the flag, by `NO_COLOR`, or by the pipe itself. Colour is never
/// "the same text with the codes taken out"; it is a separate rendering.
#[test]
fn plain_output_is_byte_identical_however_it_was_declined() {
    // One journal per invocation, in a directory of its own: a run's sequence
    // numbers come from the log it appends to, and the journal name it echoes
    // has to stay the same for the comparison to mean anything.
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let args = &["run", "-p", "same in, same out", "--session", "j.jsonl"];

    let by_pipe = stdout(&run(dirs[0].path(), args, &[]));
    let by_env = stdout(&run(dirs[1].path(), args, &[("NO_COLOR", "1")]));
    let by_flag = stdout(&run(
        dirs[2].path(),
        &[
            "run",
            "--color",
            "never",
            "-p",
            "same in, same out",
            "--session",
            "j.jsonl",
        ],
        &[],
    ));

    assert_eq!(by_pipe, by_env);
    assert_eq!(by_pipe, by_flag);
    assert!(by_pipe.contains("You said: same in, same out"), "{by_pipe}");
}

/// TC-CLI-UI-5: a real provider with no credential.
/// Expected: the failure is tagged `error:` and followed by a `note:` naming
/// the way out; both land on stderr; stdout stays empty; the exit is non-zero.
#[test]
fn a_reported_failure_names_its_way_out() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "--adapter", "deepseek", "--session", "never.jsonl"],
        &[],
    );

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "", "a failure wrote to stdout");
    let err = stderr(&out);
    assert!(
        err.starts_with("error: DEEPSEEK_API_KEY is not set"),
        "{err}"
    );
    assert!(err.contains("note: run with `--adapter mock`"), "{err}");
    assert!(!dir.path().join("never.jsonl").exists());
}

/// TC-CLI-UI-6: an unaccepted `--color` value.
/// Expected: a usage error naming the three accepted values, on stderr, with
/// clap's usage exit status of 2 - not a turn that silently ran uncoloured.
#[test]
fn an_unknown_colour_value_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["--color", "rainbow", "info"], &[]);

    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("rainbow"), "{err}");
    for value in ["auto", "always", "never"] {
        assert!(err.contains(value), "`{value}` missing from:\n{err}");
    }
}

/// TC-CLI-UI-7: the progress line on a piped run.
/// Expected: the phase reaches stderr and never stdout, so the byte-identical
/// invariant above still holds; and a pipe gets a whole line, with no spinner
/// frame and no carriage return.
#[test]
fn progress_stays_on_stderr_and_stays_plain_in_a_pipe() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["run", "--session", "j.jsonl"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("running the turn on"), "no progress: {err:?}");
    assert!(!err.contains('\r'), "a pipe got repainted frames: {err:?}");
    assert!(!err.contains(ESC), "a pipe got escape codes: {err:?}");
    assert!(
        !stdout(&out).contains("running the turn"),
        "progress leaked onto stdout"
    );
}

/// TC-CLI-UI-8: `tetanus replay` on a journal a run just wrote.
/// Expected: the timeline, not the raw event dump - the prompt under `you`,
/// the answer under `ai`, and the closing line, which reports what the two
/// steps of the mock turn were billed. `--raw` still gives the dump, so
/// nothing that scripted against it is broken.
#[test]
fn replay_reads_as_a_conversation() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    let told = stdout(&run(dir.path(), &["replay", "j.jsonl"], &[]));
    assert!(
        told.starts_with("\nturn 1\n  step 1\n  you   echo this\n"),
        "{told}"
    );
    assert!(told.contains("  ai    You said: echo this\n"), "{told}");
    assert!(
        told.ends_with("turn 1 \u{b7} natural \u{b7} 2 steps \u{b7} 57 tokens\n"),
        "{told}"
    );

    let raw = stdout(&run(dir.path(), &["replay", "j.jsonl", "--raw"], &[]));
    assert!(raw.starts_with("   0  turn/start"), "{raw}");
}

/// TC-CLI-UI-9: what `tetanus run` prints by default, and what `--trace` adds.
/// Expected: the default is the turn as a conversation, committed line by line
/// as the journal receives it; `--trace` replaces it with the raw sequence and
/// still ends on the answer. Both close on the journal path, so a user always
/// knows where the durable record went.
#[test]
fn a_run_reads_as_a_conversation_unless_a_trace_is_asked_for() {
    let dir = tempfile::tempdir().expect("temp dir");
    let args = &["run", "-p", "echo this", "--session", "j.jsonl"];

    let told = stdout(&run(dir.path(), args, &[]));
    assert!(
        told.contains("\nturn 1\n  step 1\n  you   echo this\n"),
        "{told}"
    );
    assert!(told.contains("  ai    You said: echo this\n"), "{told}");
    assert!(
        !told.contains("turn/start"),
        "the raw topics leaked:\n{told}"
    );
    assert!(told.ends_with("journal  j.jsonl\n"), "{told}");

    let dir = tempfile::tempdir().expect("temp dir");
    let traced = stdout(&run(
        dir.path(),
        &["run", "--trace", "-p", "echo this"],
        &[],
    ));
    assert!(traced.starts_with("   0     0  turn/start"), "{traced}");
    assert!(traced.contains("You said: echo this\n"), "{traced}");
}

/// TC-CLI-UI-10: `tetanus config` end to end.
/// Expected: one row per resolved key, carrying the value without its JSON
/// quotes and the layer that settled it. The provenance column is the reason
/// the command exists, so a build that printed the values alone has failed
/// even though it printed something.
#[test]
fn config_shows_what_set_each_key() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["config", "--color", "never"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "\nconfig\nlog.level  info  default\n");
}

/// TC-CLI-UI-11: `tetanus replay --live` into a pipe.
/// Expected: exactly the bytes of a plain `tetanus replay`, and no waiting.
/// `--live` is a way of watching a turn arrive, not a second wording of it,
/// so a pipe - which has no one watching - gets the same output at once.
#[test]
fn a_piped_playback_is_the_plain_replay() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    let printed = stdout(&run(dir.path(), &["replay", "j.jsonl"], &[]));
    let played = stdout(&run(dir.path(), &["replay", "j.jsonl", "--live"], &[]));

    assert_eq!(played, printed);
    assert!(
        !played.contains(ESC),
        "a pipe got escape codes:\n{played:?}"
    );
    assert!(played.contains("  ai    You said: echo this\n"), "{played}");
}

/// TC-CLI-UI-12: the two ways of asking for a playback that cannot happen.
/// Expected: both are usage errors with clap's exit status of 2 - a raw dump
/// has no block to animate, and a speed governs nothing without a playback.
/// Neither may quietly print something else instead.
#[test]
fn a_playback_that_cannot_happen_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    let clash = run(dir.path(), &["replay", "j.jsonl", "--live", "--raw"], &[]);
    assert_eq!(clash.status.code(), Some(2), "{}", stdout(&clash));
    assert!(stderr(&clash).contains("--raw"), "{}", stderr(&clash));

    let orphan = run(dir.path(), &["replay", "j.jsonl", "--speed", "4"], &[]);
    assert_eq!(orphan.status.code(), Some(2), "{}", stdout(&orphan));
    assert!(stderr(&orphan).contains("--live"), "{}", stderr(&orphan));

    let zero = run(
        dir.path(),
        &["replay", "j.jsonl", "--live", "--speed", "0"],
        &[],
    );
    assert_eq!(zero.status.code(), Some(2), "{}", stdout(&zero));
    assert!(
        stderr(&zero).contains("greater than zero"),
        "{}",
        stderr(&zero)
    );
}
