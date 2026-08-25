#!/usr/bin/env python3
"""Prune stale cargo artifact generations from a target directory.

Building this workspace costs a lot of disk. Every time a crate's inputs
change, cargo writes a new hash-generation of its artifacts into
`<target>/debug/deps` and leaves the previous ones there, so a long-lived
target directory accumulates generations nobody will ever link again. On a
shared machine that is how a build host fills up.

This keeps the newest few generations per target stem and deletes the rest.
Everything it removes is an artifact cargo can rebuild - never source, never
git state, never a work product.

**Prune; do not delete the target directory.** Deleting the whole thing
"works" and costs a full cold rebuild, which on this workspace is long enough
that an idle lane looks like a wedged one from outside. Keeping two
generations reclaims most of the space for a fraction of the rebuild.

**It holds cargo's build lock while it runs.** `<target>/debug/.cargo-build-lock`
is the file cargo itself locks for the duration of a build, so taking it here
means a concurrent `cargo build` waits for the prune instead of racing it and
finding its artifacts removed mid-link. Without the lock this is a coin toss
that usually pays out, which is worse than one that does not - it fails rarely
and confusingly, under load, on someone else's lane.

Usage:

    prune-cargo-gens.py <target-dir> [keep]      # keep defaults to 2
    prune-cargo-gens.py <target-dir> 2 --dry-run
"""

from __future__ import annotations

import argparse
import collections
import fcntl
import os
import re
import sys
from pathlib import Path

# `libfoo-0123456789abcdef.rlib`, `foo-0123456789abcdef`, and the rest of the
# family cargo writes per generation. The stem is the unit; the hex is the
# generation.
ARTIFACT_RE = re.compile(r"^(?:lib)?(.+?)-([0-9a-f]{16})(\.|$)")

BUILD_LOCK = ".cargo-build-lock"


def die(message: str) -> int:
    print(message, file=sys.stderr)
    return 2


def prune(target: Path, keep: int, dry_run: bool) -> int:
    deps = target / "debug" / "deps"
    if not deps.is_dir():
        # Refused rather than treated as "nothing to do": the overwhelmingly
        # likely cause is a mistyped path, and silently succeeding on one is
        # how a caller concludes the disk problem is elsewhere.
        return die(
            f"{deps} is not a directory.\n"
            "Point this at a cargo target directory (the one CARGO_TARGET_DIR "
            "names), not at a source tree."
        )
    if keep < 1:
        return die("keep must be at least 1: keeping zero generations would "
                   "delete artifacts of the build that is current")

    lock_path = target / "debug" / BUILD_LOCK
    # Created if absent: a target directory that has never been built has no
    # lock file, and refusing there would make the first prune the odd one out.
    lock = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o666)
    try:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(
                f"waiting for the cargo build lock at {lock_path} ...",
                file=sys.stderr,
            )
            fcntl.flock(lock, fcntl.LOCK_EX)

        # stem -> generation -> [bytes, newest mtime, files]
        stems: dict[str, dict[str, list]] = collections.defaultdict(
            lambda: collections.defaultdict(lambda: [0, 0.0, []])
        )
        for entry in os.scandir(deps):
            if not entry.is_file(follow_symlinks=False):
                continue
            match = ARTIFACT_RE.match(entry.name)
            if not match:
                continue
            stat = entry.stat()
            generation = stems[match.group(1)][match.group(2)]
            generation[0] += stat.st_size
            generation[1] = max(generation[1], stat.st_mtime)
            generation[2].append(entry.name)

        removed = 0
        freed = 0
        for _stem, generations in stems.items():
            # Newest first by mtime, so what survives is what a build is most
            # likely to want next.
            order = sorted(generations.items(), key=lambda kv: -kv[1][1])
            for _hash, (size, _mtime, names) in order[keep:]:
                for name in names:
                    path = deps / name
                    if dry_run:
                        removed += 1
                        continue
                    try:
                        path.unlink()
                        removed += 1
                    except FileNotFoundError:
                        # A concurrent cargo may have cleaned it first; that is
                        # the same outcome by another route.
                        continue
                freed += size

        verb = "would remove" if dry_run else "removed"
        print(f"{target.name}: {verb} {removed} files, {freed / 2**30:.2f}G")
        return 0
    finally:
        os.close(lock)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("target", type=Path, help="the cargo target directory")
    parser.add_argument(
        "keep",
        nargs="?",
        type=int,
        default=2,
        help="generations to keep per target stem (default: 2)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what would be removed and remove nothing",
    )
    args = parser.parse_args()
    return prune(args.target.resolve(), args.keep, args.dry_run)


if __name__ == "__main__":
    raise SystemExit(main())
