#!/usr/bin/env python3
"""Validate that commit subjects follow Conventional Commits.

Usage:
    git log <base>..<head> --format=%s%x00 | \
        python3 .github/check-commits.py RELEASE.md

Allowed types are read from the same `ci:bump-rules` block in RELEASE.md that
`next-version.py` uses, so the linter and the version bumper can never drift.
A subject is valid when it matches:

    <type>(<optional scope>)<optional !>: <summary>

Merge commits (`Merge ...`) and revert commits are tolerated. Exits non-zero
(failing CI) and prints every offending subject when any commit is invalid, so
a non-conforming commit blocks the merge.
"""
import os
import re
import sys

# `!` for breaking and the trailer override use these even if not in the rules.
ALWAYS_ALLOWED = {"revert", "release"}


def allowed_types(release_md_path):
    """Union of all commit types listed in the ci:bump-rules block."""
    text = open(release_md_path, encoding="utf-8").read()
    m = re.search(r"ci:bump-rules(.*?)-->", text, re.S)
    types = set(ALWAYS_ALLOWED)
    if not m:
        return types
    for line in m.group(1).splitlines():
        if "=" not in line:
            continue
        _, rhs = line.split("=", 1)
        for t in rhs.split(","):
            t = t.strip().lower()
            if t:
                types.add(t)
    return types


def main():
    release_md = sys.argv[1] if len(sys.argv) > 1 else "RELEASE.md"
    types = allowed_types(release_md)
    pattern = re.compile(
        r"^(" + "|".join(sorted(map(re.escape, types))) + r")(\([^)]*\))?!?: .+"
    )

    raw = sys.stdin.read()
    subjects = [s.strip() for s in raw.split("\0") if s.strip()]

    bad = []
    for s in subjects:
        # Tolerate merge commits, which GitHub creates automatically.
        if s.startswith("Merge "):
            continue
        if not pattern.match(s):
            bad.append(s)

    if bad:
        sys.stderr.write(
            "✗ These commit subjects don't follow Conventional Commits "
            f"(allowed types: {', '.join(sorted(types))}):\n"
        )
        for s in bad:
            sys.stderr.write(f"    {s}\n")
        sys.stderr.write(
            "\nUse `type(scope): summary`, e.g. `feat: add X` or `fix: stop Y`. "
            "See RELEASE.md.\n"
        )
        sys.exit(1)

    print(f"✓ {len(subjects)} commit subject(s) OK")


if __name__ == "__main__":
    main()
