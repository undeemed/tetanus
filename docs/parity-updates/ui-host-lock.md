# Parity: the same lock on both doors

Upstream: [`host/apiproxy`]'s contract layer and the trust posture §4.1.2
states for the WebSocket carrier.

tetanus: `crates/cli/src/bridge.rs` now admits a caller with the same `Auth`
the socket uses.

## The defect this closes

The bridge landed with no authentication at all. `tetanus web --token <T>`
therefore locked the socket and left every POST open: a caller who could reach
the port could start turns, read every journal in the sessions directory and
read the resolved configuration, which is exactly the reach §4.1.2 says a
connected peer has. A door with a lock beside a door without one is a room with
no lock.

Both doors now ask the same question, with the same `Auth`, before anything is
dispatched - so an unauthenticated caller never reaches the JSON-RPC layer,
which is §4.1.2's own arrangement and the reason the refusal is a status rather
than an error frame.

## Where a token may travel

| Carrier | Spelling | Why |
| --- | --- | --- |
| socket | `?token=` or `Sec-WebSocket-Protocol` | a browser cannot set headers on a handshake (§4.1.2) |
| bridge | `?token=` | the same reason, and the page already has it in its URL |
| bridge | `authorization: Bearer <token>` | a `fetch` or a shell can set a header, and a URL is logged by more things than a header |

Neither is required over the other, and the query spelling is the one the page
uses because it is the one it can carry from its own address.

## What is deliberately still open

The page itself. A stranger who reaches the port is served the same HTML, and
that is what makes the token deliverable to a reader who was handed the URL: it
is in their address bar, not in the document. Under `--open-to-anyone` the
manifest publishes a minted token instead, and the note on that flag is exact
about what it is worth.

## Tests

| Id | Case | Expected result |
| --- | --- | --- |
| TC-CLI-WEB-7 | a POST with no token, with the token in the query, and with a bearer header, under `--token` | 401; 200; 200 - and the page still served |
