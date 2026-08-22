// How a tool call and its result are drawn.
//
// Upstream's `ui-tool` is 26 components: one shared frame and a view per tool,
// so a shell command looks like a terminal, a file read looks like a file, and
// a search looks like results. The frame is the part that has to exist first,
// for the same reason the primitives did - every per-tool view sits on it, and
// adding it later means rewriting all of them.
//
// # What is here, and what is deliberately not
//
// `views` is a table keyed by tool name; a tool with no entry gets the generic
// frame, which is honest and complete rather than a placeholder: name,
// arguments as a tree, the result, and whether it worked.
//
// The seam has now been used twice as intended. `echo` is this file's own; the
// seven filesystem tools are one import from `tool-files.js` and no change
// anywhere else, which is what the table was for. The process, MCP and web
// tools follow the same way as their lanes land.
//
// # The fold line carries a summary, because a name alone is not one
//
// A transcript of a dozen calls all labelled `read` tells a reader nothing
// about which file, and opening twelve folds to find out is the cost. Upstream
// puts a per-tool summary on its `ToolRow` for the same reason. A view supplies
// it from the arguments; a tool without a view still shows its name alone,
// because a summary this page invented from arguments it does not understand
// would be a guess printed as a fact.
//
// # Why the generic frame is not a fallback to be embarrassed about
//
// A tool this page has never heard of is the ordinary case, not the exception:
// MCP servers advertise their own tools, so the set is open by construction.
// A surface that rendered only the tools it knew would show blanks for exactly
// the tools a deployment added on purpose.

import { disclosure, jsonTree, pill } from "./primitives.js";
import { fileViews } from "./tool-files.js";

/**
 * Views by tool name. Empty of everything but the tool this build has, and
 * that is the point: an entry here is a promise that the shape it reads is a
 * shape this tree can produce.
 */
export const views = {
  /**
   * Where the session is working: the project root, how it was identified,
   * the instruction files the project keeps, and what is at the top level.
   *
   * Registered and inert. The tool is on `fm/tetanus-p2-features` and answers
   * **rendered text** rather than a structure - `Workspace::render()` - so the
   * view that claims it will be a reader of that text, and writing it against
   * a format on an unlanded branch would be writing against a format still
   * free to change. Until then the generic frame shows exactly what the tool
   * said, which for a text-answering tool is most of what a view would do
   * anyway.
   */
  workspace_info: null,

  /**
   * `echo` returns what it was given. Its result is the text, so it is drawn
   * as text rather than as a one-key tree that a reader has to open to find a
   * sentence in.
   */
  echo: {
    summary: (args) => (typeof args?.text === "string" ? args.text : null),
    call: (args) => text(typeof args?.text === "string" ? args.text : JSON.stringify(args)),
    result: (content) => text(content),
  },

  // The filesystem family, from `crates/fs`. Spread rather than listed, so
  // adding a file tool is a change to one file and not to two.
  ...fileViews,
};

/**
 * A tool call, drawn by its own view when it has one.
 *
 * A view's `call` may answer `null` for arguments it does not recognise - a
 * `write` with no `content` in it - and that falls back to the tree rather
 * than to an empty fold. A view is a better rendering of a shape it knows, not
 * a promise to render every shape.
 */
export function toolCall(name, args) {
  const view = views[name];
  const root = frame("call", name, said(view, args));
  root.body.append(view?.call?.(args) ?? jsonTree(args ?? {}));
  return root;
}

/**
 * The summary beside a tool's name, or nothing.
 *
 * Bounded, and bounded here rather than in each view, so no view can put a
 * model-written argument of any length onto the fold and push the rest of the
 * row off the screen. A summary is a glance; the body is the detail.
 */
function said(view, args) {
  let summary = null;
  try {
    summary = view?.summary?.(args);
  } catch {
    // A view that threw on arguments it did not expect loses its summary and
    // nothing else. The call itself is still drawn, which is the part the
    // transcript cannot do without.
    summary = null;
  }
  if (typeof summary !== "string") return null;
  const oneLine = summary.replace(/\s+/g, " ").trim();
  if (!oneLine) return null;
  return oneLine.length > SUMMARY_MAX ? `${oneLine.slice(0, SUMMARY_MAX - 1)}\u2026` : oneLine;
}

/** How much of a summary fits on a fold before it stops being a glance. */
const SUMMARY_MAX = 90;

/**
 * A tool result, drawn by the same view, and told apart by whether it worked.
 *
 * `ok` is the tool's own answer and not this page's guess. A tool that failed
 * says so in the protocol, and a surface that inferred failure from an empty
 * result would call a successful `list_dir` on an empty directory a failure.
 */
export function toolResult(name, content, ok) {
  const view = views[name];
  // A failure is the tool's own words and never a view's reading of them: the
  // views here parse a success format, and `read`'s failure is
  // `FS_NOT_FOUND: ...`, which has no numbered lines in it to find.
  const root = frame(ok ? "result" : "failed", name, null);
  root.body.append(view?.result && ok ? view.result(content) : text(content));
  if (!ok) root.head.append(pill("failed", "bad"));
  return root;
}

/**
 * The shared frame: a folded row with the tool's name on it.
 *
 * Folded, because a transcript is read for the conversation and opened for the
 * detail. Upstream folds the same way, and the reason shows the moment a tool
 * returns a file: an unfolded result pushes the reply that follows it off the
 * screen.
 */
function frame(kind, name, summary) {
  const root = disclosure(name, { open: false, tone: kind === "failed" ? "bad" : undefined });
  root.classList.add(`tool-${kind}`);
  if (summary) {
    const aside = document.createElement("span");
    aside.className = "tool-said";
    aside.textContent = summary;
    root.head.append(aside);
  }
  return root;
}

/** A block of text from a tool, drawn as text and never as markup. */
function text(said) {
  const node = document.createElement("pre");
  node.className = "tool-text";
  node.textContent = said ?? "";
  return node;
}
