# Jev: current model families and Claude terminal integration

Scope: the terminal implementation only. Do not modify desktop `app/`, real
account configuration or credentials, existing running panes, native permissions,
or the startup sheet's once-per-run consent. No dependencies, commit or push.

Requested behavior: judge each eligible submitted user turn with Jev and choose
sufficient capacity inside the current model family of the pane's own provider.
Do not turn this into provider selection, a model picker, or a replacement UI.
Do not equate a selected model, accepted request, actual start and serving receipt.

Alternatives to check before implementation:

1. Add only a “latest” sentence to the judgment prompt. Small, but an old model
   remains executable; incomplete-context fallback can contradict the constraint.
2. Select the numerically/lexically largest model ID or native default. Reject as
   a capacity policy: these do not establish the current light/standard/frontier
   set, and the strongest/latest single release is not the whole family.
3. Restrict execution to an evidence-backed current-family set intersected with
   the selected account's live native catalog, then let Jev choose capacity.
   Prefer explicit bounded freshness evidence over guessed ID semantics. Refuse
   an unverifiable/outdated fallback instead of silently executing a legacy model.

Claude alternatives to verify against official docs and installed consumers:

1. Native interactive hook/control interface: preferred if it can actually apply
   the current turn's model while retaining approval, editing, Escape and resume.
2. SDK/stream-JSON runner: model controls exist but may replace native terminal
   interaction; do not silently substitute a new frontend.
3. Keystroke injection or an API-rewriting proxy: reject unless a supported,
   authenticated, race-safe protocol establishes the same-turn application. A
   hook message, file write or API ACK alone does not prove application.

Research: independently cross-check official provider documentation and installed
protocol/argument consumers in two rounds. Record exact compatibility boundaries;
do not read or print secrets, send test prompts without bounded intent, or change
real logins. If native Claude cannot meet the existing interaction contract, finish
the independently safe branch and report the precise missing interface/decision.

Implementation/verification sequence:

- Preserve failing regressions for latest-family exclusion, an old baseline,
  ambiguous/attached input, unavailable candidates, policy veto, and ultra scope.
- Implement the smallest shared restriction; no automatic retries/fallback to
  native or legacy models. Preserve per-pane ownership/cancellation and settings.
- Add Claude only through a source-traced native control boundary. Test exact
  submitted input, selected home/cwd, new/resume/handoff and busy/cancel/approval.
- Run targeted tests, then Node and Rust checks. Review with independent code and
  architecture lanes; repair genuine findings before accepting the result.
- If installing: preserve rollback, build/test the same source, record dirty HEAD,
  exact installed path/version/checksum and installed behavioral verification.
  Mocked responses are not native application or measured quality/cost evidence.

Stop condition: requested behavior is demonstrated in the real terminal path, or
a concrete unsupported native interface/required authority is documented without
claiming the blocked provider is connected. Preserve desktop file hashes.

## Bounded implementation decision

Native catalog/cache inspection and independent architecture cross-check agree:
there is no structured generation or capability-tier field. Optional upgrade
metadata is absent in the observed set; absence is not proof of currentness.
Use a versioned reviewed registry: light `gpt-6-luna`, standard `gpt-6.1-sol`,
frontier `gpt-6-astra`. This is the reviewed native release set, not a promise
of perpetual latestness or public release/entitlement evidence. Review expires
2026-11-18; unknown native defaults/upgrade destinations require a policy review.

Separate actual incoming baseline from the validated fallback configuration.
For a recognized, independently native-eligible legacy baseline only, catalog
mode may use the current frontier at the *exact same effort* as the migration
anchor. Do not invent supported efforts or recover unknown/denied models this
way. Keep actual baseline in Jev state, identify `c0` honestly as fallback, and
compute `changed` against actual baseline. Ambiguous input can use this current
anchor after a successful judgment; failed judgment still submits nothing.
This is an explicit conservative policy preference, not measured non-inferiority.
Ultra stays ultra-only; ordinary selection cannot acquire delegation. Empirical
policy mode cannot invent migration quality/cost evidence: reject a legacy
baseline conflict and never let its fallback bypass current membership.

Implementation ownership: executor owns current-family module, service/rubric and
new regression tests; test engineer owns integrated synthetic fixture IDs and PTY
assertions; leader owns integrated launcher wiring, bundling, diagnostics, docs,
independent review, installed verification and evidence.

## Claude native boundary: approved best-effort integration

### User-authorized follow-up (2026-10-18)

The user explicitly approved the best-effort native integration after disclosure:
an absent/crashed/timed-out extension may let Claude execute the original native
configuration. This exception is Claude-only; do not weaken Codex's fail-closed
path. Ordinary observed Jev errors must still prevent that prompt's submission.
Keep native terminal input, permissions, Escape, account home/cwd and new/resume/
handoff. No global plugin/settings installation, provider credential rewriting,
desktop changes, dependency additions or commits.

Use the existing bounded judgment rubric/client with a reviewed Claude current
family, not account labels or numeric ID sorting. Compare (1) native Mods with
the approved failure exception, (2) print/SDK UI replacement, and (3) keystroke/
API rewriting: choose (1), reject (2) for replacing the input surface and (3) for
unsupported same-request ownership. A session-local broker must own Jev secrets;
never put them in plugin source, native argv, native child environment or logs.
Revalidate exact official Mods APIs and installed consumers before writing them.

Tests must cover current-family-only menus, model+effort application at the
actual next() consumer, fresh judgment on identical follow-ups, cancellation/
late-response isolation, queued input ownership, side agents, ordinary Jev errors,
new/resume/handoff, Native bypass, and secret/metadata boundaries. Plugin tests
are not inference receipts; record real native application separately. Build and
exercise the exact local installed executable with a rollback binary preserved.

Ownership: leader owns broker/module, Rust bundling and install; executor owns
Claude policy/launcher regression tests; independent research verifies Mods contracts;
test engineer owns added PTY fixtures only after the launch contract is frozen.

The following finding records the former strict boundary, not an active refusal
of the now-authorized best-effort implementation.

Two research rounds checked the official Mods/events/API/reference contracts
against installed Claude Code 2.1.302. Native Mods can override model and effort
at `turn.step` and preserve native streaming; classic UserPromptSubmit alone
cannot change the model. However, strict enforcement is not supported by the
checked surface: missing/unloaded modules and a failed hook/catch may be skipped,
and the native request can proceed without the model override. A startup
handshake proves only the earlier load, not mandatory execution on each request.

The installed request consumer confirms the fallback (decimal byte offset
228983202 in SHA-256
`5262ce9de86243fed0d21edc39b271555d56780d0112e2b194439fe1c6662d37`).
The prompt mod runs before an ordinary classic admission guard, but the future
turn gets a newly minted UUID; a queued prompt's turnId identifies the old active
turn. A ticket does not prove later request rewriting. Classic `onFailure:block`
can enforce its own synchronous hook failure, not the later Mods request chain.
No runtime strict/required-mod flag was found in installed help or checked docs.
This is a bounded finding for this installed version, not a claim that native
model overrides do not exist or that future versions cannot support enforcement.

Alternatives remain: relax fail-closed for a best-effort native Mod; authorize a
different execution/enforcement surface; or retain the current boundary and defer
Claude. Do not implement one silently. At that historical checkpoint Claude was
on its pre-existing native path; the approval above supersedes that hold. No provider
request, login change, plugin install or global gateway setting was used to obtain
this finding.

Official cross-checks:

- https://code.claude.com/docs/en/plugins/mods/events#handle-a-hook-that-fails
- https://code.claude.com/docs/en/plugins/mods/troubleshoot#find-out-why-a-mod-does-nothing
- https://code.claude.com/docs/en/plugins/mods/troubleshoot#mods-that-run-in-the-hooks-worker-are-off-for-this-session
- https://code.claude.com/docs/en/plugins/mods/reference#turns
- https://code.claude.com/docs/en/hooks#block-the-action-when-a-hook-fails
- https://code.claude.com/docs/en/cli-reference
