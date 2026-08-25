//! Test Design Specification: the binary offers the tools the workspace built.
//!
//! Feature under test: the shipped `tetanus` binary's tool registry - that
//! every landed tool crate reaches it, that the document can narrow it, and
//! that what the binary *lists* is what a turn *dispatches*.
//!
//! Why the binary and not the assembly: a crate can exist, be tested, and be
//! reachable from nothing. `crates/toolset` composes the sources correctly in
//! its own suite; that says nothing about whether the program people run wired
//! it up. Only a case that execs `tetanus` can tell those apart, which is why
//! every case here goes through the binary and reads its output.
//!
//! Features NOT tested here: what each tool does (owned by its own crate's
//! suite), the assembly rules - duplicates, ordering, attribution - (owned by
//! `crates/toolset/tests/assembly.rs`), and the shell path end to end (owned
//! by `shell.rs`, which this deliberately does not repeat).
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or needs a credential; the one case that runs a turn uses the offline mock.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        // The harness home is the case's own directory, so a settings document
        // on the machine running the suite cannot decide what is offered.
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .args(args)
        .output()
        .expect("the binary runs")
}

/// The tool names the binary says it offers, read off `tetanus tools --json`.
fn offered(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut call = vec!["tools", "--json"];
    call.extend_from_slice(args);
    let out = run(dir, &call);
    assert!(
        out.status.success(),
        "tetanus tools failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let page: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the tools page is one JSON object");
    let mut names: Vec<String> = page["tools"]
        .as_array()
        .expect("tools is a list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name").to_string())
        .collect();
    names.sort();
    names
}

fn document(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("settings.yaml");
    std::fs::write(&path, body).expect("write the document");
    path
}

/// A page with its soft wraps taken out.
///
/// A note is folded to the terminal's width, and a long `$TMPDIR` - which is
/// what a shared build machine has - puts the fold inside the path the case is
/// looking for. The path itself holds no whitespace, so joining each wrapped
/// line back onto the one before it restores exactly what was printed and
/// changes nothing else the case reads.
fn unwrapped(page: &str) -> String {
    page.lines()
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("")
}

/// TC-CLI-TOOLSET-1: every landed tool crate's tools are offered by the binary.
///
/// The acceptance this slice exists for, and the one a green workspace does not
/// give you: before it, `crates/fs`, `crates/features`, `crates/mcp` and
/// `crates/web` were all landed, all tested, and none of their tools could be
/// called from `tetanus`, because the binary composed `echo` and the shell
/// tools by hand and nothing else.
///
/// Input: `tetanus tools --json` on a harness home with no document.
/// Expected: a representative tool from each landed source that is on by
/// default - `echo` from builtin, `shell` from exec, the seven file tools from
/// fs, and the feature tools - all present in one listing.
#[test]
fn every_landed_tool_crate_is_offered_by_the_binary() {
    let home = tempfile::tempdir().expect("temp dir");

    let names = offered(home.path(), &[]);

    for expected in [
        // builtin
        "echo",
        // exec
        "shell",
        "shell_open",
        "shell_run",
        "shell_close",
        "shell_list",
        // fs: all seven, because `FsTools` registers them as one unit and a
        // partial composition is the failure it is shaped to prevent.
        "read",
        "write",
        "edit",
        "list",
        "glob",
        "stat",
        "delete",
        // features
        "todo_write",
        "get_goal",
        "update_goal",
        "exit_plan_mode",
        "report_feedback",
        "skill",
        "workspace_info",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "the binary does not offer {expected:?}; it offers {names:?}"
        );
    }
}

/// TC-CLI-TOOLSET-2: what the binary lists is what a turn dispatches.
///
/// The listing and the registry a turn runs on are built from one function, and
/// this is the case that keeps that true from outside. A binary whose `tools`
/// page and whose turns disagreed would offer the model a tool that is not
/// there, which it discovers by spending a step on it.
///
/// Input: a turn whose prompt asks the offline adapter to run a command, on the
/// same harness home the listing was read from.
/// Expected: `shell` is in the listing, and the turn actually runs the command
/// and carries its output - so the tool named on the page is the tool the turn
/// found.
#[test]
fn a_tool_the_page_lists_is_a_tool_a_turn_can_dispatch() {
    let home = tempfile::tempdir().expect("temp dir");

    assert!(offered(home.path(), &[]).contains(&"shell".to_string()));

    let out = run(home.path(), &["run", "!echo dispatched-through-the-binary"]);

    assert!(
        out.status.success(),
        "the turn failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("dispatched-through-the-binary"),
        "the command's output did not reach the turn: {said}"
    );
}

/// TC-CLI-TOOLSET-3: `tools.sources` narrows what the binary offers.
///
/// A deployment turns a crate's tools off by naming the crate, not by naming
/// fifteen tools. This is that reaching the program rather than only the
/// assembly.
///
/// Input: a document naming `builtin` and `fs`.
/// Expected: exactly those two sources' tools - `echo` and the nine file tools
/// - and no shell or feature tool.
#[test]
fn the_document_narrows_what_the_binary_offers_by_source() {
    let home = tempfile::tempdir().expect("temp dir");
    document(home.path(), "tools:\n  sources: [builtin, fs]\n");

    let names = offered(home.path(), &[]);

    assert_eq!(
        names,
        [
            "delete",
            "echo",
            "edit",
            "glob",
            "list",
            "read",
            "read_image",
            "search",
            "stat",
            "write",
        ],
        "only the two named sources compose"
    );
}

/// TC-CLI-TOOLSET-4: naming a source this build does not ship is refused
/// against the document.
///
/// A misspelled source must not silently produce a harness with fewer tools
/// than was asked for: that failure surfaces later as a model that cannot read
/// a file, and nothing connects it back to the typo. It is also the case that
/// caught this slice composing the registry behind an `expect` - a bad document
/// panicked instead of being reported.
///
/// Input: `tools.sources` naming a source that does not exist.
/// Expected: exit 2 - §4.5's status for a value the harness will not accept -
/// naming the key, the bad word, the sources that do exist, and the document
/// that set it. Nothing panics.
#[test]
fn a_source_this_build_does_not_ship_is_refused_against_the_document() {
    let home = tempfile::tempdir().expect("temp dir");
    let path = document(home.path(), "tools:\n  sources: [builtin, nope]\n");

    let out = run(home.path(), &["tools"]);

    assert_eq!(out.status.code(), Some(2), "a bad value exits 2");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("tools.sources"), "names the key: {said}");
    assert!(said.contains("nope"), "names the bad word: {said}");
    assert!(said.contains("fs"), "names what does exist: {said}");
    assert!(
        unwrapped(&said).contains(&path.display().to_string()),
        "names the document: {said}"
    );
    assert!(!said.contains("panicked"), "reported, not panicked: {said}");
}

/// TC-CLI-TOOLSET-5: the tools that leave the machine are off until the
/// document turns them on.
///
/// The posture `crates/web` already took for itself, holding at the binary: a
/// harness whose first run in a sandbox quietly fetched a URL a model invented
/// would be a surprise nobody asked for. The `web` source is always declared,
/// so `tools.sources` names the same set on every host - it just carries
/// nothing until it is configured.
///
/// Input: the default listing, then one with `web.tools.*` set.
/// Expected: no `web_fetch` or `web_search` by default, both present after.
#[test]
fn the_web_tools_are_off_until_the_document_turns_them_on() {
    let home = tempfile::tempdir().expect("temp dir");

    let closed = offered(home.path(), &[]);
    assert!(!closed.contains(&"web_fetch".to_string()), "{closed:?}");
    assert!(!closed.contains(&"web_search".to_string()), "{closed:?}");

    document(
        home.path(),
        "web:\n  tools:\n    fetch: true\n    search: true\n",
    );
    let opened = offered(home.path(), &[]);

    assert!(opened.contains(&"web_fetch".to_string()), "{opened:?}");
    assert!(opened.contains(&"web_search".to_string()), "{opened:?}");
}

/// TC-CLI-TOOLSET-6: `tetanus info` counts the tools `tetanus tools` lists.
///
/// Two surfaces reading one assembly. They were already meant to agree - the
/// comment in `main.rs` says so - and now that the number moves with the
/// document, a case holds them together.
///
/// Input: a document narrowing the sources, then `info` and `tools` on it.
/// Expected: the count `info` prints is the length of the list `tools` prints.
#[test]
fn info_counts_the_tools_the_page_lists() {
    let home = tempfile::tempdir().expect("temp dir");
    document(home.path(), "tools:\n  sources: [builtin, fs]\n");

    let listed = offered(home.path(), &[]).len();
    let out = run(home.path(), &["info"]);

    assert!(out.status.success());
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains(&format!("{listed}")),
        "info should count {listed} tools: {said}"
    );
}
