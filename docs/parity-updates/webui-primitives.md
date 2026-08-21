# Parity: the primitives layer

Upstream: [`client/ui-primitives`], 46 components - "the design system
everything else is built from", and first in the gap list's build order for its
own stated reason: everything sits on it, so doing it late means rewriting.

tetanus: `web/app/tokens.css`, `web/app/primitives.css`, `web/app/primitives.js`.

## What is here, and why only this much

The instruction was to build only what real data already exists for. Upstream's
46 atoms serve 31 modules; this page serves one conversation, so the atoms that
have data today are the ones here:

| Upstream atom | tetanus | Data it draws |
| --- | --- | --- |
| `StateDot` | `stateDot(state, label)` | the connection state |
| `Pill` | `pill(text, tone)` | the closing line's facts |
| `Button` | `button(text, {kind, onClick})` | the composer, the chooser |
| `Input` | `input({label, placeholder})` | the chooser's fields |
| `DisclosureRow` | `disclosure(summary, {open, tone})` | reasoning, tool arguments |
| `Modal` | `modal(title)` | the workspace chooser |
| `Toast` | `toast(text, {tone, onDone})` | a fault worth interrupting for |
| `Tooltip` | `tipped(node, text)` | anywhere a glyph needs a word |
| `JsonTree` / `JsonBlock` | `jsonTree(value)`, `jsonBlock(value, summary)` | tool arguments and results |
| `MessageText` | `messageText(text)` | what a person typed |

Deliberately **not** built, because the data lands in other lanes: `TerminalBlock`
(exec, shell lane), `DiffBlock` and `ReadBlock` (fs lane), `SearchBlock` and
`WebBlock` (the web tools on the mcp branch), `OnboardingSurface`,
`RiskConfirmation`, `HoverCard`, `Menu`, `BrandWordmark`. Each of those would
be a mock screen today and a rewrite tomorrow.

`MarkdownText` and `CodeBlock` are the two that have data and are still not
here: upstream's renderer is GFM plus KaTeX plus shiki, and a safe subset is a
slice of its own rather than a corner of this one.

## The three rules the layer keeps

- **Tokens first.** Every value in `primitives.css` comes from `tokens.css`. A
  literal colour in a component is a colour that survives the next palette
  change, which is the whole failure the token layer prevents. The names follow
  upstream's shape - surface, line, text, muted, accent, state - not its
  `--dsw-alias-*` spelling, which would claim a compatibility this page does
  not have.
- **Text is set with `textContent`, never `innerHTML`.** Everything drawn here
  comes from a model, a tool or a filesystem, and none of those is trusted. The
  probe asserts it by making `innerHTML` throw.
- **A control is a real control.** A `button` with an explicit `type`, a
  `details` for a fold, a `dialog` for a modal - so the keyboard reaches them
  and a screen reader says what they are, without this page re-implementing
  behaviour the browser already has. Upstream's own note about the hover card
  exposing button semantics is the same discipline.

Two details taken from upstream's README rather than invented: the toast holds
three seconds and fades over one, with the slide dropped under
`prefers-reduced-motion`; and a state is reported by a word beside the dot,
never by the dot alone.

## What the page now draws through it

The connection state is a dot with its word. The closing line is pills, with
the reason coloured by whether the turn ended the way it meant to - the same
rule the terminal follows, where only `natural` is a turn that finished - and a
cut-off turn carries the sentence §4.4.2 asks for. A tool call's arguments are
a folded JSON tree rather than a stringified line.

## Tests

`target/probe-primitives.mjs`, **20/20**, on a bare `node` with a minimal
document: the state fallback for a state nobody defined, a pill that is not a
button, a button that is, a labelled field, a `details` that starts folded, a
JSON tree that renders `<img src=x onerror=...>` as characters, a depth bound,
a user's message keeping its asterisks, and a toast that announces itself and
schedules its own departure.

Verified in Chrome against a live server: dot reads `connected`, the closing
line reads `turn 1 · natural · 2 steps · 65 tokens` as pills with `natural`
toned as finished, and the tool call folds open to a tree. Screenshot at
`data/tetanus-ui-handoff/webui-primitives.png`.
