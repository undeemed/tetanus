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

---

# Parity update: the tool bridge and the reconnect supervisor (slice `mcp-bridge`)

Extends the rows above; fold both in one pass.

## 1. Section 3, the `mcp/*` row

Supersedes the `Today` and `Gap` columns proposed by the slice above.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `mcp/*` | 4 | An MCP client over stdio - handshake with a revision check, paginated discovery, invocation by the advertised name, and a shutdown ladder that leaves no child behind - plus the bridge that puts a server's tools in the tetanus registry beside the native ones under `mcp__<server>__<raw>`, dispatched by the same pipeline, and a reconnect supervisor with a bounded budget, a stability window that resets it, and a shutdown that cancels a pending backoff. A server that dies, hangs, or writes a line that is not a message fails the tool call in flight with its class in the result, and the turn carries on | The streamable-HTTP transport, image and audio blocks admitted into a durable attachment store, and a run-time tool re-registration: a tetanus registry is settled at boot, so a reconnected server's tool set changes what a call is *answered* with, never what the model was offered | ② |

## 2. Section 4, the port table

Extends the row above with the bridge and supervisor files.

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `mcp/mcp-client/tests/reconnect.spec.ts`, the `publicToolName` and `syncTools` halves of `mcp-client.spec.ts` | `crates/mcp/tests/supervisor.rs`, `crates/mcp/tests/bridge_turn.rs` | Keeping a server up, knowing when to stop, and what the model is offered | part ported: TC-PORT-MCP-21..28 for a replaced connection serving calls, the attempt cap, a crash loop that exhausts it although every connect succeeds, an uptime past the stability window buying a fresh budget, reconnecting turned off, a shutdown that cancels a backoff rather than waiting it out, a policy refused where it is written, and a tool the live server no longer advertises refused as unknown rather than sent. TC-PORT-MCP-29..33 for the bridge: the naming contract, two servers and a native tool of the same raw name side by side, a real server's tool called through a real turn, a server that stops answering failing its call and not the turn, and a server that dies mid-turn replaced in time for the next one. Upstream's generation bookkeeping - disposers, stale notification handlers, a re-sync racing disposal, a rollback when a foreign tool squats on the namespace - is about a registry it mutates at run time and has no counterpart in one settled at boot. One rule is an addition: a server whose own name carries the `__` separator is hashed, because `a__b`/`c` and `a`/`b__c` are one name under upstream's rule |

## 3. Changelog row

| 2026-08-21 | The MCP tool bridge and reconnect supervisor (`crates/mcp/src/tools.rs`, `src/supervisor.rs`, TC-PORT-MCP-21..33). A server's tools now reach the model: they are registered under `mcp__<server>__<raw>` beside tetanus's own and dispatched by the ordinary pipeline, so nothing in the turn engine knows an MCP tool from a native one. Every MCP call is exclusive, deliberately - `ToolMode` is opt-in because a tool may overlap only for arguments it has looked at, and nothing here has looked at anything, so a barrier is the answer that cannot make things worse. The supervisor's budget is the part with teeth: delays double to a ceiling, a cap ends the retrying for good, and the budget resets only on real uptime past that ceiling, so a server that connects and dies four times a second exhausts its cap instead of restarting for ever. Shutdown cancels a pending backoff rather than joining it. Two findings came out of writing the cases. Upstream's public name collides for distinct identities when a server's own name carries the `__` separator, and since a tetanus server name comes from a settings document that is reachable here too, so a separator in a server name now forces the hash. And a reconnect that has launched a process has not yet finished a handshake, which is why the health state and not the launch count is what a caller waits on. |
