# Parity update: PowerShell announces its prompts, and the terminal answers

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file. `docs/parity-updates` was empty on
master before this, so **this file is the only copy of what follows**.

## 1. The clause

The `shell/*`, `terminal/*`, `subprocess/*` row's Gap said:

> A prompt marker for PowerShell, whose equivalent lives in the `$PROFILE` that `-NoProfile`
> deliberately does not load.

Served. And closing it needed something the row had not named, which is the more useful half of
this note.

## 2. A terminal is asked questions, and a program that asks waits

`crate::screen` models what a terminal *shows*. It turns out that is not enough to be a terminal: a
program also *asks*, and blocks on the answer.

PowerShell's line editor asks for the cursor position (`CSI 6n`) before it prints its first prompt.
Against a terminal that never answers, `pwsh` does not start - it prints a PSReadLine bug report and
gives up. That is not a tetanus quirk: `script`, which allocates a real pty and answers nothing,
fails identically. Measured both ways before anything was written.

So the screen answers, because it is the only thing here that knows where the cursor is:

| asked | answered | why that answer |
| --- | --- | --- |
| `CSI 6n` cursor position | `CSI row;col R` | the grid knows it exactly |
| `CSI 5n` device status | `CSI 0n` | the terminal is there |
| `CSI c` primary attributes | `CSI ?1;2c` | VT100 with the advanced video option - the honest floor for what is modelled |
| `CSI > c` secondary attributes | `CSI >0;10;1c` | a shape every terminal library parses and nothing keys behaviour off |

`Screen::feed` returns the replies and is `#[must_use]`: an unanswered question is a hung program,
and the type should not let a caller forget that quietly. The session writes them back.

**This is a general fix, not a PowerShell one.** Anything that queries the terminal before drawing -
line editors, some pagers, anything built on a terminal library that probes - was previously talking
to a terminal that never replied.

## 3. The marker is typed, because pwsh has nowhere to put it

bash gets its marker from `PROMPT_COMMAND`, an environment variable, so it can be handed over before
the shell starts. A PowerShell prompt is a *function*, and the only place to define one ahead of
time is the `$PROFILE` that `-NoProfile` skips - which is exactly why the row called this a gap.

A terminal can type. `ShellBackend::terminal_setup` runs once a session has reached its first
prompt, which is when a REPL is ready to be told something, and bash already uses it for
`stty -echo`. PowerShell now uses it to define `prompt`, emitting the same OSC 133 marker with
`$LASTEXITCODE` where a native program set one and `$?` otherwise - the same pair `wrap` already
reads for one-shot commands.

The effect is the whole point of the clause: a pwsh send settles as `stdin_read` with an exit
status, where before it settled on silence after three seconds and reported nothing.

## 4. How it was verified without a Windows host

CI's `ubuntu-latest` ships a PowerShell and this fleet's boxes do not, which is how #241 went out
red. Same trick as #241's author, deliberately: a PowerShell 7.4.6 release tarball on `PATH`, and
the whole `crates/exec` suite run **both ways** - 127 cases with pwsh present, 127 without. The new
case skips itself where there is no PowerShell and says so.

## 5. Section 3 row

**Today** gains:

> A terminal that answers the questions programs ask it - cursor position, device status, device
> attributes - because a program that asks one blocks until it is answered, and PowerShell's line
> editor asks before it prints anything. PowerShell terminals announce their prompts like bash
> ones: the marker is typed into the REPL at startup, since a pwsh prompt is a function and the
> profile that would hold it is deliberately not loaded.

**Gap** loses the PowerShell prompt-marker clause and keeps *a Windows host to run the PowerShell
backend against*, which nothing here can substitute for. One clause is added:

> A pwsh viewport carries its command more than once, because PSReadLine repaints the edited line
> several times per send. The honest fix is a per-send screen delta rather than trimming repeated
> text, and it waits on that.

## 6. Section 4 rows

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| — (upstream's terminal backend is bash-only; nothing to port) | `crates/exec/tests/upstream_screen.rs` | That the terminal answers what it is asked | TC-PORT-SCREEN-8 |
| `shell/pwsh-local`, the interactive half | `crates/exec/tests/upstream_terminal_session.rs` | A PowerShell terminal's readiness and its state between sends | TC-PORT-TERM-46, skipped where no PowerShell exists |
