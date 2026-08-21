// The session list.
//
// Upstream's `ui-sidebar` is the standing list of conversations: pick one and
// the thread beside it changes. This is that list, drawn from `session.list`,
// which is a call this build already serves - so unlike the tool views, every
// field here has data behind it today.
//
// # What a row says, and why that much
//
// `SessionInfo` carries the id, the journal's path, the provider and model,
// when it was created, the last seq, the live state, and `title` - which the
// contract describes as "the session's first user message, truncated by the
// engine". The row shows the title, because that is what a person recognises
// a conversation by; the model and the age, because those are what they
// choose between when two look alike; the state, because a session with a
// turn running is not one to open blind; and nothing else, because a list
// that shows everything is a list nobody scans.
//
// The id is on the row as a title rather than as text. It is what a reader
// needs when they are talking to somebody else about a session, and never what
// they are looking for while choosing one.
//
// # Why the list is asked for rather than watched
//
// There is no push for "a session was created": `session/event` is per-session
// and a list is not a session. So the list is read when it is opened and after
// a turn settles, which is when it can have changed. Polling it on a timer
// would be a request every few seconds for an answer that changes twice an
// hour.

import { pill, stateDot } from "./primitives.js";

/**
 * Draw the list into `root`.
 *
 * `onPick` is handed the session id. `current` is the one being read, marked
 * so a reader can see where they are - a list with no current row makes
 * somebody click their own conversation to find out if they are in it.
 */
export function sessionList(root, sessions, { current, onPick } = {}) {
  root.replaceChildren();
  if (sessions.length === 0) {
    const none = document.createElement("p");
    none.className = "list-empty";
    none.textContent = "No conversations yet.";
    root.append(none);
    return;
  }
  // Newest first. A list of conversations is read from the top, and the one a
  // person wants is nearly always the one they had last.
  const ordered = [...sessions].sort((a, b) => (b.created_time ?? 0) - (a.created_time ?? 0));
  for (const session of ordered) {
    root.append(row(session, current, onPick));
  }
}

/** One conversation. */
function row(session, current, onPick) {
  const here = session.session_id === current;
  const root = document.createElement("button");
  root.type = "button";
  root.className = "list-row" + (here ? " list-here" : "");
  // The id is what a reader quotes to somebody else and never what they scan
  // for, so it is the row's title rather than its text.
  root.title = session.session_id;
  if (here) root.setAttribute("aria-current", "true");

  const line = document.createElement("span");
  line.className = "list-said";
  // The engine truncates this; an absent one is a session nobody has spoken
  // in yet, which is a fact rather than a blank.
  line.textContent = session.title || "nothing said yet";
  if (!session.title) line.classList.add("list-quiet");

  const facts = document.createElement("span");
  facts.className = "list-facts";
  facts.append(pill(session.model || "unknown model"));
  facts.append(pill(ago(session.created_time)));
  // A session with a turn in flight is not one to open blind, so the state
  // the engine reports is on the row rather than discovered on arrival.
  if (session.state && session.state !== "idle") {
    facts.append(stateDot(session.state === "running" ? "busy" : "idle", session.state));
  }
  if (here) facts.append(stateDot("ok", "open"));

  root.append(line, facts);
  if (onPick) root.addEventListener("click", () => onPick(session.session_id));
  return root;
}

/**
 * How long ago, in the units a person would say it in.
 *
 * Not a timestamp: a reader choosing between conversations is asking "which
 * one was I just in", and `1787351280325` does not answer that. The exact time
 * is one hover away on the row's title if it is ever wanted.
 */
export function ago(time, now = Date.now()) {
  if (!time) return "unknown";
  const seconds = Math.max(0, Math.round((now - time) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
}
