// The atoms every other view is built from.
//
// Upstream's `ui-primitives` is the design system the other thirty modules
// import, and it is first in the build order for the reason its own README
// gives: everything else sits on it, so doing it late means rewriting. This is
// that layer for a page with no framework - plain functions that return
// elements, with the styling in `tokens.css` and `primitives.css` rather than
// in any of them.
//
// # Why functions and not classes or components
//
// A primitive here has one job: return an element that already looks right and
// already behaves right. There is no state to own, because the views own their
// own state and hand it in. That keeps this file testable without a browser -
// `target/probe-panel.mjs` builds a document and calls these directly - and
// keeps a reader from having to learn a component model to read a page.
//
// # The rules every atom follows
//
// - **Text is set with `textContent`, never with `innerHTML`.** Everything
//   drawn here comes from a model, a tool or a filesystem, and exactly one of
//   those is trusted. The markdown family is the single exception and it
//   builds elements rather than markup.
// - **A control is a real control.** A `button` rather than a `div` with a
//   click handler, so the keyboard reaches it and a screen reader says what it
//   is. Upstream makes the same point about its hover card exposing button
//   semantics; the cheap version of that discipline is not inventing controls.
// - **State is a class, not a colour.** A caller says `ok`, `busy`, `bad` or
//   `idle`; what those look like is the token file's business.

/** The states a surface can report, and nothing outside this set. */
export const STATES = ["ok", "busy", "bad", "idle"];

/** Make an element, set its class, and fill it with text. */
function make(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

/**
 * A small round mark that says what something is doing.
 *
 * The word goes beside it, always. A dot alone encodes meaning in colour, and
 * a reader who cannot tell this green from this yellow then has a page with no
 * state on it at all.
 */
export function stateDot(state, label) {
  const known = STATES.includes(state) ? state : "idle";
  const root = make("span", `dot dot-${known}`);
  root.setAttribute("role", "status");
  const mark = make("span", "dot-mark");
  mark.setAttribute("aria-hidden", "true");
  root.append(mark, make("span", "dot-label", label ?? known));
  return root;
}

/**
 * A short fact on a coloured ground: a stop reason, a token count, a tool
 * name.
 *
 * Pills carry facts and never actions. A pill a reader can click is a button
 * that has been drawn to look like a label, which is how a page teaches people
 * to click everything.
 */
export function pill(text, tone) {
  const root = make("span", "pill" + (tone ? ` pill-${tone}` : ""), text);
  return root;
}

/**
 * A control. `kind` is `primary` for the one action a view is about, and
 * absent for the rest.
 */
export function button(text, { kind, onClick, title } = {}) {
  const root = make("button", "btn" + (kind ? ` btn-${kind}` : ""), text);
  root.type = "button";
  if (title) root.title = title;
  if (onClick) root.addEventListener("click", onClick);
  return root;
}

/** A single-line field, labelled for the people who cannot see the label. */
export function input({ value, placeholder, label, onInput } = {}) {
  const root = make("input", "field");
  root.type = "text";
  if (value) root.value = value;
  if (placeholder) root.placeholder = placeholder;
  if (label) root.setAttribute("aria-label", label);
  if (onInput) root.addEventListener("input", () => onInput(root.value));
  return root;
}

/**
 * A row that opens to show what is under it.
 *
 * `<details>` rather than a div and a click handler, because the browser
 * already knows how this behaves: the keyboard opens it, a screen reader
 * announces it, and find-in-page can open it to show a match.
 *
 * `open` is the caller's decision and not this atom's. A model's reasoning is
 * folded because it is long and secondary; a failure is open because it is the
 * thing the reader is looking for.
 */
export function disclosure(summaryText, { open = false, tone } = {}) {
  const root = make("details", "disclose" + (tone ? ` disclose-${tone}` : ""));
  root.open = Boolean(open);
  const head = make("summary", "disclose-head");
  head.append(make("span", "disclose-mark", "›"), make("span", null, summaryText));
  const body = make("div", "disclose-body");
  root.append(head, body);
  root.body = body;
  return root;
}

/**
 * A banner that says one thing and goes away.
 *
 * Upstream holds it three seconds and fades over one; the same numbers, and
 * the same reason for `role="alert"` - a reader using a screen reader gets
 * told, rather than being the only person who misses it.
 *
 * Under `prefers-reduced-motion` the slide is dropped and only the fade
 * remains, which is upstream's behaviour and the right one: motion is the part
 * that makes people ill, and a page that keeps it anyway has asked and then
 * ignored the answer.
 */
export function toast(text, { tone, onDone, hold = 3000 } = {}) {
  const root = make("div", "toast" + (tone ? ` toast-${tone}` : ""), text);
  root.setAttribute("role", "alert");
  const still = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches;
  if (still) root.classList.add("toast-still");
  setTimeout(() => {
    root.classList.add("toast-going");
    setTimeout(() => {
      root.remove();
      onDone?.();
    }, 1000);
  }, hold);
  return root;
}

/**
 * A dialog. Modal, because everything this page opens in one is a question it
 * needs answered before it can go on.
 *
 * Escape closes it: the browser does that for a `<dialog>`, and a page that
 * re-implemented it would get it subtly wrong.
 */
export function modal(titleText) {
  const root = make("dialog", "modal");
  const head = make("div", "modal-head");
  head.append(make("h2", "modal-title", titleText));
  const body = make("div", "modal-body");
  const foot = make("div", "modal-foot");
  root.append(head, body, foot);
  root.body = body;
  root.foot = foot;
  return root;
}

/**
 * A label that appears on hover and on focus.
 *
 * On focus as well as hover, because a tooltip that only answers a mouse is a
 * tooltip that does not exist for anybody using a keyboard. `title` is the
 * cheap half - the browser draws it and assistive technology reads it - and
 * the class is what lets the page style one later without changing callers.
 */
export function tipped(node, text) {
  node.title = text;
  node.classList.add("tipped");
  if (!node.getAttribute("aria-label") && !node.textContent) {
    node.setAttribute("aria-label", text);
  }
  return node;
}

/**
 * A value from the wire, drawn as a tree a reader can fold.
 *
 * Tool arguments and results are JSON and arrive from a model or a tool, so
 * they are exactly the data this page must never treat as markup. Every scalar
 * goes in with `textContent`, and the shape is built from elements.
 *
 * Depth is bounded. A structure that nests deeper than this is a structure
 * nobody is reading in a transcript, and rendering all of it is how a page
 * hangs on a tool that returned a graph.
 */
export function jsonTree(value, { depth = 0, max = 6 } = {}) {
  if (depth >= max) return make("span", "json-deep", "…");
  if (value === null) return make("span", "json-null", "null");
  const kind = Array.isArray(value) ? "array" : typeof value;
  switch (kind) {
    case "array":
    case "object": {
      const entries = Array.isArray(value)
        ? value.map((item, at) => [String(at), item])
        : Object.entries(value);
      if (entries.length === 0) {
        return make("span", "json-empty", Array.isArray(value) ? "[]" : "{}");
      }
      const root = make("div", "json-node");
      for (const [key, item] of entries) {
        const row = make("div", "json-row");
        row.append(make("span", "json-key", key));
        row.append(jsonTree(item, { depth: depth + 1, max }));
        root.append(row);
      }
      return root;
    }
    case "number":
    case "boolean":
      return make("span", `json-${kind}`, String(value));
    default:
      return make("span", "json-string", String(value));
  }
}

/** The same, wrapped in a fold so a large value does not take the page. */
export function jsonBlock(value, summaryText = "arguments") {
  const root = disclosure(summaryText, { open: false });
  root.body.append(jsonTree(value));
  return root;
}

/**
 * Text a person wrote, drawn as text.
 *
 * Upstream keeps `MessageText` separate from its markdown renderer for this
 * reason: a user's message is not a document to interpret. Someone who types
 * `*hello*` meant the asterisks.
 */
export function messageText(text) {
  return make("div", "said", text);
}
