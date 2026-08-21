# Parity: the `/api` bridge

Upstream: [`host/apiproxy`] - "the API gateway shared by every client", whose
wire rule is `ClientRequest` as the body of `POST /api/<method>` and
`ServerResponse` as that POST's response body.

tetanus: `crates/cli/src/bridge.rs`, mounted by `tetanus serve --frontend` on the prefix
`/api/`.

## What is here

| Upstream | tetanus | Same? |
| --- | --- | --- |
| `POST /api/<method>`, params in the body | same | yes |
| the response is that POST's body | same | yes |
| every POST must declare `application/json`, else **415 before dispatch** | same | yes |
| business errors ride the result; "HTTP status expresses only the carrier" | same: an unknown method is 200 with `-32601` in the envelope | yes |
| responses echo the request's id, never mint one | one POST is one call; the id is the carrier's | in effect |
| the package registers no routes; carriers wrap the gateway | the bridge is mounted by the serve composition, not by the host | yes |

## One dispatch table

The frame goes through `crates/rpc`'s codec, the same one the socket feeds. A
bridge that matched method names for itself would be a second table deciding
what the contract answers, and they would disagree on the first method added to
one of them.

That also settles the handshake: the codec requires `rpc.hello` first, and the
bridge holds one codec, so a caller greets it once and later calls are
answered. Minting a codec per POST would mean either skipping the handshake -
deciding a contract rule that is not a carrier's to decide - or demanding a
hello with every request.

## The 415 is a security rule, not a formality

A form post from any page reaches a loopback server without a preflight,
because three media types are simple enough to skip one. `application/json` is
not among them, so demanding it turns every cross-site attempt into a preflight
that the origin rules then refuse. Upstream says this in as many words; the
case asserts it.

## The push half

`GET /api/events` is the stream, and it is upstream's `ServerRequest` seat: a
POST answers once, and a subscription is a thing that keeps answering.

- The frames on it are **the notifications the other carriers send**, built the
  same way. §4.1's promise is that every carrier moves the same payloads, and a
  `data:` line holding a different shape would make this a second contract
  wearing the first one's method names.
- **One reader list, not one per POST.** The bridge is one logical connection
  spread over many requests - it holds one codec, so it greets once - and a
  subscription made by one POST belongs to that connection however many POSTs
  follow. A reader who opens the stream is that connection's ears.
- **A reader who stops reading is dropped, never waited on.** The reader has
  gone; the turn it was watching has not, and a carrier that blocked a turn on
  a closed tab would make this the worst way to watch a session rather than the
  second best.
- **`: open` is written before anything happens.** `EventSource` fires on the
  headers, but a proxy that buffers until the first byte of the body does not,
  and a reader should not have to guess whether it is connected.
- `cache-control: no-cache`, because without it a proxy between here and the
  reader may hold the whole response until it ends, which for a stream is
  never.

## Not here yet

- **`POST /api/respond`** - the client's answer to a server-initiated question.
  Nothing on this contract asks one yet: `ui.ask` is reserved (§4.2), and the
  seat for its answers lands with it.
- **Projections, `session.history` paging, model selection.** Those are
  gateway-owned domains upstream, and each needs contract surface this build
  does not publish yet.

## Tests

| Id | Case | Expected result |
| --- | --- | --- |
| TC-CLI-WEB-5 | `text/plain`; the handshake; a call after it; a method nobody has | 415 before dispatch; 200 and the capabilities; 200 and the session list; 200 with `-32601` |
