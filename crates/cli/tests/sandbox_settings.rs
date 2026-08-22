//! Test Design Specification: the settings key that confines this deployment.
//!
//! Feature under test: `sandbox.mode`, `sandbox.workspace` and
//! `sandbox.network` from the settings document, through
//! `EngineConfig::from_settings`, into every child the binary starts. Features
//! NOT tested here: what Landlock enforces (owned by `crates/sandbox`, proven
//! there by denial), and how a denial is rendered (owned by `crates/exec`).
//!
//! Why the binary: the seams have taken a policy since the sandbox slice, but
//! a policy nothing configures is a policy nobody has. The claim here is that
//! one word in a document reaches the shell tool, a persistent shell and a
//! terminal alike - and it can only be made where those three are composed.
//!
//! The mode under test is `read-only`, because it is the one whose effect is
//! visible without arranging a directory outside every grant: a write into the
//! workspace itself is refused, which `workspace-write` would allow.
//!
//! Environmental needs: Linux with Landlock (ABI 1 or better), a bash on PATH,
//! a writable temp directory. Where the kernel cannot enforce the policy the
//! cases report themselves skipped rather than passing for the wrong reason.
//! No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

/// Whether this kernel can enforce what the cases configure.
fn enforceable() -> bool {
    tetanus_sandbox::support().is_ok_and(|support| support.abi.is_some_and(|abi| abi > 0))
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        .args(args)
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .expect("the binary runs")
}

/// Write a settings document naming a mode, and answer where it is.
fn document(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("settings.yaml");
    std::fs::write(&path, body).expect("wrote the document");
    path
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

/// TC-CLI-SANDBOX-1: a mode in the document confines the command a model runs.
///
/// The whole point of the key. Before it, a deployment could only be confined
/// by editing this binary: every tool configuration derived its own
/// `danger-full-access` default, and nothing read a document.
///
/// Input: a document setting `sandbox.mode: read-only`, and a run whose
/// command tries to write a file in the workspace.
/// Expected: the run itself succeeds - a denial is the command's result, not
/// the harness's failure - the file is not created, and the model is told this
/// was policy rather than a mistake in its command.
#[test]
fn a_mode_in_the_document_confines_the_command_a_model_runs() {
    if !enforceable() {
        eprintln!("skipped: this kernel cannot enforce a sandbox policy");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = document(dir.path(), "sandbox:\n  mode: read-only\n");

    let out = run(
        dir.path(),
        &[
            "run",
            "--settings",
            settings.to_str().expect("utf-8"),
            "--prompt",
            "!echo denied-me > refused.txt",
            "--session",
            "j.jsonl",
            "--color",
            "never",
        ],
    );

    assert!(
        out.status.success(),
        "a denial is a result, not a failed run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.path().join("refused.txt").exists(),
        "the command wrote a file the policy forbade"
    );
    let journal = std::fs::read_to_string(dir.path().join("j.jsonl")).expect("journal");
    let results = records(&journal, "tool/result");
    let content = results[0]["content"].as_str().expect("text");
    assert!(
        content.contains("[sandbox:") && content.contains("read-only"),
        "the model has to be told this was policy: {content:?}"
    );
}

/// TC-CLI-SANDBOX-2: what the confined command starts is confined too.
///
/// A policy that stopped at the command would stop at nothing: the first thing
/// a model does with a refused write is try it from a script, or from another
/// shell. Landlock is inherited across `exec`, so this is a property of
/// applying the policy to the child rather than to the call - and that is
/// exactly the kind of claim worth pinning from outside the crate that made
/// it.
///
/// That the *terminal* family gets the same policy value is asserted where the
/// terminal is (`crates/exec`, TC-PORT-SANDBOX-32): the offline adapter drives
/// `shell` and cannot open a terminal, and a case that pretended otherwise
/// would be asserting this file's fixture rather than the composition.
///
/// Input: the same `read-only` document; a run whose command writes through a
/// child shell of its own.
/// Expected: the file is not created.
#[test]
fn what_the_confined_command_starts_is_confined_too() {
    if !enforceable() {
        eprintln!("skipped: this kernel cannot enforce a sandbox policy");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = document(dir.path(), "sandbox:\n  mode: read-only\n");

    let out = run(
        dir.path(),
        &[
            "run",
            "--settings",
            settings.to_str().expect("utf-8"),
            "--prompt",
            "!bash -i -c 'echo through-a-shell > refused-too.txt' 2>&1 | head -3",
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
    assert!(
        !dir.path().join("refused-too.txt").exists(),
        "a child of the confined shell escaped the policy"
    );
}

/// TC-CLI-SANDBOX-3: the config page reports what this deployment is confined
/// to, and which layer said so.
///
/// A policy nobody can read is a policy nobody trusts. `tetanus config` is the
/// one answer to "what is this harness configured to do", so a key that
/// governs every child it starts has to be on it - with its provenance, since
/// "the document said `read-only`" and "the build defaults to
/// `danger-full-access`" are the two different things a reader is checking.
///
/// Input: `tetanus config` with a document naming a mode and a workspace, and
/// again with `--defaults`.
/// Expected: the page shows the configured mode against the file layer; the
/// defaults page shows `danger-full-access` against the default layer.
#[test]
fn the_config_page_reports_what_this_deployment_is_confined_to() {
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = document(
        dir.path(),
        "sandbox:\n  mode: workspace-write\n  workspace: /tmp/somewhere\n  network: false\n",
    );

    let page = String::from_utf8(
        run(
            dir.path(),
            &[
                "config",
                "--settings",
                settings.to_str().expect("utf-8"),
                "--color",
                "never",
            ],
        )
        .stdout,
    )
    .expect("utf-8");
    assert!(
        page.contains("sandbox.mode") && page.contains("workspace-write"),
        "the page has to say what confines this deployment:\n{page}"
    );
    assert!(
        page.contains("sandbox.workspace") && page.contains("/tmp/somewhere"),
        "and where its writable root is:\n{page}"
    );

    let defaults =
        String::from_utf8(run(dir.path(), &["config", "--defaults", "--color", "never"]).stdout)
            .expect("utf-8");
    assert!(
        defaults.contains("sandbox.mode") && defaults.contains("danger-full-access"),
        "a build nobody configured is unconfined, and says so:\n{defaults}"
    );
}

/// TC-CLI-SANDBOX-4: a mode this build does not know is refused, and nothing
/// runs.
///
/// The failure mode worth paying for. A deployment that wrote `read_only` with
/// an underscore meant to be confined; ignoring the value and running
/// unconfined would be the one outcome nobody would forgive, and it would look
/// exactly like a correct configuration until somebody audited it.
///
/// Input: a document naming a mode that does not exist.
/// Expected: a non-zero exit; the message names the key, quotes what was
/// written, lists the modes that exist, and names the file to edit; and no
/// journal is written, because nothing ran.
#[test]
fn a_mode_this_build_does_not_know_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = document(dir.path(), "sandbox:\n  mode: read_only\n");

    let out = run(
        dir.path(),
        &[
            "run",
            "--settings",
            settings.to_str().expect("utf-8"),
            "--prompt",
            "!echo should-never-run > ran.txt",
            "--session",
            "j.jsonl",
            "--color",
            "never",
        ],
    );

    assert!(
        !out.status.success(),
        "a misspelled mode must not run unconfined"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("sandbox.mode") && said.contains("read_only"),
        "the message has to name the key and quote what was written: {said}"
    );
    assert!(
        said.contains("read-only") && said.contains("danger-full-access"),
        "and list the modes that do exist: {said}"
    );
    assert!(
        !dir.path().join("ran.txt").exists() && !dir.path().join("j.jsonl").exists(),
        "nothing should have run"
    );
}
