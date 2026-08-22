// What the deployment's own hooks did during a turn.
//
// `crates/hooks` runs a deployment's configured hooks at named points and
// writes two durable records per run: `hook/invoked` before, `hook/result`
// after. They are "paired by `handler_id`", which is the same shape the
// approval audit has, and the same reason applies - the two halves are
// separated on the journal by however long the hook process took, and a lone
// result saying `deny` with nothing above it saying what was denied is not an
// audit.
//
// # Why this is drawn at all, rather than left as raw events
//
// A hook is the one thing on a transcript that is neither the model's doing nor
// the harness's. When a turn stops for no reason the conversation explains, a
// hook is very often why - `decision: deny` at `PreToolUse`, from a handler
// somebody configured months ago. A reader looking for that needs it to say so
// in words: which point, which bridge, which handler, and what it decided.
//
// # The decision vocabulary is the engine's, and every result has one
//
// `recorded_decision` is explicit that a result always carries a decision, so
// "nothing happened" is the word `pass` and not an absent field - which means
// this page never has to infer silence. Six words are known and toned; a word
// added later is drawn as itself, and toned as a refusal for the same reason
// the approval audit does it: a build that has not been taught a new way of
// saying no should not draw one as neutral.

import { disclosure, pill } from "./primitives.js";

/**
 * What each recorded decision means to a reader, and how it reads.
 *
 * `approve` and `allow` are two dialects' words for the same answer and both
 * are kept: the journal records what the hook said, and flattening them here
 * would make a Claude Code hook and a Codex hook indistinguishable in an audit
 * whose whole purpose is saying which one acted.
 */
const DECISIONS = {
  approve: { said: "approved", tone: "ok" },
  allow: { said: "allowed", tone: "ok" },
  block: { said: "blocked", tone: "bad" },
  deny: { said: "denied", tone: "bad" },
  ask: { said: "asked a person", tone: "busy" },
  stop: { said: "asked the turn to stop", tone: "bad" },
  pass: { said: "no opinion", tone: undefined },
};

/** The bridges, spelled the way a person configuring one would recognise. */
const DIALECTS = { "claude-code": "Claude Code", codex: "Codex" };

/**
 * A tracker over the hook records, pairing each result with its invocation.
 *
 * Same shape as the approval audit and for the same reason; kept separate
 * because the vocabularies are different and one function switching on five
 * event types would be the place both get changed by accident.
 */
export function hooks() {
  const open = new Map();
  return {
    // The types this tracker draws, named rather than matched on the `hook/`
    // prefix. A `hook/something-new` is then *not* claimed, and falls through
    // to the raw rendering every unrecognised durable type gets - which is
    // §4.3.2's rule, and the difference between a growing vocabulary showing
    // up on the page and disappearing from it.
    handles: (type) => type === "hook/invoked" || type === "hook/result",
    row: (type, data) =>
      type === "hook/invoked" ? invoked(open, data ?? {}) : settled(open, data ?? {}),
  };
}

function invoked(open, data) {
  const root = disclosure(`hook at ${data.point ?? "an unnamed point"}`, { open: false });
  root.classList.add("hook");
  const running = pill("running", "busy");
  root.head.append(running);

  // The three facts that identify which hook this was, which is what somebody
  // reading an audit needs in order to go and find the thing and change it.
  const facts = [];
  if (data.dialect) facts.push(DIALECTS[data.dialect] ?? data.dialect);
  if (data.handlerId) facts.push(data.handlerId);
  // A match-all hook has no matcher, and that is a fact worth its own words: a
  // hook that fires on everything is a different thing to understand from one
  // that fired because a pattern matched this call.
  facts.push(data.matcher ? `matched ${data.matcher}` : "runs on every call at this point");
  root.body.append(line(facts.join(" · ")));

  if (data.handlerId !== undefined) open.set(key(data), { root, running });
  return root;
}

function settled(open, data) {
  const known = DECISIONS[data.decision];
  const outcome = pill(
    known ? known.said : (data.decision ?? "said nothing this build understands"),
    known ? known.tone : "bad",
  );
  const found = open.get(key(data));
  const said = [];
  // An exit code is only interesting when it is not zero: a hook that ran
  // cleanly and passed has nothing to say with it, and printing `exit 0` on
  // every row would bury the one that says 2.
  if (typeof data.exitCode === "number" && data.exitCode !== 0) {
    said.push(`exit ${data.exitCode}`);
  }
  if (typeof data.durationMs === "number") said.push(took(data.durationMs));

  if (found) {
    open.delete(key(data));
    found.running.replaceWith(outcome);
    if (said.length > 0) found.root.body.append(line(said.join(" · ")));
    // The stderr summary is the hook's own words about why, and it is the one
    // thing here a reader would open the fold for. So a hook that wrote to
    // stderr opens; one that did not stays folded.
    if (data.stderrSummary) {
      found.root.body.append(line(data.stderrSummary, "hook-said"));
      found.root.open = true;
    }
    return null;
  }

  // No invocation on the page - a conversation opened part-way through. The
  // row still draws and still names the handler, because an unexplained
  // decision is exactly what somebody is scrolling back to find.
  const root = document.createElement("div");
  root.className = "ask-decided";
  const which = document.createElement("span");
  which.className = "ask-why";
  which.textContent = `hook ${data.handlerId ?? "?"} at ${data.point ?? "an unnamed point"}`;
  root.append(which, outcome);
  if (data.stderrSummary) root.append(line(data.stderrSummary, "hook-said"));
  return root;
}

/**
 * What pairs an invocation with its result.
 *
 * `handler_id` is what the engine says correlates them, and the point is added
 * because one handler can be configured at more than one point and both can be
 * open at once inside a turn. Pairing on the handler alone would let a `Stop`
 * hook's result settle the `PreToolUse` row of the same handler.
 */
function key(data) {
  return `${data.point ?? ""}\u0000${data.handlerId ?? ""}`;
}

/** How long it took, at the precision a reader cares about. */
function took(ms) {
  return ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(1)}s`;
}

function line(text, className = "hook-fact") {
  const node = document.createElement("p");
  node.className = className;
  node.textContent = text;
  return node;
}
