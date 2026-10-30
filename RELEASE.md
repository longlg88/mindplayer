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
release** is cut unless an exact `Release-As: X.Y.Z` target is supplied.

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

To override a commit's bump, add a `Release-As: major`, `Release-As: minor`, or
`Release-As: patch` trailer to its message:

```
Release-As: minor
```

These named overrides keep their existing precedence over that commit's type
and breaking-change signals; the highest resulting bump across the range wins.

To align a release with an already tested version, use an exact target instead:

```
Release-As: 0.57.5
```

Exact targets must be canonical `X.Y.Z`: three non-negative decimal integers,
without leading zeroes, a `v` prefix, prerelease/build suffixes, or extra text.
Trailer names and named bumps remain case-insensitive; surrounding whitespace
is allowed. Any other `Release-As:` value fails version calculation.

The target must be strictly newer than the current release and at least the
version required by the normal bump calculation, including named overrides.
For example, from `0.57.2`, a patch range normally yields `0.57.3` and may target
`0.57.5`; a feature range requires at least `0.58.0`. All exact targets in the
range must agree: repeated identical targets are allowed, conflicting targets
fail. An exact target triggers a release even for a chore-only range, and the
reported bump reflects the target's actual major/minor/patch change. Invalid,
same, older, or insufficient targets fail before any release outputs are written.
Without an exact target, existing named-override and automatic bump behavior
is unchanged.

## The machine-readable rules (CI parses this block)

<!-- ci:bump-rules
major = breaking, major
minor = feat, feature, minor
patch = fix, perf, refactor, build, revert, deps, patch
none  = docs, chore, ci, test, style
-->

(Edit the block above to change how CI maps commit types to bumps. Breaking
signals imply MAJOR unless a named override applies; exact targets must satisfy
the resulting minimum version.)

## Changelog

### Unreleased

- Align the Vercel key row with the provider quota rows: same columns and
  colours, and dollar amounts shown to the cent.

- Reduce development build storage and clean up isolated smoke-test fixtures.

- Show only Vercel key usage, with an optional key-specific local display
  target that does not change or enforce the server-side budget.

- Allow handoff to OMP, choosing a model and provider before delivering context.

- Show the Vercel AI Gateway API key's effective budget and current-period
  usage in the session-list footer when its suffix uniquely matches a key.

- Add an OMP conversation-model and provider-route selector for new sessions;
  cancellation blocks ordinary input until a model is selected. Resumed
  sessions and auxiliary or subagent model settings are unchanged.

- Let `Ctrl-Q` close an OMP pane like other providers; `Alt-Q` remains available.

- Fix Jev rejecting the account's explicitly selected GPT-6.1 Terra baseline.
  An unranked model is offered only when the user selected it and the live
  catalog permits it; other unreviewed models still require review. A registry
  failure now explains how to choose native routing and retry manually.

- Start the actual OMP CLI from the new-session picker and discover/resume its
  native sessions, using OMP's own authentication and model configuration.
  Preserve OMP's other native input shortcuts; use Alt-W for pane navigation.
  Remove the former Codex Flow mode.

- Add the session base-camp view draft (`docs/plans/2026-10-23-office-view-gallery.ko.html`):
  read-only companions on a daily terrain with fenced stations, category capture
  and a species dex, with an offline interaction test.
- Keep Codex's addressed viewport and cursor stable while reflowing history on
  resize, resize embedded PTYs before the first zoom/layout draw, honor history
  purge and synchronized presentation, and clamp restored cursors after shrink.
- Clarify selected-turn token totals with model-call count and latest-call
  usage, make wrapped Trace execution logs mouse-scrollable, and avoid
  transient/repeated redraws while entering or observing a new session.
- Show Codex Business `individualLimit` as a credits/spend-cap row instead of
  presenting it as a monthly usage window, and mark accounts blocked only when
  the app-server refuses ordinary usage.

### v0.57.5 — 2026-10-26

- Preserve session labels, pane continuity, and resume targets across native
  `/clear` session changes.
- Answer pane-local cursor-position queries, including fragmented queries, and
  keep ready input responsive across resize events. Full native resize
  resolution remains unverified.
- Exclude a model from Jev candidates for 60 seconds after a correlated final
  `serverOverloaded` failure within the same connection, provider, and account.
  Reconsider remaining candidates on the next explicit input; Jev can abstain
  without submission. Failed turns are never retried automatically.

### v0.55.2 — 2026-10-13

- Rename account display labels without moving provider homes, preserving
  Codex's absolute rollout paths and stores used by other running processes.
- Refuse new account names whose directories are already in use, so an old
  label cannot silently reuse a renamed account's credentials.

### v0.55.1 — 2026-10-13

- Distinguish Codex users within one Business workspace using optional
  `account/read` identity; a workspace ID alone no longer hides another user's
  quota. Accept either RPC response order and retain usage if identity is absent.
- Hide zero-credit spend caps, use amber for a soft exhausted cap, and preserve
  duplicate-login explanations on credit rows.
- Accept Korean-IME and uppercase Accounts shortcuts, and relabel usage plus
  discard stale in-flight readings after account rename.
- Cover account entry, refresh, rename, credential preservation, and shared-login
  rendering with real-PTY tests that can target the locally installed executable.

### v0.55.0

- Correct Codex Business quota semantics and preserve per-login account identity
  in the quota footer.

### v0.43.0 — 2026-09-21

- Open a full-screen, read-only Trace for the focused session with Ctrl-G while
  its PTY continues running; Ctrl-G or Esc returns to the live session.
- Group observed prompts, public messages, tool calls, results, and provider
  usage into a selectable execution timeline with mouse-wheel navigation.
- Make provider-recorded usage for the selected turn the primary token metric,
  with the literal user text's local `~` estimate explicitly secondary.

### v0.42.0 — 2026-09-20

- Pin the inherited Codex account to the machine's default `~/.codex` so a
  per-account `CODEX_HOME` cannot redirect its login into another account.
- Refresh account usage immediately with Ctrl-R from the session list.
- Preserve the session-trace HTML design alternatives for review; the trace
  feature itself is not included in this release.

### v0.41.0 — 2026-09-19

- Read Codex account limits directly through its app-server so an old rollout
  cannot keep showing pre-reset usage.
- Show weekly and monthly limits for each Codex account, including reset dates
  and times, unavailable windows, and exhausted monthly allowances.
- Preserve account isolation and other providers' cached readings; do not reuse
  stale Codex gauges when a live reading fails.

### v0.15.1 — 2026-07-09

- Removed the unused orchestration feature (`o`/`b`/`m`/`p`/`s`: group
  launch, broadcast, main-mediated dispatch, peer-review, synthesis).
  Pieces it shared with other features — the text+cursor input buffer,
  the generic session-submit helper, and handoff peer-context
  resolution — were kept and moved, not deleted. No `state.json` or
  `audit.jsonl` format change; old audit lines for the removed events
  are skipped on read.
