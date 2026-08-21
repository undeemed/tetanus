//! Test Design Specification: the binary's colour policy, progress line and
//! diagnostics.
//!
//! Features tested: that the resolved palette reaches clap's own rendering as
//! well as the lines this crate writes itself; that a colour-hostile
//! environment cannot change the bytes of plain output; the shape and exit
//! status of a reported failure; that a failure about a file names the file
//! and reads in one voice whichever view opened it; that a bad `--color` value
//! and an empty value for any flag that takes one are usage errors; that a
//! redirected stderr is told a turn is running whichever view stdout was asked
//! for; which of the two views - the turn, or the raw sequence - a command
//! prints; that a model's thinking stays folded until it is asked for; that
//! the journal a run leaves behind says what it ran under; that the session
//! list finds those journals and names each by the id its store answers to;
//! that a journal read full-screen is refused where there is no screen; that a
//! model named from outside is drawn and not obeyed on the line that says it;
//! which settings document a command boots from, what it does when the one it
//! was told to read is not there, and the page that reads none at all; that
//! the config page and the session list each name the place they read, in
//! full and marked when nothing is there yet; and the shape of the
//! machine-readable output the interface contract fixes. NOT
//! tested here: the resolution rules themselves (owned by `tetanus-ui`'s
//! `color_policy.rs`), what a full-screen view draws once it has a terminal
//! (owned by `render::browse` and `tetanus_ui::Page`, neither of which needs
//! one), and the turn flow (owned by the conformance suite in `tetanus-turn`).
//!
//! Environmental needs: none. Every case runs offline, and every case states
//! the colour-related variables it depends on rather than inheriting them.

use std::path::{Path, PathBuf};
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
    // Every subcommand that builds an engine now reads the settings document
    // under the harness home, so the home is the case's own directory and
    // never the one the person running the suite happens to have configured.
    // A case that is about a document names a home of its own below, and that
    // one wins because it is applied last.
    cmd.env("TETANUS_HOME", dir)
        .env("TERM", "xterm-256color")
        .env("COLUMNS", "100");
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().expect("the binary runs")
}

/// The directory a run started in, as the run itself sees it.
///
/// A page that names a relative path absolutely resolves it against the
/// working directory the process was given, and the process asks the
/// operating system for that - which answers with symbolic links already
/// followed. `tempfile` may hand back a path that goes through one, so a case
/// comparing the two has to follow them here too.
fn here(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().canonicalize().expect("the directory is there")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("utf-8")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("utf-8")
}

/// The same, as one line per diagnostic rather than per row.
///
/// A note longer than the terminal folds under its own tag, so a sentence a
/// case is looking for can arrive across two rows. What the case means is the
/// sentence, not the rows it was drawn on.
fn said(out: &Output) -> String {
    stderr(out).split_whitespace().collect::<Vec<_>>().join(" ")
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

/// The `key  value  layer` rows of a `tetanus config` page, as pairs of key
/// and the layer that settled it.
///
/// Read as rows rather than compared whole: the engine owns which keys exist,
/// and a case that asserted the whole table by equality would fail on the
/// engine adding one - which is a change in another lane, not a fault in this
/// one. What this lane owns is that every key it is handed is printed with
/// where it came from.
fn layers(page: &str) -> Vec<(String, String)> {
    page.lines()
        // The heading is `config` alone or `config  <document>`, so it is
        // recognised by its first word rather than by the whole line.
        .skip_while(|line| line.split_whitespace().next() != Some("config"))
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut columns = line.split_whitespace();
            let key = columns.next().unwrap_or_default().to_string();
            let layer = columns.last().unwrap_or_default().to_string();
            (key, layer)
        })
        .collect()
}

/// TC-CLI-UI-10: `tetanus config` with no settings document.
/// Expected: one row per key the engine settles, carrying the value without
/// its JSON quotes and the layer that settled it - `default` for every one of
/// them, because nothing has been configured. The provenance column is the
/// reason the command exists, so a build that printed the values alone has
/// failed even though it printed something.
///
/// Environmental needs: `TETANUS_HOME` names an empty directory, so the home
/// of whoever runs the suite cannot decide what the case sees.
#[test]
fn config_shows_what_set_each_key() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(
        dir.path(),
        &["config", "--color", "never"],
        &[("TETANUS_HOME", &home.path().display().to_string())],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let page = stdout(&out);
    assert!(page.starts_with("\nconfig  "), "{page}");
    let rows = layers(&page);
    assert!(
        rows.iter().all(|(_, layer)| layer == "default"),
        "a key came from somewhere with no document to come from:\n{page}"
    );
    for key in ["sessions.root", "agent.max_steps", "provider.default"] {
        assert!(
            rows.iter().any(|(name, _)| name == key),
            "{key} is not on the page:\n{page}"
        );
    }
}

/// TC-CLI-CONF-1: `tetanus config` against a document that sets two keys.
/// Expected: exit 0; those two keys report the written value and `file`, and
/// every other key still reports `default`. A page that read the document but
/// reported `default` for what it found would be worse than one that read
/// nothing, because it would say the setting had not taken.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding the
/// document below.
#[test]
fn config_reports_the_document_that_set_a_key() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        home.path().join("settings.yaml"),
        "sessions:\n  root: journals\nagent:\n  max_steps: 3\n",
    )
    .expect("the document is written");

    let out = run(
        dir.path(),
        &["config", "--color", "never"],
        &[("TETANUS_HOME", &home.path().display().to_string())],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let page = stdout(&out);
    let rows = layers(&page);
    let layer = |key: &str| {
        rows.iter()
            .find(|(name, _)| name == key)
            .map(|(_, layer)| layer.clone())
            .unwrap_or_else(|| panic!("{key} is not on the page:\n{page}"))
    };
    assert_eq!(layer("sessions.root"), "file", "{page}");
    assert_eq!(layer("agent.max_steps"), "file", "{page}");
    assert_eq!(layer("provider.default"), "default", "{page}");
    assert!(page.contains("journals"), "{page}");
}

/// TC-CLI-CONF-2: a document whose value is one the key does not take.
/// Expected: exit 2, per §4.5's status for `InvalidParams`; the field is
/// named; the next step names the document rather than sending the reader to
/// `--help`, because nothing in a document is a flag; and nothing is printed
/// on stdout, because there is no resolved configuration to print.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding the
/// document below.
#[test]
fn a_value_the_key_does_not_take_is_a_usage_error() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        home.path().join("settings.yaml"),
        "agent:\n  max_steps: 0\n",
    )
    .expect("the document is written");

    let out = run(
        dir.path(),
        &["config", "--color", "never"],
        &[("TETANUS_HOME", &home.path().display().to_string())],
    );

    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let told = stderr(&out);
    assert!(told.contains("agent.max_steps"), "{told}");
    assert!(
        told.contains(&home.path().join("settings.yaml").display().to_string()),
        "{told}"
    );
    assert!(!told.contains("--help"), "{told}");
    assert_eq!(stdout(&out), "");
}

/// TC-CLI-CONF-3: the two ways the document itself cannot be read.
/// Expected: exit 1, per §4.5's status for `Io`; the path is named once and
/// only once, because a reader deciding which file to open reads two copies
/// of one path as two paths; and nothing on stdout.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding, in turn, a
/// document that does not parse and a directory where the document should be.
#[test]
fn a_document_that_cannot_be_read_stops_the_command() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let document = home.path().join("settings.yaml");
    let named = document.display().to_string();

    std::fs::write(&document, "not: [valid\n").expect("the document is written");
    let broken = run(
        dir.path(),
        &["config", "--color", "never"],
        &[("TETANUS_HOME", &home.path().display().to_string())],
    );
    assert_eq!(broken.status.code(), Some(1), "{}", stderr(&broken));
    assert_eq!(
        stderr(&broken).matches(&named).count(),
        1,
        "{}",
        stderr(&broken)
    );
    assert_eq!(stdout(&broken), "");

    std::fs::remove_file(&document).expect("the document is removed");
    std::fs::create_dir(&document).expect("a directory takes its place");
    let directory = run(
        dir.path(),
        &["config", "--color", "never"],
        &[("TETANUS_HOME", &home.path().display().to_string())],
    );
    assert_eq!(directory.status.code(), Some(1), "{}", stderr(&directory));
    assert_eq!(
        stderr(&directory).matches(&named).count(),
        1,
        "{}",
        stderr(&directory)
    );
    assert_eq!(stdout(&directory), "");
}

/// TC-CLI-CONF-4: `tetanus config --json` against the same document.
/// Expected: one object per §4.7, carrying every key the page carries with
/// the same layer for each. Two views of one answer that disagreed would make
/// a script and a person read the same build differently.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding the
/// document below.
#[test]
fn the_json_config_says_what_the_page_says() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        home.path().join("settings.yaml"),
        "agent:\n  max_steps: 3\n",
    )
    .expect("the document is written");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let page = stdout(&run(dir.path(), &["config", "--color", "never"], &env));
    let json = stdout(&run(dir.path(), &["config", "--json"], &env));

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("one JSON object");
    let entries = parsed["entries"].as_array().expect("the entries");
    let told: Vec<(String, String)> = entries
        .iter()
        .map(|entry| {
            (
                entry["key"].as_str().unwrap_or_default().to_string(),
                entry["layer"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    assert_eq!(told, layers(&page), "{json}\n{page}");
    assert!(
        told.contains(&("agent.max_steps".to_string(), "file".to_string())),
        "{json}"
    );
}

/// TC-CLI-CONF-5: `--dir` against a document that sets `sessions.root`.
/// Expected: without the flag the listing is the document's directory; with
/// it, the flag's. A build where the flag lost would make the document
/// impossible to overrule for one command; a build where the document could
/// never win would make it decorative.
///
/// And the provenance follows the same rule: a flag is only on the `Flag`
/// layer of the process it was typed at, so a page asked for on its own says
/// `file`, and the same page asked with `--dir` says `flag` and carries the
/// flag's value.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding the document
/// below, and the working directory holds two journals a real run wrote, in
/// two directories of their own.
#[test]
fn a_flag_beats_the_document_and_says_so() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    for (prompt, path) in [
        ("written down", "documented/a.jsonl"),
        ("typed out", "flagged/b.jsonl"),
    ] {
        let out = run(dir.path(), &["run", "-p", prompt, "--session", path], &env);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    std::fs::write(
        home.path().join("settings.yaml"),
        "sessions:\n  root: documented\n",
    )
    .expect("the document is written");

    let ids = |args: &[&str]| -> Vec<String> {
        let out = run(dir.path(), args, &env);
        assert!(out.status.success(), "{}", stderr(&out));
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("one JSON object");
        parsed["sessions"]
            .as_array()
            .expect("the sessions")
            .iter()
            .map(|session| {
                session["session_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    };

    assert_eq!(ids(&["sessions", "--json"]), ["a"], "the document lost");
    assert_eq!(
        ids(&["sessions", "--dir", "flagged", "--json"]),
        ["b"],
        "the flag lost"
    );

    let page = stdout(&run(dir.path(), &["config", "--color", "never"], &env));
    assert!(
        layers(&page).contains(&("sessions.root".to_string(), "file".to_string())),
        "{page}"
    );

    let asked = stdout(&run(
        dir.path(),
        &["config", "--dir", "flagged", "--color", "never"],
        &env,
    ));
    assert!(
        layers(&asked).contains(&("sessions.root".to_string(), "flag".to_string())),
        "{asked}"
    );
    assert!(
        asked
            .lines()
            .any(|line| line.starts_with("sessions.root") && line.contains("flagged")),
        "{asked}"
    );
}

/// TC-CLI-CONF-6: a document that cannot be read, on the two subcommands that
/// run an engine rather than describe one.
/// Expected: exit 1 and the path, from both, and no listing and no banner. A
/// boot that fell back to the defaults would put the sessions somewhere the
/// user did not ask for and say nothing about it, which is worse than not
/// starting: the run is lost either way, and only one of the two tells them.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding a document
/// that does not parse.
#[test]
fn a_document_that_cannot_be_read_stops_every_engine() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let document = home.path().join("settings.yaml");
    std::fs::write(&document, "sessions: [1, 2\n").expect("the document is written");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    for args in [vec!["sessions"], vec!["sessions", "--json"], vec!["serve"]] {
        let out = run(dir.path(), &args, &env);

        assert_eq!(out.status.code(), Some(1), "`{args:?}`: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "`{args:?}` printed a page");
        assert!(
            stderr(&out).contains(&document.display().to_string()),
            "`{args:?}` did not name the document: {}",
            stderr(&out)
        );
    }
}

/// TC-CLI-CONF-7: a document that holds a credential.
/// Expected: the key keeps its row and its layer, so a reader can still see
/// that it is set, and the value is `<redacted>` in both views. The page is
/// the engine's own `config.dump`, which §4.3 of the contract says never
/// publishes a secret's value; a surface that printed the resolved layers for
/// itself would print the credential to whoever is reading the terminal, and
/// into whatever the terminal is being logged to.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding the document
/// below.
#[test]
fn a_credential_in_the_document_is_not_printed() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        home.path().join("settings.yaml"),
        "llm:\n  providers:\n    deepseek:\n      api_key: sekrit-value\n",
    )
    .expect("the document is written");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let page = stdout(&run(dir.path(), &["config", "--color", "never"], &env));
    let json = stdout(&run(dir.path(), &["config", "--json"], &env));

    let key = "llm.providers.deepseek.api_key";
    assert!(
        layers(&page).contains(&(key.to_string(), "file".to_string())),
        "the key lost its row or its layer:\n{page}"
    );
    assert!(page.contains("<redacted>"), "{page}");
    assert!(
        !page.contains("sekrit-value"),
        "the page printed it:\n{page}"
    );
    assert!(!json.contains("sekrit-value"), "--json printed it:\n{json}");
}

/// TC-CLI-CONF-11: `--settings` naming a document, with another under the
/// harness home.
/// Expected: the named document is the one every subcommand boots from - the
/// page reports its values on the `file` layer and the listing is its root -
/// and the home's document is not read at all. The flag is global, so it
/// reads the same typed before the subcommand and after it; a flag that only
/// worked in one position would be a flag a reader has to remember the shape
/// of.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding one
/// document, the working directory holds another, and a journal sits under
/// each of the two roots they name.
#[test]
fn a_named_document_is_the_one_every_command_boots_from() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        home.path().join("settings.yaml"),
        "sessions:\n  root: at-home\n",
    )
    .expect("the home's document is written");
    std::fs::write(dir.path().join("named.yaml"), "sessions:\n  root: named\n")
        .expect("the named document is written");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    for path in ["at-home/home.jsonl", "named/flagged.jsonl"] {
        run(dir.path(), &["run", "-p", "hi", "--session", path], &env);
    }

    let page = stdout(&run(
        dir.path(),
        &["--settings", "named.yaml", "config", "--color", "never"],
        &env,
    ));
    assert!(
        page.lines()
            .any(|line| line.starts_with("sessions.root") && line.contains("named")),
        "{page}"
    );
    assert!(
        !page.contains("at-home"),
        "the home's document was read:\n{page}"
    );
    assert!(
        layers(&page).contains(&("sessions.root".to_string(), "file".to_string())),
        "{page}"
    );

    for args in [
        vec!["--settings", "named.yaml", "sessions", "--json"],
        vec!["sessions", "--settings", "named.yaml", "--json"],
    ] {
        let out = run(dir.path(), &args, &env);

        assert!(out.status.success(), "`{args:?}`: {}", stderr(&out));
        let listed: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("one JSON object");
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .expect("the sessions")
            .iter()
            .map(|session| session["session_id"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(ids, vec!["flagged"], "`{args:?}`: {}", stdout(&out));
    }
}

/// TC-CLI-CONF-12: `--settings` naming a path with nothing there.
/// Expected: exit 1 and the path, from every subcommand that boots, and
/// nothing done. A document nobody named may be absent, because a first run
/// has none; a path the user typed is a path they typed because something is
/// in it, and reading the compiled defaults instead would run a harness they
/// did not configure and say nothing about it.
///
/// Environmental needs: `TETANUS_HOME` names an empty directory, so the only
/// document in the case is the one that is not there.
#[test]
fn a_named_document_that_is_not_there_stops_the_command() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    for args in [
        vec!["config"],
        vec!["sessions"],
        vec!["run", "-p", "hi"],
        vec!["replay", "turn"],
    ] {
        let args = [vec!["--settings", "gone.yaml"], args].concat();
        let out = run(dir.path(), &args, &env);

        assert_eq!(out.status.code(), Some(1), "`{args:?}`: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "`{args:?}` printed a page");
        assert_eq!(
            stderr(&out).matches("gone.yaml").count(),
            1,
            "`{args:?}`: {}",
            stderr(&out)
        );
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("read").count(),
        0,
        "a refused command left something behind"
    );
}

/// TC-CLI-CONF-14: `tetanus config --defaults`, against a document that sets
/// keys and then against one that does not parse.
/// Expected: one page both times - every row on the `default` layer, nothing
/// of the document on it, and exit 0 even where the plain page exits 1. The
/// question `--defaults` answers is about the build rather than the machine,
/// so a document is not read to answer it, and the moment a reader most needs
/// the answer is the moment their own document is the thing that is broken.
///
/// The page also says what it is not, on stderr, so the bytes a script reads
/// are the same bytes the other page gives it.
///
/// Environmental needs: `TETANUS_HOME` names a directory holding, in turn,
/// each of the two documents.
#[test]
fn the_defaults_page_reads_no_document_at_all() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let document = home.path().join("settings.yaml");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    std::fs::write(&document, "sessions:\n  root: documented\n").expect("the document is written");
    let set = run(
        dir.path(),
        &["config", "--defaults", "--color", "never"],
        &env,
    );
    std::fs::write(&document, "sessions: [1, 2\n").expect("the document is written");
    let broken = run(
        dir.path(),
        &["config", "--defaults", "--color", "never"],
        &env,
    );

    for (case, out) in [("a document", &set), ("a broken document", &broken)] {
        assert!(out.status.success(), "{case}: {}", stderr(out));
        let page = stdout(out);
        assert!(
            !page.contains("documented"),
            "{case} reached the page:\n{page}"
        );
        assert!(
            layers(&page).iter().all(|(_, layer)| layer == "default"),
            "{case}: a row came off something else:\n{page}"
        );
        assert!(
            stderr(out).contains("not what it will run on"),
            "{case} did not say what the page is not: {}",
            stderr(out)
        );
    }
    assert_eq!(stdout(&set), stdout(&broken), "the two pages differ");
    // The plain page against that same document is the failure this one is
    // asked instead of.
    let plain = run(dir.path(), &["config", "--color", "never"], &env);
    assert_eq!(plain.status.code(), Some(1), "{}", stderr(&plain));
}

/// TC-CLI-CONF-15: the two other shapes of `--defaults`.
/// Expected: `--json` carries the keys and layers the page carries, as it does
/// for the page this one is a variant of; and `--dir` with it is a usage
/// error, because a flag that overrides a setting and a page that reads no
/// settings are two questions, and answering one while being asked both would
/// print a `flag` row on a page whose whole claim is that nothing was set.
#[test]
fn the_defaults_page_answers_json_and_refuses_a_flag_layer() {
    let dir = tempfile::tempdir().expect("temp dir");

    let page = stdout(&run(
        dir.path(),
        &["config", "--defaults", "--color", "never"],
        &[],
    ));
    let json = stdout(&run(dir.path(), &["config", "--defaults", "--json"], &[]));
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("one JSON object");
    let told: Vec<(String, String)> = parsed["entries"]
        .as_array()
        .expect("the entries")
        .iter()
        .map(|entry| {
            (
                entry["key"].as_str().unwrap_or_default().to_string(),
                entry["layer"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(told, layers(&page), "{json}\n{page}");

    let both = run(
        dir.path(),
        &["config", "--defaults", "--dir", "elsewhere"],
        &[],
    );
    assert_eq!(both.status.code(), Some(2), "{}", stderr(&both));
    assert_eq!(stdout(&both), "", "it printed a page anyway");
    assert!(
        stderr(&both).contains("cannot be used with"),
        "{}",
        stderr(&both)
    );
}

/// TC-CLI-CONF-16: which document the page says it read, in the four states a
/// reader meets it in.
/// Expected: the heading carries the document beside the title - the one under
/// the harness home, the one `--settings` named written out in full even
/// though it was typed relative, and the same path marked when nothing is
/// there yet. `--defaults` carries no document at all, because it read none.
///
/// The question this page is opened with is "why is it that, and where do I
/// change it", and the second half of it is unanswerable from the rows: a
/// `file` layer says a document won, not which document. All four headings
/// are asserted whole, because a heading that named the wrong file would be
/// worse than one that named none.
///
/// Environmental needs: `TETANUS_HOME` names a directory of the case's own,
/// holding a document for the first state and nothing for the third.
#[test]
fn the_config_page_names_the_document_it_read() {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = tempfile::tempdir().expect("temp dir");
    let env = [("TETANUS_HOME", home.path().display().to_string())];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let document = home.path().join("settings.yaml");
    let heading = |args: &[&str]| {
        let page = stdout(&run(dir.path(), args, &env));
        page.lines().nth(1).unwrap_or_default().to_string()
    };

    std::fs::write(&document, "sessions:\n  root: documented\n").expect("the document");
    assert_eq!(
        heading(&["config", "--color", "never"]),
        format!("config  {}", document.display())
    );

    std::fs::write(dir.path().join("named.yaml"), "sessions:\n  root: named\n")
        .expect("the document");
    assert_eq!(
        heading(&["--settings", "named.yaml", "config", "--color", "never"]),
        format!("config  {}/named.yaml", here(&dir).display()),
        "a relative path was printed as it was typed"
    );

    std::fs::remove_file(&document).expect("the document goes");
    assert_eq!(
        heading(&["config", "--color", "never"]),
        format!("config  {} (not there yet)", document.display())
    );

    assert_eq!(
        heading(&["config", "--defaults", "--color", "never"]),
        "config"
    );
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

/// TC-CLI-UI-30: a step budget of zero, on both commands that take one.
/// Expected: exit 2 and a sentence saying why, from the flag rather than from
/// the run. A budget is spent by taking a step and checked afterwards, so a
/// turn always takes at least one: accepted, `--max-steps 0` produces a
/// journal that records a step the command line said it could not have, and
/// closes it `step budget spent`. One is still accepted, because one step
/// with no tool call answered is a real thing to ask for.
#[test]
fn a_step_budget_of_zero_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");

    for command in [
        vec![
            "run",
            "--max-steps",
            "0",
            "-p",
            "hi",
            "--session",
            "j.jsonl",
        ],
        vec!["chat", "--max-steps", "0", "--session", "c.jsonl"],
    ] {
        let refused = run(dir.path(), &command, &[]);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{} {}",
            stdout(&refused),
            stderr(&refused)
        );
        assert!(
            stderr(&refused).contains("greater than zero"),
            "{}",
            stderr(&refused)
        );
        assert!(
            stderr(&refused).contains("at least one step"),
            "{}",
            stderr(&refused)
        );
    }

    let one = run(
        dir.path(),
        &[
            "run",
            "--max-steps",
            "1",
            "-p",
            "hi",
            "--session",
            "j.jsonl",
        ],
        &[],
    );
    assert_eq!(one.status.code(), Some(0), "{}", stderr(&one));
}

/// TC-CLI-UI-31: the two crossings this binary no longer makes for itself.
/// Expected: a stop reason the contract does not name as a variant crosses as
/// the engine's own word for it, and a journal that cannot be read reports the
/// code §4.5 gives it. Both mappings are the engine's and published
/// (`convert::stop_reason`, `convert::journal_error`); this case is what says
/// the surface still answers the same way now that it only calls them.
#[test]
fn what_the_engine_maps_is_what_the_page_reports() {
    let dir = tempfile::tempdir().expect("temp dir");

    // A budget of one stops the turn on a reason the contract names.
    let spent = run(
        dir.path(),
        &[
            "run",
            "--max-steps",
            "1",
            "-p",
            "echo this",
            "--session",
            "j.jsonl",
            "--json",
        ],
        &[],
    );
    assert!(spent.status.success(), "{}", stderr(&spent));
    let last = stdout(&spent);
    let last = last.lines().last().expect("a result").to_string();
    let result: serde_json::Value = serde_json::from_str(&last).expect(&last);
    assert_eq!(result["summary"]["stop_reason"], "max-steps", "{last}");

    // And a journal that is not one is `LogCorrupt`, which §4.5 exits 1 for.
    std::fs::write(dir.path().join("bad.jsonl"), "not json\n").expect("write");
    let corrupt = run(dir.path(), &["replay", "bad.jsonl"], &[]);
    assert_eq!(corrupt.status.code(), Some(1), "{}", stderr(&corrupt));
    assert!(
        stderr(&corrupt).contains("not readable at line 1"),
        "{}",
        stderr(&corrupt)
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

/// TC-CLI-UI-17: the phase line, with the model named from outside.
/// Expected: the line still says what is being done and still names the model
/// readably, and carries none of the sequence the name held - under both
/// colour settings, because the one that promises no colour is the one a
/// sequence written this way would arrive under anyway. TC-CLI-UI-7 asserts
/// the same line holds no escape at all under a default model; this is the
/// case where the value came from a flag, which is also where a config file's
/// value arrives.
#[test]
fn a_model_named_from_outside_cannot_drive_the_terminal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nasty = format!("mo{ESC}[2Jck{ESC}]0;pwned\u{7}");

    for colour in ["never", "always"] {
        let out = run(
            dir.path(),
            &["run", "-p", "hi", "--model", &nasty, "--color", colour],
            &[],
        );
        let err = stderr(&out);
        assert!(out.status.success(), "{err}");
        assert!(err.contains("running the turn on"), "{err:?}");
        assert!(
            err.contains("mock"),
            "the name stopped being readable: {err:?}"
        );
        assert!(!err.contains(&format!("{ESC}[2J")), "{err:?}");
        assert!(!err.contains(&format!("{ESC}]0;")), "{err:?}");
        assert!(!err.contains('\u{7}'), "{err:?}");
    }

    // And with no colour at all, the line is plain text through and through.
    let plain = run(
        dir.path(),
        &["run", "-p", "hi", "--model", &nasty, "--color", "never"],
        &[],
    );
    assert!(!stderr(&plain).contains(ESC), "{:?}", stderr(&plain));
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

/// TC-CLI-SESS-13: `tetanus sessions` on a directory two runs wrote into.
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

/// TC-CLI-SESS-14: the id the page prints against the journal it names.
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
/// Expected: exit 0 and a page that says what writes one, headed by the root
/// it looked in and marked as not there yet. An empty store is not a failure,
/// and a missing directory is the ordinary first-run state - but it is also
/// what the wrong root looks like, so the page names the one it read.
#[test]
fn an_empty_store_is_not_a_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = run(dir.path(), &["sessions", "--color", "never"], &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "\nsessions  {}/sessions (not there yet)\n\
             no sessions yet - tetanus run writes one\n",
            here(&dir).display()
        )
    );
    assert_eq!(
        stdout(&run(dir.path(), &["sessions", "--json"], &[])),
        "{\"sessions\":[]}\n"
    );
}

/// TC-CLI-SESS-15: which root the listing says it read, against the three
/// things that can choose it.
/// Expected: the heading names the directory that was actually listed - the
/// compiled default, the one the settings document set, and the one `--dir`
/// overrode it with - each written out in full. `--json` is unchanged, because
/// a caller that asked for the machine form already knows the root it passed.
///
/// A listing under the wrong root reads exactly like a listing under the right
/// one, so this is the line that tells the two apart. It is asserted for a
/// page with rows on it as well as an empty one, because the empty page is
/// where a reader most needs it and the full page is where it is easiest to
/// leave out.
///
/// Environmental needs: `TETANUS_HOME` is the case's own directory, and the
/// document under it sets a root the flag then overrides.
#[test]
fn the_listing_names_the_root_it_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let heading = |args: &[&str]| {
        let page = stdout(&run(dir.path(), args, &[]));
        page.lines().nth(1).unwrap_or_default().to_string()
    };
    let out = run(
        dir.path(),
        &["run", "-p", "echo this", "--session", "sessions/a.jsonl"],
        &[],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let root = format!("{}/sessions", here(&dir).display());
    assert_eq!(
        heading(&["sessions", "--color", "never"]),
        format!("sessions  {root}")
    );

    // A root the document named has to be there - the engine refuses to read
    // a caller's typo as an empty history - so this one is made before it is
    // listed.
    std::fs::create_dir(dir.path().join("documented")).expect("the root is made");
    std::fs::write(
        dir.path().join("settings.yaml"),
        "sessions:\n  root: documented\n",
    )
    .expect("the document is written");
    assert_eq!(
        heading(&["sessions", "--color", "never"]),
        format!("sessions  {}/documented", here(&dir).display()),
        "the document did not choose the root"
    );
    assert_eq!(
        heading(&["sessions", "--dir", "sessions", "--color", "never"]),
        format!("sessions  {root}"),
        "the flag did not beat the document"
    );

    assert_eq!(
        stdout(&run(
            dir.path(),
            &["sessions", "--dir", "sessions", "--json"],
            &[]
        ))
        .lines()
        .count(),
        1,
        "the machine form grew a heading"
    );
}

/// TC-CLI-SESS-10: the id `tetanus sessions` printed, typed into
/// `tetanus replay`.
/// Expected: exit 0 and the journal that id names, for the bare id and for the
/// id with the extension it is listed under. The page a reader takes an id off
/// and the command they retype it into are the pair this case exists for: the
/// note on a missing journal sends them to that page, so a page whose ids the
/// next command refuses is a loop with no way out of it.
#[test]
fn an_id_the_session_list_printed_is_one_replay_opens() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("settings.yaml"),
        "sessions:\n  root: journals\n",
    )
    .expect("the document is written");
    for prompt in ["echo this", "and again"] {
        let out = run(
            dir.path(),
            &[
                "run",
                "-p",
                prompt,
                "--session",
                &format!("journals/{}.jsonl", prompt.replace(' ', "-")),
            ],
            &[],
        );
        assert!(out.status.success(), "{}", stderr(&out));
    }

    let listed = stdout(&run(dir.path(), &["sessions", "--color", "never"], &[]));
    let mut ids: Vec<String> = listed
        .lines()
        .skip(2)
        .filter_map(|row| row.split_whitespace().next().map(str::to_string))
        .collect();
    ids.sort();

    assert_eq!(ids, vec!["and-again", "echo-this"], "{listed}");
    for id in &ids {
        let told = stdout(&run(dir.path(), &["replay", id, "--color", "never"], &[]));
        assert!(told.contains("session on mock-echo-1"), "`{id}`: {told}");
        let with_extension = stdout(&run(
            dir.path(),
            &["replay", &format!("{id}.jsonl"), "--color", "never"],
            &[],
        ));
        assert_eq!(with_extension, told, "`{id}.jsonl` read something else");
    }
}

/// TC-CLI-SESS-11: a target that is a path, against a document whose root
/// holds a journal of the same name, and `--dir` over the document.
/// Expected: the path is opened as it was given - a journal the user can see
/// is the one they meant, whatever a document says about roots - and an id is
/// looked up under `--dir` when one is passed.
#[test]
fn a_path_is_opened_as_given_and_a_flag_says_where_an_id_lives() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("settings.yaml"),
        "sessions:\n  root: journals\n",
    )
    .expect("the document is written");
    for (prompt, path) in [
        ("under the root", "journals/same.jsonl"),
        ("beside it", "same.jsonl"),
        ("somewhere else", "elsewhere/other.jsonl"),
    ] {
        let out = run(dir.path(), &["run", "-p", prompt, "--session", path], &[]);
        assert!(out.status.success(), "{}", stderr(&out));
    }

    let told = stdout(&run(
        dir.path(),
        &["replay", "same.jsonl", "--color", "never"],
        &[],
    ));
    assert!(
        told.contains("beside it"),
        "the root's copy was opened: {told}"
    );

    let by_id = stdout(&run(
        dir.path(),
        &["replay", "same", "--color", "never"],
        &[],
    ));
    assert!(by_id.contains("under the root"), "{by_id}");

    let flagged = stdout(&run(
        dir.path(),
        &["replay", "other", "--dir", "elsewhere", "--color", "never"],
        &[],
    ));
    assert!(flagged.contains("somewhere else"), "{flagged}");
}

/// TC-CLI-SESS-12: a target that is neither a path nor an id.
/// Expected: exit 4, `SessionNotFound` in the contract's table; the message
/// names what was typed rather than a path nobody typed; the way out names the
/// root it was looked for under, because the reader typed an id and either the
/// id is wrong or the root is; and nothing is printed on stdout.
#[test]
fn a_target_that_is_nothing_names_the_root_it_was_looked_for_under() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("settings.yaml"),
        "sessions:\n  root: journals\n",
    )
    .expect("the document is written");

    let out = run(dir.path(), &["replay", "nope", "--color", "never"], &[]);

    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "a missing journal printed a page");
    let told = said(&out);
    assert!(told.contains("no journal at nope"), "{told}");
    assert!(told.contains("journals"), "{told}");
    assert!(told.contains("tetanus sessions"), "{told}");
}

/// TC-CLI-ERR-12: a provider that answers nothing.
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

/// TC-CLI-ERR-13: a provider that answers a status.
/// Expected: exit 6 again, and the status said out loud - `deepseek answered
/// 503`, not the sentence a provider that never answered gets. The two are one
/// code and two different situations, and the number is the difference between
/// "wait and retry" and "something here is wrong". It is also the only arm of
/// the mapping that carries a number into `data`, so it is the one that proves
/// the page reads `data` and not the message it arrived with.
///
/// Environmental needs: none. The provider is a socket on a loopback port this
/// case opens itself, so nothing leaves the machine and no key is used.
#[test]
fn a_provider_that_answers_a_status_says_the_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    // Every attempt is answered, because the route may make more than one.
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = std::io::Read::read(&mut stream, &mut [0u8; 1024]);
            let _ = std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });

    let out = run(
        dir.path(),
        &["run", "--adapter", "deepseek", "--session", "p.jsonl"],
        &[
            ("DEEPSEEK_API_KEY", "sk-not-a-real-key"),
            ("DEEPSEEK_BASE_URL", &base),
        ],
    );

    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("deepseek answered 503"), "{err}");
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
        let err = said(&out);
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

/// TC-CLI-ERR-15: an empty value, on every flag that takes one.
/// Expected: clap's usage error and exit 2 from all six, and nothing done.
/// The three paths already refused it because clap refuses an empty `PathBuf`;
/// the three that stay text did not, and each carried the empty string
/// somewhere further on - a run announced itself on a model with no name and
/// wrote that name into the journal header, `replay` reported a journal
/// missing when none had been named, and `serve` said `: invalid socket
/// address`. One mistake now reads one way.
#[test]
fn a_value_that_names_nothing_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");

    for (args, named) in [
        (vec!["run", "--model", "", "-p", "hi"], "--model <ID>"),
        (vec!["replay", ""], "<JOURNAL>"),
        (vec!["serve", "--listen", ""], "--listen <ADDR>"),
        // The three that were already refused, asserted here so that the six
        // cannot drift back into two answers for one mistake.
        (vec!["run", "--session", "", "-p", "hi"], "--session <PATH>"),
        (vec!["sessions", "--dir", ""], "--dir <PATH>"),
        (vec!["--settings", "", "config"], "--settings <PATH>"),
    ] {
        let out = run(dir.path(), &args, &[]);

        assert_eq!(out.status.code(), Some(2), "`{args:?}`: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "`{args:?}` printed a page");
        let err = stderr(&out);
        assert!(err.contains(named), "`{args:?}` did not name it: {err}");
        assert!(err.contains("a value is required"), "`{args:?}`: {err}");
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("read").count(),
        0,
        "a refused run left something behind"
    );
}

/// TC-CLI-ERR-16: a journal path that is a directory, on the two views that
/// open one to write and the one that opens it to read.
/// Expected: one sentence for all three - `held: is a directory`, exit 1 - so
/// a reader who typed the wrong path is told which path it was, whichever
/// command they typed it on. The write side used to print what the operating
/// system said with nothing in front of it, which named no file at all on a
/// page whose whole subject is a file, while the read side named it. The
/// errno is gone from the page as well: `(os error 21)` says a second time
/// what the three words in front of it have said, and the number a script
/// reads is the exit status.
///
/// Environmental needs: none. `chat` is given the mock adapter and no key, so
/// it fails on the journal rather than on a credential, and it reads no
/// stdin because it never gets as far as the prompt.
#[test]
fn a_journal_that_is_a_directory_is_named_by_every_view() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join("held")).expect("a directory");

    for args in [
        vec!["run", "--session", "held", "-p", "hi"],
        vec!["chat", "--adapter", "mock", "--session", "held"],
        vec!["replay", "held"],
    ] {
        let out = run(dir.path(), &args, &[]);

        assert_eq!(out.status.code(), Some(1), "`{args:?}`: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("held: is a directory"), "`{args:?}`: {err}");
        assert!(!err.contains("os error"), "`{args:?}`: {err}");
    }
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

/// TC-CLI-WEB-1: `tetanus web` pointed at a directory with no page in it.
/// Expected: exit 1, the directory named, and nothing bound. A server that
/// came up on the address a person was about to open and then answered every
/// request with "no index.html" is a worse failure than not coming up.
#[test]
fn the_web_panel_refuses_a_frontend_that_is_not_there() {
    let dir = tempfile::tempdir().expect("temp dir");

    let refused = run(
        dir.path(),
        &["web", "--frontend", "nowhere", "--listen", "127.0.0.1:0"],
        &[],
    );

    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("nowhere"), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("index.html"),
        "{}",
        stderr(&refused)
    );
}

/// TC-CLI-WEB-2: an address this server will not bind.
/// Expected: refused, with the two it will bind named. There is no TLS here,
/// no authentication and no origin policy, so a third address would read as an
/// option this server had thought about.
#[test]
fn the_web_panel_binds_loopback_or_the_wildcard() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("app")).expect("the frontend");
    std::fs::write(dir.path().join("app/index.html"), "<html></html>").expect("the page");

    let refused = run(
        dir.path(),
        &["web", "--frontend", "app", "--listen", "192.168.1.10:5300"],
        &[],
    );

    assert_ne!(refused.status.code(), Some(0), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("127.0.0.1 or 0.0.0.0"),
        "{}",
        stderr(&refused)
    );
}

/// The bridge, asked one question over plain HTTP.
fn over_http(port: u16, method: &str, kind: &str, body: &str) -> (u16, String) {
    over_http_at(port, method, kind, body, None)
}

/// The same, with an `authorization` header when one is given.
fn over_http_at(
    port: u16,
    method: &str,
    kind: &str,
    body: &str,
    authorization: Option<&str>,
) -> (u16, String) {
    use std::io::{Read, Write};
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).expect("it connects");
    let authorization = match authorization {
        Some(value) => format!("authorization: {value}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "POST /api/{method} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: {kind}\r\n{authorization}content-length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).expect("written");
    let mut said = String::new();
    socket.read_to_string(&mut said).expect("read");
    let status = said
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = said.split_once("\r\n\r\n").map(|(_, it)| it).unwrap_or("");
    (status, body.to_string())
}

/// TC-CLI-WEB-5: the `/api` bridge - a media type that is not JSON, the
/// handshake, a call after it, and a method this build does not have.
/// Expected: 415 before dispatch for the media type, because a cross-site
/// "simple" post must never execute a side-effectful method blind; 200 with
/// the contract's own envelope for the rest, including the unknown method,
/// whose failure is the engine's answer and not a fault of the carrier.
#[test]
fn the_api_bridge_answers_the_published_contract_over_http() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("app")).expect("the frontend");
    std::fs::write(
        dir.path().join("app/index.html"),
        "<html><head></head></html>",
    )
    .expect("page");

    let mut served = std::process::Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir.path())
        .args(["web", "--frontend", "app", "--listen", "127.0.0.1:5399"])
        .env("TETANUS_HOME", dir.path())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the binary runs");
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let (refused, _) = over_http(5399, "rpc.hello", "text/plain", "{}");
    let (greeted, hello) = over_http(
        5399,
        "rpc.hello",
        "application/json",
        r#"{"protocol_version":"1.0","client":{"name":"case","version":"1"}}"#,
    );
    let (listed, sessions) = over_http(5399, "session.list", "application/json", "{}");
    let (unknown, nothing) = over_http(5399, "nope.nope", "application/json", "{}");
    served.kill().ok();
    served.wait().ok();

    assert_eq!(refused, 415, "a media type that is not JSON is refused");
    assert_eq!(greeted, 200, "{hello}");
    assert!(hello.contains("protocol_version"), "{hello}");
    assert_eq!(listed, 200, "{sessions}");
    assert!(sessions.contains("\"sessions\""), "{sessions}");
    // The carrier worked; the method did not exist. Those are different facts
    // and they are reported in different places.
    assert_eq!(unknown, 200, "{nothing}");
    assert!(nothing.contains("-32601"), "{nothing}");
}

/// TC-CLI-WEB-6: the host's own methods on the bridge - a listing, a
/// creation, one that is already there, and a path that is not qualified.
/// Expected: the picker's three failures arrive as codes with the subject path
/// in `data`, because a chooser saying "cannot be read" with nothing named is
/// a dialog the reader cannot argue with. The carrier says 200 throughout: the
/// filesystem refusing is an answer, not a transport fault.
#[test]
fn the_bridge_answers_the_host_methods_too() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("app")).expect("the frontend");
    std::fs::write(
        dir.path().join("app/index.html"),
        "<html><head></head></html>",
    )
    .expect("page");
    std::fs::create_dir(dir.path().join("already")).expect("a directory");

    let mut served = std::process::Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir.path())
        .args(["web", "--frontend", "app", "--listen", "127.0.0.1:5398"])
        .env("TETANUS_HOME", dir.path())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the binary runs");
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let root = dir.path().display().to_string();
    let (listed, listing) = over_http(
        5398,
        "host.listDirectory",
        "application/json",
        &format!(r#"{{"path":"{root}"}}"#),
    );
    let (made, created) = over_http(
        5398,
        "host.createDirectory",
        "application/json",
        &format!(r#"{{"path":"{root}","name":"fresh"}}"#),
    );
    let (again, exists) = over_http(
        5398,
        "host.createDirectory",
        "application/json",
        &format!(r#"{{"path":"{root}","name":"already"}}"#),
    );
    let (relative, unqualified) = over_http(
        5398,
        "host.listDirectory",
        "application/json",
        r#"{"path":"not/absolute"}"#,
    );
    served.kill().ok();
    served.wait().ok();

    assert_eq!(listed, 200, "{listing}");
    assert!(listing.contains("\"crumbs\""), "{listing}");
    assert!(listing.contains("already"), "{listing}");
    // A listing is directories only, so the frontend directory is in it and
    // its index.html is not.
    assert!(!listing.contains("index.html"), "{listing}");

    assert_eq!(made, 200, "{created}");
    assert!(dir.path().join("fresh").is_dir(), "{created}");

    assert_eq!(again, 200, "{exists}");
    assert!(
        exists.contains("-32602"),
        "already there is a bad argument: {exists}"
    );
    assert!(
        exists.contains("already"),
        "the subject path is missing: {exists}"
    );

    assert_eq!(relative, 200, "{unqualified}");
    assert!(unqualified.contains("-32009"), "{unqualified}");
    assert!(unqualified.contains("not/absolute"), "{unqualified}");
}

/// TC-CLI-WEB-7: the bridge under a stated token.
/// Expected: a POST with no token is 401 and never reaches the JSON-RPC layer,
/// one with the token in the query is answered, and so is one presenting it as
/// a bearer header. A door with a lock beside a door without one is a room
/// with no lock: this carrier reaches the whole engine exactly as the socket
/// does, so the posture has to be the same on both.
#[test]
fn the_bridge_is_locked_the_way_the_socket_is() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("app")).expect("the frontend");
    std::fs::write(
        dir.path().join("app/index.html"),
        "<html><head></head></html>",
    )
    .expect("page");

    let mut served = std::process::Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir.path())
        .args([
            "web",
            "--frontend",
            "app",
            "--listen",
            "127.0.0.1:5397",
            "--token",
            "a-stated-secret",
        ])
        .env("TETANUS_HOME", dir.path())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the binary runs");
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let hello = r#"{"protocol_version":"1.0","client":{"name":"case","version":"1"}}"#;
    let (bare, _) = over_http(5397, "rpc.hello", "application/json", hello);
    let (with_query, greeted) = over_http_at(
        5397,
        "rpc.hello?token=a-stated-secret",
        "application/json",
        hello,
        None,
    );
    let (with_header, _) = over_http_at(
        5397,
        "session.list",
        "application/json",
        "{}",
        Some("Bearer a-stated-secret"),
    );
    // The page is not the protocol: it stays readable, which is what makes the
    // token deliverable to a reader who was given the URL.
    let page = std::net::TcpStream::connect(("127.0.0.1", 5397)).is_ok();
    served.kill().ok();
    served.wait().ok();

    assert_eq!(bare, 401, "an unauthenticated POST reached the engine");
    assert_eq!(with_query, 200, "{greeted}");
    assert!(greeted.contains("protocol_version"), "{greeted}");
    assert_eq!(with_header, 200, "a bearer token was not accepted");
    assert!(page, "the page stopped being served");
}
