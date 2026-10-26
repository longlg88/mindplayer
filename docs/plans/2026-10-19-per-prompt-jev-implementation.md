# Per-prompt Jev implementation / 2026-10-19

Local workstation paths are anonymized with synthetic example-user identifiers; recorded evidence was not rerun.

Status: completed and locally installed as 0.55.15. Claude Jev runtime/bundle was
removed; native Claude sessions/auth/accounts remain and show no Jev selector.
Codex has a fresh F6 Jev/native choice for every prompt and optional F7 exact
bounded-state disclosure. Actual native PTY 41/41 and exact installed MindPlayer
smoke 37/37 pass; offline Node 564 pass / 46 opt-in skips / 0 failures. No paid
provider inference was exercised. Detailed source hashes, rollback, cleanup and
verification limits: [local install record](../0.55.15-local-install.validation.json).
Scope: terminal MindPlayer only. No desktop, account/login changes, dependencies,
commit or push. Preserve the inherited dirty worktree.

## Decision

Latest user scope override: "jev를 claude는 다 빼버려 codex만 적용해 ... claude
session이면 jev선택 안나오게". This resolves the former Claude identity blocker
by removing that provider integration, not by expanding its queue ownership.

Compare the revised integration choices: (1) keep dormant Claude routing but
hide its UI, rejected because the user asked to remove it; (2) remove Claude
runtime/bundle/imports and retain native sessions/auth/controls, selected;
(3) keep the cross-provider feature by replacing Claude's input queue, rejected
because the user explicitly narrowed to Codex. Remove only Jev-specific Claude
sources/tests; do not delete Claude sessions, accounts, credentials or auth code.

Execution lanes: native Codex composer/protocol/patch; Rust provider boundary and
startup removal; smoke/fixture regressions; main Node snapshot/contract/docs and
final integrated verification/install. Regression tests precede behavior edits.
No desktop changes, dependencies, commits, global CLI replacement or auth edits.

Preview alternatives: recomputing after queue release would silently change what
was reviewed; removing prior judgment would change the existing decision inputs;
retain an exact snapshot behind an opaque per-input token instead (selected).
The token binds thread object, full native input hash, actual baseline,
account/catalog/policy/rubric/family and offered menu. It is consumed once,
bounded to 32 entries per pane with 30-minute validity. Draft edits invalidate
the UI token; stale/expired/evicted tokens fail visibly before judgment/native
submission. A preceding turn's completion does not replace the reviewed prior.
No user input is added to metadata-only audit/status files.
The preview displays the outbound bounded `state`, not the separate offered
configuration menu/questions or the entire HTTP envelope.

Claude removal: four production modules and six Jev-only test modules were
removed via apply_patch. Exact source backups are retained outside the repo at
`/private/tmp/mindplayer-claude-jev-removed.MhrwV6/`. Claude pricing rows were
removed from the shared Jev rubric. Native Claude/account fixtures remain.
No session/account/auth/credential/global-plugin data was deleted.

Use proposal 1's input-row choice and a closed-by-default proposal 4 context
disclosure. Each prompt starts unselected, selection alone never sends, and
Enter submits only that prompt. Nonuse retains the actual native settings and
does not call Jev. The preview must be the exact bounded judgment snapshot, not
an invented transcript/history expansion or a claim of secret redaction.

## Boundary review

1. Outer Rust keystroke interception: small implementation, but cannot reliably
   distinguish native approvals, multiline editing, shell commands and queued
   submit identity. Rejected as the sole consent authority.
2. Turn-boundary modal gate: exact RPC/turn identity is available, but would move
   the approved composer-time selection to after submission. Not a substitute
   for the selected design.
3. Native composer/submit metadata plus existing routed consumers: preserves
   editing/queue semantics and binds each choice to the actual submission.
   Preferred; verify each provider's installed extension/schema before editing.

The current Codex private patch already owns queued user-message IDs. Claude's
installed 2.1.303 Mod API does not establish prompt.submit -> future turn
identity; do not invent it. Its optional prompt.submit.turnId identifies the
already-running turn. turn.start has text, turnId and optional agentId, but no
submission/message ID; the accepted submit result has text/context/origin, not
the future turn ID. session.turns is a count and session.messages has no message
UUID. Model-visible context is not a hidden consent-metadata channel.

Evidence: generated installed declarations at
`/var/folders/t8/nkh_jkgj0db5bs30rywyn4gr0000gq/T/mindplayer-claude-hxQYTT/.claude-plugin/types/claude-code/index.d.ts`
(PromptSubmitInput 9438, PromptSubmitResult 9508, TurnStartInput 14038), compared
with the official [Mods reference](https://code.claude.com/docs/en/plugins/mods/reference)
and [turn event guide](https://code.claude.com/docs/en/plugins/mods/events#follow-a-turn).
No live inference or native queued identity propagation was exercised. Calling
next with a modified turn.start text is structurally accepted by the types, but
actual execution-text rewriting was not verified; it cannot establish this
contract. This is an installed documented-surface limitation, not proof that
all possible future Claude integrations are impossible.

## Historical hold before Codex-only approval / 2026-10-19

The following records the earlier stopped attempt, not the current source state.
Implementation has resumed under the explicit Codex-only override above.

- Node service/router/run draft adds opt-in, per-request Jev/native validation,
  native bypass without key/policy/judge access, local metadata stripping, and
  an exact bounded snapshot preview that makes no judge or backend turn call.
  `requirePromptChoice` defaults false. The app does not enable the new flag;
  native UI/composer integration is not implemented. These are not release-ready
  feature claims.
- Five targeted Node regressions pass. The initial three captured RED 0/3 before
  repair. Final `node --test tools/local-model-router/service.test.mjs
  tools/local-model-router/router.test.mjs` passed 103/103, exit 0, 415 ms.
  These synthetic service/router tests do not prove native composer behavior.
- The retained Codex checkout at
  `/private/tmp/mindplayer-native-codex.MFgZts/source` contains only three added
  regression cases in `chat_composer_enter_tests.rs` and protocol `v2/tests.rs`.
  The protocol case is genuinely RED (0/1: routing metadata is dropped). The TUI
  test compilation was stopped at the hold, exit 130; no TUI result is claimed.
  The agent verified the source diff hash did not change on interruption.
  `tools/native-codex/queue.patch` remains the original v2 draft (SHA256
  `251ed63c1cac225bb8e2dbdf24232926594e6f2d3d484bae82771720b9684710`)
  and does NOT include those new test cases. No behavior/protocol/v3 integration
  or native release build was completed.
- The Rust app agent made no edits. Startup mode and the installed 0.55.14
  binary remain unchanged; no partially implemented default was activated.
- Final syntax checks on run/service/router and scoped `git diff --check` exited
  0. The installed version and SHA256 above were re-read and are unchanged.
  Researcher's retained declaration/probe directories are evidence, not staged
  release assets. No process matched its owned probe path. Existing Claude
  sessions referencing declaration roots were not launched by this task and
  were left untouched; no ownership-based cleanup was inferred.

Historical blocker, resolved by the latest Codex-only instruction: either authorize a MindPlayer-owned
Claude input/queue implementation (a larger change to native input/queue
ownership), or choose per-prompt consent at the actual execution boundary
(retains the native queue, but queued inputs choose later than the approved
composer-time design). A Codex-only rollout would also change the requested
cross-provider scope. Do not silently choose any of these alternatives.

## Required regressions before release

- Same session: directly type and submit Jev / native / Jev, with fresh choice
  on all three and exactly two judgments. Assert model AND effort on consumers.
- Duplicate text, queue edits, two panes/providers, stale/cancelled selection,
  late ACK and follow-up ownership do not share or inherit consent.
- Unselected Enter sends no model request; F6 does not submit. Preview is closed
  initially, uses actual snapshot bytes, reports omitted context and truncation.
- Native shell, slash commands, approvals, Escape, explicit steer, busy queue,
  Ctrl-G, pane/footer geometry and genuine-exit cleanup remain valid.
- Missing key, judge failure and invalid IPC fail visibly without fallback/retry.
- Inspect fixture writes/finalizers before running impure tests; no short outer
  timeout. Check exact task-owned processes/ports/temp resources after runs.

## Version and installation gate

Existing source/installed version: 0.55.14. Installed executable:
`/Users/example-user/.local/bin/mindplayer`, SHA256
`1d3af388be981877ed2c297b0b7c97ecb01bae4adce8c2d66cdd9d325aadf43a`.
Source HEAD: `54529865aa69ef134c25b88a78c8139ce99b181f`, dirty.

Only bump to 0.55.15 after the approved behavior has a working implementation.
Run targeted tests, fmt/clippy, release build from the same tested tree with no
version override. Preserve the exact old binary before atomic replacement; verify
the exact installed path/version/checksum and behavioral tests. Record provider
inference and platform/IME gaps honestly. Do not replace working 0.55.14 with an
incomplete approximation of the approved interaction.
