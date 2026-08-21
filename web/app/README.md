# web/app - the browser panel

A conversation with the harness, in a browser, over the WebSocket carrier `tetanus serve` already
hosts.

It is a page, not a program: `index.html` and `chat.js`, no build step, no framework and no
dependency, so the thing a reviewer opens is the file in this directory.

## Running it

```sh
tetanus serve --listen 127.0.0.1:5300 --frontend web/app   # open the address it prints
tetanus serve --listen 0.0.0.0:5300 --frontend web/app --open-to-anyone
```

`tetanus serve --frontend` binds the page's address, takes a port from the operating system for the carrier, and
tells the page which one through the host's index tap - the boot manifest, `window.TETANUS_BOOT`.
The page is served by `crates/host`, which is the same route carrier every other host route will
ride on; nothing patches the HTML on its way past.

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

A dropped connection loses nothing a reader typed. The question goes back into the box it was typed
in - a prompt that never reached the engine is a question they still have - and the line at the
foot counts down to the next dial rather than saying only that something failed: the wait doubles
from a second to a cap of fifteen, and starts from a second again once a server answers. What
failed is on the transcript, where the rest of the conversation is.

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

This directory belongs to the chat lane.
One change was made from the presentation lane, deliberately and on record: the two defects above -
a question lost when the socket dropped, and a reconnect that said nothing about when it would try
again - were found while sweeping the surfaces that had never been measured, and were fixed on
instruction rather than left waiting for a lane that is not currently running. Nothing else in this
directory was touched. It consumes
[docs/interface-contract.md](../../docs/interface-contract.md) and defines nothing of its own; a
change to the boundary belongs in that document and in `crates/protocol`, never here.

## Choosing a workspace directory

The **workspace…** button opens the chooser: two panes, the level and its
parent, drawn entirely out of `host.listDirectory` and `host.createDirectory`.

Everything the host said is drawn and nothing is inferred. `hidden` is a flag
on the row rather than a name this page re-derives, so the footer's toggle acts
on the host's answer; `truncated` is said out loud rather than quietly shown as
a short level; and the three failures the picker can return are printed as the
host worded them, because each one says what to do next.

Two panes rather than one because stepping back should not make the view
collapse - a chooser that shows a single level loses the reader's place every
time they go up. The parent leg is best-effort: a level whose parent cannot be
read is still worth showing, and the pane is simply not there.
