# Parity note: the surface vocabulary over the feature state

Slice: `tetanus_features::view` - `SessionView` and `WorkspaceView`.
Branch: `fm/tetanus-p2-ui-surfaces`, on top of `fm/tetanus-p2-features`.
For folding into [`../parity.md`](../parity.md) by the reconciliation slice;
this lane does not edit the shared file.

## 1. What this slice serves

Nothing in this slice is a port, and it adds no `TC-PORT-*` identifier.
Upstream's panels read a generated protocol client, so the behaviour to restate
is not a spec file: it is the *shape* a surface reads the feature state
through. The cases are `TC-VIEW-1..10` in `crates/features/tests/view.rs`, and
they assert the JSON rather than the struct, because the field names are the
contract and a rename that kept the struct compiling would break every
consumer while the suite stayed green.

## 2. The clause this closes in the feature-tool row

Section 3's `skill/*`, `todo/*`, `goal/*`, `plan/*`, `feedback/*`,
`attachment/*`, `workspace/*` row - written by `fm/tetanus-p2-features` - lists
in its gap column:

> the wire encoding that would carry an attachment across the boundary

Half of that clause is now served and the other half is deliberately parked, so
the row wants this wording rather than a deletion:

> a surface reads the folded state through `tetanus_features::view`
> (`SessionView`, `WorkspaceView`), with attachments named, measured and
> content-addressed but never carried; putting those shapes on the JSON-RPC
> boundary waits on the presentation lane taking the types, and
> `docs/contract-updates/features-ui-surfaces.md` §3 names the three changes it
> costs

## 3. What is unrepresentable here, and why

- **A push for live panels.** A subscriber already receives `session/event` and
  can re-fold, so a dedicated push is an optimisation nobody has measured yet.
  Naming it as a gap would be booking work the evidence does not ask for.
- **Skills as session state.** The roster is settled when the tools are
  composed, not folded per session, so it is not a field on `SessionView`. A
  surface that wants the list reads `skill::discover`. If a consumer needs it
  per session it becomes a third view, and that is a request the presentation
  lane makes, not a gap this one leaves.
- **Attachment content, thumbnails, any image decoding.** A view carries no
  bytes; `view::attachment_path` says where they are and the surface reads the
  file.
- **Per-turn slicing.** Every view here is "the state now". A timeline is the
  journal's job and `crates/cli/src/render/timeline.rs` already reads it that
  way.
