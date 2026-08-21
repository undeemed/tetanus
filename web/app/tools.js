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
// This build serves exactly one tool: `echo`. The filesystem, process, MCP and
// web tools are real, but they are real on other lanes' branches and none of
// them is on this tree - so a view for them here would be a screen drawn
// against a shape nobody can produce, which is the mock-and-rewrite the gap
// list warns about.
//
// What exists instead is the seam they drop into. `views` is a table keyed by
// tool name; a tool with no entry gets the generic frame, which is honest and
// complete rather than a placeholder: name, arguments as a tree, the result,
// and whether it worked. When the fs lane lands `read_file`, the view for it is
// one entry in this table and no change anywhere else.
//
// # Why the generic frame is not a fallback to be embarrassed about
//
// A tool this page has never heard of is the ordinary case, not the exception:
// MCP servers advertise their own tools, so the set is open by construction.
// A surface that rendered only the tools it knew would show blanks for exactly
// the tools a deployment added on purpose.

import { disclosure, jsonTree, pill } from "./primitives.js";

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
    call: (args) => text(typeof args?.text === "string" ? args.text : JSON.stringify(args)),
    result: (content) => text(content),
  },
};

/** A tool call, drawn by its own view when it has one. */
export function toolCall(name, args) {
  const root = frame("call", name);
  const view = views[name];
  root.body.append(view?.call ? view.call(args) : jsonTree(args ?? {}));
  return root;
}

/**
 * A tool result, drawn by the same view, and told apart by whether it worked.
 *
 * `ok` is the tool's own answer and not this page's guess. A tool that failed
 * says so in the protocol, and a surface that inferred failure from an empty
 * result would call a successful `list_dir` on an empty directory a failure.
 */
export function toolResult(name, content, ok) {
  const root = frame(ok ? "result" : "failed", name);
  const view = views[name];
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
function frame(kind, name) {
  const root = disclosure(name, { open: false, tone: kind === "failed" ? "bad" : undefined });
  root.classList.add(`tool-${kind}`);
  return root;
}

/** A block of text from a tool, drawn as text and never as markup. */
function text(said) {
  const node = document.createElement("pre");
  node.className = "tool-text";
  node.textContent = said ?? "";
  return node;
}
