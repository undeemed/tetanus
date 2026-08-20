//! Test Design Specification: the interactive `tetanus chat` command.
//!
//! Features tested: many turns on one journal, that each turn is asked with
//! the ones before it as history, that a chat resumes a journal it did not
//! start, every way out of the loop, the lines that are commands rather than
//! questions, and the failure when a real provider has no credential.
//!
//! Features NOT tested here: what a typed line means (owned by `chat::parse`,
//! asserted in its own module), what the page looks like (owned by
//! `render::chat`, likewise), the turn flow (owned by the conformance suite in
//! `tetanus-turn`), and the two keys - Ctrl-C and Ctrl-D at a terminal - that
//! a case with a pipe on its standard input cannot press. The pipe reaches the
//! same two exits: end of input is what Ctrl-D sends.
//!
//! Environmental needs: none. Every case runs offline on the mock adapter, and
//! the one case about a real provider is the case where it is never reached.

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tetanus_session::SessionEvent;

/// A chat with `typed` on its standard input, in a directory of its own.
///
/// Standard input is a pipe, not a terminal, which is the mode a case can
/// drive: the loop reads the same lines it reads from a keyboard and stops on
/// the end of them, and it prints no prompt marker because nobody is at one.
fn chat(dir: &Path, args: &[&str], typed: &str) -> Output {
    chat_with_key(dir, args, typed, None)
}

/// The same, with a stated `DEEPSEEK_API_KEY`. `None` removes it, so a case
/// never inherits the credential of the machine it runs on.
fn chat_with_key(dir: &Path, args: &[&str], typed: &str, key: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir)
        .arg("chat")
        .args(args)
        // The harness home is the case's own directory, so a settings
        // document on the machine running the suite cannot decide what a
        // chat runs on.
        .env("TETANUS_HOME", dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match key {
        Some(value) => cmd.env("DEEPSEEK_API_KEY", value),
        None => cmd.env_remove("DEEPSEEK_API_KEY"),
    };
    let mut child = cmd.spawn().expect("the binary runs");
    let written = child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(typed.as_bytes());
    // A chat that refuses before it reads - TC-CLI-CHAT-5, where a credential
    // is missing - can be gone by the time these bytes are offered, and then
    // the pipe has no reader and the write ends in `BrokenPipe`. That is the
    // case working, not a failure, so it is not an error here: what the run
    // did is asserted from the output below, never from the write.
    match written {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::BrokenPipe => {}
        Err(error) => panic!("the questions are written: {error}"),
    }
    child.wait_with_output().expect("the binary exits")
}

/// The page a chat printed, with the run asserted to have succeeded.
fn page(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).expect("utf-8")
}

/// Every question the journal records having been asked, in order.
fn asked(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.ty == "user/message")
        .filter_map(|event| event.data.get("content")?.as_str().map(str::to_owned))
        .collect()
}

/// The turn number each `turn/start` opened, in order.
fn turns(events: &[SessionEvent]) -> Vec<u64> {
    events
        .iter()
        .filter(|event| event.ty == "turn/start")
        .filter_map(|event| event.data.get("turn")?.as_u64())
        .collect()
}

/// What each turn's last request cost in prompt tokens.
///
/// The mock counts them off the messages it was handed, so this is the size of
/// the history the adapter really received - the one measurement of memory a
/// journal holds, and the reason these cases can assert continuity offline.
fn prompt_tokens(events: &[SessionEvent]) -> Vec<u64> {
    events
        .iter()
        .filter(|event| event.ty == "assistant/message")
        .filter_map(|event| event.data.get("usage")?.get("prompt_tokens")?.as_u64())
        .collect()
}

/// TC-CLI-CHAT-1: two questions typed one after the other.
/// Expected: exit 0; two turns numbered 1 and 2 on one journal; both answers
/// on the page; and the second turn asked with more history than the first.
/// The token count is the assertion that matters - two turns that shared a
/// journal but not a conversation would still print two answers.
#[test]
fn a_second_question_is_asked_with_the_first_exchange_behind_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = chat(
        dir.path(),
        &["-a", "mock", "-s", "talk.jsonl"],
        "what is a turn\nand what is a step\n",
    );
    let page = page(&out);

    assert!(page.contains("You said: what is a turn"), "{page}");
    assert!(page.contains("You said: and what is a step"), "{page}");

    let events = tetanus_session::replay(dir.path().join("talk.jsonl")).expect("replays");
    assert_eq!(turns(&events), vec![1, 2]);
    assert_eq!(asked(&events), vec!["what is a turn", "and what is a step"]);

    let spent = prompt_tokens(&events);
    let (first, second) = (
        spent.first().expect("turn 1"),
        spent.last().expect("turn 2"),
    );
    assert!(
        second > first,
        "the second turn was asked with no memory of the first: {spent:?}"
    );
}

/// TC-CLI-CHAT-2: a chat started again on a journal it did not start.
/// Expected: the opening page says how many turns it is carrying; the new turn
/// is numbered after them rather than from one; and it is asked with the whole
/// of the earlier conversation behind it. This is the acceptance criterion of
/// the command - leaving and coming back is not supposed to be a new
/// conversation - and it works because history is derived from the journal.
#[test]
fn a_restarted_chat_remembers_the_conversation() {
    let dir = tempfile::tempdir().expect("temp dir");
    let args = ["-a", "mock", "-s", "talk.jsonl"];
    page(&chat(dir.path(), &args, "one\ntwo\n"));

    let again = page(&chat(dir.path(), &args, "three\n"));

    assert!(again.contains("2 turns already"), "{again}");
    let events = tetanus_session::replay(dir.path().join("talk.jsonl")).expect("replays");
    assert_eq!(turns(&events), vec![1, 2, 3]);
    assert_eq!(asked(&events), vec!["one", "two", "three"]);

    let spent = prompt_tokens(&events);
    assert!(
        spent.last() > spent.first(),
        "the resumed turn was asked with no memory: {spent:?}"
    );
}

/// TC-CLI-CHAT-3: `/exit`.
/// Expected: exit 0, the question before it asked, and the line after it never
/// read. A command that only stopped printing would still spend a turn, and a
/// provider call the user asked not to make is the failure this case is about.
#[test]
fn exit_leaves_before_the_next_line_is_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = chat(
        dir.path(),
        &["-a", "mock", "-s", "talk.jsonl"],
        "asked\n/exit\nnever asked\n",
    );
    let page = page(&out);

    assert!(page.contains("You said: asked"), "{page}");
    assert!(!page.contains("never asked"), "{page}");
    let events = tetanus_session::replay(dir.path().join("talk.jsonl")).expect("replays");
    assert_eq!(asked(&events), vec!["asked"]);
}

/// TC-CLI-CHAT-4: nothing typed at all, which is Ctrl-D on an empty prompt.
/// Expected: exit 0, no turn, and a journal holding the session header and
/// nothing else - a chat that was opened and left is a session that happened,
/// and the page names the file so the user can carry on in it later.
#[test]
fn a_chat_left_immediately_spends_no_turn() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = chat(dir.path(), &["-a", "mock", "-s", "empty.jsonl"], "");
    let page = page(&out);

    assert!(page.contains("empty.jsonl"), "{page}");
    let events = tetanus_session::replay(dir.path().join("empty.jsonl")).expect("replays");
    assert_eq!(
        events
            .iter()
            .map(|event| event.ty.as_str())
            .collect::<Vec<_>>(),
        vec!["session/start"]
    );
}

/// TC-CLI-CHAT-5: the default adapter with no credential.
/// Expected: exit 5, the status contract §4.5 gives a missing credential; a
/// message naming the variable and the offline adapter; and no journal. The
/// check comes before the session is opened, so a chat nobody can hold does
/// not leave a file behind saying it was held.
#[test]
fn a_real_adapter_without_a_key_stops_before_the_journal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = chat(dir.path(), &["-s", "never.jsonl"], "hello\n");

    assert_eq!(out.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DEEPSEEK_API_KEY"), "{stderr}");
    assert!(stderr.contains("--adapter mock"), "{stderr}");
    assert!(
        !dir.path().join("never.jsonl").exists(),
        "nothing was written"
    );
}

/// TC-CLI-CHAT-6: the lines that are not questions.
/// Expected: a blank line, `/help` and `/?` each spend no turn; `/help` prints
/// the card; an unknown command is refused by name rather than asked; and the
/// question typed after all of them is still asked, numbered 1. Every one of
/// these would otherwise cost a provider call to say nothing.
#[test]
fn commands_and_blank_lines_spend_no_turn() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = chat(
        dir.path(),
        &["-a", "mock", "-s", "talk.jsonl"],
        "\n/help\n   \n/?\n/reset now\na real question\n",
    );
    let page = page(&out);

    assert!(page.contains("commands"), "the card is missing:\n{page}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("/reset"), "{stderr}");

    let events = tetanus_session::replay(dir.path().join("talk.jsonl")).expect("replays");
    assert_eq!(turns(&events), vec![1]);
    assert_eq!(asked(&events), vec!["a real question"]);
}

/// TC-CLI-CHAT-7: the escape.
/// Expected: `//exit` asks the model `/exit` and does not leave, so the line
/// after it is still read. Without the escape, every question that opens with
/// a path or a slash is one this command cannot put.
#[test]
fn a_double_slash_asks_what_would_have_been_a_command() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = chat(
        dir.path(),
        &["-a", "mock", "-s", "talk.jsonl"],
        "//exit\nstill here\n",
    );
    let page = page(&out);

    assert!(page.contains("You said: /exit"), "{page}");
    let events = tetanus_session::replay(dir.path().join("talk.jsonl")).expect("replays");
    assert_eq!(asked(&events), vec!["/exit", "still here"]);
}

/// TC-CLI-CHAT-8: a chat whose input is a pipe.
/// Expected: the transcript, and no prompt marker anywhere on it. The marker
/// is for a person deciding what to type next; a script that captured it would
/// be capturing a cursor position, and the page would no longer read as the
/// same conversation `tetanus replay` prints.
#[test]
fn a_piped_chat_prints_no_prompt_marker() {
    let dir = tempfile::tempdir().expect("temp dir");
    let page = page(&chat(
        dir.path(),
        &["-a", "mock", "-s", "talk.jsonl"],
        "asked\n",
    ));

    assert!(page.contains("You said: asked"), "{page}");
    for marker in ['\u{203a}', '>'] {
        assert!(
            !page.contains(marker),
            "the marker `{marker}` is on a piped page:\n{page}"
        );
    }
}

/// The sequences a value from outside would carry: clear the screen, and
/// rename the window. Named once, because every case below asks the same
/// question of a different value.
const ESC: char = '\u{1b}';

/// TC-CLI-CHAT-9: a chat opened on values that hold terminal control
/// sequences - the model a flag named, the path `-s` gave, and a command line
/// typed at the marker.
/// Expected: each one is still readable, and none of them reaches the terminal
/// as a sequence. The opening page is drawn before a single question is typed,
/// so a name that clears the screen clears the page that says where the
/// conversation is being written.
///
/// The page is read down to the first turn, because the rows a turn draws are
/// the timeline's and are covered where it is: this case owns the three values
/// a chat itself puts on the screen.
#[test]
fn nothing_a_chat_was_opened_with_can_drive_the_terminal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nasty = format!("mo{ESC}[2Jck{ESC}]0;pwned\u{7}");
    let journal = format!("ta{ESC}[2Jlk.jsonl");

    let out = chat(
        dir.path(),
        &[
            "-a", "mock", "--model", &nasty, "-s", &journal, "--color", "never",
        ],
        &format!("/re{ESC}[2Jset\nasked\n"),
    );
    let whole = page(&out);
    let opening = whole
        .split("\nturn 1")
        .next()
        .unwrap_or_default()
        .to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(opening.contains("chat on"), "{opening}");
    assert!(
        opening.contains("mock"),
        "the name stopped being readable:\n{opening}"
    );
    assert!(
        opening.contains("lk.jsonl"),
        "the path stopped being readable:\n{opening}"
    );
    assert!(stderr.contains("is not a command"), "{stderr}");
    assert!(stderr.contains("running the turn on"), "{stderr}");

    for (what, text) in [("the opening page", &opening), ("stderr", &stderr)] {
        assert!(!text.contains(ESC), "{what} carries an escape:\n{text:?}");
        assert!(!text.contains('\u{7}'), "{what} carries a bell:\n{text:?}");
    }

    // And the conversation still happened, on the journal that was named.
    let events = tetanus_session::replay(dir.path().join(&journal)).expect("replays");
    assert_eq!(asked(&events), vec!["asked"]);
}
