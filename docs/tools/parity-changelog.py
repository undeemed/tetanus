#!/usr/bin/env python3
"""Assemble the parity changelog from one file per entry.

The changelog used to be one table that every slice in flight appended a row
to. That made it the most-shared line in the repository: at the worst
measurement, eight of ten open pull requests touched it, so every merge forced
the other seven to hand-resolve the same non-disagreement before they could
land.

`.gitattributes` had it marked `merge=union`, which is git's built-in driver
for exactly this and which works - where the driver runs. A merge driver is
client-side configuration read from `.gitattributes`, so it helps a local
rebase and cannot be relied on for a merge performed by a forge. The fix here
is not a better driver: it is removing the shared lines, so no driver has to be
right.

One entry is one file. Two lanes writing at once touch no common line, so there
is nothing to conflict and nothing to resolve.

    add      write a new entry file, named so two lanes cannot collide
    build    render the published changelog from the entry directory
    check    report whether the published file matches the directory

`check` is deliberately not something to wire into per-pull-request CI. If a
green build required the rendered file to be current, every pull request would
have to regenerate it, and the rendered file would become the shared line this
whole change exists to remove. Run it where a single writer runs: a release
step, or by hand.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from datetime import date as date_cls
from pathlib import Path

DOCS = Path(__file__).resolve().parent.parent
ENTRIES = DOCS / "parity-changelog.d"
HEADER = DOCS / "parity-changelog.head.md"
PUBLISHED = DOCS / "parity-changelog.md"

ENTRY_RE = re.compile(
    r"^---\ndate:\s*(\d{4}-\d{2}-\d{2})\s*\n(?:order:\s*(\d+)\s*\n)?---\n(.*)$",
    re.S,
)
# Entries migrated out of the single-table changelog carry `order`: the
# position they held in that table, which was append order rather than date
# order. That order is real information and is not reconstructible from the
# dates, so it is recorded rather than inferred. New entries do not carry it -
# an allocated position is a shared resource, which is the thing this whole
# change removes - and sort after every migrated one.
UNORDERED = 10**9
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


class EntryError(Exception):
    """An entry file that cannot be rendered, named so a caller can say which."""


def slugify(text: str) -> str:
    """A short, readable stem from the entry's own opening words.

    Readability only. Uniqueness is the hash's job, because a slug derived from
    text is exactly the kind of name two lanes can pick at once.
    """
    words = re.findall(r"[a-z0-9]+", text.lower())
    stem = "-".join(words[:6])[:48].strip("-")
    return stem or "entry"


def entry_name(day: str, text: str) -> str:
    """The filename for one entry.

    `<date>-<slug>-<hash>.md`. The date sorts, the slug is for a human reading
    the directory, and the hash is what makes the name collision-free without
    any lane having to ask another what it is about to write. Two lanes writing
    different entries cannot collide; one lane writing the same entry twice
    produces the same name, which is the harmless case.
    """
    digest = hashlib.sha256(text.strip().encode("utf-8")).hexdigest()[:8]
    return f"{day}-{slugify(text)}-{digest}.md"


def read_entry(path: Path) -> tuple[str, int, str]:
    """One entry's date, historical position and text, or an error naming it."""
    raw = path.read_text(encoding="utf-8")
    match = ENTRY_RE.match(raw)
    if not match:
        raise EntryError(
            f"{path.name}: expected a `---\\ndate: YYYY-MM-DD\\n---` header "
            "followed by the entry text"
        )
    day, order, body = match.group(1), match.group(2), match.group(3)
    # A table cell is one line. Entry files are written as prose and may wrap,
    # so the text is joined here rather than the author having to write one
    # very long line and a reviewer having to read it.
    text = " ".join(body.split())
    if not text:
        raise EntryError(f"{path.name}: has a date but no text")
    if "|" in text:
        raise EntryError(
            f"{path.name}: contains a `|`, which would end the table cell early"
        )
    return day, int(order) if order is not None else UNORDERED, text


def load_entries() -> list[tuple[str, str, str]]:
    """Every entry, in published order: historical position first, then date,
    then filename.

    Position before date, which looks wrong and is not. The table this was
    migrated from was in *append* order, not date order - union merges
    interleaved it, and it goes backwards across a date boundary in seven
    places. Sorting by date would silently rewrite the order of forty-odd
    historical rows while the entry count stayed the same, so the migration
    would look clean and would not be.

    Entries written from now on carry no position and therefore sort after
    every migrated one, by date and then filename. That is stable and needs no
    coordination: any *total* order across lanes - a sequence number, a
    position in a list - would be a shared resource to allocate, which is the
    problem this file exists to remove.
    """
    if not ENTRIES.is_dir():
        return []
    found = []
    problems = []
    for path in sorted(ENTRIES.glob("*.md")):
        try:
            day, order, text = read_entry(path)
        except EntryError as error:
            problems.append(str(error))
            continue
        found.append((day, order, path.name, text))
    if problems:
        raise EntryError("\n".join(problems))
    return sorted(found, key=lambda row: (row[1], row[0], row[2]))


def render() -> str:
    header = HEADER.read_text(encoding="utf-8").rstrip("\n")
    lines = [header, "", "| Date | Change |", "| --- | --- |"]
    lines += [f"| {day} | {text} |" for day, _order, _name, text in load_entries()]
    return "\n".join(lines) + "\n"


def cmd_add(args: argparse.Namespace) -> int:
    day = args.date or date_cls.today().isoformat()
    if not DATE_RE.match(day):
        print(f"not a date: {day}", file=sys.stderr)
        return 2
    text = args.text if args.text is not None else sys.stdin.read()
    text = " ".join(text.split())
    if not text:
        print("refusing to write an empty entry", file=sys.stderr)
        return 2
    if "|" in text:
        print("an entry may not contain `|`: it would end the table cell", file=sys.stderr)
        return 2
    ENTRIES.mkdir(parents=True, exist_ok=True)
    path = ENTRIES / entry_name(day, text)
    path.write_text(f"---\ndate: {day}\n---\n{text}\n", encoding="utf-8")
    print(path.relative_to(DOCS.parent))
    return 0


def cmd_build(_args: argparse.Namespace) -> int:
    PUBLISHED.write_text(render(), encoding="utf-8")
    print(f"{PUBLISHED.relative_to(DOCS.parent)}: {len(load_entries())} entries")
    return 0


def cmd_check(_args: argparse.Namespace) -> int:
    want = render()
    have = PUBLISHED.read_text(encoding="utf-8") if PUBLISHED.exists() else ""
    if want == have:
        print(f"up to date: {len(load_entries())} entries")
        return 0
    # Lagging is expected between releases and is not an error a build should
    # fail on; the exit code is here for a release step that wants to act.
    print(
        f"{PUBLISHED.name} lags the entry directory "
        f"({len(load_entries())} entries); run `parity-changelog.py build`",
        file=sys.stderr,
    )
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    add = sub.add_parser("add", help="write one new entry file")
    add.add_argument("--date", help="YYYY-MM-DD; today when omitted")
    add.add_argument("text", nargs="?", help="the entry text; stdin when omitted")
    add.set_defaults(func=cmd_add)

    sub.add_parser("build", help="render the published changelog").set_defaults(
        func=cmd_build
    )
    sub.add_parser("check", help="report whether the published file is current").set_defaults(
        func=cmd_check
    )

    args = parser.parse_args()
    try:
        return args.func(args)
    except EntryError as error:
        print(error, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
