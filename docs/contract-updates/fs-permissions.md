# Contract update: the permission gate, the question pair, and the two knobs

Slice: `fm/tetanus-p2-fs`.
For folding into [`../interface-contract.md`](../interface-contract.md) by the
reconciliation slice; this lane does not edit the shared file.

Nothing here changes a wire type or a struct in `crates/protocol`, so no version
bump is proposed and no peer's build breaks. Two of the three are the engine
starting to write what a section already fixed; the third adds durable types
under §4.3.2's two-step rule, which is exactly what that section exists for.

## 1. §4.3.1: one more `tool/result.code`

`TOOL_NOT_PERMITTED`, written on the `tool/result` of a call a decision refused
before it ran.

§4.3.2 already says a `code` is present only on a result nobody ran, and lists
crash repair's `TOOL_NOT_STARTED` and `TOOL_OUTCOME_UNKNOWN`. §4.4.7 already
promises that a denied call "is not dispatched, and the step gets a
`tool/result` with `ok: false` whose `content` says the call was not permitted",
and §4.3.2 already says the vocabulary grows with the reasons and that a surface
reads an unknown code as "not run, for a reason this build does not know". So
this is a value joining a vocabulary the document states is open, not a new
mechanism.

Proposed row for §4.3.1's code list, and one sentence in §4.4.7 after the
"denied call is a `tool/result`" paragraph:

> The result carries `code: "TOOL_NOT_PERMITTED"`, because a call that never ran
> has no outcome to report. `ok: false` and the sentence are what the model
> reads; the code is what a surface routes on.

## 2. §4.4.7: where the gate sits in the pipeline

The section says the engine asks before the call runs, and does not say where in
the documented pipeline. The engine now asks **after `tools/pre-execute` and
before `tools/execute`**, and that ordering is load-bearing enough to write
down: a listener may rewrite a call, and approving one call while executing
another is the failure a gate exists to prevent.

Proposed sentence for §4.4.7:

> The question is put after `tools/pre-execute` and before `tools/execute`, so
> what is decided is the call that would actually run. A denial skips
> `tools/execute` and `tools/post-execute` both: a post-execute listener observes
> an execution, and there was none.

## 3. §4.3.2: two durable types for the permission knobs

`question/asked` and `question/answered` are already staged in §4.3.2 and the
engine now writes them, with the payloads that table already fixes; no change is
needed for those beyond noting they are written.

Two are new, and they are the durable form of a choice §4.4.7 has half of
already (`approval/policy`):

| `type` | `data` |
| --- | --- |
| `permission/preset` | `preset` |
| `fs/mode` | `mode` (`read-only`, `workspace-write`, `danger-full-access`) |

`permission/preset` is intent and nothing executes on it; the knobs
(`approval/policy` and `fs/mode`) are what decide anything, and each is folded by
its own reader, last-one-wins, exactly as §4.4.7 folds the policy. The intent is
recorded separately because two presets can bundle the same pair, so without it a
journal could not say which one a person chose - and the answer a surface shows
back should be the words they used.

Neither derives to a message, as the three `approval/*` types do not, and neither
carries a `turn` or a `step`, for the reason §4.3.2 already gives for the
approval pair.

By §4.3.2's two-step rule these are written now and join `KnownEvent` in the
later version the presentation lane takes. Until then `parse()` answers `None`
and a surface renders them raw, which §4.3.1 already promises.

## 4. What is deliberately *not* proposed

- **No third `ApprovalPolicy`.** A word meaning "grant without asking" would make
  the preset table read more like upstream's, and it would put a bypass of the
  gate into the enum the gate reads. A deployment that wants that attaches an
  answerer that grants, which is a decision with a name and a code path. §4.4.7's
  two words stand.
- **No `needs_approval` on `ToolDescriptor`.** §4.4.7 already defers it, and the
  reason is unchanged: it is a type the presentation lane constructs, so it lands
  when both lanes take it.
- **Nothing about `ui/approve` or `ui/ask`.** Both stay reserved. This slice
  serves the engine halves they will sit on; serving the calls moves TC-ENG-4 and
  TC-RPC-12 to whatever is reserved next, which is a change for the lane that
  serves them.
