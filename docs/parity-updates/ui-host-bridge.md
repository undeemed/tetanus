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

## Not here yet

- **`ServerRequest`/`ClientResponse`** - the SSE half and `POST /api/respond`.
  A push needs a held-open response, and until it exists a subscription made
  over this carrier has nowhere to deliver: the sink drops rather than buffers,
  and the socket at `/api/ws` is the carrier that pushes.
- **Projections, `session.history` paging, model selection.** Those are
  gateway-owned domains upstream, and each needs contract surface this build
  does not publish yet.

## Tests

| Id | Case | Expected result |
| --- | --- | --- |
| TC-CLI-WEB-5 | `text/plain`; the handshake; a call after it; a method nobody has | 415 before dispatch; 200 and the capabilities; 200 and the session list; 200 with `-32601` |
