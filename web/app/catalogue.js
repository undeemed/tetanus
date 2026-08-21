// What this deployment can run, and what it can run it with.
//
// Upstream splits this across `ui-model-selection` (choose one) and
// `ui-settings-models` (configure them). Both sit on the same fact - the
// server's catalogue - and this build already serves it: `catalog.models`
// answers providers with their models and whether their credential is there,
// and `catalog.tools` answers the tools a turn may call.
//
// # The one rule the contract writes down for a picker
//
// §4.6: "`ProviderDescriptor.available` is false when a provider is registered
// but its credential is absent, so a picker can grey the entry instead of
// failing at the first turn." That sentence is the whole design of this file.
// A picker that offered an unavailable provider would turn a missing
// environment variable into a failed conversation, and the reader would meet
// it one turn later, somewhere else, worded as a provider error.
//
// So an unavailable provider is shown - it is a fact about the deployment, and
// hiding it makes a reader wonder where their provider went - and it is shown
// as unusable, with the variable that would fix it named. The contract carries
// `credential_env` for exactly that, and naming it is the difference between
// "unavailable" and "unavailable, set `DEEPSEEK_API_KEY`".
//
// # What choosing does, and what it does not
//
// Choosing a model here starts a **new** conversation on it, because that is
// what this contract offers: `session.create` takes a provider and a model,
// and there is no call that moves a running session to another model.
// Upstream has `session.selectModel`; this contract does not, so the button
// says "Start here" rather than pretending a switch that would silently do
// something else.

import { button, pill, stateDot } from "./primitives.js";

/**
 * Draw the providers and their models.
 *
 * `onStart(provider, model)` is called when a reader picks one. Omit it for a
 * read-only catalogue.
 */
export function models(root, providers, { current, onStart } = {}) {
  root.replaceChildren();
  if (!providers || providers.length === 0) {
    root.append(nothing("This build has no providers registered."));
    return;
  }
  for (const provider of providers) {
    root.append(providerRow(provider, current, onStart));
  }
}

function providerRow(provider, current, onStart) {
  const root = document.createElement("div");
  root.className = "trace-turn";

  const head = document.createElement("div");
  head.className = "trace-head";
  const name = document.createElement("span");
  name.className = "trace-name";
  name.textContent = provider.provider;
  head.append(name);
  // Available is the engine's answer about this machine, not a guess from the
  // model list: a provider with models and no credential is registered and
  // unusable, which is a different thing from absent.
  head.append(
    provider.available
      ? stateDot("ok", "ready")
      : stateDot("bad", provider.credential_env ? `set ${provider.credential_env}` : "unavailable"),
  );
  root.append(head);

  if (provider.models.length === 0) {
    // "Advisory catalog. An unlisted model id still passes through" - so an
    // empty list is a provider that names none, not one that serves none.
    root.append(quiet("names no models; an id still passes through"));
    return root;
  }

  for (const model of provider.models) {
    const line = document.createElement("div");
    line.className = "trace-step";
    const label = document.createElement("span");
    label.className = "trace-name";
    label.textContent = model;
    line.append(label);
    if (current && current === model) line.append(pill("this conversation", "ok"));
    if (onStart) {
      const go = button("Start here", {
        title: provider.available
          ? `start a conversation on ${model}`
          : `${provider.provider} has no credential on this machine`,
        onClick: () => onStart(provider.provider, model),
      });
      // Greyed rather than hidden, which is what the contract asks a picker to
      // do with an unavailable provider.
      go.disabled = !provider.available;
      line.append(go);
    }
    root.append(line);
  }
  return root;
}

/**
 * Draw the tools a turn may call.
 *
 * The names matter more than the schemas here: a reader opening this is asking
 * "what can this agent do", and a JSON schema per tool answers a different
 * question at ten times the length. The description is the engine's own.
 */
export function tools(root, list) {
  root.replaceChildren();
  if (!list || list.length === 0) {
    root.append(nothing("This build registers no tools."));
    return;
  }
  for (const tool of list) {
    const line = document.createElement("div");
    line.className = "trace-step";
    const name = document.createElement("span");
    name.className = "trace-name";
    name.textContent = tool.name;
    line.append(name);
    if (tool.description) {
      const said = document.createElement("span");
      said.className = "trace-took";
      said.textContent = tool.description;
      line.append(said);
    }
    root.append(line);
  }
}

function nothing(said) {
  const node = document.createElement("p");
  node.className = "list-empty";
  node.textContent = said;
  return node;
}

function quiet(said) {
  const node = document.createElement("div");
  node.className = "trace-note";
  node.textContent = said;
  return node;
}
