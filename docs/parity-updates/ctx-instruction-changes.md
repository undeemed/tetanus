# Parity note: instructions that change under a session

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p6-ctx-rows`.
Scope: the `context/*` row's "re-rendering an instruction file a tool edited
mid-session", which `crates/turn/src/instructions.rs` deferred in its own module
docs when the discovery half landed.

---

## 1. What was built

`InstructionWatch` and `render_changes`: the files a session has already shown
the model, re-read at a turn boundary, with what changed reported as a
runtime-context part. TC-PORT-INSTR-13..20.

The gap it closes is not theoretical. An agent is routinely *asked* to edit
`AGENTS.md` - this project's own instructions to its agents say to - and until
now the session that did it went on being prompted with the version it read at
startup for the rest of its life. A model working from stale instructions is
worse than one working from none: it is confidently wrong, and the transcript
shows it being told the right thing.

## 2. Two decisions that differ from upstream

**A turn boundary, not a tool boundary.** Upstream reconciles inside the step,
off its `read`/`write`/`edit` tools' post-execute, and carries the result
through an inbox. tetanus reports at the start of the next turn, through the
runtime-context seam that landed earlier in this branch. The reading is then
one durable part of that turn's `context/snapshot`, beside the clock, rather
than a message injected mid-step whose position in the history depends on which
tool ran. It also costs no coupling to tool names: upstream's set is literally
`{read, write, edit}`, so a project whose tools are called anything else gets
no reconciliation at all. The case that would argue the other way - a step that
edits instructions and then acts on them - is a step acting on what it just
wrote, which it already knows.

**No scopes or baseline identity.** Upstream tracks per-scope version caches
and a baseline identity so it can replace the whole block when the project root
changes. tetanus re-renders only the changed files and keeps the original block
where it is. The workspace's discovery is root-to-cwd and its cwd is fixed for
a session (contract section 4.4.9: `cwd` is where the session was opened), so
the root cannot change under it and the identity has nothing to distinguish.

## 3. Row edit, section 3

**`context/*`, `guard/*`.** Gap: remove `re-rendering an instruction file a
tool edited mid-session`, leaving `timeout and repeat guards` (and see
`ctx-runtime-context.md` section 4 for tmux). Today: add `instruction files a
tool changed under the session reported at the next turn, whole rather than as
a diff, once`.

## 4. Row edit, section 4

| Row | Edit |
| --- | --- |
| `context/agent-instructions/tests/agent-instructions.spec.ts` | The row says "part ported: TC-PORT-INSTR-1..12". Add: "TC-PORT-INSTR-13..20 restate the reconciliation half - an edited file superseding what was loaded, a deleted one retracted, a new one added, each reported once, bounded and escaped by the same rules as the original block. Upstream's per-scope version cache and baseline identity have nothing to restate: discovery here is root-to-cwd from a `cwd` the session header fixes, so the root cannot change under a running session." |

## 5. Changelog row

| 2026-08-22 | Workspace instructions that change under a session (`crates/turn/src/instructions.rs`, TC-PORT-INSTR-13..20), the `context/*` row's last buildable clause. The block is rendered once and prepended to every request, so a session whose tools edit `AGENTS.md` - which is a thing an agent is routinely asked to do, including by this project's own instructions - went on being prompted with the version it read at startup. The model is told the whole new content rather than a diff, because a diff of guidance is a puzzle and the file as it now reads is the instruction; a deleted file is retracted with no content, because there is none and the only thing to say is that what it said no longer applies; and a change is reported exactly once, since a model told twice that a file changed reads it as a second edit that never happened. It arrives at the next turn boundary as a runtime-context part rather than inside the step that made the edit, which is where it differs from upstream deliberately: the reading is then one durable part of that turn's `context/snapshot` instead of a message whose position depends on which tool ran, and it costs no coupling to tool names - upstream's reconciliation triggers on a literal `{read, write, edit}` set, so a deployment whose tools are named anything else gets none. The change block is bounded in whole files and escapes the closing delimiter by the same rules as the original, asserted here rather than inferred, because a second renderer of the same untrusted text is a second chance to forget. Upstream's per-scope version cache and baseline identity have nothing to restate: discovery is root-to-cwd from the `cwd` contract section 4.4.9 fixes at session creation, so the project root cannot change under a running session. |
