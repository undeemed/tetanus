# web/chat - the browser panel

A conversation with the harness, in a browser, over the WebSocket carrier `tetanus serve` already
hosts.

It is a page, not a program: `index.html` and `chat.js`, no build step, no framework and no
dependency, so the thing a reviewer opens is the file in this directory.

## Running it

```sh
python3 web/chat/serve.py            # then open the address it prints
python3 web/chat/serve.py --port 5300 --dir /tmp/tetanus-web-chat
```

`serve.py` starts `tetanus serve --listen 0.0.0.0:0`, reads the address out of its banner, hands it
to the page and serves the two files. It builds the binary first if there is not one already.

To point the page at a server you started yourself, open it with the address in the query:
`index.html?ws=ws://host:port`. A conversation is named in the query too - `?session=<id>` - and the
page puts the id there itself once the session exists, so a reload continues that conversation
rather than starting another.

## What it does

Four calls, the same four `tetanus chat` makes: `rpc.hello`, `session.create`,
`session.subscribe` from seq 0, then one `agent.prompt` for each message typed. Nothing else, and
nothing this panel needed added to the contract.

Subscribing from seq 0 makes history and live delivery one ordered channel, which is why a reply
appears delta by delta as the model streams it and why a reload replays the whole transcript in the
same shape. An event type this page does not know is drawn from its raw JSON rather than dropped -
the durable vocabulary grows, and the newest half of a turn is the half a reader most wants to see
([docs/interface-contract.md](../../docs/interface-contract.md) section 4.3.1).

The transcript is deliberately the terminal's transcript: the same `you`, `ai`, `▸ call`, `✓ result`
rows, the same closing line, the same palette as
[tools/uiwatch](../../tools/uiwatch/README.md). A turn that read differently here would be a second
description of the same events.

## What it does not do

- **No authentication, and no encryption.** `serve.py` is a development server, and the carrier
  behind it answers anyone who can reach the port. Run it where you would run `cargo run`.
- **Mock replies only, today.** `tetanus serve` builds its engine with the default provider set,
  which is the offline mock. A real model through this panel waits on the engine lane.
- **One session per page.** No session list, no picker, no interrupt button. `tetanus sessions` and
  `agent.interrupt` exist; this page does not use them yet.

## Ownership

This directory belongs to the chat lane. It consumes
[docs/interface-contract.md](../../docs/interface-contract.md) and defines nothing of its own; a
change to the boundary belongs in that document and in `crates/protocol`, never here.
