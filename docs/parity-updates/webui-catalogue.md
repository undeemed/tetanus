# Parity: the model and tool catalogue

Upstream: [`client/ui-model-selection`] (choose one) and
[`client/ui-settings-models`] (configure them).

tetanus: `web/app/catalogue.js`, from `catalog.models` and `catalog.tools` -
two calls this build already serves.

## The contract wrote the design for this one

§4.6: "`ProviderDescriptor.available` is false when a provider is registered
but its credential is absent, **so a picker can grey the entry instead of
failing at the first turn**."

That is the whole file. A picker offering an unavailable provider turns a
missing environment variable into a failed conversation, and the reader meets
it one turn later, somewhere else, worded as a provider error.

So an unavailable provider is:

- **shown**, because it is a fact about the deployment and hiding it makes a
  reader wonder where their provider went;
- **greyed**, so its models cannot be started;
- **named with its fix** - `credential_env` is on the descriptor for exactly
  this, and "unavailable, set `DEEPSEEK_API_KEY`" is a different message from
  "unavailable".

A provider that lists no models says so without implying it serves none: the
contract calls the list "advisory" and says an unlisted id still passes
through.

## What choosing does

It starts a **new** conversation on that model, because that is what this
contract offers: `session.create` takes a provider and a model, and no call
moves a running session onto another one. Upstream has `session.selectModel`;
this contract does not, so the control says "Start here" rather than
pretending a switch that would quietly do something else.

## The tools list

Names and the engine's own descriptions. Not the JSON schemas: a reader
opening this asks "what can this agent do", and a schema per tool answers a
different question at ten times the length.

## One thing tidied on the way past

The page had three `innerHTML = ""` clears left from before the primitives
layer. They were on lines this build writes, so they were never a way in - but
the rule is "no `innerHTML`", not "none where it matters", because the next
person to touch one of those lines cannot tell which kind it is. All three are
`replaceChildren()` now, and a case asserts that no file in `web/app` uses
`innerHTML` at all.

## Tests

`target/probe-primitives.mjs`, **92/92**: an unavailable provider listed,
greyed and naming its variable; the running model marked; a provider with no
models saying an id still passes through; the tools list with the engine's
descriptions; the two empty states; and the `innerHTML` sweep across all eight
files.

Verified in Chrome: `mock · ready · mock-echo-1 · this conversation · Start
here`, and `echo — Return the given text unchanged.` Screenshot at
`target/shots/webui-catalogue.png`.
