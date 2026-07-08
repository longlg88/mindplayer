# Usage audit / stats — design

Status: approved, implementing.

## Why

Asked "how many orchestration lanes did I just open" and had to answer it by
hand-parsing `~/.mindplayer/state.json`'s `handoff_links` for `orch:`-prefixed
keys and cross-checking timestamps — there was no way to just ask mindplayer.
`state.json` is a **snapshot** (current archived/labeled ids), not a
**history**, so it can't answer "how many/how often" questions at all.

Goal: a lightweight, append-only record of mindplayer's own operations, and a
stats popup (`u`) that turns it into a few at-a-glance numbers. Scope is
mindplayer's own actions (opens, closes, automation sends, uptime) — not
transcript content. Metrics over point-lookup: the popup answers "how much/how
often", not "show me every event".

## Storage — `~/.mindplayer/audit.jsonl`

Append-only, one JSON object per line, alongside the existing `state.json`
sidecar. `MINDPLAYER_AUDIT` env var overrides the path (mirrors
`MINDPLAYER_STATE`), so tests never touch the real file.

No session id, title, or cwd is recorded — only counts/kinds/timestamps. The
goal is "how much", not "what", so there's nothing identifying to leak and the
log stays tiny (a few hundred bytes/day at realistic usage).

Corrupt/partial lines (e.g. a write cut off by a crash) are skipped
individually rather than invalidating the whole file — this is the one place
this sidecar differs from `state.json`'s "whole file corrupt → empty" fallback,
because JSONL is naturally line-recoverable and a crash mid-append is the
realistic failure mode here.

```json
{"ts":"2026-07-08T09:12:03Z","kind":"session_open","agent":"codex"}
{"ts":"2026-07-08T09:15:41Z","kind":"orchestration_start","children":4}
{"ts":"2026-07-08T09:20:02Z","kind":"broadcast","children":4}
{"ts":"2026-07-08T09:31:00Z","kind":"catchup_sent"}
{"ts":"2026-07-08T08:55:00Z","kind":"app_start","run_id":47309}
{"ts":"2026-07-08T10:02:17Z","kind":"app_stop","run_id":47309}
```

`run_id` is `std::process::id()` — needed because more than one mindplayer
instance is routinely open at once (one per project); pairing `app_start`/
`app_stop` by process id (not just chronological order) is the only way to get
correct durations when instances overlap.

## Events (v1)

| kind | fields | where |
|---|---|---|
| `app_start` / `app_stop` | `run_id` | `main.rs`, around the `run()` call |
| `session_open` | `agent` | `spawn.rs::spawn_pending()`, the single choke point every new PTY (new/resume/handoff/orchestration lane) passes through |
| `session_close` | — | `session_list.rs::close_selected()` (`x`) — **logged, but not shown in the popup** (see below) |
| `handoff` | — | `handoff_sync.rs::confirm_handoff()` (`h`) |
| `catchup_sent` | — | `session_list.rs::send_catchup()` (`c`), only on a successful send |
| `orchestration_start` | `children` | `orchestration_lanes.rs::start_orchestration()` (`o`) |
| `broadcast` | `children` | `orchestration_ui.rs::confirm_broadcast()` (`b`) |
| `dispatch` | — | `confirm_main_dispatch()` (`m`) and `confirm_dispatch_apply_input()` (`M`) — same kind, no need to distinguish |
| `peer_review` | — | `run_peer_review_cycle()` (`p`) |
| `synthesis` | — | `run_synthesis_cycle()` (`s`) |

`orchestration_start`/`broadcast`/`dispatch`/`peer_review`/`synthesis` map to
a feature slated for removal later — kept minimal (one line each, no extra
fields beyond what's free) since that code (and its audit lines) will likely
be deleted together.

## Active-time aggregation

`app_start`/`app_stop` pairs are matched **by `run_id`**, not by chronological
order (needed for overlapping instances — see above). A `run_id` with no
matching stop is either:

- **the current process** (`run_id == std::process::id()`) → still running,
  counts up to `now`.
- **an abandoned run** (killed/crashed, from a past process) → capped at 8h
  contribution rather than trusted at face value, so one hard-killed instance
  can't inflate the "all-time" total by days. This is a deliberate, documented
  simplification — not solved by heartbeats or PID-liveness checks (YAGNI for
  a personal single-user tool).

## Stats popup (`u`)

Confirmed layout (option 3 from the design exploration:
https://claude.ai/code/artifact/1eae80f6-657a-468c-9b3b-59c43aaf95bd):

```
┌ mindplayer usage ──────────────────────────────────────────┐
│                                                              │
│  active time  ▂▃▅▇▆▄▃▂▁▃▅▇█▆  14d                           │
│               2h 14m today  ·  118h 40m all-time            │
│                                                              │
│  sessions opened  ██████████████████████████████░░░░░░░░  412        │
│                    codex 260 · claude 140 · kiro 12          │
│                                                              │
│  handoffs 9  ·  catch-up 1                                  │
│  ─────────────────────────────────────────────────────      │
│  orchestration (phasing out)  lanes 7 · cmds sent 0          │
│                                                              │
│  esc / enter  close                                          │
└──────────────────────────────────────────────────────────────┘
```

- Real `ratatui::widgets::Sparkline` for the 14-day active-time trend — no
  custom heatmap/threshold logic to maintain.
- A single custom proportional bar (codex/claude/kiro `Span`s) for the agent
  split — cheaper on vertical space than three separate `Gauge` rows.
- **`archived`/`session_close` count is intentionally left off this screen**
  even though the event is logged — decided during design review, no need
  to surface it here; it's just not interesting alongside the rest.
- "today" reuses the same rolling-24h definition the session list's
  recent/older split already uses (`touched_recently`'s window), not a
  calendar day.
- Key: `u` (open/close), unused at the `Focus::List` level. Recomputes stats
  fresh from `audit.jsonl` each time it's opened — no caching/indexing, the
  file is small enough that a full read+parse is effectively instant.

## Testing

- `mindplayer-core::audit`: append+read round-trip; a corrupt middle line is
  skipped without losing the lines around it; `run_id` pairing across
  overlapping instances; unmatched current-run-id counts to `now`; unmatched
  foreign run-id is capped, not trusted at face value; today/all-time boundary
  respects the rolling-24h window.
- `mindplayer-tui`: `MINDPLAYER_AUDIT` redirected to a temp file per the
  existing `STATE_ENV_LOCK` pattern; verify each instrumented action appends
  exactly the expected event; verify `cargo test --all` never touches the real
  `~/.mindplayer/audit.jsonl` (mtime check, same as the `state.json` check
  already done for other features).
