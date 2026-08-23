# Parity update: what the journal keeps of a tool call

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file.

## Section 3, the process-execution row

**Today** gains:

> A credential typed at a terminal is not written down: `terminal_send` takes a
> `secret` flag, the terminal receives the real text, and the journal keeps the
> `<redacted>` sentinel in place of it - in the call, in the assistant message
> that carried it, and in the streamed chunk, because the model said it too.
> `shell` and `shell_run` take the same flag over their command line. A
> `sudo`-style backstop withholds a send made into a terminal whose last output
> line asked for a password, whether or not the model set the flag, and the two
> rules compose by union as contract §4.3 fixes for its own pair.

**Gap** gains:

> A prompt the backstop does not recognise - a "PIN", a one-time code, a
> program that asks for nothing in words - still records what the model typed
> unless it set the flag. `ECHO`-off detection cannot close that gap here and
> was measured rather than assumed: readline holds echo off at its own prompt,
> and this crate's `stty -echo` pins it off for the session. The tool
> descriptions and the operator docs therefore say plainly that a terminal
> journal holds whatever was typed into it.

## Why not upstream's shape

Upstream has no equivalent: its hook and terminal payloads are journalled whole,
and its own docs do not raise the case. This is therefore a **divergence**, not
a port, and it is one worth stating in section 5 alongside the others: a tool
decides what the record may keep, at record time, through one seam every tool
shares.

## Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| — (raised by the presentation lane, no upstream counterpart; mechanism after `sudo` 1.9.10) | `crates/turn/tests/tool_recording.rs`, `crates/exec/tests/upstream_terminal_tools.rs` | What the journal keeps of a call's arguments | TC-TOOL-RECORD-1..5, TC-PORT-TERM-42..43. The exec cases assert on the *whole journal* rather than on `tool/call`, which is how they caught the two records a fix aimed at the obvious site would have missed; TC-PORT-TERM-43 answers a real `[sudo] password for ci:` prompt with no flag set |
