# Parity update: the terminal tools over the pseudo-terminal

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file: every lane collides there.

This note continues [shell-process-execution.md](shell-process-execution.md) and
[sweep-shell-sandbox-fs.md](sweep-shell-sandbox-fs.md). Those two put the process seam, the shell
backends, the persistent (pipe-backed) shells and the `shell_*` tools in place, and the pty slice
that followed put a real pseudo-terminal under them. Each named the same remaining clause: *the
interactive behaviour over the pseudo-terminal - a viewport, scrollback paging, an owner-scoped
registry, and readiness inferred from a prompt - and the tools over it*. That clause is now served,
and this note is what replaces it.

## 1. Section 3 row

In the `shell/*`, `terminal/*`, `subprocess/*` row, the **Today** column keeps everything it says
and gains:

> Persistent *terminals* over that pseudo-terminal (`crates/exec/src/terminal.rs`): a shell driven
> one send at a time, a viewport of what changed since the last send, a bounded scrollback paged
> back through newest-line-first, a size that can change while the session runs, a `^C` that reaches
> the command rather than the shell, and a death that is reported rather than restarted. Readiness
> is *announced* rather than inferred - the shell is told to print an OSC 133 marker before every
> prompt (`crates/exec/src/sanitize.rs`), so a send settles when the shell says the command is over
> and the marker carries its exit status, with upstream's silence and deadline kept as the fallbacks
> for a program that prints no marker. An owner-scoped registry (`crates/exec/src/terminals.rs`)
> minting ids, holding names unique within one owner, refusing a foreign session as foreign, and
> closing everything on the way down. The six model-facing tools - `terminal_open`, `terminal_send`,
> `terminal_read`, `terminal_signal`, `terminal_close`, `terminal_list` - registered in the tool
> registry, scheduled by the ordinary pipeline (typing is a barrier, reading and listing are
> parallel-safe, results committed in model order), and holding the turn's own interrupt, so a
> stopped turn interrupts the command and leaves the session alive.

And the **Gap** column loses the whole first clause (`The interactive behaviour over the
pseudo-terminal … and the tools over it are the next slice`), keeping the rest: background jobs
(`run_in_background`) and the job store they need, spill files for output past the in-memory bound,
raw piped stdio for a protocol consumer, and a Windows host to run the PowerShell backend against.
One clause is added in its place:

> A terminal session's owner is a name the composition chooses, not an agent identity: the
> registry compares owners exactly and the engine gives each session its own registry, so the
> isolation exists, but "which agent" waits on there being agents.

## 2. Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `terminal/terminal-bash/tests/session.spec.ts`, `sanitize.spec.ts`, `local.spec.ts` | `crates/exec/tests/upstream_terminal_session.rs` | A shell on a real terminal: readiness, viewport, paging, signals, death | ported: TC-PORT-TERM-15..27. Upstream infers readiness from silence plus an exact syscall probe of whether the foreground process is blocked reading its terminal; tetanus is *told* by the shell's own prompt marker, so `stdin_read` is a fact and carries the command's exit status, which upstream's cannot (TC-PORT-TERM-15). Silence and the absolute deadline remain as the two fallbacks (TC-PORT-TERM-19), and a fifth wait reason - `interrupted` - has no upstream counterpart because upstream has no turn-level stop switch. Upstream keeps the terminal's echo in a viewport; this turns it off at startup (`stty -echo`), because the caller already knows what it sent and a shell with line editing echoed it twice. Its background sends need the job store |
| `terminal/terminal/tests/service.spec.ts` | `crates/exec/tests/upstream_terminal_registry.rs` | Ids, publication, ownership, names, listing, closing | ported: TC-PORT-TERM-28..33. Upstream's owner is an exact `Agent` and its disposal hangs off that agent's effect scope; here an `Owner` is an opaque name the composition chooses and a composition closes its own registry, because tetanus sessions have no owning agent yet - the comparison the registry makes is upstream's. Its `TerminalBackendCleanupError` and the aggregate rollback around a partially started session have nothing to restate: an open here either publishes or leaves nothing behind |
| `terminal/tool-terminal/tests/tools.spec.ts`, `render.spec.ts` | `crates/exec/tests/upstream_terminal_tools.rs`, `crates/cli/tests/terminal.rs` | What the model may call, and what happens when it does | ported: TC-PORT-TERM-34..40, TC-CLI-TERM-1. Every case drives a real `TurnEngine` over a real journal, for the reason the `shell_*` cases do. Upstream's `run_in_background` needs the job store, so no background argument is advertised rather than one that cannot be collected; its presentation callbacks (`presentCall`, `presentResult`, the terminal card) belong to the presentation lane; its `finalizeContent` cap is served by the renderer's own bound, because a tetanus tool returns text rather than a content-block list a later hook rewrites |

## 3. What is unrepresentable, and why

- **An agent-scoped owner.** Named above and in section 3's gap. The registry enforces exact-owner
  access today; what it cannot do is tie a session's life to an agent's, because there is no agent
  registry to tie it to. When there is, `Owner::new(agent.id())` is the whole change.
- **Background sends.** `run_in_background` returns a job id that `job_output` and `job_kill`
  collect, which is one feature with the job store behind it (`workflow/*`, `jobs/*` in section 3).
- **The presentation half.** Upstream's terminal card renders a viewport live as a send runs, off
  `readOutput()`. The seam for it exists - a send's viewport is a delta over a bounded transcript,
  and `TerminalSession::resize` is there for a surface that has a window - but what draws it is the
  presentation lane's, and the engine/presentation contract has no terminal vocabulary yet. Nothing
  in this slice adds one.
- **Full terminal emulation.** The sanitizer strips CSI, OSC and the short escapes and normalizes
  line endings; it does not maintain a screen. A program that draws with cursor movement (`htop`,
  `vim`) is therefore *runnable* here and not *readable*: the transcript holds what it wrote in the
  order it wrote it, not what a screen would show. Upstream defers the same thing deliberately, and
  a screen model is the honest name for what closing it would take.
- **A prompt-marker-free backend.** Bash announces its prompts because bash is told to
  (`PROMPT_COMMAND`). PowerShell has no equivalent that survives `-NoProfile`, so a pwsh terminal
  would settle every send on silence. It is registered as a backend type and refused loudly on a
  host without the binary; running one needs a Windows host, as the earlier note says.

## 4. Where the two families differ, for the row that has to describe both

Worth one line in section 3, because a reader seeing `shell_run` and `terminal_send` side by side
will ask: a pipe-backed session ends when a turn is stopped, because a shell reading a pipe cannot
be interrupted; a terminal-backed one interrupts the *command* and keeps the session, because a
terminal has a foreground process group to aim at. That is the difference the family exists for.
