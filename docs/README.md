# tetanus documentation

Start at the root: [README.md](../README.md) for what tetanus is and how to run it,
[ARCHITECTURE.md](../ARCHITECTURE.md) for how the workspace fits together, and
[CONTRIBUTING.md](../CONTRIBUTING.md) for how to work on it.

This directory holds the documents that go deeper than the root files.

| Document | Type | Status |
| --- | --- | --- |
| [turn-flow.md](turn-flow.md) | Design description (IEEE 1016) of the turn: canonical sequence, dispatch modes, durable log, rationale | Current, matches the code |
| [interface-contract.md](interface-contract.md) | The engine/presentation boundary: JSON-RPC envelope, calls, wire types, versioning. Its machine-readable half is `crates/protocol` | Current, contract version 1.0. No carrier serves it yet |
| [PLAN.md](PLAN.md) | Decision document: parity scope, options considered, phase plan | Current, decision closed 2026-08-18 |
| [plan-visual.html](plan-visual.html) | The option diff behind PLAN.md, as a diagram. Open it in a browser | Historical, matches PLAN.md |
| [scoping-superseded.md](scoping-superseded.md) | First-pass scoping note | Superseded by PLAN.md, kept for provenance |

## Which document answers what

- *What events does a turn emit, in what order, and which one can I hook?* -
  [turn-flow.md](turn-flow.md) sections 4.1 and 4.2.
- *Why is `system-prompt/assemble` where it is, when upstream says otherwise?* -
  [turn-flow.md](turn-flow.md) section 6.1.
- *Which crate owns what?* - [ARCHITECTURE.md](../ARCHITECTURE.md) section 4.2.
- *What does "parity with upstream" mean here, and what is out of scope?* -
  [PLAN.md](PLAN.md), plus [ARCHITECTURE.md](../ARCHITECTURE.md) section 7.
- *What does the merge gate actually prove?* - [ARCHITECTURE.md](../ARCHITECTURE.md) section 5.
- *What may a UI call, and what may change under it?* - [interface-contract.md](interface-contract.md)
  sections 4.2 and 5. Change it in its own PR, never inside a feature PR.

## Writing a new document here

Design descriptions follow IEEE 1016: identification, stakeholders and their concerns, design views,
and rationale. [turn-flow.md](turn-flow.md) is the worked example, sized to its component.
Test documentation follows IEEE 829 proportionately: stable case identifiers and explicit expected
results. Fill the sections that apply; do not add an empty heading to match a template.
