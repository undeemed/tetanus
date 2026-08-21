# Contract update: `tetanus serve` hosts an HTTP carrier too (slices `host-bridge`, `host-serve`)

Not applied to [`../interface-contract.md`](../interface-contract.md) by this
branch: the boundary document is edited in place and every lane collides there.
This is the change, ready to fold verbatim, with the changelog row that goes
with it.

Nothing about the payloads changes. What is stale is the document's count of
how many ways they travel, and one row about which of them `tetanus serve`
puts up.

## Stale text 1: section 4.1, the carrier table

**Currently reads** (line 51 onwards):

> One contract, three carriers.
> Every carrier moves the same payloads, so a surface that works over one works over the others.
>
> | Carrier | Who uses it | Framing |
> | --- | --- | --- |
> | In process | the `tetanus` binary | direct calls on the `Engine` trait, no serialization |
> | stdio | an editor or a script driving the binary | JSON-RPC 2.0, one object per line, UTF-8, no embedded newlines |
> | WebSocket | the fire UI | JSON-RPC 2.0, one object per text frame |

**Should read:**

> One contract, four carriers.
> Every carrier moves the same payloads, so a surface that works over one works over the others.
>
> | Carrier | Who uses it | Framing |
> | --- | --- | --- |
> | In process | the `tetanus` binary | direct calls on the `Engine` trait, no serialization |
> | stdio | an editor or a script driving the binary | JSON-RPC 2.0, one object per line, UTF-8, no embedded newlines |
> | WebSocket | the browser panel, and any client that can hold a connection | JSON-RPC 2.0, one object per text frame |
> | HTTP | a client that cannot hold one: a `curl`, a script, a page behind a proxy that will not carry sockets | JSON-RPC 2.0, one object as the body of `POST /api/<method>`, one object as the response body |
>
> The HTTP carrier is a door onto the same room and not a second contract: the
> method is the path, the params are the request body, the answer is the
> response body, and the frame in between is the one the other carriers move.
> It has no push. A `session.subscribe` made over it has nowhere to deliver, so
> a surface that wants events uses the WebSocket, which is served on the same
> address.
>
> Every `POST /api/<method>` must declare `application/json`. Anything else is
> refused with 415 before the call is dispatched, because a browser sends three
> media types cross-site without asking for permission first and none of them
> is that one; demanding it is what turns a cross-site attempt into a preflight
> the origin rules then refuse.
>
> An HTTP status describes the carrier and never the call. A method that ran
> and failed answers 200 with the contract's error object in the body, so a
> caller cannot mistake "the model refused" for "the server is broken".

Three lines further on, "Pushes reach all three carriers the same way"
(line 69) should read **"Pushes reach the three carriers that have a way to
push"**, with the sentence after it naming HTTP as the one that does not.

Section 7.1.1 is titled **"One sink, three carriers"** and should become
**"One sink, the carriers that can push"**; its body needs the same
qualification and no other change.

Line 79 quotes the slogan while making a different point - *"One contract,
three carriers" (§7.1.1) is not true if one of them can be made to fall over
by a peer the others merely refuse* - and should quote the new wording. The
point it is making is unaffected and stands as written.

## Stale text 2: section 4.7, the subcommand table

**Currently reads** (line 1026):

> | `tetanus serve` | hosts the stdio and WebSocket carriers |

**Should read:**

> | `tetanus serve` | hosts the stdio carrier; with `--listen`, the WebSocket carrier; with `--listen --frontend`, the browser panel with the WebSocket and HTTP carriers on that one address |

## Why one address, and why it matters to the contract

The panel is served from the same address the protocol is on. That is not
tidiness: a page served from one origin and dialling another is a cross-origin
WebSocket, which is the case §4.1.2's origin check exists to refuse. Same
origin, and the check protects the deployment instead of fighting the page.

§4.1.2's posture applies to the HTTP carrier exactly as it does to the
WebSocket one, and is enforced there: a peer is admitted or refused before the
JSON-RPC layer sees a frame, so an unauthenticated caller gets a status and
never an error object. The token may travel in the query, as it must for the
socket, or as `authorization: Bearer`, which a `fetch` or a shell can set and
which keeps the secret out of anything that logs URLs.

## Compatibility

Additive in every direction. No method, type, field or error code changes. A
client that speaks only stdio or only the socket sees exactly what it saw
before, and `tetanus serve` with no `--frontend` behaves exactly as it did.
`crates/protocol` needs no change, which is why this note has no companion
type PR: the HTTP carrier reuses `Codec`, so the frames it moves are by
construction the frames the other carriers move.

## Changelog row

> | 1.0 | Records a fourth carrier in §4.1 and widens §4.7's `tetanus serve` row. The HTTP carrier - `POST /api/<method>`, params as the request body, the answer as the response body - was built for the browser panel and for clients that cannot hold a connection open, and it moves the same frames through the same codec, so no payload, method or code changes and `crates/protocol` is untouched. Three properties are written down because they are decisions rather than mechanics: it has no push, so a `session.subscribe` over it has nowhere to deliver and a surface that wants events uses the socket served on the same address; every call must declare `application/json`, refused with 415 before dispatch, because that is what forces a cross-site attempt through a preflight the origin rules refuse; and an HTTP status describes the carrier alone, with a failed call answering 200 and the contract's error object, so that "the model refused" and "the server is broken" cannot be confused. §4.1.2's posture applies to it unchanged and is enforced before the JSON-RPC layer, with the token in the query or as a bearer header. §7.1.1's title and the two "three carriers" sentences are corrected to say which carriers can push. |
