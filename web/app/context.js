// What the model can still see, and what it can no longer.
//
// Five durable types this page was drawing as raw JSON, and one of them is the
// most consequential fact a long conversation has: `compaction/summary` says
// the older half of the history has been replaced by a summary. A reader who
// does not know that reads an answer that ignores something they said and
// concludes the model is stupid. It is not; it cannot see it.
//
// # `request/context` stops being a row and becomes a meter
//
// It is written before every request - route, model, the context window, and
// what the system prompt and the tool catalogue cost - so on a five-step turn
// it is five near-identical lines of JSON in the middle of the conversation.
// That is `{"context_window":null,"model":"mock-echo-1",...}` in the transcript
// of every screenshot taken of this page.
//
// Upstream spends a component on the same numbers and puts them in a meter
// rather than the thread (`ui-conversation`'s `ContextMeter`), which is the
// right call for a fact that is *current* rather than an event: a reader wants
// to know how full the window is now, not what it was five steps ago. So the
// page keeps the newest one and draws it in the header, and the transcript
// stops carrying any of them.
//
// A deployment whose route declares no window - `context_window: null`, which
// is what the offline mock reports - gets no meter rather than a meter reading
// zero. "Nobody said how big it is" and "it is empty" are different, and only
// one of them is a reason to relax.

import { pill } from "./primitives.js";

/** The types this module draws, named so nothing else is claimed. */
export const CONTEXT_TYPES = [
  "request/context",
  "compaction/start",
  "compaction/end",
  "compaction/summary",
  "compaction/prune",
];

/**
 * A tracker over the context family.
 *
 * `meter` is handed the newest `request/context` and returns the element to put
 * in the header, or `null` when this deployment has said nothing about a
 * window. `row` answers what belongs on the transcript, and `null` for the
 * records that belong nowhere.
 */
export function context(onMeter) {
  let latest = null;
  return {
    handles: (type) => CONTEXT_TYPES.includes(type),
    row: (type, data) => {
      if (type === "request/context") {
        latest = data ?? {};
        onMeter?.(meter(latest));
        // Nothing on the transcript. It is not dropped - it is on the journal,
        // the trace panel folds it, and the header now says what it says.
        return null;
      }
      return record(type, data ?? {});
    },
    // For a reader of this module rather than the page: the last envelope seen.
    latest: () => latest,
  };
}

/**
 * How full the window is, as a short line for the header.
 *
 * The two costs a request pays before the conversation is even counted - the
 * system prompt and the tool catalogue - are what this can honestly report,
 * because they are what the envelope carries. It deliberately does not add an
 * estimate of the conversation on top: that number would be this page's guess
 * next to the engine's measurement, and a meter that is wrong about the part a
 * reader cannot check is worse than a meter that reports less.
 */
export function meter(envelope) {
  const window = whole(envelope?.context_window);
  if (window === null) return null;
  const fixed = (whole(envelope?.system_tokens) ?? 0) + (whole(envelope?.tools_tokens) ?? 0);
  const share = Math.min(100, Math.round((fixed / window) * 100));
  const said = `${fixed} of ${window} before the conversation`;
  // Toned on how much room is left, since that is the decision a reader makes
  // with it: at four fifths spent on the prompt and the tools alone, the next
  // long tool result is what compacts the conversation.
  const tone = share >= 80 ? "bad" : share >= 50 ? "busy" : undefined;
  return pill(said, tone);
}

/** One compaction record, as a row - or `null` for the bookkeeping ones. */
function record(type, data) {
  if (type === "compaction/start") {
    // The open half of a lock. It says nothing a reader acts on and its
    // partner says everything, so it is not drawn - and it is the one type
    // here where being silent is right rather than lazy.
    return null;
  }
  if (type === "compaction/end") return ended(data);
  if (type === "compaction/summary") return summarised(data);
  if (type === "compaction/prune") return pruned(data);
  return null;
}

/**
 * A compaction that ended badly. A clean end says nothing.
 *
 * The error matters because of what it implies: the window is still full and
 * the next request is the one that fails. A compaction that succeeded needs no
 * row, because `compaction/summary` already drew one.
 */
function ended(data) {
  if (typeof data.error !== "string" || data.error === "") return null;
  const root = document.createElement("div");
  root.className = "ctx ctx-bad";
  root.append(line(`the conversation could not be summarised: ${data.error}`));
  root.append(line("the window is still full, so the next request may not fit", "ctx-note"));
  return root;
}

/** The history the model can no longer see, and what stands in for it. */
function summarised(data) {
  const root = document.createElement("div");
  root.className = "ctx";
  const head = document.createElement("div");
  head.className = "ctx-head";
  head.append(pill("summarised", "busy"));
  const many = counted(data);
  // The counts go after the sentence rather than into its subject. As a
  // subject they force an agreement this cannot get right - "6 events, about
  // 3200 tokens *was* replaced" - and the sentence is the same either way,
  // which is what a reader skimming a long transcript is reading.
  head.append(
    line(
      many === null
        ? "the earlier conversation was replaced by a summary"
        : `the earlier conversation was replaced by a summary: ${many}`,
      "ctx-said",
    ),
  );
  root.append(head);
  if (typeof data.model === "string" && data.model !== "") {
    // Which model wrote the summary, because a summary is a reading of the
    // conversation and not a copy of it: who read it is part of what it is.
    const through = data.provider ? ` (${data.provider})` : "";
    root.append(line(`written by ${data.model}${through}`, "ctx-note"));
  }
  if (typeof data.summary === "string" && data.summary !== "") {
    const said = document.createElement("pre");
    said.className = "ctx-summary";
    said.textContent = data.summary;
    root.append(said);
  }
  return root;
}

/** One tool result shortened without a model. */
function pruned(data) {
  const root = document.createElement("div");
  root.className = "ctx";
  const tokens = whole(data.shadowed_token_count);
  root.append(
    line(
      tokens === null
        ? "an over-long tool result was shortened to fit the window"
        : `an over-long tool result was shortened to fit the window, saving about ${tokens} tokens`,
      "ctx-note",
    ),
  );
  return root;
}

/**
 * How much history a record shadows, in the terms the record gives.
 *
 * Both numbers when both are there, because they answer different questions:
 * how much of the conversation is gone, and how much room that bought.
 */
function counted(data) {
  const events = Array.isArray(data.shadowed_seqs) ? data.shadowed_seqs.length : null;
  const tokens = whole(data.shadowed_token_count);
  if (events === null && tokens === null) return null;
  const parts = [];
  if (events !== null) parts.push(`${events} ${events === 1 ? "event" : "events"}`);
  if (tokens !== null) parts.push(`about ${tokens} tokens`);
  return parts.join(", ");
}

function line(text, className = "ctx-said") {
  const node = document.createElement("p");
  node.className = className;
  node.textContent = text;
  return node;
}

/** A non-negative whole number a field carries, or `null`. */
function whole(value) {
  return Number.isInteger(value) && value >= 0 ? value : null;
}
