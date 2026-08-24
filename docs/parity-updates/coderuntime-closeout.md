# Parity update: closing out the code-runtime rows (slice `coderuntime-closeout`)

Third and last note for this area; fold it with
[`coderuntime-seam.md`](coderuntime-seam.md) and
[`coderuntime-backends.md`](coderuntime-backends.md).

## 0. A correction to the first note

`coderuntime-seam.md` says `code-runtime/*` and `e2b/*` have no row in section
3 and proposes adding one. That was wrong: the row exists, currently reading

```
| `code-runtime/*`, `e2b/*` | 11 | None | Worker-thread code runtime, remote sandbox backend | ③ |
```

so both earlier notes propose a *replacement* for that row rather than a new
one, and its phase moves from ③ to ② because it is served. A correction is a
new statement rather than an edit to an old one, which is the rule
`parity-changelog.md` sets for itself; this is that statement.

## 1. Section 3, the `code-runtime/*`, `e2b/*` row - final form

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `code-runtime/*`, `e2b/*` | 11 | One trait for evaluating a model-written program, with a structured result (value, ordered logs, upstream's six-kind failure class, duration). A local backend evaluates a small deterministic language of tetanus's own on a worker thread under fuel, a wall-clock ceiling and one output ledger; a runaway program is stopped and its thread reclaimed. Members are answered on the worker or by the host over a bridge, so a program can call the harness's own tools - several of them inside one turn step. A namespace declares what its failures look like and a program can `catch` one, while a budget, an abort and a full ledger pass through uncatchable. A remote backend behind the same trait submits, polls, fetches and cancels, owning one shared sandbox with transactional setup and idempotent teardown. Registered as `run_code` from the settings document, dispatched by the ordinary pipeline, its failures contained like any other tool's | Two things, both named and neither closeable here: a real-language backend, which must wait for the shell lane's sandbox modes (`CODERT-FOLLOWUP-1`), and upstream's OOM containment, which needs a per-worker heap cap a Rust thread cannot be given | ② |

## 2. Section 4, the port table - the rows this slice adds to

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `code-runtime/code-runtime/tests/service.spec.ts` (the binding half), `code-runtime-worker-thread/tests/runtime.spec.ts` (the bridge half) | `crates/coderuntime/tests/turn.rs`, `crates/coderuntime/tests/upstream_seam.rs` | A program calling the host, and surviving one call that failed | ported: TC-PORT-CODERT-29..34. Upstream's bindings are async because a Node worker awaits across a port; the same bridge here lets a program call tools from the tetanus registry, and TC-PORT-CODERT-29 pins the payoff - three tool calls inside one program cost the turn one step rather than three. TC-PORT-CODERT-30 keeps a distinction upstream has no need for, because a tetanus tool has two ways of not working: one that ran and said no is a value the program branches on, and one that could not run is the program's failure. TC-PORT-CODERT-31 is an addition with a reason: the namespace is built from this harness's own registry, so `run_code` has to be refused as a member of it or a program can nest runs until something gives. TC-PORT-CODERT-32..34 restate the typed rejection contract as far as a language without classes can: the caught value carries the failed member's name under the property the namespace declared, which is what `RESERVED_ERROR_MEMBERS` was for, and only a program-level failure is catchable - a program that could catch its own timeout would undo the containment story with two keywords |
| none - upstream's runtime is a plugin a deployment chooses by loading | `crates/coderuntime/tests/settings.rs` | Whether a deployment gets a code runtime at all, and what a program may call | TC-CODE-SET-1..4. Completeness rather than parity, stated as such in the file: a Cordis plugin is opt-in by being loaded, and a compiled-in Rust registry is not, so the document is where that choice lives. Off unless asked for; the tools a program may call are named one by one, because a list that said "all of them" grows whenever a plugin registers something; and a document that asks for the remote backend with no provider wired is refused rather than quietly running the model's program on the harness's own machine |

## 3. What remains in these rows, and why it is not being closed here

Two items, both from the gap column of the previous note, and neither is work
this lane can honestly do:

1. **A real-language backend** (`CODERT-FOLLOWUP-1`). Handing a program to
   `python3` or `node` voids every guarantee the local backend makes - fuel
   does not exist outside this evaluator, and the program can open files and
   sockets - so it must be built on the shell lane's `crates/exec` and run
   under its sandbox modes. That lane owns the mode vocabulary and the
   escalation stamp; building a second process-spawning path here would be the
   duplication this lane's brief forbids.
2. **OOM containment as `worker-exit`.** Upstream caps a worker's heap through
   Node's `resourceLimits`. A Rust thread has no heap of its own and cannot be
   given one without installing a global allocator, which is not a decision a
   capability crate gets to make for the whole process. The output ledger and
   the fuel bound what a program can *produce* and *do*; they do not bound what
   it can allocate, and saying so is more useful than a case that passes for
   the wrong reason.

Everything else in these rows is served. The two items above are the entire
remaining content of the `code-runtime/*`, `e2b/*` area.

## 4. Changelog row

| 2026-08-21 | The code-runtime rows closed out (`crates/coderuntime`, TC-PORT-CODERT-29..34, TC-CODE-SET-1..4). Two of the four gaps the previous note named are now served and two are named as not-ours. Bindings can be answered by the host over a bridge, which is what a program needs to call a tool: the worker sends the call and blocks, the host awaits the future where futures can be driven, and three tool calls inside one program cost a turn one step instead of three. A namespace can declare what its failures look like, and the language grew `try`/`catch` so that declaration means something - the caught value carries the failed member's name under the declared property, which is what the reserved error-member set had been waiting for. Only a program-level failure is catchable: `while (true) { try { } catch (e) { } }` still ends at the compute budget, because a catchable timeout is the containment story undone by two keywords. `run_code` is turned on from the settings document, off by default, with the tools a program may call named one by one and `run_code` itself refused among them. What is left in the area is a real-language backend, which belongs to the shell lane's sandbox modes, and OOM containment, which needs a per-worker heap cap a Rust thread cannot have. |
