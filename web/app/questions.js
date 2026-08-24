// Being asked something, and the audit of what was decided.
//
// Upstream's `ui-user-questions` is the approval and question surface. Here it
// is two halves that meet in the same place:
//
// - **The live ask.** `ui/ask` is a server-to-client *request* (§4.4.3): the
//   engine blocks on it, because a tool cannot proceed until a person decides.
//   A client that advertises the `ui.ask` capability and then does not answer
//   is treated as a denial, so a surface that draws this must always finish
//   the exchange - including when the reader closes the card.
// - **The audit on the journal.** `approval/asked`, `approval/decided`,
//   `approval/policy`, `question/asked` and `question/answered` are durable
//   (§4.3.2), so a conversation opened tomorrow still shows what was asked and
//   what was decided. That half needs no live socket at all.
//
// # What is real here and what is not
//
// The shapes are this tree's own: `Question`, `QuestionOption`, `Answer` and
// `ApprovalOutcome` are in `crates/protocol`, and the event vocabulary is in
// §4.3.2 of the contract in this repository. What is not here is an engine
// that emits them - the gate that asks lives on the fs lane's branch. So this
// is built against published shapes rather than against a guess, and the
// probe drives it with the frames the contract fixes.
//
// # Why the fail-closed rule is drawn, not just obeyed
//
// `ApprovalOutcome::grants` says only `allowed-once` lets a call run - not
// `rejected`, not `cancelled`, not `unavailable`, and not a word this build
// has never seen. The card says which of those happened rather than saying
// "denied", because a reader who was never asked (`unavailable`) and a reader
// who said no are two different situations with two different fixes.

import { button, disclosure, pill } from "./primitives.js";

/** The one outcome that lets a call run, per §4.4.7. */
export const GRANTS = "allowed-once";

/** How each outcome is worded and toned. An unknown one is drawn as itself. */
const OUTCOMES = {
  "allowed-once": { said: "allowed, once", tone: "ok" },
  rejected: { said: "rejected", tone: "bad" },
  cancelled: { said: "withdrawn before it was answered", tone: undefined },
  unavailable: { said: "nobody could answer it", tone: "bad" },
};

/**
 * Draw a live `ui/ask` and call `onAnswer` with the answers.
 *
 * Every question is answered, always. The contract makes silence a denial, so
 * a card that let a reader close it without a decision would deny by accident
 * and never say so; `Dismiss` answers with no labels, which is a decision the
 * engine reads as one.
 */
export function askCard(params, onAnswer) {
  const root = document.createElement("section");
  root.className = "ask";
  root.setAttribute("role", "group");

  const head = document.createElement("h3");
  head.className = "ask-head";
  head.textContent = "The harness is asking";
  root.append(head);

  const chosen = new Map();
  for (const question of params.questions || []) {
    root.append(one(question, chosen));
  }

  const foot = document.createElement("div");
  foot.className = "ask-foot";
  const send = button("Answer", {
    kind: "primary",
    onClick: () => {
      onAnswer({
        answers: (params.questions || []).map((question) => ({
          id: question.id,
          labels: [...(chosen.get(question.id) || [])],
        })),
      });
      root.classList.add("ask-done");
      send.disabled = true;
      away.disabled = true;
    },
  });
  // Answering with nothing is a decision the engine can read. Leaving is not:
  // §4.4.3 treats a client that does not answer as a denial, so the card never
  // offers a way out that says nothing.
  const away = button("Dismiss", {
    title: "answers with no choice, which the harness reads as a refusal",
    onClick: () => {
      onAnswer({
        answers: (params.questions || []).map((question) => ({ id: question.id, labels: [] })),
      });
      root.classList.add("ask-done");
      send.disabled = true;
      away.disabled = true;
    },
  });
  foot.append(send, away);
  root.append(foot);
  return root;
}

/** One question, with its options as real radio buttons or checkboxes. */
function one(question, chosen) {
  const root = document.createElement("fieldset");
  root.className = "ask-one";
  const legend = document.createElement("legend");
  legend.textContent = question.question;
  root.append(legend);

  if (question.detail) {
    const detail = document.createElement("p");
    detail.className = "ask-detail";
    // Supporting text the asker wrote, kept out of the option labels by the
    // contract and kept out of them here.
    detail.textContent = question.detail;
    root.append(detail);
  }

  const many = Boolean(question.multi_select);
  for (const option of question.options || []) {
    const line = document.createElement("label");
    line.className = "ask-option";
    const box = document.createElement("input");
    // Radio for one, checkbox for many: the browser then enforces the arity
    // the contract states, and the keyboard behaves the way every other form
    // on the reader's machine behaves.
    box.type = many ? "checkbox" : "radio";
    box.name = `q-${question.id}`;
    box.value = option.label;
    box.addEventListener("change", () => {
      const set = chosen.get(question.id) || new Set();
      if (!many) set.clear();
      box.checked ? set.add(option.label) : set.delete(option.label);
      chosen.set(question.id, set);
    });
    const said = document.createElement("span");
    // The label is both the text and the value the answer carries (§4.6), so
    // it is shown exactly as it arrived.
    said.textContent = option.label;
    line.append(box, said);
    if (option.description) {
      const why = document.createElement("span");
      why.className = "ask-why";
      why.textContent = option.description;
      line.append(why);
    }
    root.append(line);
  }
  return root;
}

/**
 * The durable audit, as rows on the transcript - with the pair kept together.
 *
 * `approval/asked` says a decision was needed; `approval/decided` says how it
 * went; `approval/policy` says the session's rule changed. Each is on the
 * journal, so a conversation read tomorrow still shows them.
 *
 * # Why this holds state instead of being a function per event
 *
 * `crates/turn/src/approval.rs` is explicit that the two halves are "one pair
 * per question, sharing an `id`", and it appends the ask *before* the question
 * goes out and the decision whenever it settles. So the two are separated on
 * the journal by everything that happened while a person was deciding - and
 * drawn as two independent rows, the second is a bare pill saying `rejected`
 * with nothing on the page saying what was rejected. With one question in
 * flight a reader can infer it; with two they cannot, and the engine gives
 * them the `id` precisely so they do not have to.
 *
 * So a tracker: the ask draws a row and the decision finds it and completes
 * it. A decision whose ask is not on the page still draws - a reader who
 * opened a conversation part-way through gets a row naming the id rather than
 * nothing at all.
 */
export function approvals() {
  const open = new Map();
  return {
    // Named, not matched on the `approval/` prefix, so a type added to the
    // family later is not claimed and silently dropped - it falls through to
    // the raw rendering §4.3.2 asks for.
    handles: (type) => APPROVAL_TYPES.includes(type),
    row: (type, data) => row(open, type, data),
  };
}

/** The three durable types of the decision audit (§4.3.2). */
const APPROVAL_TYPES = ["approval/asked", "approval/decided", "approval/policy"];

function row(open, type, data) {
  switch (type) {
    case "approval/asked": {
      // Open, not folded. The reason is the whole content of the question -
      // "delete X and everything under it; this cannot be undone" - and a
      // reader who has to click to find out what is being decided is a reader
      // deciding without it.
      const root = disclosure(`asked whether ${data.tool_name} may run`, { open: true });
      if (data.reason) {
        const why = document.createElement("p");
        why.className = "ask-detail";
        why.textContent = data.reason;
        root.body.append(why);
      }
      if (data.call_id) root.body.append(pill(`call ${data.call_id}`));
      const waiting = pill("waiting for a decision", "busy");
      root.head.append(waiting);
      if (data.id) open.set(data.id, { root, waiting });
      return root;
    }
    case "approval/decided": {
      const known = OUTCOMES[data.outcome];
      // A word this build has never seen is drawn as itself and toned as a
      // refusal, because §4.4.7 says the engine reads an unknown outcome as a
      // denial - a surface that drew it as neutral would disagree with what
      // actually happened.
      const outcome = pill(known ? known.said : data.outcome, known ? known.tone : "bad");
      const asked = data.id === undefined ? undefined : open.get(data.id);
      if (asked) {
        open.delete(data.id);
        asked.waiting.replaceWith(outcome);
        // Folded now it is settled: a decided question is history, and the
        // reason it was asked is one click away instead of on the page in
        // front of whatever the reader is actually reading.
        asked.root.open = false;
        // Nothing to append - the row that was already on the page is the row
        // that now says how it went.
        return null;
      }
      const root = document.createElement("div");
      root.className = "ask-decided";
      if (data.id) {
        const which = document.createElement("span");
        which.className = "ask-why";
        which.textContent = `decision ${data.id}`;
        root.append(which);
      }
      root.append(outcome);
      return root;
    }
    case "approval/policy": {
      const root = document.createElement("div");
      root.className = "ask-decided";
      root.append(pill(`approvals: ${data.policy}`));
      return root;
    }
    default:
      return null;
  }
}
