# Parity update: a persistent shell keeps what its bound drops

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file. `docs/parity-updates` was empty on
master before this, so **this file is the only copy of what follows**.

## 1. Two clauses

**Spill, for the persistent half.** The row records that a one-shot command keeps the whole of its
output when the capture bound drops part of it. A persistent shell did not: its scrollback dropped
the beginning and the result said only that it had been cut. The same argument applies without
change - only the producer holds the bytes it is dropping - and the commands that reach a session's
bound are exactly the ones whose beginnings matter: a build, a test run, a log tail, where the first
error is at the top and the tail is the part the reader already has.

**The credential words the backstop did not know.** The gap named *a "PIN", a one-time code, a
program that asks for nothing in words*. Two of the three close: `passcode`, `pin`, `verification
code`, `one-time code` and `otp` join `password` and `passphrase`. The third does not close and
cannot by this mechanism; the floor for it is the sentence already in the tool descriptions.

## 2. The part worth keeping: when the dropped bytes exist

The first implementation opened the artifact when the run loop **noticed** that the bound had
dropped something. That is always too late, and the case said so precisely: it asked for 3,000 lines
and the file held the last 1,915.

A bounded buffer forgets inside `push`. That is the only moment the forgotten bytes still exist -
any reader polling from outside arrives afterwards. So `Transcript` now tells a listener what it is
*about* to forget, with the absolute position it started at, and a session listens for the length of
one command:

- a drop that begins before this command did belongs to an earlier one, and is filed under neither;
- the artifact is two halves joined in order - what was forgotten while the command ran, and what
  was still retained when it ended;
- the handle is removed by a guard rather than at the return, because a command can also end by
  timing out, by interrupt, or by the shell dying, and a handle left installed would file the next
  command's dropped output under this one's name.

This is the same shape `crates/exec/src/proc.rs` already had for one-shot commands, where the seam
controls the buffer directly and can write before it drains. Stating it once here means the next
person to bound a stream in this workspace has the rule: **if you drop bytes, hand them over at the
drop, not at the notice.**

## 3. Section 3 row

**Today** gains:

> A persistent shell keeps the whole of a command's output when its scrollback drops part of it, and
> `shell_run`'s truncation notice names the artifact exactly as `shell`'s does; the transcript hands
> over what it is about to forget, so the artifact is the command's whole output rather than its
> tail.

**Gap** loses *a "PIN", a one-time code* from the credential clause, keeping the honest remainder:

> A program that asks for a credential in words no list anticipates still records what the model
> typed unless it set the flag.

## 4. Section 4 rows

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `subprocess-local`'s spill file, applied to the persistent half | `crates/exec/tests/upstream_terminal.rs` | That a session's dropped output is kept whole | TC-PORT-TERM-47. It reassembles all 3,000 lines, which is what caught the first implementation keeping 1,915 |
| — (this lane's own backstop) | `crates/turn/tests/tool_recording.rs` | Which words mean a credential is being asked for | TC-TOOL-RECORD-5, widened. `pin` is three letters, so the case pins the edges too: `spinning up the container` is not a prompt |

## 5. One process note, since it cost a cycle

An edit to `looks_like_a_password_prompt` silently did not apply: the file had been reformatted
since it was read, the search text no longer existed, and the build stayed green over an unchanged
function. Only the case caught it. Where a change is made by search-and-replace rather than by hand,
the case has to exist first - a green build proves nothing about an edit that never landed.
