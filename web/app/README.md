# web/app - the browser panel

A conversation with the harness, in a browser, over the WebSocket carrier `tetanus serve` already
hosts.

It is a page, not a program: `index.html` and `chat.js`, no build step, no framework and no
dependency, so the thing a reviewer opens is the file in this directory.

Three pairs of names are kept apart on purpose, because a stylesheet has one namespace and no
compiler: `.row` is a line of the transcript and `.choice` is a control you pick; `.said` is a
transcript row's text and `.message` is what `messageText` draws; `.dot` is the state indicator and
`.quiet` is a hidden entry in the chooser. Each pair shared a name once, and each time the rule that
lost was silent and order-dependent.

Three probe cases hold that line now: no class is *defined bare* by both `primitives.css` and the
page's own `<style>`; no element wears a page class and a primitives class at once; and every id a
script reaches for is an id the page has. A page rule that scopes a primitive on purpose -
`.row > .disclose` - is allowed, because that is the cascade being used rather than a second
definition.

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
- **One session per page.** A conversation is named in the query and the page puts it there itself,
  but two conversations cannot be open at once.

## Ownership

This directory belongs to the chat lane.
One change was made from the presentation lane, deliberately and on record: the two defects above -
a question lost when the socket dropped, and a reconnect that said nothing about when it would try
again - were found while sweeping the surfaces that had never been measured, and were fixed on
instruction rather than left waiting for a lane that is not currently running. Nothing else in this
directory was touched. It consumes
[docs/interface-contract.md](../../docs/interface-contract.md) and defines nothing of its own; a
change to the boundary belongs in that document and in `crates/protocol`, never here.

## Being asked, and the audit of what was decided

[questions.js](questions.js) draws the live `ui/ask` card and the durable audit. Every question is
answered, always: §4.4.3 makes a client that does not answer a denial, so **Dismiss** answers with
no labels rather than offering a way out that says nothing.

[tool-hooks.js](tool-hooks.js) is the same shape for `hook/invoked` and `hook/result`, which pair by
`handler_id`. A hook is the one thing on a transcript that is neither the model's doing nor the
harness's, and when a turn stops for no reason the conversation explains, a hook is very often why -
so the row says which point, which bridge, which handler and what it decided. The pairing key is the
handler **and** the point, because one handler configured at two points can have both open inside a
turn. A clean exit is not printed; a non-zero one is, because printing `exit 0` on every row buries
the one that says 2.

Both trackers name the types they draw rather than matching a prefix, so a type added to either
family later is not claimed and silently dropped - it falls through to the raw rendering §4.3.2 asks
for.

There are **two** durable pairs and the audit draws both: `approval/asked`/`approval/decided` for
whether a tool may run, and `question/asked`/`question/answered` for what a person said when the
harness asked them something. A question record carries a *batch*, each question with an id its
answer echoes, so the answers are written onto the questions they belong to rather than listed under
them. `answered: false` says **no answer reached the tool** - `crates/turn` uses it for nobody
listening, a partial answer, an answer outside the options, a panicking answerer and an interrupt,
and that sentence is the one thing true of all five. An answer with no labels is a different thing
again and says *nothing chosen*: that is what Dismiss sends, and §4.4.3 reads it as a refusal.

`approval/asked` and `approval/decided` are "one pair per question, sharing an `id`", and the audit
is a **tracker** rather than a function per event for that reason: the two halves are separated on
the journal by everything that happened while somebody was deciding, and drawn independently the
second is a bare `rejected` with nothing saying what was rejected. An ask draws open, says it is
waiting, and folds when its decision completes it. A decision whose ask is off the top of the page
still draws, naming the id.

## What the run is working toward

[features.js](features.js) draws the standing state - the goal and its phase, plan mode, the plan
the model put up, the task list, what the run has reported, and what is attached - in the trace
panel, above the path.

It is **folded from `session/event`, not fetched**, and that is the sanctioned route rather than a
shortcut: `tetanus_features::view::SessionView` is the same state as a Rust type, and
[docs/contract-updates/features-ui-surfaces.md](../../docs/contract-updates/features-ui-surfaces.md)
§3 says `session.view` and `workspace.view` are deferred, with "a client already receives
`session/event` and can re-fold" as the answer in the meantime. Every one of these types is a
whole-value snapshot, so the fold is "the last one wins" - except where it is not, and attachments
and reports accumulate, which the panel table marks with `many`.

[docs/contract-updates/ui-features-panels.md](../../docs/contract-updates/ui-features-panels.md) is
this lane's reply to that note: what the panels needed, what the re-fold costs, and the one call
(`workspace.view`) that has no page-side substitute.

`todo.status` and `goal.phase` are strings on the wire so that a value added later renders as
itself, and they do: an unknown status keeps its word and gets a neutral mark. The empty state
tells "this run has no goal" from "this build has no goals" by reading `catalog.tools`, which the
page already asks for - not by guessing.

## How a tool call is drawn

[tools.js](tools.js) is the shared frame - a fold with the tool's name on it, its arguments, its
result, and whether it worked - and `views` is a table keyed by tool name that a per-tool view drops
into. A tool with no entry gets the frame, which is the ordinary case and not a placeholder: MCP
servers advertise their own tools, so the set is open by construction.

The fold also carries a **summary** from the view, because a transcript of a dozen calls all
labelled `read` says nothing about which file. A tool without a view shows its name alone - a
summary this page invented from arguments it does not understand would be a guess printed as a fact.

A tool bridged from an MCP server is called `mcp__<server>__<tool>`, and the fold spells that
`<server> · <tool>` with the real name kept on the row's `title`. `crates/mcp` hashes a *server*
whose own name contains the separator, precisely so that join has exactly one reading.

[tool-web.js](tool-web.js) is `web_fetch` and `web_search`. Two facts are lifted out of the prose
because a reader would otherwise skim them: **where a fetch ended** - the final URL after redirects,
so a fetch that landed on a login wall says so on line one, and its `200` is deliberately not toned
as good - and **which sources a search had**, folded from four prose lines into one row each with
the host beside the title, since the host is what decides whether to believe a claim. Every URL goes
through `markdown.js`'s `link`, which keeps the text and withholds the link for anything that is not
http or https.

[tool-features.js](tool-features.js) is the feature family - `todo_write`, `update_goal`,
`get_goal`, `exit_plan_mode`, `report_feedback`, `skill`, `tools`. It **imports the panel's own
renderers** rather than writing second ones: `todo_write`'s arguments *are* `todo/write`'s payload,
and `exit_plan_mode`'s argument is `plan/presented`'s, so two readings of those shapes would be two
places deciding what a task list looks like. A result these tools serialised is drawn only if it
parses to the shape expected; their `"{}"` fallback prints as `{}` rather than as an emptied list,
which is the opposite of what happened.

[tool-shell.js](tool-shell.js) is the shell **and terminal** families from `crates/exec` - eleven
tools over one marker table, because that crate says of the terminal family that "they are the same
markers upstream renders, so a presentation that parses one parses both". Splitting them would
split the table in two and let a marker be taught to one reader and not the other.

What a reader wants from a terminal is not "what did it print" but "is it waiting for me", so
`[wait: …]` is drawn as a phrase - *waiting for input*, *still running*, *the shell exited* - rather
than as the field value. A `terminal_read` page is never stamped `exit 0`: it is a window onto
scrollback and did not exit anything.

One finding is recorded rather than fixed here:
[docs/contract-updates/ui-terminal-send-secrets.md](../../docs/contract-updates/ui-terminal-send-secrets.md)
- a `terminal_send` answering a password prompt is on the journal in plain text, every surface draws
it, and the only reliable signal for masking it is on the engine side.

The markers that crate appends - `[exit code: N]`, `[timed out after Nms]`, `[killed by signal: X]`, the sandbox denial,
the swept process group - are, in its own words, "a wire format in all but name", and
`shell::parse_exit` is the engine-side parser. This is the page-side one: a status becomes a pill,
a policy denial and a sweep become notes, and `[stderr]` splits the two streams. Three of those
change what the output *means* and are invisible at the bottom of forty lines of build log.

These views draw a **failure** as well as a success, which the file views deliberately do not: a
non-zero exit is exactly when the code and the stderr matter, whereas a failed `read` is
`FS_NOT_FOUND: …` with no shape to read. A bracketed line this page does not recognise stays in the
body where it came from.

[tool-files.js](tool-files.js) is the filesystem family from `crates/fs`: `read` draws its window
with a real gutter, `list` marks the directories the tool marked, `glob` keeps the line that says
the search stopped, and `write` and `edit` put the text that is the change in the body instead of
inside a JSON tree. Those tools answer **rendered prose**, not structures, so each of these views is
a *reader* of a format the engine owns and can change - which is why a row that does not parse is
printed exactly as it arrived rather than dropped or guessed at, and why a failed result is never
handed to a view at all.

## What the model can still see

[context.js](context.js) draws the five records in the context family, and the one that matters is
`compaction/summary`: it says the older half of the history has been replaced by a summary. A reader
who does not know that reads an answer that ignores something they said and concludes the model is
stupid - it is not, it cannot see it. So the row says how many events and roughly how many tokens
are now shadowed, which model wrote the summary, and shows the summary in full.

`compaction/end` draws only when it carries an error, because a clean end has nothing to add to the
summary above it and a failed one changes what happens next: the window is still full. A
`compaction/prune` is quieter - an over-long tool result was shortened - and `compaction/start` says
nothing at all, since its partner says everything.

`request/context` leaves the transcript entirely. It is written before every request, so a five-step
turn printed five near-identical lines of JSON into the middle of the conversation. It is a fact
that is *current* rather than an event, so it goes in the header as a meter, the way upstream's
`ContextMeter` does. A route that declares no window gets **no meter**, not a meter reading zero:
"nobody said how big it is" and "it is empty" are different, and only one is a reason to relax. The
meter reports what the envelope actually carries - the system prompt and the tool catalogue - and
does not add this page's own estimate of the conversation on top, because that would be a guess
standing next to a measurement.

## What you can type that is not a question

[commands.js](commands.js) is the command line, matching `tetanus chat`'s. `/help`, `/stats`,
`/keys` and `/clear` run here; `/find`, `/exit`, `/think` and `/more` **answer** by saying where they
went, because the failure to avoid is a reader typing a command they know from the terminal and
watching it go to the model as a question. `/find` points at Ctrl-F: re-implementing find-in-page
inside a page that has it would be worse at it.

`//` is the escape, and it is not optional - the moment a leading slash means something, a message
that starts with one needs a way through. The command is the first word, so `/stats now` is that
command and not a message.

`/stats` folds the journal the page already holds, by `crates/cli/src/render/timeline.rs`'s rules,
event for event. That is a **second implementation of one fold**, said out loud rather than left to
be discovered: there is no `session.stats` on the boundary, so the choice was to fold or to not have
the figures. If the two ever disagree, the terminal is right and this is the copy to fix.

## At the width of a phone

The transcript wraps and the chooser does not stay in two columns. Below 560px the level being
chosen takes the whole dialog and the crumb trail above it is what steps back - the parent pane
exists so that stepping back does not lose the reader's place, and at 190px a column it loses it a
different way.

## Stopping a turn

**Stop** takes Send's seat while a turn runs - beside it, it would move Send under the cursor the
moment a turn started, and the button a reader hits would not be the one they aimed at. It calls
`agent.interrupt`, which the WebSocket carrier reads while the `agent.prompt` it stops is still
outstanding.

It says *asked*, never *stopped*: §4.4.2 ends the turn at the next step boundary and does not abort
an in-flight provider call, so a page claiming the turn had stopped would claim something the
contract does not promise. A second press is not sent - "a turn asked twice is a turn asked once" -
and an interrupt that failed puts the control back rather than giving up on the reader's behalf.

The two closing reasons that sound alike are worded apart. `cancelled` is this button. `interrupted`
is §4.4.4's crash-repair closer, written when `session.create` finds a turn whose process died -
and a reader who read one as the other would either think they stopped a turn they did not, or go
looking for whoever stopped one that nobody did.

## The keyboard

Every panel opens with Alt and a letter - `Alt+S` sessions, `Alt+T` trace, `Alt+M` models, `Alt+W`
workspace - and closing one, however it closed, puts the caret back in the composer.

The chord, the panel, the button it goes through and the words the footer prints are one row of
`CHORDS` in [keys.js](keys.js); the footer line and each button's `aria-keyshortcuts` are written
from that row rather than typed into the HTML, so a hint cannot name a key nothing listens for.
A chord is matched on the physical key *or* the character it typed, because Alt is a dead key on
macOS - `Alt+S` there types `ß` - and matching only the position would send a Dvorak reader to the
wrong key.

What the browser already does for a `<dialog>` opened with `showModal` is left to the browser:
focus is trapped inside it, Escape closes it, and the rest of the page is inert.

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
