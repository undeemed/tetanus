# Parity update: `shell/*`, `terminal/*`, `subprocess/*`

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file: every lane collides there.

## 1. Section 3 row

Replace the `shell/*`, `terminal/*`, `subprocess/*` row with:

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `shell/*`, `terminal/*`, `subprocess/*` | 32 | The process seam as `crates/exec`: one command with an argv nothing re-splits, an environment the caller listed rather than one inherited and scrubbed, a working directory, captured stdio bounded to its tail, an exit code or a named signal, incremental output handed to a sink while the command runs, and a termination that is a SIGTERM-to-SIGKILL ladder over the child's own process group, so grandchildren die with it and an orphan holding the output pipe is swept rather than left holding the turn open; two shell backends behind one trait (bash, and PowerShell so Windows is not designed out), each refusing loudly when its binary is absent instead of falling back to another shell; a one-shot executor that resolves a request against the deployment's defaults and caps before running it and renders upstream's `[stderr]`/`[timed out after Nms]`/`[killed by signal: X]`/`[exit code: N]` markers, with the parser that reads them back out of a replayed result; persistent shells that keep the working directory and exported variables between tool calls, run one command at a time, bound their transcript, and report a shell that died rather than restarting one underneath the caller; and the model-facing tools - `shell`, `shell_open`, `shell_run`, `shell_close`, `shell_list` - registered in the tool registry, scheduled by the existing pipeline as barriers and parallel-safe calls with results committed in model order, and holding the turn's own interrupt so a stopped turn kills the command it started | A PTY, and the terminal tools that only mean something with one (viewport, scrollback paging, terminal size, foreground-group signalling, prompt readiness); background jobs and the job store `run_in_background` needs; spill files for output past the in-memory bound; owner-scoped sessions; sandboxing (its own row); a Windows host to run the PowerShell backend against | ② for jobs and spill, ③ for the PTY and the sandbox |

## 2. Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `subprocess/subprocess-local/tests/spawn.spec.ts`, `process-exit.spec.ts` | `crates/exec/tests/upstream_process.rs` | What one command produces, and what ends it | ported: TC-PORT-PROC-1..18. TC-PORT-PROC-1..10 moved here with the seam (they were `crates/turn/tests/upstream_process.rs`); -11..-15 are the process-group half the old seam could not serve, -16..-18 the streaming half. Upstream's credential scrub has nothing to restate because the design is inverted - a child gets what the caller listed - and TC-PORT-PROC-6 states that instead. Its `pipe`/`inherit` stdio modes, its offset-based non-consuming readers and its spill files serve a protocol consumer this has not built; streaming here is one sink handed each piece as it arrives. Its E2B provider is a phase ③ backend |
| `shell/bash-local/tests/executor.spec.ts`, `shell/pwsh-local/tests/executor.spec.ts`, `shell/shell/tests/render.spec.ts` | `crates/exec/tests/upstream_shell.rs` | Backend resolution and refusal, defaults and caps, and the rendered result | ported: TC-PORT-SHELL-1..11. The sandbox families (`bash-sandbox`, `pwsh-sandbox`) and the escalation arguments belong to the sandbox row. `shell-env`'s `DSH_*` collection has no counterpart until a session header carries the facts to collect; TC-PORT-SHELL-6 restates the half that survives - the backend's environment is a default the caller beats. The background/`jobs` half needs a job store. The pwsh cases run on a host without PowerShell and report themselves skipped rather than passing for the wrong reason |
| `terminal/terminal-bash/tests/session.spec.ts`, `terminal/terminal/tests/service.spec.ts`, `shell/tool-bash-persistent/tests/tools.spec.ts` | `crates/exec/tests/upstream_terminal.rs` | A long-lived shell, its lifecycle, and what happens when it dies | ported: TC-PORT-TERM-1..12. Upstream's sessions are PTYs, so its viewport, scrollback pages, terminal dimensions, foreground-group inspection and signalling, and prompt-based readiness have nothing to restate: a shell reading a pipe has no terminal, and an exact marker is what a prompt was approximating. One decision is answered differently rather than ported: upstream resets a dead shell and prints a notice, and tetanus keeps the notice and drops the reset (TC-PORT-TERM-5), because a silent restart hands the model a shell in a state it did not create while the transcript says everything succeeded. Owner-scoping waits for a session to have an owner |
| `shell/tool-bash/tests/tools.spec.ts`, `integration.spec.ts`, `terminal/tool-terminal/tests/tools.spec.ts` | `crates/exec/tests/upstream_tools.rs`, `crates/cli/tests/shell.rs` | What the model may call, and what happens when it does | ported: TC-PORT-SHELL-12..19, TC-PORT-TERM-13..14, TC-CLI-SHELL-1..3. Every case drives a real `TurnEngine` over a real journal, because a tool that works in isolation and never reaches the next request is a tool the model cannot use. Upstream's `run_in_background` half needs the job store; its `sandbox_permissions`/`justification` arguments need the sandbox vocabulary; its presentation callbacks belong to the presentation lane. The tool is `shell` rather than `bash` because the backend is configuration here. `ok` on a result means the command succeeded, which is narrower than upstream's `isError`: a tetanus outcome carries the rendered text either way, so the flag can mean the plain thing while the markers carry the detail |

## 3. What is unrepresentable, and why

Named here so the reconciliation slice can quote it rather than re-derive it.

- **A PTY.** No pseudo-terminal is allocated, so there is no viewport, no terminal size, no
  `stty`-visible terminal, no foreground process group distinct from the session's own group, and no
  prompt to infer readiness from. Upstream's `terminal_send` wait reasons (`stdin_read`,
  `inferred_idle`, `timeout`, `session_exit`), its `terminal_read` paging, and its `terminal_signal`
  foreground targeting all follow from having one. What a model can observe without a PTY - state
  surviving between calls, an exit status per command, a bounded transcript, one command at a time,
  a death that is reported - is what TC-PORT-TERM-1..14 pin.
- **Background jobs.** `run_in_background`, `job_output` and `job_kill` are one feature with the job
  store behind them (`workflow/*`, `jobs/*` in section 3), so the tools advertise no background mode
  rather than advertising one that cannot be collected.
- **Spill files.** Upstream writes the complete stream to a temp file when the in-memory bound drops
  bytes, and reports the path. This reports the truncation without a path, because a spill file is a
  storage policy - where it lives, who deletes it, what a sandboxed reader may open - and inventing
  one per command inside the subprocess seam would settle those questions by accident.
- **The credential scrub.** `scrubbedParentEnv` exists because Node hands a child `process.env`.
  Nothing is inherited here, so there is no denylist to port; `Command::inherit_env` is the opt-in,
  and its name is the warning (TC-PORT-PROC-6).
- **Windows in practice.** The PowerShell backend is a value, an argv, a session invocation and a
  marker wrapper, and it is exercised as far as a POSIX host can: its shape is asserted, and its
  absence is asserted as a loud refusal (TC-PORT-SHELL-2, -3). Running commands through it needs a
  Windows host in CI.
- **Owner-scoped sessions.** Upstream's terminals belong to an agent, and another agent asking for
  one is told it does not exist. tetanus sessions are owned by the composition that opened them;
  the isolation that exists today is per-`ShellSessions` registry, which the engine builds per
  session.

## 4. Where the interrupt reaches

Worth a line in section 3's `core/*` gap list when it is next rewritten: "cancellation inside a
step" is still not served - an interrupt lands at the step boundary - but it now reaches *work
started by a tool*. A composition supplies one `Interrupt` through `tetanus_turn::boot::boot_with`;
the loop reads it and the shell tools read it, so a stopped turn terminates the process group of
whatever command was running (TC-PORT-SHELL-17, TC-PORT-PROC-17). The engine mints one per session,
because a switch shared across sessions would let an interrupt in one stop another.
