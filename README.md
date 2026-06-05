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
  the background.
- 🚦 **Live status** — each row shows `● working` (producing output now),
  `● idle` (running, waiting), or `○ done` (ended) at a glance.
- ⛶ **Full‑screen switch UX** — a clean list; press <kbd>Enter</kbd> to take a
  session full‑screen, <kbd>Ctrl‑x</kbd> to pop back. No cramped split panes.
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

**Prerequisites**

- **Rust** (stable) — the only build requirement. Install from
  [rustup.rs](https://rustup.rs): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- *(optional, to actually drive sessions)* the agent CLIs you use, on your
  `PATH`: [`codex`](https://github.com/openai/codex),
  [`claude`](https://docs.anthropic.com/claude-code), and/or
  [`kiro-cli`](https://kiro.dev/docs/cli/). MindPlayer browses history for any
  you have, and only needs a given CLI to *resume/start* that agent's sessions.

**One‑line install (TUI)**

```bash
git clone https://github.com/longlg88/mindplayer.git && cd mindplayer && ./install.sh
```

`install.sh` builds the optimized binary and copies `mindplayer` to
`~/.local/bin`. If that dir isn't on your `PATH`, the script prints the exact
line to add to your shell rc.

```bash
./install.sh                    # → ~/.local/bin/mindplayer
PREFIX=/usr/local ./install.sh  # → /usr/local/bin (may need sudo)
./install.sh --bin-dir ~/bin    # → a directory you choose
./install.sh --uninstall        # remove it again
```

**Update** — pull and re‑run the installer:

```bash
git pull && ./install.sh
```

**Using it.** The first screen asks for **working dir** (one project) or
**global** (every session). The working dir is whichever directory you point
MindPlayer at:

```bash
cd ~/code/my-project && mindplayer   # manage THIS project's sessions
mindplayer ~/code/my-project         # …or pass the dir from anywhere
mindplayer                           # current directory
```

Press <kbd>n</kbd> to start a new Codex / Claude / Kiro session (it launches in
that directory). `mindplayer --help` lists everything.

**Run from source (developing MindPlayer itself)**

```bash
cargo run -p mindplayer-tui -- ~/code/my-project   # pass your project dir
cargo build --release && ./target/release/mindplayer ~/code/my-project
```

> ⚠️ `cargo run` without a directory uses the **MindPlayer repo** as the working
> dir (you'd be browsing MindPlayer's own sessions, not your project's). Either
> pass the dir as shown above, or install and run `mindplayer` from your project.

### ⌨️ Keys

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> <kbd>↓</kbd> / <kbd>j</kbd> <kbd>k</kbd> | move selection (`▶`) |
| <kbd>Enter</kbd> | open the selected session full‑screen (resume, or switch if already running) |
| <kbd>Ctrl‑x</kbd> | back to the list (the session keeps running) |
| <kbd>n</kbd> | new session — pick codex/claude/kiro, then an optional label |
| <kbd>e</kbd> | label the selected session (tag an existing one, or edit/clear its label) |
| <kbd>x</kbd> | close (archive) & stop the selected session |
| <kbd>a</kbd> | toggle archived view · <kbd>g</kbd> toggle sub‑agents · <kbd>r</kbd> rescan |
| <kbd>q</kbd> | quit (stops all sessions) |

Inside a live session, <kbd>Shift+Enter</kbd> inserts a newline (<kbd>Enter</kbd>
submits), and Korean/CJK input works with the cursor tracking the prompt. The
**mouse wheel scrolls MindPlayer's own scrollback** (so history that ran off the
top stays readable). Because the wheel needs mouse capture, use
**<kbd>Shift</kbd>+drag** to select &amp; copy text (your terminal's native
selection — works in Ghostty, iTerm2, Terminal.app, and most others).

## 🍎 macOS app

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
