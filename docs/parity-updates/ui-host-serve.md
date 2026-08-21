# Parity: the HTTP route carrier

Upstream package: [`host/webserver`](../../README.md) - "Web HTTP and upgrade-route
registration plugin (default-exported `WebServer`, config `{host, port}`)".

tetanus: `crates/host`, `WebServer`.

## What is here

| Upstream | tetanus | Same? |
| --- | --- | --- |
| `register(route)`, named `exact`/`prefix` | `WebServer::register(Pattern, path, handler)` | yes |
| `registerUpgrade(route)`, exact pathname | `WebServer::register_upgrade(path, handler)` | yes |
| `registerFallback(handler)`, single seat | `WebServer::register_fallback(handler)` | yes |
| duplicate path throws | `Err(Taken::Route)` at registration | yes |
| second fallback claim throws | `Err(Taken::Fallback)` | yes |
| both return a disposer | both return `Registered`, a drop guard | yes, in Rust's spelling |
| match order: exact, longest prefix, fallback | same, and asserted | yes |
| unmatched with no fallback: 404 | same | yes |
| `port`, `host` readable as composition facts | `WebServer::address()` | yes |
| `host` accepts only `127.0.0.1` and `0.0.0.0` | same, refused at bind | yes |
| a handling failure is 400 and a warning, never an exit | same | yes |
| the package never prints | same; the URL line belongs to the shell | yes |

## What is not here yet, and where it goes

- **`tapIndex`/`applyIndexTaps`** - the index transform chain. It exists for the
  boot manifest, which needs the frontend to exist first; it lands with
  `frontend-static` in the next slice rather than as a hook with no caller.
- **Upgrade handling proper.** The upgrade table matches and dispatches, but a
  handler still answers with a `Response` rather than being handed the raw
  socket. The socket handoff lands with the `/api` bridge, which is the first
  route that needs one; `crates/rpc` already owns the WebSocket half.
- ~~**Disposal that waits.**~~ Landed. `stop()` stops the accept loop and
  `serve` then waits for the connections already in hand before it returns -
  a request halfway through its response is a reader who would otherwise see a
  truncated one. The wait is bounded at two seconds, because a handler that
  will not finish must not keep a process alive.

  What upstream does by destroying tracked sockets, this does by telling: a
  handler that took a socket watches `stopping()`, since the carrier gave that
  socket away and cannot close it. The SSE stream is the case that matters -
  it holds a response open for ever by design - and it writes an
  `event: closing` frame and ends, rather than dropping, so a reader does not
  read a closed socket as a network fault and reconnect into nothing.

## Deliberate differences

- **Ownership is a drop guard, not a returned function.** Rust already has the
  disposer pattern and it is spelled `Drop`; a returned closure would be a
  second lifetime story for the same fact.
- **The upgrade is read off the header, not the pathname.** Upstream matches an
  upgrade route by exact pathname; we do that too, but only for requests that
  actually carry `upgrade: websocket`. A browser's plain `GET /ws` would
  otherwise reach a socket handler and hang.
- **A refused connection is drained before it is closed.** Not in the spec, and
  necessary: a 400 written while the client is still sending is thrown away by
  the RST that a close with unread bytes sends, so the reader sees a reset
  rather than the refusal. TC-HOST-WEB-8 is the case that found it.
