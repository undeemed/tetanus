import { jsonBlock, pill, stateDot, toast } from "./primitives.js";
import { toolCall, toolResult } from "./tools.js";
import { sessionList } from "./sidebar.js";
import { approvalRow, askCard } from "./questions.js";
import { trace, trajectory } from "./trajectory.js";
import { panel } from "./features.js";
import { markdown } from "./markdown.js";
import { models, tools } from "./catalogue.js";
import { settings } from "./settings.js";

// The client half of the panel: a JSON-RPC 2.0 client over the WebSocket
// carrier `tetanus serve` hosts, and a renderer for the events it pushes.
//
// It adds nothing to the contract. Four calls in order - `rpc.hello`,
// `session.create`, `session.subscribe`, then one `agent.prompt` per message
// typed - are exactly what `tetanus chat` makes in process, so the browser and
// the terminal drive the same engine the same way.
//
// Unknown event types are drawn, never dropped: the contract says the durable
// vocabulary grows, and a panel that hid what it did not recognise would hide
// the newest half of a turn.
//
// A dropped connection loses nothing a reader typed: the question goes back in
// the box, and the hint line counts down to the next dial rather than saying
// only that something failed.

const CLIENT = { name: "tetanus-web-chat", version: "0.1.0" };
const PROTOCOL = "1.0";
/** The first wait between dials, and the longest one, in milliseconds. */
const FIRST_WAIT = 1000;
const LONGEST_WAIT = 15000;

const at = (id) => document.getElementById(id);
const view = {
  where: at("where"), who: at("who"), state: at("state"), turns: at("turns"),
  scroll: at("scroll"), asked: at("asked"), send: at("send"), hint: at("hint"),
  form: at("composer"),
};

const query = new URLSearchParams(location.search);
// A page opened through `serve.py` is told its server. A page opened straight
// from disk is told by hand, which is what makes this file testable against
// any running `tetanus serve`.
// The boot manifest the host's index tap wrote, then the query, then
// nothing. The manifest is how a page that knows nothing about the assembly
// is told what the assembly bound; `?ws=` stays because a page opened
// against a server somebody else started is a real thing to do.
const manifest = window.TETANUS_BOOT || {};
const carrier = query.get("ws") || manifest.carrier || window.TETANUS_WS || "";
// A deployment that is not loopback needs a token, and it is in the reader's
// own URL rather than in the page: a stranger who can reach the port is served
// the same HTML and cannot dial the socket with it. The page passes on what it
// was opened with, and adds nothing when there is nothing to add.
// The reader's own URL first: a stated token is theirs and never ours to
// publish. The manifest's is the demonstration posture, where the deployment
// has said out loud that every reader of this page may dial.
const token = query.get("token") || manifest.token || "";
// The same secret on the other door. The bridge admits a caller exactly as the
// socket does, so a page that dialled with a token and posted without one
// would be refused halfway through its own work.
/** Every event this page has seen, in order, for the trace to fold. */
const seen = [];

/** The model this conversation is on, so the catalogue can mark it. */
let running = null;

let greeted = null;
const post = async (method, params) => {
  const url = "/api/" + method + (token ? "?token=" + encodeURIComponent(token) : "");
  const said = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(params || {}),
  });
  const body = await said.json();
  if (body.error) throw new Error(body.error.message || "the call failed");
  return body.result;
};
window.TETANUS_CALL = async (method, params) => {
  // The bridge is its own connection, so it wants its own handshake: greeting
  // the socket says nothing about the POSTs. Done once and remembered, because
  // the contract's rule is one hello per connection and not one per call.
  if (method !== "rpc.hello") {
    greeted ||= post("rpc.hello", {
      protocol_version: manifest.protocol || "1.0",
      client: { name: "tetanus-web", version: "0.1.0" },
    });
    await greeted;
  }
  return post(method, params);
};
const address = carrier && token
  ? carrier + (carrier.includes("?") ? "&" : "?") + "token=" + encodeURIComponent(token)
  : carrier;
let session = query.get("session") || null;

let journal = "";
let socket = null;
let pending = new Map();
let nextId = 1;
let busy = false;
// How long to wait before dialling again, doubling up to a cap. A panel left
// open on a server that has gone home should not dial it twice a minute all
// afternoon, and a server that comes back should be found in seconds.
let waiting = FIRST_WAIT;
// The countdown on the hint line, cleared whenever the panel stops waiting.
let counting = null;

/* ---------------------------------------------------------------- transport */

function call(method, params) {
  return new Promise((resolve, reject) => {
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      reject(new Error("not connected"));
      return;
    }
    const id = nextId++;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
  });
}

function received(frame) {
  if (frame.id !== undefined && frame.id !== null && pending.has(frame.id)) {
    const { resolve, reject } = pending.get(frame.id);
    pending.delete(frame.id);
    // An RpcError is an answer, not a transport failure: it carries the code
    // the contract fixes, and the panel shows both halves of it.
    frame.error ? reject(new RpcFailure(frame.error)) : resolve(frame.result);
    return;
  }
  if (frame.method === "session/event") drawn(frame.params.event);
  if (frame.method === "agent/status") reported(frame.params);
  // A server-to-client request, which is a thing this page must answer rather
  // than observe: §4.4.3 makes the engine block on `ui/ask`, and a client that
  // advertises the capability and stays silent is read as a denial.
  if (frame.method === "ui/ask" && frame.id !== undefined && frame.id !== null) {
    asked(frame.id, frame.params || {});
  }
}

/** Draw an ask, and answer the request it came in on. */
function asked(id, params) {
  const here = card || turnCard(undefined);
  let answered = false;
  const reply = (result) => {
    if (answered) return;
    answered = true;
    socket?.send(JSON.stringify({ jsonrpc: "2.0", id, result }));
  };
  here.el.append(askCard(params, reply));
  toBottom();
}

class RpcFailure extends Error {
  constructor(error) {
    super(error.message || "the server said no");
    this.code = error.code;
  }
}

function connect() {
  if (!address) {
    settle("gone", "No server address. Open this page from `tetanus serve --frontend`, or add `?ws=ws://host:port`.");
    return;
  }
  view.where.replaceChildren();
  view.where.append("carrier ", strong(address));
  settle("", "Connecting…");

  socket = new WebSocket(address);
  socket.onmessage = (frame) => received(JSON.parse(frame.data));
  socket.onopen = () => boot().catch(gave);
  socket.onerror = () => {};
  socket.onclose = () => {
    for (const { reject } of pending.values()) reject(new Error("connection closed"));
    pending.clear();
    ready(false);
    dialling(waiting);
    setTimeout(connect, waiting);
    waiting = Math.min(waiting * 2, LONGEST_WAIT);
  };
}

async function boot() {
  await call("rpc.hello", { protocol_version: PROTOCOL, client: CLIENT });
  // Connected and talking: the next drop starts its waiting from the bottom
  // again, because a server that answered once is worth dialling quickly.
  waiting = FIRST_WAIT;
  const info = await call("session.create", session ? { session_id: session } : {});
  session = info.session_id;
  // So a reload continues this conversation rather than starting another.
  const url = new URL(location.href);
  url.searchParams.set("session", session);
  history.replaceState(null, "", url);

  view.who.replaceChildren();
  running = info.model;
  view.who.append("session ", strong(session), " · model ", strong(info.model));
  journal = info.path;

  // A fresh subscription from seq 0 is the whole transcript and every event
  // after it, on one ordered channel. Reading history separately would race
  // the first live push.
  clearInterval(counting);
  view.turns.replaceChildren();
  card = null;
  await call("session.subscribe", { session_id: session, from_seq: 0 });
  if (!view.turns.firstElementChild) empty();
  ready(true);
  resting();
  view.asked.focus();
}

/* ----------------------------------------------------------------- renderer */

let card = null;

// An empty session says so. The alternative is a blank page that looks like a
// panel which failed to load the conversation it was pointed at.
function empty() {
  const said = document.createElement("p");
  said.className = "empty";
  said.textContent = "Nothing said yet. Ask something below.";
  view.turns.append(said);
}

function turnCard(turn) {
  const el = document.createElement("section");
  el.className = "turn";
  if (turn !== undefined) {
    const head = document.createElement("div");
    head.className = "head";
    head.textContent = `turn ${turn}`;
    el.append(head);
  }
  view.turns.append(el);
  card = { el, live: null, calls: [], tokens: 0 };
  return card;
}

// A row with a label puts it in the column `you` and `ai` share; a row with
// none starts where those labels start. That is the terminal's own layout, and
// the point of matching it is that a turn read in a browser and the same turn
// replayed at a terminal are the same shape, not merely the same words.
function row(kind, who, said, mark = "") {
  const here = card || turnCard(undefined);
  view.turns.firstElementChild?.classList.contains("empty") && view.turns.firstElementChild.remove();
  const el = document.createElement("div");
  el.className = `row ${kind}`;
  const body = document.createElement("span");
  body.className = "said";
  body.textContent = said;
  if (who !== null) {
    const label = document.createElement("span");
    label.className = "who";
    label.textContent = who;
    el.append(label);
  } else {
    el.classList.add("wide");
  }
  if (mark) {
    const glyph = document.createElement("b");
    glyph.className = "mark";
    glyph.textContent = `${mark} `;
    el.append(glyph);
  }
  el.append(body);
  here.el.append(el);
  toBottom();
  return body;
}

// Growing text goes into one row that keeps its caret until the settled
// message replaces it, so a reply reads as it arrives instead of in blocks.
function streaming(kind, who, delta) {
  const here = card || turnCard(undefined);
  if (!here.live || here.live.kind !== kind) {
    here.live = { kind, body: row(`${kind} live`, who, "") };
  }
  here.live.body.textContent += delta;
  toBottom();
}

/**
 * One event, drawn.
 *
 * Split by the question each group answers rather than kept as one switch:
 * what the conversation said, what a tool did, what a decision was, and where
 * a turn or step began and ended. They were one function with nine arms and
 * three of them holding real work, which is one function doing four jobs.
 */
function drawn(event) {
  // Kept whole. The trace is a fold over the journal, not a second stream to
  // maintain, so a fact that reaches the page reaches it too.
  seen.push(event);
  const data = event.data || {};
  const drew =
    boundary(event.type, data) ||
    conversation(event.type, data) ||
    toolWork(event.type, data) ||
    audited(event.type, data);
  // A durable type nobody has taken yet is drawn raw, which is what §4.3.2
  // asks of a surface and what makes a landed event visible on day one.
  if (!drew) row("other", null, `${event.type}  ${JSON.stringify(data)}`);
}

/** Where a turn or a step began and ended. */
function boundary(type, data) {
  switch (type) {
    case "session/start":
      return true;
    case "turn/start":
      turnCard(data.turn);
      return true;
    case "step/start":
      row("step", null, `step ${data.step}`);
      return true;
    case "step/end":
      return true;
    case "turn/end":
      return closing(data);
    default:
      return false;
  }
}

/** The closing line: the facts as pills, and the cap's own sentence. */
function closing(data) {
  const here = card || turnCard(data.turn);
  const steps = data.steps === 1 ? "1 step" : `${data.steps} steps`;
  const end = document.createElement("div");
  end.className = "end";
  // Facts as pills, and the reason coloured by whether the turn ended the way
  // it meant to - the same rule the terminal's closing line follows: only
  // `natural` is a turn that finished, and every other reason means the
  // answer is missing something the reader cannot see is missing.
  end.append(pill(`turn ${data.turn}`));
  end.append(pill(data.stop_reason, data.stop_reason === "natural" ? "ok" : "bad"));
  end.append(pill(steps));
  if (here.tokens > 0) end.append(pill(`${here.tokens} tokens`));
  if (data.stop_reason === "max-tokens") {
    const cut = document.createElement("div");
    cut.className = "hint";
    cut.textContent = "the answer stops where the cap did; ask again to go on";
    end.append(cut);
  }
  here.el.append(end);
  card = null;
  return true;
}

/** What was said, by either side. */
function conversation(type, data) {
  switch (type) {
    case "user/message":
      row("you", "you", data.content);
      return true;
    case "assistant/chunk":
      if (data.chunk === "text") streaming("ai", "ai", data.delta);
      if (data.chunk === "reasoning") streaming("think", "think", data.delta);
      return true;
    case "assistant/message":
      return answered(data);
    default:
      return false;
  }
}

/** A settled answer replaces the text that streamed into place. */
function answered(data) {
  const here = card || turnCard(undefined);
  here.tokens += (data.usage?.prompt_tokens || 0) + (data.usage?.completion_tokens || 0);
  const settled = here.live && here.live.kind === "ai" ? here.live.body : row("ai", "ai", "");
  // A settled answer is a document: the model writes markdown, and a reader
  // who has to parse fences by eye is reading the source of an answer rather
  // than the answer. The streamed text stays plain while it arrives, because
  // a half-written fence is not a document yet.
  settled.replaceChildren(markdown(data.content));
  settled.parentElement.classList.remove("live");
  if (here.live) here.live.body.parentElement.classList.remove("live");
  here.live = null;
  return true;
}

/** What a tool was asked to do, and what it answered. */
function toolWork(type, data) {
  if (type === "tool/call") {
    const here = card || turnCard(undefined);
    here.calls.push(data.id);
    const line = row("call", null, "", "▸");
    // The fold goes on the row rather than inside the text span: a disclosure
    // is a block, and a block inside an inline element is a shape the browser
    // has to guess at.
    line.parentElement.append(toolCall(data.name, data.arguments));
    return true;
  }
  if (type === "tool/result") {
    const here = card || turnCard(undefined);
    // Pairing is by `call_id`, never by arrival order (contract §4.3.1). A
    // result that is not the newest open call says which call it answers.
    const newest = here.calls[here.calls.length - 1];
    const whose = data.call_id === newest ? "" : ` (for ${data.call_id})`;
    here.calls = here.calls.filter((id) => id !== data.call_id);
    const line = row(data.ok ? "ok" : "bad", null, whose, data.ok ? "✓" : "✗");
    line.parentElement.append(toolResult(data.name, data.content, data.ok));
    return true;
  }
  return false;
}

/** The durable audit of a decision about whether a tool may run (§4.3.2). */
function audited(type, data) {
  if (!type.startsWith("approval/")) return false;
  const here = card || turnCard(undefined);
  const said = approvalRow(type, data);
  if (!said) return false;
  here.el.append(said);
  return true;
}

function reported(status) {
  if (status.session_id !== session) return;
  if (status.state === "running") {
    const where = status.step ? `turn ${status.turn} · step ${status.step}` : `turn ${status.turn}`;
    settle("busy", `Working: ${where}`);
  } else if (!busy) {
    resting();
  }
}

/* -------------------------------------------------------------------- chrome */

// Count the wait down rather than announcing it once.
//
// "Retrying…" is a panel saying it has not given up; it is not an answer to
// the question a reader actually has, which is whether to keep watching or go
// and restart the server. A number that moves answers it, and answers it again
// every second: at `0s` the dial is happening, and if the next line still says
// a wait then that dial failed too.
function dialling(wait) {
  clearInterval(counting);
  let left = Math.round(wait / 1000);
  const say = () => {
    settle(
      "gone",
      left > 0
        ? `Connection closed. Trying again in ${left}s.`
        : "Connection closed. Trying again now.",
    );
    left -= 1;
    if (left < 0) clearInterval(counting);
  };
  say();
  counting = setInterval(say, 1000);
}

function strong(text) {
  const el = document.createElement("b");
  el.textContent = text;
  return el;
}

function settle(state, said, bad = false) {
  const tone = { live: "ok", busy: "busy", gone: "bad" }[state] || "idle";
  const word = { live: "connected", busy: "working", gone: "offline" }[state] || "connecting";
  view.state.className = `state ${state}`;
  view.state.replaceChildren(stateDot(tone, word));
  view.hint.textContent = said;
  view.hint.className = bad ? "fault" : "";
}

// The journal path, when nothing more urgent is owed. It is the one fact about
// a chat that is nowhere else on the screen, and the one a reader needs to
// replay this conversation or resume it with `tetanus chat -s <path>` - which
// is why `tetanus chat` prints it before the first question too.
function resting() {
  settle("live", journal ? `journal ${journal}` : "Ready.");
}

function ready(can) {
  view.asked.disabled = !can;
  view.send.disabled = !can;
}

function toBottom() {
  // Only when the reader is already at the end. Scrolling away is how someone
  // reads what was said earlier, and a live turn must not undo it.
  const near = view.scroll.scrollHeight - view.scroll.scrollTop - view.scroll.clientHeight < 120;
  if (near) view.scroll.scrollTop = view.scroll.scrollHeight;
}

function gave(failure) {
  const code = failure.code !== undefined ? ` (code ${failure.code})` : "";
  // The failure goes on the transcript in any case: it is part of the record
  // of the conversation, and it is what a reader scrolls back to.
  row("fault", null, failure.message + code, "!");
  // The hint line is one line, and while the panel is offline the useful half
  // is the countdown `onclose` has just started there. A call that failed
  // because the socket went away has said what it has to say on the row above.
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    return;
  }
  // A failure before there is a session is a failure to open the conversation
  // at all, and the reader is holding an address that will fail again. Say the
  // way out, because there is no session here for them to type into.
  const out = journal ? "" : " · reload without ?session= to start a new one";
  settle("live", failure.message + code + out, true);
}

async function ask(said) {
  busy = true;
  ready(false);
  view.asked.value = "";
  grow();
  try {
    await call("agent.prompt", { session_id: session, content: said });
    resting();
  } catch (failure) {
    gave(failure);
    // The question goes back where it was typed. A prompt that never reached
    // the engine is a question the reader still has, and a panel that ate it
    // is one they have to remember what they were asking - after watching the
    // connection drop, which is when nobody remembers anything.
    view.asked.value = said;
    grow();
  } finally {
    busy = false;
    if (socket && socket.readyState === WebSocket.OPEN) ready(true);
    view.asked.focus();
  }
}

function grow() {
  view.asked.style.height = "auto";
  view.asked.style.height = `${Math.min(view.asked.scrollHeight, 180)}px`;
}

view.form.addEventListener("submit", (typed) => {
  typed.preventDefault();
  const said = view.asked.value.trim();
  if (said && !busy) ask(said);
});
view.asked.addEventListener("input", grow);
view.asked.addEventListener("keydown", (key) => {
  if (key.key === "Enter" && !key.shiftKey) {
    key.preventDefault();
    view.form.requestSubmit();
  }
});

connect();

// ---------------------------------------------------------------------------
// The workspace chooser.
//
// Upstream's browse backend exists because a remote client cannot reach an OS
// dialog, and this is the client half of that seam: it draws a chooser out of
// `host.listDirectory` and `host.createDirectory` and nothing else. Two panes,
// the level and its parent, because stepping back should not make the view
// collapse - a chooser that shows one level at a time loses the reader's place
// every time they go up.
//
// Everything the host said is drawn and nothing is inferred: `hidden` is a
// flag on the row, not a name this page re-derives, and `truncated` is said
// out loud rather than quietly shown as a short level.
// ---------------------------------------------------------------------------

const picker = document.getElementById("picker");
const crumbsBar = document.getElementById("crumbs");
const panes = document.getElementById("panes");
const parentPane = document.getElementById("parent");
const herePane = document.getElementById("here");
const pickNote = document.getElementById("picknote");
const dots = document.getElementById("dots");

/** Where the chooser is, and what the host said about it. */
let standing = { path: null, listing: null };

/** Ask the host for a level and draw it, keeping the reader's place. */
async function walk(path) {
  say("");
  try {
    const listing = await window.TETANUS_CALL("host.listDirectory", path ? { path } : {});
    standing = { path: listing.path, listing };
    // The parent leg is best-effort: a level whose parent cannot be read is
    // still a level worth showing, and the pane simply is not there.
    let above = null;
    const parent = listing.crumbs.length > 1 ? listing.crumbs[listing.crumbs.length - 2] : null;
    if (parent) {
      try {
        above = await window.TETANUS_CALL("host.listDirectory", { path: parent.path });
      } catch { above = null; }
    }
    draw(listing, above);
  } catch (err) {
    say(err.message, true);
  }
}

/** One level, and its parent beside it when there is one. */
function draw(listing, above) {
  crumbsBar.replaceChildren(...listing.crumbs.map((crumb) => {
    const jump = document.createElement("button");
    jump.className = "row";
    jump.style.width = "auto";
    jump.textContent = crumb.name;
    jump.onclick = () => walk(crumb.path);
    return jump;
  }));

  panes.classList.toggle("alone", !above);
  parentPane.hidden = !above;
  if (above) {
    // The level we came from is marked in its parent, so the reader can see
    // where they are standing rather than work it out from the crumbs.
    fill(parentPane, above, listing.path);
  }
  fill(herePane, listing, null);

  const cut = listing.truncated ? " · the level is longer than this" : "";
  say(`${listing.entries.length} directories${cut}`);
}

/** Draw one pane's rows, under the reader's own hidden-files choice. */
function fill(pane, listing, mark) {
  const rows = listing.entries.filter((row) => dots.checked || !row.hidden);
  if (rows.length === 0) {
    const empty = document.createElement("div");
    empty.className = "row dot";
    empty.textContent = listing.entries.length ? "· every entry here is hidden" : "· nothing here";
    pane.replaceChildren(empty);
    return;
  }
  pane.replaceChildren(...rows.map((row) => {
    const go = document.createElement("button");
    go.className = "row" + (row.hidden ? " dot" : "") + (row.path === mark ? " here" : "");
    go.textContent = row.name;
    go.onclick = () => walk(row.path);
    return go;
  }));
}

/** A sentence under the panes: what the level is, or what went wrong. */
function say(text, bad) {
  pickNote.textContent = text;
  pickNote.classList.toggle("fault", Boolean(bad));
}

document.getElementById("pick").onclick = () => {
  picker.showModal();
  // No path: the host opens at the account's home, which is where a chooser
  // that was not told where to start should start.
  walk(standing.path);
};
document.getElementById("pickclose").onclick = () => picker.close();
dots.onchange = () => { if (standing.listing) walk(standing.path); };

document.getElementById("newdir").onclick = async () => {
  const name = prompt("New folder in " + (standing.path || "?"));
  if (!name) return;
  try {
    await window.TETANUS_CALL("host.createDirectory", { path: standing.path, name });
    await walk(standing.path);
  } catch (err) {
    // The host's three failures each say what to do next, so its sentence is
    // shown rather than replaced with one of this page's own.
    say(err.message, true);
  }
};

// ---------------------------------------------------------------------------
// The session list.
//
// Read when it is opened and after a turn settles, which is when it can have
// changed: there is no push for "a session was created", because
// `session/event` is per-session and a list is not a session, and polling on a
// timer would be a request every few seconds for an answer that changes twice
// an hour.
// ---------------------------------------------------------------------------

const sessionsDialog = document.getElementById("sessions");
const sessionsList = document.getElementById("session-list");

async function showSessions() {
  sessionsDialog.showModal();
  try {
    const answered = await window.TETANUS_CALL("session.list", {});
    sessionList(sessionsList, answered.sessions || [], {
      current: session,
      onPick: (id) => {
        // A conversation is named in the query, so a reload continues it -
        // the same rule the page already follows for the one it is in.
        const url = new URL(location.href);
        url.searchParams.set("session", id);
        location.href = url.toString();
      },
    });
  } catch (err) {
    sessionsList.replaceChildren();
    const said = document.createElement("p");
    said.className = "list-empty";
    said.textContent = err.message;
    sessionsList.append(said);
  }
}

document.getElementById("sessions-open").onclick = showSessions;
document.getElementById("sessions-close").onclick = () => sessionsDialog.close();

// ---------------------------------------------------------------------------
// The run's path.
// ---------------------------------------------------------------------------

const traceDialog = document.getElementById("trace");
const traceBody = document.getElementById("trace-body");

document.getElementById("trace-open").onclick = () => {
  traceDialog.showModal();
  // What the run is working toward sits above what it did: a goal is the
  // reason for the path, and reading the path first is reading the answer
  // before the question.
  panel(document.getElementById("standing"), seen);
  trace(traceBody, trajectory(seen));
};
document.getElementById("trace-close").onclick = () => traceDialog.close();

// ---------------------------------------------------------------------------
// What this deployment can run.
// ---------------------------------------------------------------------------

const catalogueDialog = document.getElementById("catalogue");

document.getElementById("catalogue-open").onclick = async () => {
  catalogueDialog.showModal();
  const shown = document.getElementById("catalogue-models");
  const listed = document.getElementById("catalogue-tools");
  try {
    const [providers, toolset] = await Promise.all([
      window.TETANUS_CALL("catalog.models", {}),
      window.TETANUS_CALL("catalog.tools", {}),
    ]);
    models(shown, providers.providers || [], {
      current: running,
      onStart: (provider, chosen) => {
        // A new conversation, because that is what this contract offers:
        // `session.create` takes a route and a model, and nothing moves a
        // running session onto another one.
        const url = new URL(location.href);
        url.searchParams.delete("session");
        url.searchParams.set("provider", provider);
        url.searchParams.set("model", chosen);
        location.href = url.toString();
      },
    });
    tools(listed, toolset.tools || []);
    // The same dialog: what this build can run, and what it is configured to
    // do with it. Two calls, one question - "what is this deployment".
    const dumped = await window.TETANUS_CALL("config.dump", {});
    settings(document.getElementById("catalogue-settings"), dumped.entries || [], {
      document: dumped.document,
    });
  } catch (err) {
    shown.replaceChildren();
    const said = document.createElement("p");
    said.className = "list-empty";
    said.textContent = err.message;
    shown.append(said);
  }
};
document.getElementById("catalogue-close").onclick = () => catalogueDialog.close();
