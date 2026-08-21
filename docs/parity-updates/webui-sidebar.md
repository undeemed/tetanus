# Parity: the session list

Upstream: [`client/ui-sidebar`] - the standing list of conversations.

tetanus: `web/app/sidebar.js`, opened from the header, drawn from
`session.list`.

## Backed by real data, unlike the tool views

Every field on a row comes from `SessionInfo`, which this build already
serves: `title` (which the contract defines as "the session's first user
message, truncated by the engine"), `model`, `created_time`, `state`, and
`session_id`. Nothing here is a mock, which is why this module could be built
now and the fs and exec tool views could not.

## What a row says, and what it deliberately does not

| Shown | Why |
| --- | --- |
| the title | it is what a person recognises a conversation by |
| the model | it is what they choose between when two look alike |
| how long ago | the same |
| the live state, when it is not idle | a session with a turn in flight is not one to open blind |
| the id, as the row's `title` attribute | it is what a reader quotes to somebody else, and never what they scan for |

Not shown: the journal path, the provider, the last seq. A list that shows
everything is a list nobody scans, and each of those is one call away for a
surface that needs it.

Newest first, because the conversation a person wants is nearly always the one
they had last. A session nobody has spoken in says `nothing said yet` rather
than showing a blank - the engine gives no title until a message exists, and a
blank row reads as a bug.

## Read, not watched

There is no push for "a session was created": `session/event` is per-session,
and a list is not a session. So the list is read when it is opened, which is
when it can matter. Polling it on a timer would be a request every few seconds
for an answer that changes twice an hour.

## One thing this found

The page speaks two carriers - the socket for the conversation, the bridge for
calls like this one - and the bridge is its own connection, so it wants its own
`rpc.hello`. Greeting the socket says nothing about the POSTs. The page now
greets the bridge once, lazily, before its first call, and remembers it:
§4.4.1's rule is one handshake per connection, not one per call.

## Tests

`target/probe-primitives.mjs`, **35/35**: newest first, a session with no title
saying so, the open one marked with `aria-current` for a reader who cannot see
the highlight, the id as a title rather than a line to scan, an empty list that
says it is empty, a running session marked on its row, and ages read the way a
person says them.

Verified in Chrome against a live server holding ten real sessions: five
recent empty ones say `nothing said yet`, and the five with messages show them
- `post-rename check`, `renamed to serve`, `auth change check`, `hello from
off the box, second check`, `hello from off the box`. Screenshot at
`data/tetanus-ui-handoff/webui-sessions.png`.
