# Parity update — Claude Code hook configuration

Slice: `core-hook-claude-config`. Lane: engine/core (phase ②, hooks block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hooks-claude-code/src/config.ts`, `tests/config.spec.ts` | `crates/hooks/src/claude_code.rs`, `crates/hooks/tests/claude_code_config.rs` | TC-HOOK-CC-1..14 |

## The design worth naming: two failure policies in one parser

Parsing is **lenient about shape** and **strict about matchers**, and the split
is the point of the module:

- A malformed entry is dropped, never fatal. A settings file is hand-edited,
  and one bad stanza must not stop the harness booting; the hook it described
  simply does not run.
- An uncompilable matcher on a *runnable* group is fatal, naming the event.
  Those hooks would never fire, silently, and a hook a deployment believes is
  guarding something but which cannot match is worse than no hook at all.

Three rules interact and their order matters, which is why TC-HOOK-CC-11..13
exist: an unsupported event is ignored before its groups are read; a matcher on
an event with no subject is discarded before it is judged; and a group with
nothing runnable is dropped before its matcher is judged. Each of those keeps a
fatal error from firing over configuration that could not have had an effect.

## Cases beyond the upstream suite

- **TC-HOOK-CC-13** — a bad matcher on a group with nothing runnable is not
  fatal. This pins the emptiness-before-matcher order, which upstream's code
  has but its suite never asks about. The mutation check confirms it: moving
  the emptiness check after the matcher check fails two cases.
- **TC-HOOK-CC-14** — substitution does not re-scan what it produced. A project
  directory whose name contains another token must not be rewritten. Contrived
  as a path, but it is the class of bug that makes a config expand differently
  depending on which variable happened to be set.

## Changelog row

| 2026-08-21 | Claude Code's hook configuration ported (`crates/hooks/src/claude_code.rs`, TC-HOOK-CC-1..14), the first of the two adapters. The module is one parser with two deliberate failure policies: lenient about shape, because a settings file is hand-edited and one bad stanza must not stop the harness booting, and strict about matchers, because an uncompilable matcher means that group's hooks never fire and a hook a deployment believes is guarding something but which cannot match is worse than no hook at all. Three rules interact and their order is load-bearing - an unsupported event is ignored before its groups are read, a matcher on an event with no matchable subject is discarded before it is judged, and a group with nothing runnable is dropped before its matcher is judged - so a fatal error never fires over configuration that could not have had an effect. Two cases go beyond upstream: a bad matcher on an unrunnable group is not fatal, pinning an ordering upstream's code has but its suite never asks about, and substitution does not re-scan its own output. A mutation check confirms the suite bites: moving the emptiness check, judging subjectless matchers, defaulting the hook type to something other than `command`, accepting a bad matcher, and substituting only the first occurrence each fail one or two cases. |
