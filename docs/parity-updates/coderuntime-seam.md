# Parity update: the code-runtime seam (slice `coderuntime-seam`)

Not folded into [`../parity.md`](../parity.md) by this branch; the
reconciliation slice folds every lane's note in one pass. `code-runtime/*` and
`e2b/*` have no row in section 3 yet, so the first block below is a new row
rather than a replacement.

## 1. Section 3, a new row

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `code-runtime/*`, `e2b/*` | 11 | The seam: one trait for evaluating a model-written program, a structured result (value, logs, failure class, duration), upstream's six-kind failure taxonomy, and the portable binding-name rules every backend shares. A local backend evaluates a small deterministic language of tetanus's own on a worker thread | The rest of the local backend's budgets, the tool registration, the remote backend, and the typed rejection classes a binding namespace may declare | ② |

## 2. Section 4, the port table

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `code-runtime/code-runtime/tests/service.spec.ts`, `reserved.spec.ts` | `crates/coderuntime/tests/upstream_seam.rs` | What a run answers with, what counts as misuse, and the names no backend may expose | part ported: TC-PORT-CODERT-1..14 for the descriptors, a failed program as a field rather than a rejection, a pre-aborted request that starts no worker, misuse refused before anything runs, the portable identifier rule and both reserved sets asserted against upstream's own table of names, a shut-down runtime refusing later runs, bindings called both ways with nested JSON surviving, an unknown member listing what there is, run isolation and an empty ambient environment, a non-lossless completion failing rather than being rendered, logs kept in order across a failure, the duration, and awkward member names treated as ordinary members. Upstream's service-registration cases are Cordis lifecycle - `ctx.codeRuntime`, removal on fiber disposal, a refused second implementation - and a tetanus runtime is a value a composer holds, so there is no registry to remove it from. Its typed rejection classes are unported: a binding failure here is a message the program reads, and `RESERVED_ERROR_MEMBERS` is kept and tested as the settled half of that contract |

**The language difference, stated once.** Upstream evaluates TypeScript in a
Node worker thread. A Rust harness has no JavaScript engine and this crate does
not add one, so the local backend evaluates a small language of its own:
`let`, assignment, `if`, `while`, `return`, JSON literals, the usual operators,
and calls into host bindings. Parity is claimed at the *seam* - the request,
the failure taxonomy, the caps, the binding rules - and never at the language.
Every upstream case that is about TypeScript specifically (erasable syntax,
`Object.prototype` collisions in a JS object graph, forged worker-port traffic)
is therefore unrepresentable rather than unported, except where the same
question exists here in a different shape: TC-PORT-CODERT-13 restates the
prototype-collision case as a member name that is awkward for *some* target
language, which is the portable form of the same promise.

## 3. Changelog row

| 2026-08-21 | The code-runtime seam implemented (`crates/coderuntime`, TC-PORT-CODERT-1..14), opening the last in-scope parity area with nothing behind it. A tool call is a function the harness chose; a program is the model writing the control flow and the harness running it once. The seam is deliberately two methods and a result: a failed program is a *field* on that result, and only misuse of the seam itself - a namespace called `console`, two namespaces of one name - is an error of `run`, because a caller can always fix the second and never the first. The portable name rules are kept as the union of every target language's, not narrowed to the one language this backend evaluates, for the reason upstream gives: a binding called `lambda` that works today and breaks the day a Python backend lands is a bug with a long fuse. The language is not JavaScript and the crate says so in its first paragraph rather than letting a reader assume; what parity is claimed on is the seam. |
