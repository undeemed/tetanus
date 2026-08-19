//! Test Design Specification: the binary's colour policy, progress line and
//! diagnostics.
//!
//! Features tested: that the resolved palette reaches clap's own rendering as
//! well as the lines this crate writes itself; that a colour-hostile
//! environment cannot change the bytes of plain output; the shape and exit
//! status of a reported failure; that a bad `--color` value is a usage error;
//! which of the two views - the turn, or the raw sequence - a command prints;
//! that a model's thinking stays folded until it is asked for; that the
//! journal a run leaves behind says what it ran under; that the session list
//! finds those journals and names each by the id its store answers to; that a
//! journal read full-screen is refused where there is no screen; and
//! the shape of the machine-readable output the interface contract fixes. NOT tested
//! here: the resolution rules themselves (owned by
//! `tetanus-ui`'s `color_policy.rs`), what a full-screen view draws once it
//! has a terminal (owned by `render::browse` and `tetanus_ui::Page`, neither
//! of which needs one), and the turn flow (owned by the
//! conformance suite in `tetanus-turn`).
//!
//! Environmental needs: none. Every case runs offline, and every case states
//! the colour-related variables it depends on rather than inheriting them.

use std::path::Path;
use std::process::{Command, Output};

mod common;

use common::without_duration;

const ESC: &str = "\u{1b}";

/// Run the binary with the colour environment stated, never inherited.
fn run(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir).args(args);
    for name in [
        "NO_COLOR",
        "CLICOLOR",
        "CLICOLOR_FORCE",
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_BASE_URL",
    ] {
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
/// How long each run took is not part of that claim and is dropped before the
/// comparison, because a loaded runner can push one of three otherwise
/// identical runs past the second the closing line starts reporting at.
#[test]
fn plain_output_is_byte_identical_however_it_was_declined() {
    // One journal per invocation, in a directory of its own: a run's sequence
    // numbers come from the log it appends to, and the journal name it echoes
    // has to stay the same for the comparison to mean anything.
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let args = &["run", "-p", "same in, same out", "--session", "j.jsonl"];

    let by_pipe = without_duration(&stdout(&run(dirs[0].path(), args, &[])));
    let by_env = without_duration(&stdout(&run(dirs[1].path(), args, &[("NO_COLOR", "1")])));
    let by_flag = without_duration(&stdout(&run(
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
    )));

    assert_eq!(by_pipe, by_env);
    assert_eq!(by_pipe, by_flag);
    assert!(by_pipe.contains("You said: same in, same out"), "{by_pipe}");
}

/// TC-CLI-UI-5: a real provider with no credential.
/// Expected: the failure is tagged `error:` and followed by a `note:` naming
/// the way out; both land on stderr; stdout stays empty; and the status is 5,
/// which is what contract §4.5 gives `MissingCredential`.
#[test]
fn a_reported_failure_names_its_way_out() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "--adapter", "deepseek", "--session", "never.jsonl"],
        &[],
    );

    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "a failure wrote to stdout");
    let err = stderr(&out);
    assert!(
        err.starts_with("error: DEEPSEEK_API_KEY is not set"),
        "{err}"
    );
    assert!(err.contains("note: "), "{err}");
    assert!(err.contains("`--adapter mock`"), "{err}");
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
/// Expected: the timeline, not the raw event dump - the header naming what
/// the session ran under, the prompt under `you`, the answer under `ai`, and
/// the closing line, which reports what the two steps of the mock turn were
/// billed. `--raw` still gives the dump, so nothing that scripted against it
/// is broken. The journal here carries a real run's timestamps, so the closing
/// line's wall clock is dropped before the line is matched; TC-CLI-TL-13
/// asserts that field against fixed ones.
#[test]
fn replay_reads_as_a_conversation() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    let told = without_duration(&stdout(&run(dir.path(), &["replay", "j.jsonl"], &[])));
    assert!(
        told.starts_with("session on mock-echo-1\n\nturn 1\n  step 1\n  you   echo this\n"),
        "{told}"
    );
    assert!(told.contains("  ai    You said: echo this\n"), "{told}");
    assert!(
        told.ends_with("turn 1 \u{b7} natural \u{b7} 2 steps \u{b7} 57 tokens\n"),
        "{told}"
    );

    let raw = stdout(&run(dir.path(), &["replay", "j.jsonl", "--raw"], &[]));
    assert!(raw.starts_with("   0  session/start"), "{raw}");
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
    // The tracer reads the turn's own bus, so its first entry is still
    // `turn/start`; the journal seq beside it is 1, because the header the
    // session was opened with took seq 0.
    assert!(traced.starts_with("   0     1  turn/start"), "{traced}");
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

/// TC-CLI-UI-13: a journal whose model thought before it answered.
/// Expected: `replay` shows one folded line naming what is behind it, and
/// `replay --think` shows every line of it. The mock adapter never thinks, so
/// this case states the journal itself - a reasoning model's journal is an
/// ordinary journal, and the surface must read one written by any adapter.
#[test]
fn thinking_is_folded_until_it_is_asked_for() {
    let dir = tempfile::tempdir().expect("temp dir");
    let journal = [
        r#"{"type":"turn/start","seq":0,"time":10,"data":{"turn":1}}"#,
        r#"{"type":"assistant/message","seq":1,"time":20,"data":{"content":"42","reasoning":"Work it out.\nSix by seven.\nThat is 42."}}"#,
        r#"{"type":"turn/end","seq":2,"time":30,"data":{"turn":1,"steps":1,"stop_reason":"natural"}}"#,
    ]
    .join("\n");
    std::fs::write(dir.path().join("t.jsonl"), format!("{journal}\n")).expect("journal");

    let folded = stdout(&run(dir.path(), &["replay", "t.jsonl"], &[]));
    assert!(
        folded.contains("  think Work it out.  +2 lines\n"),
        "not folded:\n{folded}"
    );
    assert!(!folded.contains("Six by seven"), "{folded}");

    let opened = stdout(&run(dir.path(), &["replay", "t.jsonl", "--think"], &[]));
    for line in ["Work it out.", "Six by seven.", "That is 42."] {
        assert!(opened.contains(line), "`{line}` missing from:\n{opened}");
    }
    assert!(opened.contains("  ai    42\n"), "{opened}");
}

/// TC-CLI-UI-14: the journal a run leaves behind.
/// Expected: its first line is the `session/start` header, carrying the id,
/// the provider, the model and the step budget the run actually used - the
/// facts that let `tetanus sessions` and `tetanus replay` read a journal
/// nobody told them about. A second run on the same journal reopens it: one
/// header, and the turn numbering carries on.
#[test]
fn a_run_writes_a_journal_that_says_what_it_ran_under() {
    let dir = tempfile::tempdir().expect("temp dir");
    let args = &[
        "run",
        "-p",
        "echo this",
        "--session",
        "j.jsonl",
        "--max-steps",
        "3",
    ];
    assert!(run(dir.path(), args, &[]).status.success());

    let written = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");
    let first: serde_json::Value =
        serde_json::from_str(written.lines().next().expect("a line")).expect("json");

    assert_eq!(first["type"], "session/start");
    assert_eq!(first["seq"], 0);
    assert_eq!(first["data"]["provider"], "mock");
    assert_eq!(first["data"]["model"], "mock-echo-1");
    assert_eq!(first["data"]["max_steps"], 3);
    assert!(
        first["data"]["session_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "no id on the header: {first}"
    );

    assert!(run(dir.path(), args, &[]).status.success());
    let again = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");
    assert_eq!(
        again.matches(r#""type":"session/start""#).count(),
        1,
        "a reopened journal gained a second header:\n{again}"
    );
    assert!(again.contains(r#""turn":2"#), "the turn did not carry on");
}

/// TC-CLI-UI-15: `tetanus replay --ui` with nowhere to draw.
/// Expected: `InvalidParams`, exit 2 per contract §4.5, a message naming the
/// terminal, and nothing printed - a piped `--ui` must not quietly fall back
/// to the plain page, because a script that asked for a view and got a
/// transcript has been answered a different question. It is refused before the
/// path is read, so a `--ui` at a path that is not there says the same thing.
/// The three flags it cannot be combined with are clap's to refuse.
#[test]
fn a_journal_read_full_screen_needs_a_screen() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    let piped = run(dir.path(), &["replay", "j.jsonl", "--ui"], &[]);
    assert_eq!(piped.status.code(), Some(2), "{}", stdout(&piped));
    assert!(stderr(&piped).contains("terminal"), "{}", stderr(&piped));
    assert!(stdout(&piped).is_empty(), "{}", stdout(&piped));

    let missing = run(dir.path(), &["replay", "nope.jsonl", "--ui"], &[]);
    assert_eq!(missing.status.code(), Some(2), "{}", stdout(&missing));

    for flag in ["--raw", "--live", "--json"] {
        let clash = run(dir.path(), &["replay", "j.jsonl", "--ui", flag], &[]);
        assert_eq!(clash.status.code(), Some(2), "{}", stdout(&clash));
        assert!(stderr(&clash).contains(flag), "{}", stderr(&clash));
    }
}

/// TC-CLI-UI-16: `tetanus sessions --ui` with nowhere to draw.
/// Expected: the same answer `run` and `replay` give a piped `--ui` -
/// `InvalidParams`, exit 2 per contract §4.5, the terminal named, and nothing
/// on stdout. It is refused before the directory is read, so a `--ui` at a
/// directory holding no journals says the same thing rather than printing the
/// empty list. `--json` asks for the opposite of a screen, and `--think`
/// means nothing with no journal open, so clap refuses both pairings itself.
#[test]
fn a_session_picker_needs_a_screen() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "sessions/j.jsonl"],
        &[],
    );

    let piped = run(dir.path(), &["sessions", "--ui"], &[]);
    assert_eq!(piped.status.code(), Some(2), "{}", stdout(&piped));
    assert!(stderr(&piped).contains("terminal"), "{}", stderr(&piped));
    assert!(stdout(&piped).is_empty(), "{}", stdout(&piped));

    let nowhere = run(dir.path(), &["sessions", "--dir", "nope", "--ui"], &[]);
    assert_eq!(nowhere.status.code(), Some(2), "{}", stdout(&nowhere));

    let clash = run(dir.path(), &["sessions", "--ui", "--json"], &[]);
    assert_eq!(clash.status.code(), Some(2), "{}", stdout(&clash));
    assert!(stderr(&clash).contains("--json"), "{}", stderr(&clash));

    let alone = run(dir.path(), &["sessions", "--think"], &[]);
    assert_eq!(alone.status.code(), Some(2), "{}", stdout(&alone));
    assert!(stderr(&alone).contains("--ui"), "{}", stderr(&alone));
}

/// TC-CLI-JSON-1: `tetanus run --json`.
/// Expected: one JSON object per line and nothing else on stdout - every line
/// but the last a `SessionEvent`, the last the `agent.prompt` result. A script
/// reads lines until the stream ends and treats the last one as the answer,
/// which is only true if no human line is mixed into them.
#[test]
fn a_json_run_streams_events_and_ends_with_the_result() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl", "--json"],
        &[],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let printed = stdout(&out);
    assert!(!printed.contains(ESC), "{printed:?}");
    let lines: Vec<&str> = printed.lines().collect();
    let (last, events) = lines.split_last().expect("at least the result");

    for line in events {
        let event: serde_json::Value = serde_json::from_str(line).expect(line);
        assert!(event["type"].is_string(), "{line}");
        assert!(event["seq"].is_u64(), "{line}");
    }
    let result: serde_json::Value = serde_json::from_str(last).expect(last);
    let summary = &result["summary"];
    assert_eq!(summary["turn"], 1);
    assert_eq!(summary["stop_reason"], "natural");
    assert_eq!(summary["content"], "You said: echo this");
    assert!(
        summary["usage"]["prompt_tokens"].as_u64().unwrap() > 0,
        "{last}"
    );
}

/// TC-CLI-JSON-2: what those event lines are.
/// Expected: the journal's own lines, byte for byte and in order. The contract
/// says the result types are printed verbatim, so a streamed event and the
/// durable record of it cannot be two different documents.
#[test]
fn the_streamed_events_are_the_journal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl", "--json"],
        &[],
    );

    let printed = stdout(&out);
    let streamed = printed.lines().collect::<Vec<_>>();
    let journal = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");

    assert_eq!(
        streamed[..streamed.len() - 1].join("\n"),
        journal.trim_end()
    );
}

/// TC-CLI-JSON-3: `tetanus replay --json`.
/// Expected: exactly one line, the `session.events` page: every event of the
/// journal, the seq a next page would start at, and `eof`. A read does not
/// stream, so a script that ran one reads one line.
#[test]
fn a_json_replay_is_one_page_on_one_line() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    let out = run(dir.path(), &["replay", "j.jsonl", "--json"], &[]);
    let printed = stdout(&out);

    assert_eq!(printed.lines().count(), 1, "{printed}");
    let page: serde_json::Value = serde_json::from_str(printed.trim()).expect(&printed);
    let events = page["events"].as_array().expect("events");
    let journal = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");

    assert_eq!(events.len(), journal.lines().count());
    assert_eq!(page["eof"], true);
    assert_eq!(page["next_seq"], events.len() as u64);
}

/// TC-CLI-JSON-4: contract output asked for beside a human view.
/// Expected: a usage error, exit 2, from clap. `--json` fixes what stdout is;
/// a second view on the same stream would make the JSON unreadable to the
/// script that asked for it, and quietly dropping one of them would be worse.
#[test]
fn json_beside_a_human_view_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "j.jsonl"],
        &[],
    );

    for args in [
        vec!["replay", "j.jsonl", "--json", "--raw"],
        vec!["replay", "j.jsonl", "--json", "--live"],
        vec!["run", "--json", "--trace"],
    ] {
        let out = run(dir.path(), &args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stdout(&out));
    }
}

/// TC-CLI-CAT-9: `tetanus models`, with and without the credential.
/// Expected: both providers listed under the names `--adapter` accepts; the
/// one whose key is absent names the variable to set, and the same command
/// reads `ready` once that variable is exported. The catalogue is read at the
/// moment it is asked for - a cached answer would tell a user who just fixed
/// their key that it is still broken.
#[test]
fn the_model_page_answers_whether_a_provider_can_be_reached() {
    let dir = tempfile::tempdir().expect("temp dir");

    let bare = stdout(&run(dir.path(), &["models"], &[]));
    assert!(bare.contains("mock"), "{bare}");
    assert!(
        bare.contains("deepseek  set DEEPSEEK_API_KEY"),
        "the unreachable provider does not say what to do:\n{bare}"
    );

    let keyed = stdout(&run(
        dir.path(),
        &["models"],
        &[("DEEPSEEK_API_KEY", "sk-x")],
    ));
    assert!(
        keyed.contains("deepseek  ready"),
        "the key was exported and the page did not notice:\n{keyed}"
    );
}

/// TC-CLI-CAT-10: the three subcommands that do not stream, with `--json`.
/// Expected: exactly one line each, parsing as the call's result type, with
/// its one documented field and no escape bytes even when colour is forced.
/// Contract §4.7: a subcommand that does not stream prints exactly one line,
/// so a script reads one line whichever of them it ran.
#[test]
fn a_read_only_subcommand_prints_exactly_one_result_line() {
    let dir = tempfile::tempdir().expect("temp dir");

    for (args, field) in [
        (["models", "--json"], "providers"),
        (["tools", "--json"], "tools"),
        (["config", "--json"], "entries"),
    ] {
        let out = run(dir.path(), &args, &[("CLICOLOR_FORCE", "1")]);
        assert!(out.status.success(), "{}", stderr(&out));
        let printed = stdout(&out);

        let lines: Vec<&str> = printed.lines().collect();
        assert_eq!(lines.len(), 1, "`{args:?}` printed:\n{printed}");
        assert!(
            !printed.contains(ESC),
            "`{args:?}` was coloured:\n{printed}"
        );

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("one JSON object");
        let object = parsed.as_object().expect("an object");
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            vec![field],
            "`{args:?}` added a field"
        );
        assert!(object[field].is_array(), "`{field}` is not a list");
    }
}

/// TC-CLI-CAT-11: `tetanus tools` against the tools a turn really calls.
/// Expected: every tool named by a `tool/call` in the journal of an offline
/// run is on the page. The page is built from the registry the turn is booted
/// with, and this is the case that keeps that true: a tool a run can call and
/// the page does not list is a tool nobody can discover.
#[test]
fn the_tool_page_lists_what_a_turn_can_call() {
    let dir = tempfile::tempdir().expect("temp dir");
    run(dir.path(), &["run", "--session", "j.jsonl"], &[]);
    let journal = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");

    let called: Vec<String> = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["type"] == "tool/call")
        .filter_map(|event| event["data"]["name"].as_str().map(str::to_string))
        .collect();
    assert!(!called.is_empty(), "the turn called no tool:\n{journal}");

    let page = stdout(&run(dir.path(), &["tools"], &[]));
    for tool in called {
        assert!(page.contains(&tool), "`{tool}` is not on the page:\n{page}");
    }
}

/// TC-CLI-SESS-7: `tetanus sessions` on a directory two runs wrote into.
/// Expected: one row per journal, newest first, each carrying the size of the
/// journal and the prompt that opened it. This is the page a user reads an id
/// out of, so it is asserted against journals a real run wrote and not
/// against a fixture.
#[test]
fn sessions_lists_what_the_runs_wrote() {
    let dir = tempfile::tempdir().expect("temp dir");
    for (prompt, path) in [
        ("echo this", "sessions/a.jsonl"),
        ("and again", "sessions/b.jsonl"),
    ] {
        let out = run(dir.path(), &["run", "-p", prompt, "--session", path], &[]);
        assert!(out.status.success(), "{}", stderr(&out));
    }

    let told = stdout(&run(dir.path(), &["sessions", "--color", "never"], &[]));
    let rows: Vec<&str> = told.lines().skip(2).collect();

    assert_eq!(rows.len(), 2, "{told}");
    assert!(
        rows[0].starts_with("b  "),
        "the newest is not first:\n{told}"
    );
    assert!(rows[0].ends_with("idle  and again"), "{told}");
    assert!(rows[1].starts_with("a  "), "{told}");
    assert!(rows[1].ends_with("idle  echo this"), "{told}");
    assert!(rows[0].contains("18 events"), "{told}");
}

/// TC-CLI-SESS-8: the id the page prints against the journal it names.
/// Expected: the id is the journal's file name, because a store resolves an
/// id to `<root>/<id>.jsonl` and an id that resolves to nothing is worth
/// nothing to the reader who retypes it. `--json` carries both, verbatim.
#[test]
fn the_id_a_session_is_listed_under_names_its_journal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "sessions/kept.jsonl"],
        &[],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let listed = stdout(&run(dir.path(), &["sessions", "--json"], &[]));
    let page: serde_json::Value = serde_json::from_str(listed.trim()).expect("one json object");
    let session = &page["sessions"][0];

    assert_eq!(session["session_id"], "kept");
    assert_eq!(session["path"], "sessions/kept.jsonl");
    assert_eq!(session["model"], "mock-echo-1");
    assert_eq!(session["state"], "idle");
    assert_eq!(listed.lines().count(), 1, "not one line: {listed:?}");
}

/// TC-CLI-SESS-9: `tetanus sessions` before anything has been run.
/// Expected: exit 0 and a page that says what writes one. An empty store is
/// not a failure, and a missing directory is the ordinary first-run state.
#[test]
fn an_empty_store_is_not_a_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["sessions", "--color", "never"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "\nsessions\nno sessions yet - tetanus run writes one\n"
    );
    assert_eq!(
        stdout(&run(dir.path(), &["sessions", "--json"], &[])),
        "{\"sessions\":[]}\n"
    );
}

/// TC-CLI-ERR-5: a provider that answers nothing.
/// Expected: exit 6, `ProviderError` in the contract's table - not the 1 a
/// build failure would exit with, and not the 5 a missing key exits with. The
/// key is present here and the endpoint is a closed local port, so nothing
/// leaves the machine. A script that retries a flaky provider and reports a
/// broken installation needs those three to be three different numbers.
#[test]
fn a_provider_that_cannot_be_reached_exits_six() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "--adapter", "deepseek", "--session", "p.jsonl"],
        &[
            ("DEEPSEEK_API_KEY", "sk-not-a-real-key"),
            ("DEEPSEEK_BASE_URL", "http://127.0.0.1:1"),
        ],
    );

    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("deepseek could not be reached"), "{err}");
    assert!(err.contains("note: try again"), "{err}");
}

/// TC-CLI-ERR-6: a journal whose sequence numbers do not follow.
/// Expected: exit 1, `LogCorrupt`, the line named, and a note pointing at the
/// one command that still reads a broken journal. Naming the line is the
/// difference between a file a user can repair and a file they delete.
#[test]
fn a_corrupt_journal_names_the_line_it_stopped_at() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("bad.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn/start\",\"seq\":7,\"time\":1,\"data\":{}}\n",
    )
    .expect("write");

    let out = run(dir.path(), &["replay", "bad.jsonl"], &[]);

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("the journal is not readable at line 1"),
        "{err}"
    );
    assert!(err.contains("--raw"), "{err}");
}

/// TC-CLI-ERR-8: the note from TC-CLI-ERR-6, taken at its word.
/// Expected: `--raw` on the very journal the cooked view refused prints the
/// line and exits 0. A note that names a command has to name one that works;
/// before this, `--raw` failed the same way and the advice was a dead end.
#[test]
fn the_raw_view_reads_the_journal_the_note_sends_you_to() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("bad.jsonl"),
        "{\"type\":\"turn/start\",\"seq\":7,\"time\":1,\"data\":{}}\n",
    )
    .expect("write");

    let refused = run(dir.path(), &["replay", "bad.jsonl"], &[]);
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("--raw"), "{}", stderr(&refused));

    let out = run(dir.path(), &["replay", "bad.jsonl", "--raw"], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "   7  turn/start           {}\n");
}

/// TC-CLI-ERR-9: a journal with one line that is not an event at all.
/// Expected: exit 1 and `LogCorrupt`, but the page is still printed - the
/// lines before the bad one, the bad one marked and quoted under its number,
/// and the lines after it. The failure goes to stderr with a note that does
/// not send the user back to the view they are already in.
#[test]
fn the_raw_view_shows_a_broken_line_where_it_is() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("half.jsonl"),
        "{\"type\":\"turn/start\",\"seq\":0,\"time\":1,\"data\":{}}\n\
         {oh no\n\
         {\"type\":\"turn/end\",\"seq\":2,\"time\":3,\"data\":{}}\n",
    )
    .expect("write");

    let out = run(dir.path(), &["replay", "half.jsonl", "--raw"], &[]);

    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        concat!(
            "   0  turn/start           {}\n",
            "   ?  unreadable           line 2: {oh no\n",
            "   2  turn/end             {}\n",
        )
    );
    let err = stderr(&out);
    assert!(err.contains("not readable at line 2"), "{err}");
    assert!(
        !err.contains("--raw"),
        "the note sends the user back to the view they ran:\n{err}"
    );
}

/// TC-CLI-ERR-10: a journal path that is not there, in all three shapes.
/// Expected: the contract's `SessionNotFound` status, the path named, a note
/// that points at `tetanus sessions`, and nothing at all on stdout. Reading a
/// path that does not exist as an empty session is how a typo becomes a blank
/// page and a zero exit, which is the failure a user never notices.
#[test]
fn a_journal_that_is_not_there_is_not_an_empty_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let want = i32::from(tetanus_protocol::rpc::ErrorCode::SessionNotFound.exit_status());

    for args in [
        vec!["replay", "nope.jsonl"],
        vec!["replay", "nope.jsonl", "--raw"],
        vec!["replay", "nope.jsonl", "--json"],
    ] {
        let out = run(dir.path(), &args, &[]);

        assert_eq!(
            out.status.code(),
            Some(want),
            "`{args:?}`: {}",
            stderr(&out)
        );
        assert_eq!(
            stdout(&out),
            "",
            "`{args:?}` wrote a page for a missing file"
        );
        let err = stderr(&out);
        assert!(
            err.contains("no journal at nope.jsonl"),
            "`{args:?}`: {err}"
        );
        assert!(err.contains("tetanus sessions"), "`{args:?}`: {err}");
    }
    assert!(
        !dir.path().join("nope.jsonl").exists(),
        "a read created a file"
    );
}

/// TC-CLI-ERR-11: a journal that is there and holds nothing.
/// Expected: exit 0 and one line saying so, in every human view. The file
/// exists, so this is not a failure - but a page with nothing on it reads
/// exactly like a command that did nothing, and the two have to be told
/// apart. `--json` is unaffected: an empty page is a valid result.
#[test]
fn an_empty_journal_says_it_is_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("empty.jsonl"), "").expect("write");

    for args in [
        vec!["replay", "empty.jsonl"],
        vec!["replay", "empty.jsonl", "--raw"],
        vec!["replay", "empty.jsonl", "--live"],
    ] {
        let out = run(dir.path(), &args, &[]);

        assert_eq!(out.status.code(), Some(0), "`{args:?}`: {}", stderr(&out));
        assert_eq!(stdout(&out), "the journal is empty\n", "`{args:?}`");
    }

    let json = run(dir.path(), &["replay", "empty.jsonl", "--json"], &[]);
    assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
    assert_eq!(
        stdout(&json),
        "{\"events\":[],\"next_seq\":0,\"eof\":true}\n"
    );
}

/// TC-CLI-ERR-7: a turn that ends normally.
/// Expected: exit 0. The statuses above are only worth anything if success is
/// still zero, and a renderer that writes to a closed pipe must not turn a
/// finished turn into a failure.
#[test]
fn a_finished_turn_still_exits_zero() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["run", "--session", "j.jsonl"], &[]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

/// TC-CLI-INFO-3: the build page against the catalogues and the contract.
/// Expected: the counts equal the lengths of the two `--json` catalogues, and
/// the protocol equals the version the contract crate publishes. The page is
/// what a bug report quotes, so a count assembled by hand here and a list
/// printed there must not be able to disagree.
#[test]
fn the_build_page_counts_agree_with_the_catalogues() {
    let dir = tempfile::tempdir().expect("temp dir");

    fn listed(dir: &Path, args: &[&str], field: &str) -> usize {
        let printed = stdout(&run(dir, args, &[]));
        let parsed: serde_json::Value = serde_json::from_str(printed.trim()).expect("JSON");
        parsed[field].as_array().expect("a list").len()
    }

    fn value(page: &str, label: &str) -> String {
        page.lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("no `{label}` row:\n{page}"))
            .split_whitespace()
            .nth(1)
            .expect("a value")
            .to_string()
    }

    let page = stdout(&run(dir.path(), &["info"], &[]));
    let providers = listed(dir.path(), &["models", "--json"], "providers");
    let tools = listed(dir.path(), &["tools", "--json"], "tools");

    assert_eq!(value(&page, "providers"), providers.to_string(), "{page}");
    assert_eq!(value(&page, "tools"), tools.to_string(), "{page}");
    assert_eq!(
        value(&page, "protocol"),
        tetanus_protocol::PROTOCOL_VERSION,
        "{page}"
    );
    assert!(page.starts_with("\ntetanus "), "{page}");
}
