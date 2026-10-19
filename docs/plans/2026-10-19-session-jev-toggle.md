# Session Jev toggle / 0.55.16

Approved scope: terminal MindPlayer only. Show the pane-bound Codex account,
keep F6 Jev ON/OFF across prompts in the same running session, remove F7 context
preview, bump source version and install the exact tested binary. Claude stays
native without Jev. Preserve desktop, accounts/auth, native controls and queues.

## Decisions before edits

1. Startup-global mode is ambiguous across panes; fresh choice for every input
   repeats the UX problem; session-local sticky F6 is selected. New sessions start
   OFF (no external transfer), F6 enables subsequent prompts, F6 again disables.
   No disk persistence or cross-session inheritance is introduced.
2. Global quota labels do not identify the focused session; putting account text
   inside the native composer needs an additional trust/IPC surface; use the
   existing pane-bound account mapping in its split/zoom bottom border, without
   consuming another native terminal row or displacing the title/status/counts.
   Labels are arbitrary display labels, not a claim about authenticated identity.
3. Hiding F7 but retaining its RPC/token lifecycle leaves unused complexity;
   disabling only the key leaves a stale public surface; remove the unused
   native preview UI/RPC and Node token machinery. Keep the actual bounded
   judgment context and policy unchanged, with no history or secret-redaction
   claims. Rejection of retired local RPC must not forward it to the backend.

## Cleanup plan and ownership

- Lock new sticky ON/OFF, no-F7, pane-bound-label behavior with regressions first.
- Main: pinned native composer/protocol/patch, source version/docs, integration,
  native build, verification and paired local install/rollback.
- UI lane: existing Rust pane titles, focused key/help, pure rendering regressions.
- Node lane: preview-only deletion in service/router and strict v4 private UI.
- Test lane: owned synthetic fixtures, actual native PTY and Rust smoke migration.
- Keep diffs bounded, use existing paths, no dependencies/global CLI changes.
  Do not delete sessions, account homes, credentials or running user processes.

## Invariants and verification

The sticky mode is session state, but the selected mode is copied onto each
actual native UserMessage when submitted. Draft edits do not turn it off;
queued/accepted messages cannot be rewritten by a later toggle. New chat/session
ownership starts OFF. Shell/admin/approval/embedded input keep native semantics.
Native OFF bypasses Jev and preserves native model/effort, not a guessed reset.
No preview ID or removed local RPC reaches the backend.

Evidence round 1: compare screenshot, live source, installed version and exact
pane/native/Node consumers. Round 2: independently compare regressions, actual
PTY traffic/lifecycle and installed executable behavior. No external API change
or provider inference is needed; do not substitute build/hash for behavior.

Run scoped native Rust regressions, targeted app units, offline Node, fmt/Clippy,
native PTY and source/installed smoke. Inspect impure writes/cleanup before tests;
no short external timeout. Keep source hashes across frozen tests, exact PID/port
cleanup, source revision/dirty state, version/hash/installed path and rollback.
Record visual text-cell evidence separately from OS styling/IME proof. A matching
Code Mode host must be tested through actual Core execution before activation.

## Completed local verification

0.55.16 is installed at `/Users/eden.jang/.local/bin/mindplayer`. The final
source and exact installed executable each passed the same 41 scoped Rust PTY
smoke tests. The pinned private native candidate passed 47 contracts/cases
(44 actual PTY cases, 45 isolated fixtures) and 57 native Rust regressions.
Offline Node passed 580 with 49 explicit native/live opt-in skips. Formatting,
owned strict Clippy and exact task-owned process/port cleanup passed.

The F7 deletion and session toggle simplify the previous selection/preview
lifecycle. Native input snapshots, queue ownership and controls remain intact.
The test-only terminal projection was repaired from genuine retained ANSI and
an independent Rust vt100 comparison; no product assertion was weakened.

The paired old executable/native manifest remain in
`/Users/eden.jang/.local/bin/mindplayer-0.55.15-rollback.P55Fp3`.
Existing running MindPlayer and child panes are not hot-patched: restart the app
and reopen old panes. Desktop files are byte-identical. No live provider
inference, login, global settings, commit or push was performed. Text-cell
captures do not prove original terminal styling or IME behavior.

Exact hashes, commands, retained RED/GREEN evidence, changed-file scope and
remaining verification gaps: [installation validation](../0.55.16-local-install.validation.json).
