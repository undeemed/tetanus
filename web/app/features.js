// What the run is working toward: the goal, the plan, the task list, the
// attachments, and what it has reported.
//
// Upstream ships `ui-goal`, `ui-plan`, `ui-skill`, `ui-attachment` and
// `ui-message-feedback` as separate modules. They answer one question between
// them - "what is this agent trying to do, and under what plan" - and they
// share a shape: a small amount of standing state that changes rarely, sits
// beside the conversation rather than inside it, and is read from the journal
// so a session opened tomorrow still shows it.
//
// # The views this file promised are now written
//
// It was a frame with every view registered and deliberately empty, because
// `crates/features` was on a branch nobody had landed and drawing
// `event.data.goal.objective` against an unlanded struct would have been
// drawing against a shape still free to change. That crate is on master now,
// so each `view` is filled in and each one reads a field the engine writes.
//
// # These are folded here, not fetched, and that is the sanctioned path
//
// `tetanus_features::view::SessionView` is the same state as a struct, and
// `docs/contract-updates/features-ui-surfaces.md` §3 says plainly that it is
// not on the wire: `session.view` and `workspace.view` are deferred, and the
// note's own answer for a surface in the meantime is that "a client already
// receives `session/event` and can re-fold".
//
// That is what this does. The fold is not a second opinion about the state -
// every one of these types is a whole-value snapshot, so folding is "the last
// one wins" and nothing else. Where it is not - attachments accumulate - the
// panel says so with `many`. `docs/contract-updates/ui-features-panels.md`
// records what this lane would still rather have on the wire, and why.
//
// # Two fields are strings on purpose, so they are drawn as strings
//
// `todo.status` and `goal.phase` are strings rather than enums precisely so a
// value added later renders as itself instead of failing to parse the view
// (the note's §4). So every switch here has a default that shows the word the
// engine sent, and a phase this build has never heard of gets a neutral tone
// rather than being hidden or called an error.

import { markdown } from "./markdown.js";
import { pill } from "./primitives.js";

/**
 * The standing-state panels, by the event type each one folds.
 *
 * `many` says the type accumulates rather than replaces. It is the one thing a
 * fold here can get wrong in a way a reader would not notice: a goal edited
 * three times is one goal, but three attachments are three attachments, and
 * last-one-wins would silently show a session one file when it holds three.
 */
export const PANELS = {
  "goal/changed": {
    title: "Goal",
    reads: "the objective, its phase, and the blocker while it is blocked",
    view: goalView,
  },
  "plan/mode": {
    title: "Plan mode",
    reads: "whether the agent is planning rather than acting",
    view: planModeView,
  },
  "plan/presented": {
    title: "Plan",
    reads: "the plan the agent put up for review",
    view: planView,
  },
  "todo/write": {
    title: "Tasks",
    reads: "the whole task list, and how many are in each state",
    view: todosView,
  },
  "feedback/recorded": {
    title: "Reported",
    reads: "what the run has told its operator, and who said it",
    view: feedbackView,
    many: true,
  },
  "attachment/added": {
    title: "Attached",
    reads: "each attachment's name, media type and size",
    view: attachmentView,
    many: true,
  },
};

/**
 * Fold a journal into the standing state the panels draw.
 *
 * One pass, and one entry per panel that has anything to say, in the order the
 * panels are declared rather than the order the events arrived - a panel that
 * moved up the page because the model happened to edit its goal last is a page
 * that will not hold still while somebody reads it.
 */
export function standing(events) {
  const held = new Map();
  for (const event of events) {
    const panel = PANELS[event.type];
    if (!panel) continue;
    const seen = held.get(event.type) ?? [];
    // Last one wins unless the type accumulates. Both of those are the
    // engine's rule and not this page's reading of it: `todo/write` is
    // documented as "one whole-list snapshot", and an attachment view is
    // "every attachment the session admitted, oldest first".
    held.set(event.type, panel.many ? [...seen, event] : [event]);
  }
  return Object.entries(PANELS)
    .filter(([type]) => held.has(type))
    .map(([type, panel]) => ({ type, title: panel.title, events: held.get(type) }));
}

/**
 * The tools whose presence means this deployment can produce these panels.
 *
 * The names live here, beside the panels they explain, and they are read from
 * `catalog.tools` - the call the page already makes. This is the difference
 * between the two empty states, and it is not a guess: a build that offers
 * `update_goal` can have a goal, and one that does not never will.
 */
export const FEATURE_TOOLS = [
  "get_goal",
  "update_goal",
  "todo_write",
  "exit_plan_mode",
  "skill",
  "report_feedback",
  "workspace_info",
];

/**
 * Draw the standing state, or say plainly that there is none.
 *
 * The empty state names why rather than showing a blank: a reader looking at a
 * panel with nothing in it should be able to tell "this agent has no goal"
 * from "this build cannot show goals yet", because those call for different
 * things - one is a prompt away, the other is a lane landing. `offers` is what
 * `catalog.tools` advertised, which is how those two are told apart without
 * either of them being assumed.
 */
export function panel(root, events, { offers = null } = {}) {
  root.replaceChildren();
  const entries = standing(events);
  if (entries.length === 0) {
    const none = document.createElement("p");
    none.className = "list-empty";
    const able = Array.isArray(offers)
      ? offers.some((name) => FEATURE_TOOLS.includes(name))
      : true;
    none.textContent = able
      ? "This run has no goal or plan yet."
      : "Goals, plans and task lists are not part of this build yet.";
    root.append(none);
    return;
  }
  for (const entry of entries) {
    const box = document.createElement("div");
    box.className = "trace-turn";
    const head = document.createElement("div");
    head.className = "trace-head";
    head.textContent = entry.title;
    box.append(head);
    for (const event of entry.events) box.append(drawn(entry.type, event));
    root.append(box);
  }
}

/**
 * One event, through its panel's view - or raw when the view could not read it.
 *
 * The raw path is not dead code and it is not a fallback to be embarrassed
 * about. §4.3.2 asks a surface to draw a durable type it has not taken rather
 * than swallow it, and the same applies within a type: a payload shaped in a
 * way this build has never seen is still a fact about the session, and showing
 * the JSON is strictly better than showing nothing.
 */
function drawn(type, event) {
  const view = PANELS[type]?.view;
  let node = null;
  try {
    node = view?.(event.data ?? {});
  } catch {
    node = null;
  }
  if (node) return node;
  const said = document.createElement("pre");
  said.className = "tool-text";
  said.textContent = JSON.stringify(event.data ?? {}, null, 1);
  return said;
}

// --- the views --------------------------------------------------------------

/** How a phase reads as a state, with an unknown one drawn as itself. */
const PHASES = {
  active: "busy",
  paused: "idle",
  blocked: "bad",
  complete: "ok",
};

/**
 * The goal, or the record of one being cleared.
 *
 * A clear is a fact worth drawing rather than an absence to fall silent about:
 * the journal keeps the tombstone precisely so "no goal yet" and "the goal was
 * put down" can be told apart, and only one of the two means somebody decided
 * something.
 */
function goalView(data) {
  if (data.operation === "clear" && data.cleared) {
    const root = block();
    root.append(line(`cleared - it was "${data.cleared.objective}"`, "feat-quiet"));
    return root;
  }
  const goal = data.goal;
  if (!goal || typeof goal.objective !== "string") return null;
  const root = block();
  const head = document.createElement("div");
  head.className = "feat-head";
  head.append(line(goal.objective, "feat-said"));
  if (typeof goal.phase === "string") head.append(pill(goal.phase, PHASES[goal.phase]));
  root.append(head);
  if (goal.blocker) {
    // The code is the thing a policy or a surface routes on and the message is
    // for the person; both are shown, because a code with no message is a
    // reader looking it up and a message with no code is a reader who cannot.
    root.append(line(`${goal.blocker.code}: ${goal.blocker.message}`, "feat-bad"));
  }
  if (typeof goal.revision === "number") {
    root.append(line(`revision ${goal.revision}`, "feat-quiet"));
  }
  return root;
}

/** Whether the agent is planning rather than acting. A bool, as a sentence. */
function planModeView(data) {
  if (typeof data.active !== "boolean") return null;
  const root = block();
  root.append(
    line(
      data.active
        ? "planning - the agent is working out what to do rather than doing it"
        : "acting - plan mode is off",
      data.active ? "feat-said" : "feat-quiet",
    ),
  );
  return root;
}

/**
 * The plan the model put up, rendered.
 *
 * Rendered here and nowhere else, which is what the contract note means by "a
 * view never carries presentation": `plan.presented` is the model's markdown
 * exactly as written, because the engine does not know the width or the theme
 * and this page does.
 */
function planView(data) {
  if (typeof data.plan !== "string" || data.plan.trim() === "") return null;
  const root = block();
  root.append(markdown(data.plan));
  return root;
}

/** The task list, in the model's own order, with the counts over it. */
function todosView(data) {
  if (!Array.isArray(data.todos)) return null;
  const root = block();
  const counts = { pending: 0, in_progress: 0, completed: 0 };
  for (const item of data.todos) {
    if (typeof item?.status === "string" && item.status in counts) counts[item.status] += 1;
  }
  root.append(
    line(
      data.todos.length === 0
        ? "the list was emptied"
        : `${counts.completed} done · ${counts.in_progress} in progress · ${counts.pending} to do`,
      "feat-quiet",
    ),
  );
  for (const item of data.todos) {
    const row = document.createElement("div");
    row.className = "feat-todo";
    const state = typeof item?.status === "string" ? item.status : "";
    // The order is the model's. A list sorted by status here would move a task
    // the moment it started, and the model wrote them in the order it means to
    // do them.
    const mark = document.createElement("span");
    mark.className = `feat-mark feat-${state.replace(/[^a-z_]/g, "") || "unknown"}`;
    mark.setAttribute("aria-hidden", "true");
    mark.textContent = { completed: "✓", in_progress: "▸", pending: "·" }[state] ?? "?";
    const said = document.createElement("span");
    said.className = state === "completed" ? "feat-done" : "feat-said";
    said.textContent = typeof item?.content === "string" ? item.content : JSON.stringify(item);
    // The status is spelled out beside the mark, because a glyph alone is a
    // state nobody using a screen reader can read and a state this build has
    // never heard of has no glyph at all.
    const word = document.createElement("span");
    word.className = "feat-state";
    word.textContent = state;
    row.append(mark, said, word);
    root.append(row);
  }
  return root;
}

/** One thing the run reported, and who said it. */
function feedbackView(data) {
  if (typeof data.text !== "string") return null;
  const root = block();
  root.append(line(data.text, "feat-said"));
  // `author` absent is an unattributed remark and not an anonymous one, which
  // is the engine's distinction and worth keeping: a run that had no author to
  // name should not be drawn as one that withheld a name.
  root.append(line(data.author ? `- ${data.author}` : "- unattributed", "feat-quiet"));
  return root;
}

/** One attachment, named, measured and described - never its bytes. */
function attachmentView(data) {
  if (typeof data.name !== "string") return null;
  const root = document.createElement("div");
  root.className = "feat-file";
  root.append(line(data.name, "feat-said"));
  const facts = [];
  if (typeof data.media_type === "string") facts.push(data.media_type);
  if (typeof data.bytes === "number") facts.push(size(data.bytes));
  if (data.dimensions) facts.push(`${data.dimensions.width}×${data.dimensions.height}`);
  if (facts.length > 0) root.append(line(facts.join(" · "), "feat-quiet"));
  return root;
}

// --- the small pieces --------------------------------------------------------

function block() {
  const node = document.createElement("div");
  node.className = "feat";
  return node;
}

function line(text, className) {
  const node = document.createElement("p");
  node.className = className;
  node.textContent = text;
  return node;
}

/**
 * A byte count, for a reader.
 *
 * Formatted here because the engine deliberately does not: a view "carries no
 * presentation", and the surface is the side that knows how much room it has.
 * Binary units with binary names - 1024 bytes is a KiB and calling it a kB is
 * the error that makes a file look 2.4% smaller than the disk says.
 */
export function size(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} bytes`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = bytes / 1024;
  let at = 0;
  while (value >= 1024 && at < units.length - 1) {
    value /= 1024;
    at += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[at]}`;
}
