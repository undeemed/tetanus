# Parity update: closing the `shell/*`, `terminal/*`, `subprocess/*` row

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file: every lane collides there.

This is the fourth and last note from this lane. [shell-process-execution.md](shell-process-execution.md)
built the seam, [sweep-shell-sandbox-fs.md](sweep-shell-sandbox-fs.md) swept the row against the
code, and [shell-terminal-tools.md](shell-terminal-tools.md) served the pseudo-terminal and its
tools. What was left in the Gap column after those was four clauses. Three are now served and the
fourth is answered in part and named precisely for the rest.

## 1. The Gap column, clause by clause

| Clause as it stood | Now |
| --- | --- |
| *The interactive behaviour over the pseudo-terminal … and the tools over it are the next slice* | **Served**, by the previous note (TC-PORT-TERM-15..40) |
| *Spill files for output past the in-memory bound* | **Served.** The storage lane wrote the policy (`tetanus_core::spill`), so this put the producer on the other end of it: the artifact is opened on the first overflow, holds the complete stream because the buffer still had it at that instant, and the truncation notice a model reads carries the locator (TC-PORT-PROC-19..21, TC-PORT-SHELL-20). The binary keeps them beside the session's journal (TC-CLI-SHELL-4) |
| *Raw piped stdio handed to a protocol consumer, which waits on the first consumer that needs it* | **Served.** The consumer arrived (`crates/mcp`) and had spawned its own child; `tetanus_exec::piped` is the seam and MCP is on it (TC-PORT-PROC-22..26). It closed a leak rather than tidying: the old transport killed the server, so a server that started helpers of its own left them running |
| *Owner-scoped sessions, which wait on a session having an owner* | **Served.** `SessionTools` now carries a `ToolScope` - the session's id and where its journal lives - so a terminal's owner is the session that opened it and the registry's exact-owner comparison can refuse something |
| *Background jobs (`run_in_background`) and the job store they need* | **Half served, and the half that is not is named below.** On a terminal it needs no store: `wait_ms` starts work and leaves it running, `terminal_read` collects it, `terminal_signal` stops it (TC-PORT-TERM-41). For one-shot `shell` it genuinely needs the store |

## 2. Section 3 row

The **Today** column gains:

> Output past the capture bound kept whole rather than only reported: the artifact is opened the
> first time a bound is exceeded and never before, it holds the complete stream, and the truncation
> notice names it, with the binary filing them beside the session's own journal. One seam for a
> child this harness *talks to* as well as one it waits for - a protocol peer on stdio, ended over
> its own process group so a server's helpers go with it, which is what `crates/mcp` now starts its
> servers through. Terminals owned by the session that opened them, with the id carried into the
> registry rather than assumed. Long work started and collected later on a terminal, through a
> per-send wait bound rather than a job.

The **Gap** column becomes:

> `run_in_background` for the one-shot `shell` tool, which needs the job store from `workflow/*`,
> `schedule/*`, `jobs/*`: a one-shot's process group is swept when its call returns, so there is
> nothing to collect from afterwards, and a second store built here would leave that row with two to
> reconcile. A screen model for programs that draw with cursor movement. A prompt marker for
> PowerShell. A Windows host to run the PowerShell backend against.

## 3. What stays open, and exactly why

**`run_in_background` for `shell`.** Not built, and not for want of a place to put it. The store it
needs must offer three things this lane must not invent privately, because the next reader of
"what is running" would find two answers: an id minted somewhere a later tool call can name; a
bounded, consuming reader over the output produced since the last read (upstream's `readOutput`);
and a cancel that reaches the work rather than the handle. `crates/exec` has the third already
(`terminate_group`), and the first two are exactly what a job store is. When `jobs/*` lands, the
shell side is a `run_in_background` argument on the existing tool that starts the same
`proc::Command` without awaiting it and hands the store the group id - the seam is ready for it, and
the parity row should say so rather than describing the tool as finished.

**A screen model.** The sanitizer strips CSI, OSC and the short escapes and normalizes line
endings; it does not maintain a screen. So `htop` and `vim` are *runnable* here and not *readable*:
the transcript holds what the program wrote in the order it wrote it, not what a screen would show.
Closing this is a terminal emulator - a grid, a cursor, scroll regions, attributes - and it is a
crate-sized piece of work whose only consumer today is a presentation that does not exist yet.
Upstream defers it the same way, and for the same reason.

**A prompt marker for PowerShell.** Readiness is exact here because bash is told to print an OSC 133
marker before every prompt. PowerShell's equivalent lives in `$PROFILE`, which `-NoProfile`
deliberately does not load, and this crate will not drop `-NoProfile` - a session whose behaviour
depends on the operator's dotfiles behaves differently in every deployment. A pwsh terminal would
therefore settle every send on silence, which works and is worse. What would close it: a
`-Command` preamble that defines `prompt` and then hands control to the REPL, which needs a
Windows host to prove.

**A Windows host.** Unchanged from the first note in this lane. The PowerShell backend is a value,
an argv, a session invocation and a marker wrapper; its shape is asserted and its absence is
asserted as a loud refusal (TC-PORT-SHELL-2, -3). Running commands through it needs a Windows host
in CI, and nothing in this workspace can substitute for one.

**The presentation half.** Upstream renders a live terminal card off its send operation's
`readOutput`. The engine side of that exists - a send's viewport is a delta over a bounded
transcript, and `TerminalSession::resize` is there for a surface with a window - but what draws it,
and the contract vocabulary that would carry it, belong to the presentation lane and to
`docs/interface-contract.md`. This lane adds no terminal vocabulary to the boundary.

## 4. Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `subprocess/subprocess-local/tests/spawn.spec.ts` (its spill file), `packages/spill` | `crates/exec/tests/upstream_process.rs`, `crates/exec/tests/upstream_shell.rs`, `crates/cli/tests/shell.rs` | What happens to output the bound drops | ported: TC-PORT-PROC-19..21, TC-PORT-SHELL-20, TC-CLI-SHELL-4. Upstream writes a temp file; tetanus writes into the storage lane's spill store, so one kind of artifact exists rather than two, and a deployment that configures none keeps the old plain notice |
| `packages/mcp`'s stdio transport, the piped half of `subprocess-local` | `crates/exec/tests/upstream_piped.rs`, and `crates/mcp`'s own suite over the new seam | A child this harness talks to | ported: TC-PORT-PROC-22..26. Framing stays in `crates/mcp`: this seam hands over two pipes and never reads them. Upstream's offset-based non-consuming readers belong with the job store |
| `terminal/tool-terminal/tests/tools.spec.ts` (`run_in_background`) | `crates/exec/tests/upstream_terminal_tools.rs` | Starting work and collecting it later | ported *as a substitution*: TC-PORT-TERM-41. No job id, because no job store: the session is the collection point, and `wait_ms` is what makes leaving a command running a thing a model asks for rather than a timeout it suffers |
