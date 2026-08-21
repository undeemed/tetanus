# Parity update — hook stdin payloads

Slice: `core-hook-payloads`. Lane: engine/core (phase ②, hooks block).

## Section 4 — partially ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| payload builders of `hooks-claude-code/src/index.ts` and `hooks-codex/src/index.ts` | `crates/hooks/src/payload.rs`, `crates/hooks/tests/payload.rs` | TC-HOOK-PAY-1..12 |

Upstream has no payload suite: these shapes are asserted incidentally by
`bridge.spec.ts` driving a whole agent. Porting them as their own module is
what lets the two dialects be compared side by side, which is where the parity
risk actually is.

## Deferred, with the reason — the bridges themselves

`hooks-claude-code/tests/bridge.spec.ts` (446 lines) and
`hooks-codex/tests/bridge.spec.ts` (238 lines) are **not** ported. They drive a
full `Agent` through cordis extension points — `PreToolUse`, `PostToolUse`,
`SessionStart`, `UserPromptSubmit`, `Stop` — deciding *when* a hook fires.

The turn engine has no interception points to register against. Building them
is turn-engine work that overlaps the tool pipeline, and inventing a private
set here would be a second, competing interception surface.

So the bridges split cleanly in two, and this is the separable half:

- **payloads** — pure, dialect-defining, all four cross-dialect differences.
  Landed here.
- **wiring** — registration against interception points. Blocked on those
  points existing; not blocked on anything in this crate.

Nothing in `payload.rs` changes when the wiring lands.

## The four differences that would silently break a real hook

| | Claude Code | Codex |
| --- | --- | --- |
| no transcript yet | `""` | `null` |
| `tool_input` | the arguments, verbatim | just `{ "command": … }` |
| every payload also carries | — | `model`, `permission_mode` |
| turn-scoped events also carry | — | `turn_id`, as a **string** |

A hook written for one dialect reads the other's payload as missing data rather
than as an error, so each is a case. The mutation check confirms all four bite.

## Cases beyond the upstream suite

- **TC-HOOK-PAY-10** — the empty-string/null split on an absent transcript. One
  line in each builder, and exactly what a rewrite loses.
- **TC-HOOK-PAY-11** — a call with no `command` still gets the key under Codex,
  because its hooks index into `tool_input.command` and an absent key is a
  different failure from an empty command.
- **TC-HOOK-PAY-12** — arguments that are not an object are survivable in both.
  Arguments come from the model, and a payload builder that panicked on a
  malformed one would fail a turn from a hook that was only watching.

## Changelog row

| 2026-08-21 | The hook stdin payloads ported (`crates/hooks/src/payload.rs`, TC-HOOK-PAY-1..12): what each dialect tells a hook is happening. Upstream has no payload suite - these shapes are asserted incidentally by `bridge.spec.ts` driving a whole agent - so porting them as their own module is what puts the two dialects side by side, which is where the parity risk lives. Four differences would each silently break a real hook, and each is now a case: an absent transcript is `""` in Claude Code and `null` in Codex; `tool_input` is the arguments verbatim in one and narrowed to `{command}` in the other; every Codex payload carries `model` and `permission_mode`; and the turn-scoped Codex events carry `turn_id` as a string. Three cases go beyond upstream, all about what a hook reads when something is missing rather than wrong. The bridges themselves - registering these against `PreToolUse` and friends - are deliberately not ported: the turn engine has no interception points to register against, building them is turn-engine work that overlaps the tool pipeline, and inventing a private set here would be a second competing interception surface. The split is clean and nothing in this module changes when the wiring lands. |
