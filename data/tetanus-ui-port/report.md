# Porting DeepSeek Harness's browser UI into tetanus

## 1. Identification

- **Subject:** the browser panel served by `tetanus serve --frontend`.
- **Status:** stage 1 delivered. One of upstream's real screens - the conversation view - runs against our engine and streams a real turn. `web/app` is untouched and still the panel that ships.
- **Authoritative copy:** this file, in the tetanus repository.
- **Audience:** whoever decides whether stage 2 happens, and whoever does it.

## 2. The short version

Upstream's conversation view now renders our journal.
It took **175 vendored files** and **about 900 lines of our own adapter**, not the 66,900 lines the brief warned about, because the thing that makes their UI look enormous is type-only imports that never run.

Two findings change the premise the brief was written on, and both are in section 4:
their published npm packages **cannot be installed at all**, and the packages they publish are under a **different licence** from the source we were pointed at.

## 3. Which shape, and the evidence

The brief named two shapes and asked for evidence rather than preference.

> **(a)** implement their API surface in our host crate so their client runs unmodified;
> **(b)** replace their connection layer so their components speak our existing carrier.

**Chosen: (b)** - and with the seam in a different place from where the brief expected it.

### 3.1 Why not (a)

Their client does not dial six endpoints, it dials a typed gateway.
`packages/host/apiproxy/src/api/` is **3,130 lines** across **twelve domains**: `sessions`, `subagents`, `host`, `workspace`, `skills`, `agentPresets`, `events`, `goals`, `settings`, `credentials`, `llm`, `downloads`.
Serving that from `crates/host` means reimplementing twelve domains in Rust and tracking an upstream that has no stable release.

Ours is fifteen methods (`crates/protocol/src/methods.rs`).
The size difference is roughly two orders of magnitude in the wrong direction.

And it buys nothing on the client side.
Shape (a) is supposed to let their client "run unmodified", but their client is 44 packages that have to be built either way, and (section 4.1) cannot be installed.
So (a) is (b)'s work plus a Rust gateway.

### 3.2 Why the seam is not the HTTP path either

The interesting finding.
Their components never touch the gateway.
`ChatView` reads three values - `chat.order`, `chat.nodes`, `chat.timeline` - out of a store their session projection fills.
The type is `ConversationTimelineSnapshot`, and it is *derived*, not transported.

So the real gap between the two projects is not "which URL" and never was.
It is: **our durable event vocabulary versus their derived conversation model.**
Whatever shape you pick, something has to fold `turn/start`, `assistant/chunk`, `tool/call` … into their nodes.
`web/deepseek/src/timeline.ts` is that fold, written out, and it is the port.

Putting it in TypeScript beside their types rather than in Rust behind a gateway is what makes it 350 lines instead of a subsystem.

### 3.3 Measured

| Measurement | Value | How |
| --- | --- | --- |
| Workspace packages in their checkout | 237 | `package.json` scan |
| Packages `ui-conversation` reaches | 64 | dependency + peerDependency closure |
| Files a naive import trace finds from `ChatView` | 220 (40,020 lines) | all imports followed |
| Files `ChatView` actually **runs** | 55 (8,428 lines) | value imports only; `import type` erased |
| Files the whole screen runs, with every node renderer | 175 | `web/deepseek/tools/vendor.py` |
| Our adapter | ~900 lines | `web/deepseek/src/` |

**The 4x gap between rows three and four is the whole story of this port.**
`import type` is erased before resolution, so four fifths of what a component "depends on" is compile-time only.
Anyone who scoped this port by reading `package.json` would have costed it at 103,000 lines and refused.

## 4. Two findings that change the brief's premises

### 4.1 Upstream's npm release cannot be installed

The brief assumed a source copy.
An npm dependency would have been better - no vendoring, no 175 files, a version number instead of a snapshot - so it was tried first.
All 68 packages in the closure **are** published, and prebuilt `lib/*.js` ships in the tarballs.

It still cannot be installed:

```
npm error 404 Not Found - GET https://registry.npmjs.org/@deepseek-ai%2fdsh-compact
npm error 404 Not Found - GET https://registry.npmjs.org/@deepseek-ai%2fdsh-user-interaction
```

- `@deepseek-ai/dsh-client-runtime@0.0.1-rc.1` **depends** on `@deepseek-ai/dsh-compact` - not published.
- `@deepseek-ai/dsh-host-apiproxy@0.0.1-rc.1` **depends** on `@deepseek-ai/dsh-user-interaction` - not published.

Both are core; neither is optional; `--legacy-peer-deps` does not help because these are `dependencies`.
Their published client stack is broken as a consumable artifact.
Reproduce: `npm install @deepseek-ai/dsh-client-ui-conversation@0.0.1-rc.1`.

**Consequence:** vendoring is not a preference, it is the only route. Section 6 is how it is kept honest.

### 4.2 The published packages are BSD-3-Clause, not MIT

The brief states "Upstream is MIT", and for the source checkout that is exactly right: root `LICENSE` is the MIT licence, and **all 221** workspace `package.json` files declare `"license": "MIT"`.

The npm tarballs at `0.0.1-rc.1` say something else.
They ship a `LICENSE` reading `BSD 3-Clause License / Copyright (c) 2026, DeepSeek`, and declare `"license": "BSD-3-Clause"`.

Everything vendored here comes from the **source checkout**, so MIT applies and the brief's premise holds.
Recorded because it is a live trap: BSD-3-Clause clause 3 forbids using the copyright holder's name to promote a derived product, and an npm tarball dropped into `upstream/` later would bring that obligation in under headers claiming MIT.
`web/deepseek/NOTICE.md` says so where the next person will look.

### 4.3 The trade mark is not licensed, and rebranding is where that bites

A copyright licence grants copyright permission and says nothing about trade marks.
`ui-primitives/BrandWordmark.tsx` is DeepSeek's whale mark and the `deepseek-official HARNESS` letterforms drawn as SVG; `FishLogo.tsx` is the whale again.

A rebrand pass that changes the words and keeps the art gets this exactly backwards.
Neither file is vendored, `tools/vendor.py` refuses both, and `panel_port.rs::upstream_brand_art_is_not_vendored` keeps it refused.
Nothing in the conversation view referenced either symbol, so the cost was zero.

## 5. What was delivered

### 5.1 The screen

Upstream's `ChatView`, its node renderers, its tool tree and its markdown pipeline, over our carrier.
Driven in a real browser against a real engine, a real turn streams end to end:

- the user bubble, right-aligned, with upstream's clock and copy actions;
- streamed assistant text growing in place, replaced by the settled answer;
- a tool card with the arguments as `IN` and the command's output as `OUT`, `data-state="ok"` once the result lands;
- events this build has no shaped row for - `request/context` - drawn as labelled raw cards rather than dropped, which is contract §4.3.2's rule.

### 5.2 The parts

| File | Lines | What it does |
| --- | --- | --- |
| `src/carrier.ts` | ~200 | JSON-RPC 2.0 on `/api/ws`: `rpc.hello`, calls, and the two pushes. |
| `src/timeline.ts` | ~350 | **The port.** Journal to `ChatConversationViewNode`s. |
| `src/store.ts` | ~80 | The nine keys `ChatView` selects. |
| `src/renderers.tsx` | ~75 | Upstream's keyed node table, minus the DI container. |
| `src/App.tsx`, `App.module.css`, `locale.ts`, `brand.ts` | ~300 | One session, a composer, chrome, the product strings. |

Three substitutions replace framework rather than logic, and each is smaller than what it replaces:

- **cordis** (their DI container) becomes a `Record<string, ComponentType>` in `renderers.tsx`. Their registry *is* a map from node kind to component; the container exists so a plugin can add a row type at runtime, and this panel has no plugins.
- **their locale service** becomes a lookup with `{name}` interpolation over their own `en` dictionary, which is vendored rather than re-typed.
- **their session projection** becomes `timeline.ts`.

### 5.3 The build story for a Rust repository

The constraint is real: `web_app.rs` records the project's promise of "one self-contained binary, no Node, no `node_modules`, no runtime to install".
This panel needs a bundler, so the promise has to be restated rather than quietly broken.

How it is kept true for anyone who wants it:

- **The binary still needs nothing.** `cargo build` is unchanged; `crates/host` serves a directory of files and has no idea what made them.
- **`web/app` still needs nothing.** It has no build step and is still what ships.
- **`dist/` is not committed.** It is a build artifact, regenerated by the gate.
- **Missing Node is not a build failure.** `cargo test --workspace` on a machine with no Node prints what it did not check and passes.

What is *not* true any more: a fresh clone with no Node cannot serve **this** panel, because there is no `dist/` to serve.
That is the honest cost of a React port and it should be stated to the captain plainly rather than engineered around.
The two ways out, if it matters, are committing `dist/` (a minified blob in every diff) or publishing it as a release asset (a second distribution channel). Neither is worth doing before the panel is chosen.

### 5.4 How the assets get served

Nothing new. `Frontend::mount` already serves a directory with SPA semantics, and the boot manifest already reaches the page through the index tap:

```
tetanus serve --listen 127.0.0.1:5300 --frontend web/deepseek/dist
```

The one thing that had to be right is that the page reads `window.TETANUS_BOOT` as data instead of being patched on the way out - which `crates/cli/src/web.rs` already argues for at length, and which `src/carrier.ts` obeys.

## 6. The behaviour floor: the fifteen TC-WEB cases

Each is answered by an equivalent, or the drop is justified here.
The equivalents live in `crates/host/tests/panel_port.rs` and `crates/toolset/tests/panel_tool_cards.rs`.

| # | TC-WEB case | Answered by | Stronger, equal, or dropped |
| --- | --- | --- | --- |
| 1 | one class name has one owner | `every_component_stylesheet_is_scoped` | **stronger** |
| 2 | no element wears one name from each stylesheet | same | **stronger** |
| 3 | every id a script reaches for exists | `every_id_a_script_reaches_for_is_on_the_page` | equal |
| 4 | every module the page imports is a file that exists | `the_panel_builds` | **stronger** |
| 5 | nothing on the page is set as markup | `nothing_the_panel_adds_is_set_as_markup` | equal, narrowed - see below |
| 6 | nothing pins a width a narrow screen cannot give | `nothing_the_panel_adds_pins_a_width` | equal, narrowed |
| 7 | the page's modules are the ones it loads | `the_panel_builds` | **stronger** |
| 8 | every tool offered is drawn or listed as bare | `the_tools_that_get_a_shaped_card_are_the_ones_named` | restated - see below |
| 9 | the page draws no tool this build does not offer | same | restated |
| 10 | every durable type the engine writes is drawn or listed | `the_panel_accounts_for_every_durable_type_the_engine_writes` | **stronger** |
| 11 | the page names no event the engine cannot produce | `the_panel_folds_no_event_the_engine_cannot_produce` | **stronger** |
| 12 | every script parses as a module | `the_panel_builds` | **stronger** |
| 13 | no module declares one top-level name twice | `the_panel_builds` | **stronger** |
| 14 | the picker starts what it picked | **dropped** - see below | dropped |
| 15 | the catalogue marks the current conversation | **dropped** - see below | dropped |

Two more were added, because the port created two risks the old panel did not have: `every_vendored_file_carries_its_notice` and `upstream_brand_art_is_not_vendored`.

### 6.1 Why 4, 7, 12 and 13 are stronger, not moot

The brief is right that a React build makes them easier, and right to insist they be re-pointed rather than deleted.
They are re-pointed at one thing: **the build**, which `the_panel_builds` runs.

A bundler will not emit a bundle for a tree containing a syntax error (12), a duplicate top-level binding (13), an import that resolves to nothing (4), or a module nothing reaches (7 - unreached modules are simply absent from the graph).
And it links the graph, which `node --check` per file cannot: a file can parse perfectly and import a module that does not exist.

Then it earned its keep immediately: the first working build failed with `draw is not a function`, because every one of upstream's node views is wrapped in `memo` and a memo component is an object React renders rather than a function anybody may call. The panel loaded, connected, and died on the first message.

### 6.2 Why 1 and 2 are stronger

The old panel's failure was structural: a stylesheet has one namespace and no compiler, so `.row` defined twice is silent and order-dependent.
Every stylesheet here is a CSS module compiled to `[name]__[local]__[hash]`, so two files exporting `.row` produce two different class names and the collision cannot be expressed.

The case asserts the mechanism is on (the hash is in the pattern) and that no global sheet carries class rules - the one route back into the old failure.
Upstream's theme sheets are global and are allowed to be: they declare `--dsw-*` custom properties on `:root` and `body`, which cannot collide.

### 6.3 Why 10 and 11 are stronger

The old cases scrape string literals out of JavaScript.
The fold declares what it draws in one exported array (`KNOWN` in `timeline.ts`), so the case reads a list instead of guessing at one.

Writing it surfaced a **pre-existing blind spot in the old scan**: it reads a `mod topic` constant and a literal `.append`, and misses a bare `const NAME: &str = "family/name"`. That is how `llm/retry`, `llm/retry-started` and `subagent/descriptor` are declared, and `web_app.rs` therefore reports them as unwritten while the engine writes them. It also misses `session/start`, which is declared as a serde rename on `KnownEvent`.

`panel_port.rs` reads all four conventions. Its `UNDRAWN` list is consequently longer and more honest than `web_app.rs`'s.
**`web_app.rs` should get the same fix**, and it is deliberately not done here: that would be a change to the shipping panel's guard inside a PR about a candidate panel.

### 6.4 Why 5 and 6 are narrowed, and to what

Both are scoped to files this project wrote.

For 5, upstream has exactly one `dangerouslySetInnerHTML`, in `CodeBlock.tsx`, rendering the HTML a syntax highlighter produced - the sanctioned path for that library, and not user input. Widening the rule to the vendored tree buys either a false failure or an exception list that reads as permission.

For 6, upstream's own responsive behaviour is upstream's, and it is exercised by their tests, not ours. Our chrome is where a fixed width would be ours to fix.

### 6.5 Why 8 and 9 are restated

The panel has no view table of its own, so neither case can be asked literally: nothing here can draw a tool that does not exist, and every tool is drawn.

What can rot instead is real, and nothing said it before: upstream's row model maps a tool **name** to a card shape, and the two projects named their tools independently. Our `shell` is their `bash`; our `search` is their `grep`. Those get the generic card. But `read`, `write`, `edit` and `glob` happen to be spelled the same in both projects and therefore get a **shaped** card - and a tool renamed on either side would change its look with nothing in either repository saying so.

`the_tools_that_get_a_shaped_card_are_the_ones_named` asserts that intersection in both directions.

### 6.6 Why 14 and 15 are dropped

Both test surfaces this stage does not have, and inventing a stub to assert against would be worse than the gap.

- **14, the picker starts what it picked.** There is no directory picker: this screen opens one session and does not choose a workspace. Upstream's picker is `ui-directory-picker-browse`, which is stage 3.
- **15, the catalogue marks the current conversation by route and model.** There is no session catalogue - no sidebar, no session list. The model *is* shown, in the header bar, so half of the claim has a place to live; the route half needs a catalogue to be a claim about.

**Both must come back with the surfaces they test.** They are listed in section 10 against the stages that reintroduce them.

## 7. Coverage, measured

Two numbers, never added together.
A single blended figure would let 8,000 well-covered vendored lines hide an uncovered branch in the 900 lines that are new here, which is the one place a defect can hide.

### 7.1 The adapter - ours - is at 100%, gated

`web/deepseek/src` is the carrier, the fold, the store, the renderer table and the screen.
Nobody upstream has ever run a line of it.

```
File            | % Stmts | % Branch | % Funcs | % Lines
----------------|---------|----------|---------|--------
src/App.tsx     |     100 |      100 |     100 |     100
src/carrier.ts  |     100 |      100 |     100 |     100
src/locale.ts   |     100 |      100 |     100 |     100
src/main.tsx    |     100 |      100 |     100 |     100
src/renderers.tsx |   100 |      100 |     100 |     100
src/store.ts    |     100 |      100 |     100 |     100
src/timeline.ts |     100 |      100 |     100 |     100
----------------|---------|----------|---------|--------
All files       |     100 |      100 |     100 |     100
  statements 317/317   branches 203/203   functions 69/69   lines 279/279
```

175 cases across seven spec files.
The threshold is **per file** and lives in `vitest.config.ts`, so a well-covered big file cannot subsidise a bare one; `crates/host/tests/panel_port.rs::the_adapters_specs_hold_every_line` is what makes `cargo test --workspace` fail when it is not met.

One deliberate exclusion, and it is the only one: `src/brand.ts`, four exported string constants with no branch.
A test asserting a constant equals itself is decoration.

The cases are not happy-path. What is exercised, by count: every branch of the fold including all ten known event types and the unknown fallback; `data` arriving as `null`, a string, a number, an array and a boolean on **every** known type; a `tool/result` with no matching `tool/call`; two calls sharing one id; a socket frame that is not JSON, a frame that is the four bytes `null`, a binary frame, a reply to an id nobody asked for, the same id answered twice, and a close with calls in flight; a `session.create` that answers without an id or a model; a rejection carrying neither a numeric code nor a string message.

**Two of those found real defects while being written**, which is the argument for writing them:

- `JSON.parse('null')` returns `null`, and `typeof null === 'object'`, so the obvious guard misses it. A peer sending four bytes crashed the whole message handler. Fixed in `carrier.ts`; the case is `a JSON null is dropped without throwing`.
- Every one of upstream's node views is wrapped in `memo`, and a memo component is an object React renders rather than a function anybody may call. `draw(owner)` threw `draw is not a function` at the first row that arrived - the panel loaded, connected, and died on the first message. Fixed in `renderers.tsx`; the case is in `renderers.spec.tsx` and the mutation table below re-proves it.

### 7.2 Upstream's specs came with upstream's components

**26 spec files, 521 cases, all passing** against the vendored copy.
`pnpm run test:upstream`, and `panel_port.rs::upstreams_own_specs_still_pass` runs it inside the gate.

They are **not** held to a coverage threshold, and that is a decision with a reason rather than a shortcut: **upstream does not hold this code to one either.**
Their own `vitest.config.ts` excludes `packages/client/ui-conversation/src/client/*`, `ui-tool/src/*`, `ui-slots/src/*`, `runtime/src/**/!(settings-scope).ts`, `ui-primitives/src/JsonTree.tsx`, `Menu.tsx`, `DisclosureRow.tsx`, `RiskConfirmation.tsx` and `markdown/plain-text.ts` from their per-file 100% gate, each marked `TODO(gui)`.
The code being ported *is* upstream's own coverage debt. Adopting a number they do not hold would be inventing a claim; the number is measured and reported instead:

`pnpm run test:vendored:coverage` runs **both** suites and measures the vendored tree, which is the only measurement that means anything here: our own specs drive upstream's components hard - `app.spec.tsx` pushes a whole turn through `ChatView`, the node views and the tool tree - so a number counting only upstream's suites reports those packages at 0% while they are being exercised on every run.

```
                  covered/total lines
ui-attachment          89/    89   100.0%
ui-primitives        1521/  1598    95.2%
apiproxy               11/    20    55.0%
ui-tool               113/   202    55.9%
ui-conversation       206/   600    34.3%
cosmokit               43/   189    22.8%
attachment              3/    21    14.3%
cordis                 50/   868     5.8%
llm                     2/    43     4.7%
ui-slots                3/   163     1.8%
session                 1/   131     0.8%
runtime                16/  2386     0.7%
-------------------------------------------
TOTAL                2058/  6310    32.6%
```

**Read the shape, not the total.** The components the panel actually draws with are well covered - `ui-primitives` at 95%, `ui-attachment` at 100%, and `ui-tool`/`ui-conversation` in the middle because their own specs are among the 43 that could not come across.

The total is dragged down by four support packages - `runtime` (2,386 lines), `cordis` (868), `cosmokit` (189) and `session` (131) - which together are **56% of the denominator and almost none of the executed code**. They are in the tree because a vendored file imports them, not because the conversation view runs them: `runtime` is upstream's session service, workspace manager and projection store, and this panel replaces every one of those with `src/`. Rollup tree-shakes them out of the bundle; the closure tool cannot, because it resolves imports rather than executing them.

That is a real cost of the port and it is worth naming: **about 3,500 of the 6,310 vendored lines are reached by an import and barely executed.** Trimming them is possible - the closure could be computed from the bundle's module graph rather than from the import graph - and it is deliberately not done here, because the tool that computes the closure is what makes a refresh a copy rather than a merge, and making it cleverer before the panel is even chosen is optimising the wrong end.

### 7.3 The 43 specs that could not come across, and why

Written to `web/deepseek/upstream/SPECS-NOT-PORTED.txt` by `tools/vendor.py`, computed rather than listed, so a refresh cannot quietly widen it. Two reasons, both structural:

| Count | Reason |
| --- | --- |
| 38 | needs `@deepseek-ai/cordis` or `@deepseek-ai/dsh-client-test-runtime` |
| 5 | its subject is outside the ported closure |

The first is the dependency-injection container and the whole-client-context harness that this port exists to avoid: `src/renderers.tsx` replaces the container with a map, so a spec that assembles a cordis context has no context to assemble. Bringing them would mean vendoring the framework, which is the thing that made the naive port look like 103,000 lines.

The second is a spec whose subject was not vendored - `input/machine.ts`, `skeleton/safari.ts` and so on are outside the conversation view's runtime closure. A spec for a file that is not here has nothing to assert against.

**What that costs, said plainly:** the tool card renderers (`read-card`, `diff-card`, `search-card`, `terminal-card`, `web-card`, `tool-row`, `tool-call-tree`) and the conversation's own view specs are among the 41. Those components are still exercised - by `renderers.spec.tsx` and by `app.spec.tsx` driving a whole turn through them, which is what puts `ui-tool` at 56% and `ui-conversation` at 34% in the table above - but by our cases rather than theirs. Recovering upstream's own is stage 2 work and depends on whether we vendor their test runtime, which is the "track or fork" question in section 10.

Two edits were needed to make the 26 that did come across pass, and both are recorded in each file's header:

- **A `FishLogo` suite was removed** from `icons.client.spec.tsx`. It asserts the brand art this port deliberately does not vendor.
- **Per-case timeouts were raised to 180s.** Upstream writes `{ timeout: 20_000 }` on a markdown-prefix comparison that takes 36s on this box, which runs several compile lanes at once. Raising a threshold only weakens a case that asserts something happened *fast*; these assert that output matches, so a longer allowance changes nothing they claim. Lowering one would be the dangerous direction.

### 7.4 The snapshot goldens were re-baselined, and here is the proof that is all that happened

46 file snapshots came across. They contain CSS-module class names, and a CSS-module hash is derived from the **file path** - which changed when the sources were vendored one directory shallower. The goldens therefore cannot travel unchanged.

Re-baselining a golden is the easiest way to turn 46 of somebody else's assertions into 46 assertions that agree with whatever the code now does, so it was done with evidence rather than on trust: the pristine goldens were kept, the suite was re-baselined, and the diff was masked for CSS-module hashes and checked for asymmetry.

The first attempt was **not** hash-only. It also showed `strut` where upstream had `katex-strut`, which is a KaTeX **version** difference: upstream pins `katex@0.16.47` and an unpinned install took `0.18.5`, which renders different markup. Pinning to upstream's version removed it.

After the pin, every one of the 360 changed lines is symmetric under hash masking - that is, each removed line has an identical added line once the hash is replaced. No DOM structure assertion changed. The check is reproducible:

```sh
diff -r <pristine> upstream/ui-primitives/tests/fixtures \
  | grep -E "^[<>]" | sed -E 's/_[0-9a-f]{6}\b/_HASH/g' \
  | sed -E 's/^[<>] //' | sort | uniq -c | awk '$1 % 2 == 1'   # prints nothing
```

### 7.5 Mutation testing: the coverage survives being attacked

Coverage that only survives `vitest run` is decoration. Ten mutations, each a defect somebody could plausibly write, applied to the real source and reverted:

| Mutation | What it breaks | Verdict |
| --- | --- | --- |
| `isError: failed` to `isError: false` | a failed tool always draws green | **caught** |
| `data['call_id']` to `data['id']` in the result arm | a result never finds its call; every tool row runs for ever | **caught** |
| drop the `if (!nodes.has(...))` guard on `order.push` | a streamed row is appended once per delta | **caught** |
| `kind: 'tool-result'` to `'tool-settled'` | upstream stops recognising a settled call | **caught** |
| read `payload['running']` instead of `payload['state']` | the running label never appears | **caught** |
| drop the reject in the socket's `close` handler | a call in flight when the socket dies waits for ever | **caught** |
| drop the seq de-duplication in `arrived` | a reconnect draws every answer twice | **caught** |
| `createElement(draw, ...)` back to `draw(...)` | the panel dies on the first row - the real regression | **caught** |
| drop the `session.subscribe` call | the transcript never updates | **caught** |
| `asked.trim()` to `asked` | whitespace is sent to the model as a prompt | **caught** |

**Survivors: 0.**

One is worth a sentence because it nearly read as a gap. `kind: 'tool-result'` to `'tool-settled'` is **not** caught by `app.spec.tsx`, the whole-turn integration case - because upstream's row model tests `'kind' in block` rather than the value, so the renamed row still settles and still draws green. It is caught by `timeline.spec.ts`, which asserts the discriminator itself. The unit case is the strict one here and the integration case is not a substitute for it.

### 7.6 The Rust side: this branch adds no Rust source lines

Stated first because it is the whole answer. The three Rust files this branch adds are **test** files:

```
crates/host/tests/panel_port.rs
crates/cli/tests/panel_engine.rs
crates/toolset/tests/panel_tool_cards.rs
```

No `src/` file in any crate is touched, so there is no new Rust line to cover and the crates' figures are unchanged by this diff. `cargo-llvm-cov` excludes integration test targets from its report, so those three do not appear in it; what shows they run is `cargo test --workspace` naming all 13 cases.

The table for the crates involved, measured with `cargo llvm-cov --no-report -p tetanus-host -p tetanus-toolset -p tetanus-hardness test` then `cargo llvm-cov report`:

```
### tetanus-host
frontend.rs              lines  92.94%  regions  89.51%  fns  81.82%
lib.rs                   lines  84.46%  regions  82.25%  fns  95.24%
picker.rs                lines  85.42%  regions  84.38%  fns  69.23%
respond.rs               lines  90.00%  regions  90.00%  fns 100.00%
route.rs                 lines  77.59%  regions  69.62%  fns  63.64%
TOTAL                    lines  85.64%  regions  83.33%  fns  85.71%

### tetanus-toolset
assembly.rs              lines  86.67%  regions  85.11%  fns  88.00%
composition.rs           lines  71.79%  regions  72.14%  fns  53.57%
lib.rs                   lines  78.42%  regions  73.91%  fns  66.67%
servers.rs               lines  87.76%  regions  84.93%  fns  81.82%
TOTAL                    lines  80.04%  regions  77.84%  fns  70.59%
```

Those gaps are pre-existing and belong to code this branch did not write. Closing them is worth doing and it is not this PR: it would mean editing other lanes' source to add tests for behaviour this change does not touch, which is exactly the kind of unrelated edit that makes a diff unreviewable.

**Where the real risk of this change lives, it is at 100%** - in TypeScript, gated per file, and proven against mutation.

## 8. The CI gap, and who runs the build now

The brief's third addition is correct and the finding is worse than it looks.

`.github/workflows/ci.yml` ran `fmt`, `clippy`, `build`, `test` and nothing else - no `setup-node`, no `npm`, no `pnpm`.
TC-WEB-12 skips when Node is absent.
**So the single automated check that catches a dead panel has never run on a pull request in this repository.** It only ever ran on a developer's machine.

### 8.1 What changed

The build is a **test**, not a workflow step: `panel_port.rs::the_panel_builds` runs `pnpm run build`, and `the_adapter_type_checks` runs `tsc`.

That is deliberate, and it is the answer to "who runs it".
`cargo test --workspace` is what this project calls its merge gate.
Putting the build inside it means a developer running the gate locally gets the same answer CI gets, there is exactly one rule about what happens when Node is missing, and there is no second place for the guard to be forgotten - which is precisely how the existing guard came to be dead.

The workflow's new job is to make that test able to run: `pnpm/action-setup`, `actions/setup-node` with a pnpm cache, and `pnpm install --frozen-lockfile` in `web/deepseek`, all before the cargo steps.

**What fails when it breaks:** `cargo test --workspace` fails, so the `test` step fails, so the required check fails, so the PR cannot merge. A broken panel build is a red gate, not a warning.

### 8.2 Missing Node fails in CI and skips on a laptop

`CI` is set by every hosted runner. The cases read it:

- **`CI` set, no Node / no pnpm / no `node_modules`** - the case **fails**, naming which of the three is missing and what to add to the workflow.
- **`CI` unset** - the case prints on stderr exactly what it did not check, and passes.

A skip in CI is not protection, it is the absence of protection wearing protection's clothes. That is the whole reason this section exists.

### 8.3 On sequencing

The brief offered to route the CI fix as its own PR ahead of this one. **It should not be**, and the reason is concrete rather than preference: before this branch there is nothing for `setup-node` to build. A prior PR would add a toolchain to CI, leave `web_app.rs`'s guard skipping exactly as it does today (that guard's fallback is a scan, not a build), and change no outcome. The gap closes when something exists that the toolchain can fail on, which is this branch.

There **is** a separate PR worth routing, and it is a different one: fixing `web_app.rs`'s topic scan (section 6.3) so the *shipping* panel's guard stops under-reporting. That is a change to the live panel's tests and does not belong here.

## 9. The structural gate, and the one number that does not clear its floor

`CONTRIBUTING.md` asks a change to hold **7000+** on `sentrux gate`, with **6200** as the floor. This branch reports **5000**, and that has to be said plainly rather than buried.

```
Quality:      7192 -> 5000
Coupling:     0.37 -> 0.04
Cycles:       0 -> 4
Complex functions: 11 -> 28
```

**Every point of that drop is upstream's code.** `sentrux` scans `git ls-files`, and this branch commits 178 files of vendored React. Measured with the vendored directory out of the index and nothing else changed:

```
Quality:      7192 -> 7138
Coupling:     0.37 -> 0.06
Cycles:       0 -> 0
Complex functions: unchanged
✓ No degradation detected
```

Reproducible in three commands:

```sh
git rm -r --cached web/deepseek/upstream -q
sentrux gate .
git reset -q
```

So the code this branch *writes* costs 54 points and introduces no cycle, no god file and no complex function. The 2,192-point difference and all four cycles are structural facts about DeepSeek Harness, which this change is copying rather than authoring.

`sentrux` has no ignore mechanism - `gate` takes a path and a `--save`, and nothing else - so there is no way to express "measure our code, record theirs" today. Three ways out, and **this is not the worker's call to make**:

1. **Accept the number with this explanation**, as the coverage split in section 7 does: two numbers, never blended, each honest about what it measures.
2. **Teach `sentrux` a vendored-path ignore**, which is the real fix and is a change to a tool outside this repository.
3. **Keep `upstream/` out of git** and fetch it during the build. This is the worst of the three: it breaks the licence wiring (the copyright headers are what makes the copy compliant, and they have to exist in the tree a reader gets), and it makes the build depend on a checkout CI does not have.

**Never run `sentrux gate --save`** to make this go away. That overwrites the fact every other branch is measured against, and a branch that saves its own numbers reports "no degradation" by construction.

## 10. The staged plan

Each stage is shippable on its own and each names the guards it brings back.

| Stage | What | Size | Brings back |
| --- | --- | --- | --- |
| **1 - done** | conversation view, one session, composer, tool cards | 175 vendored + ~900 ours | 1-13 |
| **2** | upstream's composer (`InputBar`), approvals and questions | ~40 more files | the `ui/ask` and `ui/approve` waits; `approval/*` and `question/*` leave `UNDRAWN` |
| **3** | the shell: sidebar, session catalogue, directory picker | ~60 more files | **TC-WEB-14 and TC-WEB-15** |
| **4** | shaped tool views, and reconciling the two tool vocabularies | ~30 more files | a real successor to TC-WEB-8/9 |
| **5** | the remaining `UNDRAWN` families: compaction, retry chains, todo, plan, goal, workflow | fold work, few new files | shrinks `UNDRAWN` toward empty |
| **6** | delete `web/app` | a deletion | retire `web_app.rs`, having proved each of its fifteen has a successor |

Stage 6 is the captain's call and nothing before it forces his hand.

Two things to decide before stage 2, and both are cheap to get wrong later:

1. **Do we track upstream, or fork?** `tools/vendor.py` assumes tracking - it recomputes the closure and preserves our two edits. Every stage that edits a vendored file makes tracking more expensive. Tracking is the right default while their code is moving; say so out loud before the edit count grows.
2. **Where does `dist/` come from for a user who is not building?** Section 5.3. Only worth answering once the panel is chosen.

## 11. What this port is not

Stated plainly so nobody reads more into a working screenshot than is there.

- It is **one screen**. No sidebar, no session list, no settings, no subagents, no goals, no plan mode, no workspaces.
- The composer is **ours**, not theirs. Upstream's `InputBar` is stage 2. What is there is a textarea and two buttons.
- Approvals and questions are **not served**. A `ui/ask` push is received and ignored; a turn needing one will appear to hang.
- Paging is **not wired**. `loadOlder` is a no-op; the first 500 events are the window.
- Every tool that is not `read`, `write`, `edit` or `glob` gets the **generic** card. That is honest and complete, not a stub - but it is not the shaped view upstream ships for `bash`.
