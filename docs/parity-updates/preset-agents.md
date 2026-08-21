# Parity update: named agent presets (slice `preset`)

Not folded into [`../parity.md`](../parity.md) by this branch; the
reconciliation slice folds every lane's note in one pass.

## 1. Section 3, the `preset/*` row

Replaces the row whose `Today` column begins "The preset roster: a directory
per preset across ordered roots...".

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `preset/*` (agent presets, persona) | 9 | The preset roster - a directory per preset across ordered roots, the earlier root winning a duplicate and the loser recorded as shadowed, a candidate that is not a working preset reported rather than skipped, a root that cannot be read answered as a fault - and applying one to a run: a preset names a model, a provider, a step budget, a tool subset, a prompt shape and a persona, written inline in the settings document or in a preset directory under the same keys, selected per session over `session.create`, recorded in the session header at creation and inherited by a fork. The tool subset is what that session's model is offered; the persona is a prompt section of its own at order zero | Authoring (copying a shipped preset into a writable root, tightening modes, deleting), switching the preset of a running session, and a surface flag: `session.create` carries the choice, and `tetanus run` still takes its model from `--model` | ② |

## 2. Section 4, the port table

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `preset/agent-presets/tests/{settings,session,mount}.spec.ts`, `preset/persona/tests/persona.spec.ts` | `crates/engine/tests/presets.rs` | What a named agent is, and what selecting one does to a session | part ported: TC-PORT-PRESET-1..10 for a roster read out of the settings document, a preset directory read with the same vocabulary and losing to the document, a selection that changes both the model and the tool set, an explicit argument winning over the preset, the preset recorded in the session header and unchanged when the document later changes, an unknown id refused with the known ids in the refusal, the default preset composing a session that named none, the prompt shape and the persona reaching the assembly of that session and no other, a preset naming a tool the harness does not have refused rather than quietly narrowed, and a fork continuing as the agent it forked from. Upstream composes a preset by mounting a Cordis plugin tree per session, so most of `mount.spec.ts` is that machinery - a row that fails to load, a process-global service, an isolate realm, an attributed subtree - and a tetanus preset names settings rather than plugins. Its live switch of a running composition is deliberately not served: TC-PORT-PRESET-5 pins the opposite, because a session whose agent changed half way through would make its journal a record of two of them. Its authoring half needs a write path `crates/config` does not have |

## 3. Changelog row

| 2026-08-21 | Named agent presets applied to a run (`crates/engine/src/preset.rs`, `AgentPreset` in `crates/config`, TC-PORT-PRESET-1..10), closing the half of the `preset/*` row the roster left open. A preset was a directory nobody could use; it now names a model, a provider, a step budget, a tool subset, a prompt shape and a persona, and `session.create` takes one by name. Three rules carry it. The tool subset is applied to the registry that session's turns run on, so the model is never offered a tool it may not call - being offered one and refused is a step spent on a refusal. The id is resolved once, at creation, and written into the session header: a session whose agent changed under it half way through a conversation would leave a journal that is a record of two agents with nothing marking the boundary, and a fork inherits rather than re-resolves for the same reason. And a preset that names a tool the harness does not have is refused where it is used rather than silently narrowed, because a typo that produces a smaller agent is a capability nobody took away. An inline definition in the settings document beats a directory of the same id, which is the roots' own trust order applied one level up. `SessionCreateParams` gains an optional `preset`; the contract note is `docs/contract-updates/mcp-preset.md`. |
