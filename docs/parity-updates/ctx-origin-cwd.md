# Parity note: the origin fact still published with no producer

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p6-ctx-rows`.
Scope: contract section 4.4.9's `cwd`, and the `SessionOrigin` a composer
supplies for `spawned_by` and `depth`.

---

## 1. What this is, and why it is being landed twice

An earlier slice of this lane (`fm/tetanus-p4-parity-sweep`, commits `354bfd9`
and `379aac3`) found and closed this gap. **That branch was never merged.** It
is not an ancestor of master, its sweep note is not in `docs/parity-updates/`,
and `crates/engine/tests/session_origin.rs` does not exist on master. The
finding it recorded is therefore still true today, which is why this lands
again rather than being cited.

The finding: §4.4.9 is a page of contract, `KnownEvent::SessionStart` declares
`cwd`, `spawned_by` and `depth`, the §4.3.1 table lists all three, and
TC-PROTO-30..32 pass - **over hand-built values**. On master today the engine's
`SessionHeader` has no `cwd` field at all, and writes `spawned_by: None` and
`depth: None` unconditionally. No journal tetanus has ever written carries any
of the three. `cwd` is the field the contract says a journal full of relative
paths is unreadable without.

Two lanes have since read those fields - `tetanus_subagent::children` folds
`spawned_by`, `depth` bounds delegation - so the *readers* landed while the
writer did not.

## 2. What changed

`SessionHeader` gains `cwd`. `SessionOrigin` carries the three facts a composer
knows and nothing below it does, and `EngineConfig::session_origin` supplies
them. A fork inherits all three, per §4.4.9's rule that a fork is the same work
taken a second way; `parent_session` and `fork_seq` stay its own. A journal
written before the fields existed keeps the header it has rather than being
backfilled with a directory it was never opened in.

`session_origin` is deliberately not read from the settings document: where a
run was opened is a fact about the process, and a settings key for it would let
a deployment write a directory its journals were never opened in.

TC-ORIGIN-1..8: 1 through 7 restated unchanged against today's tree - they passed as
written, which is its own evidence that the gap is the same one. TC-PORT-FORK-1
gains the inherited `cwd`, read from the parent rather than written out, since
what it asserts is that the child carries the parent's value.

A mutation check confirms the cases bite, and it found a hole worth recording.
Not writing `cwd` fails two suites, and a root session claiming depth 0 fails
two cases. But a fork that *re-read the process directory* instead of
inheriting passed everything: TC-ORIGIN-5 forks through the same engine, so the
parent's directory and the process's are the same value and the mutation is
invisible to it. TC-ORIGIN-8 is the arrangement that tells them apart, and it is
the ordinary one rather than a contrived one - a journal opened somewhere
yesterday, resumed and forked from somewhere else today. It fails under the
mutation and passes without it.

## 3. Row edit, section 3

**`core/*`.** Gap: drop "the rest of the header metadata (cwd, subagent origin,
delegation depth...)"; replace with "a subagent spawner to fill `spawned_by`
and `depth`, which the journal now carries and the composer can now set".
Today: add "every origin fact section 4.4.9 names, on the journal a real run
writes, inherited across a fork".

## 4. Changelog row

| 2026-08-22 | Contract section 4.4.9's origin facts given a producer, for the second time (`crates/engine/src/session.rs`, TC-ORIGIN-1..7). The first was `fm/tetanus-p4-parity-sweep`, which was never merged - it is not an ancestor of master, so the gap it closed is still open there, and the cases restated unchanged against today's tree, which is its own evidence. The gap: §4.4.9 is a page long, `KnownEvent::SessionStart` declares `cwd`, `spawned_by` and `depth`, the §4.3.1 table lists them and TC-PROTO-30..32 pass - over values every one of those cases builds by hand. On master the engine's header had no `cwd` field at all and wrote the other two as `None` unconditionally, so no journal tetanus ever wrote carried one, including the `cwd` the contract says a journal full of relative paths is unreadable without. Two lanes had meanwhile landed *readers* of those fields, which is the sharpest form of this defect: the fold that answers what a session delegated was reading a field nothing set. `cwd` is now written at creation from the process or a configured directory, `spawned_by` and `depth` come from a `SessionOrigin` the composer supplies, and a fork inherits all three because a fork is the same work taken a second way. `SessionOrigin` is not on `SessionCreateParams` (section 5: a field on a type the presentation lane constructs is not free) and not in the settings document either, because where a run was opened is a fact about the process and a key for it would let a deployment write a directory its journals were never opened in. Three subtleties carry their own cases: a root session is depth *absent* rather than zero, a fork of a root session invents no delegation, and a journal written before the fields existed keeps the header it has rather than being backfilled. |
