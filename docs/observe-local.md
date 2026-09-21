# Observe Trace

Version: `0.43.0`.

Open a session in MindPlayer, then press **Ctrl-G** to replace the live viewport
with its full-screen Trace. Press **Ctrl-G** again or **Esc** to return to the
same live session. The former Ctrl-O sidecar and `:observe` command popup are
retired.

The selected session's PTY keeps running while Trace displays available local
transcript events. The primary token figure is provider-recorded usage
accumulated across the selected turn: context input, cached input, output, and
reasoning. The selected user prompt's UTF-8 text estimate is secondary and
carries a `~` marker because provider logs do not record message-only
tokenization. Session-cumulative totals are intentionally omitted.
**Up/Down** selects a prompt. The mouse wheel selects prompts while it is over
the prompt rail and scrolls event history while it is over the execution pane;
**PageUp/PageDown** also scrolls event history.
Trace is read-only and consumes keys and mouse input while open, so navigation
never leaks into a mouse-aware Codex session. The toggle is per live pane, in
memory only.

Observe Trace reads local records only, makes no TypeSafe calls, and installs
no provider hooks. Usage updates depend on when the provider writes its log.
Missing usage is not zero. Usage scope is stated in the panel; a provider API
response is not necessarily a complete user prompt turn.

Recorded tool calls/results and public messages are observable; hidden reasoning,
unrecorded subprocess activity and separate child-agent transcripts are not.
Long records and history are bounded; the panel reports partial coverage.
Providers without a supported execution-log adapter show that limitation.

Existing processes continue running their loaded version; start a new
MindPlayer process after upgrading to use this build.
