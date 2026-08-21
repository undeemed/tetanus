# Parity note: `fs/*` and `interaction/*`

Slice: the filesystem service, its tools, and the permission gates on the tool
pipeline (`crates/fs`, plus the pipeline changes in `crates/turn`).
Branch: `fm/tetanus-p2-fs`.
For folding into [`../parity.md`](../parity.md) section 3 and section 4 by the
reconciliation slice; this lane does not edit the shared file.

## 1. Section 3 rows, replaced

Both rows read `None` in the `Today` column before this slice.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `fs/*` (fs, local, sandboxed, tools, observation policy) | 20 | The filesystem service and both backends: read, write, edit, list, glob, stat and delete behind one trait, each failing in a named class with a machine-routable code and a sentence a model can act on rather than an `io::Error` string; stable target identity across aliases; atomic publish by rename; guarded writes (`createIfAbsent`, `replaceIfVersion`) and the read-match-write edit in one critical section; the read-before-write observation policy keyed per session; the mode vocabulary (`read-only`, `workspace-write`, `danger-full-access`) selecting the backend; seven model-facing tools with schemas, read windows and refusal wording, dispatched by the ordinary tool pipeline | Read windows over bytes rather than text, image reads and the attachment store they need, a search tool over file *contents* (`grep`), presentation of a diff, and the kernel sandbox backends that isolate untrusted code rather than untrusted paths | ③ for the kernel backends; the rest is scoped in section 4 below |
| `interaction/*` (approvals, questions, commands, permission presets) | 9 | The approval seam wired into the tool pipeline: a tool declares whether one pending call needs deciding, the engine puts the question between `tools/pre-execute` and the dispatch, a refused call is never dispatched and the model reads a `tool/result` carrying `TOOL_NOT_PERMITTED` and why; the headless default that denies rather than hanging, visible on the journal; user questions with their own durable pair, the three rules that decide whether what came back is an answer, the interrupt that withdraws one, and the `ask_user_question` tool; permission presets bundling the filesystem mode and the approval policy under one name, recorded as intent and folded back from the journal | `approval.set` and `ui/approve`/`ui/ask` served over the boundary (contract §4.2 still reserves them), slash commands, and the presentation of a prompt | ② |

## 2. Section 4 rows to add

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `fs/fs/tests/service.spec.ts`, `fs-local/tests/filesystem.spec.ts` | `crates/fs/tests/upstream_filesystem.rs` | Identity across aliases, absence as an answer, the error classes, guarded writes, the literal edit, listings, globs and deletes | ported: TC-PORT-FS-1..20 |
| `fs/fs-sandbox/tests/fs-sandbox.spec.ts` | `crates/fs/tests/upstream_sandbox.rs` | What the fence refuses, what a refusal says, and what each mode permits | ported: TC-PORT-FS-21..28, beside the containment half already at `crates/turn/tests/upstream_fs_containment.rs` |
| `fs/fs-observation-policy/tests/policy.spec.ts` | `crates/fs/tests/upstream_observation.rs` | A session may not overwrite or edit what it has not read, and may not write back over a file that moved | ported: TC-PORT-FS-29..36 |
| `fs/tool-fs/tests/tools.spec.ts`, `error.spec.ts`, `tool-fs-search/tests/tools.spec.ts` | `crates/fs/tests/upstream_fs_tools.rs`, `crates/fs/tests/turn_files.rs` | The roster and its schemas, the concurrency class per call, the read window, the refusal shape, and a whole turn reading and writing real files | ported: TC-PORT-FS-37..50 |
| `interaction/user-approval` applied to the loop | `crates/turn/tests/upstream_permission.rs` | The gate, the audit, the headless default, and that a refused call never runs | ported: TC-PORT-INT-1..11, beside TC-PORT-APPR-* for the seam itself |
| `interaction/permission-presets/tests/permission-presets.spec.ts` | `crates/fs/tests/upstream_presets.rs` | The switch, the fold, the table, and the round trip through a journal | part ported: TC-PORT-INT-12..16 |
| `interaction/user-questions/tests/user-questions.spec.ts`, `tool-ask-user/tests/tool-ask-user.spec.ts` | `crates/turn/tests/upstream_questions.rs` | What counts as an answer, what the journal records, and what a tool does with no answer | ported: TC-PORT-INT-17..28 |

## 3. What is unrepresentable, and why

Named here rather than left for a reader to notice missing.

- **Upstream's `streamText`, `readBytes` and `lstat`.** The service reads text
  whole under a stated cap and the window a model sees is the tool layer's, so
  there is no byte-level or streaming consumer to serve. `lstat` exists upstream
  so a consumer with trust-boundary rules can reject a repository-owned symlink
  *before* resolution follows it; tetanus fences at resolution, so a link out of
  the workspace is already refused and there is nothing for a second probe to
  decide. `FsTarget::path` answers what `processPath` answers; `fileUrl` has no
  consumer in this workspace.
- **`FsVersion` as an opaque token is kept; its derivation is not portable.**
  Upstream leaves the token to the backend, and so does this - the local one uses
  filesystem identity plus mtime plus size. A case asserting the token's *shape*
  would be asserting an implementation detail the contract says not to read, so
  the cases assert only that it moves when the file moves.
- **LF normalization and the diff basis.** Upstream normalizes storage text so
  `before`/`after` share a diff basis. tetanus carries both sides whole and
  computes no diff, because rendering one is the presentation lane's by
  `docs/interface-contract.md` §5. A backend that normalized would be changing
  bytes a model wrote, which is worse than leaving the diff to the renderer.
- **`grep` and `fd`.** Upstream's search tools shell out to `ripgrep` and `fd`.
  tetanus answers the path question in-process (`crates/fs/src/glob.rs`): a
  harness that needs an external binary to list files fails differently on every
  machine. Searching file *contents* is not served at all yet and is named in the
  gap column above rather than faked with a slower walk.
- **`read_image` and the attachment store.** No attachment store exists in this
  workspace, so there is nothing to durably commit image bytes to. Upstream's own
  tool is composition-conditional for the same reason.
- **Windows ACL behaviour** (`fs-local/tests/win32.spec.ts`). Out of scope for a
  Unix-hosted gate; the case would assert nothing on the machine that runs it.
- **Upstream's three `fs/*` waterfall events.** Upstream derives the write and
  edit intents through events so a deployment can omit the policy plugin and get
  unconditional mutation. tetanus makes the policy a value the tool layer holds
  and serves the same composition choice as `FsTools::unobserved`
  (TC-PORT-FS-34). An event seam whose only listener is the one plugin that ships
  with it is indirection without a second implementation.
- **Upstream's `custom` pseudo-preset.** It is the name upstream returns when the
  knobs match no table entry. Here `effective_preset` answers `None`, which says
  the same thing without reserving a name a table must then be checked against.
- **The `plan-review` question intent.** It changes how a surface draws a
  question and nothing about the protocol, so it is the presentation lane's.

## 4. Where tetanus deliberately differs

- **The fence judges reads too.** Upstream's sandboxed backend passes every read
  through untouched and judges only the two mutations. tetanus fences at
  resolution, so a read outside the workspace is refused as well. Strictly
  narrower, one rule instead of two, and it costs a coding agent nothing.
  `crates/fs/src/access.rs` carries the reasoning.
- **`danger-full-access` selects the backend rather than being a mode of the
  fenced one.** A confining type with a branch whose job is to skip the fence is
  a type where the one branch that matters is the one a mistake is silent in.
  `access::backend` is the single place the choice is made.
- **The widest preset still asks.** Upstream bundles `never` with
  `danger-full-access` because there `never` means "do not prompt" and its
  prompts are escalation requests that full access makes unnecessary. In tetanus
  `never` settles every ask `rejected` (contract §4.4.7), so that bundle would
  make the widest preset refuse the very calls the narrower one allows. A
  deployment that wants irreversible calls to run unattended attaches an answerer
  that grants them - a decision with a name and a code path.
- **A read limit past the cap is clamped, not refused** (TC-PORT-FS-42). Upstream
  refuses it. A model that asked for too much wanted the file, and a refusal
  costs a round trip to learn a number it could not have known.
- **Permission is `Allow` by default while every other classifier in the pipeline
  fails closed.** A harness that asked about every read would train whoever
  answers to approve without reading. The classifier *panicking* still fails
  closed - it asks - which is the conservative direction for this question, and
  TC-PORT-INT-10 pins it.

## 5. What this lane did not wire

The tools are composed by whoever builds a `ToolRegistry`
(`FsTools::new(backend, observed, session_id).register(&mut registry)`), and the
shipped binary's registry lives in `crates/cli/src/main.rs`, which
`docs/interface-contract.md` §4.7 gives to the presentation lane. Composing them
there - and the settings keys that would choose a preset per deployment - is one
line plus its tests in a lane this slice does not touch. Every case here composes
the suite the way that call would.
