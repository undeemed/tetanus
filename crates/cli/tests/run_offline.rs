//! Test Design Specification: the headless `tetanus run` command.
//!
//! Features tested: an offline turn on the built-in mock adapter, the printed
//! event sequence, the journal on disk, and the failure message when a real
//! provider has no credential. Features NOT tested here: the flow itself
//! (owned by the conformance suite in `tetanus-turn`) and any live provider.
//!
//! Environmental needs: none. Every case runs offline.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str], key: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir).args(args);
    match key {
        Some(value) => cmd.env("DEEPSEEK_API_KEY", value),
        None => cmd.env_remove("DEEPSEEK_API_KEY"),
    };
    cmd.output().expect("the binary runs")
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
    assert!(stdout.contains("stop    natural"), "{stdout}");
    assert!(stdout.contains("You said: run one full turn"), "{stdout}");

    let journal = dir.path().join("journal.jsonl");
    let events = tetanus_session::replay(&journal).expect("the journal replays");
    assert_eq!(events.first().expect("first").ty, "turn/start");
    assert_eq!(events.last().expect("last").ty, "turn/end");
}

/// TC-CLI-2: the same run is reproducible, so two runs print the same sequence.
/// Expected: byte-identical event sections.
#[test]
fn the_offline_run_is_reproducible() {
    let dir = tempfile::tempdir().expect("temp dir");
    let args = [
        "run",
        "--prompt",
        "same in, same out",
        "--session",
        "a.jsonl",
    ];
    let first = run(dir.path(), &args, None);
    let second = run(
        dir.path(),
        &[
            "run",
            "--prompt",
            "same in, same out",
            "--session",
            "b.jsonl",
        ],
        None,
    );

    let section = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .take_while(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(section(&first), section(&second));
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
