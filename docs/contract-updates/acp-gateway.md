# Contract update proposal: `agent.steer` is missing from `method::ALL`

- **Lane:** external contract surfaces (`acp/*`, `sdk/*`, `api/*`, query half of `session-query/*`).
- **Branch:** `fm/tetanus-p3-acp`.
- **Status:** proposal. Nothing in this branch edits `crates/protocol` or `docs/interface-contract.md`.
- **Kind:** defect in the shared contract crate. No wire change; no behaviour change to any served call.

## What is wrong

`tetanus_protocol::methods::method::ALL` documents itself as

> Every client-to-server method this contract defines, served or reserved.

and gives its reason:

> A routing arm is written by hand, so the one that gets forgotten is the one no case names.

`method::AGENT_STEER` is declared as a constant, is routed by `crates/rpc`'s codec, has a
default `Engine` body answering `NotImplemented`, and has a `capability::AGENT_STEER` string.
It is not in `ALL`.

That is exactly the mistake the constant's own doc comment says it cannot catch:

> Adding a constant above and not adding it here is the single mistake this cannot catch,
> which is why the two sit together.

`APPROVAL_SET`, the other reserved call, *is* in `ALL`, so the omission is not a policy about
reserved calls. It is an oversight.

## Why it matters

Any consumer that iterates `ALL` to check completeness silently skips `agent.steer`. This
lane's descriptor catalog (`crates/sdk/src/gateway.rs`) is one such consumer: TC-PORT-API-1
iterates `ALL` and asserts a descriptor exists for each entry, and that case would pass with
`agent.steer` described nowhere. `AGENTS.md` records the same hazard for TC-ENG-4 and
TC-RPC-12 - "the RPC routing arm is added by hand, so the arm that gets forgotten is the one
no case names".

The hazard is not hypothetical for this lane specifically: the ACP bridge maps ACP's
`session/cancel` onto the contract's interrupt/steer neighbourhood, and a caller enumerating
`ALL` to discover what a build can do would conclude `agent.steer` does not exist rather than
that it is reserved.

## Proposed change

In `crates/protocol/src/methods.rs`, add `AGENT_STEER` to `method::ALL`, between
`AGENT_INTERRUPT` and `CATALOG_TOOLS`, matching the order the constants are declared in:

```rust
    AGENT_PROMPT,
    AGENT_STATUS,
    AGENT_INTERRUPT,
    AGENT_STEER,
    CATALOG_TOOLS,
```

Nothing else changes. The call is already routed and already answers `NotImplemented`; this
only makes the list say what the codec already does.

## Blast radius

- `crates/rpc/tests`: any case iterating `ALL` gains one method. That method is already
  routed, so a case asserting "every method in `ALL` is routed rather than unknown" keeps
  passing; a case asserting a *count* would need its number changed.
- `crates/sdk/tests/upstream_api.rs`: TC-PORT-API-1 keeps passing (the descriptor exists
  already). TC-PORT-API-3, which pins the omission so that fixing it does not go unnoticed,
  **fails on purpose** and is retired in the same change.
- No wire change. `ALL` is not serialised anywhere.

## Why this lane did not just do it

`docs/interface-contract.md` §4.7 gives `crates/protocol` to the engine lane, and `AGENTS.md`
requires a change to the contract or the protocol crate to land as its own pull request
touching both plus the doc's changelog - never inside a feature pull request. This is that
proposal. The pinning case names this file so the two move together.
