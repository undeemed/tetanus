# Parity update — the hook runner

Slice: `core-hook-runner`. Lane: engine/core (phase ②, hooks block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/runner.ts`, `tests/runner.spec.ts` | `crates/hooks/src/runner.rs`, `crates/hooks/tests/runner.rs` | TC-HOOK-RUN-1..12 |

## Structural difference — the executor is a trait this crate owns

Upstream's runner takes a `ShellExecutor` from `@deepseek-ai/dsh-shell`.
`crates/hooks` declares its own narrow `HookExecutor` seam instead: run one
command with a timeout, stdin, a working directory and an environment, and
report exit code plus streams.

Two reasons, and neither is expedience:

- Running commands is a service this crate *consumes*. A shell crate owns that,
  and a separate lane is building it; a dependency edge from hooks to shell
  would couple two lanes' schedules for no behavioural gain.
- Upstream's own suite duck-types `ShellExecutor` for exactly this reason, and
  says so: the real executor is exercised end-to-end by the bridges that
  consume the library, not by the protocol's tests.

When the shell lane lands its executor, implementing `HookExecutor` for it is
the whole of the integration. Nothing here changes.

## Cases beyond the upstream suite

- **TC-HOOK-RUN-10** — a failed run is still timed. The duration goes on the
  `hook/result` event, and a fault reporting no duration leaves a gap in the
  audit trail exactly where someone is looking.
- **TC-HOOK-RUN-11** — a hook that cannot run can never block the turn, over
  three distinct failure shapes. This is the property the whole error path
  exists for: a deployment whose hook binary is missing must not find every
  tool call denied. The mutation check confirms it bites — decoding an
  infrastructure fault as exit 2 fails two cases.
- **TC-HOOK-RUN-12** — an absurd per-hook timeout saturates rather than
  wrapping. The seconds-to-milliseconds conversion multiplies a value that came
  from a configuration file, and a wrapped result would give a hook
  microseconds and read as one that always times out.

## Changelog row

| 2026-08-21 | The hook runner ported (`crates/hooks/src/runner.rs`, TC-HOOK-RUN-1..12). It is thin plumbing over an executor, and the rule worth the slice is what it does when the executor fails: a hook that could not be run at all becomes an outcome with no exit code and the fault on stderr, which the codec's rules make non-blocking because only exit 2 blocks. A deployment whose hook binary is missing must not find every tool call denied, and TC-HOOK-RUN-11 pins that over three failure shapes - the mutation check confirms it, since decoding a fault as exit 2 fails two cases. The executor is a narrow `HookExecutor` trait this crate declares rather than a dependency on the shell crate: running commands is a service this crate consumes, a separate lane owns it, and upstream's own suite duck-types the same seam for the same reason. Two more cases go beyond upstream: a failed run is still timed, because the duration is durable on `hook/result` and a gap there is exactly where a reader will look; and an absurd per-hook timeout saturates rather than wrapping, since the seconds-to-milliseconds conversion multiplies a configured value and a wrapped one would silently give a hook microseconds. |
