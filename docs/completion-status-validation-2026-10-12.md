# Codex completion borders — local validation, 2026-10-12

## Result and source

Approved direction A is implemented in local MindPlayer **0.54.2**. Pane
borders indicate lifecycle status, while existing corners and `▸` indicate
focus/zoom. Category colors remain in the title. Status glyphs precede
truncatable category/name text; no header chip, extra pane row, or dependency
was added. The existing seven-pane 4+3 layout and full-width quota footer are
unchanged: their geometry calculation was not modified.

- Base revision: `109c3e480d02edfadd02bb9e62460ac64558d3ff` (`v0.54.0`).
- Source state: dirty working tree, including the preceding 0.54.1 resize fixes
  and their regression tests. Existing changes were preserved. No commit/push.
- Source/package and installed version: `0.54.2`, a local patch build.
- Installed executable: `/Users/eden.jang/.local/bin/mindplayer`.
- Built and installed SHA-256: `cd5919b405053d75b3d590573e7a7639dab85709e04cf8a23de6a7f75a0f7bb4`.
- Rollback executable: `/Users/eden.jang/.mindplayer/mindplayer-status-rollback.NTNztX/mindplayer-0.54.1`.
- Rollback version: `0.54.1`; SHA-256: `8da19fc7d6c2989e47c34de4566be2e469b566125b37ad78f100418709b80762`.

The previous executable was copied and verified before an atomic replacement.
Existing user sessions were not terminated or restarted. Running old processes
retain their old code; newly launched MindPlayer processes use the patch.

## Cause and behavior

The previous frame policy let category, focused, and zoomed colors override
status colors. A completed response also commonly appeared idle: a generic
`Stop` is insufficient current-turn completion evidence. Regression tests
reproduced the priority and narrow-header failures before the fixes; the old
installed 0.54.1 also failed the new status-color PTY reproducer.

| State | Glyph | Border RGB |
| --- | --- | --- |
| Idle / completion unknown | `○` | 140, 148, 162 |
| Working | `●` | 240, 212, 120 |
| Completed response | `✓` | 145, 217, 174 |
| Approval / action required | `!` | 255, 183, 131 |
| Process ended / success unknown | `○` | 174, 183, 198 |

Completion means the response ended, not that the task succeeded or passed
verification. Codex CLI 0.162.1's locally observed rollout records supply
`event_msg` payloads with `task_started` and `task_complete` plus `turn_id`.
The new bounded incremental reader correlates these records; hooks are not
required for Codex completion. It does not infer completion from assistant
text, token counts, a prompt, silence, or an unmatched completion record.

Readers start at EOF for resumed sessions so historical completions cannot
become current completion. Newly created sessions adopt their actual rollout
with input-time cutoffs and retain the original child hook ID. Partial JSON
and UTF-8 are retained until newline; oversized records, file truncation,
replacement, and disappearance are handled conservatively.

New submitted prompts and explicit interruption invalidate old completion
correlation. Enter while approving/steering a live turn preserves that turn;
automatic initial/handoff/peer prompts start a new boundary. Bracketed paste
drafts do not clear completion before submission. Failed PTY enqueue restores
the prior tracking state. Process exit remains gray; interruption cannot
resurrect green via a delayed old completion.

## Fresh verification

- `cargo test --workspace --quiet -- --test-threads=1`: **731 passed,
  5 intentionally ignored, 0 failed** on the final source.
- `cargo test --manifest-path crates/vt100-mp/Cargo.toml --quiet`: **8 unit
  tests + 1 documentation test passed**.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all --check` and `git diff --check`: passed.
- `cargo build --release`: passed, without a version environment override.
- `MINDPLAYER_TEST_BINARY=/Users/eden.jang/.local/bin/mindplayer cargo test
  -p mindplayer-tui --test smoke --quiet -- --test-threads=1`: **18 passed,
  1 fixture-only test ignored, 0 failed** in 47.61 seconds. These tests execute
  the installed release, not only a debug build or matching checksum.
- TestBackend checks five status colors at widths 12/24/48 under normal,
  focused, and zoomed rendering with a colored category; glyph-first clipping
  is checked at width 12.
- Real PTYs check matched/mismatched/historical completion, narrow resize,
  focus/zoom, input/interruption boundaries, approval blocking, and process
  exit. Earlier inline paint/resize/zoom-restore smoke coverage remains passing.
- Independent review approved the final state/input/hook changes with no
  remaining concrete findings after fixes and regression tests for hook-ID
  migration, failed input delivery, and automatic new-prompt boundaries.

The test fixture removes inherited `NO_COLOR`/`CLICOLOR` settings to inspect
production RGB escape sequences. Production continues to honor color settings.
Fixtures isolate HOME, accounts, hooks, transcripts, and fake agent processes;
no real user Codex request or hook/config installation was performed.

## Changed files, simplifications, and limits

- Rendering: `crates/mindplayer-tui/src/ui.rs`.
- Provenance: new `crates/mindplayer-tui/src/turn_status.rs`, plus
  `agent_hooks.rs`, `app.rs`, and `main.rs`.
- Lifecycle/input integration: `app/{pane,spawn,handoff_sync,session_list}.rs`.
- Regressions: `app/tests.rs`, `ui.rs` tests, and `tests/smoke.rs`.
- Version/design/record: `Cargo.toml`, `Cargo.lock`, `DESIGN.md`, this document.

One shared frame-color policy replaces category/focus/zoom color overrides.
Existing focus shapes, title metadata, input paths, and test fixtures are
reused; transactional dispatch centralizes tracking rollback. No extra UI
surface, pane reordering, dependency, or terminal renderer was introduced.
The standalone HTML proposal and preceding resize validation record are
unchanged by this completion patch.

Native GUI/font screenshot comparison and a live Codex model turn were not
performed. Exact RGB/cell and PTY checks are functional evidence, not a claim
of screenshot matching at the user's seven-pane terminal size. Historical
completion before opening a resumed pane intentionally stays unknown until a
fresh correlated turn. A separately labelled interrupted state and automatic
ASCII fallback for unusual wide-glyph fonts are not part of this patch.
