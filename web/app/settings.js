// What this deployment is configured to do, and who decided each of it.
//
// Upstream's `ui-settings-general`. Here it is `config.dump` drawn out - a
// call this build already answers, and the same answer `tetanus config`
// prints, so the terminal and the page cannot disagree about what a key is
// set to.
//
// # The layer is the point, not the value
//
// A reader opening this is almost never asking "what is `agent.max_steps`".
// They are asking "why is it that, and where do I change it", which is a
// question about the layer: a default is changed in a document, a document is
// changed in a file, an environment variable is changed in a shell, and a flag
// is changed on the command line that is already running. So the layer sits
// beside every value rather than being available on request, and the table is
// ordered by key, because a reader looks a key up first and wonders where it
// came from second.
//
// # The redaction sentinel is drawn, never interpreted
//
// §4.6: "`ConfigEntry.value` never carries a secret... A surface renders the
// sentinel as it renders any other value, and must not take it for the
// setting."
//
// Both halves are honoured. `<redacted>` is drawn as the value it is, and this
// page never treats it as one: it is not offered for copying, and the row says
// the value is withheld rather than letting a reader think the credential is
// literally the string `<redacted>` - which is exactly the mistake that
// sentence exists to prevent, and one somebody has certainly made by pasting
// it into a settings file.

import { pill } from "./primitives.js";

/** What the engine substitutes for a secret (§4.6). */
export const REDACTED = "<redacted>";

/** Where a value came from, in the words a reader would use to change it. */
const LAYERS = {
  default: { said: "built in", how: "set it in the settings document" },
  file: { said: "settings document", how: "edit the document this build read" },
  env: { said: "environment", how: "set it in the shell that starts the harness" },
  flag: { said: "command line", how: "the flag on the running command" },
};

/**
 * Draw the resolved configuration.
 *
 * `document` is the file the engine booted from, *if the caller knows it*.
 * `ConfigDumpResult` carries only `entries` on this contract, so a page over
 * the wire does not know - and says nothing rather than claiming none was
 * read, which would be a different and false statement. The terminal's own
 * table names the file because it resolved it itself before calling.
 *
 * When it is known it goes on the heading rather than on each row: which file
 * is a fact about the machine this ran on and not about any one key.
 */
export function settings(root, entries, { document: read } = {}) {
  root.replaceChildren();
  if (read) {
    const head = window.document.createElement("p");
    head.className = "list-empty";
    head.textContent = `Read from ${read}`;
    root.append(head);
  }

  if (!entries || entries.length === 0) {
    const none = window.document.createElement("p");
    none.className = "list-empty";
    // A build with no defaults resolved nothing, which is a fact rather than
    // an error, and saying so beats an empty table under a heading.
    none.textContent = "Nothing is set.";
    root.append(none);
    return;
  }

  const ordered = [...entries].sort((a, b) => a.key.localeCompare(b.key));
  for (const entry of ordered) root.append(row(entry));
}

function row(entry) {
  const root = window.document.createElement("div");
  root.className = "trace-step";

  const key = window.document.createElement("span");
  key.className = "trace-name";
  key.textContent = entry.key;
  root.append(key);

  const withheld = entry.value === REDACTED;
  const value = window.document.createElement("span");
  value.className = withheld ? "trace-took set-withheld" : "trace-took";
  // Values are printed the way a person writes them into a settings file, so a
  // string loses its JSON quotes and every other shape keeps its own spelling
  // - which is what still tells `true` the boolean apart from `"true"` the
  // string. The same rule the terminal's table follows.
  value.textContent = written(entry.value);
  root.append(value);

  const known = LAYERS[entry.layer];
  // A layer this build has never heard of is drawn as itself: §7.5 makes the
  // set growable, and a surface that showed nothing for a new one would hide
  // exactly the layer somebody just added.
  root.append(pill(known ? known.said : String(entry.layer)));

  if (withheld) {
    // Said in words, because the sentinel is a value and the fact that it
    // stands in for one is not. A reader who takes `<redacted>` for the
    // setting has been told the opposite of what is true.
    const note = window.document.createElement("span");
    note.className = "trace-note";
    note.textContent = "the value is withheld, not empty";
    root.append(note);
  }
  return root;
}

/** A JSON value as a person would have typed it into a document. */
export function written(value) {
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}
