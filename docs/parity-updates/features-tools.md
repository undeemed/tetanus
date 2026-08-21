# Parity note: the built-in feature tools

Slice: `crates/features` - skill, todo, goal, plan, feedback, attachment and
workspace.
Branch: `fm/tetanus-p2-features`.
For folding into [`../parity.md`](../parity.md) sections 3 and 4 by the
reconciliation slice; this lane does not edit the shared file.

## 1. Section 3 row, replaced

The row reads `None` in the `Today` column before this slice.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `skill/*`, `todo/*`, `goal/*`, `plan/*`, `feedback/*`, `attachment/*`, `workspace/*` | 32 | The feature tools, each a registered tool over state kept only on the append-only journal, so a replay reproduces it: a whole-list todo replaced every call and cleared by the next turn rather than by the end of this one; a standing goal with compare-and-set revisions, the phase table, and a tombstone on clear; plan mode folded from the log, changing the prompt and never the tool catalogue, with an exit tool that records the plan it presents; an append-only operator feedback channel that never derives to a message; skills discovered from project and user roots in precedence order, the earlier root winning and the loser recorded as shadowed, broken candidates reported rather than skipped, and model invocability checked at the call as well as in the catalogue; attachments admitted as a whole batch before anything is stored, content-addressed so equal bytes are stored once, and images measured from their headers before a decode; and a workspace sketch naming the project root, the marker that identified it, the instruction files and the top level | The autonomous goal-round driver, upstream's persisted workspace registry for a picker, a durable model-facing skill catalogue with its per-step tombstone protocol, skill root watching, per-message feedback ratings, and the wire encoding that would carry an attachment across the boundary | ② for the driver and the registry; the rest is named in section 3 below |

## 2. Section 4 rows to add

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `todo/tool-todo/tests/tool-todo.spec.ts`, `integration.spec.ts`, `projection.spec.ts` | `crates/features/tests/upstream_todo.rs` | Whole-list replacement, the trimmed identity, the active-status policy, the turn boundary that clears it, and the fold across a reload | ported: TC-PORT-TODO-1..11 |
| `goal/goal/tests/goal.spec.ts`, `projection.spec.ts`, `tool-goal/tests/tool-goal.spec.ts` | `crates/features/tests/upstream_goal.rs` | Whole-state changes, compare-and-set revisions, the phase table, the tombstone, and the conditional arguments each action needs | part ported: TC-PORT-GOAL-1..12 |
| `plan/plan-mode/tests/integration.spec.ts`, `invariant.spec.ts` | `crates/features/tests/upstream_plan_feedback.rs` | The mode as a fold, the guidance section that renders only while it is on, and the exit that records the plan | ported: TC-PORT-PLAN-1..7 |
| `feedback/command-feedback/tests/command-feedback.spec.ts` | `crates/features/tests/upstream_plan_feedback.rs` | One record per remark, whitespace normalized and content never parsed, empty refused, and nothing reaching the model | ported: TC-PORT-FEED-1..6 |
| `skill/skill-filesystem/tests/skill-filesystem.spec.ts`, `skill/skill/tests/skill.spec.ts` | `crates/features/tests/upstream_skill.rs` | Both file shapes, root precedence and shadowing, faults that do not hide siblings, the boolean spellings, CRLF frontmatter, and what the model may load | part ported: TC-PORT-SKILL-1..12 |
| `workspace/workspace/tests/workspace.spec.ts` | `crates/features/tests/upstream_workspace_attachment.rs` | The project root and its marker, the refusals for a non-directory and a missing path, and the sketch | part ported: TC-PORT-WS-1..5 |
| `attachment/attachment/tests/index.spec.ts`, `attachment-local/tests/store.spec.ts`, `image.spec.ts` | `crates/features/tests/upstream_workspace_attachment.rs` | Whole-batch admission, every limit, header-measured dimensions, content addressing with verification, and caller mistakes told apart from storage faults | part ported: TC-PORT-ATTACH-1..10 |

## 3. What is unrepresentable, and why

- **The goal-round driver** (`goal/goal-round-driver`, 26 cases). It is an
  autonomy layer over the goal state: a round budget, reservations that go stale
  against a revision, authority checks distinguishing a human turn from a model
  one, wrap-up instructions, and checkpoint failures that disarm it. tetanus has
  no autonomous continuation loop for it to drive - a turn ends when the model
  stops asking for tools - so there is nothing to reserve rounds against.
  Upstream's `activation` (`armed`/`disarmed`) is process-local by its own
  definition and never persisted, so a journal-only restatement has nothing to
  hold, and `maxGoalRounds` bounds rounds that driver admits.
- **Upstream's workspace registry.** Most of `workspace/workspace` is a picker's
  list: entries bootstrapped from session headers in `createdAt` order, ties
  broken by session id, persisted stable order reloaded on restart, titles,
  cwd-drift grouping, and rollback of a provisional cache entry when a record
  write fails. That is a surface's state over a durable store, not something a
  turn reads. `tetanus_core::storage` is the seam it would sit on and nothing
  calls it yet. What restates here is what one session knows about the one
  project it is in.
- **The durable model-facing skill catalogue.** Upstream injects the
  name-and-description list as a durable message at the first step, deduplicates
  it per step, replaces it when the snapshot changes, and writes an empty
  tombstone when the last skill disappears. That protocol exists because its
  skill set changes while a session runs, which is what its root watcher is for;
  tetanus settles the roster when the tool is composed, so the catalogue is the
  tool's description and there is no per-step reconciliation to restate. The
  watcher itself, and its canonicalization, symlink and late-callback
  containment cases, go with it.
- **`skill-badge`**, a bundled skill shipping a PNG asset, and `resourceBase`,
  the hint for skills that carry files alongside their Markdown. Neither has a
  consumer here.
- **`message-feedback`.** A per-message rating with its own durable rows,
  monotonic host times, item versions, ABA-safe deletes, session-id fencing and
  a checkpointed sidecar. It is a different feature over a store this workspace
  does not have, and its Gateway/Remote namespace is a boundary concern.
  `command-feedback` is the half that restates.
- **The telemetry disclosure sentences** upstream's feedback command returns
  (full, feedback-gated, or disabled session sharing). They are a deployment's
  text about a telemetry service that does not exist here.
- **Attachment transport.** Upstream carries attachments across its API as
  base64 with a media type; the tetanus boundary has no attachment type, and
  adding one is a `crates/protocol` change that `docs/interface-contract.md` §5
  says both lanes land together. This slice stores and records them; nothing
  yet puts one into a turn's messages, which is named in the gap column.
- **Cordis machinery throughout**: projection registries with `stateVersion`,
  HMR disposal, loader-composition and namespace-export cases, `ctx.inject`
  conditional children. The tetanus equivalents are a fold over the log and an
  `EffectHandle`, and where a disposal case has a counterpart it is ported
  (TC-PORT-PLAN-4).

## 4. Where tetanus deliberately differs

- **Plan mode records a flip when it is made.** Upstream defers a user's
  selection to the next accepted pre-step so a flip lands on a step boundary.
  A tetanus assembly is built once per step from the log as it stands, so there
  is no window in which a mid-step flip could be observed; the deferral has
  nothing to protect.
- **Skills are read directly rather than through the filesystem service.**
  Upstream reads skill files through `ctx.fs`. The tetanus fs seam exists to
  judge model-supplied paths, and these paths are the deployment's own - routing
  them through a fence rooted at the workspace would refuse a user's
  `~/.agents/skills` for being outside it.
- **The content address is not a cryptographic digest.** It is a 128-bit FNV-1a
  with the length appended, and every hit is verified byte for byte before it is
  treated as a match (TC-PORT-ATTACH-9). The property attachments need is "equal
  bytes, equal name", which does not require a hash dependency; a store that
  assumed the digest instead of checking would need one.
- **A goal may be completed from any unfinished phase.** Upstream allows
  completion from every stopped phase too; this states it as a case
  (TC-PORT-GOAL-5) because forcing a resume first would put a lie on the
  journal - the goal was not resumed, it turned out to be done.

## 5. What this lane did not wire

The tools are composed by whoever builds a `ToolRegistry`, and the shipped
binary's registry lives in `crates/cli/src/main.rs`, which
`docs/interface-contract.md` §4.7 gives to the presentation lane. Composing them
there - and the settings keys that would choose the todo parallelism policy, the
plan-mode guidance text, the skill roots and the attachment limits - is that
lane's change. Every case here composes the suite the way that call would.
