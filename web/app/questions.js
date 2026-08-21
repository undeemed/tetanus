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
 * The durable audit, as a row on the transcript.
 *
 * `approval/asked` says a decision was needed; `approval/decided` says how it
 * went; `approval/policy` says the session's rule changed. Each is drawn from
 * the journal, so a conversation read tomorrow still shows them.
 */
export function approvalRow(type, data) {
  switch (type) {
    case "approval/asked": {
      const root = disclosure(`asked whether ${data.tool_name} may run`, { open: false });
      if (data.reason) {
        const why = document.createElement("p");
        why.className = "ask-detail";
        why.textContent = data.reason;
        root.body.append(why);
      }
      if (data.call_id) root.body.append(pill(`call ${data.call_id}`));
      return root;
    }
    case "approval/decided": {
      const known = OUTCOMES[data.outcome];
      const root = document.createElement("div");
      root.className = "ask-decided";
      // A word this build has never seen is drawn as itself and toned as a
      // refusal, because §4.4.7 says the engine reads an unknown outcome as a
      // denial - a surface that drew it as neutral would disagree with what
      // actually happened.
      root.append(pill(known ? known.said : data.outcome, known ? known.tone : "bad"));
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
