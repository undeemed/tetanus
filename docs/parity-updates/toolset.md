# Note: the tool registration surface, and the binary that reads it

Slice: `crates/toolset` - the one place that says which tools this build
offers - and the wiring that makes the shipped binary offer them.
Branch: `fm/tetanus-p2-toolset-wire`.

This is not a parity port. It is the seam the landed tool crates are composed
through, and the closing of a gap that a green workspace did not show: five
tool crates existed, were tested, and could not be called from `tetanus`.

## 1. What was actually wrong

`crates/fs`, `crates/exec`, `crates/features`, `crates/mcp` and `crates/web`
had all landed on `master`. The binary's registry was seven lines in
`crates/cli/src/main.rs` that composed `EchoTool` and the shell tools by hand.
So `tetanus tools` listed six entries, and a model driven by this binary could
not read a file, write one, keep a task list, or fetch a URL - on a build whose
suite was green, because every one of those crates tests its own tools directly
and none of them tests that the program people run offers them.

`crates/cli` belongs to the presentation lane by
[`../interface-contract.md`](../interface-contract.md) §4.7, so no tool lane
could add itself there. That is the reason the assembly is its own crate rather
than a function in the binary.

The tools page now lists twenty entries with no document, and twenty-two with
the web tools turned on.

## 2. What a landed crate adds

Exactly one entry in `tetanus_toolset::sources()` and one dependency line in
`crates/toolset/Cargo.toml`. Nothing else in the workspace changes: not the
binary, not the engine, not a test that lists tools.

| Source | Tools today | Composed from |
| --- | --- | --- |
| `builtin` | `echo` | nothing - the turn itself |
| `exec` | `shell`, `shell_open`, `shell_run`, `shell_close`, `shell_list` | the session's interrupt |
| `fs` | `read`, `write`, `edit`, `list`, `glob`, `stat`, `delete` | the workspace root, the filesystem mode, the session id |
| `features` | `todo_write`, `get_goal`, `update_goal`, `exit_plan_mode`, `report_feedback`, `skill`, `workspace_info` | the session's journal, the skill roots |
| `web` | `web_fetch`, `web_search` | the settings document; empty until it says so |
| `mcp` | whatever the declared servers advertise | servers connected at boot; empty until one is declared |

Two things the note this surface shipped with did not anticipate.

**No tool crate was edited.** The original note asked `crates/fs` for a
`tools()` accessor returning `Vec<Arc<dyn Tool>>`, and implied the same of the
others. `Source::registered` drains a throwaway `ToolRegistry` instead, so a
crate that publishes only `register(&mut ToolRegistry)` - which is most of them
- composes as it is. That is five pull requests against five lanes that did not
have to happen. The assembly still sees each source's names separately, which
is all the duplicate check needs.

**`sources()` takes a `Composition`.** Most of what landed is not free-standing:
the shell tools read the turn's stop switch, the file tools key their
read-before-write observations on a session id, and the feature tools keep
their whole state as a fold over that session's journal. A bare `sources()`
could only ever have composed `builtin`.

## 3. What the surface decides, so the tool lanes do not each decide it

- **A duplicate name is refused, naming both sources** (TC-TOOLSET-3).
  `ToolRegistry::register` keys by name and the last registration wins, which is
  correct for a registry and wrong for an assembly: the model would be offered
  one tool's schema and run another's body. `read` is the collision to expect -
  `crates/fs` offers it and an MCP server may too - and the assembly turns that
  into a startup error naming both crates instead of a silent swap.
- **Sources are named, and a deployment selects by source** (TC-TOOLSET-5..8,
  TC-CLI-TOOLSET-3). `tools.sources: [builtin, fs]` is how a deployment turns
  the rest off, rather than naming fifteen tools it does not want. An absent key
  is every source; an explicit empty list is none.
- **Selection is in declaration order** whatever order a document names, so two
  deployments that named the same sources get the same registry. Which order the
  *model* reads them in stays `tools.order`'s.
- **Every tool is attributable** (TC-TOOLSET-9). With twenty tools on offer,
  "why is this here" is a question a user asks, and the answer comes from the
  same place the tools do.
- **Reaching outside the machine stays opt-in** (TC-CLI-TOOLSET-5). `web` and
  `mcp` are declared and empty until the document names them, which is the
  posture `crates/web` already took for its own tools (TC-PORT-WEB-27) held one
  level up. Declared-and-empty rather than absent, so `tools.sources` means the
  same thing on every host.

## 4. The engine is deliberately not a call site

The surface this slice started from wired `EngineConfig::default()` to the
shipped assembly, and TC-TOOLSET-2 asserted the two agreed. That is not what
landed, for two reasons.

The engine has no session. Composing the file tools there would key their
observations on nobody and fold the feature tools over a journal that is not a
session's - tools that are present and quietly wrong, which is worse than
absent.

And it would make `crates/engine` depend on every tool crate, which is the line
[`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) §4.2 draws when it says nothing
depends on `tetanus-fs`: a consumer of the tool seam, not a layer under it, so
a harness composed without file tools still builds.

What the engine must not do is grow a *private* expression of the tool set,
which is the drift that justified this crate in the first place - `crates/cli`
and `crates/engine` each said `EchoTool` and agreed by coincidence. TC-TOOLSET-2
now holds the engine's default against the assembly's `builtin` source, so
adding a tool to one without the other fails. The engine keeps its offline
minimum; the binary composes the shipped set.

## 5. What this deliberately does not do

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
- **MCP servers are not started by the binary yet.** The `mcp` source takes
  already-connected tools, because a handshake is asynchronous and a
  composition is not. `tetanus_mcp::settings::connect_all` is the function that
  fills it and it is tested (TC-PORT-MCP-38); the binary calling it at boot is
  the next slice, and until then the source is declared and empty.
- **`Parallelism` is stated, not configured.** `crates/features` gives it no
  default on purpose, saying the composer states it; this is the composer, and
  it states `SingleActive`. A settings key for it is a feature, not wiring.

## 6. Parity

Nothing in this maps to an upstream spec file. Upstream composes tools through
Cordis plugin loading, where each plugin registers into `ctx.tools` and a
duplicate is resolved by load order - the behaviour TC-TOOLSET-3 deliberately
refuses. The difference is worth stating in `docs/parity.md` when a tool row
next moves: tetanus composes tools at build time from a declared set, so
duplicate detection is possible at all, and a deployment's control is over
sources rather than over a plugin list.

## 7. Rows to fold

### Section 3, the tools row

Append to `Today`: ", composed from one declared set of named sources that the
settings document selects by crate, so the tools page and the registry a turn
dispatches from are one list".

### Changelog row for `parity-changelog.md`

| 2026-08-22 | The tool registration surface (`crates/toolset`, TC-TOOLSET-1..10 plus 1b and 1c) and the binary wired to it (TC-CLI-TOOLSET-1..6). Five tool crates had landed and the shipped binary offered none of them: it composed `echo` and the shell tools by hand, so `tetanus tools` listed six entries on a build whose suite was green, because every tool crate tests its own tools and none of them tested the program people run. The tools page now lists twenty. The general lesson is the one TC-CLI-TOOLSET-1 is shaped around: a crate existing, being tested, and being reachable from the binary are three different facts, and only a case that execs the binary can tell them apart. Two findings came out of the wiring. `Source::registered` drains a throwaway registry, so no tool crate needed an accessor added and five cross-lane pull requests did not have to happen. And the first cut composed the registry behind an `expect` on the claim that `tools.sources` was checked when the document was read - nothing checked it, so a misspelled source name panicked; it is now refused against the document that set it, and the check runs once at boot for the path whose closure has nowhere to return a failure to. |
