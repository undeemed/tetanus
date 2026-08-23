// What you can type at this page that is not a question.
//
// Upstream ships `ui-commands` and `ui-input-trigger` for this, and
// `tetanus chat` has had a command line since it was written:
// `crates/cli/src/chat.rs` parses `/help`, `/stats`, `/find`, `/keys`,
// `/think`, `/more` and `/exit`. The browser panel had none of them, so a
// reader who typed `/stats` here sent the literal text to a model.
//
// # The list is shorter than the terminal's, and the omissions are answers
//
// A command that acts on a page belongs to the surface that has that page.
// `chat.rs` already makes exactly this argument in `elsewhere()`, refusing
// `/keys`, `/stats` and `/find` in the headless chat because "this chat's page
// is the reader's own scrollback, which it cannot rewrite and their terminal
// already searches". The browser has a page, so it takes the ones that act on
// one - and refuses the ones the browser itself already answers better:
//
// - `/find` is Ctrl-F. Re-implementing find-in-page inside a page that has it
//   would be worse at it and would not open a fold to show a match.
// - `/exit` is closing the tab, and a page cannot close a tab it did not open.
// - `/think` and `/more` are flags on a run, not state this panel holds.
//
// Each of those still *answers*, saying where it went, because the failure to
// avoid is a reader typing a command they know from the terminal and watching
// it go to the model as a question.
//
// # `//` is the escape, and it is not optional
//
// The moment a leading slash means something, a message that starts with one
// needs a way through. `chat.rs` uses `//` and so does this: `//` alone is
// nothing, and `//anything` asks `/anything`.

/** The commands this page answers, in the order `/help` lists them. */
export const COMMANDS = [
  { names: ["/help", "/?"], says: "what you can type here" },
  { names: ["/stats"], says: "what this conversation has cost so far" },
  { names: ["/keys"], says: "the keys and chords this page answers" },
  { names: ["/clear"], says: "clear the transcript on screen; the journal keeps everything" },
];

/** The commands that belong somewhere else, and where. */
const ELSEWHERE = {
  "/find": "/find is your browser's own find - Ctrl-F, or \u2318F - which searches this page already",
  "/exit": "/exit is for the terminal; close the tab, and the conversation is on its journal",
  "/quit": "/quit is for the terminal; close the tab, and the conversation is on its journal",
  "/q": "/q is for the terminal; close the tab, and the conversation is on its journal",
  "/think": "/think is a flag on a run - `tetanus run --think` - not something this panel holds",
  "/more": "/more is for `tetanus chat --ui`; this page already draws every turn in full",
};

/**
 * Read one line the reader typed.
 *
 * Answers `{ kind }`, and every kind is handled by the caller - there is no
 * path where a line is read and then dropped.
 *
 * - `ask` with `said`: a question for the model.
 * - `blank`: nothing to do.
 * - `run` with `name`: a command this page answers.
 * - `elsewhere` with `said`: a command that belongs to another surface.
 * - `unknown` with `name`: a slash word nothing answers.
 */
export function parse(line) {
  const said = String(line ?? "").trim();
  // The escape first, before anything treats the leading slash as a sigil.
  if (said.startsWith("//")) {
    const rest = said.slice(1);
    return rest === "/" ? { kind: "blank" } : { kind: "ask", said: rest };
  }
  if (said === "") return { kind: "blank" };
  if (!said.startsWith("/")) return { kind: "ask", said };

  // The command is the first word. `/stats now` is not a message, and a page
  // that sent it as one would be answering a typo with a model call - the
  // same reasoning `chat.rs` gives for reporting the word rather than the
  // line.
  const word = said.split(/\s+/)[0];
  const known = COMMANDS.find((command) => command.names.includes(word));
  if (known) return { kind: "run", name: known.names[0], said };
  if (ELSEWHERE[word]) return { kind: "elsewhere", said: ELSEWHERE[word] };
  return { kind: "unknown", name: word };
}

/** The help text, built from the same table the parser reads. */
export function help() {
  const lines = COMMANDS.map((command) => `${command.names.join(", ")} - ${command.says}`);
  // The ones that go elsewhere are listed too. A reader looking for `/find`
  // needs to be told where it went, and finding nothing under `/help` is how
  // they conclude the page has no commands at all.
  lines.push("");
  for (const said of Object.values(ELSEWHERE)) {
    if (!lines.includes(said)) lines.push(said);
  }
  lines.push("");
  lines.push("Start a message with // to send a line that begins with a slash.");
  return lines.join("\n");
}

// --- what the conversation has cost ----------------------------------------

/**
 * Fold a journal into what it says about the conversation on it.
 *
 * This is `crates/cli/src/render/timeline.rs`'s `stats` fold, event for event,
 * and it is a second implementation of one rule - which is worth saying out
 * loud rather than leaving to be discovered. There is no `session.stats` call
 * on the boundary, so a page that wanted these figures had two options: fold
 * the journal it already holds, or not have them. The fold is small and every
 * figure is derived from event times and the usage a message reported, which
 * is the property that makes two implementations able to agree at all.
 *
 * If the figures ever disagree with the terminal's, the terminal is right and
 * this is the copy to fix.
 */
export function stats(events) {
  const out = {
    turns: 0, steps: 0, thinking: 0, tooling: 0,
    waited: 0, waits: 0, decoding: 0, decoded: 0,
    promptTokens: 0, completionTokens: 0,
  };
  // What the fold carries between events: the open step, the first token of
  // its message, and the calls waiting on a result.
  const at = { step: null, first: null, calls: new Map() };
  for (const event of events) {
    FOLD[event.type]?.(out, at, Number(event.time) || 0, event.data || {});
  }
  return out;
}

/**
 * One function per event this fold counts, rather than one switch counting
 * all of them.
 *
 * The same shape `crates/exec`'s renderer took for the same reason: each of
 * these is a separate rule about a separate fact, and a reader checking
 * "how is the first-token wait measured" should find one four-line function
 * rather than an arm of something long.
 */
const FOLD = {
  "turn/start": (out) => {
    out.turns += 1;
  },

  "step/start": (out, at, time) => {
    out.steps += 1;
    at.step = time;
    at.first = null;
  },

  // The wait for a first token is measured to the *first* chunk and no later
  // one, which is why this does nothing once one has arrived.
  "assistant/chunk": (out, at, time) => {
    if (at.first !== null) return;
    at.first = time;
    if (at.step === null) return;
    out.waited += Math.max(0, time - at.step);
    out.waits += 1;
  },

  "assistant/message": (out, at, time, data) => {
    if (at.step !== null) {
      out.thinking += Math.max(0, time - at.step);
      at.step = null;
    }
    const usage = data.usage;
    // Decoding starts at the first token, not at the step, so the rate is not
    // diluted by however long the provider took to say anything at all.
    if (at.first !== null && usage) {
      out.decoding += Math.max(0, time - at.first);
      out.decoded += Number(usage.completion_tokens) || 0;
    }
    at.first = null;
    if (!usage) return;
    out.promptTokens += Number(usage.prompt_tokens) || 0;
    out.completionTokens += Number(usage.completion_tokens) || 0;
  },

  "tool/call": (out, at, time, data) => {
    if (data.id !== undefined) at.calls.set(data.id, time);
  },

  // Paired by the id a result names (§4.3.1) and never by arrival order: two
  // calls in flight answer in whichever order the tools finish, so pairing by
  // position attributes one call's time to the other - and leaves a call that
  // was never answered holding a result that was not its.
  "tool/result": (out, at, time, data) => {
    if (!at.calls.has(data.call_id)) return;
    out.tooling += Math.max(0, time - at.calls.get(data.call_id));
    at.calls.delete(data.call_id);
  },
};


/**
 * The strip a reader asks for, worded as the terminal words it.
 *
 * A group with nothing in it is left out whole rather than printed as zeroes -
 * the terminal's rule, and its reason: a conversation whose every request
 * failed has counts and no billing, and `0 tokens` reads as a conversation
 * that was free rather than one that never got an answer.
 */
export function told(figures) {
  if (figures.turns === 0 && figures.steps === 0) return "nothing has been asked yet";
  const groups = [
    counted(figures),
    took(figures),
    fast(figures),
    billed(figures),
  ].filter((group) => group !== null);
  return groups.join("   ");
}

function counted(figures) {
  const counts = [];
  if (figures.turns > 0) counts.push(plural(figures.turns, "turn"));
  if (figures.steps > 0) counts.push(plural(figures.steps, "step"));
  return counts.length > 0 ? counts.join(" \u00b7 ") : null;
}

function took(figures) {
  const said = [];
  if (figures.thinking > 0) said.push(`model ${duration(figures.thinking)}`);
  if (figures.tooling > 0) said.push(`tools ${duration(figures.tooling)}`);
  return said.length > 0 ? said.join(" \u00b7 ") : null;
}

function fast(figures) {
  const said = [];
  if (figures.waits > 0) {
    said.push(`first token ${duration(Math.round(figures.waited / figures.waits))}`);
  }
  // The terminal's floor: too little time to divide by is no rate at all, not
  // a rate of zero and not an enormous one from a millisecond of decoding.
  if (figures.decoding >= 200 && figures.decoded > 0) {
    const rate = Math.floor((figures.decoded * 1000) / figures.decoding);
    if (rate > 0) said.push(`${rate} tok/s`);
  }
  return said.length > 0 ? said.join(" \u00b7 ") : null;
}

function billed(figures) {
  const total = figures.promptTokens + figures.completionTokens;
  if (total === 0) return null;
  return `${figures.promptTokens} in \u00b7 ${figures.completionTokens} out \u00b7 ${total} tokens`;
}

function plural(many, thing) {
  return `${many} ${thing}${many === 1 ? "" : "s"}`;
}

/** A duration, at the precision the terminal prints. */
function duration(ms) {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60000);
  const seconds = Math.round((ms % 60000) / 1000);
  return `${minutes}m ${seconds}s`;
}
