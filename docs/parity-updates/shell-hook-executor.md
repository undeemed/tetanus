# Parity update: the executor a hook runs through

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file: every lane collides there.

This closes the last clause of the `shell/*`, `terminal/*`, `subprocess/*` row that had a named
consumer waiting on it. [shell-row-closed.md](shell-row-closed.md) served the MCP half of *raw
piped stdio handed to a protocol consumer … (MCP on stdio, out-of-process hooks)*; this is the
second half.

## 1. Section 3, the process-execution row

The **Today** column gains:

> The executor a deployment's hooks run through (`crates/exec/src/hooks.rs`): a configured hook is a
> real child with an argv nothing re-splits, an environment named rather than inherited, and a
> timeout that ends the whole process group, so a hook that hangs after starting a watcher does not
> leave the watcher behind. An unrunnable hook is reported as infrastructure rather than as the
> hook's answer, which is what lets the protocol keep it non-blocking.

Nothing moves out of the **Gap** column that was not already accounted for there; the clause this
closes was written as one item covering two consumers, and both are now served.

## 2. Section 3, the `hooks/*` row - one line for its owner

That row's Gap reads: *Only the bridges: registering these against `PreToolUse`, `PostToolUse`,
`SessionStart`, `UserPromptSubmit` and `Stop`. Waits on interception points in the turn engine -
nothing in `crates/hooks` blocks it, and no payload or protocol behaviour changes when it lands.*

That is still true, and one clause can now be added to it: **nothing in `crates/exec` blocks it
either.** `HookExecutor` had exactly one implementation in this workspace - a recorder in the hook
lane's own suite - so the bridge slice would have had to write a real one as a side quest. It has
one now (`tetanus_exec::hooks::ShellHookExecutor`), asserted against real hook scripts and driven
through `run_hook` rather than around it, so what the bridge supplies is an executor it constructs
rather than an executor it writes.

The bridge itself is deliberately **not** in this slice. It needs interception points in the turn
engine, and inventing them from the process lane would settle where a hook fires - the one question
the hook protocol's own row exists to answer.

## 3. Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `hooks/hook-protocol`'s runner over its `ShellExecutor`, which its suite duck-types | `crates/exec/tests/upstream_hook_exec.rs` | A configured hook as a real child, and what the protocol makes of it | ported: TC-PORT-HOOK-1..6. The cases drive `tetanus_hooks::run_hook`, not the executor alone: what has to be true is not that a command ran but that the protocol reads the result as a hook - a `2` blocks with its reason, a hook that never ran blocks nothing. Which hook fires on which event, the payloads, and the merge of several answers stay with the hook lane's own suite (TC-HOOK-*), which is right to use a recorder: this file replaces the recorder at the composition, not in those cases |

## 4. Two decisions that differ from upstream, and why

- **The environment is a list, not a scrub.** Upstream hands a hook `process.env` minus a denylist
  (`scrubbedParentEnv`). Nothing is inherited in this workspace, so the question is inverted:
  `HookEnv::passed` names what reaches a hook, defaulting to what a shell script cannot work without
  (`PATH`, `HOME`, and the locale and terminal variables), and a deployment that wants more says
  which more. The denylist shape has the failure mode this avoids: every credential added later is
  exposed until somebody remembers to extend the list.
- **The timeout ceiling is the hook protocol's, not the shell tool's.** A hook's own configured
  timeout is clamped, as every timeout here is, but by ten minutes -
  `DEFAULT_HOOK_TIMEOUT_MS` - rather than by the ceiling a model's `shell` call is held to. A hook
  and a model's command are different kinds of thing, and the shorter cap would silently shorten a
  hook a deployment had asked for.

## 5. What a hook still does not get

- **Sandboxing by default.** A hook is a program an operator wrote in a settings file, not one a
  model wrote, so it runs under the executor's ordinary policy rather than confined tighter than
  the harness itself. The policy is the place to say otherwise, once, for commands and hooks
  together; the executor takes a `ShellConfig`, so a deployment that wants confined hooks already
  can.
- **Streaming.** A hook's output is collected, not streamed: the protocol decodes one complete
  answer, so there is no consumer for a partial one. The seam has a sink
  (`ShellExec::run_with`) the day a presentation wants to show a slow hook working.
