# Parity update: what two days of drift did to the code-runtime branch

Fourth note for this area, and not a slice: this records a **rescue**. Fold it with
[`coderuntime-seam.md`](coderuntime-seam.md), [`coderuntime-backends.md`](coderuntime-backends.md)
and [`coderuntime-closeout.md`](coderuntime-closeout.md).

`fm/tetanus-p3-coderuntime` was finished and measured, then never landed, and master moved on
without it. This note says what survived the rebase unchanged, what did not, and the one claim that
had to be corrected rather than carried.

## What the branch actually was

It was reported as sixteen commits. It is **nine**. The other seven were the mcp/web lane's, which
the branch had been built on top of and which landed separately under different SHAs when that lane
rebased. Rebasing the nine with `--onto` rather than replaying all sixteen is why this produced two
small conflicts instead of a re-litigation of another lane's work.

## What survived unchanged

All of it, which is the headline. The crate compiles against today's `crates/turn` with no edit to
a single line of Rust, and its 47 cases pass: the seam and its result type, the worker-thread
backend under fuel, ceiling and ledger, the remote backend's submit/poll/fetch/cancel, the
`run_code` tool, the tool bindings a program calls, `catch`, and the settings module. Two days of
drift did not touch the seams it depends on.

The conflicts were the ordinary kind and were resolved by keeping both sides: three new workspace
members in `Cargo.toml` and one new package stanza in `Cargo.lock`; master's three new `AGENTS.md`
entries alongside this branch's one; and a crate count in `README.md` that had moved from eleven to
twenty-two while this branch was away, and is now twenty-three.

## The one thing that no longer fits, stated plainly

**`run_code` is not offered by the shipped binary, and cannot be until `crates/toolset` grows a
shape for it.** That crate did not exist when this branch was written. Master's own rule is in
`AGENTS.md`: a tool crate is not offered by the binary until it is a line in `crates/toolset`, and
`sources()` is the whole registration surface.

The reason this is not a five-line addition is worth writing down, because the next person will
reach for the obvious fix and find it does not work:

- Every existing source is **independent**. `Source::new` takes a finished list, and
  `Source::registered` drains a *throwaway* `ToolRegistry` that only ever holds that one crate's
  tools. Both shapes assume a source can be built without knowing what any other source offers,
  which is what lets `Assembly::build` merge them and refuse a duplicate by naming both crates.
- `run_code` is not independent. Its whole point (`coderuntime: a program can call the harness's
  tools`) is that a program can call the *other* tools, so
  `tetanus_coderuntime::settings::tool` takes an `Arc<ToolRegistry>` of what is already registered.
  Under `sources()` no source can see that, and the registry that would satisfy it does not exist
  until `Assembly::build` has already run.

So wiring it needs a **late source**: one built from the assembled registry rather than beside it.
That is a change to how `Assembly` is shaped - where the duplicate check runs, what `tools.sources`
can switch off, and what `tetanus tools` attributes a late tool to - and it belongs to whoever owns
`crates/toolset`, not to a rebase.

Two consequences are already applied here rather than left implicit:

1. The `README.md` status row **no longer says the runtime is "turned on from the settings
   document"**. As written that promised a deployment could set a key and get a tool, and today it
   would set the key and get nothing, because nothing calls `settings::tool`. The row now says the
   runtime is registered by whoever composes a registry and configured from the document, and names
   the binary wiring in the `Planned` column.
2. `crates/coderuntime/src/settings.rs` is kept exactly as it is. It is correct, it is covered by
   `TC-CODE-SET-1..4`, and it is the function the late source will call. Deleting it because
   nothing calls it yet would throw away the half of the work that the wiring depends on.

## What is unchanged about the rows

The `code-runtime/*`, `e2b/*` row moves as `coderuntime-closeout.md` describes, with one clause
added to its gap column: offering `run_code` from the shipped binary, waiting on the `crates/toolset`
late-source shape above. Everything else in those three notes stands.
