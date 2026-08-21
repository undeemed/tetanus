# Parity update: the MCP client (slice `mcp-client`)

Not folded into [`../parity.md`](../parity.md) by this branch: every lane in
flight edits that file, so each slice writes its note here and one
reconciliation slice folds them in. Three rows, ready to paste.

## 1. Section 3, the `mcp/*` row

Replaces the row reading `| `mcp/*` | 4 | None | MCP client | ② |`.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `mcp/*` | 4 | An MCP client over stdio: the handshake with a revision check, tool discovery that drains pages and refuses a cursor that repeats, tool invocation by the name the server advertised, and a shutdown ladder that leaves no child behind. A server that dies, hangs, or writes a line that is not a message fails the call in flight with a class the journal carries, never the turn | The streamable-HTTP transport, image and audio blocks admitted into a durable attachment store, and the tool bridge's re-sync on `notifications/tools/list_changed` | ② |

## 2. Section 4, the port table

One row, to be inserted in file order.

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `mcp/mcp-client/tests/mcp-client.spec.ts`, `mcp-client.e2e.ts` | `crates/mcp/tests/upstream_client.rs`, `crates/mcp/tests/stdio_server.rs` | What this client does with a message, and what it does with a program | part ported: TC-PORT-MCP-1..13 against a scripted server on a channel pair - the handshake and the initialized notification, a protocol revision nobody speaks, paginated discovery, a cursor that repeats, the raw name on the wire, `isError` as a tool failure rather than a server failure, a JSON-RPC error naming the call it refused, a line that is not a message, a peer that hangs up, answers matched by id rather than by arrival, a server request refused rather than ignored, a budget that fails one call and leaves the connection up, and a content block this build cannot carry named rather than dropped. TC-PORT-MCP-14..20 spend a real child process on the things a channel pair cannot be wrong about: an end-to-end connect-list-call against the fixture server, a close that reaps it, a server that ignores end of input and is killed, a server that exits mid-call, a mute server that fails the handshake with no process left over, a refused handshake, and a real server writing a log line to stdout. Upstream's Cordis plugin lifecycle - load-path guards, `unwrapExports`, HMR disposers, per-app-root name reservations - has no counterpart in a compile-time registry. Its image and audio admission needs the durable attachment store `docs/parity.md` still carries as a gap; the restatement is that an unsupported block is named rather than discarded (TC-PORT-MCP-13). Its streamable-HTTP transport is not ported |

## 3. Changelog row, for `parity-changelog.md`

Appended by the reconciliation slice; the file is `merge=union`, so this row is
written once and never revised.

| 2026-08-21 | An MCP client implemented (`crates/mcp`) and ported (TC-PORT-MCP-1..20), opening the `mcp/*` row. Tools a server advertises are the first capability in this workspace that neither the harness nor the model owns: a program someone else wrote, started by this process, answering on a pipe. Three rules carry it. A line that is not a message ends the connection rather than being skipped, because newline-delimited JSON has no resynchronisation point and a client that skipped would be guessing where the next message starts - and writing a log line to stdout is the single most common way an MCP server is broken, so the fault quotes it. A call that runs out of budget fails alone: the server is told to forget the request, the connection stays up, and one slow tool does not cost every other tool its server. And nothing this crate starts outlives it - `kill_on_drop` under a close-input-wait-kill ladder, with the departure reported rather than assumed, so TC-PORT-MCP-15, -16 and -18 assert against `/proc` that the child is gone, including on the path where the handshake failed and no client was ever returned. The fixture server the cases talk to is a real program behind a `fixture` feature, so a published build does not ship a test double; the channel-pair cases and the process cases are kept apart on purpose, because a process spent to assert a string is a process spent for nothing. |
