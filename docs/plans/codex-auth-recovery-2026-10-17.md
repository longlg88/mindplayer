# Codex account verification and recovery

Scope: repair account verification and preserve new/resume/handoff launches across
authentication recovery. Do not infer account identity from a display label,
silently rebind a mismatch, copy credentials, or automatically log out any home.

Evidence: the required identity check currently uses optional quota enrichment
with a 250 ms grace. A read-only Codex 0.163.0 exchange on the selected inherited
home returned quota at 1122 ms and account/read at 1859 ms. The latter exposes
workspaceRouting.chatgptAccountId. The current verifier can discard that response.
An inherited account cannot recover through the existing Accounts sign-in action.

Alternatives:

1. Login on every launch: unnecessary browser work and shared-home disruption.
2. Increase the quota grace / manual v-v binding: still couples identity to usage
   and loses the requested launch after failed authentication.
3. Required live account check, explicit identity confirmation, targeted sign-in,
   then fresh verification and one-shot continuation: selected. Keep the existing
   workspace-plus-user fingerprint, and never silently accept a different login.

Implementation / regression plan:

- First reproduce delayed identity, signed-out, quota-error, and continuation
  failures using owned temporary fixtures. No live credential replacement.
- Separate required identity reads from optional quota presentation. Refresh
  managed authentication and preserve workspace separation and existing bindings.
- Run verification asynchronously. Keep the exact pending command/account/home,
  including initial handoff input, while authentication or confirmation is pending.
- Allow an explicit native sign-in for inherited Codex without logout. Recheck
  after login exits; failure/cancel must not start inference or retry login forever.
- Show the actual email/workspace when binding; mismatch needs an explicit user
  decision. A matching stored binding continues automatically.
- Verify targeted tests, full Rust suite, formatting/clippy, and installed-path
  behavior if installation is performed. Report live-login gaps honestly.
