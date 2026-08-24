//! Test Design Specification: persistent terminals, ported.
//!
//! Feature under test: `tetanus_exec::terminal` and `tetanus_exec::sanitize` -
//! a shell on a real pseudo-terminal that a caller drives one send at a time,
//! its readiness, its viewport, its bounded scrollback and the pages it hands
//! back, signalling its foreground group, and how it reports its own death.
//! Upstream pins the same decisions in
//! `packages/terminal/terminal-bash/tests/session.spec.ts`,
//! `packages/terminal/terminal-bash/tests/sanitize.spec.ts` and
//! `packages/terminal/terminal-bash/tests/local.spec.ts`.
//!
//! Approach: real terminals with a real bash on them. The claims here are all
//! claims about what a *terminal* does - a program answers `test -t 1`
//! differently, a `^C` reaches a process group rather than a process, and the
//! shell announces its own prompts - so a fake terminal would be asserting the
//! fake. The sanitizer is the one piece that is pure, and its cases feed it
//! the byte sequences a terminal actually emits, including ones split across
//! two reads.
//!
//! What upstream has here that this does not, and why. Upstream infers
//! readiness from silence and from an exact syscall probe of whether the
//! foreground process is blocked reading its terminal; this is told by the
//! shell, through the OSC 133 marker its prompt prints, so `stdin_read` is a
//! fact here and carries the command's exit status where upstream's carries
//! nothing. Silence remains as the fallback for a program that prints no
//! marker, which is what TC-PORT-TERM-19 pins. Upstream's background sends
//! (`run_in_background`) need the job store this phase has not built.
//!
//! Environmental needs: Linux with `/dev/ptmx`, a bash on PATH, a writable
//! temp directory. Every case reports itself skipped where a terminal cannot
//! be allocated rather than passing for the wrong reason. No case reaches a
//! network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use tetanus_exec::backend::Bash;
use tetanus_exec::sanitize::{Sanitizer, PROMPT_TEXT};
use tetanus_exec::terminal::{
    Status, TerminalConfig, TerminalError, TerminalSession, TerminalSignal, WaitReason,
};
use tetanus_turn::interrupt::Interrupt;

/// TC-PORT-TERM-15: a terminal session keeps its directory and its variables
/// between sends, and each send reports what the command exited with.
///
/// Upstream: "state persists across sends" (`session.spec.ts`).
///
/// The reason a persistent terminal exists, and the half upstream cannot
/// report: its `stdin_read` says the shell is asking for input again, and
/// nothing says whether the command worked. The prompt marker here carries the
/// status, so a model reading a result does not have to run `echo $?` to find
/// out.
///
/// Input: one session; `cd` and `export` in the first send, `pwd` and the
/// variable in the second, a deliberately failing command in the third.
/// Expected: the second send sees both, both settle as `stdin_read` with code
/// 0, and the third reports code 3 with the session still running.
#[tokio::test]
async fn a_terminal_keeps_its_directory_and_variables_between_sends() {
    let workspace = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(workspace.path().join("inner")).expect("a directory to change into");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };

    let first = session
        .send("cd inner && export TETANUS_TERM=kept", true, None)
        .await
        .expect("the first send");
    assert_eq!(first.wait, WaitReason::StdinRead);
    assert_eq!(first.code, Some(0));

    let second = session
        .send("pwd; echo \"var=$TETANUS_TERM\"", true, None)
        .await
        .expect("the second send");
    assert!(
        second.viewport.contains("/inner"),
        "the directory did not survive: {:?}",
        second.viewport
    );
    assert!(
        second.viewport.contains("var=kept"),
        "the variable did not survive: {:?}",
        second.viewport
    );
    assert_eq!(second.code, Some(0));

    // A subshell, because `exit` typed at the session would end the shell -
    // which is TC-PORT-TERM-25's case, not this one.
    let third = session
        .send("(exit 3)", true, None)
        .await
        .expect("the third send");
    assert_eq!(third.code, Some(3), "the marker carries the real status");
    assert_eq!(third.status, Status::Running, "the shell is still there");
    session.close().await;
}

/// TC-PORT-TERM-16: a program on this session believes it has a terminal.
///
/// Upstream: its whole terminal backend exists for this. It is also the exact
/// line between this and `tetanus_exec::session`: the pipe-backed shell there
/// answers "no" to the same question, so a program that colours, pages,
/// prompts for a password or refuses to run without a tty behaves differently
/// in the two - and only one of them can carry an interactive tool.
///
/// Input: `test -t 1`, `tty`, and `stty size` in one send.
/// Expected: the program says it has a terminal, names a `/dev/pts` device,
/// and reads back the size the session was opened with.
#[tokio::test]
async fn a_program_in_a_terminal_session_believes_it_has_one() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };

    let seen = session
        .send("test -t 1 && echo IS-A-TTY; tty; stty size", true, None)
        .await
        .expect("sent");

    assert!(
        seen.viewport.contains("IS-A-TTY"),
        "the program saw a pipe: {:?}",
        seen.viewport
    );
    assert!(
        seen.viewport.contains("/dev/pts/"),
        "the program could not name its terminal: {:?}",
        seen.viewport
    );
    assert!(
        seen.viewport.contains("40 160"),
        "the program read a different size from the one it was given: {:?}",
        seen.viewport
    );

    // And the shape can change afterwards, which is what a surface showing a
    // live terminal needs: a program that draws a full screen has to be told.
    session.resize(24, 80).expect("resized");
    assert_eq!(session.size().expect("measured"), (24, 80));
    let resized = session.send("stty size", true, None).await.expect("sent");
    assert!(
        resized.viewport.contains("24 80"),
        "the program did not see the resize: {:?}",
        resized.viewport
    );
    session.close().await;
}

/// TC-PORT-TERM-17: the terminal's control language never reaches the reader,
/// and the prompt marker never reaches it either.
///
/// Upstream: `sanitize.spec.ts` ("removes CSI/OSC sequences", "keeps the
/// private prompt marker out of the text").
///
/// A model reading `\x1b[32mok\x1b[0m` reads `[32mok[0m`, and a window-title
/// sequence reads as a line the program never printed. The marker is worse
/// than noise: it is this crate's own protocol, and showing it to a model
/// teaches the model to print one.
///
/// Input: a command printing coloured text, a window title, and a bare
/// carriage return, through a real terminal.
/// Expected: the printable text survives exactly; no escape byte, no `[3`
/// fragment, no `133;D` and no title text is in the viewport.
#[tokio::test]
async fn escape_sequences_and_the_prompt_marker_never_reach_the_reader() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };

    // The sequences are in a script rather than on the command line, so what
    // the assertions look for can only have come out of the terminal. A shell
    // echoes what is typed at it, and a case that typed its own payload would
    // find it either way.
    let script = workspace.path().join("noisy.sh");
    std::fs::write(
        &script,
        "printf '\\033]0;a window title\\007\\033[32mGREEN-OK\\033[0m\\n'\nprintf 'over\\rwritten\\n'\n",
    )
    .expect("a script to run");
    let seen = session
        .send(&format!("sh {}", script.display()), true, None)
        .await
        .expect("sent");

    assert!(
        seen.viewport.contains("GREEN-OK"),
        "the text inside the colour was lost: {:?}",
        seen.viewport
    );
    assert!(
        !seen.viewport.contains('\x1b'),
        "an escape byte reached the reader: {:?}",
        seen.viewport
    );
    assert!(
        !seen.viewport.contains("a window title"),
        "an OSC payload was printed as text: {:?}",
        seen.viewport
    );
    assert!(
        !seen.viewport.contains("133;D"),
        "the prompt marker reached the reader: {:?}",
        seen.viewport
    );
    assert!(
        seen.viewport.contains("over\nwritten"),
        "a bare carriage return should read as a new line: {:?}",
        seen.viewport
    );
    session.close().await;
}

/// TC-PORT-TERM-18: an escape sequence split across two reads is carried, not
/// half-printed.
///
/// Upstream: `sanitize.spec.ts` ("preserves split-sequence carry").
///
/// The failure this prevents is intermittent by nature: a terminal read
/// returns whatever the kernel had, so the same program produces a clean
/// transcript on an idle machine and `[3` on a loaded one. Asserted directly
/// on the sanitizer, because provoking a specific split through a real
/// terminal would be asserting the scheduler.
///
/// Input: a colour sequence, a prompt marker and a `\r\n` each cut in half
/// across two chunks.
/// Expected: nothing of any sequence is printed, the marker is read once with
/// its status, and the split `\r\n` is one newline.
#[test]
fn a_sequence_split_across_two_reads_is_carried() {
    let mut sanitizer = Sanitizer::new();

    let first = sanitizer.push("before\x1b[3");
    assert_eq!(first.text, "before", "half a CSI must not be printed");
    assert!(first.prompts.is_empty());

    let second = sanitizer.push("2mafter\x1b]133;D;");
    assert_eq!(second.text, "after");
    assert!(second.prompts.is_empty(), "half a marker is not a marker");

    let third = sanitizer.push("7\x07tail\r");
    assert_eq!(
        third.prompts,
        vec![Some(7)],
        "the marker carries the command's status"
    );
    assert_eq!(third.text, "tail", "a trailing \\r waits for what follows");

    let fourth = sanitizer.push("\nnext");
    assert_eq!(
        fourth.text, "\nnext",
        "a \\r\\n split across two reads is one newline"
    );
    assert_eq!(sanitizer.flush(), "");
}

/// TC-PORT-TERM-19: a program that prints nothing and does not finish settles
/// on silence; one that never stops printing settles on the deadline.
///
/// Upstream: `session.spec.ts` ("returns inferred_idle after quiet", "returns
/// timeout at the absolute bound").
///
/// The two fallbacks behind the marker, and they answer different questions. A
/// program waiting for input is quiet, so silence is the only sign it is ready
/// for more - `python`, `psql`, `ssh` asking for a password. A program that is
/// working prints as it goes, so silence never comes and only the deadline
/// ends the wait. Both leave the session running, because neither says the
/// command is over.
///
/// Input: `cat` with no input, on a session whose silence bound is short; then
/// a loop printing continuously, on one whose deadline is short.
/// Expected: `inferred_idle` with no exit code, then `timeout` with none, and
/// the session running in both cases.
#[tokio::test]
async fn silence_and_the_deadline_are_the_two_fallbacks_behind_the_marker() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal_with(
        workspace.path(),
        TerminalConfig {
            idle_silence: Duration::from_millis(400),
            timeout: Duration::from_secs(20),
            ..config(workspace.path())
        },
    )
    .await
    else {
        return;
    };

    let quiet = session.send("cat", true, None).await.expect("sent");
    assert_eq!(quiet.wait, WaitReason::InferredIdle);
    assert_eq!(quiet.code, None, "silence says nothing about a status");
    assert_eq!(quiet.status, Status::Running);
    // Ctrl-D, which is how a program reading a terminal is told there is no
    // more input: it leaves the shell at a prompt for the next case.
    session.send("\u{4}", false, None).await.expect("sent");
    session.close().await;

    let Some(chatty) = terminal_with(
        workspace.path(),
        TerminalConfig {
            idle_silence: Duration::from_secs(30),
            timeout: Duration::from_millis(600),
            ..config(workspace.path())
        },
    )
    .await
    else {
        return;
    };
    let busy = chatty
        .send(
            "while true; do echo still-working; sleep 0.05; done",
            true,
            None,
        )
        .await
        .expect("sent");
    assert_eq!(busy.wait, WaitReason::Timeout);
    assert_eq!(busy.code, None, "a deadline says nothing about a status");
    assert_eq!(busy.status, Status::Running);
    assert!(
        busy.viewport.contains("still-working"),
        "a send that timed out still hands back what it saw: {:?}",
        busy.viewport
    );
    chatty.close().await;
}

/// TC-PORT-TERM-20: an interrupt reaches the command, and the session survives
/// it.
///
/// Upstream: `session.spec.ts` ("cancel sends SIGINT to the foreground
/// group"), and the reason its cancel is not a close.
///
/// This is the case a pipe-backed session cannot have. `tetanus_exec::session`
/// ends the whole shell when a turn is stopped, because a shell reading a pipe
/// has no way to interrupt one command; a terminal does, so a stopped turn
/// costs the command and not the session's directory, variables and history.
///
/// Input: a long sleep, with the turn's interrupt thrown while it runs; then
/// another send on the same session.
/// Expected: the send settles as `interrupted`, the session is still running,
/// and it answers the next send.
#[tokio::test]
async fn an_interrupt_reaches_the_command_and_the_session_survives() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    let interrupt = Interrupt::new();

    let thrown = Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        thrown.stop();
    });
    let stopped = session
        .send("sleep 30", true, Some(&interrupt))
        .await
        .expect("sent");

    assert_eq!(stopped.wait, WaitReason::Interrupted);
    assert_eq!(stopped.status, Status::Running, "the shell is still there");

    // A fresh switch: the turn that was stopped is over, and the session
    // outlived it.
    let after = session
        .send("echo STILL-HERE", true, None)
        .await
        .expect("sent");
    assert!(
        after.viewport.contains("STILL-HERE"),
        "the session died with its command: {:?}",
        after.viewport
    );
    session.close().await;
}

/// TC-PORT-TERM-21: a second send while one is running is refused, not queued.
///
/// Upstream: `SEND_ACTIVE` (`session.spec.ts`, "rejects a concurrent send").
///
/// Two commands typed at one terminal interleave into one stream nobody can
/// attribute: the second command's echo lands inside the first one's output,
/// and both viewports are wrong. Refusing says so; queueing would hide it
/// behind a wait the caller did not ask for and cannot see.
///
/// Input: one send left running, and a second attempted on the same session.
/// Expected: the second is refused as an active send naming the session, and
/// the first still settles.
#[tokio::test]
async fn a_second_send_while_one_is_running_is_refused() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    let session = Arc::new(session);

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.send("sleep 2", true, None).await }
    });
    // Long enough that the first send owns the session, short enough that it
    // has not settled: the sleep is the wait, not the assertion.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let refused = session.send("echo second", true, None).await;
    match refused {
        Err(TerminalError::SendActive(id)) => assert_eq!(id, session.id()),
        other => panic!("a concurrent send should be refused, got {other:?}"),
    }

    let first = running.await.expect("the task").expect("the first send");
    assert_eq!(first.wait, WaitReason::StdinRead);
    session.close().await;
}

/// TC-PORT-TERM-22: retained output pages back from the newest line.
///
/// Upstream: `TerminalReadResult` and its `read` cases ("newest-relative
/// offset", "count caps the page", "an offset past the end is an empty page").
///
/// Newest-relative is the part worth pinning: a model asking for "the last
/// hundred lines" while the terminal keeps printing must keep getting the last
/// hundred, where an offset counted from the beginning would slide under it.
///
/// Input: fifty numbered lines, then three pages - the newest ten, the ten
/// before those, and one past the end.
/// Expected: each page holds the lines its offset names, the totals agree, and
/// the page past the end is empty rather than an error.
#[tokio::test]
async fn retained_output_pages_back_from_the_newest_line() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    session
        .send("for i in $(seq 1 50); do echo line-$i; done", true, None)
        .await
        .expect("sent");

    let newest = session.read(0, Some(10)).expect("a page");
    assert!(
        newest.text.contains("line-50"),
        "the newest page must hold the newest line: {:?}",
        newest.text
    );
    assert!(
        !newest.text.contains("line-30"),
        "ten lines back does not reach line 30: {:?}",
        newest.text
    );
    assert_eq!(newest.line_begin, 0);
    assert_eq!(newest.line_end, 10);

    let older = session.read(10, Some(10)).expect("a page");
    assert!(
        older.text.contains("line-41") && !older.text.contains("line-50"),
        "the second page is the ten before the newest ten: {:?}",
        older.text
    );
    assert_eq!(older.total_lines, newest.total_lines);

    let past_the_end = session
        .read(newest.total_lines + 5, Some(10))
        .expect("a page");
    assert_eq!(past_the_end.text, "");
    assert_eq!(past_the_end.line_begin, past_the_end.line_end);

    match session.read(0, Some(0)) {
        Err(TerminalError::BadPage(_)) => {}
        other => panic!("a page of no lines has no answer, got {other:?}"),
    }
    session.close().await;
}

/// TC-PORT-TERM-23: output past the scrollback bound drops the beginning, and
/// the page says so.
///
/// Upstream: `scrollbackMaxBytes` and the `truncated` flag it sets.
///
/// A terminal that kept everything would cost a model's `yes` loop all the
/// memory the machine has. What matters is which end goes - the beginning,
/// because the end is what someone is reading - and that a reader is told,
/// rather than handed a shorter transcript that looks complete.
///
/// Input: far more output than the bound keeps, on a small bound.
/// Expected: the newest line is retained, the first is gone, and both the page
/// and the send report the truncation.
#[tokio::test]
async fn output_past_the_bound_drops_the_beginning_and_says_so() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal_with(
        workspace.path(),
        TerminalConfig {
            scrollback_bytes: 16 * 1024,
            timeout: Duration::from_secs(60),
            ..config(workspace.path())
        },
    )
    .await
    else {
        return;
    };

    let burst = session
        .send(
            "for i in $(seq 1 5000); do echo line-$i; done; echo THE-VERY-LAST-LINE",
            true,
            None,
        )
        .await
        .expect("sent");

    assert!(
        burst.truncated,
        "far more was printed than the bound keeps, and the send did not say so"
    );
    let page = session.read(0, Some(5)).expect("a page");
    assert!(
        page.truncated,
        "the page did not report the loss: {page:#?}"
    );
    assert!(
        page.text.contains("THE-VERY-LAST-LINE"),
        "the end is what a reader is reading: {:?}",
        page.text
    );
    assert!(
        !session.scrollback().contains("line-1\n"),
        "the beginning should have gone first"
    );
    session.close().await;
}

/// TC-PORT-TERM-24: a signal goes to the foreground group, and one that would
/// end the shell is refused.
///
/// Upstream: `terminal_signal`'s allowed list, and "shell-targeted SIGKILL is
/// rejected; use terminal_close".
///
/// A `^C` on a terminal reaches whatever owns the terminal, which is the
/// command while one is running. The refusal is the other half: `SIGKILL` at a
/// shell that is only waiting for input is a close written as a signal, and
/// one nobody would be told about - the session would simply stop answering.
///
/// Input: a long sleep started, then `SIGINT`; then `SIGKILL` at an idle
/// session.
/// Expected: the signal names the command's group rather than the shell's, the
/// session survives, and the `SIGKILL` is refused with the advice to close.
#[tokio::test]
async fn a_signal_goes_to_the_foreground_group_and_a_shell_killer_is_refused() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    let session = Arc::new(session);

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.send("sleep 30", true, None).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let group = session
        .signal(TerminalSignal::Int)
        .expect("the signal was delivered");
    assert_ne!(
        group,
        session.pid(),
        "the signal should reach the command's group, not the shell's"
    );
    let interrupted = running.await.expect("the task").expect("the send");
    assert_eq!(interrupted.status, Status::Running);

    // The second half asserts what happens when the *shell* owns the terminal,
    // so wait until it does rather than assuming the interrupt was instant. It
    // is not: `SIGINT` reaches the command, the command dies, and bash
    // reclaims the terminal some microseconds later - a gap the machine widens
    // whenever it is busy. Asserting through it made this case fail only under
    // load, which is the worst kind of case to own.
    for _ in 0..400 {
        if session
            .foreground_group()
            .is_ok_and(|group| group == session.pid())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        session.foreground_group().ok(),
        Some(session.pid()),
        "the shell should have the terminal back once its command is gone"
    );

    match session.signal(TerminalSignal::Kill) {
        Err(TerminalError::WouldKillShell { id, signal }) => {
            assert_eq!(id, session.id());
            assert_eq!(signal, "SIGKILL");
        }
        other => panic!("a shell-targeted SIGKILL should be refused, got {other:?}"),
    }
    assert_eq!(session.status(), Status::Running);
    session.close().await;
}

/// TC-PORT-TERM-25: a shell that exits is reported, and nothing is restarted
/// underneath the caller.
///
/// Upstream: `session_exit`, and its own decision to reset the session. This
/// keeps the notice and drops the reset, for the reason
/// `crates/exec/src/session.rs` gives: a restarted shell is a shell in a state
/// the model did not create, while the transcript says everything worked.
///
/// Input: `exit 7` typed at the session, then another send.
/// Expected: the send that killed it settles as `session_exit` with the
/// status; the session reports it exited with 7; and the next send is refused
/// with that status rather than answered by a new shell.
#[tokio::test]
async fn a_shell_that_exits_is_reported_and_never_restarted() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };

    let last = session.send("exit 7", true, None).await.expect("sent");
    assert_eq!(last.wait, WaitReason::SessionExit);
    assert_eq!(
        last.status,
        Status::Exited {
            code: Some(7),
            signal: None
        }
    );

    match session.send("echo anyone-there", true, None).await {
        Err(TerminalError::Ended { id, status }) => {
            assert_eq!(id, session.id());
            assert_eq!(
                status,
                Status::Exited {
                    code: Some(7),
                    signal: None
                }
            );
        }
        other => panic!("a dead session must be reported, got {other:?}"),
    }
}

/// TC-PORT-TERM-26: closing a session takes everything on the terminal.
///
/// Upstream: "close awaits quiescence of the captured owned process tree".
///
/// A terminal session is a process group by construction - its shell called
/// `setsid` - so a background job started in it is inside that group. Closing
/// the session without reaching the group would leave the job holding the
/// terminal open, which is how a harness that has exited leaves a `sleep`
/// behind for an hour.
///
/// Input: a session that starts a long background job, then a close.
/// Expected: the session reports it has exited, and the background job is
/// gone.
#[tokio::test]
async fn closing_a_session_takes_everything_on_the_terminal() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    let pidfile = workspace.path().join("child.pid");
    session
        .send(
            &format!("sleep 120 & echo $! > {}", pidfile.display()),
            true,
            None,
        )
        .await
        .expect("sent");
    let child = read_pid(&pidfile);

    session.close().await;

    assert!(
        matches!(session.status(), Status::Exited { .. }),
        "the session did not end: {:?}",
        session.status()
    );
    assert!(!alive(child), "a job on the terminal outlived the session");
}

/// TC-PORT-TERM-27: what the shell said before its first prompt is kept, and a
/// session is published only once it has reached one.
///
/// Upstream: `motd`, and "the service publishes only after backend setup
/// succeeds".
///
/// A session id that names a shell which never started is an id every later
/// call fails on, and the caller cannot tell that from a shell that died a
/// moment later. Waiting for the first prompt is what makes the id mean
/// "there is a shell here".
///
/// Input: a session opened with a startup file that prints a banner, and one
/// opened against a shell this host does not have.
/// Expected: the banner is the session's `motd` and its status is running; the
/// missing shell is refused loudly, with no session handed back.
#[tokio::test]
async fn a_session_is_published_only_once_its_shell_is_asking_for_input() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let mut env = std::collections::BTreeMap::new();
    env.insert("BASH_ENV".to_string(), String::new());
    let Some(session) = terminal_with(
        workspace.path(),
        TerminalConfig {
            env: [(
                "PROMPT_COMMAND".to_string(),
                // The backend's own marker, with a banner in front of it: a
                // startup file's greeting, said the way a deployment would.
                format!(
                    "printf 'welcome to the machine\\n'; PROMPT_COMMAND='printf \"\\033]133;D;%s\\007\" \"$?\"; PS1=\"{PROMPT_TEXT}\"'"
                ),
            )]
            .into_iter()
            .collect(),
            ..config(workspace.path())
        },
    )
    .await
    else {
        return;
    };

    assert!(
        session.motd().contains("welcome to the machine"),
        "the banner was lost: {:?}",
        session.motd()
    );
    assert_eq!(session.status(), Status::Running);
    session.close().await;

    let missing = TerminalSession::open(
        "pty-missing".into(),
        None,
        "nowhere".into(),
        Arc::new(Bash::at("/nowhere/no-such-shell")),
        config(workspace.path()),
    )
    .await;
    match missing {
        Err(TerminalError::Backend(refused)) => assert!(
            refused.to_string().contains("no-such-shell"),
            "the refusal should name what is missing: {refused}"
        ),
        other => panic!("a missing shell must be refused loudly, got {other:?}"),
    }
}

// ---------------------------------------------------------------- fixtures

/// A terminal session on a bash rooted at `workspace`, or `None` after
/// reporting the case skipped where this host has no terminal to allocate.
async fn terminal(workspace: &std::path::Path) -> Option<TerminalSession> {
    terminal_with(workspace, config(workspace)).await
}

async fn terminal_with(
    workspace: &std::path::Path,
    config: TerminalConfig,
) -> Option<TerminalSession> {
    match TerminalSession::open(
        "pty-1".into(),
        None,
        "bash".into(),
        Arc::new(Bash::new()),
        TerminalConfig {
            cwd: workspace.to_path_buf(),
            ..config
        },
    )
    .await
    {
        Ok(session) => Some(session),
        Err(TerminalError::Pty(tetanus_exec::pty::PtyError::Allocate(why))) => {
            eprintln!("skipped: this host cannot allocate a pseudo-terminal ({why})");
            None
        }
        Err(TerminalError::Backend(why)) => {
            eprintln!("skipped: this host has no bash ({why})");
            None
        }
        Err(other) => panic!("the terminal session could not be opened: {other}"),
    }
}

/// Budgets short enough that a case waiting for one waits for milliseconds.
fn config(workspace: &std::path::Path) -> TerminalConfig {
    TerminalConfig {
        cwd: workspace.to_path_buf(),
        idle_silence: Duration::from_secs(5),
        timeout: Duration::from_secs(20),
        grace: Duration::from_millis(200),
        ..TerminalConfig::default()
    }
}

/// The pid a case's shell recorded, waiting briefly for the file to appear.
fn read_pid(path: &std::path::Path) -> i32 {
    for _ in 0..300 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("no pid was ever recorded at {}", path.display());
}

/// Whether a process still exists, asked the way a shell asks: signal zero.
fn alive(pid: i32) -> bool {
    for _ in 0..1_000 {
        // Safety: signal zero delivers nothing; it only asks whether the
        // process exists and could be signalled.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}
