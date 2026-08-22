// The shell tools, drawn as a terminal.
//
// Upstream's `ui-tool` renders `bash` as a command and its output, with the
// exit status as a pill rather than as a line of text at the bottom. It can do
// that because the markers the tool appends are, in `crates/exec`'s own words,
// "a wire format in all but name": `[exit code: N]`, `[timed out after Nms]`,
// `[killed by signal: X]`, and `crate::shell::parse_exit` is the parser on the
// engine side ([crates/exec/src/shell.rs](../../crates/exec/src/shell.rs)).
//
// This is that parser for the page. It peels the markers `markers_of` appends,
// in the shape it appends them, and draws each as what it is: a status as a
// pill, a policy denial as a note, a sweep as a note.
//
// # Why the markers are read at all, rather than left in the text
//
// Three of them change what the output *means* and are invisible at the bottom
// of forty lines of build log. A command that timed out printed the same first
// thirty-nine lines as one that finished. A command the sandbox denied is not a
// broken command, and a reader who does not see that goes looking for the bug.
// A command whose process group had to be swept left something running.
//
// # Where this can be wrong, and what it costs
//
// A command whose *own output* ends with a line indistinguishable from a marker
// gets that line read as a marker. `parse_exit` carries exactly the same risk
// and documents it, and the cost is one pill drawn from a line of output. So
// the shapes matched here are exact and closed: an unrecognised bracketed line
// stays in the body, where it came from.
//
// # A failure is drawn by this view, unlike the filesystem family
//
// `ok` is false whenever the command failed, and a failed command is precisely
// when a reader needs the exit code and the stderr split out. So these views
// declare `failure` as well as `result`; the file tools deliberately do not,
// because their failures are `FS_NOT_FOUND: ...` and have no shape to read.

import { pill } from "./primitives.js";
import { codeBlock } from "./markdown.js";

/** The markers `crates/exec` appends, and what each one is. */
const MARKERS = [
  { start: "[exit code: ", end: "]", as: "pill", tone: "bad", say: (v) => `exit ${v}` },
  { start: "[killed by signal: ", end: "]", as: "pill", tone: "bad", say: (v) => `killed by ${v}` },
  { start: "[timed out after ", end: "]", as: "pill", tone: "bad", say: (v) => `timed out after ${v}` },
  { exact: "[interrupted]", as: "pill", tone: "busy", say: () => "interrupted" },
  { start: "[sandbox: ", end: "]", as: "note", say: (v) => v },
  {
    exact: "[the command left processes running; they were killed with its process group]",
    as: "note",
    say: () => "the command left processes running; they were killed with its process group",
  },
  { start: "[output truncated;", end: "]", as: "note", say: (v) => `output truncated;${v}` },
];

// The brackets are dropped from a note and kept nowhere: they are the marker
// syntax, and this page has just finished parsing it. What is inside each of
// these is a sentence written for a person - "this is policy, not a bug in the
// command" - and printing it still wrapped in the machine's punctuation asks
// the reader to do the parsing again.
// A pill's text is rewritten rather than unwrapped, because `exit code: 2` is
// a field name and a value where `exit 2` is the thing itself.

/** Views for the five tools `crates/exec` registers. */
export const shellViews = {
  shell: {
    // The model is asked for a `description` in five to ten words "for the
    // person watching", so when it wrote one, that is the summary it wrote it
    // to be. The command itself is one keystroke away in the body.
    summary: (args) => phrase(args?.description) ?? phrase(args?.command),
    call: (args) => command(args, ["workdir", "timeout_ms"]),
    result: (said) => terminal(said),
    failure: (said) => terminal(said),
  },

  shell_run: {
    summary: (args) => {
      const said = phrase(args?.command);
      const where = phrase(args?.session_id);
      return said && where ? `${said} · in ${where}` : said ?? where;
    },
    call: (args) => command(args, ["session_id"]),
    result: (said) => terminal(said),
    failure: (said) => terminal(said),
  },

  shell_open: { summary: (args) => phrase(args?.cwd) ?? "the workspace root" },
  shell_close: { summary: (args) => phrase(args?.session_id) },
  shell_list: { result: (said) => sessions(said) },
};

/**
 * What a command printed, with the markers taken off the end and drawn.
 *
 * The peel is a loop from the end because `markers_of` appends between one and
 * four of them, in a fixed order, one per line. Reading only the last line -
 * which is all `parse_exit` needs, since it only wants the status - would miss
 * the sandbox denial that is the whole reason the command failed.
 */
export function terminal(said) {
  const { body, marks } = peel(String(said ?? ""));
  const root = document.createElement("div");
  root.className = "sh";

  if (marks.some((mark) => mark.as === "pill")) {
    const row = document.createElement("div");
    row.className = "sh-marks";
    for (const mark of marks) {
      if (mark.as === "pill") row.append(pill(mark.text, mark.tone));
    }
    root.append(row);
  } else {
    // Silence is what success looks like to the model, and a pill is what it
    // looks like to a reader: the absence of a status marker is `exit 0`, and
    // saying so is cheaper than making someone infer it from nothing.
    const row = document.createElement("div");
    row.className = "sh-marks";
    row.append(pill("exit 0", "ok"));
    root.append(row);
  }

  const [out, err] = split(body);
  if (out === "(no output)") {
    root.append(note("the command printed nothing"));
  } else if (out !== "") {
    root.append(stream(out, null));
  }
  if (err !== null) root.append(stream(err, "stderr"));

  for (const mark of marks) {
    if (mark.as === "note") root.append(note(mark.text));
  }
  return root;
}

/**
 * `shell_list`'s answer: `id`, backend, directory and state, tab-separated.
 *
 * The one row that matters is a session that is gone, because every later
 * `shell_run` against it will fail and the reason is here. A row this page
 * cannot split is printed whole rather than dropped.
 */
export function sessions(said) {
  const root = document.createElement("div");
  root.className = "sh-list";
  for (const row of String(said ?? "").replace(/\n$/, "").split("\n")) {
    const parts = row.split("\t");
    if (parts.length < 4) {
      root.append(note(row));
      continue;
    }
    const [id, backend, cwd, state] = parts;
    const line = document.createElement("div");
    line.className = "sh-session";
    line.append(cell(id, "sh-id"), cell(backend, "sh-backend"), cell(cwd, "sh-cwd"));
    // `gone: <reason>` is the tool's own wording and is kept; only the tone is
    // this page's, and it is decided on the prefix rather than on a guess.
    line.append(pill(state, state.startsWith("gone") ? "bad" : "ok"));
    root.append(line);
  }
  return root;
}

/**
 * Take the trailing markers off a rendered result.
 *
 * Returned in the order the engine wrote them, which is the order a reader
 * needs: what the policy did, what the harness did, what the budget did, and
 * last the exit status.
 */
export function peel(text) {
  const lines = text.replace(/\n$/, "").split("\n");
  const marks = [];
  while (lines.length > 0) {
    const found = recognise(lines[lines.length - 1]);
    if (!found) break;
    marks.unshift(found);
    lines.pop();
  }
  return { body: lines.join("\n"), marks };
}

/** One line, as a marker, or `null` for every line that is not one. */
function recognise(line) {
  for (const shape of MARKERS) {
    if (shape.exact === line) {
      return { as: shape.as, tone: shape.tone, text: shape.say ? shape.say() : line };
    }
    if (shape.start && line.startsWith(shape.start) && line.endsWith(shape.end)) {
      const value = line.slice(shape.start.length, line.length - shape.end.length);
      // A marker's value is one line and carries no bracket of its own, which
      // is the same guard `suffix_marker` uses on the engine side.
      if (value.includes("]")) return null;
      return { as: shape.as, tone: shape.tone, text: shape.say ? shape.say(value) : line };
    }
  }
  return null;
}

/**
 * The body, split at the `[stderr]` line the renderer puts between the two
 * streams. `null` for the second half when the command wrote nothing to it.
 */
function split(body) {
  const at = body.split("\n").indexOf("[stderr]");
  if (at < 0) return [body, null];
  const lines = body.split("\n");
  return [lines.slice(0, at).join("\n"), lines.slice(at + 1).join("\n")];
}

/** One stream of output, captioned when it is the one nobody expects. */
function stream(text, caption) {
  const root = document.createElement("div");
  root.className = caption ? "sh-stream sh-bad" : "sh-stream";
  if (caption) {
    const head = document.createElement("p");
    head.className = "sh-caption";
    head.textContent = caption;
    root.append(head);
  }
  const body = document.createElement("pre");
  body.className = "sh-text";
  body.textContent = text;
  root.append(body);
  return root;
}

/** The command, as a block, with whatever else the call carried beside it. */
function command(args, extras) {
  const said = phrase(args?.command);
  if (said === null) return null;
  const root = document.createElement("div");
  root.className = "sh-call";
  root.append(codeBlock(args.command, "sh"));
  const facts = extras
    .filter((key) => args?.[key] !== undefined && args?.[key] !== null && args?.[key] !== "")
    .map((key) => `${key}: ${args[key]}`);
  if (facts.length > 0) root.append(note(facts.join(" · ")));
  return root;
}

function note(text) {
  const node = document.createElement("p");
  node.className = "sh-note";
  node.textContent = text;
  return node;
}

function cell(text, className) {
  const node = document.createElement("span");
  node.className = className;
  node.textContent = text;
  return node;
}

/** A non-empty string an argument carries, or `null`. */
function phrase(value) {
  if (typeof value !== "string") return null;
  const said = value.trim();
  return said === "" ? null : said;
}
