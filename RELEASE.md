# Release & versioning policy

MindPlayer uses [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`.

Every push to `main` runs CI. **A release is cut only if CI is fully green.** The
release version bump is decided automatically from the commit messages since the
last release, using the rules below — this file is the source of truth the CI
reads (`.github/next-version.py` parses the `ci:bump-rules` block).

## What each bump means

| Bump | When | Examples |
|------|------|----------|
| **MAJOR** | A breaking change for users | remove/rename a CLI flag or the binary; change the on-disk `~/.mindplayer/state.json` format incompatibly; drop support for an agent; change default behavior in a way that breaks existing workflows |
| **MINOR** | A backward-compatible new capability | add a new agent, a new key/command, a new status, a new view or option |
| **PATCH** | No new capability | bug fix, performance, refactor, dependency bump, docs/CI/test-only changes that ship in the binary |

If a range of commits since the last release contains a mix, the **highest** bump
wins. If it contains only `docs`/`chore`/`ci`/`test`/`style` commits, **no
release** is cut.

## How to signal the bump

Use [Conventional Commits](https://www.conventionalcommits.org) prefixes on the
commit subject — `type(scope): summary`. A `!` after the type, or a
`BREAKING CHANGE:` line in the body, forces a MAJOR bump:

```
feat: add Kiro agent support           → MINOR
fix: stop working/idle rows flapping    → PATCH
feat!: rename binary to agentplex       → MAJOR
perf: memoize blocked detection         → PATCH
docs: rewrite install section           → (no release on its own)
```

To override the computed bump for a specific release, add a trailer to the
commit message:

```
Release-As: minor
```

## The machine-readable rules (CI parses this block)

<!-- ci:bump-rules
major = breaking, major
minor = feat, feature, minor
patch = fix, perf, refactor, build, revert, deps, patch
none  = docs, chore, ci, test, style
-->

(Edit the block above to change how the CI maps commit types to version bumps;
`!`/`BREAKING CHANGE` always force MAJOR, and `Release-As:` always wins.)
