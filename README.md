<div align="center">

<img src="assets/mascot.png" alt="MindPlayer mascot" width="140" />

# MindPlayer

**Run many Codex &amp; Claude Code sessions like tabs — in one terminal window.**

Browse your whole session history, watch token usage, and juggle several live
sessions at once: start a task in one, switch out, drive another, switch back —
nothing stops in the background.

![License](https://img.shields.io/badge/license-MIT-7EA2F7)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust&logoColor=white)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey)
![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen)

</div>

---

## 🤔 Why

Codex and Claude Code pile up **hundreds** of sessions across your projects — but
the CLIs give you no way to see them all, compare token usage, or run more than
one at a time. MindPlayer is a thin, fast layer on top: a history‑aware launcher
plus an embedded terminal, so sessions feel like **browser tabs**.

It reads your existing `~/.codex`, `~/.claude`, `~/.kiro`, and `~/.cursor`
session metadata **read‑only** — it never modifies it — so it just works with
the sessions you already have.

## ✨ Features

- 📜 **Full session history** — scans `~/.codex/sessions`, `~/.claude/projects`,
  `~/.kiro/sessions/cli`, and `~/.cursor/chats`, with titles where each CLI's
  verified local schema exposes them.
- ⚡ **Active working set** — keep recently used, in-progress, and running
  sessions loaded. Older history remains searchable in a lightweight Archive;
  restoring it resumes the same native session, not a handoff or a new chat.
- 🤖 **Codex · Claude · Kiro · Cursor** — browse, resume, and start sessions for
  all four from one list (press <kbd>n</kbd> to pick the agent).
- 🔢 **Usage dashboard** — per-session token totals for Codex/Claude, Kiro
  context occupancy, and account plan usage for Kiro and Cursor. Cursor session
  metadata still exposes no per-session token or context metric; its account
  quota is a separate first-party reading, not inferred from chat files.
- 👥 **Several logins per provider** — keep work and personal Codex or Claude accounts
  (or two Claude plans) side by side and pick which one a new session starts
  on, with <kbd>u</kbd>. The login already on this machine keeps working and is
  listed like any other; a second one gets a home of its own, so signing it in
  never disturbs the first. Nothing is copied between them.
- **Session list** — a fixed provider column on the left leads directly into
  the task and handoff tree; account, status, age and ID sit to the right.
  Category gaps separate topics; connected handoff lanes share a rail and
  background. Narrow layouts prioritize the title over optional metadata.
  Mixed working directories retain a bounded folder hint when space permits.
- **Direct startup** — after choosing a working directory, collection uses the
  list surface and opens the list immediately, including an empty result.
  Failed collection stays an error with Enter to retry, not a successful empty
  list or an intermediate `Collected` screen.
- 🪟 **Many sessions at once** — resume or start several; each keeps running in
  the background. **Mark several** in the list (<kbd>Space</kbd>) and launch them
  together as live panes with one <kbd>Enter</kbd>.
- 🚦 **Live status** — opened sessions show `! blocked` (approval needed),
  `● working`, `○ idle` (waiting), `✓ done` (turn completed), or `○ ended`
  (process exited). `not open` means the source is listed but has no unambiguous
  live pane owned by this MindPlayer instance; it does not claim the session
  is idle or stopped in other terminals. For Codex and Claude Code, lifecycle
  events/hooks drive status when available (see `--install-agent-hooks`
  below); otherwise the pane's own text is used. Kiro's patterns are editable
  in `~/.mindplayer/kiro-patterns.json` (see **How it works**).
  When this instance has no retained PTYs, one `no sessions open here`
  footer replaces repeated `not open` badges; mixed lists retain per-row status.
- ⏱️ **Time since last prompt** — a live session's time column shows how
  long it's been since *you* last typed something, not just "now" (which a
  running agent's own file mtime would otherwise always show) — so you can
  tell a session that's been quietly working solo for 20 minutes from one
  that's been sitting idle waiting on you for just as long.
- 🗂️ **Topic categories** — group sessions on one subject under a named
  category with <kbd>t</kbd>: start with a single session, add more as the work
  splits, or select several (<kbd>v</kbd> + <kbd>Space</kbd>) and categorize
  them all at once. The list groups each handoff thread under one topic.
  `├─`/`└─` connect only adjacent lanes whose parent is visible in that group;
  missing, filtered, or off-screen parents never attach to an unrelated row.
  The lane count describes those visible connections. <kbd>→</kbd>/<kbd>←</kbd>
  walk into and out of a topic. The `recent` / `older` split is unchanged —
  a topic moves as one, so it never straddles the divider.
- ⇄ **Context sync across a topic** — sessions in a category keep each other
  current: entering one pulls in **only what its peers did since the last sync**,
  read straight from their transcripts (nothing to maintain by hand). A quiet
  peer means nothing is injected at all, and a busy session is never interrupted
  mid‑turn. Per-category toggle in the header menu (<kbd>t</kbd>); `⇄ auto`
  appears on enabled categories, while the repeated off label is omitted.
- 🟣 **In-progress mark** — flag a session with <kbd>i</kbd> as work you're not
  done with yet, independent of its live status; a thin rail keeps it visible
  even once it's buried in `older`.
- 💬 **Catch-up prompt** — pick up a session you left mid-task with
  <kbd>c</kbd>: it asks that session's own agent to introduce the project and
  summarize what you last asked and what it did. The built-in prompt no longer
  requests an HTML backlog; existing user-edited prompts are not overwritten.
- **Manual session usage** — press <kbd>r</kbd> on one session in the list to
  measure its token usage in the background. Unmeasured rows omit token text;
  the footer explicitly reports when nothing has been measured, never zero.
  Results are manual snapshots held for this run, not live counters; restarting
  clears them. Totals describe measured sessions only, and bounded or estimated
  results are distinguished from complete measurements. Narrow rows may omit
  the usage annotation to preserve the task title. Session activity, discovery
  and account quota updates remain automatic.
- 🔍 **Search** — <kbd>/</kbd> filters the visible list as you type.
- 📊 **Usage stats** — <kbd>u</kbd> shows a popup with active time (today/all
  time, plus a 14‑day trend), sessions opened per agent, handoffs, and
  catch‑ups sent — from a small local append-only log
  (`~/.mindplayer/audit.jsonl`), not from reading any transcript content.
- 🚥 **Subscription limits** — account-level plan usage stays separate from
  session tokens and context occupancy: Claude uses its authenticated usage API,
  Codex reads the latest local rate-limit snapshot, Kiro runs its own bounded
  `/usage` command, and Cursor calls its first-party `/api/usage-summary`. A
  quota with no number is shown as the **reason** it's missing, never as `0%` —
  which would read as "plenty left".
  Each account starts a separate footer row. Only windows belonging to the
  same provider and account share a line when they fit.
  One Vercel summary identifies API-key use, known budget blocking and the
  attributed user's measured spend/limit. Key and user uncertainty remain
  independent; unknown usage is never presented as available budget.
- 📚 **Conversation log** — Active Codex, Claude, and Kiro user/assistant turns are
  mirrored from the agents' transcripts into a normalized store at
  `~/.mindplayer/convo/`. Cursor and OMP transcripts are not mirrored.
  The store grows incrementally by byte watermark, so a re-scan costs nothing,
  and preserves turns even when the original logs rotate or are cleared. A
  directory lock keeps two MindPlayer instances from duplicating each other's
  work.
- ⛶ **Live panes** — drive one session full‑screen or split the view across
  every session you launch together (mark a whole topic and they all land in
  one grid). <kbd>Tab</kbd> (or <kbd>Ctrl‑w</kbd>) cycles focus,
  <kbd>Ctrl‑z</kbd> zooms the focused pane back to full size (toggle again for
  the split), <kbd>Ctrl‑q</kbd> closes the focused pane, and <kbd>Ctrl‑x</kbd>
  pops back to the list.
- 🗓️ **Recent first** — the list opens with everything touched in the **last
  24 hours** (a rolling window, not a calendar day — no midnight cliff) grouped
  at the top under a `recent` / `older` divider, so what you're working on now
  is always in reach.
- ⚡ **Live & snappy** — the list auto‑reorders by recent activity and refreshes
  in the background; input feels native.
- 🏷️ **Labels** — tag any session with a subject (a new one at creation, or an
  existing one with <kbd>e</kbd>); it shows as `🏷 your label`.
- 🗂️ **Scope & archive** — view the current working dir or everything; archive
  finished sessions (originals untouched); hide sub‑agent/`/team` workers.
- 🌏 **Friendly input** — soft newlines on
  `Shift`/`Alt`/`Ctrl`+`Enter`, full Korean/CJK (IME) support, and shortcuts
  that work on a Korean keyboard layout too.
- 🦆 **A walking buddy** — a small pixel character paces along the bottom of
  the session list (and the startup screens). Ships as a **rubber duck**;
  press <kbd>c</kbd> on the first screen to swap in a bunny, chick, slime,
  penguin, or octopus. Your pick is remembered.

## 📦 Install

> **TL;DR**:
> ```bash
> curl -fsSLO https://raw.githubusercontent.com/longlg88/mindplayer/main/install.sh
> less install.sh
> bash install.sh
> ```

**Prerequisites**

| For | You need |
|-----|----------|
| MindPlayer itself | nothing — the installer downloads a prebuilt binary (`curl`/`wget`). Only `--build` needs **Rust**. |
| Driving sessions | the agent CLIs you use, on `PATH`: [`codex`](https://github.com/openai/codex), [`claude`](https://docs.anthropic.com/claude-code), [`kiro-cli`](https://kiro.dev/docs/cli/), and Cursor `agent` — browsing works without them; a CLI is only needed to *resume/start* that agent |

**Keeping the CLIs current.** Before a Codex, Claude or OMP session starts
(new, resumed or handed off), MindPlayer runs that CLI's own `update` command,
unless it already ran within the last 10 minutes. The session waits, showing
`updating <cli>…`, for at most 90 seconds. The 10 minutes are shared by every
MindPlayer instance on the machine. The check is fail-open: if the update fails
or times out, the session starts on the installed version and the status line
says so. Sign-in panes and other programs are never held. Set
`MINDPLAYER_CLI_UPDATE=0` (or `off`, `false`) to turn this off.

**2.0.0부터 설치된 CLI를 직접 실행합니다.** Codex·Claude의 새 세션, 재개,
handoff와 로그인은 MindPlayer 프로세스가 받은 `PATH`의 `codex`·`claude`를
각 실행 시점에 찾습니다. 별도 버전으로 고정하지 않습니다. 세션을 시작하기 전에
MindPlayer가 해당 CLI 자신의 `update` 명령을 실행합니다.

- 평소 사용하던 설치 방법으로 CLI를 업데이트하면 다음 실행부터 반영됩니다.
  같은 PATH 항목의 실행 파일 교체나 심볼릭 링크 변경에는 MindPlayer 재시작이
  필요 없습니다. PATH 환경변수 자체를 바꿨다면 MindPlayer를 다시 실행해야 합니다.
- MindPlayer가 CLI를 직접 다운로드하지는 않습니다. 업데이트는 CLI 자신의 `update`
  명령이 하며, 실패하면 **현재 설치된 CLI**로 세션을 시작합니다. 최신 버전 설치까지는
  보장하지 않습니다.
- 실행 중인 pane의 프로세스는 교체하거나 종료하지 않습니다. 새 CLI를 사용하려면
  해당 세션을 새로 실행하거나 재개하세요.
- Codex 전용 고정 빌드, 내장 Node 중계, F6 Jev 토글·라우팅 상태 표시는 제거했습니다.
  Codex 모델·effort는 원래 CLI에서 설정합니다. 기존 전용 설치 파일이나 자격 증명은
  삭제하지 않습니다.
- 계정별 실행 환경, 작업 폴더, 재개 인수, handoff 입력, Claude fullscreen 설정과
  OMP 실행 경로는 유지합니다. Codex·Claude pane 아래쪽에는 검증된 실행 계정
  레이블을 표시하며, OMP는 아래의 실제 응답 계정 관찰을 사용합니다.
- Codex 안에서 `/clear`, `/resume`, `/fork`로 전환한 세션을 MindPlayer 목록의
  ID·레이블에 자동 반영하던 전용 연동도 제거했습니다. CLI 내부 명령은 사용할 수
  있지만 목록은 실행 당시 연결을 유지하므로, 전환한 세션을 목록에서 조작하려면
  다시 탐색한 해당 세션을 선택하세요.

`tools/local-model-router`와 `tools/native-codex`는 별도 실험 도구이며 현재 앱에
포함되거나 세션 실행에 호출되지 않습니다. 과거 Jev 검증 문서는 이전 통합 버전의
기록이며 2.0.0의 동작을 설명하지 않습니다.

The installer downloads the latest **release binary** (no build).
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

On that first screen, <kbd>c</kbd> opens the character picker — the rubber
duck walking on the floor can become a bunny, chick, slime, penguin, or
octopus, and the choice sticks for next time.

Press <kbd>n</kbd> for a new Codex / Claude / Kiro / Cursor / OMP session.

Claude new, resumed, and handoff sessions use its native
[fullscreen renderer](https://code.claude.com/docs/en/fullscreen) through
`--settings '{"tui":"fullscreen"}'`. Claude then lays out old conversation
content at the current pane width when you zoom and scroll, rather than leaving
classic-mode hard-wrapped terminal history at its original narrow width.
This launch-only setting does not edit global Claude settings or change native
authentication or managed policy. The renderer is a Claude research preview;
native opt-outs such as `CLAUDE_CODE_NO_FLICKER=0` and
`CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` still apply. Already-running panes keep
their renderer: restart MindPlayer at a safe stopping point, then resume the
conversation to use the updated launch setting.

The OMP choice runs the installed `omp` executable in the selected working
directory. A new OMP session asks you to select a conversation model and its
provider route from OMP's authenticated model catalog before sending a message.
Cancel leaves ordinary input blocked; use `/mindplayer-model` to choose again
or `/login` to add credentials. The catalog does not verify live entitlement
for each model, and provider labels do not identify individual accounts in an
OMP account pool. This selection does not set models used by auxiliary roles
or subagents; extension-initiated side turns are not covered by the input guard.
OMP keeps its own provider login, model configuration, tools, and session data;
MindPlayer does not copy
Codex credentials or route OMP through Jev. A new pane uses a per-run
`autoResume: false` overlay; it does not change OMP's saved settings. Existing
OMP sessions are discovered from the active OMP session store and resumed by
their session file without the new-session picker. Install OMP and make `omp`
available on `PATH` before choosing it. `mindplayer --help` lists the remaining
controls.

OMP account labels use the credential ID stamped by the installed OMP on its
live main-agent response, matched to OMP's token-free OAuth account metadata.
The list and pane show the observed email/account and workspace when available;
API keys show a credential ID, not an inferred person. Account/model/session
changes revoke the old label until another response proves the identity.

MindPlayer keeps the account of each OMP session's latest observed response in
`~/.mindplayer/state.json`, as the same token-free label, under the native
session ID and transcript file that OMP reported. A row whose session is
stopped or not open shows that stored account, also after MindPlayer restarts.
A running pane shows only what its own responses prove, so the list says
`unverified` and the pane says `계정 미확인` between an account, model, or
session change and the next response. A session that has not answered in a
MindPlayer pane since 2.2.0, or whose installed OMP lacks the required
extension APIs, has no stored account and stays `unverified`. A provider name,
selected model, or first login in a pool is not account evidence. The observer
is per pane, uses private temporary files, and does not copy tokens, modify
login state, or pin the OMP executable. Native integration is verified with
OMP 18.22.2; installing a different version still uses that installed
executable.

In an OMP pane, MindPlayer reserves `Ctrl-X` (list), `Ctrl-Z` (zoom),
`Tab` or `Alt-W` (next pane), `Shift-Tab` (previous pane), and `Ctrl-Q` or
`Alt-Q` (close pane). As with other providers, Tab and Shift-Tab move panes
only while two or more are open, including zoomed views; with one pane,
OMP receives both keys. Closing a live pane hides it without terminating
the session. OMP retains its native `Ctrl-P`, `Ctrl-T`, `Ctrl-G`, `Ctrl-W`
and `Ctrl-Y` shortcuts.

To keep an effort shortcut available while navigating multiple panes, remap
OMP's native thinking-effort action in `~/.omp/agent/keybindings.yml`:

```yaml
app.thinking.cycle: F7
```

F7 avoids macOS Option-key text composition and letter-key changes under
Korean input. On a Mac with media keys enabled, press `Fn` + `F7`.

Restart OMP sessions after changing the binding. This is OMP user
configuration, not key translation or a setting that MindPlayer installs
automatically. The default-profile binding also affects standalone OMP.
If a profile uses a separate configuration directory, configure its binding
there too.

Clipboard shortcuts retain their modifiers in OMP: `Cmd-V` on macOS or
`Ctrl-V` pastes through OMP, and `Alt-Shift-C` / `Alt-Shift-L` copies the
whole prompt / current line. Terminal-managed pastes still use bracketed
paste for multiline text. Drag across a pane to copy its displayed text;
`Ctrl-C` remains OMP's native clear/interrupt shortcut, not a copy command.

Handoff can also target OMP: select OMP in the handoff picker, then choose its
conversation model and provider. The handoff context stays queued until the
choice succeeds; cancelling the picker leaves it queued while you retry with
`/mindplayer-model`.

MindPlayer's Trace and handoff views do not reconstruct OMP's branched
transcript; inspect the conversation inside OMP instead.

Codex and Claude launches (new, resume, and handoff) verify the selected home's
provider-reported user and workspace/organization before execution, using one
shared recovery lifecycle. This is separate from quota
availability. A matching saved binding continues without another browser login;
missing or changed identity opens Accounts for inspection. The account name is
only a label, not proof of identity: check the displayed email and, for Claude, the
plan before pressing `v` to bind. Confirmation performs another fresh read.

Use `l` to sign in to that exact provider home, including the shared default home.
MindPlayer does not run logout first or copy another home's credentials. After
successful login it verifies again and continues the waiting launch, preserving
its label and handoff input. Cancelled/failed login never starts the conversation.
Signing into the default home can affect other applications using it. Account
labels are arbitrary; there is no built-in company/account allowlist. Existing
Codex bindings are retained; Claude bindings are stored separately.

Claude's native `auth status --json` reports configured authentication, not proof
that a token currently works. Launch verification therefore requires a successful
server profile response in addition to checking the native provider/configuration
context. The profile request follows Claude Code 2.1.302's implementation and is
an internal compatibility boundary, not a guaranteed public API. An unavailable
or changed response fails closed with a recoverable error; it does not silently
trust cached identity, substitute another account, or spend a model request to
test authentication. Project settings and managed login policies remain in force.

**Optional — confirmed live status for Codex & Claude.** Both expose official
lifecycle hooks; registering a small bundled script lets MindPlayer read a
session's real state (blocked on approval / working / idle) instead of
guessing from on‑screen text. Dry‑run first, then apply if it looks right —
it's purely additive and never touches your existing hooks:

```bash
mindplayer --install-agent-hooks           # show the plan, changes nothing
mindplayer --install-agent-hooks --apply   # write ~/.claude/settings.json and ~/.codex/hooks.json
```

Kiro has no equivalent hook (checked against its own docs), so it keeps using
a screen‑text fallback — see `~/.mindplayer/kiro-patterns.json` under **How it
works** if you ever need to teach it new wording.

**Develop MindPlayer itself** (from a clone): `make` shows all targets.

```bash
cargo run -p mindplayer-tui -- ~/code/my-project   # run against your project
make test                                          # cargo test --all
```

### ⌨️ Keys

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> <kbd>↓</kbd> | move selection (`▶`) |
| <kbd>→</kbd> / <kbd>←</kbd> | move around the tree: <kbd>→</kbd> unfolds a **category** then steps inside it, <kbd>←</kbd> folds it and steps back out to the header. Neither opens a session — that's <kbd>Enter</kbd> |
| <kbd>Enter</kbd> | open the selected session (on a **category** header, folds/unfolds it instead), **adding it to the live view** (resume, or focus if already shown); remove a pane with <kbd>Ctrl‑q</kbd>. In multi-select mode, launch **all marked** at once |
| <kbd>v</kbd> | toggle **multi‑select** mode — then <kbd>Space</kbd> marks sessions and <kbd>Enter</kbd> launches them all (<kbd>Esc</kbd> cancels) |
| <kbd>Space</kbd> | mark / unmark the selected session (`✓`) — multi‑select mode only |
| <kbd>Tab</kbd> / <kbd>Shift‑Tab</kbd> | next / previous live pane when 2+ are open, including OMP and zoomed views |
| <kbd>Ctrl‑w</kbd> / <kbd>Alt‑w</kbd> | cycle live-pane focus; use <kbd>Alt‑w</kbd> in OMP |
| <kbd>Ctrl‑z</kbd> | zoom the focused pane to full size (toggle again for the split view) |
| <kbd>Ctrl‑q</kbd> / <kbd>Alt‑q</kbd> | close the focused live pane; <kbd>Alt‑q</kbd> also works in OMP |
| <kbd>Ctrl‑p</kbd> / <kbd>Ctrl‑t</kbd> | native session input; MindPlayer no longer intercepts these for HTML features |
| <kbd>Ctrl‑x</kbd> | back to the list (the session keeps running) |
| <kbd>n</kbd> | new session — pick codex/claude/kiro/cursor/omp, then an optional label |
| <kbd>h</kbd> | handoff the selected session — pick a target login or OMP, then choose an OMP model when applicable |
| <kbd>d</kbd> | change the working directory (blank = global) and rescan in place |
| <kbd>e</kbd> | label the selected session (tag an existing one, or edit/clear its label) |
| <kbd>i</kbd> | toggle the **in-progress** mark on the selected session (see above) |
| <kbd>c</kbd> | send a **catch-up prompt** to the selected session (confirms first if it's busy) |
| <kbd>t</kbd> | on a **session**: put it in a **topic category** — pick an existing one, create a new one, or remove it. In multi‑select, categorizes **all marked** at once |
| <kbd>t</kbd> | on a **category header**: the category menu — auto‑sync on/off, sync now, rename, remove |
| <kbd>/</kbd> | search / filter the visible sessions |
| <kbd>x</kbd> | close (archive) & stop the selected session |
| <kbd>a</kbd> | toggle Active / Archive · <kbd>[</kbd>/<kbd>]</kbd> page Archive · <kbd>Enter</kbd> restore/resume |
| <kbd>r</kbd> | manually measure usage for the selected session; no batch refresh or session launch |
| <kbd>u</kbd> | **accounts** — which login each provider starts a session on, with its usage. See below |
| <kbd>?</kbd> | show the full keyboard-shortcut list |
| <kbd>q</kbd> | quit (stops all sessions) |

Inside a live session, <kbd>Enter</kbd> submits and
<kbd>Shift</kbd>/<kbd>Alt</kbd>/<kbd>Ctrl</kbd>+<kbd>Enter</kbd> inserts a
newline, and Korean/CJK input works with the cursor tracking the prompt. The
**mouse wheel goes to mouse-aware CLIs**, including Claude's fullscreen
renderer; otherwise it scrolls MindPlayer's own terminal history. **Drag inside
a pane to select &amp; copy just that pane's text** to the system clipboard
(via OSC 52), so a side-by-side split never copies the neighboring pane.

MindPlayer no longer scans for HTML files, shows HTML notifications, opens local
HTML previews, or offers the HTML transition-report generator. Existing reports,
HTML files, and custom prompt files are left untouched. Ordinary link copying,
catch-up and session handoff remain available.

### 👥 Accounts

<kbd>u</kbd> opens the one screen that answers *which login does my next
session run on*:

```
┌ Accounts ──────────────────────────────────────────────────────────┐
│ CODEX                                                              │
│    work          in use      codex     ▰▰▰▰▰▱   83.0%  resets Mon  │
│  ▶ personal      reserve     codex     ▰▱▱▱▱▱   12.0%  resets Mon  │
│ CLAUDE                                                             │
│    default       in use      claude 5h ▰▰▱▱▱▱   41.0%  resets 14:20│
│                              claude wk ▰▰▰▰▰▰   96.0%  resets Thu  │
└────────────────────────────────────────────────────────────────────┘
```

| Key | Action |
| --- | --- |
| <kbd>w</kbd> | **use this one** — new sessions of that provider start here; the rest of its accounts drop to reserve |
| <kbd>Enter</kbd> | start a session on the highlighted account **now**, without changing which one the next session takes |
| <kbd>a</kbd> | add an account — name it, then its provider's own sign‑in runs in a pane, inside the new account's home |
| <kbd>v</kbd> | inspect / explicitly bind Codex or Claude identity; confirmation rechecks the selected home |
| <kbd>r</kbd> | rename the display label (the login home stays put) · <kbd>l</kbd> sign in again · <kbd>d</kbd> off · <kbd>x</kbd> erase login (twice) |

**Which organization a Claude login lands on.** One Claude login belongs to one
organization, for example a Team organization or a personal Max plan. The browser
signs in to whichever organization it currently shows, so a login can silently
land on the wrong one. Each Claude row therefore shows its plan — `team`, `max`,
`pro`, `enterprise` or `unknown plan` — and no MindPlayer message shows an
organization UUID.

- A bound account remembers its organization and plan. <kbd>l</kbd> then runs
  `claude --settings '{"forceLoginOrgUUID":"…"}' auth login`, so the sign-in page
  opens on that organization, and the status line names the plan to sign in with.
  An account with no stored organization signs in as before.
- When a login ends, and on <kbd>v</kbd> and on a launch check, MindPlayer compares
  the login it got with the stored ones. **Same login:** it is confirmed with its
  email and plan, and a changed plan is refreshed silently. **Same subscription as
  another account:** nothing is bound. An account added with <kbd>a</kbd> in this run is cancelled
  — it is removed from the list and its home directory is erased, exactly as <kbd>x</kbd>
  does, with no `claude auth logout` — and any other account keeps its binding;
  <kbd>v</kbd> still binds it if you want it. **Changed:** the message names the plan that
  was expected and the one that was reached; <kbd>v</kbd> binds the new login,
  <kbd>l</kbd> signs in again.
- A newly added account whose login is new is registered at once, with no <kbd>v</kbd>.
- The usage probe also asks Claude's profile endpoint, with the same token and on a
  best-effort basis, who the usage belongs to. If that request fails, the usage
  reading is unaffected and the rows simply carry no login. An account whose usage
  login equals an earlier account's reads `same subscription as <name>` here and
  `= <name>` in the footer, where the plan also sits next to the account name once
  a provider has two or more accounts. An account whose usage login is no longer
  the bound one reads `login changed · press v`.

For MindPlayer-managed accounts, exactly one account per provider is *in use*,
so the launch choice never depends on list order. Usage for different accounts
occupies separate footer rows. OMP manages its own native credential pool instead
of using these account slots.

**Sessions stay with the login that made them.** Codex and Claude resumes use
the source account's home; a successful verified launch binds the pane to that
account label. The list shows the account even when the provider has only one
login. OMP instead displays its observed native response account as described
above. To carry a conversation to another login, hand it over with <kbd>h</kbd>
— that starts a new session there with the previous context.

**Two caveats.** Cursor keeps its usage credential in the macOS Keychain, one
per machine, and that one belongs to the login this machine came with — so a
second Cursor account runs its sessions normally but shows no usage figure
rather than borrowing the first one's. A second Claude account keeps its login
in a Keychain entry of its own, keyed to that account's home, and its usage is
read from there. And nothing is ever copied between accounts: a duplicated
login refreshes its token and locks the original holder out, which is a failure
neither side can explain.

## 🧠 How it works

### Active and Archive

Active membership uses recorded human use (opening/resuming a session or sending
input) and available native human-prompt timestamps, not transcript modification
time. The default inactivity window is seven days. Running sessions—including
hidden panes—and sessions marked in progress stay Active. Explicitly closing a
session with `x` still stops and archives it; automatic expiry never kills work.

Press `a` for Archive, `/` to search its metadata, and `PageUp` / `PageDown` to
page through up to 200 entries at a time, including filtered search results.
Outside search, `[` / `]` also change pages; inside search they remain literal
characters. Automatic catalog refresh preserves the selected source even if
new rows move it to another page. `Enter` loads only the selected source, saves its
Active membership, and resumes its original native ID and account/path. Labels,
thread links, and categories stay attached. A missing or changed source reports
an error without creating a replacement session or discarding the search query.
Archive token usage is shown
as unknown until loaded, not measured zero. `Ctrl-q` only hides a live pane; the
running count includes it, and it can be reopened from Active.

New and handoff sessions have one temporary row until their native transcript
appears. Refresh preserves that owner; discovery replaces the temporary row and
its selection together. Closing with `x` before startup cancels that session's
queued launch and pending authentication, without canceling neighboring sessions.
Pending labels and handoff links stay with their in-process owner rather than
being assigned to another recently created session in the same directory.
Closing an ended, unwritten session removes its temporary row; native transcript
files are never deleted by this action.

Late files from children that actually ran still use the existing bounded
account/directory/start-time matching. This does not establish exact ownership
between simultaneous external sessions in the same account and directory.

The rebuildable `session-catalog.json` cache lives beside `state.json`. Catalog
indexing, Archive search, and restoration run on workers. Unchanged archived
files are not reparsed; periodic usage refresh and conversation-log ingestion
only visit Active sources. Initial metadata discovery samples bounded head/tail
windows, so a native human turn never observed outside those windows may be
unknown. MindPlayer's own recorded use persists independently, bound to the
provider and exact source path rather than the native ID alone. Ambiguous
legacy ID-only use never chooses an account/source automatically. The existing
24-hour `recent` / `older` divider is only a display grouping within Active.

Session titles use genuine task text rather than Claude local-command notices,
tool feedback, or injected runtime context; explicit labels still win. Recognized
Slack operational introductions retain the action and actual request. Version
1.0.1 rebuilds older metadata catalogs once using bounded reads so cached titles
also refresh, without rewriting transcripts, labels, or handoff relationships.

Explicit close/restore decisions are source-qualified too: restoring one
account's transcript does not reopen a closed same-ID transcript from another
source. Unreadable files and incomplete directory scans defer legacy use
migration until a complete census; failed migration saves are reported and
retried. These new source-specific decisions require a current reader; an
older installed version only understands the legacy ID-wide fields.

Native Codex `/resume` retains the original PTY owner while tracking the new
thread/source. Merely delivering input is not proof that a model turn started.
The terminal preserves explicit Codex Ctrl+Enter instead of converting it to
plain Enter. Rapid unbracketed text plus CR still follows the native paste guard;
MindPlayer does not replay Enter or disable that safeguard.

Automatic peer context is bound to its original source through preparation and
deferred delivery. A native source switch cancels stale context; synchronization
receipts advance only after successful PTY enqueue, not when preparation starts
or context is merely queued. If cancelled context had held user keystrokes, the
status reports their byte count: they remain in memory until the pane closes,
are never replayed (even after switching back), and must be explicitly retyped
if needed. Closing the pane discards those retained bytes.

To change automatic retention, set `active_retention_days` in `state.json` while
MindPlayer is closed, then restart. `0` disables automatic expiry, retaining the
explicit Archive list; it can therefore load a large history again. No setting
deletes or rewrites provider transcripts, and existing saved states migrate
without losing their labels or relationships.

### Sources and local state

Read‑only data sources:

- **Codex** — `~/.codex/sessions/YYYY/MM/DD/rollout-*-<uuid>.jsonl`
- **Claude** — `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`
- **Kiro** — `~/.kiro/sessions/cli/<uuid>.json` (a metadata sidecar with cwd,
  timestamps, and title). Kiro records no cumulative token counts, so the usage
  column shows its **context‑window occupancy** (e.g. `15%`) instead.
- **Cursor session metadata** —
  `~/.cursor/chats/<workspace-hash>/<chat-id>/meta.json`. Only
  `hasConversation=true` chats are listed; cwd, timestamps, optional title, and
  subagent state are available, but per-session token/context metrics and
  transcript content are not exposed by this verified local schema.

Account-level quota sources are separate from those session stores. Kiro uses
`kiro-cli chat --no-interactive /usage`. On macOS, Cursor reads the Cursor Agent
access token from the login Keychain item `cursor-access-token` / `cursor-user`,
validates its JWT subject and expiry, and sends only the resulting session cookie
to `https://cursor.com/api/usage-summary`. The cookie is staged in a private
mode-0600 curl config, never argv; HTTPS is required and redirects are disabled.
Cursor quota is unavailable rather than guessed when that macOS Agent credential
is absent or the response contains no usable plan/category/personal/team cap.
Included and capped on-demand usage are shown as separate rows so spending is not
masked by the included aggregate. Auto and API breakdowns are not added as footer
rows. Browser cookies, chat content, project content, and session metadata are not
used as quota.

The Vercel footer resolves the current workspace's gateway credential via
`omp token vercel-ai-gateway`, then uses gateway quota metadata and the
signed-in Vercel CLI's read access to identify the key and read its budgets.
Identification requires a unique six-character key suffix, a determinable team,
complete key-list pagination, and CLI read permission. Suffix matching is not
full-key identity verification. The key is never placed in command arguments
or logs. Ctrl-R refreshes the summary.
Queries use bounded, paginated raw key listings and authenticated, read-only
`vercel api` requests for budgets, including default-covered meters.
An explicit key budget takes precedence; an absent or archived override can
inherit the team's active API-key default. Only a returned meter supplies usage:
an unmetered policy remains unknown, not fabricated zero or remaining balance.
A valid zero reading is displayed as zero. Failed or incomplete queries remain
unavailable. An absent key budget remains distinct from failed lookup; inactive,
nonarchived overrides are not silently replaced by defaults.
Rotating a Gateway key does not migrate its custom budget; it can inherit a
team default. Remove revoked entries from OMP's credential store so `omp token`
selects the current key. MindPlayer does not change credentials or server
budgets and does not use the paid reporting API.

The unified summary follows the selected key's raw `metadata.spendAttribution`
and `createdBy`, never the CLI login. A user-attributed key shows that creator's
custom or default budget and period. Measured spend at or above either
independent cap marks the summary `blocked`; a user over its cap remains blocked
even when the key has room. A blocked key retains its own spend/limit detail.
An explicit team attribution is reported as team-attributed; missing attribution
remains unknown rather than assuming that no user cap applies.

Ordinary default-key limit boilerplate is omitted when a measured user budget
already explains the state. If user spend is unavailable, a valid key meter
can still be shown, clearly labeled as key spend. An unmetered user limit keeps
its limit and unknown-spend warning; partial failures never promote the summary
to unblocked. `Not blocked by budgets` describes only the observed key/user
budget checks, not guaranteed access: team/project limits or other authorization
rules may still reject a request. Long amounts and reset details wrap rather
than lose digits. Other providers' quota windows share a row only when they fit.

Budget fields and inheritance follow Vercel's
[budget-list API](https://vercel.com/docs/rest-api/ai-gateway/list-ai-gateway-budgets)
and [default-budget rules](https://vercel.com/docs/ai-gateway/observability-and-spend/budgets).

The footer uses server-side caps only. Local display-target overrides are no
longer read; existing `~/.mindplayer/vercel-key-target.json` files are left
untouched and cannot make the server-budget lookup fail.

What MindPlayer writes, all under `~/.mindplayer/`:

| Path | Contents |
| --- | --- |
| `state.json` | sidecar state — archived ids, human-use timestamps, retention, labels, in-progress marks, topic categories, your walking-buddy pick |
| `session-catalog.json` | rebuildable, private metadata cache; original source paths and fingerprints, no transcript bodies |
| `audit.jsonl` | append-only usage log: counts and timestamps only, no session id, title, or cwd |
| `accounts.json` | the accounts per provider — names, which one is in use, nothing secret |
| `accounts/<provider>/<name>/` | a second login's own home. The provider's CLI writes its credentials here; MindPlayer only points the CLI at it |
| `convo/` | the conversation log — `index.json` plus one append-only `<session-id>.jsonl` per session |
| `prompts/` | editable canned prompts (see below) |
| `hooks/` | per-pane status readings written by the agents' own lifecycle hooks |
| `logs/` | per-session stderr |

The source data under `~/.codex`, `~/.claude`, `~/.kiro`, and `~/.cursor` is
only ever read. A second account's home under `~/.mindplayer/accounts/` is
created empty and then written by that provider's own CLI when you sign in
there; MindPlayer stores no credential of its own, and never reads one
account's to answer for another.

**Live status, in more detail.** Codex and Claude Code status comes from
their own official lifecycle hooks (`UserPromptSubmit`, `PreToolUse`,
`PostToolUse`, `PermissionRequest`, `Stop`) once installed via
`--install-agent-hooks --apply` — each event writes a small JSON reading to
`~/.mindplayer/hooks/<pane-id>.json`, which MindPlayer polls every 500ms; a
stale `working` reading (nothing written for 5+ minutes) falls back to the
screen‑text heuristic rather than reporting a status that's just an
artifact of a crashed hook. Kiro-cli has no comparable hook, so it always
uses the screen‑text heuristic, driven by `~/.mindplayer/kiro-patterns.json`
(created with sensible defaults on first use) — edit it to add wording from a
newer kiro-cli release without waiting on a MindPlayer update. Cursor currently
uses the same generic PTY screen-state fallback (blocked/working/idle markers)
without a Cursor-specific lifecycle hook.

The catch-up prompt (<kbd>c</kbd>) lives at
`~/.mindplayer/prompts/catchup.md`, seeded with the built-in default on first
use. Edit it in any text editor; the next send picks up the change immediately.
Existing custom prompts, including old `transition_report.md` files, are left
untouched; MindPlayer no longer invokes the transition-report prompt.

Resuming launches `codex resume <id>` / `claude --resume <id>` /
`kiro-cli chat --resume-id <id>` / `agent --resume <id>` in an embedded PTY in
the session's original directory.

<details>
<summary>Performance & gotchas</summary>

- **Performance.** Catalog and activity refreshes read metadata without token
  aggregation. Explicit list <kbd>r</kbd> reads only the selected session's
  usage source using provider-specific bounds; incomplete totals are marked
  partial rather than presented as full-session totals.
- **codex + PTY.** codex aborts on startup if its stderr shares the TUI's PTY, so
  children are spawned as `sh -c 'exec <prog> <args> 2>>~/.mindplayer/logs/<id>.stderr.log'`.

</details>

## 🗂️ Layout

```
mindplayer/
└─ crates/
   ├─ mindplayer-core/   # discovery, token aggregation, archive state, resume
   └─ mindplayer-tui/    # ratatui + portable-pty + vt100 → binary `mindplayer`
```

## 🛠️ Develop

```bash
cargo test --all
node --test crates/mindplayer-tui/tests/omp_account.test.mjs
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

Dev builds and tests use reduced debug information and disable incremental
compilation to save disk space. Backtraces remain available, but variable and
type debug symbols are reduced, and rebuilds may be slower. Override these
settings with Cargo environment variables, for example
`CARGO_PROFILE_DEV_DEBUG=2` and `CARGO_PROFILE_DEV_INCREMENTAL=true`. Release
builds are unaffected.

Contributions welcome — open an issue or PR.

## 📄 License

[MIT](LICENSE) © MindPlayer authors

<div align="center"><sub>Built with 🦀 Rust · ratatui</sub></div>
