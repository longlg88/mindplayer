// MindPlayer frontend. Talks to the Rust backend via the global Tauri API.
// Each session gets its own xterm.js instance so multiple sessions keep running
// in the background and switching just shows a different one.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const state = {
  scope: "working_dir",
  cwd: "",
  sessions: [],
  aggregate: null,
  selected: 0,
  showArchived: false,
  showSubagents: false,
  terminals: new Map(), // id -> { term, fit, el, id }
  ended: new Set(), // ids whose process exited (frame kept)
  activeId: null,
  newAgent: null,
};

const $ = (id) => document.getElementById(id);
const humanTokens = (n) =>
  n >= 1_000_000
    ? (n / 1e6).toFixed(1) + "M"
    : n >= 1_000
      ? (n / 1e3).toFixed(1) + "K"
      : String(n);

function show(screen) {
  for (const s of ["screen-scope", "screen-scan", "screen-main"]) {
    $(s).classList.toggle("hidden", s !== screen);
  }
}

// --- init -----------------------------------------------------------------

async function init() {
  state.cwd = await invoke("default_cwd");
  $("cwd-input").value = state.cwd;

  document.querySelectorAll(".scope-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      document.querySelectorAll(".scope-btn").forEach((b) => b.classList.remove("selected"));
      btn.classList.add("selected");
      state.scope = btn.dataset.scope;
      // The path field only matters for working-dir scope.
      $("cwd-input").disabled = state.scope !== "working_dir";
    });
  });
  // Enter in the path field starts the scan.
  $("cwd-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") runScan();
  });
  $("scan-btn").addEventListener("click", runScan);
  $("open-btn").addEventListener("click", () => {
    show("screen-main");
    renderMain();
  });
  $("rescan-btn").addEventListener("click", runScan);
  $("show-archived").addEventListener("change", (e) => {
    state.showArchived = e.target.checked;
    state.selected = 0;
    renderList();
  });
  $("show-subagents").addEventListener("change", (e) => {
    state.showSubagents = e.target.checked;
    state.selected = 0;
    renderList();
  });

  // New-session modal: pick agent -> optional label -> start.
  $("new-btn").addEventListener("click", openNewModal);
  $("new-cancel").addEventListener("click", closeNewModal);
  document.querySelectorAll(".new-opt").forEach((b) =>
    b.addEventListener("click", () => pickAgent(b.dataset.agent)),
  );
  $("new-start").addEventListener("click", startNew);
  $("new-label-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") startNew();
    if (e.key === "Escape") closeNewModal();
  });

  // Change-working-dir modal (main view).
  $("dir-btn").addEventListener("click", openDirModal);
  $("dir-cancel").addEventListener("click", closeDirModal);
  $("dir-set").addEventListener("click", applyDir);
  $("dir-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") applyDir();
    if (e.key === "Escape") closeDirModal();
  });

  $("close-btn").addEventListener("click", closeSelected);
  window.addEventListener("resize", () => fitActive());

  await setupPtyEvents();
}

// --- scan -----------------------------------------------------------------

async function runScan() {
  // For working-dir scope, resolve & validate the typed path first; stay on the
  // scope screen with an error if it isn't a real directory.
  if (state.scope === "working_dir") {
    const typed = $("cwd-input").value;
    try {
      state.cwd = await invoke("resolve_cwd", { cwd: typed });
      $("cwd-input").value = state.cwd;
      $("cwd-error").classList.add("hidden");
    } catch (err) {
      const el = $("cwd-error");
      el.textContent = String(err);
      el.classList.remove("hidden");
      return;
    }
  }

  show("screen-scan");
  $("scan-title").textContent = "Collecting…";
  $("scan-spinner").classList.remove("hidden");
  $("scan-stats").classList.add("hidden");

  const res = await invoke("scan_sessions", { scope: state.scope, cwd: state.cwd });
  state.sessions = res.sessions;
  state.aggregate = res.aggregate;
  state.selected = 0;

  const a = res.aggregate;
  $("scan-title").textContent = "Collected";
  $("scan-spinner").classList.add("hidden");
  $("stat-codex").textContent = a.codex_count;
  $("stat-claude").textContent = a.claude_count;
  $("stat-kiro").textContent = a.kiro_count;
  const total = Math.max(a.codex_count + a.claude_count + a.kiro_count, 1);
  $("ratio-bar").style.width = (100 * a.codex_count) / total + "%";
  $("stat-total").textContent = humanTokens(a.total.total);
  // Kiro token counts aren't read from its log, so show "—" rather than 0.
  $("stat-breakdown").textContent = `(codex ${humanTokens(a.codex.total)} · claude ${humanTokens(a.claude.total)} · kiro ${a.kiro_count > 0 ? "—" : "0"})`;
  $("scan-stats").classList.remove("hidden");
}

// Rescan without leaving the main view (picks up new sessions / resolves labels).
async function scanSilent() {
  if ($("screen-main").classList.contains("hidden")) return;
  try {
    const res = await invoke("scan_sessions", { scope: state.scope, cwd: state.cwd });
    const selectedId = selectedSession()?.id;
    state.sessions = res.sessions;
    state.aggregate = res.aggregate;
    if (selectedId) {
      const vis = visibleSessions();
      const idx = vis.findIndex((s) => s.id === selectedId);
      if (idx >= 0) state.selected = idx;
    }
    renderList();
    renderStatus();
  } catch (_) {}
}

// --- main / list ----------------------------------------------------------

function visibleSessions() {
  return state.sessions.filter((s) => {
    if (s.archived !== state.showArchived) return false;
    if (!state.showSubagents && s.is_subagent) return false;
    return true;
  });
}

function selectedSession() {
  return visibleSessions()[state.selected];
}

function renderMain() {
  renderList();
  renderStatus();
  fitActive();
}

function dotFor(id) {
  if (state.terminals.has(id) && !state.ended.has(id)) return "●";
  if (state.ended.has(id)) return "○";
  return "";
}

function renderList() {
  const list = $("session-list");
  list.innerHTML = "";
  const vis = visibleSessions();
  if (state.selected >= vis.length) state.selected = Math.max(0, vis.length - 1);

  vis.forEach((s, i) => {
    const li = document.createElement("li");
    li.className = "session" + (i === state.selected ? " active" : "");
    li.innerHTML =
      `<span class="dot">${dotFor(s.id)}</span>` +
      `<span class="tag ${s.agent}">${s.agent}</span>` +
      `<span class="title"></span>` +
      `<span class="tok">${
        s.agent === "kiro"
          ? s.context_pct != null
            ? Math.round(s.context_pct) + "%"
            : "—"
          : humanTokens(s.tokens.total)
      }</span>`;
    li.querySelector(".title").textContent = s.title || "(untitled)";
    li.addEventListener("click", () => {
      state.selected = i;
      renderList();
      resumeSelected();
    });
    list.appendChild(li);
  });

  const tab = state.showArchived ? "archived" : "active";
  $("list-title").textContent = `Sessions · ${tab} (${vis.length})`;
}

function renderStatus() {
  const a = state.aggregate;
  if (!a) return;
  const count = a.codex_count + a.claude_count + a.kiro_count;
  const scopeLabel = state.scope === "global" ? "global" : `working dir (${state.cwd})`;
  const kiro = a.kiro_count > 0 ? " · kiro —" : "";
  $("statusbar").textContent =
    `${count} sessions · ${humanTokens(a.total.total)} tok ` +
    `(codex ${humanTokens(a.codex.total)} · claude ${humanTokens(a.claude.total)}${kiro}) · ${scopeLabel}`;
}

// --- terminals (one per session) -----------------------------------------

function createTerminal(initialId) {
  const el = document.createElement("div");
  el.className = "term-instance hidden";
  $("terminal").appendChild(el);

  const term = new Terminal({
    fontFamily: "SF Mono, Menlo, monospace",
    fontSize: 13,
    theme: { background: "#0c0e13", foreground: "#f3f5f8" },
    cursorBlink: true,
    scrollback: 5000,
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(el);

  const t = { term, fit, el, id: initialId };
  term.onData((data) => invoke("pty_write", { id: t.id, data }));
  // Shift+Enter inserts a newline (LF) instead of submitting (CR).
  term.attachCustomKeyEventHandler((e) => {
    if (e.type === "keydown" && e.key === "Enter" && e.shiftKey) {
      invoke("pty_write", { id: t.id, data: "\n" });
      return false;
    }
    return true;
  });
  requestAnimationFrame(() => fit.fit());
  return t;
}

function disposeTerminal(id) {
  const t = state.terminals.get(id);
  if (!t) return;
  t.term.dispose();
  t.el.remove();
  state.terminals.delete(id);
}

function setActive(id) {
  state.activeId = id;
  for (const [tid, t] of state.terminals) {
    t.el.classList.toggle("hidden", tid !== id);
  }
  $("term-hint").classList.add("hidden");
  $("close-btn").classList.remove("hidden");
  fitActive();
  updateTermTitle();
  renderList();
}

function fitActive() {
  const t = state.terminals.get(state.activeId);
  if (!t) return;
  t.fit.fit();
  invoke("pty_resize", { id: t.id, cols: t.term.cols, rows: t.term.rows });
  t.term.focus();
}

function updateTermTitle() {
  if (!state.activeId) {
    $("term-title").textContent = "Live";
    return;
  }
  const ended = state.ended.has(state.activeId) ? " (ended)" : "";
  const n = state.terminals.size;
  const live = n > 1 ? ` [${n} live]` : "";
  $("term-title").textContent = `Live · ${state.activeId.slice(0, 8)}${ended}${live}`;
}

// --- resume / new ---------------------------------------------------------

async function resumeSelected() {
  const s = selectedSession();
  if (!s) return;
  // Already attached and alive → just bring it to the foreground.
  if (state.terminals.has(s.id) && !state.ended.has(s.id)) {
    setActive(s.id);
    return;
  }
  // Ended → drop the old terminal and relaunch.
  if (state.terminals.has(s.id)) disposeTerminal(s.id);
  state.ended.delete(s.id);

  const t = createTerminal(s.id);
  state.terminals.set(s.id, t);
  setActive(s.id);
  await invoke("pty_start", {
    sessionId: s.id,
    agent: s.agent,
    cwd: s.cwd,
    cols: t.term.cols,
    rows: t.term.rows,
  });
  fitActive();
}

function openNewModal() {
  state.newAgent = null;
  $("new-step-agent").classList.remove("hidden");
  $("new-step-label").classList.add("hidden");
  $("new-label-input").value = "";
  $("new-title").textContent = "New session";
  $("new-modal").classList.remove("hidden");
}

function closeNewModal() {
  $("new-modal").classList.add("hidden");
}

// --- change working dir (main view) ---------------------------------------

function openDirModal() {
  $("dir-input").value = state.scope === "global" ? "" : state.cwd;
  $("dir-error").classList.add("hidden");
  $("dir-modal").classList.remove("hidden");
  $("dir-input").focus();
}

function closeDirModal() {
  $("dir-modal").classList.add("hidden");
}

// Validate the typed path, switch the scope to it (blank = global), and rescan.
async function applyDir() {
  const typed = $("dir-input").value.trim();
  if (typed === "") {
    state.scope = "global";
    state.cwd = "";
  } else {
    try {
      state.cwd = await invoke("resolve_cwd", { cwd: typed });
    } catch (err) {
      const el = $("dir-error");
      el.textContent = String(err);
      el.classList.remove("hidden");
      return; // keep the modal open so the path can be fixed
    }
    state.scope = "working_dir";
    $("cwd-input").value = state.cwd;
  }
  closeDirModal();
  syncScopeButtons();
  state.selected = 0;
  // Rescan in place; scanSilent keeps us on the main view.
  await scanSilent();
  renderStatus();
}

// Keep the scope-screen buttons/field in sync after a programmatic scope change.
function syncScopeButtons() {
  document.querySelectorAll(".scope-btn").forEach((b) => {
    b.classList.toggle("selected", b.dataset.scope === state.scope);
  });
  $("cwd-input").disabled = state.scope !== "working_dir";
}

function pickAgent(agent) {
  state.newAgent = agent;
  $("new-title").textContent = `New ${agent} session`;
  $("new-step-agent").classList.add("hidden");
  $("new-step-label").classList.remove("hidden");
  $("new-label-input").focus();
}

async function startNew() {
  const agent = state.newAgent || "codex";
  const label = $("new-label-input").value.trim();
  closeNewModal();

  const t = createTerminal(null);
  setActivePendingTerminal(t);
  const id = await invoke("pty_new", {
    agent,
    cwd: state.cwd,
    label,
    cols: t.term.cols,
    rows: t.term.rows,
  });
  t.id = id;
  state.terminals.set(id, t);
  setActive(id);

  // If labeled, re-scan a few times so the new session shows up labeled once it
  // has written its rollout file (after the user's first interaction).
  if (label) {
    [4000, 9000, 16000].forEach((ms) => setTimeout(scanSilent, ms));
  } else {
    setTimeout(scanSilent, 4000);
  }
}

// Show a not-yet-keyed terminal while pty_new resolves its id.
function setActivePendingTerminal(t) {
  for (const [, other] of state.terminals) other.el.classList.add("hidden");
  t.el.classList.remove("hidden");
  $("term-hint").classList.add("hidden");
  $("close-btn").classList.remove("hidden");
  requestAnimationFrame(() => t.fit.fit());
}

async function closeSelected() {
  const s = selectedSession();
  if (!s) return;
  if (state.terminals.has(s.id)) {
    await invoke("pty_kill", { id: s.id });
    disposeTerminal(s.id);
  }
  state.ended.delete(s.id);
  if (state.activeId === s.id) {
    state.activeId = null;
    $("close-btn").classList.add("hidden");
    $("term-hint").classList.remove("hidden");
    updateTermTitle();
  }
  await invoke("set_archived", { id: s.id, archived: true });
  const found = state.sessions.find((x) => x.id === s.id);
  if (found) found.archived = true;
  renderList();
}

// --- pty events -----------------------------------------------------------

async function setupPtyEvents() {
  await listen("pty://output", (e) => {
    const t = state.terminals.get(e.payload.id);
    if (!t) return;
    const bytes = Uint8Array.from(atob(e.payload.b64), (c) => c.charCodeAt(0));
    t.term.write(bytes);
  });
  await listen("pty://exit", (e) => {
    if (state.terminals.has(e.payload.id)) {
      state.ended.add(e.payload.id);
      if (state.activeId === e.payload.id) updateTermTitle();
      renderList();
    }
  });
}

init();
