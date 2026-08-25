# Parity update: the job store gets its caller

Written by the filesystem lane, taking the wiring the process lane named and
declined. Nothing here edits `docs/parity.md`.

Branch: `fm/tetanus-jobs-caller`.

## 1. The clause this closes

The `shell/*`, `terminal/*`, `subprocess/*` row's Gap has read, since that lane
closed the rest of it:

> `run_in_background` for the one-shot `shell` tool, which needs the job store
> from `workflow/*`, `schedule/*`, `jobs/*`: a one-shot's process group is
> swept when its call returns, so there is nothing to collect from afterwards,
> and a second store built here would leave that row with two to reconcile.

That store landed with the workflow rescue and has had no caller since. This is
the caller.

## 2. Section 3 rows

**`shell/*`, `terminal/*`, `subprocess/*`** - Today gains:

> A command started in the background and collected later: `run_in_background`
> on `shell` answers at once with a job id, the record is written before the
> work starts, and `job_list`, `job_output` and `job_kill` read and stop it.
> The record outlives the turn, which is the point and also the cost - a turn
> that ends before the job does cannot read its output, and the schema says so.

Gap loses the `run_in_background` clause and gains:

> A per-job signal. `job_kill` stops the session's work through the turn's own
> interrupt, which is what reaches a process group rather than a handle; aiming
> at *one* job needs the group id on the record, which is the next slice.

**`workflow/*`, `schedule/*`, `jobs/*`** - Gap loses `a caller for the job
store`, keeping the script runtime, the model-facing workflow tools and the
schedule-to-inbox delivery.

## 3. Section 4 row

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `shell/tool-bash/tests/tools.spec.ts` (`run_in_background`), `jobs/tool-jobs/tests/*` | `crates/exec/tests/upstream_jobs.rs` | Starting work that outlives the turn, and collecting it | ported: TC-PORT-JOB-13..18. Upstream's `job_output` is a consuming reader over a live stream; this answers the record, so a second read says the same thing rather than draining it - a model that read once and lost the turn can read again. Its per-job cancel needs the group id on the record and is named in the gap |

## 4. Three decisions

- **The record is written before the work starts.** A job that began with no
  record is work nobody can find; a record whose work never started is one
  `job_output` away from saying so. Only one of those is recoverable.
- **A running job is an `ok` answer, not a failure.** "Still running" is
  something a model acts on by asking again. A failed *call* is something it
  acts on by trying something else, and those are different instructions.
- **A job is read back rather than streamed.** Upstream's reader consumes; this
  answers the durable record, so two reads agree and a turn that died between
  them loses nothing.

## 5. The defect the binary found, which unit tests could not

The first cut registered the three job tools **only when a store was composed**,
with what looked like a good argument: `job_list` over no store has nothing to
say.

Running `tetanus tools` showed the cost. The catalogue composes no session and
therefore no store, so it advertised five exec tools where a run offers eight -
which is exactly the disagreement `docs/interface-contract.md` §4.7.3 forbids,
and the one a client cannot tell from an empty toolbox. Every unit case passed
throughout: each composition was internally consistent, and the two were never
compared.

They are declared always now and answer in words when there is nowhere to keep
a record, the way `read_image` does without an attachment store. TC-PORT-JOB-18
pins it in the direction that would have caught the first cut.

## 6. A cross-lane note

`crates/exec` and `crates/toolset` are the process lane's files and this slice
edits both, on assignment, because that lane named this wiring as theirs and
was not carrying it. The change is additive: one optional argument, three new
tools, one new parameter on `register_or_explain` whose `None` reproduces the
old behaviour exactly.

The presentation lane's `browser_views` case caught the three new tools
immediately and asked for a view or a reason. Each has a line in `STILL_BARE`
with the reason, and `job_list` is marked as the one that most wants a real
view - a reader watching a build wants it to change without being asked.

## 7. Changelog entry

Written as its own file by `docs/tools/parity-changelog.py add`.
