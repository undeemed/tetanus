#!/usr/bin/env python3
"""Refresh `upstream/` from a DeepSeek Harness checkout.

`upstream/` is not the whole of their client - it is the closure of what the
conversation view actually *runs*, which is a twentieth of their web app by
line count because most of what a component imports is types the transpiler
erases. That closure is a fact about their source, so it is computed rather
than listed: hand-listing it went stale twice inside one afternoon.

Their specs come with their components. A UI ported without its tests is a UI
nobody can change, and upstream's suites are MIT like the rest of it. A spec
that cannot come across is written to `upstream/SPECS-NOT-PORTED.txt` with the
reason, because a silently dropped test is worse than a missing one.

Usage:

    python3 tools/vendor.py <path-to-deepseek-harness-checkout>

The checkout is read and never written.
"""

from __future__ import annotations

import json
import os
import re
import sys

# Where the conversation view starts. One entry per keyed node renderer plus
# the view itself, because upstream registers the renderers through a
# dependency-injection container this panel replaces with a plain map
# (`src/renderers.tsx`) - so the container's edges have to be named here.
ENTRIES = [
    "packages/client/ui-conversation/src/client/chat/ChatView.tsx",
    "packages/client/ui-conversation/src/client/chat/AssistantNodeView.tsx",
    "packages/client/ui-conversation/src/client/chat/TurnTailNodeView.tsx",
    "packages/client/ui-conversation/src/client/chat/CommandNodeView.tsx",
    "packages/client/ui-conversation/src/client/locales.ts",
    "packages/client/ui-tool/src/client/tool/ToolCallTree.tsx",
    "packages/client/ui-tool/src/client/locale.ts",
    # Value-imported by a vendored spec rather than by a component, so the
    # closure does not reach it on its own. The brand module rather than the
    # package barrel, for the reason DEEP states: the barrel is a Service
    # registration that pulls in cordis and cosmokit, and the spec wants one
    # 19-line branding helper.
    "packages/attachment/attachment/src/brand.ts",
]

# Theme tokens. Every vendored component styles itself from `--dsw-*` and
# nothing else, so these are as load-bearing as the components.
STYLES = [
    "packages/client/ui-theme/src/styles/base.css",
    "packages/client/ui-theme/src/styles/design-platform.css",
    "packages/client/ui-theme/src/styles/scrollbar.css",
    "packages/client/ui-theme/src/styles/shiki.css",
]

# Vendored paths this project edits on purpose. Refreshing skips them so an
# edit is never silently reverted; the diff against upstream is then a
# deliberate, reviewable list rather than an accident.
MODIFIED = {
    "ui-primitives/index.ts",
    "ui-conversation/client/chat/ChatView.tsx",
}

# Brand art. The MIT licence grants copyright permission and says nothing
# about trade marks, so DeepSeek's whale mark and the `deepseek-official
# HARNESS` letterforms are not vendored into a differently-named product.
# Nothing in the conversation view references either.
REFUSED = {
    "packages/client/ui-primitives/src/BrandWordmark.tsx",
    "packages/client/ui-primitives/src/FishLogo.tsx",
}

# Upstream's own specs, for the packages whose components came across. A spec
# for a package that is not here has nothing to assert against.
SPEC_PACKAGES = ["ui-primitives", "ui-conversation", "ui-tool", "ui-attachment"]

# Upstream package name to the directory it is vendored into. The build's
# alias table in `vite.config.ts` says the same thing; this copy is what lets
# the tool decide whether a spec's imports will resolve before writing it.
VENDORED_DIRS = {
    "@deepseek-ai/dsh-attachment": "attachment",
    "@deepseek-ai/dsh-client-runtime": "runtime",
    "@deepseek-ai/dsh-client-ui-attachment": "ui-attachment",
    "@deepseek-ai/dsh-client-ui-conversation": "ui-conversation",
    "@deepseek-ai/dsh-client-ui-primitives": "ui-primitives",
    "@deepseek-ai/dsh-client-ui-slots": "ui-slots",
    "@deepseek-ai/dsh-client-ui-theme": "ui-theme",
    "@deepseek-ai/dsh-client-ui-tool": "ui-tool",
}

# A spec is refused when it needs upstream's own test harness. `cordis` is the
# dependency-injection container this panel replaces with a plain map, and
# `dsh-client-test-runtime` assembles a whole client context; bringing either
# would mean vendoring the framework the port exists to avoid.
SPEC_HARNESS = re.compile(r"""from '[^']*(?:cordis|test-runtime)""")

# Suites that must not come across: they assert the brand art REFUSED above
# draws correctly, so they test files this port deliberately does not contain.
REFUSED_SUITES = ["FishLogo", "BrandWordmark"]

# The floor a per-case timeout is raised to.
#
# A `{ timeout: N }` in a spec is a statement about the machine, not an
# assertion about behaviour, and upstream's machine is not this one: this box
# runs several compile lanes at once, and their 20s markdown-prefix comparison
# takes 36s here. Raising a threshold only ever weakens a case that asserts
# something happened FAST; these assert that output matches, so a longer
# allowance changes nothing they claim. Lowering one would be the dangerous
# direction.
SPEC_TIMEOUT_FLOOR = 180_000
TIMEOUT_OPTION = re.compile(r"\{\s*timeout:\s*([\d_]+)\s*\}")

# A barrel is not a dependency; the symbol is.
#
# `terminal-card-model.ts` imports ONE function - `resolveWorkspacePath`, 17
# lines with no imports of its own - from `@deepseek-ai/dsh-client-runtime/
# client`. Followed to the barrel, that single symbol drags 56 files and about
# 3,600 lines across six packages into the tree: upstream's session service,
# workspace manager, projection store, their dependency-injection container
# and its utility library. Measured, 0.7% of `runtime` and 5.8% of `cordis`
# ever execute here, because this panel replaces every one of those with
# `src/`.
#
# It is also what the structural gate reacts to. Vendoring that tail cost 1,043
# quality points and three of the four dependency cycles; the packages the
# conversation view actually draws with cost none - `ui-primitives` measured 90
# points BETTER than not having it.
#
# So a barrel re-export is followed to the module that owns the symbol. One
# entry, because there is one such import; a second would be a line here and a
# sentence saying which symbol and why.
# Keyed by (barrel specifier, symbol) and answering the specifier to rewrite
# the import to. The rewrite is recorded in the file's own header, because a
# changed import is a modification even when it changes no behaviour.
DEEP: dict[tuple[str, str], str] = {
    ("@deepseek-ai/dsh-client-runtime/client", "resolveWorkspacePath"):
        "@deepseek-ai/dsh-client-runtime/client/workspaces/path.ts",
    # The same shape one layer down: a vendored spec wants the `AttachmentId`
    # branding helper, and the package barrel that re-exports it registers a
    # cordis Service - so the barrel costs the container and its utility
    # library for 19 lines of type branding.
    ("@deepseek-ai/dsh-attachment", "AttachmentId"):
        "@deepseek-ai/dsh-attachment/brand.ts",
}

# Value imports only. `import type` and `export type` are erased before
# resolution, and following them is what turns a 175-file closure into a
# 579-file one that never runs.
#
# Done by deletion rather than by one clever pattern: a `from` clause may sit
# several lines below its `import`, so a single expression has to span
# newlines - and one that spans newlines while also excluding `type` reads the
# NEXT statement's specifier whenever a type import is followed by a value one.
# Cutting the type statements out first makes what is left unambiguous. The
# first shape this missed was a multi-line `import { ... } from './render.tsx'`,
# which resolved cleanly right up to the link step.
TYPE_STATEMENT = re.compile(
    r"""(?:^|\n)\s*(?:import|export)\s+type\s[\s\S]*?from\s+['"][^'"]+['"]"""
)
FROM_CLAUSE = re.compile(r"""\bfrom\s+['"]([^'"]+)['"]""")
SIDE_EFFECT = re.compile(r"""(?:^|\n)\s*import\s+['"]([^'"]+)['"]""")
DYNAMIC_IMPORT = re.compile(r"""\bimport\(\s*['"]([^'"]+)['"]\s*\)""")

VERBATIM = (
    "/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.\n"
    " * Vendored verbatim from deepseek-ai/deepseek-harness: {rel}\n"
    " * The full notice is web/deepseek/upstream/LICENSE. Unmodified\n"
    " * apart from this header. */\n"
)

NARROWED = (
    "/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.\n"
    " * Vendored from deepseek-ai/deepseek-harness: {rel}\n"
    " * The full notice is web/deepseek/upstream/LICENSE.\n"
    " *\n"
    " * MODIFIED by the tetanus project: a barrel import was narrowed to the\n"
    " * module that owns the symbol (see DEEP in tools/vendor.py). Same symbol,\n"
    " * same behaviour; what changes is that the barrel's other exports are no\n"
    " * longer dragged in behind it. */\n"
)


class _Header:
    """`HEADER.format(rel=..., narrowed=...)`, picking which text applies."""

    @staticmethod
    def format(rel: str, narrowed: bool = False) -> str:
        return (NARROWED if narrowed else VERBATIM).format(rel=rel)


HEADER = _Header


NAMED_IMPORT = re.compile(
    r"""(?:^|\n)\s*import\s*\{([^}]*)\}\s*from\s+['"]([^'"]+)['"]"""
)


def specifiers(source: str) -> list[str]:
    """Every module one source actually loads at run time."""
    without_types = TYPE_STATEMENT.sub("\n", source)
    return (
        FROM_CLAUSE.findall(without_types)
        + SIDE_EFFECT.findall(without_types)
        + DYNAMIC_IMPORT.findall(without_types)
    )


def deepen(source: str) -> tuple[str, bool]:
    """Rewrite every barrel import DEEP knows how to narrow.

    Answers the source and whether anything changed. A barrel import naming two
    symbols where only one is known keeps the barrel: narrowing it would leave
    the other silently unresolved.
    """
    changed = False
    for named, spec in NAMED_IMPORT.findall(TYPE_STATEMENT.sub("\n", source)):
        symbols = [
            piece.strip().split(" as ")[0].strip()
            for piece in named.split(",")
            if piece.strip() and not piece.strip().startswith("type ")
        ]
        if not symbols:
            continue
        deep = {DEEP.get((spec, symbol)) for symbol in symbols}
        if len(deep) != 1 or None in deep:
            continue
        narrowed = deep.pop()
        assert narrowed is not None
        for quote in ("'", '"'):
            if f"from {quote}{spec}{quote}" in source:
                source = source.replace(
                    f"from {quote}{spec}{quote}", f"from {quote}{narrowed}{quote}"
                )
                changed = True
    return source, changed


def packages(root: str) -> dict[str, str]:
    """Every workspace package in the checkout, by declared name."""
    found: dict[str, str] = {}
    for base, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in {"node_modules", ".git", "dist", "lib"}]
        if "package.json" not in files:
            continue
        try:
            with open(os.path.join(base, "package.json"), encoding="utf-8") as handle:
                declared = json.load(handle)
        except (OSError, ValueError):
            continue
        name = declared.get("name")
        if isinstance(name, str):
            found[name] = os.path.relpath(base, root)
    return found


def resolve(spec: str, frm: str, root: str, pkgs: dict[str, str]) -> str | None:
    """One import specifier to a repository-relative source path."""
    if spec.startswith("."):
        base = os.path.normpath(os.path.join(os.path.dirname(frm), spec))
        candidates = [
            base,
            base + ".ts",
            base + ".tsx",
            base + "/index.ts",
            base + "/index.tsx",
        ]
    elif spec.startswith("@deepseek-ai/"):
        parts = spec.split("/")
        name, sub = "/".join(parts[:2]), "/".join(parts[2:])
        if name not in pkgs:
            return None
        directory = pkgs[name]
        candidates = (
            [f"{directory}/src/index.ts", f"{directory}/src/index.tsx"]
            if sub == ""
            else [
                f"{directory}/src/{sub}",
                f"{directory}/src/{sub}.ts",
                f"{directory}/src/{sub}.tsx",
                f"{directory}/src/{sub}/index.ts",
                f"{directory}/src/{sub}/index.tsx",
            ]
        )
    else:
        return None
    for candidate in candidates:
        if os.path.isfile(os.path.join(root, candidate)):
            return candidate
    return None


def closure(root: str, pkgs: dict[str, str]) -> set[str]:
    """Everything the entry points reach at run time."""
    seen: set[str] = set()
    stack = list(ENTRIES)
    while stack:
        rel = stack.pop()
        if rel in seen or rel in REFUSED:
            continue
        if not os.path.isfile(os.path.join(root, rel)):
            raise SystemExit(f"{rel}: not in the checkout - upstream moved it")
        seen.add(rel)
        if rel.endswith(".css"):
            continue
        with open(os.path.join(root, rel), encoding="utf-8", errors="ignore") as handle:
            source = handle.read()
        source, _ = deepen(source)
        for spec in specifiers(source):
            found = resolve(spec, rel, root, pkgs)
            if found is not None:
                stack.append(found)
    return seen


def landed(rel: str) -> str:
    """Where one upstream path is vendored to."""
    parts = rel.split("/")
    if parts[0] == "packages" and parts[1] == "client" or parts[0] == "packages":
        package, rest = parts[2], "/".join(parts[3:])
    elif parts[0] == "vendor":
        package, rest = parts[1], "/".join(parts[2:])
    else:
        raise SystemExit(f"{rel}: not under packages/ or vendor/")
    if not rest.startswith("src/"):
        raise SystemExit(f"{rel}: not under src/")
    return f"{package}/{rest[len('src/') :]}"


def without_suite(source: str, name: str) -> str:
    """One top-level `describe('name', ...)` block removed, braces balanced."""
    for quote in ("'", '"'):
        marker = f"describe({quote}{name}{quote}"
        at = source.find(marker)
        if at == -1:
            continue
        open_at = source.index("{", at)
        depth, end = 1, len(source)
        for offset, ch in enumerate(source[open_at + 1 :], start=open_at + 1):
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    end = offset
                    break
        tail = source.find("\n", end)
        return source[:at] + source[tail + 1 :]
    return source


def spec_header(rel: str, edited: bool) -> str:
    """The provenance header one vendored spec carries."""
    note = (
        "\n * MODIFIED by the tetanus project: a suite for upstream brand art was\n"
        " * removed (the files it tests are deliberately not vendored - see\n"
        " * NOTICE.md), and/or a per-case timeout was raised to the floor in\n"
        " * tools/vendor.py, because this box is shared and upstream's is not.\n"
        " * No assertion changed."
        if edited
        else "\n * Unmodified apart from this header and the relative import paths,\n"
        " * which lost their `src/` segment when the sources were vendored one\n"
        " * directory shallower."
    )
    return (
        "/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.\n"
        f" * Vendored from deepseek-ai/deepseek-harness: {rel}\n"
        " * The full notice is web/deepseek/upstream/LICENSE."
        f"{note} */\n"
    )


def resolvable(source: str, spec_dir: str, out: str) -> bool:
    """Whether every relative import a spec makes exists in the vendored tree.

    A spec whose subject was not vendored has nothing to assert against, and
    deciding that here is what keeps it a computed fact rather than a list
    somebody has to remember to update.
    """
    for spec in specifiers(source):
        if spec.startswith("."):
            base = os.path.normpath(os.path.join(spec_dir, spec))
        elif spec.startswith("@deepseek-ai/"):
            # The vendored layout, which is the package name's last segment
            # with the `dsh-client-`/`dsh-` prefix already stripped by the
            # directory it landed in.
            parts = spec.split("/")
            landed_dir = VENDORED_DIRS.get("/".join(parts[:2]))
            if landed_dir is None:
                return False
            base = os.path.join(landed_dir, *parts[2:])
        else:
            continue
        # A FILE, not a directory: `runtime/client` still exists as a folder
        # after the barrel it named was narrowed away, so `exists` would call a
        # spec resolvable whose import has nothing to load.
        if not any(
            os.path.isfile(os.path.join(out, base + end))
            for end in ("", ".ts", ".tsx", "/index.ts", "/index.tsx")
        ):
            return False
    return True


def vendor_specs(root: str, out: str) -> tuple[int, list[str]]:
    """Upstream's specs for the packages whose components came across."""
    taken, refused = 0, []
    for package in SPEC_PACKAGES:
        directory = os.path.join(root, "packages/client", package, "tests")
        if not os.path.isdir(directory):
            continue
        for name in sorted(os.listdir(directory)):
            if ".spec." not in name:
                continue
            rel = f"packages/client/{package}/tests/{name}"
            with open(os.path.join(directory, name), encoding="utf-8") as handle:
                body = handle.read()
            if SPEC_HARNESS.search(body):
                refused.append(f"{rel}: needs upstream's cordis/test-runtime harness")
                continue
            body = body.replace("'../src/", "'../").replace('"../src/', '"../')
            edited = False

            def raised(match: re.Match[str]) -> str:
                stated = int(match.group(1).replace("_", ""))
                if stated >= SPEC_TIMEOUT_FLOOR:
                    return match.group(0)
                return f"{{ timeout: {SPEC_TIMEOUT_FLOOR} }}"

            lifted = TIMEOUT_OPTION.sub(raised, body)
            edited = edited or lifted != body
            body = lifted
            for suite in REFUSED_SUITES:
                shorter = without_suite(body, suite)
                edited = edited or shorter != body
                body = shorter
            body, _ = deepen(body)
            if not resolvable(body, f"{package}/tests", out):
                refused.append(f"{rel}: its subject is outside the ported closure")
                continue
            path = os.path.join(out, package, "tests", name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(spec_header(rel, edited) + body)
            taken += 1
    return taken, refused


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    root = os.path.abspath(sys.argv[1])
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out = os.path.join(here, "upstream")
    pkgs = packages(root)

    wanted = closure(root, pkgs) | set(STYLES)
    written = kept = 0
    for rel in sorted(wanted):
        target = landed(rel)
        path = os.path.join(out, target)
        if target in MODIFIED and os.path.exists(path):
            kept += 1
            continue
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(os.path.join(root, rel), encoding="utf-8") as handle:
            body = handle.read()
        body, narrowed = deepen(body)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(HEADER.format(rel=rel, narrowed=narrowed) + body)
        written += 1

    with open(os.path.join(root, "LICENSE"), encoding="utf-8") as handle:
        text = handle.read()
    with open(os.path.join(out, "LICENSE"), "w", encoding="utf-8") as handle:
        handle.write(text)

    specs, refused = vendor_specs(root, out)
    with open(
        os.path.join(out, "SPECS-NOT-PORTED.txt"), "w", encoding="utf-8"
    ) as handle:
        handle.write(
            "Upstream specs that could not come across, and why.\n"
            "Written by tools/vendor.py; the reasoning is in\n"
            "data/tetanus-ui-port/report.md.\n\n"
        )
        handle.write("\n".join(sorted(refused)) + "\n")

    print(
        f"{written} vendored, {kept} kept as modified, {specs} specs taken, "
        f"{len(refused)} specs refused, LICENSE refreshed"
    )


if __name__ == "__main__":
    main()
