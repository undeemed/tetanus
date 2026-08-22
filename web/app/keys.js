// Driving the page without a mouse.
//
// This is the half of a design system nobody screenshots. Upstream's
// `ui-primitives` spends a component on it - `Modal.tsx` traps focus and
// closes on Escape, `Menu.tsx` arrows between items - because a panel that can
// only be opened by pointing at it is a panel some readers cannot open at all.
//
// A `<dialog>` opened with `showModal` already gives most of it: focus is
// trapped inside, Escape closes it, and the rest of the page is inert. Two
// things it does not give, and they are the two things here:
//
//   - a way in from the keyboard, since a button is only reachable by tabbing
//     past everything before it; and
//   - a way back out to where the reader was, since the browser drops focus to
//     the body when a dialog closes, and a reader who opened a panel to look
//     something up wants to carry on typing.
//
// # One table, three readers
//
// The chord, the panel it opens, the button it opens it through and the words
// the footer says are one row in `CHORDS`. The footer line and the
// `aria-keyshortcuts` attribute are both written from that row rather than
// typed again in the HTML, because the failure this prevents is specific and
// silent: a hint that names a key nothing listens for, which a reader tries
// once and stops trusting the rest of the line.

/**
 * Every panel this page opens, and how a keyboard asks for it.
 *
 * `code` is the physical key and `letter` is the character it usually types.
 * Both are matched, and it is not belt-and-braces: Alt is a dead key on macOS,
 * where Alt+S types `ß` and never `s`, so matching the character alone loses
 * every Mac; and `code` is the QWERTY position, so matching the position alone
 * sends a Dvorak reader to the key their keyboard does not have the letter on.
 * Matching either means the chord is the one the reader's own keys describe.
 */
export const CHORDS = [
  { letter: "s", code: "KeyS", dialog: "sessions", opener: "sessions-open", says: "sessions" },
  { letter: "t", code: "KeyT", dialog: "trace", opener: "trace-open", says: "trace" },
  { letter: "m", code: "KeyM", dialog: "catalogue", opener: "catalogue-open", says: "models" },
  { letter: "w", code: "KeyW", dialog: "picker", opener: "pick", says: "workspace" },
];

/**
 * Which panel a keystroke asks for, or `null` for every keystroke that is not
 * one of these chords.
 *
 * Alt and nothing else. A bare letter has to reach the composer - a page that
 * took `s` for itself is a page you cannot type the word "session" into - and
 * Ctrl or Meta alongside it belongs to the browser: Ctrl+Alt+S is a system
 * shortcut on more than one desktop, and a page that swallows it has broken
 * something outside itself to save a keystroke inside.
 */
export function asked(event) {
  if (!event.altKey || event.ctrlKey || event.metaKey) return null;
  const typed = typeof event.key === "string" ? event.key.toLowerCase() : "";
  return CHORDS.find((panel) => panel.code === event.code || panel.letter === typed) ?? null;
}

/** The chords, worded for the line at the foot of the page. */
export function hint() {
  return CHORDS.map((panel) => `Alt+${panel.letter.toUpperCase()} ${panel.says}`).join(" · ");
}

/**
 * Wire the keyboard onto a document.
 *
 * `refocus` is called whenever a panel closes, however it closed - Escape, the
 * button, or a click on the backdrop - which is why it is hung on the dialog's
 * own `close` event and not on the paths that close it. There are four such
 * paths and a reader who found the fifth would be the one who lost their
 * place.
 */
export function keyboard(doc, refocus) {
  for (const panel of CHORDS) {
    doc.getElementById(panel.dialog)?.addEventListener("close", refocus);
    // The hint and the chord come from the same row, and so does what a
    // screen reader announces about the button.
    doc.getElementById(panel.opener)?.setAttribute("aria-keyshortcuts", `Alt+${panel.letter.toUpperCase()}`);
  }
  const line = doc.getElementById("chords");
  if (line) line.textContent = hint();

  doc.addEventListener("keydown", (event) => {
    const panel = asked(event);
    if (!panel) return;
    const dialog = doc.getElementById(panel.dialog);
    // A chord for a panel that is already open does nothing rather than
    // reopening it: `showModal` on an open dialog throws, and reloading a
    // list under a reader who is halfway down it is not what they asked for.
    if (!dialog || dialog.open) return;
    event.preventDefault();
    // Through the button, not around it. A panel opened by key is then a
    // panel that fetched its data, and the two ways in cannot come to mean
    // different things.
    doc.getElementById(panel.opener)?.click();
  });
}
