# Parity changelog

Every change to [parity.md](parity.md) adds an entry here, newest last.

**This file is generated. Do not edit it.**
One entry is one file in [`parity-changelog.d/`](parity-changelog.d), and
[`docs/tools/parity-changelog.py`](tools/parity-changelog.py) renders them into
the table below.

**Why one file per entry.**
This was a single table that every slice in flight appended a row to, which
made it the most-shared line in the repository: at the worst measurement, eight
of ten open pull requests touched it, so every merge forced the other seven to
hand-resolve the same non-disagreement before they could land. `.gitattributes`
marked it `merge=union` to keep both sides automatically, and that works where
the driver runs - but a merge driver is client-side configuration, and the real
problem was that the lines were shared at all. One file per entry means two
lanes writing at once touch nothing in common, so no driver has to be right.

**Adding one.**
`python3 docs/tools/parity-changelog.py add "your entry"`. The filename carries
a hash of the text, so two lanes cannot pick the same name without having
written the same entry. Rendering is a separate step run by a single writer -
if it were required for a green build, every pull request would regenerate this
file and it would become the shared line again. That is why the table below may
lag the directory; `parity-changelog.py check` says whether it does.

**The one thing to know about the order.**
Entries migrated from the old table carry an `order` field recording the
position they held in it. That table was in *append* order rather than date
order - union merges interleaved it, and its dates go backwards in seven places
- so sorting the migration by date would have silently rewritten the sequence
of forty-odd historical rows while the entry count stayed the same. New entries
carry no position and sort after every migrated one, by date and then filename.

**Entries are historical facts.**
Write one, never revise one; a correction is a new entry saying what it
corrects. `parity.md` itself is edited in place, and a conflict there is
information.
