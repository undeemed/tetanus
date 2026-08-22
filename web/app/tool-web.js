// The two web tools, drawn as a page and as results.
//
// `crates/web` fetches a page and searches, and like everything else in this
// harness both answer rendered prose. Two facts in that prose are worth
// lifting out of it, and both are things a reader would otherwise skim past.
//
// **Where a fetch actually ended.** `render_fetch` writes the *final* URL, not
// the one that was asked for, so a fetch that followed a redirect - to a login
// wall, to a consent page, to an unrelated host - says so in its first line and
// says it in the same grey as everything else. A model that summarised a
// consent page as the article is a failure that starts here, and the reader
// who could have caught it is looking at line one.
//
// **Which sources a search actually had.** The answer prose is the part a model
// will quote; the sources under it are the part that says whether to believe
// it. Upstream's search view puts them in a list with the host visible, and the
// host is the fact - `[1] Title - example.com` tells a reader more about a
// claim than the title does.
//
// # Links are vetted, never trusted
//
// Every URL here came off the network by way of a model's request. They go
// through `markdown.js`'s `link`, which refuses anything that is not http or
// https and keeps the text while withholding the link. That function exists for
// exactly this and there is no second copy of the rule here.

import { pill } from "./primitives.js";
import { link } from "./markdown.js";

/** Views for `web_fetch` and `web_search`. */
export const webViews = {
  web_fetch: {
    summary: (args) => (typeof args?.url === "string" ? args.url.trim() || null : null),
    result: (said) => fetched(said),
  },
  web_search: {
    summary: (args) => (typeof args?.query === "string" ? args.query.trim() || null : null),
    result: (said) => searched(said),
  },
};

/**
 * A fetched page: `<final url> (<status> <media type>)`, a blank line, the
 * text, and sometimes a note saying it was cut.
 *
 * The header is lifted because of where the fetch *ended*. Everything after it
 * is the page's own text and is drawn as text - not as markdown, because it is
 * already extracted prose and running a second renderer over it would invent
 * headings out of whatever the extractor left behind.
 */
export function fetched(said) {
  const text = String(said ?? "");
  const root = document.createElement("div");
  root.className = "web";
  const at = text.indexOf("\n\n");
  const head = at < 0 ? text : text.slice(0, at);
  const rest = at < 0 ? "" : text.slice(at + 2);

  const parsed = /^(\S+) \((\d+) (.+)\)$/.exec(head);
  if (!parsed) {
    // A header this page cannot read is the page's first line, and it is
    // printed as one. Nothing is inferred from a shape that did not match.
    root.append(body(text));
    return root;
  }
  const [, url, status, media] = parsed;
  const row = document.createElement("div");
  row.className = "web-head";
  row.append(link(url, url));
  // A status is a number the reader may need and a media type says whether
  // what follows is prose at all. `200` is not toned as good: a 200 that
  // landed on a login wall is the case this line exists to expose.
  row.append(pill(status, Number(status) >= 400 ? "bad" : undefined));
  row.append(pill(media));
  root.append(row);

  const cut = "\n\n[the page was longer than this tool returns; this is the beginning of it]";
  if (rest.endsWith(cut)) {
    root.append(body(rest.slice(0, -cut.length)));
    root.append(note("the page was longer than this tool returns; this is the beginning of it"));
  } else {
    root.append(body(rest));
  }
  return root;
}

/**
 * A search: the answer, then `Sources:` and a numbered block per source.
 *
 * Each source is four lines in the prose - `[n] Title - host`, the URL, and an
 * optional snippet - and they are folded back into one row so the title, the
 * host and the link are one thing rather than three lines a reader assembles.
 * A line that does not fit the shape is printed where it stood.
 */
export function searched(said) {
  const root = document.createElement("div");
  root.className = "web";
  const lines = String(said ?? "").replace(/\n+$/, "").split("\n");

  const prose = [];
  let at = 0;
  while (at < lines.length && lines[at] !== "Sources:") {
    prose.push(lines[at]);
    at += 1;
  }
  const answer = prose.join("\n").trim();
  if (answer !== "") root.append(body(answer));
  if (at >= lines.length) return root;

  at += 1;
  const list = document.createElement("ol");
  list.className = "web-sources";
  while (at < lines.length) {
    const line = lines[at];
    const titled = /^\[(\d+)\] (.*?) - (\S+)$/.exec(line);
    if (!titled) break;
    const item = document.createElement("li");
    const title = document.createElement("span");
    title.className = "web-title";
    title.textContent = titled[2];
    // The host, beside the title. It is the fact that decides whether to
    // believe a claim, and it is the one upstream shows for the same reason.
    const host = document.createElement("span");
    host.className = "web-host";
    host.textContent = titled[3];
    item.append(title, host);
    at += 1;
    // The URL is on its own indented line, and the snippet, when there is one,
    // on the next. Both are optional in the shape, so both are looked for
    // rather than assumed.
    if (at < lines.length && /^ {4}\S+:\/\//.test(lines[at])) {
      const href = lines[at].trim();
      const where = document.createElement("div");
      where.className = "web-url";
      where.append(link(href, href));
      item.append(where);
      at += 1;
    }
    if (at < lines.length && lines[at].startsWith("    ")) {
      item.append(note(lines[at].trim()));
      at += 1;
    }
    list.append(item);
  }
  if (list.children.length > 0) root.append(list);

  // Whatever is left, printed - one note per paragraph rather than one note
  // holding all of it, because the two that end up here are unrelated
  // sentences and run together they read as one confused one. Both should be
  // seen: the "more results were found" line, and the instruction to the model
  // to cite what it uses - which is not addressed to the reader but is part of
  // what the model was told, and hiding it would hide why it wrote what it
  // wrote.
  for (const said of lines.slice(at).join("\n").split(/\n\s*\n/)) {
    const trimmed = said.trim();
    if (trimmed !== "") root.append(note(unwrapped(trimmed)));
  }
  return root;
}

/**
 * A line the engine wrapped in brackets, as the sentence inside them.
 *
 * The same call the shell views make about a marker note: the brackets are the
 * machine's punctuation, this page has just finished reading it, and printing
 * it back asks the reader to read it again. Only a line that is entirely one
 * bracketed span is unwrapped, so a sentence that merely contains brackets is
 * left alone.
 */
function unwrapped(said) {
  const whole = /^\[([^\[\]]+)\]$/.exec(said);
  return whole ? whole[1] : said;
}

/** Text from a page or a model, drawn as text. */
function body(text) {
  const node = document.createElement("pre");
  node.className = "web-text";
  node.textContent = text;
  return node;
}

function note(text) {
  const node = document.createElement("p");
  node.className = "web-note";
  node.textContent = text;
  return node;
}
