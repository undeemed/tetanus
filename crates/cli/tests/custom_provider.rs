//! Test Design Specification: a provider a settings document declares, run
//! from the command line.
//!
//! Features tested: `--adapter <name>` resolving a route out of the registry
//! the document composed, `tetanus models` listing it, the alias that keeps
//! `--adapter deepseek` working, and the three ways naming a provider is
//! refused - a name nothing serves, a route whose credential is absent, and a
//! route that advertises no model to default to. One full turn is driven end
//! to end against a stub endpoint, and one piped `tetanus chat` turn over the
//! same stub, because `run` and `chat` resolve a provider through the same
//! function and a case that covered one would say nothing about the other.
//!
//! Approach: a stub HTTP server on a loopback port answering
//! `POST /chat/completions` with one canned SSE completion. That is the whole
//! wire this feature adds - any OpenAI-compatible endpoint - so a case that
//! mocked the adapter instead would assert the plumbing and never the claim.
//!
//! Features NOT tested here: the wire body and the stream decoding
//! (`crates/turn/tests/deepseek_adapter.rs`), reading the block out of a
//! document (`crates/engine/tests/providers.rs`), and the browser panel
//! (`crates/host/tests/web_app.rs`). None is restated.
//!
//! Environmental needs: a loopback port and a writable temp directory. No
//! case reaches a network or a real API key: the credential each one exports
//! is a placeholder the stub never reads.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// What the stub answers with, and what the page must therefore print.
const SAID: &str = "stub says hi";

/// The variable the declared provider's block names. Nothing else reads it,
/// so a case that exports it cannot decide anything for a case beside it.
const KEY_ENV: &str = "TETANUS_LOCAL_KEY";

/// A stub OpenAI-compatible endpoint on a loopback port.
///
/// It answers every request the same way and keeps answering, because a route
/// may retry: a server that served one request and stopped would turn a retry
/// into a connection refused halfway through a case about something else.
struct Stub {
    base_url: String,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("an address"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                // Read what arrived before answering: a server that answers
                // and closes while the request is still being written ends
                // the client's send in a reset rather than in a response.
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let body = format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({
                        "choices": [{
                            "delta": { "content": SAID },
                            "finish_reason": "stop",
                        }]
                    })
                );
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                         connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
            }
        });
        Self { base_url }
    }
}

/// A settings document declaring one provider on a stub of its own, for a
/// case that never has to reach the endpoint.
fn document(dir: &Path, models: &str) -> PathBuf {
    document_on(dir, &Stub::start(), models)
}

/// The same, on a stub the case keeps, so the address in the document and the
/// address the case asserts against are one address.
fn document_on(dir: &Path, stub: &Stub, models: &str) -> PathBuf {
    let path = dir.join("settings.yaml");
    std::fs::write(
        &path,
        format!(
            "llm:
  providers:
    local:
      base_url: {}
      api_key_env: {KEY_ENV}
      models: {models}
",
            stub.base_url
        ),
    )
    .expect("write");
    path
}

/// One run of the binary, with the harness home in the case's own directory
/// so no document on the machine running the suite can decide anything.
fn run(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir)
        .args(args)
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("DEEPSEEK_BASE_URL")
        .env_remove(KEY_ENV);
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().expect("the binary runs")
}

/// The same, with `typed` on standard input: the mode a `chat` case can drive.
fn piped(dir: &Path, args: &[&str], env: &[(&str, &str)], typed: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir)
        .args(args)
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("DEEPSEEK_BASE_URL")
        .env_remove(KEY_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        cmd.env(name, value);
    }
    let mut child = cmd.spawn().expect("the binary runs");
    // A run that refuses before it reads can be gone before these bytes are
    // offered, and a pipe with no reader ends in `BrokenPipe`. What happened
    // is asserted from the output, never from the write.
    let _ = child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(typed.as_bytes());
    child.wait_with_output().expect("the binary exits")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// TC-CLI-PROV-1: a turn on a provider the document declared.
///
/// Input: a document declaring `local` on a stub endpoint, and
/// `tetanus run --adapter local -m stub-model`.
/// Expected: exit 0, and the stub's reply on the page. This is the whole
/// feature in one line of proof: three lines of configuration and a model
/// nothing in this binary knows about runs a turn.
#[test]
fn a_declared_provider_runs_a_turn() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = Stub::start();
    let path = document_on(dir.path(), &stub, "[stub-model]");
    let journal = dir.path().join("s.jsonl");

    let out = run(
        dir.path(),
        &[
            "--color",
            "never",
            "--settings",
            &path.display().to_string(),
            "run",
            "--adapter",
            "local",
            "-m",
            "stub-model",
            "-p",
            "hi",
            "--session",
            &journal.display().to_string(),
        ],
        &[(KEY_ENV, "placeholder")],
    );

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains(SAID), "{}", stdout(&out));
    assert!(journal.exists(), "the journal was written");
}

/// TC-CLI-PROV-2: the model page lists a provider the document declared.
///
/// Input: `tetanus models --json` against the same document.
/// Expected: the built-in routes and `local`, with the document's model and
/// its credential reference, `available` following the exported variable. The
/// page and the panel read one registry, so a provider that runs must also be
/// one a reader can discover.
#[test]
fn the_model_page_lists_a_declared_provider() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = document(dir.path(), "[stub-model]");

    let listed = stdout(&run(
        dir.path(),
        &[
            "--settings",
            &path.display().to_string(),
            "models",
            "--json",
        ],
        &[(KEY_ENV, "placeholder")],
    ));
    let catalog: serde_json::Value = serde_json::from_str(listed.trim()).expect("one JSON line");
    let providers = catalog["providers"].as_array().expect("providers");
    let local = providers
        .iter()
        .find(|entry| entry["provider"] == "local")
        .unwrap_or_else(|| panic!("local is not listed: {listed}"));

    assert_eq!(local["models"][0], "stub-model");
    assert_eq!(local["credential_env"], KEY_ENV);
    assert_eq!(local["available"], true);

    // And without the key, the same page greys it out rather than dropping it.
    let unkeyed = stdout(&run(
        dir.path(),
        &["--settings", &path.display().to_string(), "models"],
        &[],
    ));
    assert!(unkeyed.contains("local"), "{unkeyed}");
    assert!(unkeyed.contains(&format!("set {KEY_ENV}")), "{unkeyed}");
}

/// TC-CLI-PROV-3: a provider name nothing serves is refused, and the message
/// says what is served.
///
/// Input: `--adapter nosuch` against a document declaring `local`.
/// Expected: exit 2 - §4.5's status for an argument the harness will not
/// accept - naming the key, and listing the registry's own routes including
/// the declared one. Listing a compiled pair instead would tell a reader who
/// declared a provider that it does not exist.
#[test]
fn a_name_nothing_serves_is_refused_and_lists_what_is() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = document(dir.path(), "[stub-model]");

    let out = run(
        dir.path(),
        &[
            "--color",
            "never",
            "--settings",
            &path.display().to_string(),
            "run",
            "--adapter",
            "nosuch",
            "-p",
            "hi",
        ],
        &[(KEY_ENV, "placeholder")],
    );

    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    let said = stderr(&out);
    assert!(said.contains("nosuch"), "names what was typed: {said}");
    assert!(said.contains("mock"), "names what is served: {said}");
    assert!(said.contains("local"), "including the declared one: {said}");
}

/// TC-CLI-PROV-4: a declared provider whose credential is absent fails before
/// a journal is opened.
///
/// Input: `--adapter local` with the block's variable unset.
/// Expected: exit 5 - §4.5's status for a missing credential - naming the
/// variable the block declared, and no journal on disk. A route that failed
/// after opening one would leave a session holding no turns.
#[test]
fn a_declared_provider_with_no_credential_says_which_variable() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = document(dir.path(), "[stub-model]");
    let journal = dir.path().join("never.jsonl");

    let out = run(
        dir.path(),
        &[
            "--color",
            "never",
            "--settings",
            &path.display().to_string(),
            "run",
            "--adapter",
            "local",
            "-p",
            "hi",
            "--session",
            &journal.display().to_string(),
        ],
        &[],
    );

    assert_eq!(out.status.code(), Some(5), "{}", stdout(&out));
    assert!(stderr(&out).contains(KEY_ENV), "{}", stderr(&out));
    assert!(!journal.exists(), "no journal for a turn that cannot run");
}

/// TC-CLI-PROV-5: the `deepseek` alias still names the built-in route.
///
/// Input: `--adapter deepseek` with the stub standing in for the public
/// endpoint through `DEEPSEEK_BASE_URL`.
/// Expected: exit 0 and the stub's reply. Every document, script and case in
/// this repository that types the short spelling keeps working, which is the
/// reason the alias exists rather than a spelling unification that would
/// rewrite journal fixtures across the conformance suite.
#[test]
fn the_deepseek_alias_still_resolves() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = Stub::start();
    let journal = dir.path().join("alias.jsonl");

    let out = run(
        dir.path(),
        &[
            "--color",
            "never",
            "run",
            "--adapter",
            "deepseek",
            "-p",
            "hi",
            "--session",
            &journal.display().to_string(),
        ],
        &[
            ("DEEPSEEK_API_KEY", "placeholder"),
            ("DEEPSEEK_BASE_URL", stub.base_url.trim_end_matches("/v1")),
        ],
    );

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains(SAID), "{}", stdout(&out));
}

/// TC-CLI-PROV-6: a provider advertising no models, asked for without one.
///
/// Input: a block whose `models` list is empty, and no `-m`.
/// Expected: exit 2 and the sentence that says there is nothing to default to.
/// An empty catalogue is legal - an unlisted id still passes through - so the
/// failure belongs at the moment nobody named a model, not at the moment the
/// block was read.
#[test]
fn a_provider_that_advertises_no_models_needs_one_named() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = document(dir.path(), "[]");

    let out = run(
        dir.path(),
        &[
            "--color",
            "never",
            "--settings",
            &path.display().to_string(),
            "run",
            "--adapter",
            "local",
            "-p",
            "hi",
        ],
        &[(KEY_ENV, "placeholder")],
    );

    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("advertises no models"),
        "{}",
        stderr(&out)
    );
}

/// TC-CLI-PROV-7: one piped `tetanus chat` turn on a declared provider.
///
/// Input: a chat given one line on standard input, on `--adapter local`.
/// Expected: exit 0 and the stub's reply on the page. `run` and `chat` resolve
/// a provider through the same function but reach it by different paths, and
/// only one of them was proved by TC-CLI-PROV-1.
#[test]
fn a_piped_chat_talks_to_a_declared_provider() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = Stub::start();
    let path = document_on(dir.path(), &stub, "[stub-model]");
    let journal = dir.path().join("chat.jsonl");

    let out = piped(
        dir.path(),
        &[
            "--color",
            "never",
            "--settings",
            &path.display().to_string(),
            "chat",
            "--adapter",
            "local",
            "-m",
            "stub-model",
            "--session",
            &journal.display().to_string(),
        ],
        &[(KEY_ENV, "placeholder")],
        "hello\n",
    );

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains(SAID), "{}", stdout(&out));
}
