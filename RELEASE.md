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

- **2.2.0:** keep the account of each OMP session's latest observed response.
  A row whose session is stopped or not open shows that account, also after a
  restart. A running pane still shows only what its own responses prove, and a
  session that has not answered in a MindPlayer pane stays `unverified`.
- Run a provider CLI's own `update` before a Codex, Claude or OMP session
  starts, at most once per CLI every 10 minutes. The session waits up to 90
  seconds and starts on the installed version when the update fails or times
  out. `MINDPLAYER_CLI_UPDATE=0` turns this off.
- Show each session row's own activity time. A handoff thread still
  sorts by its newest lane, but the original session and its handoff no longer
  share one displayed time.
- Explain a refused account name inside the name box, and let a label-only
  rename proceed while panes are open on that account.
- Read a second Claude account's usage from that account's own Keychain entry,
  so a signed-in account no longer shows `sign-in needed`.

- **2.1.0:** show Claude account labels even with one configured account and
  bind verified launch identities to their panes. Observe native OMP response
  credentials for OAuth account/workspace or API-key ID display; revoke stale
  attribution on account, model, or session changes without copying tokens or
  pinning the installed OMP executable. Unobserved identities remain unverified.
- Replace ambiguous `saved` status with `not open`, and use
  `no sessions open here` when no sessions are open in this instance.
- Add native OMP identity-transition coverage and offline observer regressions
  to CI and release checks.

- **2.0.1:** keep one temporary session row across catalog refreshes and replace
  it when the native transcript is discovered. Preserve selection through adoption
  so an immediate close targets the intended session rather than its neighbor.
- Cancel only the selected session's pending/queued launch and authentication
  when closing it. Remove new/handoff placeholders consistently, including ended
  sessions without transcripts. Never claim a late file for a child that did not run.
- Correlate pending labels and handoff metadata with their in-process owner.
  Adoption consumes and persists only that owner's records; recent neighboring
  sessions and same-parent handoff siblings retain their metadata.
- Add isolated real-TUI coverage for refresh before transcript creation,
  delayed transcript adoption, close during held authentication, and persistence
  across restart. Preserve native transcript files and Ctrl-Q's hide-live behavior.

- **2.0.0 — breaking:** use the installed PATH CLI for Codex and Claude
  new/resume/handoff/login launches instead of the private frozen Codex build.
  Remove integrated F6 Jev routing, embedded Node launcher assets and router UI.
  CLI replacement or symlink retargeting affects subsequent launches without
  replacing existing panes; changing PATH itself requires restarting MindPlayer.
  MindPlayer does not download or update provider CLIs.
- Preserve account environments, cwd, resume arguments, handoff context, Claude
  fullscreen settings and OMP launch behavior. Leave existing credentials and
  private Codex installations untouched.
- Add native PTY regressions for executable replacement and symlink retargeting
  across resume, new session and handoff, plus an opt-in installed-CLI version smoke.
- Remove private-router-only list identity migration after in-CLI `/clear`,
  `/resume` and `/fork`. The native commands remain available, but the list keeps
  the launch-time session binding; rediscover/select the new session for list actions.

- In 1.0.9, move provider identity to a fixed left column beside the handoff tree
  and task title, retaining the title's cell budget and right-side account/status
  metadata. Start each account's quota on its own row; only that account's windows
  may share a line when they fit.

- In 1.0.8, apply the approved title-first session list: clearer handoff rails and shared
  family shading, category spacing, right-side metadata, and title-first
  degradation on narrow terminals. Preserve mixed-cwd hints, owned status,
  compact status glyphs and logical selection across decorative gaps.
- Remove the Collected startup screen and its 800 ms pause. Collection uses the
  list surface; empty success opens the list and scan failures offer Enter retry.
- Combine Vercel key/user budgets into one truthful summary with attributed-user
  spend/limit and independent blocking/unknown states. Omit ordinary default-key
  boilerplate; wrap financial/reset details without clipping. Retire local
  display-target parsing without modifying existing user files or server budgets.
- Collapse repeated saved, unmeasured and sync-off labels. Cache list geometry
  and cwd disambiguation during visibility rebuilds; retain manual token
  measurement and pack provider quota windows only when they fit.
- Isolate the completed-Vercel probe cleanup smoke from unrelated account
  workers. Preserve the Archive search timing guard and report its actual
  observed screen and elapsed time instead of a later diagnostic frame.

- In 1.0.7, show the selected Gateway key's attributed-user budget separately
  from its API-key budget. Read attribution from raw key metadata, not the CLI
  login; distinguish blocked spend, absent meters, unknown attribution, and
  independent read failures. Keep narrow-footer budget states meaningful.
  Recognized Jev budget rejections now report their scope and numeric spend/limit
  without forwarding upstream payloads or dispatching a native turn. Server
  budgets and credentials are unchanged.

- In 1.0.6, include default-covered Vercel API-key meters and resolve an active
  default when the key has no custom budget. Distinguish measured zero spend
  from unavailable usage; a policy-only reading shows its inherited limit
  without inventing dollars or percentages. Keep failed/incomplete requests
  separate from genuine absence. Requests remain read-only and use the existing
  Vercel CLI login.

- In 1.0.5, page Vercel key and budget queries in smaller batches to stay within
  the bounded subprocess output limit. Read structured budget listings so a key
  without a budget displays `no active budget`, not `unavailable`; incomplete
  queries and authentication failures remain errors. No server budgets are changed.

- In 1.0.1, make session-list state explicit with `saved` for sources not owned
  by an open pane in this instance; retain source-qualified live status.
- Extract genuine Claude/Codex task titles, excluding native command notices,
  meta records and tool feedback. Shorten recognized Slack operational preambles
  without discarding the task. Rebuild older metadata catalogs once.
- Draw handoff branches and lane counts from visible adjacent parent/child rows,
  preserving atomic thread categories; missing, filtered, clipped or ambiguous
  parents no longer imply a connection to the preceding session.

- In 1.0.0, remove HTML discovery, notifications, manual browser previews and
  the HTML transition-report generator. Ctrl-P/Ctrl-T return to native session
  input; existing user files and custom prompts remain untouched.
- Replace automatic transcript token recounts with list `r`: measure one selected
  source on demand, without launching it. Session discovery and activity remain
  automatic but use metadata-only reads, including Active catalog refresh and
  restore. Unmeasured usage is unknown, and manual snapshots are process-local.
- This removal of existing workflows is a MAJOR change under the policy above;
  account quota refresh, PTY controls, categories and session handoff are retained.

- Keep Claude conversation history at the current width after split-pane zoom
  and mouse-wheel scrolling. New, resumed, and handoff Claude sessions select
  its native fullscreen renderer with a launch-local setting; global settings,
  native authentication, managed policy, and other providers remain unchanged.
  Already-running panes retain their current renderer.

- Hide Claude sessions recorded only as `claude -p` runs, such as one-shot
  posts that another agent starts, the same way as other helper sessions. A
  run that was later resumed interactively stays in the list.

- In 0.61.4, make OMP pane navigation consistent: Tab moves forward and
  Shift-Tab backward when two or more panes are open, including zoomed views.
  Single-pane forwarding is unchanged. Document native effort remapping to
  F7 in OMP's `keybindings.yml`; MindPlayer does not rewrite that file.
  The default-profile remap also affects standalone OMP; separate configuration
  directories need their own binding.
  Use a function key rather than an Option/letter chord to avoid macOS text
  composition and Korean-input differences without changing Ghostty bindings.

- Align the Vercel key row with the provider quota rows: same columns and
  colours, and dollar amounts shown to the cent.

- Reduce development build storage and clean up isolated smoke-test fixtures.

- Keep Claude metadata-only scans compatible with current Clippy while
  preserving full-scan token totals and warnings-as-errors CI checks.

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

### v0.61.2 — 2026-11-07 (local build)

- Preserve Command and shifted character shortcuts in OMP panes, fixing
  `Cmd-V` paste and `Alt-Shift-C` / `Alt-Shift-L` copy without changing other
  providers' key encoding or multiline bracketed paste.

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
