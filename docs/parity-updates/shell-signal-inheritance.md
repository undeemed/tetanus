# Parity update: a child starts with a signal disposition of its own

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file.

`docs/parity-updates` is empty on master after the last fold, so **this file is the only copy of
this knowledge outside the branch that carries it**.

## 1. What was wrong, in one paragraph a future reader can act on

A signal set to `SIG_IGN` is inherited across `fork` **and** across `exec`. So is a blocked signal
mask. POSIX (2.11) has a shell set `SIGINT` and `SIGQUIT` to `SIG_IGN` for any command it runs in
the background, so that a background job does not die when somebody presses `^C` at the terminal
that launched it. That is how `tetanus serve &`, a systemd unit, a CI runner and an orchestrator all
start a harness.

Everything downstream inherited it: the `bash` this crate starts on a pseudo-terminal, and every
command that `bash` ran. The consequences were invisible in exactly the way that matters:

- `terminal_signal` answered `delivered SIGINT to foreground process group N` and the command did
  not stop.
- The turn's own interrupt reached the right process group and the work continued.
- `killpg` returned success throughout - because **delivery genuinely succeeded**. The process
  simply ignored it.

In plain terms: **stopping a turn silently did nothing in every backgrounded deployment.** A model
that ran a wrong command and asked to stop it was told it had been stopped.

## 2. Why it read as flakiness for days

It surfaced as three tests that failed in the full workspace run and passed in isolation, which
everybody - including this lane at first - read as load. Load was a correlation, not a cause: a busy
machine is also a machine being driven from a script rather than by hand.

The measurement that separated them took two runs on one idle machine:

| How the same test binary was started | `killpg(fg, SIGINT)` | The `sleep 30` |
| --- | --- | --- |
| From an interactive shell | reports success | dies |
| With `&` from a non-interactive shell | reports success | **survives** |

No load in either. That table is the whole diagnosis, and it is why the fix is not a longer timeout.

## 3. What was built

`crates/exec/src/signals.rs` gives every child this crate starts the disposition a program launched
from a terminal has: eight ignorable signals back to `SIG_DFL`, and an empty signal mask, applied
between `fork` and `exec` in system calls only - which is all that window allows. `bash`, `sudo` and
`tmux` all do the same thing at the same point. It is applied by all four seams: terminals
(`pty.rs`), one-shot commands (`proc.rs`), persistent shells (`session.rs`) and protocol peers
(`piped.rs`).

`SIGPIPE` is in the set for a reason of its own: a Rust parent ignores it process-wide so a write to
a closed pipe is an error rather than a death. That is right for this process and wrong for a
shell - it is how `yes | head` learns to stop.

## 4. Two further defects the investigation turned up

- **An interrupt has to reach the shell as well as the foreground group.** A shell running a *list* -
  a `for` loop, `a && b`, a script - forks each command as its own job, so signalling the foreground
  group kills one `sleep` and the shell starts the next. Interrupt-class signals now go to both;
  terminating ones stay on the foreground group, because a shell that received one would end the
  session. The loop case had been passing on an idle machine **by accident**: idle timing happened
  to catch the shell between jobs, which is the one case where the shell does get the signal and
  does abandon the list.
- **A send that did not settle on a prompt leaves the shell owing one.** The next send could settle
  on that stale marker instantly, with an empty viewport, reporting a command it never ran as
  finished. The debt is counted now, which removes a hazard `terminal.rs` had documented as
  unavoidable and replaces a bounded wait that a busy machine won.

## 5. Section 3 row

The process-execution row's **Today** gains:

> Every child starts with a signal disposition of its own - the ignorable signals reset and the mask
> emptied between `fork` and `exec` - so an interrupt reaches a command whatever the harness was
> started with. Without it a harness launched in the background hands every shell it starts an
> ignored `SIGINT`, and every "stop this" is reported as delivered and does nothing.

Nothing leaves the **Gap** column: this was a defect, not a missing feature.

## 6. Section 4 row

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| — (no upstream counterpart; Node's `child_process` does not reset dispositions either, so upstream is likely to have the same latent behaviour) | `crates/exec/tests/upstream_signal_inheritance.rs` | That an interrupt works when the harness's own `SIGINT` is ignored | TC-PORT-TERM-44. The case reproduces the **cause**, not the correlation: it sets `SIGINT` to `SIG_IGN` in the test process - exactly what a background launch does - and asserts a command on a terminal still dies. No load, no sleeping, no second process to arrange. Removing the fix fails it in 10.9s; restoring it passes in 0.3s |

## 7. The rule this leaves behind

Worth stating as a rule rather than as a bug report, because it generalises past signals:

> **What a harness inherits, its children inherit.** A disposition, a mask, an environment variable,
> a resource limit, a working directory, an open descriptor: each survives `exec` unless something
> resets it, and each is therefore a property of *who started the harness* rather than of the
> harness. Anything a tool promises about a child - "this interrupt stops it", "this timeout kills
> it" - is a promise about the child's inherited state as much as about the code that makes it.

The cheap test for the whole class is the one this slice used: run the same thing twice, once from
an interactive shell and once with `&`, and compare. Any difference is inheritance.
