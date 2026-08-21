# Parity: the resolved settings

Upstream: [`client/ui-settings-general`].

tetanus: `web/app/settings.js`, from `config.dump` - the same call
`tetanus config` prints, so the terminal and the page cannot disagree about
what a key is set to.

## The layer is the point, not the value

A reader opening this is almost never asking what `agent.max_steps` is. They
are asking why it is that and where to change it, which is a question about the
layer: a default is changed in a document, a document in a file, an environment
variable in a shell, a flag on the command line already running. So the layer
sits beside every value, and each is worded as the place a reader would go -
"built in", "settings document", "environment", "command line".

Ordered by key, because a reader looks a key up first and wonders where it came
from second. A layer this build has never heard of is drawn as itself: §7.5
makes the set growable, and a surface that showed nothing for a new one would
hide exactly the layer somebody just added.

## The redaction sentinel is drawn and never interpreted

§4.6: "`ConfigEntry.value` never carries a secret... A surface renders the
sentinel as it renders any other value, and must not take it for the setting."

Both halves. `<redacted>` is drawn as the value it is, and the row says in
words that the value is **withheld, not empty** - because a reader who takes
`<redacted>` for the setting has been told the opposite of what is true, and
that is a mistake somebody has certainly made by pasting it into a document.

## A gap this found in the wire

`ConfigDumpResult` carries `entries` and nothing else, so **a page over the
wire cannot know which settings document the engine read**. `tetanus config`
names it only because the CLI resolves the document itself before calling.

The page therefore says nothing about the document rather than claiming none
was read - a different and false statement. Naming it would be one optional
field on `ConfigDumpResult`; it is not this lane's to add, and it is small
enough that it is recorded here rather than as a contract note of its own
unless somebody wants it.

## Tests

`target/probe-primitives.mjs`, **100/100**: the table ordered by key, every
value carrying its layer, an unknown layer drawn as itself, the sentinel shown
with its "withheld, not empty" note, values written the way a person types them
into a document (a string loses its quotes, `true` and `[]` keep their
spelling), "Nothing is set" for an empty dump, and no claim about a document
the page does not know.

Verified in Chrome against a live server: twelve rows, `agent.max_steps 8 built
in`, `llm.retry.backoff.initial_delay_ms 500 built in`, and the rest.
Screenshot at `target/shots/webui-settings.png`.
