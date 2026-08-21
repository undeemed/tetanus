# Parity update — Codex hook configuration

Slice: `core-hook-codex-config`. Lane: engine/core (phase ②, hooks block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hooks-codex/src/config.ts`, `tests/config.spec.ts` | `crates/hooks/src/codex.rs`, `crates/hooks/tests/codex_config.rs` | TC-HOOK-CX-1..13 |

## Four deliberate differences from the Claude Code adapter

Same job, and the shape rules are the same for the same reasons. What differs
is dialect, not policy:

- **Five events, not seven.** `SubagentStop` is a real Codex event this adapter
  does not serve, and it is dropped exactly like one Codex never defined —
  serving half of an event is worse than not serving it.
- **No substitution.** A `${VAR}` in a command is the shell's business later.
  Rewriting it at parse time would change the command the deployment wrote.
- **`async: true` is refused and recorded.** This adapter runs a hook and waits
  for its answer; a background hook's answer would arrive after the decision it
  was meant to inform, so running it as if it were synchronous would silently
  change its meaning.
- **Two timeout spellings**, `timeout` and `timeoutSec`, because Codex takes
  both.

## Cases beyond the upstream suite

- **TC-HOOK-CX-11** — this dialect judges matchers as regexes. It is the
  difference that actually changes which hooks fire: a configured `Bash`
  selects only `Bash` under Claude Code and also `BashOutput` here. Pinning it
  at the parse boundary is what stops the two adapters sharing a mode by
  accident, and the mutation check confirms it — judging with the other
  dialect fails two cases.
- **TC-HOOK-CX-12** — a lone `async` hook empties its group, and an empty group
  is not then judged for its matcher. Two refusals that must not add up to a
  fatal error.
- **TC-HOOK-CX-13** — `async: false` still runs. Only the literal `true` means
  background; reading the key's presence as a refusal would silently disable a
  hook that wrote the field out to say "no".

## Changelog row

| 2026-08-21 | Codex's hook configuration ported (`crates/hooks/src/codex.rs`, TC-HOOK-CX-1..13), the second adapter's parse. The shape rules match Claude Code's and for the same reasons - lenient about malformed entries so one bad stanza cannot stop a boot, strict about a matcher on a runnable group so a hook that could never fire is refused rather than silently dead - and four things differ by dialect rather than by policy: five served events rather than seven, with a real Codex event this adapter does not serve dropped like an unknown one because serving half of it would be worse; no parse-time substitution, since a `${VAR}` is the shell's business later and rewriting it would change the command the deployment wrote; `async: true` refused and recorded, because a background hook's answer would arrive after the decision it was meant to inform; and two accepted spellings of the timeout. Three cases go beyond upstream: this dialect judges matchers as regexes, which is the difference that actually changes which hooks fire and which the mutation check confirms by failing two cases when the other dialect judges; a lone async hook empties its group without its matcher then being judged; and `async: false` still runs, since only the literal `true` means background. |
