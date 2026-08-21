// The run's path: steps, retries, timings.
//
// Upstream's `ui-trajectory` answers "what actually happened in this run" -
// which is a different question from "what was said", and the reason it is a
// module of its own. The thread reads like a conversation; this reads like a
// trace.
//
// # What is real here
//
// Everything in the fold below comes off the journal this build already
// writes: `turn/start`, `step/start`, `assistant/chunk`, `assistant/message`
// with its `usage`, `tool/call`, `tool/result`, `step/end`, `turn/end`, and
// the time on every event. The timings are arithmetic over those times - the
// same arithmetic the terminal's closing line and `/stats` do, deliberately,
// because two surfaces disagreeing about how long a turn took would be worse
// than neither showing it.
//
// # What is registered and left empty
//
// `llm/retry`, `llm/retry-started` and `context/snapshot` are durable types in
// §4.3.2 that nothing in this tree emits yet - the retry policy and the
// context providers are other lanes' work. Their rows are written and wired by
// event type, so the day an engine writes one it appears here, and until then
// the trace simply has none. That is the instruction taken literally: build
// the frame, register the view, and do not fake the data.

/**
 * Fold a journal into the path a run took.
 *
 * One entry per turn, each with its steps. A step holds the two durations that
 * mean different things - the wait for a first token, and the time spent
 * decoding after it - because a slow provider and a long answer look identical
 * in a single number.
 */
export function trajectory(events) {
  const turns = [];
  let turn = null;
  let step = null;
  const calls = new Map();

  for (const event of events) {
    const at = event.time ?? 0;
    const data = event.data || {};
    switch (event.type) {
      case "turn/start":
        turn = { turn: data.turn, at, steps: [], notes: [], ended: null, reason: null };
        turns.push(turn);
        break;
      case "step/start":
        turn ||= { turn: data.turn, at, steps: [], notes: [], ended: null, reason: null };
        if (!turns.includes(turn)) turns.push(turn);
        step = { step: data.step, at, first: null, settled: null, tokens: 0, tools: [], retries: [] };
        turn.steps.push(step);
        break;
      case "assistant/chunk":
        if (step && step.first === null) step.first = at;
        break;
      case "assistant/message":
        if (step) {
          step.settled = at;
          step.tokens = data.usage?.completion_tokens ?? 0;
          step.prompt = data.usage?.prompt_tokens ?? 0;
        }
        break;
      case "tool/call":
        calls.set(data.id, { name: data.name, at });
        break;
      case "tool/result": {
        const called = calls.get(data.call_id);
        calls.delete(data.call_id);
        // Paired by id and never by arrival order (§4.3.1): two calls in
        // flight finish in whichever order the tools finish.
        if (step) {
          step.tools.push({
            name: data.name,
            ok: data.ok !== false,
            took: called ? at - called.at : null,
          });
        }
        break;
      }
      // Registered, and empty until an engine writes one. `llm/retry` is
      // written before the wait and `llm/retry-started` when it is over, so a
      // trace can show the gap between them as time nobody was working.
      case "llm/retry":
        if (step) step.retries.push({ retry: data.retry, code: data.code, delay: data.delay_ms });
        break;
      case "llm/retry-started":
        if (step) {
          const last = step.retries[step.retries.length - 1];
          if (last) last.resumed = at;
        }
        break;
      case "context/snapshot":
        if (turn) turn.notes.push({ kind: "context", parts: (data.parts || []).map((p) => p.name) });
        break;
      case "step/end":
        step = null;
        break;
      case "turn/end":
        if (turn) {
          turn.ended = at;
          turn.reason = data.stop_reason;
        }
        turn = null;
        break;
      default:
        break;
    }
  }
  return turns;
}

/** Draw the path into `root`. */
export function trace(root, turns) {
  root.replaceChildren();
  if (turns.length === 0) {
    const none = document.createElement("p");
    none.className = "list-empty";
    none.textContent = "Nothing has run yet.";
    root.append(none);
    return;
  }
  for (const turn of turns) {
    root.append(turnRow(turn));
  }
}

function turnRow(turn) {
  const root = document.createElement("div");
  root.className = "trace-turn";

  const head = document.createElement("div");
  head.className = "trace-head";
  head.append(cell(`turn ${turn.turn ?? "?"}`, "trace-name"));
  if (turn.ended !== null) head.append(cell(took(turn.ended - turn.at), "trace-took"));
  if (turn.reason) {
    // The same rule the terminal's closing line follows: only `natural` is a
    // turn that finished, so every other reason is drawn as a turn that did
    // not.
    head.append(cell(turn.reason, turn.reason === "natural" ? "trace-ok" : "trace-bad"));
  }
  root.append(head);

  for (const step of turn.steps) root.append(stepRow(step));
  for (const note of turn.notes) {
    root.append(cell(`context: ${note.parts.join(", ") || "none"}`, "trace-note"));
  }
  return root;
}

function stepRow(step) {
  const root = document.createElement("div");
  root.className = "trace-step";
  root.append(cell(`step ${step.step ?? "?"}`, "trace-name"));

  // The two halves of a model call, kept apart: a slow provider and a long
  // answer are the same number of seconds and different problems.
  if (step.first !== null) root.append(cell(`first token ${took(step.first - step.at)}`, "trace-took"));
  if (step.first !== null && step.settled !== null) {
    root.append(cell(`decoding ${took(step.settled - step.first)}`, "trace-took"));
  }
  if (step.tokens) root.append(cell(`${step.tokens} out`, "trace-took"));

  for (const tool of step.tools) {
    const said = tool.took === null ? tool.name : `${tool.name} ${took(tool.took)}`;
    root.append(cell(said, tool.ok ? "trace-tool" : "trace-bad"));
  }
  for (const retry of step.retries) {
    // The wait is time nobody was working, which is the fact a trace exists to
    // show; the code is why.
    const waited = retry.delay ? ` after ${took(retry.delay)}` : "";
    root.append(cell(`retry ${retry.retry ?? "?"} (${retry.code ?? "unknown"})${waited}`, "trace-bad"));
  }
  return root;
}

function cell(text, className) {
  const node = document.createElement("span");
  node.className = className;
  node.textContent = text;
  return node;
}

/**
 * A duration a person can read.
 *
 * Milliseconds under a second, because that is where a trace's interesting
 * differences live; seconds above it, because nobody reads `4238ms` as "four
 * seconds" without doing the division themselves.
 */
export function took(ms) {
  if (ms === null || ms === undefined) return "";
  if (ms < 1000) return `${Math.max(0, Math.round(ms))}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}
