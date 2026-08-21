# Note: the tool registration surface, and the line each pending crate adds

Slice: `crates/toolset` - the one place that says which tools this build
offers.
Branch: `fm/tetanus-p2-toolset`.

This is not a parity port. It is the seam five in-flight tool crates each need
one line in, written against what is on `master` today so that the line is
settled before the crates arrive rather than negotiated afterwards by whoever
merges last.

## 1. Why it is a crate and not a function in `crates/cli`

The registry lived in two places: `crates/cli/src/main.rs::registry()` and
`crates/engine/src/lib.rs`'s `EngineConfig::default`. Both said `EchoTool`, so
they agreed by coincidence. `crates/cli` belongs to the presentation lane by
[`../interface-contract.md`](../interface-contract.md) §4.7, so no tool lane can
add itself there, and every tool lane would otherwise have to ask that lane for
a line - five times, serially, at the end.

`crates/toolset` depends on `tetanus-turn` and `tetanus-config` only. Both call
sites now read it, which is one line changed in each and no further change as
crates land. TC-TOOLSET-2 holds the two together by reading the engine's own
default rather than a copy of it, so wiring either back to a private expression
fails a case.

## 2. What a landed crate adds

Exactly one entry in `tetanus_toolset::sources()`, and one dependency line in
`crates/toolset/Cargo.toml`. Nothing else in the workspace changes: not the
binary, not the engine, not a test that lists tools.

The pending crates, with the line each will add. Names are what each lane
already registers, so these are transcriptions rather than proposals.

**`crates/fs`** (`fm/tetanus-p2-fs`, seven tools). Its tools are composed per
session, because the observation policy is keyed on the session id and the
backend is chosen by the deployment's filesystem mode - so this source is built
by a function taking those two, and `sources()` grows a parameter rather than a
bare entry:

```rust
Source::new(
    "fs",
    "Reading and changing files inside the workspace.",
    tetanus_fs::FsTools::new(backend, observed, session_id).tools(),
),
```

`FsTools::register(&mut registry)` exists today and is what the lane's own cases
use; landing it here needs a `tools()` accessor returning the same seven as a
`Vec<Arc<dyn Tool>>`, which is a two-line addition in that crate and is the only
change this note asks of it.

**`crates/exec`** (six tools):

```rust
Source::new(
    "exec",
    "Running commands, bounded in output and in time.",
    tetanus_exec::tools(),
),
```

**`crates/features`** (`fm/tetanus-p2-features`, the built-in feature tools).
Journal-backed, so each is composed with the session log:

```rust
Source::new(
    "features",
    "The task list, the standing goal, plan mode, feedback, skills and the workspace sketch.",
    tetanus_features::tools(log, skills, cwd),
),
```

**`crates/mcp`**:

```rust
Source::new(
    "mcp",
    "Tools an MCP server offers, discovered at boot.",
    tetanus_mcp::tools(clients),
),
```

**`crates/web`**:

```rust
Source::new(
    "web",
    "Fetching a URL and searching the web.",
    tetanus_web::tools(),
),
```

## 3. What the surface decides, so five lanes do not each decide it

- **A duplicate name is refused, naming both sources** (TC-TOOLSET-3).
  `ToolRegistry::register` keys by name and the last registration wins, which is
  correct for a registry and wrong for an assembly: the model would be offered
  one tool's schema and run another's body. `read` is the collision to expect -
  `crates/fs` offers it and an MCP server may too - and the assembly turns that
  into a startup error naming both crates instead of a silent swap.
- **Sources are named, and a deployment selects by source** (TC-TOOLSET-5..8).
  `tools.sources: [fs, features]` is how a deployment turns off the web tools,
  rather than naming fifteen tools it does not want. An absent key is every
  source; an explicit empty list is none.
- **Selection is in declaration order** whatever order a document names, so two
  deployments that named the same sources get the same registry. Which order the
  *model* reads them in stays `tools.order`'s.
- **Every tool is attributable** (TC-TOOLSET-9). With forty tools on offer, "why
  is this here" is a question a user asks, and the answer comes from the same
  place the tools do.

## 4. What this deliberately does not do

- **No feature flags.** A `#[cfg(feature = "fs")]` per source would let a build
  ship without a crate that is in the workspace, and the first thing it would
  buy is a matrix of build configurations nobody runs. A crate in the workspace
  is composed; a deployment that does not want its tools says so in
  `tools.sources`, which is a runtime answer a user can change without a
  rebuild.
- **No per-tool enable list.** Sources are the unit because a tool crate is
  what lands, what collides, and what a user recognises. A per-tool list would
  also have to answer what happens when a named tool disappears, and the honest
  answers are all worse than naming the crate.
- **No ordering opinion.** `tools.order` already settles what the model reads
  first, is already checked against the registry, and already has cases
  (TC-ORDER-*). A second ordering here would be two things to keep in step.

## 5. Parity

Nothing in this maps to an upstream spec file. Upstream composes tools through
Cordis plugin loading, where each plugin registers into `ctx.tools` and a
duplicate is resolved by load order - the behaviour TC-TOOLSET-3 deliberately
refuses. The difference is worth stating in `docs/parity.md` when a tool row
next moves: tetanus composes tools at build time from a declared set, so
duplicate detection is possible at all, and a deployment's control is over
sources rather than over a plugin list.
