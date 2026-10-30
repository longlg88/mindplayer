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
- 🪟 **Many sessions at once** — resume or start several; each keeps running in
  the background. **Mark several** in the list (<kbd>Space</kbd>) and launch them
  together as live panes with one <kbd>Enter</kbd>.
- 🚦 **Live status** — each row shows `○ done` (just finished — sorted
  above everything else, since there's nothing left for the agent to do and
  everything left is on you), `● blocked` (paused at an approval prompt —
  needs you), `● working` (producing output now), or `● idle` (running,
  waiting) at a glance. For Codex and Claude Code this comes straight from
  the agent's own lifecycle hooks once installed (see `--install-agent-hooks`
  below) — a confirmed state, not a screen‑text guess. Kiro and Cursor use
  the pane's own text instead; Kiro's patterns are editable in
  `~/.mindplayer/kiro-patterns.json` (see **How it works**).
- ⏱️ **Time since last prompt** — a live session's time column shows how
  long it's been since *you* last typed something, not just "now" (which a
  running agent's own file mtime would otherwise always show) — so you can
  tell a session that's been quietly working solo for 20 minutes from one
  that's been sitting idle waiting on you for just as long.
- 🗂️ **Topic categories** — group sessions on one subject under a named
  category with <kbd>t</kbd>: start with a single session, add more as the work
  splits, or select several (<kbd>v</kbd> + <kbd>Space</kbd>) and categorize
  them all at once. The list groups by topic, handoff lanes stay nested inside,
  and <kbd>→</kbd>/<kbd>←</kbd> walk into and out of a topic. The `recent` / `older` split
  is unchanged — a topic moves as one, so it never straddles the divider.
- ⇄ **Context sync across a topic** — sessions in a category keep each other
  current: entering one pulls in **only what its peers did since the last sync**,
  read straight from their transcripts (nothing to maintain by hand). A quiet
  peer means nothing is injected at all, and a busy session is never interrupted
  mid‑turn. Per‑category toggle in the header menu (<kbd>t</kbd>), shown as
  `⇄ auto` / `⇄ off` on every category row.
- 🟣 **In-progress mark** — flag a session with <kbd>i</kbd> as work you're not
  done with yet, independent of its live status; a thin rail keeps it visible
  even once it's buried in `older`.
- 💬 **Catch-up prompt** — pick up a session you left mid-task with
  <kbd>c</kbd>: it asks that session's own agent to introduce the project,
  open (or create) `backlog.html`, and summarize what you last asked and what
  it did — no need to remember, and mindplayer never reads the answer itself.
- 📝 **Transition report** — <kbd>Ctrl‑t</kbd> from a live pane opens a
  one-line prompt (`topic / RUNBOOK §n / files`), then shows the fully
  assembled prompt for review: <kbd>Enter</kbd> sends it as-is, or
  <kbd>e</kbd> edits it first (multi-line) before sending. Asks the focused pane's own agent to write
  `transition-<topic>.html` against your `_assets/transition-template.html`
  structure. Provider-agnostic (it's just text) and works whether one pane or
  several are open.
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
  The Vercel row reports the active AI Gateway key's budget and current-period
  spend when the key and its budget can be identified.
- 📚 **Conversation log** — Codex, Claude, and Kiro user/assistant turns are
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

로컬 수정본의 Jev는 **Codex 전용**입니다. 시작 화면에서 공통 모드를 고르지 않고,
Codex 입력줄의 `F6`는 **현재 실행 세션의 Jev ON/OFF 토글**입니다. 처음에는 OFF이고,
한 번 켜면 수정·붙여넣기·전송 뒤에도 ON을 유지하며 다시 누르면 OFF가 됩니다.
`Enter`만 입력을 전송합니다. OFF는 Jev 호출 없이 네이티브 model/effort를 유지합니다.
전송/큐 수락 시점의 선택을 각 입력에 보관하므로 이후 토글은 기존 대기 입력을 바꾸지
않습니다. 선택은 다른 세션이나 앱 재시작에 상속하지 않습니다. F7 맥락 미리보기는
제거했습니다. Jev ON에서 전송하는 것은 전체 대화가 아니라 실제 제한된 판단 입력입니다.
세션 pane의 아래쪽 테두리에 해당 pane에 연결된 Codex 계정 레이블을 표시합니다.

**Claude 세션에는 Jev 선택이나 호출이 없습니다.** Claude Jev 확장·중계는 제거했고
Claude/Kiro/Cursor와 로그인·관리 명령은 원래 CLI로 실행합니다. 계정·인증·전역
플러그인·설정은 변경하지 않습니다. 이미 실행 중인 pane에는 소급 적용되지 않습니다.
구현·검증 상태는 [변경 기록](docs/plans/2026-10-19-session-jev-toggle.md)에 둡니다.

Codex의 후보 선택 질문에는 공식 **API 참고 토큰 단가**도 제공합니다.
입력·출력·캐시와 긴 입력 할증 조건을 구분하고, 충분한 역량을 먼저 판단한 뒤 보조적으로
비교합니다. 가격 확인일은 2026-10-18이고 2026-11-18부터 미상으로 처리합니다.
전체 네이티브 토큰 수·캐시 사용·과금 방식은 알 수 없으므로 예상 실행 비용이나 구독
쿼터 절감량을 만들지 않습니다. 일반 Jev 오류는 원문 대신 허용된 사유 코드와 한국어
설명을 표시하며, 모델 요청을 보내거나 자동 재시도하지 않습니다.

Jev를 사용하는 경로는 Codex의 매 새 사용자 턴에서 버전이 고정된 기준과 현재 프롬프트,
직전 수락된 같은 세션의 작은 판단 상태로 Jev가 모델·추론 강도를 선택하게 합니다.
기본 경로에서 전체 대화를 보내거나 paging하지 않습니다. compact 뒤에도 판단 상태는
유지하지만 이를 작업 완료 사실로 취급하지 않습니다. 설치본의 실제 Jev 연속 3회와
한·영 예시 12건 확인은 [0.55.8 검증 기록](docs/jev-decision-boundary-0.55.8.validation.json)에 있습니다.
Codex 실행은 합성 backend이며 실제 실행 모델/effort나 품질·비용 절감의 증명은 아닙니다.
새로 여는 모든 Codex pane은 Jev ON/OFF와 관계없이 Node.js 22 이상과 아래의
검증된 전용 Codex UI가 필요합니다. OFF에서는 Jev 키·정책 조회와 판단 호출을 건너뜁니다.
일반 PATH의 Codex로 조용히 대체하지 않습니다.
기존 pane은 그대로 두며, 새로 여는 Codex pane에 적용합니다.
정책 파일이 없으면 현재 모델 목록 설명 기반의 선택을 사용하며, 측정된 비용 절감은
보장하지 않습니다. 로컬 0.55.9는 기존 모델줄에서 판단·요청 수락·실행 중·턴 완료를
구분합니다. 실행 중은 `turn/started`로 확인하며, 별도 서버 모델 변경 보고가 없어도
정상 표시합니다. 요청 모델·effort를 실제 서빙 영수증이라고 주장하지 않습니다.
[0.55.9 실행 상태 검증](docs/jev-routing-lifecycle-0.55.9.validation.json).
로컬 0.55.10은 MindPlayer 전용 네이티브 UI로 실행 중 Enter도 개별 새 턴으로
큐에 보관합니다. 정상 완료 후 각 입력이 Jev를 거쳐 모델과 effort를 함께 적용하며,
실패·중단 뒤에는 큐를 자동 실행하지 않습니다. 일반 Codex 설치와 기존 pane은
변경하지 않습니다. 이 경로에는 검증된 별도 UI 설치가 필요하며, 누락·변조 때
일반 Codex로 조용히 우회하지 않습니다. [전용 UI 빌드·설치 경계](tools/native-codex/README.ko.md).
설치본의 실제 Jev 두 번·네이티브 두 턴에서 요청과 세션 기록의 모델·effort 일치를
확인했습니다. [0.55.10 설치·검증 기록](docs/jev-native-queue-0.55.10.validation.json).
기존 실행 pane은 그대로 유지되므로 새 실행 파일로 MindPlayer와 세션을 다시 열어야
이 경로가 적용됩니다. 서버 내부 서빙 모델이나 비용 절감을 보장하는 검증은 아닙니다.
[시험 방법·규칙·남은 한계](tools/local-model-router/README.ko.md)를 참고하세요.

로컬 0.55.11은 판단 중 Esc를 입력 시도별로 처리하고, 동일 문장 재입력도 새롭게
판단합니다. 실제 턴이 이미 제출됐다면 실제 턴 ID를 받아 중단합니다. 모델과 effort
변경 안내는 Core 적용 설정과 시작된 턴이 일치할 때만 표시합니다. 자동 goal 계속
실행은 새 사용자 입력과 구분합니다. 계정 사용량은 같은 로그인으로 확인돼도 각
슬롯의 측정값을 모두 표시하고 경고를 유지합니다. 이는 인증 분리 복구가 아닙니다.

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

In an OMP pane, MindPlayer reserves `Ctrl-X` (list), `Ctrl-Z` (zoom),
`Alt-W` (next pane), and `Ctrl-Q` or `Alt-Q` (close pane). As with other
providers, closing a live pane hides it without terminating the session. OMP
receives its native `Ctrl-P`, `Ctrl-T`, `Ctrl-G`, `Ctrl-W`, `Ctrl-Y`, Tab and
Shift-Tab; its native `Ctrl-Q` is reserved by MindPlayer.

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
only a label, not proof of identity: check the displayed email and workspace/organization
before pressing `v` to bind. Confirmation performs another fresh read.

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
| <kbd>Tab</kbd> / <kbd>Shift‑Tab</kbd> | cycle live panes when 2+ are open; in OMP both keys go to OMP |
| <kbd>Ctrl‑w</kbd> / <kbd>Alt‑w</kbd> | cycle live-pane focus; use <kbd>Alt‑w</kbd> in OMP |
| <kbd>Ctrl‑z</kbd> | zoom the focused pane to full size (toggle again for the split view) |
| <kbd>Ctrl‑q</kbd> / <kbd>Alt‑q</kbd> | close the focused live pane; <kbd>Alt‑q</kbd> also works in OMP |
| <kbd>Ctrl‑t</kbd> | **transition report** outside OMP — `topic / RUNBOOK §n / files` → review → <kbd>Enter</kbd> sends, <kbd>e</kbd> edits first |
| <kbd>Ctrl‑p</kbd> | open a local `.html` produced by a focused non-OMP pane in your browser |
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
| <kbd>a</kbd> | toggle archived view · <kbd>g</kbd> toggle sub‑agents · <kbd>r</kbd> rescan |
| <kbd>u</kbd> | **accounts** — which login each provider starts a session on, with its usage. See below |
| <kbd>?</kbd> | show the full keyboard-shortcut list |
| <kbd>q</kbd> | quit (stops all sessions) |

Inside a live session, <kbd>Enter</kbd> submits and
<kbd>Shift</kbd>/<kbd>Alt</kbd>/<kbd>Ctrl</kbd>+<kbd>Enter</kbd> inserts a
newline, and Korean/CJK input works with the cursor tracking the prompt. The
**mouse wheel scrolls MindPlayer's own scrollback** (so history that ran off the
top stays readable). **Drag inside a pane to select &amp; copy just that pane's
text** to the system clipboard (via OSC 52) — so a side‑by‑side split never
copies the neighbor pane too. (For panes running a full‑screen mouse app like
Codex, the drag goes to that app; use your terminal's <kbd>Shift</kbd>+drag
native selection there.)

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

Exactly one account per provider is *in use*, so the choice never depends on
list order. The footer keeps one line per provider — the account a new session
would take — and the rest live here.

**Sessions stay with the login that made them.** A transcript only exists in
that account's home, so resuming always goes back to it, and the list marks a
session with its account name only when that is *not* the one in use. To carry
a conversation to another login, hand it over with <kbd>h</kbd> — that starts a
new session there with the previous context.

**Two caveats.** Claude and Cursor keep their usage credential in the macOS
Keychain, one per machine, and that one belongs to the login this machine came
with — so a second account of either runs its sessions normally but shows no
usage figure rather than borrowing the first one's. And nothing is ever copied
between accounts: a duplicated login refreshes its token and locks the original
holder out, which is a failure neither side can explain.

## 🧠 How it works

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
signed-in Vercel CLI's read access to identify the key and read its budget.
Only the selected key's current-period usage is shown. Identification requires
a unique six-character key suffix, a determinable team, complete key-list
pagination, and CLI permission to read the key budget. Suffix matching is not
full-key identity verification. The key is never placed in command arguments
or logs. Ctrl-R refreshes the row.

An optional `~/.mindplayer/vercel-key-target.json` can set a local display
target for one key and its budget period, for example
`{"key_id":"key_example","refresh_period":"monthly","target_usd":200}`.
This compares the key's actual usage against the local target; **it does not
change or enforce Vercel's server-side budget**. With no matching target the
row shows the key budget reported by Vercel. User and team budgets may also
apply even though they are not displayed.

What MindPlayer writes, all under `~/.mindplayer/`:

| Path | Contents |
| --- | --- |
| `state.json` | sidecar state — archived ids, labels, in-progress marks, topic categories, your walking-buddy pick |
| `audit.jsonl` | append-only usage log: counts and timestamps only, no session id, title, or cwd |
| `vercel-key-target.json` | optional key-specific local display target; no API key secret |
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

Canned prompts (<kbd>c</kbd> catch-up, <kbd>Ctrl‑t</kbd> transition report)
live as plain `.md` files under `~/.mindplayer/prompts/` — `catchup.md`,
`transition_report.md` — seeded with the built-in default the first time
each is used. Edit them in any text editor; the next send picks up the
change immediately, no rebuild or restart needed.

Resuming launches `codex resume <id>` / `claude --resume <id>` /
`kiro-cli chat --resume-id <id>` / `agent --resume <id>` in an embedded PTY in
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
└─ crates/
   ├─ mindplayer-core/   # discovery, token aggregation, archive state, resume
   └─ mindplayer-tui/    # ratatui + portable-pty + vt100 → binary `mindplayer`
```

## 🛠️ Develop

```bash
cargo test --all
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
