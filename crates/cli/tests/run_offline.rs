//! Test Design Specification: the headless `tetanus run` command.
//!
//! Features tested: an offline turn on the built-in mock adapter, the printed
//! event sequence, the journal on disk, where the prompt is read from, the
//! failure message when a real provider has no credential, the refusal of a
//! full-screen view with no screen to draw on, the closing row that says where
//! the journal went, and what the settings document decides for a run that was
//! given no flags, and the document a `--settings` path names in place of
//! it. Features NOT tested here: the
//! flow itself (owned by the conformance suite in `tetanus-turn`), any live
//! provider, resolving a prompt from plain data (owned by `prompt`, asserted
//! in its own module), and what a full-screen view draws once it has a
//! terminal - a case that runs offline cannot have one, so the drawing is
//! asserted by `tetanus_ui::Page` and the loop by `tetanus_ui::show`.
//!
//! Environmental needs: none. Every case runs offline.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;

use common::without_duration;

fn run(dir: &Path, args: &[&str], key: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    // The harness home is the case's own directory, so a settings document on
    // the machine running the suite cannot decide what a run runs on.
    cmd.current_dir(dir).args(args).env("TETANUS_HOME", dir);
    match key {
        Some(value) => cmd.env("DEEPSEEK_API_KEY", value),
        None => cmd.env_remove("DEEPSEEK_API_KEY"),
    };
    cmd.output().expect("the binary runs")
}

/// The same run, with `typed` on its standard input rather than a terminal.
fn piped(dir: &Path, args: &[&str], typed: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        .args(args)
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(typed.as_bytes())
        .expect("the prompt is written");
    child.wait_with_output().expect("the binary exits")
}

/// TC-CLI-1: one full turn with no key, no network and no config.
/// Expected: exit 0; the printed sequence opens on `turn/start` and closes on
/// `turn/end` with the tool pipeline between; the answer is the deterministic
/// mock reply; the named journal exists and replays.
#[test]
fn runs_one_full_turn_offline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &[
            "run",
            "--trace",
            "--prompt",
            "run one full turn",
            "--session",
            "journal.jsonl",
        ],
        None,
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let topics: Vec<&str> = stdout
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().last())
        .collect();

    assert_eq!(topics.first(), Some(&"turn/start"));
    assert_eq!(topics.last(), Some(&"turn/end"));
    for expected in [
        "agent/pre-step",
        "system-prompt/assemble",
        "agent/request",
        "llm/stream",
        "tools/execute",
        "agent/turn-stopping",
    ] {
        assert!(
            topics.contains(&expected),
            "{expected} is missing from:\n{stdout}"
        );
    }
    // Column widths belong to the presentation suite; this case only cares
    // that the summary reports the stop reason.
    let stop = stdout.lines().find(|line| line.starts_with("stop"));
    assert!(
        stop.is_some_and(|line| line.ends_with("natural")),
        "{stdout}"
    );
    assert!(stdout.contains("You said: run one full turn"), "{stdout}");

    let journal = dir.path().join("journal.jsonl");
    let events = tetanus_session::replay(&journal).expect("the journal replays");
    // The journal opens on the header the session was created with, not on
    // the turn: a reader has to be able to tell what the turn ran under.
    assert_eq!(events.first().expect("first").ty, "session/start");
    assert_eq!(events[1].ty, "turn/start");
    assert_eq!(events.last().expect("last").ty, "turn/end");
}

/// TC-CLI-2: the same run is reproducible, so two runs print the same turn.
/// Expected: byte-identical stdout, once how long each run took is dropped.
/// Each run gets its own directory and the same journal name, so even the path
/// the summary echoes is the same. The wall clock is the one field a repeated
/// run does not owe the first one, and a loaded machine can push one run past
/// the second the closing line starts reporting at.
#[test]
fn the_offline_run_is_reproducible() {
    let args = [
        "run",
        "--prompt",
        "same in, same out",
        "--session",
        "j.jsonl",
    ];
    let first = tempfile::tempdir().expect("temp dir");
    let second = tempfile::tempdir().expect("temp dir");

    let stdout = |dir: &Path| {
        without_duration(&String::from_utf8(run(dir, &args, None).stdout).expect("utf-8"))
    };

    assert_eq!(stdout(first.path()), stdout(second.path()));
}

/// TC-CLI-3: a real provider with no credential.
/// Expected: a non-zero exit that names the environment variable and points at
/// the offline adapter; no journal is created.
#[test]
fn a_real_adapter_without_a_key_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "--adapter", "deepseek", "--session", "never.jsonl"],
        None,
    );

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DEEPSEEK_API_KEY"), "{stderr}");
    assert!(stderr.contains("--adapter mock"), "{stderr}");
    assert!(
        !dir.path().join("never.jsonl").exists(),
        "nothing was written"
    );
}

/// TC-CLI-4: the prompt as the command's argument, the way upstream's headless
/// profile takes it.
/// Expected: exit 0, and the turn answers what was typed - not the default a
/// bare `tetanus run` asks. A positional that silently lost its value would
/// still exit 0 and still print a turn, so the answer is what is asserted.
#[test]
fn a_positional_prompt_is_what_the_turn_asks() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "list the files", "--session", "journal.jsonl"],
        None,
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.contains("You said: list the files"), "{stdout}");
}

/// TC-CLI-5: `-`, with a prompt on standard input.
/// Expected: exit 0, and the turn asks the whole of it - the blank line in the
/// middle included. This is the case the source exists for: a prompt with
/// paragraphs in it is one no shell quotes comfortably.
#[test]
fn a_dash_reads_the_prompt_from_standard_input() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = piped(
        dir.path(),
        &["run", "-", "--session", "journal.jsonl"],
        "first line\n\nlast line\n",
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = tetanus_session::replay(dir.path().join("journal.jsonl")).expect("replays");
    let asked = events
        .iter()
        .find(|event| event.ty == "user/message")
        .map(|event| event.data.to_string())
        .unwrap_or_default();
    assert!(asked.contains("first line"), "{asked}");
    assert!(asked.contains("last line"), "{asked}");
}

/// TC-CLI-6: `-`, with nothing on standard input.
/// Expected: exit 2 - the status §4.5 gives a bad argument - a message naming
/// the prompt, and no journal. Stopping before the journal is opened is the
/// point: a turn that ran would leave a file recording that the agent was
/// asked nothing, and would have spent a provider call to write it.
#[test]
fn an_empty_standard_input_stops_before_the_journal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = piped(dir.path(), &["run", "-", "--session", "never.jsonl"], "");

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("prompt"), "{stderr}");
    assert!(
        !dir.path().join("never.jsonl").exists(),
        "nothing was written"
    );
}

/// TC-CLI-7: both prompt forms at once.
/// Expected: a usage error, exit 2, and no journal. The two forms say the same
/// thing, so accepting both would mean silently picking one and running a turn
/// the user did not ask for.
#[test]
fn the_two_prompt_forms_are_refused_together() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &[
            "run",
            "here",
            "--prompt",
            "there",
            "--session",
            "never.jsonl",
        ],
        None,
    );

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--prompt"), "{stderr}");
    assert!(
        !dir.path().join("never.jsonl").exists(),
        "nothing was written"
    );
}

/// TC-CLI-8: a full-screen view with nowhere to draw.
/// Expected: `InvalidParams`, exit 2 per contract §4.5, a message naming the
/// terminal, and no journal - the refusal comes before anything is opened, so
/// a run that will never be seen leaves nothing behind. And `--ui` with
/// `--json` is refused by the parser, because one of them owns the screen and
/// the other owns stdout.
#[test]
fn a_full_screen_view_needs_a_screen() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["run", "--ui", "--session", "never.jsonl"],
        None,
    );

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("terminal"), "{stderr}");
    assert!(
        !dir.path().join("never.jsonl").exists(),
        "nothing was written"
    );

    let clash = run(
        dir.path(),
        &["run", "--ui", "--json", "--session", "never.jsonl"],
        None,
    );

    assert_eq!(clash.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&clash.stderr);
    assert!(stderr.contains("--json"), "{stderr}");
}

/// TC-CLI-9: a journal named with escape sequences and a line feed.
/// Expected: exit 0, and the closing `journal` row is one line holding the
/// name with the sequences taken out and the feed turned into a space. The
/// name is what `--session` was given, so a shell can put anything in it; this
/// row is the last thing a run prints, and a sequence in it reaches a terminal
/// that has already been told the run is over.
#[test]
fn a_journal_named_with_an_escape_sequence_stays_one_row() {
    let dir = tempfile::tempdir().expect("temp dir");
    let named = "na\u{1b}[2Jsty\u{1b}]0;pwned\u{7}\nlog.jsonl";
    let out = run(
        dir.path(),
        &["run", "--prompt", "hi", "--session", named],
        None,
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("journal"))
        .collect();

    assert_eq!(rows, vec!["journal  nasty log.jsonl"], "{stdout:?}");
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
    // The file itself keeps the name it was given: taming is what is drawn,
    // not what is opened.
    assert!(dir.path().join(named).exists(), "the journal was written");
}

/// The `session/start` a journal opens with: where a run records the provider,
/// the model and the step budget it actually ran under.
fn header(dir: &Path, path: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join(path)).expect("the journal exists");
    let first = text
        .lines()
        .next()
        .expect("the journal has a header")
        .to_string();
    serde_json::from_str::<serde_json::Value>(&first).expect("the header is json")["data"].clone()
}

/// The settings document under the case's own harness home.
fn document(dir: &Path, text: &str) {
    std::fs::write(dir.join("settings.yaml"), text).expect("the document is written");
}

/// TC-CLI-CONF-8: a document setting `sessions.root`, `model.default` and
/// `agent.max_steps`, against a bare `tetanus run`.
/// Expected: exit 0; the journal is at `journals/turn.jsonl` rather than under
/// the compiled root; its header records the document's model and its budget;
/// and the closing row names the path the document chose. The header is what
/// is read rather than the screen: it is what the next reader of the journal
/// is told the turn ran under.
#[test]
fn a_document_decides_where_a_run_goes_and_what_it_runs_on() {
    let dir = tempfile::tempdir().expect("temp dir");
    document(
        dir.path(),
        "sessions:\n  root: journals\nmodel:\n  default: documented-model\nagent:\n  max_steps: 3\n",
    );

    let out = run(dir.path(), &["run", "--prompt", "hi"], None);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let head = header(dir.path(), "journals/turn.jsonl");
    assert_eq!(head["model"], "documented-model", "{head}");
    assert_eq!(head["max_steps"], 3, "{head}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.contains("journal  journals/turn.jsonl"), "{stdout}");
}

/// TC-CLI-CONF-9: the same document, with each flag it names given on the
/// command line.
/// Expected: the flag wins every time - the journal is where `--session` put
/// it, the header holds `--model` and `--max-steps`, and `--adapter mock`
/// beats a document asking for a provider this case has no credential for.
/// A flag that lost to a document would be a flag that does nothing, which is
/// worse than not having it.
#[test]
fn a_flag_beats_the_document_a_run_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    document(
        dir.path(),
        "sessions:\n  root: journals\nmodel:\n  default: documented-model\nagent:\n  max_steps: 3\n",
    );

    let out = run(
        dir.path(),
        &[
            "run",
            "--prompt",
            "hi",
            "--session",
            "flagged/x.jsonl",
            "--model",
            "flagged-model",
            "--max-steps",
            "2",
        ],
        None,
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let head = header(dir.path(), "flagged/x.jsonl");
    assert_eq!(head["model"], "flagged-model", "{head}");
    assert_eq!(head["max_steps"], 2, "{head}");

    let other = tempfile::tempdir().expect("temp dir");
    document(other.path(), "provider:\n  default: deepseek\n");
    let offline = run(
        other.path(),
        &["run", "--prompt", "hi", "--adapter", "mock"],
        None,
    );

    assert!(
        offline.status.success(),
        "{}",
        String::from_utf8_lossy(&offline.stderr)
    );
    assert_eq!(
        header(other.path(), "sessions/turn.jsonl")["provider"],
        "mock"
    );
}

/// TC-CLI-CONF-13: `--settings` naming a document, with a different document
/// under the harness home.
/// Expected: the named document decides everything - where the journal goes,
/// the model, the budget - and the home's own document decides nothing. A
/// build that merged the two, or that let the home win, would make the flag a
/// suggestion: the reason to type a path is to run on that file and no other.
///
/// Environmental needs: the case's own harness home holds a document, and a
/// second document sits in a directory of its own.
#[test]
fn a_named_document_is_read_instead_of_the_one_at_home() {
    let dir = tempfile::tempdir().expect("temp dir");
    document(
        dir.path(),
        "sessions:\n  root: at-home\nmodel:\n  default: home-model\nagent:\n  max_steps: 7\n",
    );
    let named = dir.path().join("elsewhere/other.yaml");
    std::fs::create_dir_all(named.parent().expect("a parent")).expect("the directory is made");
    std::fs::write(
        &named,
        "sessions:\n  root: named\nmodel:\n  default: named-model\nagent:\n  max_steps: 2\n",
    )
    .expect("the document is written");

    let out = run(
        dir.path(),
        &["--settings", "elsewhere/other.yaml", "run", "-p", "hi"],
        None,
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let head = header(dir.path(), "named/turn.jsonl");
    assert_eq!(head["model"], "named-model", "{head}");
    assert_eq!(head["max_steps"], 2, "{head}");
    assert!(
        !dir.path().join("at-home").exists(),
        "the home's document was read as well"
    );
}

/// TC-CLI-CONF-10: the three ways a document stops a run - a provider this
/// case cannot reach, a provider this build does not have, and a document that
/// does not parse.
/// Expected: exit 5, 2 and 1 in that order, each naming what has to be fixed,
/// and no journal from any of them. A run that fell back to the compiled
/// defaults would answer on a provider nobody asked for and record it as if it
/// had been asked for.
#[test]
fn a_document_a_run_cannot_use_stops_it_before_the_journal() {
    for (settings, code, named) in [
        ("provider:\n  default: deepseek\n", 5, "DEEPSEEK_API_KEY"),
        ("provider:\n  default: bogus\n", 2, "provider.default"),
        ("sessions: [1, 2\n", 1, "settings.yaml"),
    ] {
        let dir = tempfile::tempdir().expect("temp dir");
        document(dir.path(), settings);

        let out = run(dir.path(), &["run", "--prompt", "hi"], None);

        assert_eq!(out.status.code(), Some(code), "{settings:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(named), "{settings:?}: {stderr}");
        assert!(
            !dir.path().join("sessions").exists(),
            "{settings:?} left a journal behind"
        );
    }
}
