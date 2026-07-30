# Category context sync — design

Status: implemented.

## Why

Categorize (shipped) groups sessions by topic, but the sessions in a category
know nothing about each other. The reported flow:

1. work in one session, realise it needs a topic, create a category
2. `n` a second session, put it in the same category
3. the new session has no idea what the first one did

Sessions are deliberately separate CLIs (Codex / Claude / Kiro) with separate
context windows, so this only gets worse as each accumulates its own history.

## What already exists (and why it never fires)

`spawn_thread_sync_for` (`app/handoff_sync.rs:202`) already does the hard part:
it reads each peer's **real transcript** (`handoff::extract_transcript`), builds
one `## {agent} lane` section per peer, writes an artifact under
`~/.mindplayer/handoffs/`, and injects a prompt — inline when small, artifact
path plus a tail excerpt when large. It runs on a background thread because
reading peer transcripts synchronously froze the UI.

Notably this needs **nothing from the agent**. No note file to maintain, no new
tool to teach. That is why this design carries no notes/DB component: an earlier
draft had agents writing shared notes, and it was dropped once it became clear
the transcripts are already the source of truth.

Two things stop it from working for categories:

**1. Peers are lineage-only.** `thread_peer_sessions` (`:163`) selects sessions
sharing a `thread_root`, i.e. a handoff ancestry. A session created with `n` has
no handoff link, so `thread_root(B) == B`, peers is empty, and no sync ever runs.

**2. It syncs once per session, ever.** `thread_sync_needed` (`:188`) is
`!thread_sync_at.contains_key(id) && !state.thread_synced.contains(id)`, the
second half persisted. Right for a one-shot handoff; wrong for three sessions
working a topic for hours — the second one learns the first one's state once and
never again.

## Design

### Peers include category members

```rust
// same handoff thread OR same category
```

One predicate change. Categories and threads compose: a thread inside a category
contributes all of its lanes.

### Delta injection replaces "sync once"

The existing comment justifies once-ever by saying a timestamp comparison
"re-triggers forever". Re-reading it, the harm was not the re-trigger — it was
re-sending the **whole peer transcript** every time. Shrink the payload and the
problem goes away:

- keep a watermark per `(target, peer)`: the peer transcript's byte length at the
  last sync
- on resume, a peer with `len > watermark` contributes only the bytes past it
- no peer past its watermark → no sync at all, so it terminates on its own
- `len < watermark` (file rewritten or rotated) → discard the watermark and read
  the whole file

Byte offsets rather than timestamps because `parse_turn` returns `(role, text)`
with no timestamp, and `extract_jsonl_transcript_with_limit` (`handoff.rs:339`)
already branches on file length and does a bounded tail read — the offset shape
is already there. Agent transcripts are append-only JSONL, so the offset holds;
the shrink case is the only exception and it fails safe.

State changes from a set to a per-pair map:

```
sync_marks: { "<targetId>": { "<peerId>": 12345 } }
```

`state.thread_synced` stays for the existing one-shot handoff path so old state
files keep working.

### Trigger: per-category toggle

Auto-sync is a **per-category** setting, so a tightly-coupled topic can stay in
lockstep while a loose one stays quiet.

- **on** — entering a session in that category injects the delta, but only when
  the target is idle. A running turn is never interrupted. This mirrors catch-up,
  which sends immediately when idle and asks first when working/blocked.
- **off** — nothing automatic; only `sync now`.
- **sync now** — immediate regardless of the toggle; confirms first if the target
  is busy.

Stored on `Category`. The default is on, and it must be written as
`#[serde(default = "…")]` — a bare `#[serde(default)]` on a `bool` yields
`false`, which would silently load every existing category as disabled.

### Where it is configured: `t` on a category header

`t` on a header currently does nothing useful (`begin_category_pick` reports
"pick a session row first"), and a header already denotes the category itself, so
that keypress becomes the category menu. No new key is taken.

```
┌ mindplayer ────────────────────────┐
│ ▶ auto-sync        on              │
│   sync now                         │
│   rename…                          │
│   remove category                  │
└────────────────────────────────────┘
```

Arrow keys are structural only: `→` unfolds a category then steps inside, `←`
folds it and steps back out, and neither opens a session — `enter` does that.
An earlier cut had `→` toggling, and bound to resume on a session row; pressing it
on a category then bounced open/shut and eventually opened a session.

`rename` and `remove` are included because neither is reachable today —
`State::rename_category` exists with no caller, and emptying a category means
clearing each session one at a time.

## Testing

- peers resolve across a category with no handoff link between members
- a thread inside a category contributes every lane exactly once
- watermark: second sync with no peer activity injects nothing
- watermark: after peer activity, only the new bytes are included
- watermark: a shrunk peer file falls back to a full read
- auto-sync off performs no injection on resume; `sync now` still does
- a busy target is not interrupted by an auto sync
- `serde` round-trip: a category persisted before this feature loads with
  auto-sync **on**, not off

## Open questions

- **Cross-project categories.** `session_category` is id→id with no cwd, and
  Global scope lists every project, so one category can span repos. Peer context
  from another repo may be exactly what is wanted (a migration touching two
  repos) or pure noise. Not resolved; worth watching once it is in use.
- **Kiro idle detection.** The "only inject when idle" rule leans on status, and
  Kiro has no lifecycle hook — its status comes from screen-text matching
  (`kiro_patterns.rs`). Gating is therefore less certain for Kiro than for
  Claude/Codex.
- **`t` is now overloaded** — assign-category on a session row, category menu on a
  header. Contextual and discoverable from the footer, but two meanings for one
  key.
- **Context cost.** Every injection consumes the target's context window. Deltas
  keep it small, but a very chatty peer could still push a long tail.
