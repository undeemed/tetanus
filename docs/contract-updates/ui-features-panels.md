# Contract note: what the presentation lane needs from the feature surfaces

This is the reply [`features-ui-surfaces.md`](features-ui-surfaces.md) asks for.
Its §3 defers `session.view` and `workspace.view`, and closes with "this note
exists so the other lane can say what it needs before that is settled". This is
that, written after building the panels rather than before, so every item below
is a thing the building actually ran into.

Nothing here is a blocker. `web/app/features.js` draws all six panels today, by
the route §3 itself offers: the page subscribes from seq 0, receives
`session/event`, and re-folds. What follows is what that costs and what would
make it cheaper, in the order the cost is felt.

## 1. Two of the six panels cannot be drawn at all, and it is not the fold

`skill` and `workspace_info` have no durable event. The other four write one -
`goal/changed`, `plan/mode`, `plan/presented`, `todo/write`, plus
`feedback/recorded` and `attachment/added` - so a page that folds the journal
has them. `WorkspaceView` is explicitly *not* folded from the journal ("it is
read from the filesystem, it changes when the disk changes"), and skills are
discovered from disk too.

So for these two the re-fold answer does not apply, and there is no page-side
substitute: a browser cannot read the project's directory. **`workspace.view`
is the one call this lane would take first**, ahead of `session.view`, because
`session.view` has a working alternative and this has none.

Today the workspace panel is served instead by `host.listDirectory`, which is a
directory chooser and not a project description: it cannot say which marker
identified the root, whether the listing was truncated, or which instruction
files the project keeps - the three facts `WorkspaceView` exists to carry, and
the three a reader uses to tell "this is a project" from "this is a directory".

## 2. `as_of_seq` has no equivalent in a re-fold, and it is the one that bites

A page folding `session/event` knows the `seq` of the last event it applied, so
in the ordinary case it can answer the same question. What it cannot do is
answer it about a *fold it did not perform*: there is nothing to compare
against, because there is no second source. That is fine while re-folding is
the only route and becomes wrong the moment `session.view` lands beside it -
at which point a page holding both needs `as_of_seq` to order them, which is
exactly what the field is for. Recorded here so it is not discovered then.

## 3. Three payload shapes are folded from prose in the event, not from a view

`SessionView` is a clean projection; the events are not always the same shape,
and one of them needed reading twice:

- **`goal/changed` carries two shapes.** A create or update writes
  `{ operation, goal }`; a clear writes `{ operation: "clear", cleared: {
  revision, objective } }` with no `goal` key at all. A consumer that reads
  `data.goal` gets `undefined` on a clear and draws nothing, which is the one
  case where the journal deliberately keeps a tombstone so that "no goal yet"
  and "the goal was put down" can be told apart. It would cost nothing to say
  this in the note beside the `SessionView` example, where a panel author looks.

- **`todo/write` counts are recomputed.** `TodoListView` carries `pending`,
  `in_progress` and `completed` beside the items, with the stated reason that a
  surface should not fold the same list twice. The event carries only
  `{ todos }`, so a page folding events counts them itself - which is fine
  arithmetic, but it is arithmetic in a second place, and the view exists
  precisely to avoid that.

- **`feedback/recorded` is the entry itself**, not wrapped in a key, where its
  neighbours all wrap. Not a problem, but the asymmetry cost a reading of the
  Rust to confirm.

## 4. What this lane does not need

**Not a push.** §3 wonders whether one is wanted and answers "wait until
somebody has measured the polling". Agreed, and now measured on the smaller
case: this page re-folds all six panels on every open of the trace dialog, over
the whole journal, and it is not perceptible. A push for these types would be a
mechanism to maintain for something nothing is waiting on.

**Not rendered markdown.** `plan.presented` staying the model's own markdown is
right and this page renders it. The same goes for the two string-typed fields:
`todo.status` and `goal.phase` are drawn as themselves when this build has never
heard of them, which is the behaviour §4 asks for and which one of the probe's
cases pins.

**Not bytes.** `attachment/added` naming and measuring without carrying content
is right, and the panel shows name, media type, size and dimensions without
ever asking for the object. It does need the fetch-by-id route eventually - a
thumbnail is the obvious next thing - but not before there is a call to make.

## 5. The one thing that would change nothing on this side

§4's rest-pattern rule has no cost in JavaScript: a page reads the fields it
knows and ignores the rest by construction, so a field added to any of these
types cannot break this consumer. The reciprocal risk is real though and worth
stating from this side: **a field this page reads that is later renamed fails
silently**, drawing an empty panel rather than a build error. The six fields
this lane would notice being renamed are `goal.objective`, `goal.phase`,
`goal.blocker.message`, `todo.content`, `todo.status` and `attachment.name`.
