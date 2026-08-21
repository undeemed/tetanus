# Parity: goals, plans and skills - registered and empty

Upstream: [`client/ui-goal`], [`client/ui-plan`], [`client/ui-skill`].

tetanus: `web/app/features.js`.

## Why this one is a frame and not a view

The features crate is real, and it is on `fm/tetanus-p2-features`:
`crates/features/src/{goal,plan,skill}.rs`, writing `goal/changed`,
`plan/mode` and `plan/presented`. None of those types is on this tree, on
master, or in the event vocabulary this branch's contract publishes - §4.3.2
names ten durable types and six staged, and these are not among them.

So the rule holds: register the view, leave it empty, do not fake a shape you
cannot see. Reading `event.data.goal.objective` today would couple this file to
a struct on an unlanded branch that is still free to change, and would draw a
panel nothing here can produce.

## What that means concretely

| Piece | State |
| --- | --- |
| `PANELS` | names the three event types, with one line each saying what the view will read when the shape is published |
| `standing()` | a real fold: newest record per type wins, because these are standing facts and not a history. Yields nothing on this tree, which is the correct answer |
| `panel()` | draws what the fold found, or the empty state |
| the events themselves | not swallowed - the page still draws an unclaimed durable type raw, which is what §4.3.2 asks for and what makes a landed event visible on day one |

## The empty state distinguishes two nothings

"This run has no goal or plan yet" and "Goals, plans and skills are not part of
this build yet" are different sentences, because they call for different
things: one is a prompt away, the other is a lane landing. A single blank panel
would tell a reader neither.

## What lands next, and how small it is

When the features lane lands, each view is one entry in `PANELS`:

- **goal** reads the objective, the phase (`active`, `paused`, `blocked`,
  `complete`) and the blocker's code and message while blocked;
- **plan mode** reads whether the agent is planning rather than acting;
- **plan** reads the markdown plan put up for review - which is why the
  markdown family is the slice after this one.

## Tests

`target/probe-primitives.mjs`, **61/61**: the three types registered with no
view reading a field, each carrying its `reads` note; the fold yielding nothing
for a journal this build writes and the newest record for one that carries two;
and the two empty states told apart.
