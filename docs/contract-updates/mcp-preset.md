# Contract update: `session.create` takes a preset (slice `preset`)

Not applied to [`../interface-contract.md`](../interface-contract.md) by this
branch: the boundary document is edited in place and every lane collides there.
This is the change, ready to fold, with the changelog row that goes with it.

## The change

Section 4.4, `session.create`, gains one optional parameter. `crates/protocol`
carries it already (`SessionCreateParams::preset`), and the engine serves it.

| Field | Type | Required | Meaning |
| --- | --- | ---: | --- |
| `preset` | string | no | The named agent preset this session is composed from: a model, a tool subset, a prompt shape and a persona, resolved server-side out of the settings document. Omit for the server's default preset, if it has one, and for no preset at all when it has none. |

Three sentences belong in the prose beside the table:

- **An explicit `provider`, `model` or `max_steps` wins over what the preset
  says.** A caller that named both asked for that model on that agent.
- **A preset this server does not compose is `InvalidParams`**, with
  `data.field = "preset"`, `data.preset` naming what was asked for, and
  `data.known` listing the ids that exist, so a surface can offer them.
- **The choice is durable and is made once.** The id is written into the
  session's `session/start` header, a fork inherits it, and a document edited
  afterwards does not move a session that is already composed.

`SessionInfo` is unchanged: a surface that needs the preset of a cold session
reads the header through `session.events`. Adding it to `SessionInfo` is a
separate question and a separate PR.

## Compatibility

Additive and optional in both directions. A client that never sends `preset`
sees exactly what it saw before, and a server built before this change ignores
the field per section 7.5's forward-compatibility rule. No capability is
needed: a build with no presets configured answers a request that names one
with the `InvalidParams` above, which is the same answer it would give for a
preset id that was deleted.

## Changelog row for the contract document

| 1.x | `session.create` gains an optional `preset`, naming the agent a session is composed from. Additive; the field is absent on every request written before it, and a server that does not know the field ignores it. The refusal for an unknown id carries the known ids so a surface can offer them rather than making the user guess. |
