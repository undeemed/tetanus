# Contract note: `catalog.tools` over a carrier does not answer what `tetanus tools` prints

Found by the presentation lane while verifying the web panel against the
tool-wiring block on master `850f308`. It is a question for whoever owns
`crates/cli`'s serve arm and `crates/engine`'s catalogues, not something a
surface can answer.

## What is reproducible

One binary, one tree, no settings document:

```sh
tetanus tools | grep -cE '^[a-z_]+ '        # 26
tetanus info  | grep -i tool                # tools      26

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"rpc.hello","params":{"client":{"name":"p","version":"0"},"protocol":"1.0"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"catalog.tools","params":{}}' \
  | tetanus serve                            # result.tools == []
```

Over the WebSocket carrier the same call answered a single tool, `echo`. So the
three answers to "what can this build run" are **26**, **1** and **0**,
depending on who is asked.

## Why it matters more than a wrong number

`crates/cli/src/tools.rs` states the invariant this breaks, in its own words:
the registry is built in one place "so `tetanus tools` cannot list a tool a run
does not have. It answers `catalog.tools`." And `crates/toolset` exists so that
"the catalogue and the per-session registry both come from here … a tool added
here is a tool the model can call *and* a tool `tetanus tools` lists, and those
two cannot disagree." Over a carrier they disagree.

The surface consequence is not cosmetic. Every client that is not the binary
itself reads the boundary, so:

- the web panel's model-and-tool catalogue shows a deployment with one tool;
- anything deciding what a build can do from `catalog.tools` - including this
  lane's own "does this build have goals at all" check in
  `web/app/features.js`, which reads exactly this call - is told no;
- an editor or script driving the harness over stdio sees an empty toolbox on a
  build with twenty-six tools.

## What this lane is not doing

Not fixing it. The registry the served engine is constructed with is engine and
CLI territory, and a page-side workaround - falling back to a hard-coded list,
or inferring the toolbox from tool calls as they appear - would paper over a
boundary defect with a second, staler answer. Three disagreeing answers is
already the problem; a fourth on the client is not the fix.

Not changing the panel's behaviour either. The empty state currently reads
"Goals, plans and task lists are not part of this build yet" on a build that
has them, which is wrong - and it is wrong *because it believes the boundary*,
which is the behaviour to keep. It will read correctly the day the call does.

## What would tell the two apart

The gap is between `catalog()` in `crates/cli/src/tools.rs`, which reads the
document and returns 26, and whatever the served `HarnessEngine` was
constructed with, which returns 0-1. Both are supposed to come from
`tetanus_toolset::registry`. The serve arm does pass one:

```rust
tools: Arc::new(registry(policy, &document, &listing(&booted.resolved))?),
```

so the interesting question is whether that registry is the one
`Catalogs::new(&config)` ends up reading, and whether the composition it is
built from has the workspace and session context the `fs`, `features` and
`exec` sources need - a source given none of those registers nothing, and
`echo` is precisely the source that needs nothing.

A case that would have caught it, and that this lane would like to exist: one
that asserts `catalog.tools` over a carrier and `tetanus tools` name the same
set. There is no client-side substitute; a surface cannot tell an empty
toolbox from an empty answer.
