// A model's answer, rendered as a document - safely.
//
// Upstream's `MarkdownText` is GFM plus KaTeX plus shiki. This is the subset
// that needs no dependency, and it keeps the part of upstream's renderer that
// is not about looks at all: what it refuses to do with untrusted text.
//
// # The threat, stated plainly
//
// Everything here was written by a model, which is to say by whatever the
// model was persuaded to write - a tool result, a fetched page, a file in the
// repository can all reach it. So this renderer:
//
// - **builds elements, never markup.** There is no `innerHTML` anywhere in it,
//   which means no parser confusion, no mutation XSS, and nothing to audit
//   about sanitiser bypasses. Upstream states the same rule as "omits raw
//   HTML"; building elements is how that becomes true rather than intended.
// - **neutralises every link it cannot vouch for.** `http`, `https` and
//   `mailto` become anchors with `rel="noopener noreferrer nofollow"` and
//   `target="_blank"`. Everything else - `javascript:`, `data:`, `file:`, a
//   relative path - keeps its text and gets no anchor at all, which is
//   upstream's behaviour and the only safe default: a link the page cannot
//   check is a link the reader should not be able to click by accident.
// - **draws no images.** Upstream renders absolute HTTP(S) images without a
//   referrer; this build has no reason to fetch anything a model names, and an
//   image tag is a request to a server of the model's choosing. The alt text
//   is kept, so nothing is lost but the fetch.
//
// # What it renders
//
// Fenced code with a language banner, headings, unordered and ordered lists,
// block quotes, horizontal rules, paragraphs; inline code, bold, italic,
// links and autolinks. Tables, footnotes, task lists and math are not here -
// each is a slice, and none of them changes the security posture above.

import { button } from "./primitives.js";

/** Schemes a link may keep. Everything else keeps its text and loses the link. */
const SAFE = ["http:", "https:", "mailto:"];

/**
 * Render markdown into a fresh element.
 *
 * The whole document each time: this build's answers arrive complete, and
 * upstream's incremental cache exists for streaming, which the page does with
 * plain text until the message settles.
 */
export function markdown(text) {
  const root = document.createElement("div");
  root.className = "md";
  for (const block of blocks(String(text ?? ""))) root.append(block);
  return root;
}

/**
 * Every kind of block, in the order they are tried.
 *
 * A table rather than a chain of `if`s inside one loop: each entry answers one
 * question - "does this line start my kind of block, and if so where does it
 * end" - and adding a kind is an entry rather than another branch in a
 * function that already had six.
 *
 * Order matters and is the only thing the table encodes: a fence is tried
 * before a rule, because ``` is not three dashes but a lazier reader of both
 * would have to say so.
 */
const KINDS = [fenced, blank, rule, heading, quoted, listed];

/** Split source into block elements. */
function* blocks(text) {
  const lines = text.split("\n");
  let at = 0;
  while (at < lines.length) {
    const read = KINDS.reduce((found, kind) => found ?? kind(lines, at), null) ?? paragraph(lines, at);
    if (read.node) yield read.node;
    // Every reader returns where the next block starts, and every one of them
    // moves: a reader that returned its own line would spin here for ever.
    at = read.at > at ? read.at : at + 1;
  }
}

/**
 * A fence runs to its closing fence, or to the end - an unclosed fence is a
 * model that stopped mid-answer, and the right rendering is the code it did
 * write rather than the rest of the document as code.
 */
function fenced(lines, at) {
  const fence = /^```(\w*)\s*$/.exec(lines[at]);
  if (!fence) return null;
  const said = [];
  let to = at + 1;
  while (to < lines.length && !/^```\s*$/.test(lines[to])) said.push(lines[to++]);
  return { node: codeBlock(said.join("\n"), fence[1]), at: to + 1 };
}

/** Blank lines separate blocks and draw nothing themselves. */
function blank(lines, at) {
  return /^\s*$/.test(lines[at]) ? { node: null, at: at + 1 } : null;
}

function rule(lines, at) {
  if (!/^ {0,3}(-{3,}|\*{3,}|_{3,})\s*$/.test(lines[at])) return null;
  return { node: document.createElement("hr"), at: at + 1 };
}

function heading(lines, at) {
  const hit = /^(#{1,6})\s+(.*)$/.exec(lines[at]);
  if (!hit) return null;
  const node = document.createElement(`h${hit[1].length}`);
  inline(node, hit[2]);
  return { node, at: at + 1 };
}

/** A quote is a block of its own, so its contents are blocks too. */
function quoted(lines, at) {
  if (!/^\s*>\s?/.test(lines[at])) return null;
  const said = [];
  let to = at;
  while (to < lines.length && /^\s*>\s?/.test(lines[to])) said.push(lines[to++].replace(/^\s*>\s?/, ""));
  const node = document.createElement("blockquote");
  for (const block of blocks(said.join("\n"))) node.append(block);
  return { node, at: to };
}

function listed(lines, at) {
  const bullet = /^\s*([-*+]|\d+[.)])\s+/.exec(lines[at]);
  if (!bullet) return null;
  const node = document.createElement(/\d/.test(bullet[1]) ? "ol" : "ul");
  let to = at;
  while (to < lines.length) {
    const item = /^\s*(?:[-*+]|\d+[.)])\s+(.*)$/.exec(lines[to]);
    if (!item) break;
    const li = document.createElement("li");
    inline(li, item[1]);
    node.append(li);
    to += 1;
  }
  return { node, at: to };
}

/** Whatever is left, to the next blank line or block opener. */
function paragraph(lines, at) {
  const said = [];
  let to = at;
  while (to < lines.length && !/^\s*$/.test(lines[to]) && !/^```/.test(lines[to])) said.push(lines[to++]);
  const node = document.createElement("p");
  inline(node, said.join(" "));
  return { node, at: to };
}

/**
 * A fenced block: the language, a copy control, and the code as text.
 *
 * Upstream's `CodeBlock` has a language banner and a copy control too, and the
 * copy is the part that earns its place - a reader who wants a command wants
 * it in their shell, and selecting pre-formatted text with a mouse is how
 * people paste half a line.
 */
export function codeBlock(code, language) {
  const root = document.createElement("figure");
  root.className = "code";
  const head = document.createElement("figcaption");
  head.className = "code-head";
  const named = document.createElement("span");
  named.textContent = language || "text";
  head.append(named);
  head.append(
    button("Copy", {
      title: "copy this block",
      onClick: async (event) => {
        try {
          await navigator.clipboard.writeText(code);
          event.target.textContent = "Copied";
          // Upstream restores its own copy affordance after a second; the same
          // number, and the same reason - a control stuck saying "Copied" is a
          // control a reader stops trusting.
          setTimeout(() => {
            event.target.textContent = "Copy";
          }, 1000);
        } catch {
          event.target.textContent = "Copy failed";
        }
      },
    }),
  );
  const body = document.createElement("pre");
  const text = document.createElement("code");
  // The one place it matters most: code from a model, set as text.
  text.textContent = code;
  body.append(text);
  root.append(head, body);
  return root;
}

/**
 * Inline markup, appended into `parent`.
 *
 * One pass, longest match first, and every branch appends an element or text -
 * there is no string of markup assembled anywhere, so there is nothing for a
 * parser to be confused by.
 */
export function inline(parent, text) {
  const pattern =
    /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(\[[^\]]+\]\([^)\s]+\))|(<https?:\/\/[^>\s]+>)|(https?:\/\/[^\s<]+)/;
  let rest = String(text ?? "");
  while (rest.length > 0) {
    const hit = pattern.exec(rest);
    if (!hit) {
      parent.append(document.createTextNode(rest));
      return;
    }
    if (hit.index > 0) parent.append(document.createTextNode(rest.slice(0, hit.index)));
    const [whole] = hit;
    if (whole.startsWith("`")) {
      const code = document.createElement("code");
      code.textContent = whole.slice(1, -1);
      parent.append(code);
    } else if (whole.startsWith("**")) {
      const strong = document.createElement("strong");
      strong.textContent = whole.slice(2, -2);
      parent.append(strong);
    } else if (whole.startsWith("*")) {
      const em = document.createElement("em");
      em.textContent = whole.slice(1, -1);
      parent.append(em);
    } else if (whole.startsWith("[")) {
      const split = /^\[([^\]]+)\]\(([^)\s]+)\)$/.exec(whole);
      parent.append(link(split[2], split[1]));
    } else if (whole.startsWith("<")) {
      parent.append(link(whole.slice(1, -1), whole.slice(1, -1)));
    } else {
      parent.append(link(whole, whole));
    }
    rest = rest.slice(hit.index + whole.length);
  }
}

/**
 * An anchor for a link this page can vouch for, and plain text for one it
 * cannot.
 *
 * The check is on the parsed scheme, not on the text: `JaVaScRiPt:` and
 * `java\tscript:` are the same trick, and a page comparing prefixes catches
 * neither.
 */
export function link(href, said) {
  let scheme = null;
  try {
    scheme = new URL(href, "https://tetanus.invalid/").protocol;
  } catch {
    scheme = null;
  }
  // A relative path parses against the base and comes out `https:`, so it has
  // to be refused by shape rather than by scheme: this page has no pages of
  // its own for a model to link to.
  const absolute = /^[a-z][a-z0-9+.-]*:/i.test(href.trim());
  if (!absolute || !SAFE.includes(scheme)) {
    const plain = document.createElement("span");
    plain.className = "md-inert";
    // The text survives; only the link is withheld. A reader can still read
    // what the model wrote and decide for themselves.
    plain.textContent = said;
    return plain;
  }
  const anchor = document.createElement("a");
  anchor.href = href;
  anchor.textContent = said;
  anchor.target = "_blank";
  anchor.rel = "noopener noreferrer nofollow";
  return anchor;
}
