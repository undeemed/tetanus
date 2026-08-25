# Note: the panel's reads, checked against the contract

Written by the filesystem lane, taking the `web/app` coverage gap this lane's
own backlog audit named - and taking the part of it that touches no file the
presentation lane owns. Nothing here edits `web/app`, `docs/parity.md` or the
boundary document.

## Why this, out of everything the coverage gap contains

The audit's finding was that `web/app` has no automated cases anywhere. The
whole of that is a browser-testing decision - a runner, a headless browser, a
CI job - and it belongs to the lane that owns the page.

One part of it does not need any of that, and the presentation lane had already
written down why it cannot fix it from its side:

> a field this page reads that is later renamed fails silently, drawing an
> empty panel rather than a build error.

JavaScript has no build to break. Rust has one, so the check belongs on the
engine's side of the seam, where a rename is *made*: `crates/protocol/tests/panel_fields.rs`
scans `web/app/*.js` for `data.<field>` and asserts every name appears in
`docs/interface-contract.md`. It costs the panel nothing and it fails the
engine's build the day a published field is renamed under it.

TC-PANEL-FIELD-1 is that check. TC-PANEL-FIELD-2 is the one that keeps the
check honest, and it exists because of a defect in its own first cut.

## The defect the first cut had, which is the point of the second case

The first scan matched `data\.[a-z_]+`, which truncates a camel-cased name at
its first capital. It reported four fields as unpublished:

    duration   exit   handler   stderr

and every one of those was a fragment: the panel reads `data.durationMs`,
`data.exitCode`, `data.handlerId` and `data.stderrSummary`, all four of which
the contract names and `crates/hooks` writes. The probe was wrong and the panel
was right.

That is the failure mode a checker of *names* has to be built against: a wrong
probe does not fail quietly, it accuses the other side with a specific and
plausible list. Had it been reported rather than checked, it would have cost
the presentation lane an afternoon looking for a defect that was in my
regular expression. TC-PANEL-FIELD-2 pins the scan against exactly that -
a camel-cased read taken whole, and `metadata.point` not read as a field.

## What it does not do

- **It does not run the panel.** No browser, no DOM, no rendering. It is a text
  comparison, and it says so.
- **It does not check values.** That a field exists is not that it carries
  something sensible; the shape cases in `crates/engine` do that from the
  engine's side.
- **It does not constrain the panel.** A name the panel stops reading simply
  stops being checked. Nothing here tells the presentation lane what to draw.

## Rows

No section 3 or section 4 row changes: this adds a case over an agreement both
documents already state, rather than serving any upstream behaviour. The
`client/*` row stays out of scope by section 5, and this note exists so the
next reader of that row knows the seam under it is now defended.

## Changelog entry

Written as its own file by `docs/tools/parity-changelog.py add`, not as a row
in the rendered table.
