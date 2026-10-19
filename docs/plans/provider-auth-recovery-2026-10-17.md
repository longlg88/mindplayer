# Provider-neutral account verification and Claude recovery

Scope: extend existing Codex verification/recovery to multiple Claude accounts;
remove user-specific account/company examples from repository content. Preserve
all existing unrelated changes, existing account files, credential homes, and
running sessions. Never derive identity from a display name or silently rebind,
copy credentials, log out, or select a real OAuth account.

Latest user boundary: desktop `app/` is excluded; terminal/core changes are allowed.
The PTY regression found that native recovery login still used the list scope
instead of the held conversation cwd. Reuse the lifecycle's selected recovery
context for login as well as verification; standalone login keeps the current
scope. Preserve the failing cross-directory PTY test before making this repair.
With multiple held projects, prefer the focused held command for the selected
account, then a deterministic held fallback, then standalone scope. Independent
architecture review found this second case; both providers' regression tests
failed on the wrong project before the shared helper was repaired.

Alternatives (before implementation):

1. Always log in on every launch: simple but disruptive, especially shared homes;
   rejects healthy reusable authentication and does not establish correct identity.
2. Duplicate Codex's lifecycle for Claude: provider-local changes but doubles the
   asynchronous race/continuation surface and creates inconsistent behavior.
3. Reuse one verification lifecycle with provider-specific identity readers and
   stored fingerprints: selected. Keep existing Codex bindings backward compatible;
   add a separate optional Claude binding. Unsupported providers stay unchanged.

Research: cross-check official Claude Code authentication/CLI/environment docs
against installed CLI help and sanitized native status. Do not mistake cached
identity metadata for a successful live inference or OAuth restoration. Record
unsupported response shapes and real-login limitations explicitly.

Research decision: installed Claude Code 2.1.302 `auth status` accepts invalid
synthetic OAuth tokens and reads saved account metadata. Status alone must never
authorize a conversation. Compare three verification mechanisms: cached native
status (reject as proof), a native model request (reject per-launch inference cost
and side effects), and a required profile request using the installed official
CLI's source-traced endpoint/response contract (selected, fail closed). Treat that
profile endpoint as an internal compatibility boundary, not a documented public
API. Preserve the actual launch cwd/settings and config-specific credential
namespace. No successful live profile or OAuth restoration may be claimed
without a successful real response; missing real credentials is an explicit gap.

Implementation sequence / ownership:

- Leader: read current lifecycle and write bounded regression cases before
  generalizing it; share checks, held commands, one-use approvals, login completion,
  explicit confirmation, safe focus, and panel messages for Codex and Claude.
- Core executor: provider-specific Claude identity reader and timeout/shape/env
  tests; backward-compatible optional binding and explicit default Claude home.
- Sanitization executor: replace specific labels/company paths in docs, fixture,
  and example content without changing user configuration or git history.
- Independent review: inspect account/home coupling, API-key/gateway precedence,
  failed/cancelled login, stale results and duplicate/renamed accounts.

Acceptance / regression tests:

- Arbitrary names (not a fixed company allowlist) and both providers work for new,
  resume, and handoff. Matching identity proceeds once; mismatch/missing identity
  never starts conversation before explicit confirmation or login recovery.
- `v` shows actual provider identity; second `v` refreshes before binding. A saved
  binding failure rolls back memory. `l` targets exactly the selected home;
  successful login is followed by fresh identity checks and original continuation.
- Claude inherited and isolated homes do not inherit ambient account/gateway
  credentials; no logout, credential copying, or unrelated session adoption.
- Delays, login cancellation/failure, focus changes, and concurrent account checks
  preserve the existing Codex safety contracts for both providers.
- Case-insensitive repository content audit has no user-specific company/account
  literals (excluding .git, build output, and local live account configuration).

Verification: targeted RED/GREEN regressions; workspace Rust tests; fmt; clippy;
Node/doc fixtures affected by sanitized examples; real PTY new/resume/handoff
tests. If installing, preserve rollback, build exact source, record revision,
dirty state/version/checksum and exercise the exact installed executable. Report
real OAuth selection as untested unless explicitly performed by the user.
