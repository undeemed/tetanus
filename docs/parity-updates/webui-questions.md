# Parity: the approval and question surface

Upstream: [`client/ui-user-questions`] - the approval and question surface.

tetanus: `web/app/questions.js`, plus the `ui/ask` handling in `chat.js`.

## Two halves, and both are backed by shapes in this tree

- **The live ask.** `ui/ask` is a server-to-client *request* (§4.4.3): the
  engine blocks on it, because a tool cannot proceed until somebody decides.
  `AskParams`, `AskResult`, `Question`, `QuestionOption` and `Answer` are in
  `crates/protocol` on this branch.
- **The durable audit.** `approval/asked`, `approval/decided` and
  `approval/policy` are in §4.3.2's event table on this branch, so a
  conversation opened tomorrow still shows what was asked and how it went.

What is **not** here is an engine that emits either: the gate that asks lives
on the fs lane's branch. So unlike the tool views - where the result shapes are
unwritten and a view would be a guess - this is built against shapes this
repository publishes, and driven in the probes with exactly the frames the
contract fixes.

## The rule that shaped the card

§4.4.3: "A client that advertises the capability and then fails to answer must
answer with an error; the engine treats any error as a denial." Silence is a
denial, so a card that let a reader walk away without deciding would deny by
accident and never say so.

Therefore **every question is answered, always**. `Answer` sends the labels
chosen; `Dismiss` sends the same answers with no labels, which is a decision
the engine can read. Untouched questions in a batch go back with empty label
lists rather than being left out, because §4.6 makes the answer echo the
question's id and a missing id is not an answer.

## Drawing the fail-closed rule, not just obeying it

`ApprovalOutcome::grants` says only `allowed-once` lets a call run. The card
does not collapse the rest into "denied":

| Outcome | Drawn as | Why it is not the same as the others |
| --- | --- | --- |
| `allowed-once` | allowed, once | it granted one call, not a rule |
| `rejected` | rejected | a person said no |
| `cancelled` | withdrawn before it was answered | nobody said no; the asker left |
| `unavailable` | nobody could answer it | the fail-closed path - the fix is a client that can answer |
| anything else | the word itself, toned as a refusal | §4.4.7 has the engine read an unknown outcome as a denial, so a surface that drew it as neutral would disagree with what happened |

## Real controls, so the arity is the browser's problem

A single-select question renders radios and a multi-select renders checkboxes.
The contract states the arity; making it the browser's job means the keyboard,
the screen reader and the reader's own habits all work without this page
implementing any of it.

## Tests

`target/probe-primitives.mjs`, **46/46**: radios for one and checkboxes for
many, the asker's supporting text kept out of the option labels, the chosen
label carried back, every question answered including the untouched one,
dismissal answering with no labels, and the five outcome renderings including a
word nobody defined.

`target/probe-panel.mjs`, **11/11**: a `ui/ask` frame arriving on the page's own
socket is drawn on the transcript, and answering puts an `AskResult` back on
the wire against the request's id - the engine is not left blocked.

Verified in Chrome: two radios, two checkboxes, `Answer` and `Dismiss`.
Screenshot at `data/tetanus-ui-handoff/webui-ask.png`.
