// The filesystem tools, drawn as files rather than as JSON.
//
// Upstream's `ui-tool` is twenty-six components and most of them exist for
// this family: a file read looks like a file, a listing looks like a
// directory, a search looks like results. Until now every one of these seven
// tools drew as the generic frame - a fold labelled `read`, and a `<pre>` of
// whatever the tool wrote. That is honest, and it is unreadable at the two
// moments it matters: skimming a transcript to find where a file was touched,
// and reading a windowed file whose line numbers are tab-aligned into a
// proportional column.
//
// `crates/fs` landed on master with seven tools - read, write, edit, list,
// glob, stat, delete - so these views read a shape this tree can actually
// produce ([crates/fs/src/tools.rs](../../crates/fs/src/tools.rs)).
//
// # These tools answer prose, and that is the design constraint
//
// Every one of them returns rendered text, not a structure: `read` writes a
// header line and then `%6d\t<line>`, `list` writes a header and then a name
// per row. So a view here is a *reader* of that text, and a reader can be
// wrong - the format is the engine's to change.
//
// Every one of these views therefore degrades to showing the line exactly as
// the tool wrote it. A row that does not parse is not dropped and not guessed
// at; it is printed. The worst outcome of the engine rewording a header is
// that this page shows the header as a sentence instead of as a caption, which
// is what the generic frame did anyway.
//
// # What is deliberately not here
//
// No diffing of an `edit` against the file it edited. The tool reports how
// many occurrences it replaced and not what the file now says, so a diff would
// have to be computed against a copy this page does not have - and a diff
// drawn from the wrong base is worse than no diff.

import { codeBlock } from "./markdown.js";

/** The most lines of a file this page will draw as rows. */
const MOST_ROWS = 600;

/**
 * Views for the seven filesystem tools, keyed the way `crates/fs` registers
 * them.
 *
 * A view is `{ summary, call, result }` and every part is optional: a tool
 * with no `result` falls back to the tool's own words, which for the four that
 * answer a single sentence is the right rendering already.
 */
export const fileViews = {
  /**
   * The one whose result most needs a view: a numbered window of a file.
   */
  read: {
    summary: (args) => {
      const where = path(args);
      const from = whole(args?.offset);
      const many = whole(args?.limit);
      if (from && many) return `${where} · ${many} lines from ${from}`;
      if (from) return `${where} · from line ${from}`;
      if (many) return `${where} · first ${many} lines`;
      return where;
    },
    result: (said) => numbered(said),
  },

  /**
   * A write is the one call whose arguments are the interesting part: the
   * content is the change, and a reader scanning a transcript for what was
   * written should not have to open a JSON tree to find it.
   */
  write: {
    summary: (args) => {
      const content = typeof args?.content === "string" ? args.content : null;
      return content === null
        ? path(args)
        : `${path(args)} · ${count(rows(content), "line")}`;
    },
    call: (args) =>
      typeof args?.content === "string"
        ? codeBlock(args.content, language(path(args)))
        : null,
  },

  /**
   * An edit is two texts, and showing them one above the other is the whole
   * job. Labelled rather than coloured: red and green are how a diff is drawn
   * and this is not one - there is no third text to diff against.
   */
  edit: {
    summary: (args) =>
      args?.replace_all ? `${path(args)} · every occurrence` : path(args),
    call: (args) => {
      if (typeof args?.old_string !== "string") return null;
      const root = document.createElement("div");
      root.className = "fs-edit";
      root.append(labelled("replace", args.old_string, path(args)));
      // An empty replacement is a deletion, and a code block containing
      // nothing reads as a rendering failure. Say which it is.
      const now = typeof args.new_string === "string" ? args.new_string : "";
      root.append(
        now === ""
          ? labelled("with nothing - the matched text is deleted", null, null)
          : labelled("with", now, path(args)),
      );
      // `replace_all` is on the fold and is not repeated here. It was, and the
      // page said the same thing twice a centimetre apart.
      return root;
    },
  },

  list: {
    summary: (args) => path(args, "."),
    result: (said) => entries(said),
  },

  glob: {
    summary: (args) => {
      const pattern = typeof args?.pattern === "string" ? args.pattern : "";
      const where = typeof args?.path === "string" ? args.path.trim() : "";
      return where ? `${pattern} under ${where}` : pattern;
    },
    result: (said) => matches(said),
  },

  stat: { summary: (args) => path(args) },

  delete: {
    summary: (args) =>
      args?.recursive ? `${path(args)} · and everything under it` : path(args),
  },
};

// --- reading what these tools wrote ----------------------------------------

/**
 * `read`'s answer: a caption, then `%6d\t<line>`, then sometimes a note saying
 * how much was left.
 *
 * The line numbers go in a gutter of their own so the code starts at one
 * column whatever the numbers are, which is the thing tab alignment cannot do
 * once the font is not the terminal's. A row that is not `number tab text` is
 * printed as it came: the caption is one, the trailing note is one, and so is
 * whatever the engine writes there next.
 */
export function numbered(said) {
  const rows = lines(said);
  const root = block("fs-file");
  let drawn = 0;
  for (const row of rows) {
    const hit = /^\s*(\d+)\t([\s\S]*)$/.exec(row);
    if (!hit) {
      root.append(note(row));
      continue;
    }
    if (drawn === MOST_ROWS) {
      root.append(
        note(`… the rest of this result is not drawn; ${MOST_ROWS} lines is this page's limit`),
      );
      // Every further numbered row is skipped, but a note after them is not:
      // "read again from line N" is the one line a reader needs most.
      continue;
    }
    if (drawn > MOST_ROWS) continue;
    drawn += 1;
    const line = document.createElement("div");
    line.className = "fs-line";
    const no = document.createElement("span");
    no.className = "fs-no";
    no.textContent = hit[1];
    // The gutter is decoration to anything that reads the page aloud, and a
    // screen reader saying every line number before every line is unusable.
    no.setAttribute("aria-hidden", "true");
    const code = document.createElement("span");
    code.className = "fs-code";
    code.textContent = hit[2];
    line.append(no, code);
    root.append(line);
  }
  return root;
}

/**
 * `list`'s answer: a caption, then a name per row - a directory with a
 * trailing slash, a file with its size in parentheses, anything else with its
 * kind.
 *
 * The slash is the tool's mark and not this page's guess, which is the same
 * rule the directory picker follows about `hidden`: re-deriving what the
 * engine already said is how two answers to one question appear.
 */
export function entries(said) {
  const rows = lines(said);
  const root = block("fs-list");
  for (const row of rows) {
    const dir = /^(.+)\/$/.exec(row);
    if (dir) {
      root.append(entry(`${dir[1]}/`, null, "fs-dir"));
      continue;
    }
    const sized = /^(.+) \((\d+) bytes\)$/.exec(row);
    if (sized) {
      root.append(entry(sized[1], `${sized[2]} bytes`, null));
      continue;
    }
    const kinded = /^(.+) \(([a-z ]+)\)$/.exec(row);
    if (kinded) {
      root.append(entry(kinded[1], kinded[2], "fs-other"));
      continue;
    }
    root.append(note(row));
  }
  return root;
}

/**
 * `glob`'s answer: one path per row, and sometimes a line saying it stopped.
 *
 * That last line is the one a reader must not miss - a search that stopped at
 * its cap and a search that found exactly that many look identical otherwise -
 * so it keeps the note styling rather than being drawn as another result.
 */
export function matches(said) {
  const rows = lines(said);
  const root = block("fs-list");
  for (const row of rows) {
    root.append(row.startsWith("...") ? note(row) : entry(row, null, null));
  }
  return root;
}

// --- the small pieces -------------------------------------------------------

/** A result's rows, with the blank last line a trailing newline leaves. */
function lines(said) {
  return String(said ?? "").replace(/\n$/, "").split("\n");
}

/** A caption or an aside: whatever the tool wrote, printed as it wrote it. */
function note(text) {
  const node = document.createElement("p");
  node.className = "fs-note";
  node.textContent = text;
  return node;
}

function block(className) {
  const node = document.createElement("div");
  node.className = className;
  return node;
}

/** One row of a listing: a name, and the fact the tool put beside it. */
function entry(name, fact, tone) {
  const row = document.createElement("div");
  row.className = "fs-entry" + (tone ? ` ${tone}` : "");
  const said = document.createElement("span");
  said.className = "fs-name";
  said.textContent = name;
  row.append(said);
  if (fact) {
    const aside = document.createElement("span");
    aside.className = "fs-fact";
    aside.textContent = fact;
    row.append(aside);
  }
  return row;
}

/** A titled half of an edit. */
function labelled(what, text, named) {
  const root = document.createElement("div");
  root.className = "fs-half";
  const head = document.createElement("p");
  head.className = "fs-note";
  head.textContent = what;
  root.append(head);
  if (text !== null) root.append(codeBlock(text, language(named)));
  return root;
}

/** The path an argument names, or a phrase saying it named none. */
function path(args, fallback = "") {
  const said = typeof args?.path === "string" ? args.path.trim() : "";
  return said || fallback || "an unnamed path";
}

/** A positive whole number an argument carries, or `null`. */
function whole(value) {
  return Number.isInteger(value) && value > 0 ? value : null;
}

function count(many, thing) {
  return `${many} ${thing}${many === 1 ? "" : "s"}`;
}

/**
 * How many lines a piece of text has, counted the way the tool counts them.
 *
 * Rust's `str::lines` treats a trailing newline as the end of the last line
 * and not as the start of an empty one, so `"a\n"` is one line. Splitting on
 * `\n` here would say two, and the summary would then disagree with the
 * `(N lines, M bytes)` the tool's own result reports about the same write.
 */
function rows(text) {
  return text === "" ? 0 : text.replace(/\n$/, "").split("\n").length;
}

/**
 * What to call the language of a file, for the block's caption only.
 *
 * The extension, lowercased, with nothing mapped onto anything: `rs` reads as
 * `rs`. A table of pretty names here would be a second place that decides what
 * a `.rs` file is, and the caption is the only thing that reads it - nothing
 * on this page highlights syntax.
 */
function language(named) {
  const hit = /\.([A-Za-z0-9]+)$/.exec(String(named ?? "").split("/").pop() ?? "");
  return hit ? hit[1].toLowerCase() : "text";
}
