# Parity: the socket on the page's own port

Upstream: [`host/webserver`]'s upgrade table - "the upgrade handler owns the
protocol handshake and connection contents; the webserver only delivers the raw
socket and request" - with the `/api` bridge and downlink WebSockets owned by
the connection plugin.

tetanus: `WebServer::register_upgrade` now hands over the socket, and
`tetanus serve --frontend` seats `crates/rpc`'s carrier at `/api/ws`.

## What changed

| Before | Now |
| --- | --- |
| an upgrade route answered with a `Response` | it is handed the `TcpStream` and the parsed request |
| the head was read off the socket | the head is **peeked**, so a handover leaves the socket untouched |
| the carrier bound a second port | one port: the page and the protocol share it |
| the manifest named `ws://host:<other port>` | it names `ws://host:<same port>/api/ws` |

## Why peek, and not read

An upgrade handler has to read the request itself: `crates/rpc` does its own
WebSocket handshake, its own origin check and its own token check (§4.1.2), and
none of that works on a socket somebody already took the head off. So the
carrier peeks - the kernel keeps the bytes until they are taken - parses what
it sees, and consumes the head **only** for requests it is going to answer
itself.

Two things fell out of that and are now covered:

- A peek returns what has arrived, so a head still in flight would spin the
  loop. Nothing new means wait a moment, and the whole read is bounded by a
  five-second `HEAD_TIMEOUT`. A client that opens a socket and dribbles must
  not hold a task forever.
- TC-HOST-WEB-6 now proves the handover rather than asserting a string: the
  test's handler reads the request line back off the socket and answers with
  it, so a carrier that ate the head fails the case.

## Why one port matters, beyond tidiness

A page served from one origin and dialling another is a cross-origin
WebSocket - exactly the case §4.1.2's origin check exists to refuse. Same
origin, and the check protects the deployment instead of fighting the page.

`tetanus serve --frontend` therefore names its own origins to the carrier: the addresses the
server can be opened on, in both schemes, and nothing else. `localhost` and
`127.0.0.1` are two origins to a browser and one machine to everybody else, so
both are named; `https` is named because a proxy terminating TLS sends it while
the server behind it is plain. There is no wildcard, because a wildcard here is
the refusal deleted.

Verified in a real browser: the page loads from `http://127.0.0.1:5313`, dials
`ws://127.0.0.1:5313/api/ws`, reports `connected`, and a question comes back
`turn 1 · natural · 2 steps · 68 tokens` on the mock.

## Tests

| Id | Case | Expected result |
| --- | --- | --- |
| TC-HOST-WEB-6 | an upgrade, and a plain GET of the same path | the handler reads the request line itself; the plain GET is 404 |
| TC-CLI-WEB-3 | the origins a loopback panel allows | its own, both schemes; not another port, another site, or `null` |
| TC-CLI-WEB-4 | the wildcard bind | the public name and the loopback names, and nothing else |
