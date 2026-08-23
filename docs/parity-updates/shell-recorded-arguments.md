# Parity update: what the journal keeps of a tool call

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file.

## Section 3, the process-execution row

**Today** gains:

> A credential typed at a terminal is not written down: `terminal_send` takes a
> `secret` flag, the terminal receives the real text, and the journal keeps the
> `<redacted>` sentinel in place of it - in the call, in the assistant message
> that carried it, and in the streamed chunk, because the model said it too.
> `shell` and `shell_run` take the same flag over their command line.

**Gap** gains:

> A password the model does not flag is still recorded. The reliable signal is
> the terminal's `ECHO` state and it is not readable from the pty master in
> this arrangement, so there is no automatic detection to build on; the tool
> descriptions and the operator docs say plainly that a terminal journal holds
> whatever was typed into it.

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
| — (raised by the presentation lane, no upstream counterpart) | `crates/turn/tests/tool_recording.rs`, `crates/exec/tests/upstream_terminal_tools.rs` | What the journal keeps of a call's arguments | TC-TOOL-RECORD-1..4, TC-PORT-TERM-42. The exec case asserts on the *whole journal* rather than on `tool/call`, which is how it caught the two records the first fix would have missed |
