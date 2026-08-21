# Parity update — hook matcher

Slice: `core-hook-matcher`. Lane: engine/core (phase ②, hooks block).

Rows for `docs/parity.md`, held here so four lanes can land in parallel without
colliding on that file. Merge them into section 4 and the changelog when the
shared doc is next recomposed.

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/matcher.ts`, `tests/matcher.spec.ts` | `crates/hooks/src/matcher.rs`, `crates/hooks/tests/matcher.rs` | TC-HOOK-MATCH-1..11 |

This is the first row of the `hooks/*` block and the first file of the new
`crates/hooks` crate.

## Section 5 — deliberate difference

**Rust regexes are not JavaScript regexes.** Upstream compiles a matcher with
`new RegExp`. This uses the `regex` crate, which has no backreferences and no
lookaround, because it guarantees linear-time matching. A pattern using either
is valid upstream and is refused here.

The difference is a widening of what `matcher_diagnostic` refuses, which is the
safe direction. The unsafe direction would be accepting a pattern that then
never fires: a deployment sees a hook it configured, that the harness reported
no problem with, silently never running. TC-HOOK-MATCH-11 pins the two
functions against each other so they cannot drift into that state — a refused
matcher must select nothing, for every pattern in the suite.

Two consequences worth stating plainly:

- A deployment migrating a Claude Code or Codex hook config that uses
  lookaround gets a diagnostic naming the pattern, not silence.
- Adopting a JS-compatible engine later would *narrow* what is refused, so it
  is a compatible change: nothing that works today would stop working.

## Changelog row

| 2026-08-21 | The hook matcher ported (`crates/hooks/src/matcher.rs`, TC-HOOK-MATCH-1..11), opening the `hooks/*` block and the `crates/hooks` crate. A matcher is the pattern beside each configured hook that decides whether it fires, and the two dialects read one pattern differently: Claude Code treats word-and-pipe patterns as literal alternatives, so `Bash` selects `Bash` and not `BashOutput`, while Codex has no literal path and reads the same pattern as `/Bash/`, which selects both. Getting that split wrong fires a hook for a tool it was never configured for, which is why it is two cases on the same input rather than one. Matching contains its own failures - an uncompilable pattern selects nothing rather than panicking, because matching happens inside a running turn - and `matcher_diagnostic` is the parse-time half that catches the pattern before it gets there. The engine is the `regex` crate rather than JavaScript's, so lookaround and backreferences are refused where upstream accepts them; that is recorded as a deliberate difference, and TC-HOOK-MATCH-11 pins the diagnostic and the matcher against each other so the refusal can never become a hook that is accepted and silently never runs. A mutation check confirms the suite bites: dropping the literal fast path, narrowing the match-all sentinels, panicking on a bad pattern, and blinding the diagnostic each fail two or three cases. |
