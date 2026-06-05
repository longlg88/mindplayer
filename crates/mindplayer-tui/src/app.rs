//! Application state machine: scope select -> scanning -> main, plus the
//! embedded PTY lifecycle and archive actions.

use crate::pty::PtySession;
use chrono::{DateTime, Utc};
use mindplayer_core::{
    resume, scan, sort_by_recency, tokens::human_tokens, Agent, Aggregate, ScanConfig, Scope,
    Session, State,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// Background refresh result: per-session (id, last-active-from-mtime).
type ActivityUpdate = Vec<(String, DateTime<Utc>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    ScopeSelect,
    Scanning,
    ScanSummary,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Terminal,
}

/// How a session in the list is doing right now, for the status badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Live PTY producing output right now.
    Working,
    /// Live PTY, but quiet (waiting for input / idle).
    Idle,
    /// Child has exited; final frame kept.
    Ended,
    /// Not running inside MindPlayer (a history entry).
    Inactive,
}

/// A session counts as "working" if it produced output within this window.
const WORKING_WINDOW: Duration = Duration::from_millis(1200);

pub struct App {
    pub screen: Screen,
    /// 0 = working dir, 1 = global.
    pub scope_choice: usize,
    pub cwd: PathBuf,
    pub scope: Scope,
    pub cfg: ScanConfig,
    pub state: State,

    /// Full in-scope scan (drives the aggregate / scan numbers).
    pub all_sessions: Vec<Session>,
    /// Totals over the full in-scope scan (drives the post-scan "Collected"
    /// screen). Includes archived + sub-agent sessions.
    pub aggregate: Aggregate,
    /// Totals over just the currently-visible rows (drives the status bar, so
    /// its count/tokens match the list the user is looking at).
    pub visible_aggregate: Aggregate,
    /// Indices into `all_sessions` for the rows shown, after the archived /
    /// sub-agent view filters. Indices (not clones) keep the refresh cheap.
    pub visible: Vec<usize>,
    pub selected: usize,
    pub show_archived: bool,
    /// Show spawned helper/sub-agent sessions (hidden by default).
    pub show_subagents: bool,

    pub focus: Focus,
    /// All concurrently-running (or recently-ended) sessions, keyed by id, so
    /// switching between sessions keeps the others running in the background.
    pub ptys: HashMap<String, PtySession>,
    /// The session id currently shown in the right pane.
    pub active: Option<String>,
    /// Session ids whose child has exited; their final frame is kept visible.
    pub ended: HashSet<String>,
    /// Per-session last-seen output counter and the time it last changed, used
    /// to show a "working" badge for sessions actively producing output.
    out_seq: HashMap<String, u64>,
    out_at: HashMap<String, Instant>,
    /// Set when a resume is requested; consumed once the right-pane size is known.
    pub pending: Option<PendingSpawn>,
    /// codex/claude picker for a new session; None when hidden.
    pub new_picker: Option<usize>,
    /// When `Some`, the label-input step of new-session creation is active and
    /// holds the text typed so far.
    pub new_label: Option<String>,
    /// Agent chosen in the picker, awaiting a label.
    pub new_agent: Option<Agent>,
    /// When `Some`, the label-input modal is editing an EXISTING session's label
    /// (this is its id) rather than creating a new session. Shares `new_label`
    /// as the text buffer.
    pub label_target: Option<String>,
    /// Monotonic counter so each new session gets a unique synthetic id.
    new_counter: u64,
    /// Sessions started inside MindPlayer that have no disk file yet (codex /
    /// claude only write the rollout after the first interaction). Kept in the
    /// list so a brand-new session never disappears; reconciled to the real
    /// session once its file appears.
    extra_sessions: Vec<Session>,
    /// For each synthetic new-session id, the set of real session ids that
    /// already existed when it was created. `merge_extras` only adopts a real
    /// session that is NOT in this baseline, so it can never re-key the new
    /// session's live PTY onto a pre-existing (or freshly-resumed) session.
    new_baselines: HashMap<String, HashSet<String>>,

    /// Last known inner size of the right pane (rows, cols).
    pub pty_rows: u16,
    pub pty_cols: u16,

    scan_rx: Option<Receiver<Vec<Session>>>,
    /// In-flight background mtime refresh (id -> last_active), kept off the main
    /// thread so the periodic re-sort never stalls input/rendering.
    refresh_rx: Option<Receiver<ActivityUpdate>>,
    /// In-flight background full re-scan (to pick up newly created sessions).
    bg_rescan_rx: Option<Receiver<Vec<Session>>>,
    /// When to kick the next background re-scan (after creating a session).
    rescan_due: Option<Instant>,
    pub spinner: usize,
    pub status: String,
    pub should_quit: bool,
}

/// A pending PTY spawn, deferred until the pane size is known.
pub struct PendingSpawn {
    pub command: mindplayer_core::Command,
    pub session_id: String,
}

impl App {
    /// Start with the process's current directory as the working-dir scope.
    pub fn new() -> Self {
        Self::new_in(std::env::current_dir().unwrap_or_default())
    }

    /// Start with an explicit directory as the working-dir scope (and the cwd
    /// new sessions launch in). Lets `mindplayer <dir>` target any project
    /// without `cd`-ing there first.
    pub fn new_in(cwd: PathBuf) -> Self {
        App {
            screen: Screen::ScopeSelect,
            scope_choice: 0,
            scope: Scope::WorkingDir(cwd.clone()),
            cwd,
            cfg: ScanConfig::from_env(),
            state: State::load(),
            all_sessions: Vec::new(),
            aggregate: Aggregate::default(),
            visible_aggregate: Aggregate::default(),
            visible: Vec::new(),
            selected: 0,
            show_archived: false,
            show_subagents: false,
            focus: Focus::List,
            ptys: HashMap::new(),
            active: None,
            ended: HashSet::new(),
            out_seq: HashMap::new(),
            out_at: HashMap::new(),
            pending: None,
            new_picker: None,
            new_label: None,
            new_agent: None,
            label_target: None,
            new_counter: 0,
            extra_sessions: Vec::new(),
            new_baselines: HashMap::new(),
            pty_rows: 24,
            pty_cols: 80,
            scan_rx: None,
            refresh_rx: None,
            bg_rescan_rx: None,
            rescan_due: None,
            spinner: 0,
            status: String::new(),
            should_quit: false,
        }
    }

    /// Whether the animated mascot is currently on screen (so the loop should
    /// redraw on each tick to animate it).
    pub fn mascot_visible(&self) -> bool {
        match self.screen {
            Screen::ScopeSelect | Screen::Scanning | Screen::ScanSummary => true,
            Screen::Main => self.focus == Focus::List || self.active_pty().is_none(),
        }
    }

    /// Whether a deferred background re-scan is due, clearing the timer.
    pub fn rescan_due(&mut self) -> bool {
        match self.rescan_due {
            Some(at) if Instant::now() >= at => {
                self.rescan_due = None;
                true
            }
            _ => false,
        }
    }

    // --- new-session label input -----------------------------------------

    /// Picker -> label input: remember the agent, start an empty label buffer.
    pub fn choose_new_agent(&mut self, agent: Agent) {
        self.new_agent = Some(agent);
        self.new_label = Some(String::new());
        self.new_picker = None;
    }

    pub fn label_input_push(&mut self, c: char) {
        if let Some(buf) = self.new_label.as_mut() {
            buf.push(c);
        }
    }

    pub fn label_input_backspace(&mut self) {
        if let Some(buf) = self.new_label.as_mut() {
            buf.pop();
        }
    }

    /// Confirm the label input and spawn the new session.
    pub fn confirm_new_session(&mut self) {
        let agent = self.new_agent.unwrap_or(Agent::Codex);
        let label = self.new_label.take().unwrap_or_default();
        self.request_new(agent, &label);
    }

    pub fn cancel_new_session(&mut self) {
        self.new_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.label_target = None;
    }

    /// Open the label-input modal for the currently-selected session so an
    /// existing session (one created outside MindPlayer, or without a label)
    /// can be tagged. Pre-fills the current label so it can be edited or
    /// cleared. Synthetic placeholders use the new-session flow instead.
    pub fn begin_label_edit(&mut self) {
        let Some(s) = self.selected_session() else {
            return;
        };
        if s.id.starts_with("new:") {
            self.status = "label is set when you create a new session".to_string();
            return;
        }
        let id = s.id.clone();
        let existing = self.state.label_for(&id).unwrap_or_default().to_string();
        self.label_target = Some(id);
        self.new_label = Some(existing);
    }

    /// Confirm the label-input modal when editing an existing session: persist
    /// the label and update the in-memory title. A blank label clears it (the
    /// auto-extracted title is restored on the next scan).
    pub fn confirm_label_edit(&mut self) {
        let Some(id) = self.label_target.take() else {
            return;
        };
        let buf = self.new_label.take().unwrap_or_default();
        let label = buf.trim();
        self.state.set_label(&id, label);
        let _ = self.state.save();
        if label.is_empty() {
            self.status = format!("label cleared for {}", short(&id));
            // Re-extract the original title from disk shortly.
            self.rescan_due = Some(Instant::now());
        } else {
            if let Some(s) = self.all_sessions.iter_mut().find(|s| s.id == id) {
                s.title = format!("🏷 {label}");
            }
            self.status = format!("labeled: {label}");
        }
    }

    // --- scope + scanning -------------------------------------------------

    pub fn scope_label(&self) -> String {
        match self.scope {
            Scope::Global => "global".to_string(),
            Scope::WorkingDir(_) => format!("working dir ({})", self.cwd.display()),
        }
    }

    /// Spawn a scan of the current scope on a background thread.
    fn spawn_scan(&self) -> Receiver<Vec<Session>> {
        let scope = self.scope.clone();
        let cfg = self.cfg.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(scan(&scope, &cfg));
        });
        rx
    }

    /// Confirm the scope choice and kick off the initial scan (scan screen).
    pub fn start_scan(&mut self) {
        self.scope = if self.scope_choice == 0 {
            Scope::WorkingDir(self.cwd.clone())
        } else {
            Scope::Global
        };
        self.state.last_scope = Some(self.scope.label());
        let _ = self.state.save();

        self.scan_rx = Some(self.spawn_scan());
        self.screen = Screen::Scanning;
        self.spinner = 0;
    }

    /// Re-scan in the background without leaving the main view — used to pick up
    /// newly created sessions (and resolve their pending labels). No-op if one
    /// is already running.
    pub fn start_bg_rescan(&mut self) {
        if self.bg_rescan_rx.is_none() {
            self.bg_rescan_rx = Some(self.spawn_scan());
        }
    }

    /// Apply a finished background re-scan in place (keeps the main view and the
    /// cursor on the same session), resolving any pending labels against the
    /// fresh session set. Returns true if anything changed.
    pub fn poll_bg_rescan(&mut self) -> bool {
        let Some(rx) = &self.bg_rescan_rx else {
            return false;
        };
        let Ok(mut sessions) = rx.try_recv() else {
            return false;
        };
        self.bg_rescan_rx = None;

        let selected_id = self.selected_session().map(|s| s.id.clone());
        // Resolve labels against the raw scan, persist, then stamp titles.
        if self.state.resolve_pending(&sessions) {
            let _ = self.state.save();
        }
        self.state.apply(&mut sessions);
        self.aggregate = Aggregate::of(&sessions);
        self.all_sessions = sessions;
        self.merge_extras();
        self.rebuild_visible();
        if let Some(id) = selected_id {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }
        // Keep retrying (until matched or expired) while labels are unresolved.
        if !self.state.pending_labels.is_empty() {
            self.rescan_due = Some(Instant::now() + Duration::from_secs(6));
        }
        true
    }

    /// Poll the scan thread; when finished, populate state and show the summary.
    /// Returns true if results arrived (needs redraw).
    pub fn poll_scan(&mut self) -> bool {
        if let Some(rx) = &self.scan_rx {
            if let Ok(mut sessions) = rx.try_recv() {
                // Resolve labels queued in a previous run before stamping titles.
                if self.state.resolve_pending(&sessions) {
                    let _ = self.state.save();
                }
                self.state.apply(&mut sessions);
                self.aggregate = Aggregate::of(&sessions);
                self.all_sessions = sessions;
                self.merge_extras();
                self.rebuild_visible();
                self.scan_rx = None;
                self.screen = Screen::ScanSummary;
                // If labels are still unresolved (their sessions don't exist
                // yet), keep trying via background re-scans.
                if !self.state.pending_labels.is_empty() {
                    self.rescan_due = Some(Instant::now() + Duration::from_secs(6));
                }
                return true;
            }
        }
        false
    }

    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    /// Move from the scan summary into the main view.
    pub fn open_main(&mut self) {
        self.screen = Screen::Main;
    }

    // --- list management --------------------------------------------------

    fn rebuild_visible(&mut self) {
        let show_archived = self.show_archived;
        let show_subagents = self.show_subagents;
        self.visible = self
            .all_sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| show_archived == s.archived)
            .filter(|(_, s)| show_subagents || !s.is_subagent)
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        // Keep the status-bar totals in sync with what's actually listed.
        self.visible_aggregate = Aggregate::of_refs(
            self.visible
                .iter()
                .filter_map(|&i| self.all_sessions.get(i)),
        );
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.visible
            .get(self.selected)
            .and_then(|&i| self.all_sessions.get(i))
    }

    /// The session at a visible row (used by the renderer).
    pub fn session_at(&self, row: usize) -> Option<&Session> {
        self.visible
            .get(row)
            .and_then(|&i| self.all_sessions.get(i))
    }

    pub fn toggle_archived_view(&mut self) {
        self.show_archived = !self.show_archived;
        self.selected = 0;
        self.rebuild_visible();
    }

    pub fn toggle_subagents(&mut self) {
        self.show_subagents = !self.show_subagents;
        self.selected = 0;
        self.rebuild_visible();
    }

    pub fn rescan(&mut self) {
        self.start_scan();
    }

    /// Kick off a background mtime refresh (no-op if one is already running).
    /// The `stat` of every session file happens off the main thread so input
    /// and rendering never stall; results are applied in [`Self::poll_refresh`].
    pub fn start_refresh(&mut self) {
        if self.refresh_rx.is_some() || self.all_sessions.is_empty() {
            return;
        }
        let items: Vec<(String, PathBuf)> = self
            .all_sessions
            .iter()
            .map(|s| (s.id.clone(), s.file.clone()))
            .collect();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let out: Vec<(String, DateTime<Utc>)> = items
                .into_iter()
                .filter_map(|(id, path)| {
                    std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .map(|t| (id, DateTime::<Utc>::from(t)))
                })
                .collect();
            let _ = tx.send(out);
        });
        self.refresh_rx = Some(rx);
    }

    /// Apply a finished background refresh: update activity times, re-sort
    /// newest-first, and keep the cursor on the same session by id. Returns
    /// true if the list changed (needs redraw).
    pub fn poll_refresh(&mut self) -> bool {
        let Some(rx) = &self.refresh_rx else {
            return false;
        };
        let Ok(updates) = rx.try_recv() else {
            return false;
        };
        self.refresh_rx = None;

        let times: HashMap<String, DateTime<Utc>> = updates.into_iter().collect();
        for s in self.all_sessions.iter_mut() {
            if let Some(t) = times.get(&s.id) {
                s.last_active = Some(*t);
            }
        }
        let selected_id = self.selected_session().map(|s| s.id.clone());
        sort_by_recency(&mut self.all_sessions);
        self.rebuild_visible();
        if let Some(id) = selected_id {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }
        true
    }

    // --- PTY lifecycle ----------------------------------------------------

    /// The session id currently shown in the right pane, if it has a PTY.
    pub fn active_pty(&self) -> Option<&PtySession> {
        self.active.as_ref().and_then(|id| self.ptys.get(id))
    }

    /// Whether the displayed session's child has exited.
    pub fn active_ended(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|id| self.ended.contains(id))
    }

    /// True if a session is alive and is the one being displayed.
    pub fn has_live_pty(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|id| self.ptys.contains_key(id))
    }

    /// Whether session `id` is running (has a PTY that hasn't ended).
    pub fn is_running(&self, id: &str) -> bool {
        self.ptys.contains_key(id) && !self.ended.contains(id)
    }

    /// Refresh per-session output-activity tracking from each PTY's read
    /// counter. Returns true if any session's activity changed (needs redraw to
    /// flip its status badge). Also drops tracking for closed sessions.
    pub fn poll_activity(&mut self) -> bool {
        let mut changed = false;
        let now = Instant::now();
        for (id, pty) in self.ptys.iter() {
            let seq = pty.output_seq();
            if self.out_seq.get(id) != Some(&seq) {
                self.out_seq.insert(id.clone(), seq);
                self.out_at.insert(id.clone(), now);
                changed = true;
            }
        }
        self.out_seq.retain(|id, _| self.ptys.contains_key(id));
        self.out_at.retain(|id, _| self.ptys.contains_key(id));
        changed
    }

    /// True while any session is within its "working" window — the loop uses
    /// this to keep redrawing so a badge can decay from working → idle even
    /// with no new events.
    pub fn any_recent_activity(&self) -> bool {
        self.out_at.values().any(|t| t.elapsed() < WORKING_WINDOW)
    }

    /// Status of session `id` for the list badge.
    pub fn session_status(&self, id: &str) -> SessionStatus {
        if self.ended.contains(id) {
            SessionStatus::Ended
        } else if self.ptys.contains_key(id) {
            let working = self
                .out_at
                .get(id)
                .is_some_and(|t| t.elapsed() < WORKING_WINDOW);
            if working {
                SessionStatus::Working
            } else {
                SessionStatus::Idle
            }
        } else {
            SessionStatus::Inactive
        }
    }

    /// Request a resume of the selected session in the right pane. If it is
    /// already running, just switch to it (keeping every other session alive).
    pub fn request_resume(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        self.focus = Focus::Terminal;
        if self.is_running(&session.id) {
            // Already live in the background — just bring it to the foreground.
            self.active = Some(session.id);
            return;
        }
        // A synthetic new-session has no real id to `resume`; just show its
        // (possibly ended) pane if it still exists, otherwise stay on the list.
        if session.id.starts_with("new:") {
            if self.ptys.contains_key(&session.id) {
                self.active = Some(session.id);
            } else {
                self.focus = Focus::List;
            }
            return;
        }
        self.pending = Some(PendingSpawn {
            command: resume(&session),
            session_id: session.id.clone(),
        });
        self.active = Some(session.id.clone());
        self.status = format!("resuming {} {}", session.agent.as_str(), short(&session.id));
    }

    /// Spawn a new Codex/Claude session in the current scope dir, optionally
    /// tagging the resulting session with a user label.
    pub fn request_new(&mut self, agent: Agent, label: &str) {
        let dir = match &self.scope {
            Scope::WorkingDir(p) => p.clone(),
            Scope::Global => self.cwd.clone(),
        };
        let command = mindplayer_core::new_session(agent, dir.clone());
        // Synthetic, unique id so it never collides with a real session or a
        // previous new session of the same agent.
        self.new_counter += 1;
        let session_id = format!("new:{}:{}", agent.as_str(), self.new_counter);
        // Snapshot the real sessions that already exist, so reconciliation can
        // only ever attach this new session's PTY to a genuinely-new disk
        // session — never to one that was already present (or freshly resumed).
        let baseline: HashSet<String> = self
            .all_sessions
            .iter()
            .filter(|s| !s.id.starts_with("new:"))
            .map(|s| s.id.clone())
            .collect();
        self.new_baselines.insert(session_id.clone(), baseline);
        self.active = Some(session_id.clone());
        self.pending = Some(PendingSpawn {
            command,
            session_id: session_id.clone(),
        });
        self.new_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.focus = Focus::Terminal;

        let label = label.trim();
        let now = Utc::now();
        let title = if label.is_empty() {
            format!("(new {} session)", agent.as_str())
        } else {
            format!("🏷 {label}")
        };
        // Show the new session in the list immediately so it never disappears,
        // even before codex/claude writes its rollout file. Reconciled to the
        // real session (and its PTY re-keyed) once that file appears.
        let synthetic = Session {
            id: session_id,
            agent,
            cwd: dir.clone(),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            tokens: Default::default(),
            title,
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        self.extra_sessions.push(synthetic.clone());
        self.all_sessions.push(synthetic);
        self.rebuild_visible();

        if label.is_empty() {
            self.status = format!("new {} session", agent.as_str());
        } else {
            self.status = format!("new {} session: {label}", agent.as_str());
            // Persisted: codex/claude only write the rollout file after the
            // first interaction, so the label is matched on a later scan (and
            // survives restarts). A small margin absorbs clock skew.
            self.state.add_pending_label(
                agent.as_str(),
                dir,
                now - chrono::Duration::seconds(5),
                label,
            );
            let _ = self.state.save();
        }
        // Pick the new session up in the list shortly after it's created.
        self.rescan_due = Some(Instant::now() + Duration::from_secs(3));
    }

    /// Re-attach background-created sessions after a fresh scan: drop the
    /// synthetic placeholder once its real disk session appears (re-keying the
    /// live PTY to the real id), and re-append the ones still unmatched so they
    /// stay visible.
    fn merge_extras(&mut self) {
        if self.extra_sessions.is_empty() {
            return;
        }
        let mut claimed: HashSet<String> = HashSet::new();
        let mut remaining = Vec::new();
        for extra in std::mem::take(&mut self.extra_sessions) {
            let after = extra
                .started_at
                .map(|t| t - chrono::Duration::seconds(30))
                .unwrap_or_else(Utc::now);
            let baseline = self.new_baselines.get(&extra.id);
            let ptys = &self.ptys;
            let matched = self
                .all_sessions
                .iter()
                .filter(|s| {
                    !s.id.starts_with("new:")
                        && !claimed.contains(&s.id)
                        // Never re-key onto a session that already owns a live
                        // PTY (e.g. one the user resumed) — that would drop the
                        // displaced PtySession and silently SIGKILL its child.
                        && !ptys.contains_key(&s.id)
                        // Only adopt a session that did NOT exist when this new
                        // session was created — i.e. the one codex/claude just
                        // wrote — never a pre-existing same-dir/same-agent one.
                        && baseline.is_none_or(|b| !b.contains(&s.id))
                        && s.agent == extra.agent
                        && s.cwd == extra.cwd
                        && s.started_at.is_some_and(|t| t >= after)
                })
                .max_by_key(|s| s.started_at)
                .map(|s| s.id.clone());
            match matched {
                Some(real_id) => {
                    // Move the live PTY / state from the synthetic id to the real
                    // one. The filter guarantees `real_id` is not already a live
                    // PTY, so this insert never clobbers a running session.
                    if let Some(pty) = self.ptys.remove(&extra.id) {
                        self.ptys.insert(real_id.clone(), pty);
                    }
                    if self.ended.remove(&extra.id) {
                        self.ended.insert(real_id.clone());
                    }
                    if self.active.as_deref() == Some(extra.id.as_str()) {
                        self.active = Some(real_id.clone());
                    }
                    self.new_baselines.remove(&extra.id);
                    claimed.insert(real_id);
                    // The real session is already in `all_sessions`; drop the extra.
                }
                None => {
                    self.all_sessions.push(extra.clone());
                    remaining.push(extra);
                }
            }
        }
        self.extra_sessions = remaining;
    }

    /// Consume a pending spawn now that the pane size is known. Other sessions'
    /// PTYs are left running in the background.
    pub fn spawn_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let id = pending.session_id.clone();
        // Replace only a previous (ended) PTY for this same id.
        if let Some(mut old) = self.ptys.remove(&id) {
            old.kill();
        }
        self.ended.remove(&id);
        match PtySession::spawn(&pending.command, &id, self.pty_rows, self.pty_cols) {
            Ok(pty) => {
                self.ptys.insert(id.clone(), pty);
                self.active = Some(id);
            }
            Err(e) => {
                self.status = format!("failed to start {}: {e}", pending.command.program);
                self.focus = Focus::List;
                self.active = None;
            }
        }
    }

    /// Keep the displayed PTY sized to the right pane (background PTYs are
    /// resized when they next become active).
    pub fn sync_pty_size(&mut self) {
        let (rows, cols) = (self.pty_rows, self.pty_cols);
        if let Some(id) = self.active.clone() {
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.resize(rows, cols);
            }
        }
    }

    pub fn detach_terminal(&mut self) {
        self.focus = Focus::List;
    }

    /// Detect children that have exited across ALL sessions. A finished session
    /// keeps its final frame (so output/errors stay readable); if it was the
    /// displayed one, focus returns to the list. Returns true if anything
    /// changed (needs redraw — e.g. the live ● dot).
    pub fn reap_pty(&mut self) -> bool {
        let mut newly_dead = Vec::new();
        for (id, pty) in self.ptys.iter_mut() {
            if !self.ended.contains(id) && !pty.is_alive() {
                newly_dead.push(id.clone());
            }
        }
        if newly_dead.is_empty() {
            return false;
        }
        for id in newly_dead {
            if self.active.as_deref() == Some(id.as_str()) {
                self.focus = Focus::List;
                self.status = "session ended — enter to relaunch".to_string();
            }
            self.ended.insert(id);
        }
        true
    }

    /// True (resetting) if the displayed PTY produced new output.
    pub fn pty_dirty(&self) -> bool {
        self.active_pty().is_some_and(|p| p.take_dirty())
    }

    /// Scroll the displayed session's scrollback (positive = older). Returns
    /// true if the view moved.
    pub fn scroll_active(&self, delta: isize) -> bool {
        self.active_pty().is_some_and(|p| p.scroll_by(delta))
    }

    /// Close the selected session: stop its PTY (if any) and archive it. A
    /// brand-new session with no disk file yet is simply dropped (nothing to
    /// archive).
    pub fn close_selected(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        // Remember a deliberate neighbor (the row that will slide under the
        // cursor) by id, so after the list shrinks the selection lands on it
        // instead of silently inheriting whatever shifted into the old index —
        // important because the next 'x' archives + SIGKILLs the selected row.
        let neighbor_id = self
            .visible
            .get(self.selected + 1)
            .or_else(|| {
                self.selected
                    .checked_sub(1)
                    .and_then(|i| self.visible.get(i))
            })
            .and_then(|&i| self.all_sessions.get(i))
            .map(|s| s.id.clone());
        if let Some(mut pty) = self.ptys.remove(&session.id) {
            pty.kill();
        }
        self.ended.remove(&session.id);
        if self.active.as_deref() == Some(session.id.as_str()) {
            self.active = None;
            self.focus = Focus::List;
        }
        if session.id.starts_with("new:") {
            // Synthetic placeholder (no rollout file): just remove it.
            self.extra_sessions.retain(|s| s.id != session.id);
            self.all_sessions.retain(|s| s.id != session.id);
            self.new_baselines.remove(&session.id);
            self.status = "closed new session".to_string();
        } else {
            self.state.set_archived(&session.id, true);
            let _ = self.state.save();
            if let Some(s) = self.all_sessions.iter_mut().find(|s| s.id == session.id) {
                s.archived = true;
            }
            self.status = format!("archived {}", short(&session.id));
        }
        self.rebuild_visible();
        // Restore the cursor onto the remembered neighbor by id.
        if let Some(nid) = neighbor_id {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == nid)
            {
                self.selected = pos;
            }
        }
    }

    /// Forward encoded keystrokes to the displayed PTY.
    pub fn send_to_pty(&mut self, bytes: &[u8]) {
        if let Some(id) = self.active.clone() {
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.send(bytes);
            }
        }
    }

    /// Forward pasted text to the displayed PTY (only when a live session has
    /// focus). Returns true if it was delivered (caller redraws). Pastes go
    /// nowhere useful from the list, so they're ignored there.
    pub fn paste_to_pty(&mut self, text: &str) -> bool {
        if self.focus != Focus::Terminal {
            return false;
        }
        if let Some(id) = self.active.clone() {
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.paste(text);
                return true;
            }
        }
        false
    }

    pub fn quit(&mut self) {
        for (_, mut pty) in self.ptys.drain() {
            pty.kill();
        }
        self.should_quit = true;
    }

    /// One-line summary used in the status bar. Totals the *visible* rows so the
    /// count and tokens match the list on screen (not the full scan, which also
    /// counts archived + sub-agent sessions and is shown on the scan screen).
    pub fn summary_line(&self) -> String {
        let a = &self.visible_aggregate;
        // Only mention kiro once there are kiro sessions, to keep the bar short.
        // Kiro token counts aren't read from its log, so show "—" not "0".
        let kiro = if a.kiro_count > 0 {
            " · kiro —".to_string()
        } else {
            String::new()
        };
        format!(
            "{} sessions · {} tok (codex {} · claude {}{}) · {}",
            a.session_count(),
            human_tokens(a.total.total),
            human_tokens(a.codex.total),
            human_tokens(a.claude.total),
            kiro,
            self.scope_label(),
        )
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mindplayer_core::session::TokenUsage;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Serializes tests that set the process-global `MINDPLAYER_STATE` env var,
    /// so concurrent tests can't clobber each other's sidecar path.
    static STATE_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn session(id: &str, agent: Agent, archived: bool) -> Session {
        Session {
            id: id.into(),
            agent,
            cwd: PathBuf::new(),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            tokens: TokenUsage::default(),
            title: id.into(),
            archived,
            is_subagent: false,
            context_pct: None,
        }
    }

    fn app_with(sessions: Vec<Session>) -> App {
        let mut app = App::new();
        app.all_sessions = sessions;
        app.rebuild_visible();
        app
    }

    #[test]
    fn new_session_persists_then_reconciles() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-newstate-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = App::new();
        app.scope = Scope::WorkingDir(PathBuf::from("/work"));

        // New labeled session shows up immediately (no disk file yet).
        app.request_new(Agent::Codex, "deploy check");
        assert_eq!(app.visible.len(), 1);
        let syn = app.session_at(0).unwrap();
        assert!(syn.id.starts_with("new:"));
        assert_eq!(syn.title, "🏷 deploy check");

        // A later scan discovers the real session (same agent/cwd, started after).
        let real = Session {
            id: "real-1234".into(),
            agent: Agent::Codex,
            cwd: PathBuf::from("/work"),
            file: PathBuf::new(),
            started_at: Some(chrono::Utc::now()),
            last_active: Some(chrono::Utc::now()),
            tokens: TokenUsage::default(),
            title: "deploy check".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        app.all_sessions = vec![real];
        app.merge_extras();
        app.rebuild_visible();

        // Placeholder reconciled away; the real session remains.
        assert!(app.extra_sessions.is_empty());
        assert!(app.all_sessions.iter().all(|s| !s.id.starts_with("new:")));
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.session_at(0).unwrap().id, "real-1234");

        let _ = std::fs::remove_file(&tmp);
        std::env::remove_var("MINDPLAYER_STATE");
    }

    #[test]
    fn new_session_stays_until_reconciled() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-newstate2-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = App::new();
        app.scope = Scope::WorkingDir(PathBuf::from("/work"));
        app.request_new(Agent::Claude, "");

        // A scan that finds nothing matching must NOT drop the new session.
        app.all_sessions = vec![session("unrelated", Agent::Codex, false)];
        app.merge_extras();
        app.rebuild_visible();
        assert_eq!(app.extra_sessions.len(), 1);
        assert!(app
            .all_sessions
            .iter()
            .any(|s| s.id.starts_with("new:claude")));

        std::env::remove_var("MINDPLAYER_STATE");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn visible_excludes_archived_by_default() {
        let app = app_with(vec![
            session("a", Agent::Codex, false),
            session("b", Agent::Claude, true),
        ]);
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.session_at(0).unwrap().id, "a");
    }

    #[test]
    fn toggle_archived_view_swaps_set() {
        let mut app = app_with(vec![
            session("a", Agent::Codex, false),
            session("b", Agent::Claude, true),
        ]);
        app.toggle_archived_view();
        assert!(app.show_archived);
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.session_at(0).unwrap().id, "b");
    }

    #[test]
    fn move_selection_wraps() {
        let mut app = app_with(vec![
            session("a", Agent::Codex, false),
            session("b", Agent::Codex, false),
        ]);
        assert_eq!(app.selected, 0);
        app.move_selection(-1);
        assert_eq!(app.selected, 1, "wrap to last");
        app.move_selection(1);
        assert_eq!(app.selected, 0, "wrap to first");
    }

    #[test]
    fn close_selected_archives_and_hides() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Redirect the sidecar write to a temp file so real state is untouched.
        let tmp = std::env::temp_dir().join(format!("mp-state-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = app_with(vec![
            session("a", Agent::Codex, false),
            session("b", Agent::Codex, false),
        ]);
        app.selected = 0;
        app.close_selected();

        let saved = mindplayer_core::State::load_from(&tmp);
        assert!(saved.is_archived("a"), "archive persisted to sidecar");
        let _ = std::fs::remove_file(&tmp);
        std::env::remove_var("MINDPLAYER_STATE");
        assert!(
            app.all_sessions
                .iter()
                .find(|s| s.id == "a")
                .unwrap()
                .archived
        );
        assert!(app.visible.iter().all(|&i| app.all_sessions[i].id != "a"));
    }

    #[test]
    fn merge_extras_ignores_preexisting_session() {
        // Regression for the HIGH bug: a new session must never be reconciled
        // onto a session that already existed when it was created (e.g. one the
        // user just resumed in the same dir) — doing so would re-key its live
        // PTY over the running one and silently kill it.
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-merge-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let now = chrono::Utc::now();
        let pre = Session {
            id: "pre-real".into(),
            agent: Agent::Codex,
            cwd: PathBuf::from("/work"),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            tokens: TokenUsage::default(),
            title: "already running".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };

        let mut app = App::new();
        app.scope = Scope::WorkingDir(PathBuf::from("/work"));
        app.all_sessions = vec![pre.clone()];
        app.rebuild_visible();

        // User starts a brand-new session in the SAME dir/agent.
        app.request_new(Agent::Codex, "");
        // A rescan: the new session's rollout file isn't on disk yet, so the
        // scan still only sees the pre-existing session.
        app.all_sessions = vec![pre];
        app.merge_extras();

        // The synthetic survives (not adopted onto the pre-existing session).
        assert_eq!(
            app.extra_sessions.len(),
            1,
            "new session not reconciled away"
        );
        assert!(app.all_sessions.iter().any(|s| s.id.starts_with("new:")));
        assert!(app.all_sessions.iter().any(|s| s.id == "pre-real"));

        let _ = std::fs::remove_file(&tmp);
        std::env::remove_var("MINDPLAYER_STATE");
    }

    #[test]
    fn close_selected_keeps_cursor_on_neighbor() {
        // Regression: after archiving a middle row the cursor must land on a
        // deliberate neighbor by id, so a repeated 'x' can't archive+kill a
        // session the user never moved onto.
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-neigh-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = app_with(vec![
            session("a", Agent::Codex, false),
            session("b", Agent::Codex, false),
            session("c", Agent::Codex, false),
        ]);
        app.selected = 1; // "b"
        app.close_selected();
        // "b" archived → visible [a, c]; cursor lands on the next neighbor "c".
        assert_eq!(app.selected_session().unwrap().id, "c");

        // Closing the last row falls back to the previous neighbor.
        app.selected = app
            .visible
            .iter()
            .position(|&i| app.all_sessions[i].id == "c")
            .unwrap();
        app.close_selected();
        assert_eq!(app.selected_session().unwrap().id, "a");

        let _ = std::fs::remove_file(&tmp);
        std::env::remove_var("MINDPLAYER_STATE");
    }

    #[test]
    fn label_edit_sets_and_persists() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-label-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = app_with(vec![session("real-1", Agent::Codex, false)]);
        app.selected = 0;
        app.begin_label_edit();
        assert_eq!(app.label_target.as_deref(), Some("real-1"));
        assert_eq!(app.new_label.as_deref(), Some(""), "no existing label");

        for c in "deploy check".chars() {
            app.label_input_push(c);
        }
        app.confirm_label_edit();

        assert!(app.label_target.is_none() && app.new_label.is_none());
        assert_eq!(app.all_sessions[0].title, "🏷 deploy check");
        let saved = mindplayer_core::State::load_from(&tmp);
        assert_eq!(saved.label_for("real-1"), Some("deploy check"));

        // Re-opening pre-fills the existing label so it can be edited/cleared.
        app.begin_label_edit();
        assert_eq!(app.new_label.as_deref(), Some("deploy check"));

        let _ = std::fs::remove_file(&tmp);
        std::env::remove_var("MINDPLAYER_STATE");
    }

    #[test]
    fn label_edit_skips_synthetic_placeholder() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-labelsyn-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = App::new();
        app.scope = Scope::WorkingDir(PathBuf::from("/work"));
        app.request_new(Agent::Codex, "");
        app.selected = 0; // the synthetic new: row
        app.begin_label_edit();
        // Synthetic placeholders use the new-session label flow, not this modal.
        assert!(app.label_target.is_none());
        assert!(app.new_label.is_none());

        let _ = std::fs::remove_file(&tmp);
        std::env::remove_var("MINDPLAYER_STATE");
    }
}
