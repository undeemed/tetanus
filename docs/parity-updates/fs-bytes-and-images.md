# Parity update: byte windows and image reads

Written by the filesystem lane, for a reconciliation slice to fold into
[`../parity.md`](../parity.md). Nothing here edits that file, and this note is
the only copy of the row edits until it lands.

Branch: `fm/tetanus-p2-fs-bytes`.
Closes two clauses of the `fs/*` row's Gap column.

## 1. Section 3, the `fs/*` row

**Today**: `eight model-facing tools` becomes `nine model-facing tools`, and
the clause gains:

> a byte window under the text read (`read_bytes`), which carries neither of
> the limits `read` does - not UTF-8, not the text cap - so a caller that knows
> what it is looking at can have a header without a whole file; and a picture
> read into an attachment store rather than into the turn, answered as an id, a
> media type, a size and dimensions, with the store behind a one-method seam so
> the file tools do not depend on the crate that holds it

**Gap**: remove `read windows over bytes rather than text` and `image reads and
the attachment store they need`. What remains in that column is presentation of
a diff, which is the presentation lane's by the interface contract.

## 2. Section 4, the `tool-fs` row

Its `State` cell gains:

> Byte windows and image reads are served (TC-PORT-FS-58..62). Upstream's
> `readBytes` is the same primitive with the same reason - its text read
> refuses what is not text, and a picture is not text - and an offset past the
> end answers empty here rather than refusing, because a caller windowing
> through a file meets that boundary on its last read every time. Upstream's
> `read_image` returns an attachment reference and so does this; what differs
> is where the store lives, because `crates/fs` deliberately does not depend on
> the crate that holds it (`ImageSink`, one method, supplied by the
> composition).

## 3. Three decisions worth stating

- **The seam, not the dependency.** `ARCHITECTURE.md` §4.2 says nothing depends
  on `tetanus-fs` because it is a consumer of the tool seam rather than a layer
  under it. A `crates/fs` that reached into `crates/features` for the
  attachment store would invert that, and would make the file tools
  unavailable to any composition that did not also want the feature tools.
- **A refusing sink rather than an absent tool.** With no store composed,
  `read_image` is still registered and answers with what is missing and what to
  do instead. A tool that vanished tells a model nothing, and tells its author
  nothing either - a build that dropped it looks exactly like one that never
  had it.
- **The media type comes from the bytes.** The name came from a model reading a
  directory listing; the bytes are the thing being stored. An unrecognised
  header is stored as octets rather than refused, because a picture this build
  cannot name is still a picture somebody can open, and the four signatures
  recognised are the four `crates/features` can already measure - a longer
  table would be a second sniffing implementation to keep in step with the one
  that reads dimensions.

## 4. What is left in this area

- **Presentation of a diff**, which the interface contract gives to the
  presentation lane.
- **A thumbnail route.** `read_image` answers an id and the bytes are in the
  store; fetching them over the boundary is the `attachment` half of the view
  types the contract's §5.1 defers, and the presentation lane has said it wants
  `workspace.view` first.

## 5. Changelog entry

Written as its own file by `docs/tools/parity-changelog.py add`:
`docs/parity-changelog.d/2026-08-24-a-byte-window-under-the-text-afd05c92.md`.

Not a row to paste into `docs/parity-changelog.md`. That file is rendered from
the entry directory, so a row added to it by hand disappears on the next build,
with no error and no conflict - which is the loss the one-file-per-entry split
exists to prevent.
