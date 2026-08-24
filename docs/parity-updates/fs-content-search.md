# Parity update: searching file contents

Written by the filesystem lane, for a reconciliation slice to fold into
[`../parity.md`](../parity.md). Nothing here edits that file: every lane
collides on it, and this note is the only copy of what is below until it lands.

Branch: `fm/tetanus-p2-fs`.
Closes one clause of the `fs/*` row's Gap column.

## 1. Section 3, the `fs/*` row

**Today**: `seven model-facing tools` becomes `eight model-facing tools`, and
the clause gains:

> including a content search that answers with the matching lines, their file
> and their line number, bounded in matches rather than in bytes and read
> through the same service the other tools use, so the fence and the kernel
> worker judge a search exactly as they judge a read

**Gap**: remove `a search tool over file *contents* (\`grep\`)`. What is left in
that column is unchanged: read windows over bytes rather than text, image reads
and the attachment store they need, and presentation of a diff.

## 2. Section 4, the `tool-fs` row

Its `State` cell gains, after the existing sentence about `grep` and `fd`:

> The content search is served in process (TC-PORT-FS-51..57): upstream shells
> out to `ripgrep`, and the reason tetanus does not is the reason it does not
> shell out to `fd` either - a harness that needs an external binary to answer
> a question fails differently on every machine. What upstream gets from
> `ripgrep` and this does not is its ignore-file handling and its speed on a
> very large tree; what this has instead is one bound on matching lines and an
> answer that says when it stopped.

## 3. Three decisions, and why they are not obvious

- **A search reads through the `FileSystem` seam.** It could have walked the
  directory itself and been faster. It does not, because the fence, the
  observation policy and the Landlock worker all live behind that seam, and a
  second path into the filesystem would be a second answer to "may this be
  read" - the asymmetry `crates/sandbox` exists to prevent, arriving from the
  other side.
- **An unreadable file is skipped, counted and reported.** Not fatal, because
  one image in a source tree must not fail a call; not silent, because a search
  that steps over a file quietly has answered "no matches" about something it
  never looked at. The count is in the header of the answer.
- **A search is not an observation.** TC-PORT-FS-57 pins that a file a search
  matched still cannot be overwritten without being read. A search shows a
  model a handful of lines out of a file it has otherwise never seen, so
  counting it would let a model grep for one word and replace everything -
  which is the exact failure the read-before-write rule exists to stop.

## 4. What this does not do

- **No ignore-file handling.** `.gitignore` is not read, so a search under a
  workspace with a `target/` directory searches it. The glob argument is the
  control, and narrowing it is one argument rather than a configuration file
  this crate would have to learn to parse.
- **No replace.** Upstream's search tools do not offer one either, and a
  content edit goes through `edit`, which is guarded by the version the file
  had when it was read.
- **No byte offsets.** A match is a line, because a line is what a model reads.
  Byte-level access is the remaining `read windows over bytes` clause of the
  same row, and it has a different consumer.

## 5. Changelog row

| 2026-08-22 | A content search in the filesystem tools (`crates/fs/src/tools.rs`, TC-PORT-FS-51..57), closing the `grep` clause of the `fs/*` row. Upstream shells out to `ripgrep`; this answers in process for the reason `glob` already answers the path question that way - a harness that needs an external binary to answer a question fails differently on every machine. It reads through the `FileSystem` seam rather than walking the disk, so the fence and the Landlock worker judge a search exactly as they judge a read, and a search cannot become a second path into the filesystem with its own idea of what is allowed. A file it cannot read is skipped, counted and reported: not fatal, because one image in a source tree must not fail the call, and not silent, because a search that steps over a file quietly has answered "no matches" about something it never looked at. The bound is on matching lines rather than bytes, since a search is worth having only while it is cheaper than reading the files, and the answer says when it stopped so a model narrows its pattern instead of believing it has seen everything. The matcher is the `regex` crate for the property its documentation leads with - linear time, no backreferences - because the pattern comes from the model. One decision is pinned by a case rather than stated: TC-PORT-FS-57 holds that a search is not an observation, so a file a search matched still cannot be overwritten without being read, because otherwise a model could grep for one word and replace everything. Writing the cases caught a defect in the first cut: a glob answers with directories too, and reading one counted as an unreadable file, so a search of any nested tree reported skipped files that were not files. |
