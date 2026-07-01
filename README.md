<div align="center">

<img src="assets/mascot.png" alt="MindPlayer mascot" width="140" />

# MindPlayer

**Run many Codex &amp; Claude Code sessions like tabs — in one terminal window.**

Browse your whole session history, watch token usage, and juggle several live
sessions at once: start a task in one, switch out, drive another, switch back —
nothing stops in the background.

![License](https://img.shields.io/badge/license-MIT-7EA2F7)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust&logoColor=white)
![Tauri](https://img.shields.io/badge/macOS%20app-Tauri-24C8DB?logo=tauri&logoColor=white)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey)
![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen)

</div>

---

## 🤔 Why

Codex and Claude Code pile up **hundreds** of sessions across your projects — but
the CLIs give you no way to see them all, compare token usage, or run more than
one at a time. MindPlayer is a thin, fast layer on top: a history‑aware launcher
plus an embedded terminal, so sessions feel like **browser tabs**.

It reads your existing `~/.codex` and `~/.claude` transcripts **read‑only** — it
never modifies them — so it just works with the sessions you already have.

## ✨ Features

- 📜 **Full session history** — scans `~/.codex/sessions`, `~/.claude/projects`,
  and `~/.kiro/sessions/cli`, with real titles pulled from the first actual
  prompt (boilerplate skipped).
- 🤖 **Codex · Claude · Kiro** — browse, resume, and start sessions for all
  three from one list (press <kbd>n</kbd> to pick the agent).
- 🔢 **Token dashboard** — per‑session and total usage, Codex vs Claude vs Kiro.
- 🪟 **Many sessions at once** — resume or start several; each keeps running in
  the background. **Mark several** in the list (<kbd>Space</kbd>) and launch them
  together as live panes with one <kbd>Enter</kbd>.
- 🚦 **Live status** — each row shows `● working` (producing output now),
  `● idle` (running, waiting), or `○ done` (ended) at a glance.
- ⛶ **Live panes** — drive one session full‑screen or split the view across up
  to **6** at once. <kbd>Tab</kbd> (or <kbd>Ctrl‑w</kbd>) cycles focus,
  <kbd>Ctrl‑z</kbd> zooms the focused pane back to full size (toggle again for
  the split — no more squinting at a 6‑way grid), <kbd>Ctrl‑o</kbd> toggles the
  horizontal/vertical split, <kbd>Ctrl‑q</kbd> closes the focused pane, and
  <kbd>Ctrl‑x</kbd> pops back to the list.
- 🗓️ **Today first** — the list opens with **today's** sessions (KST) grouped at
  the top under a `today` / `earlier` divider, so what you're working on now is
  always in reach.
- ⚡ **Live & snappy** — the list auto‑reorders by recent activity and refreshes
  in the background; input feels native.
- 🏷️ **Labels** — tag any session with a subject (a new one at creation, or an
  existing one with <kbd>e</kbd>); it shows as `🏷 your label`.
- 🗂️ **Scope & archive** — view the current working dir or everything; archive
  finished sessions (originals untouched); hide sub‑agent/`/team` workers.
- 🌏 **Friendly input** — `Shift+Enter` soft newlines, full Korean/CJK (IME)
  support, and shortcuts that work on a Korean keyboard layout too.

Two front‑ends, one Rust core:

| | |
|---|---|
| 🖥️ **TUI** (`mindplayer`) | ratatui + an embedded PTY. Runs anywhere a terminal does. |
| 🍎 **macOS app** (`app/`) | a Tauri wrapper with xterm.js for the live terminal. |

## 📦 Install

> **TL;DR** — most people only need the **TUI**:
> ```bash
> curl -fsSLO https://raw.githubusercontent.com/longlg88/mindplayer/main/install.sh
> less install.sh
> bash install.sh
> ```
> The macOS app is optional, and `npm` is only for *that* (run inside `app/`) —
> **never in the repo root** (the root is a Rust workspace, it has no `package.json`).

**Prerequisites**

| For | You need |
|-----|----------|
| The TUI (everyone) | nothing — the installer downloads a prebuilt binary (`curl`/`wget`). Only `--build` needs **Rust**. |
| Driving sessions | the agent CLIs you use, on `PATH`: [`codex`](https://github.com/openai/codex), [`claude`](https://docs.anthropic.com/claude-code), [`kiro-cli`](https://kiro.dev/docs/cli/) — browsing works without them; a CLI is only needed to *resume/start* that agent |

**Install the TUI** — the installer downloads the latest **release binary** (no build).
Download the script, inspect it, then run it:

```bash
curl -fsSLO https://raw.githubusercontent.com/longlg88/mindplayer/main/install.sh
less install.sh
bash install.sh
```

Prebuilt binaries: macOS (arm64) and Linux (x86_64). On any other platform the
installer falls back to a source build automatically.

```bash
./install.sh --build             # build from source instead of downloading
PREFIX=/usr/local ./install.sh   # → /usr/local/bin (may need sudo)
./install.sh --bin-dir ~/bin     # a directory you choose
./install.sh --uninstall         # remove it
```

**Update**: download and re-run the installer — it always fetches the latest
release. For reproducible installs, fetch `install.sh` from a specific git tag
instead of `main`.

**Use it.** Run it in the project whose sessions you want — the first screen
asks **working dir** (this project) or **global** (everything):

```bash
cd ~/code/my-project && mindplayer    # manage THIS project's sessions
mindplayer ~/code/my-project          # …or point it anywhere, no cd
```

Press <kbd>n</kbd> for a new Codex / Claude / Kiro session. `mindplayer --help`
lists the rest.

**Optional — the macOS app.** Downloads the prebuilt `.app` from the latest
release and installs it into `/Applications` (re-run to update):

```bash
curl -fsSLO https://raw.githubusercontent.com/longlg88/mindplayer/main/install.sh
less install.sh
bash install.sh --app
# or from a clone:  ./install.sh --app
# no-sudo install location:  APP_DIR=~/Applications ./install.sh --app
```

**Develop MindPlayer itself** (from a clone): `make` shows all targets.

```bash
cargo run -p mindplayer-tui -- ~/code/my-project   # run against your project
make test                                          # cargo test --all
```

> ⚠️ Running `cargo run` (without a dir) or `npm` **at the repo root** is the
> common mistake: the root is a Rust workspace (no `package.json`). Build the
> TUI with `./install.sh` / `cargo`, and run `npm` only inside `app/`.

### ⌨️ Keys

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> <kbd>↓</kbd> / <kbd>j</kbd> <kbd>k</kbd> | move selection (`▶`) |
| <kbd>Enter</kbd> | open the selected session, **adding it to the live view** (resume, or focus if already shown); remove a pane with <kbd>Ctrl‑q</kbd>. In multi‑select mode, launch **all marked** at once |
| <kbd>v</kbd> | toggle **multi‑select** mode — then <kbd>Space</kbd> marks sessions and <kbd>Enter</kbd> launches them all (<kbd>Esc</kbd> cancels) |
| <kbd>Space</kbd> | mark / unmark the selected session (`✓`) — multi‑select mode only |
| <kbd>Tab</kbd> / <kbd>Shift‑Tab</kbd> | inside live panes, cycle focus forward / back (only when 2+ panes are open; otherwise <kbd>Tab</kbd> goes to the session) |
| <kbd>Ctrl‑w</kbd> | cycle live‑pane focus (same as <kbd>Tab</kbd>; needs 2+ panes) |
| <kbd>Ctrl‑z</kbd> | zoom the focused pane to full size (toggle again for the split view) |
| <kbd>Ctrl‑o</kbd> | toggle the live‑pane split (horizontal ⇄ vertical) |
| <kbd>Ctrl‑q</kbd> | close the focused live pane |
| <kbd>Ctrl‑x</kbd> | back to the list (the session keeps running) |
| <kbd>n</kbd> | new session — pick codex/claude/kiro, then an optional label |
| <kbd>h</kbd> | handoff the selected session to another provider |
| <kbd>d</kbd> | change the working directory (blank = global) and rescan in place |
| <kbd>e</kbd> | label the selected session (tag an existing one, or edit/clear its label) |
| <kbd>o</kbd> | start an orchestration group with a main lane and child lanes |
| <kbd>m</kbd> | ask the orchestration main lane to route work to specific child lanes |
| <kbd>M</kbd> | paste and apply the main lane's `MINDPLAYER_DISPATCH` block |
| <kbd>p</kbd> | run a child-lane peer review cycle |
| <kbd>s</kbd> | send child-lane results back to the main lane for synthesis |
| <kbd>x</kbd> | close (archive) & stop the selected session |
| <kbd>a</kbd> | toggle archived view · <kbd>g</kbd> toggle sub‑agents · <kbd>r</kbd> rescan |
| <kbd>q</kbd> | quit (stops all sessions) |

Inside a live session, <kbd>Shift+Enter</kbd> inserts a newline (<kbd>Enter</kbd>
submits), and Korean/CJK input works with the cursor tracking the prompt. The
**mouse wheel scrolls MindPlayer's own scrollback** (so history that ran off the
top stays readable). **Drag inside a pane to select &amp; copy just that pane's
text** to the system clipboard (via OSC 52) — so a side‑by‑side split never
copies the neighbor pane too. (For panes running a full‑screen mouse app like
Codex, the drag goes to that app; use your terminal's <kbd>Shift</kbd>+drag
native selection there.)

### 🧭 Orchestration

MindPlayer can run a public multi-lane orchestration thread across Codex, Claude
Code, or Kiro:

1. Press <kbd>o</kbd> to create a main coordinator lane plus numbered child
   lanes.
2. Press <kbd>m</kbd> on the orchestration thread to ask the main lane to decide
   which child lanes should receive the next work.
3. Copy the main lane's `MINDPLAYER_DISPATCH` block, press <kbd>M</kbd>, paste
   it, and MindPlayer sends each lane only its assigned instruction.
4. Press <kbd>p</kbd> when child lanes should review one another's results.
5. Press <kbd>s</kbd> to wait for child lanes to become idle and send the latest
   implementation/review context back to the main lane for synthesis.

Opening an orchestration lane with <kbd>Enter</kbd> does not auto-submit thread
sync prompts; explicit orchestration commands control when context is injected.

## 🍎 macOS app (optional)

Prefer a windowed app over the TUI? Build it with **`./install.sh --app`**, or
work on it directly — all npm commands run **inside `app/`** (never the repo root):

```bash
cd app
npm install      # xterm.js + Tauri CLI
npm run dev      # dev window
npm run build    # → .app / .dmg in app/src-tauri/target/release/bundle/
```

## 🧠 How it works

Read‑only data sources:

- **Codex** — `~/.codex/sessions/YYYY/MM/DD/rollout-*-<uuid>.jsonl`
- **Claude** — `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`
- **Kiro** — `~/.kiro/sessions/cli/<uuid>.json` (a metadata sidecar with cwd,
  timestamps, and title). Kiro records no cumulative token counts, so the usage
  column shows its **context‑window occupancy** (e.g. `15%`) instead.

MindPlayer keeps its own tiny sidecar state (archived ids, labels) at
`~/.mindplayer/state.json` and per‑session stderr logs at
`~/.mindplayer/logs/`. Resuming launches `codex resume <id>` /
`claude --resume <id>` / `kiro-cli chat --resume-id <id>` in an embedded PTY in
the session's original directory.

<details>
<summary>Performance & gotchas</summary>

- **Performance.** Real stores can be multi‑GB with files up to hundreds of MB.
  MindPlayer never reads a whole file just to list it — Codex cwd/tokens come from
  the first line plus a bounded tail read, Claude is filtered by directory name,
  and parsing is parallelized. Warm scans are ~1–2 s.
- **codex + PTY.** codex aborts on startup if its stderr shares the TUI's PTY, so
  children are spawned as `sh -c 'exec <prog> <args> 2>>~/.mindplayer/logs/<id>.stderr.log'`.

</details>

## 🗂️ Layout

```
mindplayer/
├─ crates/
│  ├─ mindplayer-core/   # discovery, token aggregation, archive state, resume
│  └─ mindplayer-tui/    # ratatui + portable-pty + vt100 → binary `mindplayer`
└─ app/                  # Tauri macOS app (src-tauri = Rust backend, src = frontend)
```

## 🛠️ Develop

```bash
cargo test --all
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo clippy --manifest-path app/src-tauri/Cargo.toml -- -D warnings   # Tauri backend
```

Contributions welcome — open an issue or PR.

## 📄 License

[MIT](LICENSE) © MindPlayer authors

The macOS app vendors [xterm.js](https://github.com/xtermjs/xterm.js) (MIT) under
`app/src/vendor/` — see [its notice](app/src/vendor/NOTICE.md).

<div align="center"><sub>Built with 🦀 Rust · ratatui · Tauri · xterm.js</sub></div>
