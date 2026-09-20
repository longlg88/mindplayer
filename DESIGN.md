# Design

## Approved direction — 2026-09-20

The user selected **C / token analysis** in `docs/session-trace-options.html`.
Implement this in the existing Rust TUI: selected session → `:trace` → request
usage table, selected-request execution details and repeated-read evidence.
Esc restores the session list. Preserve background PTY processing while viewing
the trace. Read the selected session's local log; never infer exact file token
costs or unrecorded reasoning. Cached input is a subset of input, not additive.

The HTML files are illustrative previews, not application surfaces. Earlier
browser-dashboard and separate-window designs below are historical drafts,
superseded by this decision. Trace implementation follows on a new branch after
the account-fix/refresh release.

## Source of truth

- Status: Draft
- Last refreshed: 2026-09-20
- Primary product surfaces: session list, dedicated session trace window
- Evidence reviewed: README.md, mindplayer-core session/token models, the TUI
  entrypoint and renderer, and the local Codex JSONL event schema.

## Brand

- Personality: dense, calm, operator-oriented, trustworthy.
- Trust signals: show source event type, timestamp, token scope, and whether a
  statement is observed or inferred.
- Avoid: pretending hidden model reasoning is available, decorative dashboards,
  or uncited AI-generated explanations.

## Product goals

- Goals: open one session's trace from the list; explain token usage by turn;
  connect request, observable decision record, tool execution, and result.
- Non-goals: expose private chain-of-thought, rewrite provider transcripts, or
  add a second token-counting source that disagrees with provider logs.
- Success signals: a user can answer “what ran, why did it run, and how many
  tokens did it cost?” without opening the raw JSONL manually.

## Personas and jobs

- Primary persona: an engineer comparing agent sessions and investigating cost,
  latency, or an unexpected action.
- User jobs: inspect a single session, find the expensive turn, verify a tool
  result, and distinguish observed facts from summaries.

## Information architecture

- Primary navigation: session list → dedicated trace window.
- Core routes/screens: mindplayer trace <session-id>; Overview, Timeline, Raw
  log tabs; session switcher in the trace window.
- Content hierarchy: session identity → aggregate tokens → turn timeline → raw
  evidence.

## Design principles

- Evidence before interpretation: every decision summary links to the request,
  tool call, or result that supports it.
- Token scopes stay explicit: cumulative session totals and per-turn deltas are
  never presented as the same number.
- Progressive disclosure: the default view is scannable; raw payloads and full
  command output expand on demand.

## Visual language

- Color: MindPlayer dark terminal palette; blue for navigation, green for
  successful execution, amber for uncertainty, red only for errors.
- Typography: system sans for labels, monospace for ids, commands, paths, and
  event payloads.
- Spacing/layout rhythm: compact 8px grid, restrained borders, one bright focus
  state.
- Shape/radius/elevation: 10px panels, 1px borders, no floating-card shadows.
- Motion: short tab/filter transitions only; respect reduced-motion preference.
- Imagery/iconography: text badges and small inline symbols; no decorative art.

## Components

- Existing components to reuse: session identity, agent/status colors, token
  formatting, footer shortcut language.
- New/changed components: trace window shell, token metric cards, token-by-turn
  table, evidence timeline card, raw-event disclosure, source/inference badge.
- Variants and states: loading, unavailable source file, partial transcript,
  malformed event, empty session, active/failed tool call.

## Accessibility

- Target standard: WCAG 2.1 AA intent for the HTML prototype.
- Keyboard/focus behavior: visible focus, logical tab order, Esc closes the
  trace window, / focuses search, and all filters are buttons.
- Contrast/readability: never use color alone for state; every status has text.
- Screen-reader semantics: landmark regions, button labels, table headers, and
  native details disclosures.
- Reduced motion and sensory considerations: no required animation.

## Responsive behavior

- Supported breakpoints/devices: desktop-first; usable down to 760px wide.
- Layout adaptations: the session rail becomes a horizontal selector below the
  header; metric cards wrap; evidence cards become single-column.
- Touch/hover differences: buttons remain full-label controls; hover is optional.

## Interaction states

- Loading: show “reading transcript” with the known file path.
- Empty: explain that the provider did not record token/event details.
- Error: keep aggregate metadata visible and show the failed source read.
- Success: show source timestamp and “observed” labels.
- Disabled: raw payload actions are disabled when the source is unavailable.
- Offline/slow network: this view is local-first; TypeSafe summaries, if ever
  enabled, are optional and must not block raw evidence.

## Content voice

- Tone: factual and concise.
- Terminology: “observed” for JSONL facts, “summary” for derived text,
  “decision record” for an auditable action rationale, “reasoning” only as a
  token category—not as a claim that hidden thought is displayed.
- Microcopy rules: show units and scope beside every number; say “not recorded”
  instead of guessing.

## Implementation constraints

- Framework/styling system: standalone HTML prototype first; production surface
  should follow the existing Rust TUI conventions.
- Design-token constraints: reuse the existing dark palette and compact density.
- Performance constraints: parse only the selected session; cap raw payload
  previews; avoid rereading all sessions when opening the trace.
- Compatibility constraints: Codex, Claude, Kiro, and Cursor have different
  evidence levels; the UI must support partial data.
- Test/screenshot expectations: verify keyboard navigation, narrow layout, and
  source/inference labels before production implementation.

## Open questions

- [ ] Should mindplayer trace <session-id> open a new terminal window through a
  platform-specific launcher, or reuse the current terminal process?
- [ ] Which provider-specific event fields should be normalized in v1?
- [ ] Should derived summaries be local deterministic labels only, or optionally
  call TypeSafe after the evidence view has loaded?
