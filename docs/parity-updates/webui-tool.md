# Parity: the tool frame

Upstream: [`client/ui-tool`], 26 components - one shared frame and a view per
tool, so a shell command looks like a terminal and a file read looks like a
file.

tetanus: `web/app/tools.js`.

## What is here

The frame, and the seam the per-tool views drop into. `views` is a table keyed
by tool name; a tool with an entry is drawn its own way, and a tool without one
gets the shared frame: the name on a fold, the arguments as a tree, the result,
and whether it worked.

## Why only one view

The instruction was to build tool rendering against the fs, exec, mcp and web
tools that now exist. They exist - and they exist on other lanes' branches,
none of which is on this tree or on master. Measured just now:

| Branch | Tools it adds |
| --- | --- |
| `fm/tetanus-p2-mcp` | `web_fetch`, `web_search`, plus whatever an MCP server advertises |
| `fm/tetanus-p2-shell` | process execution |
| `fm/tetanus-p2-fs` | a filesystem service - `crates/fs` has containment, sandbox and observation, and registers no `ToolDescriptor` at all yet |
| this tree, and master | `echo` |

So a `read_file` view written here today would be drawn against a shape nothing
in this tree can produce, judged against no output, and rewritten when the real
one lands. That is the mock-and-rewrite the gap list exists to prevent, and the
same rule that kept `TerminalBlock` and `DiffBlock` out of the primitives
slice.

What is built instead is the part that has to exist before any of them and
cannot be added afterwards without touching all of them. When the fs lane
lands `read_file`, its view is one entry in `views` and no change anywhere
else.

## The generic frame is not a placeholder

A tool this page has never heard of is the ordinary case rather than the
exception: MCP servers advertise their own tools, so the set is open by
construction, and a surface that rendered only the tools it knew would show
blanks for exactly the tools a deployment added on purpose. The generic frame
is complete - name, arguments, result, outcome - and stays the right answer for
those tools forever.

## Two decisions worth stating

- **Folded by default.** A transcript is read for the conversation and opened
  for the detail; an unfolded result pushes the reply after it off the screen
  the moment a tool returns a file. Upstream folds the same way.
- **Failure is the tool's own answer, never inferred.** A surface that guessed
  failure from an empty result would call a successful `list_dir` on an empty
  directory a failure. `ok` comes from the protocol.

## Tests

`target/probe-primitives.mjs`, now **28/28**: a tool with no view still draws
whole with its arguments as a tree; the one tool with a view uses it and does
not make a reader open a tree to find a sentence; an empty result that
succeeded is not marked failed; a failure is, in the tool's own words.

Verified in Chrome against a live server: the `echo` call and its result both
fold, both carry the tool's name.
