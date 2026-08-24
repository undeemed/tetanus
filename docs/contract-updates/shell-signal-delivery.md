# Contract note: what "delivered" means on a terminal signal

Written by the process-execution lane, for the boundary lane to fold into
[../interface-contract.md](../interface-contract.md). Nothing here edits that file, and
`docs/contract-updates` is empty on master after the last fold, so this is the only copy.

## The narrow point

`terminal_signal` answers, and every surface draws, a line of the form:

```
delivered SIGINT to foreground process group 4213
```

Until this slice that sentence could be false in the way that matters: the harness had been started
in the background, every child had inherited an ignored `SIGINT`, and the signal was delivered to a
process that ignored it. `killpg` reported success because delivery succeeded. The parity note
[`shell-signal-inheritance.md`](../parity-updates/shell-signal-inheritance.md) has the mechanism.

That specific falsehood is fixed - children no longer inherit the harness's ignore. What remains is
a narrower statement, and a surface should not round it up:

> **`delivered` means the signal was delivered, not that the process obeyed it.** A program is
> entitled to handle or ignore a signal - `trap '' INT` is a legitimate thing for a script to do -
> and a harness cannot promise compliance. What it can now promise is that the disposition being
> exercised is the *program's own*, not one it inherited from whoever launched the harness.

## What a surface should do with that

Nothing new is required, and one thing should be avoided: do not render `delivered` as "stopped".
The honest reading for a reader is *the signal reached the command's process group*; whether the
command ended is what the next `terminal_read` or the session's status shows. A presentation that
wants to say "stopped" should say it from the observed exit, not from the delivery.

## No type changes

No boundary type changes here, and none are proposed: the result already carries the group it
reached. This is a note about the meaning of an existing string, filed so the next person to read
that string in a panel does not have to re-derive what it can and cannot promise.
