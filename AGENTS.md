# MindPlayer-specific UI proposal lessons

This file supplements, and does not override, working2's root AGENTS contract.

- Latest UX override (2026-10-19, 0.55.16): Codex F6 is a session-local sticky
  ON/OFF toggle, initially OFF. Do not require repeated F6 for each prompt or
  invalidate the session mode on draft edits/submission. Copy the current mode
  onto each actual UserMessage; later toggles never rewrite queued choices.
  Remove F7 context preview. Show the pane-bound Codex account label in both split
  and zoom, not merely global quota labels. This supersedes the older fresh-choice
  and optional-preview instructions below; Claude stays Jev-free and desktop
  remains out of scope. Version bump and exact local install are requested.

- Latest scope override (2026-10-19): remove Claude Jev integration completely;
  apply the approved per-prompt toggle and optional context preview to Codex
  only. Claude sessions must have no Jev selector/call/status wrapper. Preserve
  Claude native sessions, account/auth code, credentials and controls. This
  resolves the earlier Claude identity blocker without taking over its queue.
  Continue the already-requested version bump and exact local install after
  real Codex behavior verification; do not ask again for this settled scope.

- Implementation approval (2026-10-19): implement proposal 1's per-prompt input-row
  toggle with proposal 4's optional, initially closed exact outbound-context
  preview; bump and locally install the terminal app, not the desktop app. This
  supersedes startup/run-local inheritance and the comparison-only boundary
  below. EVERY prompt, including queued and repeated inputs in one session,
  needs a fresh choice. Bind choice to actual native submission identity; never
  guess queue ownership from a pane flag, matching text or an unproven FIFO.
  Preserve native controls and validate the exact installed consumer, not just
  HTML or build metadata. Do not claim an unimplemented privacy/history feature.
- Verified implementation boundary (2026-10-19): installed Claude 2.1.303's
  prompt.submit.turnId refers to the already-running turn, not a future queued
  input's turn. The inspected submit/turn/session types provide no proven bridge
  between those identities. Do not pass consent through model-visible context,
  infer FIFO/text ownership, or assume turn.start text rewriting works because
  Next accepts it structurally. Changing native queue ownership or moving the
  choice to execution time requires an explicit UX/scope decision. Keep the
  incomplete per-prompt draft disabled and do not bump/install it as complete.
- Repeated prompt-scope correction (2026-10-19): the latest HTML request is
  Jev use/nonuse selection for EVERY new prompt, including the second, third
  and later inputs in the SAME ongoing session. All five alternatives must
  reset to unselected; do not offer session defaults, previous-choice inheritance,
  or startup-only selection as alternatives. Show and test repeated actual input,
  not only a manually loaded follow-up example. This supersedes the older
  startup-only/per-turn-control prohibitions FOR THESE COMPARISON PROPOSALS ONLY;
  it does not approve real app changes or revoke previous product approvals.
- Latest approval superseding 04 (2026-10-17): implement startup gallery **07
  configuration sheet**. Show external-transfer disclosure before selecting Jev;
  confirming that choice supplies consent once, without a redundant `c` checkbox.
  Keep the existing scope/scan and all pane boundaries. A cancelled pending Jev
  judgment must not suppress a new attempt with identical text or apply its late
  result to that attempt. Never fake native model-change confirmations from ACKs.
  Keep both measured account gauges visible even for a shared login, with a
  truthful shared-identity warning; do not call display repair auth restoration.
- Previous approval (2026-10-17): the user selected startup gallery **04 bottom
  startup bar** and requested real implementation, including downstream UI.
  This supersedes the comparison-only scope below. Preserve the existing
  scope/scan flow, gate first-list actions until a run-local choice, and inherit
  it for every new PTY. Native bypasses Jev completely; Jev requires explicit
  external-transfer consent and bounded readiness failures. No persistence,
  per-session selector, silent fallback, or hot switch. Verify actual Rust PTYs
  and real routed turns; HTML or mocked inference alone is not completion.
- Latest boundary correction (2026-10-17): the user now wants the Jev choice
  ONCE WHEN MINDPLAYER STARTS, not before each session. Show ten startup TUI
  alternatives in HTML. Existing/new session launches inherit this run's choice;
  reattaching an already-running pane must not silently change its effective mode.
  This supersedes older per-session/pre-open choice proposals, not the approved
  pane/footer/native-input behavior. Do not infer persistence or app-install scope.
- Latest repeated correction (2026-10-17): an HTML proposal for terminal UI
  must draw terminal-feasible character-cell UI, not web forms placed on a dark
  rectangle. Do not use browser radio/select/button styling as TUI controls.
  The user explicitly wants at least ten visually distinct UI ideas in HTML,
  without explanatory report prose. Do not substitute another Bash toy, app
  implementation, or a long direction assessment. Keep source/evidence outside
  the proposed terminal surfaces and preserve the existing panes/footer.
- Repeated correction (2026-10-16): when the user asks for UI that works in
  Bash/a terminal, deliver a runnable keyboard-driven terminal surface and
  verify it in a real PTY. An interactive HTML mock is not a terminal UI.
  Browser-only radio buttons/selects are report controls, not implementation
  evidence. Keep terminal prototype, real app integration and real inference
  claims separate. The latest explicit pre-open Jev use/no-use requirement
  supersedes the older prohibition on an enable choice at that boundary only;
  do not add a confirmation before every prompt.
- Repeated user feedback (2026-10-12/13): UI proposals must show **how the
  actual MindPlayer TUI changes**, not open as a prose-heavy design report.
- First viewport: show current and proposed screens together with changed areas
  visibly marked. Open a proposed panel only when that panel is actually part
  of the requested interaction; never invent a panel for an automatic service.
  Do not require clicks or scrolling to discover the main change.
- Preserve the actual 4-top/3-bottom pane composition and full-width quota
  footer. A before/after comparison on the HTML page is not a new app split.
- Keep visible commentary to short labels. Put rationale, alternatives,
  implementation constraints and evidence behind a closed disclosure or in
  DESIGN.md. Keep mock/proposed/not-implemented scope concise but visible.
- Verify the final localhost page in the browser, not just source assertions;
  show the user the revised view and distinguish prototype checks from Rust
  runtime evidence.
- Repeated existing-UI feedback: inspect the real surface before proposing a
  replacement. A self-authored DESIGN.md or mock is not proof of a user decision.
  A runnable standalone Bash demo is not a substitute for the existing
  MindPlayer look and interaction contract. Keep its rejected status explicit;
  use Ghostty when the user requests that terminal host.
  Explicitly distinguish retaining, extending and replacing Ctrl-G Trace; do
  not infer that approving an HTML URL approved an unstated navigation contract.
- The user cancelled the 0.55.0 Inspect work on 2026-10-13. Do not revive that
  design or implement new CCO-derived features from a candidate list without
  a new user decision. Compare capabilities against the actual current code first.
- Account rename changes the display label only; never move a provider home.
  Codex stores absolute rollout paths in its SQLite index, and other processes
  may still hold the original home. Rename verification must include existing
  session resume with the real Codex CLI/app-server, not only an auth sentinel
  or a fake CLI that echoes CODEX_HOME. Refuse reusing an occupied old slot.
- Account-isolation incident correction (2026-10-17): hiding a duplicate usage
  gauge does not resolve two intended independent logins returning the same
  identity. Verify both homes independently, trace the login path, and separate
  a confirmed prevention fix from restoration of the existing credentials.
  Fresh-login allocation must reserve an unoccupied home atomically, including
  after a display-label rename. Never repair identity by copying credentials,
  guessing a workspace from a label, or logging another account out silently.
- Repeated account-recovery correction (2026-10-17): a quota response is not
  a required authentication check. Never use optional enrichment timing for
  launch identity, or send the user into a verify/bind dead end when sign-in
  is needed. Preserve the exact account/home and new/resume/handoff command
  through explicit login, then recheck before one-shot continuation. A label
  is not identity; show actual user/workspace before binding a mismatch. Do
  not silently log out the shared default home or claim credentials restored
  from a parser fix, a green fake CLI, or a successful quota lookup alone.
- Provider-neutral recovery correction (2026-10-17): verification/recovery must
  cover both Codex and Claude multiple-account flows, not a provider-specific
  patch. Keep one asynchronous lifecycle with provider-specific identity reads.
  Display labels and company names must never select identity or routing; use
  generic fixtures/examples and verify arbitrary labels. Do not mistake cached
  native auth metadata for proof of a successful live OAuth request.
- Jev proposals: classify each submitted prompt and select a model inside the
  already chosen pane/provider. Do not reframe this as Codex-versus-Claude
  routing or expand it into an execution planner/monitoring dashboard. Keep the
  existing input surface. Repeated correction (2026-10-13): the service always
  judges and applies the model automatically. Do not add automatic/fixed modes,
  a model picker, an enable button, a settings popup, or per-turn confirmation.
  At most show the resulting current model as read-only text in the existing
  model line. Service/backend architecture and user-facing controls are separate.
  All explanatory and UI copy for this proposal must be Korean.
- Repeated correction (2026-10-15): every eligible submitted user turn must
  actually call Jev. Missing model-comparison evidence must prevent an unsafe
  model change, not prevent the judgment itself. Verify the installed input
  path with real authenticated Jev responses; mocked routing or a version bump
  alone is not completion. Never label retaining the native model as proven
  cost optimization when its comparison evidence is missing.
- Repeated UI correction (2026-10-15): show the complete user journey, not just
  a changed model label or a backend report: entering a session, composing,
  judgment pending, model request, execution, completion, and fresh judgment
  for follow-up turns. Show actual-screen reconstruction beside the proposal,
  keep the ordinary path visible without opening disclosures, and label
  synthetic UI states separately from verified runtime behavior.
- Routing failure correction (2026-10-15): validate the same credential provider
  and OS trust path used by the user's pane. A successful direct TypeSafe test
  does not verify Vercel Gateway. Preserve certificate verification, never retry
  a turn automatically, and surface a bounded cause instead of only a generic
  routing failure. Distinguish prompt-like session rows from legitimate forks
  using protocol evidence, not title or zero-token heuristics.
- Gateway response correction (2026-10-16): observed model-configuration Choice
  probabilities may be rounded to hundredths. Handle only quantized, feasible
  distributions whose winner cannot change within rounding intervals; preserve
  raw values and keep capability/quality-policy validation strict. Verify real
  consecutive turns, not one successful response or a transport-only probe.
- Execution-status correction (2026-10-16): separate Jev selection, request
  acceptance, actual turn start and terminal outcome. An ACK is not running;
  `turn/started` is. Never gate normal execution/completion on `model/rerouted`,
  or label a running turn unknown merely because no serving receipt exists.
  A server model-change report is supplementary, not an inference/effort receipt.
  Verify ACK-only and no-reroute execution in the exact installed PTY path,
  including late ACKs, stale turns and follow-up resets; tests must establish
  user-visible behavior rather than preserve the faulty label.
- Repeated routing-verification correction (2026-10-16): a synthetic Codex
  backend is not proof of the user's native model/effort application. Correlate
  the actual pane's request and turn ID with the native rollout's `turn_context`
  model, effort and collaboration settings. An assistant's claim about its own
  effort is not configuration evidence. Distinguish new user turns from inputs
  steering an already-running turn before claiming judgment on every prompt.
- Native busy-input constraint (2026-10-16, Codex 0.162.1): never hold a
  `turn/steer` RPC until the old turn completes. The native event loop awaits
  that response, so doing so can block Escape and approvals. A synthetic
  backend completing independently does not test this deadlock. Native queue
  keymap overrides must also preserve idle `!shell` submission; do not ship a
  queue-only Enter binding or fabricate review/compact errors for regular turns.
- Routing-contract migration lesson (2026-10-18): before freezing a model-ID,
  rubric-version or request-shape change, trace all consumers, including shared
  PTY assertion helpers, exact state-key lists, semantic label counts and embedded
  asset lists. A passing isolated Node contract does not establish fixture or
  installed-binary compatibility. Preserve exact assertions and captured REDs;
  replace obsolete model-name selectors with the same model-independent meaning,
  never weaken a duplicate/lifecycle check just to accept the new output.
- Native-lifecycle verification lesson (2026-10-18): trace pane removal through
  the PTY owner, not just its display layout. Ctrl-Q hides a pane and retains
  its running PTY; app quit/native exit ends it. Test both resource retention
  and genuine-exit cleanup without changing native behavior to fit a fixture.
  Trace portable-pty's actual TERM/HUP/grace consumer before blaming SIGKILL.
- Claude baseline/pricing lesson (2026-10-18): a reviewed registry must account
  for the actual native baseline, including explicit specialized IDs, without
  silently enrolling ordinary sessions in verification-gated models. Preserve
  captured pre-judge failures and test the real broker/Mod consumer, not only
  launcher presence. Display only allowlisted failure reasons, never upstream
  text. Token price references need exact IDs, dated sources, conditional tiers
  and expiry; they establish neither subscription quota nor actual native bills.
