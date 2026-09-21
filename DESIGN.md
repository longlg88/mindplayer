# Design

## Approved direction — 2026-09-21

The user selected **A / full-screen Trace** in `docs/observe-trace-ui.html`.
From a focused live session, **Ctrl-G** replaces the session viewport with its
Trace view; **Ctrl-G** or **Esc** returns to that same live session. The PTY
continues to run and ingest output behind the Trace view. Ctrl-G reuses the
experimental observer command entry point instead of adding a function key
that conflicts with macOS media controls or stealing a new PTY editing chord.
The intermediate command popup and `:observe` command are retired. Read the
selected session's local log; never infer exact file token costs or unrecorded
reasoning. The primary token figure is provider-recorded usage accumulated
across the selected user turn; the user's literal prompt text estimate is
secondary and explicitly marked `~`. Cached input is a subset of input, not
additive.

The HTML files are illustrative previews, not application surfaces. Earlier
browser-dashboard and separate-window designs below are historical drafts,
superseded by this decision. Trace implementation follows on a new branch after
the account-fix/refresh release.

### Experimental observer review — 2026-09-21

The former `Ctrl-O` sidecar resembled a second session pane and its raw,
line-oriented transcript did not make a prompt → observed tool call/result →
usage sequence scannable. It is superseded by the full-screen Trace and should
not remain as a second observer entry point. The implementation uses a session
identity, then a selectable prompt rail with a local prompt-text token estimate,
provider-recorded request context, and observed execution events. Session-
cumulative totals are omitted from Trace because they obscure the selected
prompt. The agent PTY keeps running behind that view.

## Source of truth

- Status: Approved prototype direction
- Last refreshed: 2026-09-21
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
- Content hierarchy: session identity → prompt selection → that prompt's token
  usage → observed execution evidence.

## Design principles

- Evidence before interpretation: every decision summary links to the request,
  tool call, or result that supports it.
- Token scopes stay explicit: Trace foregrounds provider-recorded usage for the
  selected turn, keeps the `~`-marked local estimate of the literal user text
  secondary, and does not show session-cumulative totals.
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
