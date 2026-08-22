# Contract note: the workspace and attachment surfaces

Slice: `tetanus_features::view` - the vocabulary a surface reads the feature
state through.
Branch: `fm/tetanus-p2-ui-surfaces`, on top of `fm/tetanus-p2-features`.
For folding into [`../interface-contract.md`](../interface-contract.md) by the
reconciliation slice.

**Read this before writing a panel.** The shapes below are the whole contract;
everything else in `crates/features` is the engine side and a surface should not
reach for it. Nothing here is on the wire *yet* - §3 says what it would take -
so a surface built against this today builds against a Rust type, and the field
names are chosen so that adding the wire later changes nothing on your side.

## 1. The two surfaces

**`SessionView`** is folded from one session's journal in a single pass and
describes one moment. **`WorkspaceView`** is read from the filesystem and
changes when the disk does, not when the session does - which is why they are
two types and refresh on two rhythms.

```jsonc
// SessionView
{
  "as_of_seq": 42,          // journal position this view folded; -1 for an empty log
  "todos": {                // null before the first write, and after a later turn began
    "items": [ { "content": "port the cases", "status": "pending" } ],
    "pending": 1, "in_progress": 0, "completed": 0
  },
  "goal": {                 // null before the first create and after a clear
    "revision": 2,
    "objective": "ship the parser",
    "phase": "blocked",     // active | paused | blocked | complete
    "blocker": { "code": "needs-credential", "message": "the deploy key is not here" }
  },
  "plan": { "active": true, "presented": "1. read it\n2. rewrite it\n" },
  "feedback": { "count": 3, "latest": { "text": "no test runner", "author": "model" } },
  "attachments": [
    { "id": "9f...-6", "name": "shot.png", "media_type": "image/png",
      "bytes": 20614, "dimensions": { "width": 320, "height": 200 } }
  ]
}
```

```jsonc
// WorkspaceView
{
  "root": "/srv/project",
  "cwd": "/srv/project/crates/parser",  // null when it equals the root
  "marker": ".git",                     // null when no marker was found
  "entries": [ { "name": "src", "directory": true } ],
  "truncated": false,
  "instructions": [ "AGENTS.md" ]       // named as the prompt names them
}
```

## 2. The five decisions a panel author needs

**`as_of_seq` is how you order two views.** A live panel receives folds out of
order eventually; without a position it cannot tell which is newer, and a stale
one cannot say that it is stale. `-1` means the log is empty, matching
`SessionInfo.last_seq` in §4.3.

**`null` and empty are different, everywhere they appear.** `todos: null` is "no
plan yet"; `todos.items: []` is "a plan the model emptied". `goal: null` before a
create *and* after a clear - if you need to tell those apart, the journal carries
the tombstone and `goal::was_cleared` reads it. `cwd: null` means it equals the
root, so you do not draw the path twice. `marker: null` means no repository
marker was found and the working directory is standing in, which is the
difference between "this is a project" and "this is a directory" and leads a
user to different next actions.

**A view never carries bytes.** An attachment is named, measured and described;
`id` is a content address, so equal bytes always give an equal id and a
thumbnail cache keyed on it is correct by construction. Fetch content by id -
`view::attachment_path(store_root, id)` today, a call when §3 lands. Putting
base64 in a fold would make a frame nobody can read, a log line nobody can grep,
and a memory spike on every subscriber, for a thumbnail the surface wants once.

**A view never carries presentation.** No rendered markdown, no truncation, no
localized strings, no colour. `plan.presented` is the model's markdown exactly as
written. The surface knows the width and the theme; this crate does not, and a
guess here would be a guess every consumer then has to undo.

**`goal.revision` is load-bearing, not decoration.** A "pause" button sends back
the revision it was drawn with. If the goal moved since, the call is refused
rather than applied to something else - which is the difference between a stale
click doing nothing and a stale click pausing a goal the user never saw.

## 3. What it would take to put these on the wire

Nothing in `crates/protocol` changes in this slice, deliberately: these types
live in `crates/features`, and a surface inside this workspace can hold them.
Publishing them over the JSON-RPC boundary is a separate change, and it is these
three things -

- two calls, `session.view` and `workspace.view`, in §4.2's table, both reads and
  both idempotent by §4.4.12;
- the structs above added to `crates/protocol::types`, which is a **minor**
  change by §5 for a client that matches with a rest pattern and a build break
  for one that does not;
- a push, if a panel is to be live rather than polled - and the honest cheaper
  answer is that a client already receives `session/event` and can re-fold, so a
  push should wait until somebody has measured the polling.

Deferred rather than skipped, because §5's rule is that a type the presentation
lane constructs lands when both lanes take it, and this note exists so the other
lane can say what it needs before that is settled.

## 4. Growing these types

Adding a field is minor; removing or renaming one is major. The cost to you is
one habit: **match a struct with a rest pattern** (`let View { todos, .. }`), or a
field added later stops your build. This is §5's fourth compatibility rule, and
it is the one nothing on the engine side can verify for you.

Two fields are strings where an enum would be tempting - `todo.status` and
`goal.phase` - so that a value added later renders as itself rather than failing
to parse the whole view. Match the ones you know and show the rest verbatim.

## 5. What is deliberately not here

- **A feedback list.** `count` plus the newest entry, because a session that
  reported forty times should not put forty strings in every fold. The journal
  has all of them.
- **Skills.** The roster is settled when the tools are composed rather than
  folded per session, so it is not session state; a surface that wants to list
  skills reads `skill::discover` once. Say so if you need it per session and it
  becomes a third view rather than a field on this one.
- **Attachment content, thumbnails, and any image decoding.** §2's third rule.
- **Per-turn or per-step slicing.** Every view here is "the state now". A
  timeline is the journal's job, and `crates/cli/src/render/timeline.rs` already
  reads it that way.
