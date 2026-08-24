# Parity update: the backends, the tool, and the sandboxing boundary (slice `coderuntime-backends`)

Extends [`coderuntime-seam.md`](coderuntime-seam.md); fold both in one pass.
Not applied to [`../parity.md`](../parity.md) here, because every lane edits
that file.

## 1. Section 3, the `code-runtime/*`, `e2b/*` row

Supersedes the row proposed by the seam slice.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `code-runtime/*`, `e2b/*` | 11 | One trait for evaluating a model-written program and a structured result (value, ordered logs, upstream's six-kind failure class, duration). A local backend evaluates a small deterministic language of tetanus's own on a worker thread, under fuel, a wall-clock ceiling and one output ledger over logs and value together; a runaway program is stopped and its thread reclaimed. A remote backend behind the same trait submits, polls, fetches and cancels against a provider seam, owning one shared sandbox with transactional setup and idempotent teardown. Both are registered as the `run_code` tool and dispatched by the ordinary pipeline, so a program that fails is a failed tool call and the turn survives | The typed rejection classes a binding namespace may declare; a real-language backend, which needs the sandbox modes named in the follow-up below; upstream's OOM containment, which needs a per-worker heap cap; async bindings, which need a bridge a synchronous evaluator does not have | ② |

## 2. Section 4, the port table

Two rows, beside the seam row from the previous slice.

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `code-runtime/code-runtime-worker-thread/tests/runtime.spec.ts` | `crates/coderuntime/tests/budgets.rs`, `crates/coderuntime/tests/turn.rs` | What a program cannot outlive, and what a turn does with one that failed | part ported: TC-PORT-CODERT-15..28 for a hot loop ended at the compute budget with its worker reclaimed, a run that mostly waits ended at the wall clock with waiting costing no fuel, an abort mid-run, runaway logs failing at the cap with the fitting prefix kept, an oversized value failing without substitution, logs and value counted in one ledger, a panicking binding contained as `worker-exit` with the host healthy after it, shutdown ending an in-flight run, a binding that never returns bounded from outside and named as the reason, budgets that belong to the caller, a ledger nothing can talk past, and then the pipeline: a turn that runs a program and reads its value in the next step, an infinite loop failing its call while the turn goes on, a tool description generated from the bindings, and an empty program refused before a worker exists. Upstream's OOM case needs a per-worker heap cap, which a Rust thread cannot have without an allocator this crate has no business installing. Its forged-port family is about a serialization boundary this backend does not have - the evaluator runs in the host's address space - so TC-PORT-CODERT-24 pins the property those cases protect instead. Its TypeScript-specific cases (erasable syntax, JS prototype graphs) are unrepresentable; TC-PORT-CODERT-13 restates the portable half of the prototype question |
| `e2b/e2b/tests/e2b.spec.ts` | `crates/coderuntime/tests/upstream_e2b.rs` | Owning one remote sandbox, and running a program in it | part ported: TC-PORT-E2B-1..9 for the same program through the same seam on a remote substrate, one sandbox created and shared and killed once, a setup failure rolling the creation back with the original failure preserved even when the rollback fails too, a kill of an already-gone sandbox as success, poll-until-settled and a cancel that reaches the provider, a run past its ceiling cancelled rather than waited for, a key required and never sent into the sandbox with the configured cwd travelling instead, a program carrying bindings refused, and a sandbox that died under a job reported as `worker-exit` with the next run getting a new one. Upstream's E2B package is a sandbox *owner* shared by a filesystem adapter and a subprocess adapter, so its shell quoting, its login-shell control home and its SDK re-exports have nothing here to attach to; the submit/poll/fetch/cancel shape is this lane's brief rather than a file to port line for line, because upstream's code runtime and its e2b package never meet |

## 3. The sandboxing boundary, as the brief asks

**What this runtime relies on today, stated exactly.** The local backend
executes no native code. It opens no file, makes no connection, starts no
process, and reads no environment: a program can compute, and it can call the
host bindings the composer passed in. Its enforcement is therefore entirely
in-process and entirely this crate's:

- **fuel**, a step budget the evaluator spends on every step, which is what
  ends a loop that never ends;
- **a wall-clock ceiling**, which bounds a run that mostly waits, plus an outer
  bound for the one case the flag cannot reach - a host binding that never
  returns;
- **one output ledger** over logs and completion value together, checked as
  output is produced rather than after;
- **the binding surface itself**, which is the only way out of the evaluator
  and is fixed by the composer, not by the program.

It relies on **no** filesystem fence and **no** OS sandbox, because it reaches
neither. `tetanus_turn::fs`'s path containment and the shell lane's sandbox
modes are not being duplicated here; they are not being used here either, and
that is a property of what this backend does rather than a gap in it.

The remote backend relies on the provider's own isolation and on nothing of
tetanus's: a program submitted to a sandbox runs under whatever that provider
enforces, which is why the key is never forwarded into it (TC-PORT-E2B-7).

**The named follow-up.** `CODERT-FOLLOWUP-1: a real-language backend runs under
the shell lane's sandbox modes.` The moment a backend hands a program to an
actual interpreter - `python3`, `node`, anything - every guarantee above stops
applying: fuel does not exist outside this evaluator, and the program can open
files and sockets. That backend must be built on the shell lane's `crates/exec`
and must run under its sandbox policy rather than spawning a process of its
own, for the same reason this crate does not spawn one today. It is a
follow-up rather than a plan: the mode vocabulary, the escalation stamp and the
kernel backends are that lane's to settle, and this note records the dependency
so the sequencing is deliberate instead of discovered.

## 4. Changelog row

| 2026-08-21 | The code runtime's two backends and its tool (`crates/coderuntime`, TC-PORT-CODERT-15..28, TC-PORT-E2B-1..9), closing the last in-scope parity area that had no implementation anywhere. The local backend's containment claim is stronger than "the call returned": a Rust thread cannot be killed, so the evaluator reads a stop flag on every step and the cases assert the worker count falls back to zero - which is what stops a harness accumulating one wedged thread per runaway program. Writing them found two defects. The clock was read every 512 steps, so a loop spending 20ms per iteration inside a host binding ran for two seconds before its ceiling was looked at; a binding call now forces the next step to read the clock. And the outer bound joined the worker unconditionally, which made a run whose binding blocks for thirty seconds take thirty seconds to return - the very thing the bound exists to prevent - so a worker still inside a host binding is now reaped in the background and stays counted as live, because that count should tell the truth rather than a comfortable number. The remote backend restates upstream's e2b *ownership* rules rather than its SDK: one shared sandbox, transactional setup, idempotent teardown, a cancel that reaches the provider. The sandboxing boundary is stated rather than guessed: this runtime relies on fuel, a ceiling, an output ledger and a fixed binding surface, and on no OS enforcement at all because it reaches nothing - with `CODERT-FOLLOWUP-1` naming what a real-language backend would need from the shell lane's `crates/exec` before it could exist. |
