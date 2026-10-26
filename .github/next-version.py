#!/usr/bin/env python3
"""Compute the next semver version from commit messages + RELEASE.md rules.

Usage:
    git log <last_tag>..HEAD --format=%B%x00 | \
        python3 .github/next-version.py RELEASE.md <current_version>

Reads the `ci:bump-rules` block in RELEASE.md (type -> bump), classifies each
commit, picks the highest bump, applies it to <current_version>, and prints:

    bump=<major|minor|patch|none>
    version=<X.Y.Z>          (only when bump != none)

If $GITHUB_OUTPUT is set, the same key=value lines are appended there.
Conventions: `type!:` / `BREAKING CHANGE` -> major; `Release-As: <bump>`
overrides a commit's bump. `Release-As: X.Y.Z` selects an exact forward version
at least as high as the normal bump requires, and releases even chore-only ranges.
"""
import os
import re
import sys

RANK = {"none": 0, "patch": 1, "minor": 2, "major": 3}


def parse_rules(release_md_path):
    """Return {type_prefix: bump} from the ci:bump-rules block."""
    text = open(release_md_path, encoding="utf-8").read()
    m = re.search(r"ci:bump-rules(.*?)-->", text, re.S)
    rules = {}
    if not m:
        return rules
    for line in m.group(1).splitlines():
        line = line.strip()
        if "=" not in line:
            continue
        bump, types = line.split("=", 1)
        bump = bump.strip().lower()
        if bump not in RANK:
            continue
        for t in types.split(","):
            t = t.strip().lower()
            if t:
                rules[t] = bump
    return rules


def classify(message, rules):
    """Bump implied by a single commit message."""
    # Explicit override wins.
    mo = re.search(r"^\s*Release-As:\s*(major|minor|patch)\s*$", message, re.I | re.M)
    if mo:
        return mo.group(1).lower()
    subject = message.strip().splitlines()[0] if message.strip() else ""
    # Breaking: `type!:` in subject, or BREAKING CHANGE in body.
    if re.match(r"^[a-zA-Z]+(\([^)]*\))?!:", subject) or re.search(
        r"BREAKING[ -]CHANGE", message
    ):
        return "major"
    # Conventional type prefix.
    tm = re.match(r"^([a-zA-Z]+)(\([^)]*\))?:", subject)
    if tm:
        return rules.get(tm.group(1).lower(), "none")
    return "none"


def main():
    release_md = sys.argv[1] if len(sys.argv) > 1 else "RELEASE.md"
    current = sys.argv[2] if len(sys.argv) > 2 else "0.0.0"
    rules = parse_rules(release_md)

    raw = sys.stdin.read()
    # Commits are NUL-separated (git log --format=%B%x00).
    commits = [c for c in raw.split("\0") if c.strip()]
    best = "none"
    targets = set()
    for c in commits:
        for value in re.findall(r"^[ \t]*Release-As:[ \t]*(.*)$", c, re.I | re.M):
            value = value.strip()
            if value.lower() in ("major", "minor", "patch"):
                continue
            if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
                raise SystemExit("Release-As: invalid value; expected major, minor, patch, or canonical X.Y.Z")
            targets.add(tuple(map(int, value.split("."))))
        b = classify(c, rules)
        if RANK[b] > RANK[best]:
            best = b
    if len(targets) > 1:
        raise SystemExit("Release-As: conflicting exact targets in commit range")

    version = None
    if best != "none" or targets:
        nums = re.findall(r"\d+", current)
        major, minor, patch = (int(nums[0]) if len(nums) > 0 else 0,
                               int(nums[1]) if len(nums) > 1 else 0,
                               int(nums[2]) if len(nums) > 2 else 0)
        baseline = (major, minor, patch)
        if best == "major":
            major, minor, patch = major + 1, 0, 0
        elif best == "minor":
            minor, patch = minor + 1, 0
        elif best == "patch":
            patch += 1
        if targets:
            target = next(iter(targets))
            if target <= baseline:
                raise SystemExit("Release-As target must be strictly newer than the current version")
            if target < (major, minor, patch):
                raise SystemExit(f"Release-As target is below required version {major}.{minor}.{patch}")
            if target[0] > baseline[0]:
                best = "major"
            elif target[1] > baseline[1]:
                best = "minor"
            else:
                best = "patch"
            major, minor, patch = target
        version = f"{major}.{minor}.{patch}"

    out = [f"bump={best}"]
    if version is not None:
        out.append(f"version={version}")

    text = "\n".join(out) + "\n"
    sys.stdout.write(text)
    gh = os.environ.get("GITHUB_OUTPUT")
    if gh:
        with open(gh, "a", encoding="utf-8") as f:
            f.write(text)


if __name__ == "__main__":
    main()
