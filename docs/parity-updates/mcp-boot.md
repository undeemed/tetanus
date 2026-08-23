# Note: the servers a document declares are started at boot (slice `mcp-boot`)

Not a parity port. `crates/mcp` could connect a server and put its tools in a
registry from the day it landed, and nothing called it: a deployment could
write `mcp.servers.<name>` with a command, the binary would read the key,
compose an empty `mcp` source, and offer nothing.

## 1. What was wrong, and the pattern it belongs to

This is the third time the same shape has produced a defect on a green
workspace, and the third is what makes it a pattern worth naming:

1. Five tool crates landed and the binary offered none of them.
2. `mcp.servers.*` was read and never acted on - this slice.
3. `tetanus serve --frontend` served one tool while `tetanus serve` served
   twenty-six.

Each time, a crate was complete and tested, and the suite was green, because a
capability crate tests its own capability and nothing tested the program people
run. **"The crate is complete" and "the binary reaches it" are two claims, and
they need two cases.** `AGENTS.md` carries that as a rule.

## 2. Where the boot lives, and why not in the binary

`tetanus_toolset::Servers` reads `mcp.servers.*`, starts every enabled server,
and hands the bridged tools to `Composition::mcp`. It is in `crates/toolset`
rather than `crates/cli` because the binary deliberately depends on no tool
crate - the assembly is what knows they exist - and because what it produces is
exactly what a composition takes.

Every serving surface gets it for free, because they all go through
`tools::served`. That is worth stating: when this slice was first written, it
wired `serve`, `run`, `chat` and the catalogue by hand and left the frontend
out - the same omission that caused defect (3) above. Rebased onto `served`,
the frontend gets declared MCP servers without a line being written for it, and
TC-CLI-CAT-12 covers the result.

## 3. The rules

- **A server that will not start does not stop the harness**, and is *named*.
  One broken line in a document must not cost a laptop its working agent; a
  tool silently absent is a capability nobody took away. The fault, with its
  class, goes to stderr at boot rather than being discovered later as a tool
  the model kept not calling.
- **One connection per server per process**, shared by every session. An MCP
  tool belongs to its supervisor rather than to whoever called it, so a server
  per session would multiply child processes by the number of conversations.
- **Nothing started outlives the command.** Every surface runs the
  close-input, wait, kill ladder before it returns; `kill_on_drop` is the
  backstop for the paths that never get there. The cases read the process table
  after `tetanus tools` and after `tetanus run` to prove it.
- **The catalogue starts them too, and stops them again.** A listing that
  skipped them would advertise a set no run offers; a listing is not a session,
  so it does not keep them.

## 4. Cases

TC-CLI-MCP-1..6 (`crates/cli/tests/mcp_boot.rs`), all against the shipped
binary: a declared server started and its tool offered, the bridged name
carrying the document's name for the server, a server that will not start named
with the run carrying on, `enabled: false` honoured, the page and a turn
offered the same set, and nothing left running after either command.

Mutation-verified: with the boot replaced by `Servers::none()`, all six fail.
Two did not at first - the orphan case and the switched-off case both asserted
*absence*, which is also what a build that starts nothing reports - so each now
also asserts the tool **is** offered on the path where it should be.

The server the cases use is a small MCP server in Python, written in the test
and started by the binary as an ordinary declared server. Reusing the fixture
in `crates/mcp` is not available: `CARGO_BIN_EXE_*` is only set for binaries of
the package under test, and the two ways round that either ship a test double
in a release build or pass only under `--workspace`.

## 5. Rows to fold

### Section 3, the `mcp/*` row

Append to `Today`: ", with the servers a settings document declares started by
the binary at boot, their tools offered beside the harness's own on every
carrier, a server that will not start named rather than silently absent, and
none of them outliving the command that started them".

### Changelog row

Appended to `parity-changelog.md` by this branch.
