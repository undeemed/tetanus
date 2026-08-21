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
}

class RpcFailure extends Error {
  constructor(error) {
    super(error.message || "the server said no");
    this.code = error.code;
  }
}

function connect() {
  if (!address) {
    settle("gone", "No server address. Open this page from `tetanus web`, or add `?ws=ws://host:port`.");
    return;
  }
  view.where.innerHTML = "";
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

  view.who.innerHTML = "";
  view.who.append("session ", strong(session), " · model ", strong(info.model));
  journal = info.path;

  // A fresh subscription from seq 0 is the whole transcript and every event
  // after it, on one ordered channel. Reading history separately would race
  // the first live push.
  clearInterval(counting);
  view.turns.innerHTML = "";
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

function drawn(event) {
  const data = event.data || {};
  switch (event.type) {
    case "session/start":
      break;
    case "turn/start":
      turnCard(data.turn);
      break;
    case "step/start":
      row("step", null, `step ${data.step}`);
      break;
    case "user/message":
      row("you", "you", data.content);
      break;
    case "assistant/chunk":
      if (data.chunk === "text") streaming("ai", "ai", data.delta);
      if (data.chunk === "reasoning") streaming("think", "think", data.delta);
      break;
    case "assistant/message": {
      const here = card || turnCard(undefined);
      here.tokens += (data.usage?.prompt_tokens || 0) + (data.usage?.completion_tokens || 0);
      const settled = here.live && here.live.kind === "ai" ? here.live.body : row("ai", "ai", "");
      settled.textContent = data.content;
      settled.parentElement.classList.remove("live");
      if (here.live) here.live.body.parentElement.classList.remove("live");
      here.live = null;
      break;
    }
    case "tool/call":
      (card || turnCard(undefined)).calls.push(data.id);
      row("call", null, `${data.name}  ${JSON.stringify(data.arguments)}`, "▸");
      break;
    case "tool/result": {
      const here = card || turnCard(undefined);
      // Pairing is by `call_id`, never by arrival order (contract §4.3.1). A
      // result that is not the newest open call says which call it answers.
      const newest = here.calls[here.calls.length - 1];
      const whose = data.call_id === newest ? "" : ` (for ${data.call_id})`;
      here.calls = here.calls.filter((id) => id !== data.call_id);
      const glyph = data.ok ? "✓" : "✗";
      row(data.ok ? "ok" : "bad", null, `${data.name}  ${data.content}${whose}`, glyph);
      break;
    }
    case "step/end":
      break;
    case "turn/end": {
      const here = card || turnCard(data.turn);
      const steps = data.steps === 1 ? "1 step" : `${data.steps} steps`;
      const parts = [`turn ${data.turn}`, data.stop_reason, steps];
      if (here.tokens > 0) parts.push(`${here.tokens} tokens`);
      const end = document.createElement("div");
      end.className = "end";
      end.textContent = parts.join(" · ");
      here.el.append(end);
      card = null;
      break;
    }
    default:
      row("other", null, `${event.type}  ${JSON.stringify(data)}`);
  }
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
  view.state.className = `state ${state}`;
  view.state.textContent = { live: "connected", busy: "working", gone: "offline" }[state] || "connecting";
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
