# Codex resize/zoom fix — local validation, 2026-10-12

Local workstation paths are anonymized with synthetic example-user identifiers; recorded evidence was not rerun.

## Source and installed binary

- Base revision: `109c3e480d02edfadd02bb9e62460ac64558d3ff` (`v0.54.0`).
- Source state: uncommitted resize fixes; dirty working tree. No commit or push.
- Source/package and installed version: `0.54.1` (local patch build, not a published release).
- Installed executable: `/Users/example-user/.local/bin/mindplayer`.
- Built and installed SHA-256: `8da19fc7d6c2989e47c34de4566be2e469b566125b37ad78f100418709b80762`.
- Rollback executable: `/Users/example-user/.mindplayer/mindplayer-resize-rollback.1zdWv7/mindplayer-0.54.0`.
- Rollback version: `0.54.0`; SHA-256: `2e8c814b24a52de46d96442c6094cd251a71e23c3453ebe0c609a10e7538d650`.

Existing user sessions were not terminated or restarted. A running old process
continues using its old code; the patch applies to newly started MindPlayer processes.

## Reproduced failures and boundary repairs

- Width reflow moved absolute-addressed live rows into history or shifted a footer upward through unused bottom rows. Reflow history separately; retain the meaningful addressed suffix and its cursor. On severe height shrink, explicitly allocate space for that suffix and retain its tail rather than truncating it accidentally.
- The first layout/zoom draw sampled the parser at its previous dimensions. Resize before rendering; serialize the PTY resize syscall and parser resize against child-output parsing. Cache geometry only after a successful syscall.
- Saved cursors could restore outside a shrunken grid. Clamp the restored row and column.
- Codex history replay retained old history because `CSI 3 J` was ignored. Erase saved history only, without clearing the live viewport or moving its cursor.
- Fragmented synchronized paint exposed partial cells and cursor state. Continue live parsing while holding a visible-only presentation snapshot until `CSI ? 2026 l`. Repeated begin markers extend the recovery deadline. A one-second embedding timeout, resize, user scrolling, or EOF releases a stale snapshot. Idle timeout recovery schedules its own redraw, independently of the working-status badge.
- A one-column pane receiving a wide Korean glyph panicked; clipped wide cells also lost their second half. Skip unrenderable glyphs, preserve history through the one-column transition, and reuse existing row truncation to clear clipped wide-cell halves.

No dependencies were added. Existing reflow, row truncation, PTY dirty tracking,
and test fixtures are reused; no separate rendering engine was introduced.

## Alternatives considered

1. Disable all Codex history reflow: rejected because historical text would retain obsolete widths.
2. Replace the terminal emulator: rejected as a broad dependency/migration change for a bounded bug.
3. Repair viewport/history ownership, resize ordering, and the missing control sequences in the existing parser: selected and regression-tested.

Protocol evidence was cross-checked in two rounds: pinned Codex/Crossterm source,
then independent terminal protocol specifications. The installed Codex was `0.162.1`.
Pinned [Codex clear/replay source](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/codex-rs/tui/src/custom_terminal.rs#L553-L569),
[XTerm erase semantics](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html), and
[synchronized-output specification](https://github.com/contour-terminal/vt-extensions/blob/f8a5ee15e272a825261d72c19715790d529901b1/synchronized-output.md)
support the repairs. The one-second recovery duration is our embedding policy,
not a Codex guarantee or protocol-specified deadline.

## Fresh validation

- `cargo test --workspace --quiet`: **709 passed, 5 ignored, 0 failed**.
- `cargo test --manifest-path crates/vt100-mp/Cargo.toml --offline --quiet`: **8 unit tests + 1 documentation test passed**.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Workspace formatting, changed vendored-file formatting, and `git diff --check`: passed.
- `cargo build --release -p mindplayer-tui`: passed; built from the patched source tree with its regression tests present.
- `MINDPLAYER_TEST_BINARY=/Users/example-user/.local/bin/mindplayer cargo test -p mindplayer-tui --test smoke --quiet`: **13 passed, 1 fixture-only test ignored, 0 failed**. This exercises the exact installed release executable, not only a debug build or matching checksum.
- Two real pane PTYs render an inline ratatui fixture. Width/height changes and zoom/restore repeat three times, checking exact restored geometry and stable physical semantic-row/prompt positions across changing paint ticks.
- Regression tests cover first-draw geometry, history replay, saved cursor restore, tiny Korean-text panes, oversized pinned suffixes, cropped cursors, and height-only shrink with unused blank rows.
- Independent review found a severe-height-shrink suffix truncation; the red reproducer was fixed. Follow-up review approved the allocation/cursor repair with no blocking findings.

One smoke run initially failed because startup readiness accepted stale geometry
before a later footer-height update. The test now waits for complete advancing
paint epochs at stable exact geometry instead of relying on matching text counts.

## Changed files and remaining limits

- Runtime: `crates/mindplayer-tui/src/{pty,ui}.rs`.
- Terminal state: `crates/vt100-mp/src/{grid,parser,row,screen}.rs`.
- End-to-end fixture: `crates/mindplayer-tui/tests/smoke.rs`.
- Version/notes: `Cargo.toml`, `Cargo.lock`, `RELEASE.md`, this record.
- Regression gates: `.github/workflows/{ci,release}.yml`, `.gitignore` (generated vendored-library lockfile only).

No real user Codex conversation was submitted/resumed for validation. The PTY
fixture reproduces inline rendering and control sequences but is not the entire
Codex application. Native GUI terminal/font combinations and Codex batches
exceeding the recovery timeout were not measured. Timed-out incomplete frames
may intentionally become visible to avoid an indefinitely frozen pane.
