// The feature tools, drawn with the renderers the panel already uses.
//
// Seven of the tools this binary offers had no view: `todo_write`,
// `update_goal`, `get_goal`, `exit_plan_mode`, `report_feedback`, `skill` and
// `tools`. Their durable *events* have been drawn in the trace panel for a
// while, so the page could show you the task list under "Tasks" while the call
// that wrote it sat in the transcript as a JSON tree of eleven objects.
//
// # The renderers are imported, not rewritten
//
// This is the point of the slice rather than a convenience. `todo_write`'s
// arguments *are* `todo/write`'s payload - both are `{ todos: [...] }` - and
// `exit_plan_mode`'s argument is `plan/presented`'s. Writing a second reading
// of those shapes here would be two places that decide what a task list looks
// like, and they would drift the first time one of them learned a new status.
// So `features.js` exports its three renderers and this file calls them.
//
// What is *not* shared is the wording around them, because a call and a record
// are different claims: the call is what the model asked for, the record is
// what the session now holds.
//
// # `get_goal` and the two absences
//
// A read that finds no goal answers `{ "goal": null, "cleared": false }` or the
// same with `cleared: true`, and the engine is explicit that these are
// different facts - only one of them means somebody decided something. The
// generic frame would have shown both as two lines of JSON that differ by one
// word.

import { goalView, planView, todosView } from "./features.js";
import { markdown } from "./markdown.js";
import { pill } from "./primitives.js";

/** Views for the tools `crates/features` registers. */
export const featureViews = {
  /**
   * The whole task list, replacing any previous one. Drawn as the list it is,
   * on the call as well as the result: the call is where the model says what
   * it intends to do, and that is the interesting half.
   */
  todo_write: {
    summary: (args) => (Array.isArray(args?.todos) ? count(args.todos.length, "task") : null),
    call: (args) => todosView(args ?? {}),
    result: (said) => parsed(said, (data) => todosView(data)),
  },

  update_goal: {
    // `action` is the verb - create, update, pause, block, complete, clear -
    // and it is what a reader scanning for "when did this get blocked" is
    // looking for, so it leads.
    summary: (args) => {
      const action = phrase(args?.action);
      const what = phrase(args?.objective) ?? phrase(args?.blocker_message);
      return action && what ? `${action}: ${what}` : action ?? what;
    },
    call: (args) => asked(args ?? {}),
    result: (said) => parsed(said, (data) => goalView(data)),
  },

  get_goal: {
    result: (said) =>
      parsed(said, (data) => (data.goal ? goalView(data) : absent(data))),
  },

  /**
   * The plan the model worked out. Markdown, and rendered - `plan/presented`
   * is drawn that way in the panel for the reason the contract note gives, and
   * the call carries the identical string.
   */
  exit_plan_mode: {
    summary: (args) => firstLine(args?.plan),
    call: (args) => planView(args ?? {}),
  },

  report_feedback: {
    summary: (args) => phrase(args?.text),
    // Not `feedbackView`: that one draws the *record*, which carries an author,
    // and a call has none. Attributing this to "unattributed" would be saying
    // something false about a remark whose author is plainly the model.
    call: (args) => said(phrase(args?.text) ?? ""),
  },

  skill: {
    summary: (args) => phrase(args?.name),
    // `# Skill: <name>` and then the skill's own text. It is markdown written
    // for a model to read, and a reader checking what the model was just told
    // is reading the same document.
    result: (text) => markdown(String(text ?? "")),
  },

};

/**
 * A goal change as the arguments describe it, before the engine has answered.
 *
 * The revision is drawn because it is the compare-and-set token: a call that
 * carries one is a call that can be refused for naming a goal that has moved,
 * and that is the likeliest reason for the failure underneath it.
 */
function asked(args) {
  const root = document.createElement("div");
  root.className = "feat";
  const head = document.createElement("div");
  head.className = "feat-head";
  if (phrase(args.action)) head.append(pill(args.action, "busy"));
  if (phrase(args.objective)) head.append(line(args.objective, "feat-said"));
  root.append(head);
  if (phrase(args.blocker_code) || phrase(args.blocker_message)) {
    root.append(
      line(
        [phrase(args.blocker_code), phrase(args.blocker_message)].filter(Boolean).join(": "),
        "feat-bad",
      ),
    );
  }
  if (Number.isInteger(args.revision)) {
    root.append(line(`against revision ${args.revision}`, "feat-quiet"));
  }
  return root;
}

/** What a `get_goal` found instead of a goal. */
function absent(data) {
  const root = document.createElement("div");
  root.className = "feat";
  root.append(
    line(
      data.cleared === true
        ? "no goal - the one this session had was cleared"
        : "no goal has been set on this session",
      "feat-quiet",
    ),
  );
  return root;
}

/**
 * A result the tool serialised as JSON, through a renderer - or `null`, which
 * leaves the frame to print exactly what the tool said.
 *
 * `null` rather than a guess is the whole of the error handling here. These
 * tools answer `serde_json::to_string(...)` and fall back to `"{}"` when that
 * fails, so a page that assumed the shape would draw an empty list for a
 * serialisation failure - which reads as "the model emptied the list".
 */
function parsed(text, draw) {
  let data = null;
  try {
    data = JSON.parse(String(text ?? ""));
  } catch {
    return null;
  }
  if (data === null || typeof data !== "object") return null;
  try {
    return draw(data);
  } catch {
    return null;
  }
}

function said(text) {
  const node = document.createElement("p");
  node.className = "feat-said";
  node.textContent = text;
  return node;
}

function line(text, className) {
  const node = document.createElement("p");
  node.className = className;
  node.textContent = text;
  return node;
}

/** The first line of a longer text, for a fold that has one row. */
function firstLine(value) {
  const whole = phrase(value);
  if (whole === null) return null;
  const first = whole.split("\n").find((one) => one.trim() !== "");
  return first === undefined ? null : first.trim();
}

function phrase(value) {
  if (typeof value !== "string") return null;
  const text = value.trim();
  return text === "" ? null : text;
}

function count(many, thing) {
  return `${many} ${thing}${many === 1 ? "" : "s"}`;
}
