// What the run is working toward: the goal, the plan, the skills.
//
// Upstream ships `ui-goal`, `ui-plan` and `ui-skill` as separate modules. They
// answer one question between them - "what is this agent trying to do, and
// under what plan" - and they share a shape: a small amount of standing state
// that changes rarely, sits beside the conversation rather than inside it, and
// is read from the journal so a session opened tomorrow still shows it.
//
// # This is a frame with the views registered and empty, on purpose
//
// The features crate is real and it is on `fm/tetanus-p2-features`, not on this
// tree and not on master. It writes `goal/changed`, `plan/mode` and
// `plan/presented`, and none of those is in the event vocabulary this branch's
// contract publishes (§4.3.2 names ten durable types and six staged ones; these
// are not among them).
//
// So the rule the tool frame set applies here too, and the instruction says it
// outright: register the view, leave it empty, do not fake a shape. What that
// means concretely:
//
// - `PANELS` names the three event types and says, in one line each, what the
//   view will read when the shape is published. Nothing reads a field today.
// - `standing()` folds a journal and returns what it finds, which on this tree
//   is always nothing, so `panel()` draws the honest empty state rather than a
//   mock goal.
// - The events themselves are not swallowed. Until a view claims one, the page
//   draws it through the raw path it already has for an unknown durable type,
//   which is what §4.3.2 means by "a surface renders them raw until it takes
//   them".
//
// The alternative - reading `event.data.goal.objective` today, from a struct on
// a branch nobody has landed - would couple this file to a shape that is still
// free to change, and would draw a panel nothing here can produce.

/**
 * The standing-state panels, by the event type each one waits for.
 *
 * `reads` is documentation rather than code: it is what the view will take
 * when the type is published, written down so the follow-up slice is an entry
 * here and not an investigation.
 */
export const PANELS = {
  "goal/changed": {
    title: "Goal",
    reads: "the objective, its phase, and the blocker while it is blocked",
    view: null,
  },
  "plan/mode": {
    title: "Plan mode",
    reads: "whether the agent is planning rather than acting",
    view: null,
  },
  "plan/presented": {
    title: "Plan",
    reads: "the plan the agent put up for review",
    view: null,
  },
  // Published by the features lane while this file was being written, and
  // still not on this branch: `Attachment { id, name, media_type, ... }` with
  // a `Limits` beside it. Registered now so the entry is waiting rather than
  // being discovered later.
  "attachment/added": {
    title: "Attached",
    reads: "each attachment's name and media type, against the limits the deployment set",
    view: null,
  },
};

/**
 * Fold a journal into the standing state a panel would draw.
 *
 * Returns one entry per panel that has anything to say. On this tree that is
 * always none, because nothing writes these types - which is the correct
 * answer and not a stub: the fold is real, and the day an engine writes one
 * the entry appears with the raw event in it, ready for a view to claim.
 */
export function standing(events) {
  const found = [];
  for (const event of events) {
    const panel = PANELS[event.type];
    if (!panel) continue;
    // Last one wins: these are standing facts, not a history. A goal edited
    // three times is one goal, and the newest record is the one that holds.
    const already = found.findIndex((entry) => entry.type === event.type);
    const entry = { type: event.type, title: panel.title, event };
    if (already >= 0) found[already] = entry;
    else found.push(entry);
  }
  return found;
}

/**
 * Draw the standing state, or say plainly that there is none.
 *
 * The empty state names why rather than showing a blank: a reader looking at a
 * panel with nothing in it should be able to tell "this agent has no goal"
 * from "this build cannot show goals yet", because those call for different
 * things - one is a prompt away, the other is a lane landing.
 */
export function panel(root, events, { hasGoals = false } = {}) {
  root.replaceChildren();
  const entries = standing(events);
  if (entries.length === 0) {
    const none = document.createElement("p");
    none.className = "list-empty";
    none.textContent = hasGoals
      ? "This run has no goal or plan yet."
      : "Goals, plans and skills are not part of this build yet.";
    root.append(none);
    return;
  }
  for (const entry of entries) {
    const box = document.createElement("div");
    box.className = "trace-turn";
    const head = document.createElement("div");
    head.className = "trace-head";
    head.textContent = entry.title;
    const said = document.createElement("pre");
    said.className = "tool-text";
    // Drawn raw until a view claims it, which is what §4.3.2 asks a surface to
    // do with a durable type it has not taken yet - and what keeps a landed
    // event visible on day one rather than swallowed until somebody writes a
    // renderer for it.
    said.textContent = JSON.stringify(entry.event.data ?? {}, null, 1);
    box.append(head, said);
    root.append(box);
  }
}
