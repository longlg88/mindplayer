//! Application state machine: scope select -> scanning -> main, plus the
//! embedded PTY lifecycle and archive actions.

use crate::{experimental_handoff, pty::PtySession};
use chrono::{DateTime, Utc};
use mindplayer_core::{
    refresh_activity_and_usage, resume, scan, sort_by_recency, tokens::human_tokens, Agent,
    Aggregate, ScanConfig, Scope, Session, State, TokenUsage,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// Background refresh result for one already-discovered session.
struct ActivityUpdate {
    id: String,
    last_active: Option<DateTime<Utc>>,
    tokens: TokenUsage,
    context_pct: Option<f64>,
}

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
    /// Live PTY paused at a confirm/approval prompt — needs the user. Most urgent.
    Blocked,
    /// Live PTY producing output right now.
    Working,
    /// Live PTY, but quiet (running, nothing happening).
    Idle,
    /// Child has exited; final frame kept.
    Ended,
    /// Not running inside MindPlayer (a history entry).
    Inactive,
}

/// Sort rank for the herdr-style rollup: most urgent (blocked/working) first,
/// finished/historical last. Recency breaks ties within a rank.
fn status_rank(s: SessionStatus) -> u8 {
    match s {
        SessionStatus::Blocked => 0,
        SessionStatus::Working => 1,
        SessionStatus::Idle => 2,
        SessionStatus::Inactive => 3,
        SessionStatus::Ended => 4,
    }
}

/// How long a session keeps reading as "working" after its last output.
/// Promotion to Working is instant (any new output stamps the time to now);
/// demotion to Idle waits out this hold. The gap between the two is the
/// hysteresis that stops sessions with bursty output from flipping
/// Working↔Idle and bouncing up and down the urgency-sorted list.
const WORKING_HOLD: Duration = Duration::from_secs(6);

/// How long a screen-text "busy" marker (interrupt hint / spinner / "still
/// running") is trusted after the last output. The busy/blocked flags are only
/// recomputed when new PTY bytes arrive, so a finished session that left a
/// marker on its final frame would otherwise read "working" forever. A genuinely
/// working agent ticks its elapsed-time spinner every second (well within this
/// window), so gating busy on recent output keeps live sessions "working" while
/// letting a long-quiet one fall back to idle/done.
const BUSY_TRUST: Duration = Duration::from_secs(20);
/// Whether a session counts as working, given when it last produced output.
/// Demotion is delayed by `hold` (see [`WORKING_HOLD`]); promotion is instant
/// because `last_output` is stamped to "now" the moment any output arrives.
fn working_within_hold(last_output: Option<Instant>, now: Instant, hold: Duration) -> bool {
    last_output.is_some_and(|t| now.saturating_duration_since(t) < hold)
}

fn classify_live_session_status(
    blocked: bool,
    idle: bool,
    busy: bool,
    last_output: Option<Instant>,
    now: Instant,
) -> SessionStatus {
    if blocked {
        // Quiet + a confirm/approval prompt on screen → waiting for you.
        SessionStatus::Blocked
    } else if busy && working_within_hold(last_output, now, BUSY_TRUST) {
        // A trusted busy marker (interrupt hint / spinner / "still running")
        // means the turn is active even if the input box is still visible.
        SessionStatus::Working
    } else if idle {
        // Prompt/input box is back. This overrides final fresh output because a
        // completed turn prints one last batch before becoming idle.
        SessionStatus::Idle
    } else if working_within_hold(last_output, now, WORKING_HOLD) {
        // Produced output within the hold window — treat as working even
        // through brief silences (hysteresis) so it doesn't bounce.
        SessionStatus::Working
    } else {
        SessionStatus::Idle
    }
}

fn should_send_initial_input(looks_idle: bool, _output_seq: u64, _queued_for: Duration) -> bool {
    looks_idle
}

fn should_stamp_activity(turn_submitted: bool, looks_busy: bool) -> bool {
    turn_submitted || looks_busy
}

fn input_submits_turn(bytes: &[u8]) -> bool {
    bytes.iter().any(|b| matches!(b, b'\r' | b'\n'))
}

fn matches_search(s: &Session, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || s.title.to_lowercase().contains(&query)
        || s.id.to_lowercase().contains(&query)
        || s.agent.as_str().contains(&query)
}

fn handoff_label(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty() {
        None
    } else if label.starts_with("(handoff)") {
        Some(label.to_string())
    } else {
        Some(format!("(handoff){label}"))
    }
}

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
    /// Set by the list renderer each frame: true when the animated hero block
    /// (mascot) is actually on screen, so the loop only animates when useful.
    pub hero_visible: bool,
    /// Set by the list renderer each frame: the number of session rows visible
    /// in the list pane, used as the PageUp/PageDown step.
    pub list_rows: u16,

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
    /// Sessions where the user (or a handoff bootstrap) has submitted a turn.
    /// Initial TUI paint after opening a session is output too, but it should
    /// not make the row look like active work.
    turn_submitted: HashSet<String>,
    /// Initial prompt payloads waiting for a newly-spawned agent's input prompt.
    pending_initial_inputs: HashMap<String, DeferredInitialInput>,
    /// Set when a resume is requested; consumed once the right-pane size is known.
    pub pending: Option<PendingSpawn>,
    /// codex/claude picker for a new session; None when hidden.
    pub new_picker: Option<usize>,
    /// EXPERIMENTAL: target-agent picker for a cross-agent handoff.
    pub handoff_picker: Option<usize>,
    /// When `Some`, the label-input step of new-session creation is active and
    /// holds the text typed so far.
    pub new_label: Option<String>,
    /// Agent chosen in the picker, awaiting a label.
    pub new_agent: Option<Agent>,
    /// When `Some`, the label-input modal is editing an EXISTING session's label
    /// (this is its id) rather than creating a new session. Shares `new_label`
    /// as the text buffer.
    pub label_target: Option<String>,
    /// When `Some`, the working-dir input modal is open and holds the path text
    /// typed so far. Confirming re-points the scope at that directory.
    pub dir_input: Option<String>,
    /// When `Some`, the session list is filtered as the user types after `/`.
    pub search_query: Option<String>,
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
    /// Last known top-left origin of the live pane's inner area, for translating
    /// absolute mouse coordinates into pane-relative ones when forwarding to the
    /// child.
    pub pty_x: u16,
    pub pty_y: u16,

    scan_rx: Option<Receiver<Vec<Session>>>,
    /// In-flight background usage refresh, kept off the main thread so periodic
    /// token updates and re-sort never stall input/rendering.
    refresh_rx: Option<Receiver<Vec<ActivityUpdate>>>,
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
    pub initial_input: Option<Vec<u8>>,
}

struct DeferredInitialInput {
    bytes: Vec<u8>,
    queued_at: Instant,
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
            hero_visible: false,
            list_rows: 0,
            focus: Focus::List,
            ptys: HashMap::new(),
            active: None,
            ended: HashSet::new(),
            out_seq: HashMap::new(),
            out_at: HashMap::new(),
            turn_submitted: HashSet::new(),
            pending_initial_inputs: HashMap::new(),
            pending: None,
            new_picker: None,
            handoff_picker: None,
            new_label: None,
            new_agent: None,
            label_target: None,
            dir_input: None,
            search_query: None,
            new_counter: 0,
            extra_sessions: Vec::new(),
            new_baselines: HashMap::new(),
            pty_rows: 24,
            pty_cols: 80,
            pty_x: 0,
            pty_y: 0,
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
            // Only animate when something is actually moving: the list's hero
            // block (when shown), or the idle mascot in an empty live pane.
            Screen::Main => match self.focus {
                Focus::List => self.hero_visible,
                Focus::Terminal => self.active_pty().is_none(),
            },
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
        self.handoff_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.label_target = None;
    }

    // --- experimental cross-agent handoff ----------------------------------

    pub fn begin_handoff(&mut self) {
        if !experimental_handoff::enabled() {
            self.status = format!(
                "experimental handoff is disabled (set {}=1)",
                experimental_handoff::ENV_FLAG
            );
            return;
        }
        if self.selected_session().is_none() {
            return;
        }
        self.handoff_picker = Some(0);
    }

    pub fn cancel_handoff(&mut self) {
        self.handoff_picker = None;
    }

    pub fn confirm_handoff(&mut self, target: Agent) {
        let Some(source) = self.selected_session().cloned() else {
            self.handoff_picker = None;
            return;
        };
        self.handoff_picker = None;
        if !experimental_handoff::enabled() {
            self.status = format!(
                "experimental handoff is disabled (set {}=1)",
                experimental_handoff::ENV_FLAG
            );
            return;
        }
        if source.agent == target {
            self.status = format!("handoff target is already {}", target.as_str());
            return;
        }

        let prepared = match experimental_handoff::prepare_initial_input(&source, target) {
            Ok(prepared) => prepared,
            Err(e) => {
                self.status = format!("handoff failed: {e}");
                return;
            }
        };
        let command = experimental_handoff::command_for(&source, target);
        self.new_counter += 1;
        let session_id = format!(
            "handoff:{}:{}:{}",
            source.agent.as_str(),
            target.as_str(),
            self.new_counter
        );
        let baseline: HashSet<String> = self
            .all_sessions
            .iter()
            .filter(|s| !s.id.starts_with("new:") && !s.id.starts_with("handoff:"))
            .map(|s| s.id.clone())
            .collect();
        self.new_baselines.insert(session_id.clone(), baseline);
        let handoff_label = self.state.label_for(&source.id).and_then(handoff_label);
        self.pending = Some(PendingSpawn {
            command,
            session_id: session_id.clone(),
            initial_input: Some(prepared.input),
        });
        self.active = Some(session_id.clone());
        self.focus = Focus::Terminal;

        let now = Utc::now();
        let synthetic = Session {
            id: session_id,
            agent: target,
            cwd: source.cwd.clone(),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            tokens: Default::default(),
            title: handoff_label
                .as_ref()
                .map(|label| format!("🏷 {label}"))
                .unwrap_or_else(|| experimental_handoff::title_for(&source, target)),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        self.extra_sessions.push(synthetic.clone());
        self.all_sessions.push(synthetic);
        self.rebuild_visible();
        if let Some(id) = self.active.clone() {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }
        let trunc = if prepared.inline_truncated {
            "artifact only"
        } else {
            "full inline"
        };
        if let Some(label) = &handoff_label {
            self.state.add_pending_label(
                target.as_str(),
                source.cwd.clone(),
                now - chrono::Duration::seconds(5),
                label,
            );
            let _ = self.state.save();
        }
        self.status = format!(
            "experimental handoff {} -> {} ({} chars, {trunc}, {})",
            source.agent.as_str(),
            target.as_str(),
            prepared.transcript_chars,
            prepared.artifact.display()
        );
        self.rescan_due = Some(Instant::now() + Duration::from_secs(3));
    }

    // --- session search ----------------------------------------------------

    pub fn begin_search(&mut self) {
        self.search_query = Some(String::new());
        self.rebuild_visible();
    }

    pub fn search_push(&mut self, c: char) {
        if let Some(query) = self.search_query.as_mut() {
            query.push(c);
            self.rebuild_visible();
        }
    }

    pub fn search_backspace(&mut self) {
        if let Some(query) = self.search_query.as_mut() {
            query.pop();
            self.rebuild_visible();
        }
    }

    pub fn cancel_search(&mut self) {
        self.search_query = None;
        self.rebuild_visible();
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

    // --- working-dir input ------------------------------------------------

    /// Open the working-dir modal, pre-filled with the current directory so it
    /// can be edited or replaced.
    pub fn begin_dir_input(&mut self) {
        self.dir_input = Some(self.cwd.display().to_string());
    }

    pub fn dir_input_push(&mut self, c: char) {
        if let Some(buf) = self.dir_input.as_mut() {
            buf.push(c);
        }
    }

    pub fn dir_input_backspace(&mut self) {
        if let Some(buf) = self.dir_input.as_mut() {
            buf.pop();
        }
    }

    pub fn cancel_dir_input(&mut self) {
        self.dir_input = None;
    }

    /// Confirm the working-dir modal: validate the path, re-point the scope at
    /// it, and kick a fresh scan in place. Invalid paths keep the modal open
    /// with an error in the status line. A blank entry switches to global scope.
    pub fn confirm_dir_input(&mut self) {
        let raw = self.dir_input.clone().unwrap_or_default();
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            self.scope = Scope::Global;
            self.dir_input = None;
            self.state.last_scope = Some(self.scope.label());
            let _ = self.state.save();
            self.status = "scope → global".to_string();
            self.start_bg_rescan();
            return;
        }

        let path = expand_tilde(trimmed);
        let resolved = path.canonicalize().unwrap_or(path);
        if !resolved.is_dir() {
            self.status = format!("not a directory: {}", resolved.display());
            return; // keep the modal open so the user can fix it
        }

        self.cwd = resolved.clone();
        self.scope = Scope::WorkingDir(resolved.clone());
        self.dir_input = None;
        self.state.last_scope = Some(self.scope.label());
        let _ = self.state.save();
        self.status = format!("working dir → {}", resolved.display());
        self.start_bg_rescan();
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
            .filter(|(_, s)| {
                self.search_query
                    .as_deref()
                    .is_none_or(|query| matches_search(s, query))
            })
            .map(|(i, _)| i)
            .collect();
        // herdr-style rollup: float the most urgent states to the top
        // (blocked → working → idle → history → done). Stable sort keeps the
        // recency order (from all_sessions) within each rank.
        let mut vis = std::mem::take(&mut self.visible);
        vis.sort_by_cached_key(|&i| status_rank(self.session_status(&self.all_sessions[i].id)));
        self.visible = vis;
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

    /// Move the selection by a small page step (PageUp/PageDown). Unlike
    /// single-step movement this clamps at the ends instead of wrapping.
    pub fn move_page(&mut self, dir: isize) {
        if self.visible.is_empty() {
            return;
        }
        let page = 4;
        let last = self.visible.len() as isize - 1;
        let next = (self.selected as isize + dir * page).clamp(0, last);
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

    /// Kick off a background usage refresh (no-op if one is already running).
    /// File stats and token parsing happen off the main thread so input and
    /// rendering never stall; results are applied in [`Self::poll_refresh`].
    pub fn start_refresh(&mut self) {
        if self.refresh_rx.is_some() || self.all_sessions.is_empty() {
            return;
        }
        let mut sessions = self.all_sessions.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            refresh_activity_and_usage(&mut sessions);
            let out: Vec<ActivityUpdate> = sessions
                .into_iter()
                .map(|s| ActivityUpdate {
                    id: s.id,
                    last_active: s.last_active,
                    tokens: s.tokens,
                    context_pct: s.context_pct,
                })
                .collect();
            let _ = tx.send(out);
        });
        self.refresh_rx = Some(rx);
    }

    /// Apply a finished background refresh: update activity/usage, re-sort
    /// newest-first, and keep the cursor on the same session by id. Returns true
    /// if the list changed (needs redraw).
    pub fn poll_refresh(&mut self) -> bool {
        let Some(rx) = &self.refresh_rx else {
            return false;
        };
        let Ok(updates) = rx.try_recv() else {
            return false;
        };
        self.refresh_rx = None;

        let updates: HashMap<String, ActivityUpdate> =
            updates.into_iter().map(|u| (u.id.clone(), u)).collect();
        for s in self.all_sessions.iter_mut() {
            if let Some(update) = updates.get(&s.id) {
                s.last_active = update.last_active;
                s.tokens = update.tokens;
                s.context_pct = update.context_pct;
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
            .is_some_and(|id| self.ptys.contains_key(id) && !self.ended.contains(id))
    }

    pub fn live_pty_count(&self) -> usize {
        self.ptys
            .keys()
            .filter(|id| !self.ended.contains(*id))
            .count()
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
            if self.ended.contains(id) {
                continue;
            }
            let seq = pty.output_seq();
            if self.out_seq.get(id) != Some(&seq) {
                self.out_seq.insert(id.clone(), seq);
                if should_stamp_activity(self.turn_submitted.contains(id), pty.looks_busy()) {
                    self.out_at.insert(id.clone(), now);
                }
                changed = true;
            }
        }
        self.out_seq
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.out_at
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.turn_submitted
            .retain(|id| self.ptys.contains_key(id) && !self.ended.contains(id));
        changed
    }

    /// True while any session is within its "working" window — the loop uses
    /// this to keep redrawing so a badge can decay from working → idle even
    /// with no new events.
    pub fn any_recent_activity(&self) -> bool {
        self.out_at
            .iter()
            .any(|(id, t)| !self.ended.contains(id) && t.elapsed() < WORKING_HOLD)
    }

    /// Status of session `id` for the list badge.
    pub fn session_status(&self, id: &str) -> SessionStatus {
        if self.ended.contains(id) {
            SessionStatus::Ended
        } else if let Some(pty) = self.ptys.get(id) {
            classify_live_session_status(
                pty.looks_blocked(),
                pty.looks_idle(),
                pty.looks_busy(),
                self.out_at.get(id).copied(),
                Instant::now(),
            )
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
            initial_input: None,
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
            initial_input: None,
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
        // The synthetic row is Inactive, so the urgency sort sinks it down the
        // list. Keep the cursor on it by id so returning to the list and
        // pressing `x` can't archive+kill a different session.
        if let Some(id) = self.active.clone() {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }

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
                    if let Some(input) = self.pending_initial_inputs.remove(&extra.id) {
                        self.pending_initial_inputs.insert(real_id.clone(), input);
                    }
                    if self.turn_submitted.remove(&extra.id) {
                        self.turn_submitted.insert(real_id.clone());
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
        self.pending_initial_inputs.remove(&id);
        self.out_seq.remove(&id);
        self.out_at.remove(&id);
        self.turn_submitted.remove(&id);
        match PtySession::spawn(&pending.command, &id, self.pty_rows, self.pty_cols) {
            Ok(pty) => {
                if let Some(input) = pending.initial_input {
                    self.pending_initial_inputs.insert(
                        id.clone(),
                        DeferredInitialInput {
                            bytes: input,
                            queued_at: Instant::now(),
                        },
                    );
                }
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

    /// Submit queued first-turn prompts only after the child has rendered an
    /// input prompt. Sending immediately after spawn can race the agent TUI
    /// startup and lose the handoff prompt before it reaches the transcript.
    pub fn flush_initial_inputs(&mut self) -> bool {
        if self.pending_initial_inputs.is_empty() {
            return false;
        }
        let now = Instant::now();
        let ready: Vec<String> = self
            .pending_initial_inputs
            .iter()
            .filter_map(|(id, input)| {
                let pty = self.ptys.get(id)?;
                if self.ended.contains(id) {
                    return None;
                }
                should_send_initial_input(
                    pty.looks_idle(),
                    pty.output_seq(),
                    now.saturating_duration_since(input.queued_at),
                )
                .then(|| id.clone())
            })
            .collect();
        let mut sent = false;
        for id in ready {
            let Some(input) = self.pending_initial_inputs.remove(&id) else {
                continue;
            };
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.paste_and_submit(&input.bytes);
                self.turn_submitted.insert(id.clone());
                self.status = format!("submitted handoff context to {}", short(&id));
                sent = true;
            }
        }
        sent
    }

    fn active_initial_input_pending(&mut self) -> bool {
        let Some(id) = self.active.as_ref() else {
            return false;
        };
        if !self.pending_initial_inputs.contains_key(id) {
            return false;
        }
        self.status =
            "waiting for target prompt to submit handoff context; input is held".to_string();
        true
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
                // The leader just exited; clean up its group (MCP / language
                // servers) now, while the pgid is still alive, instead of
                // orphaning them.
                pty.signal_group();
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

    /// Whether the displayed child has xterm mouse reporting on — if so, mouse
    /// events are forwarded to it instead of scrolling MindPlayer's scrollback.
    pub fn active_wants_mouse(&self) -> bool {
        self.active_pty().is_some_and(|p| p.mouse_wanted())
    }

    /// Translate an absolute terminal cell (from a mouse event) into 1-based
    /// coordinates relative to the live pane's inner area, clamped to it.
    pub fn pane_relative(&self, col: u16, row: u16) -> (u16, u16) {
        let c = col
            .saturating_sub(self.pty_x)
            .min(self.pty_cols.saturating_sub(1))
            + 1;
        let r = row
            .saturating_sub(self.pty_y)
            .min(self.pty_rows.saturating_sub(1))
            + 1;
        (c, r)
    }

    /// Forward a (pane-relative) mouse event to the displayed child. Returns
    /// true if a sequence was sent (caller redraws).
    pub fn forward_mouse_to_pty(
        &mut self,
        cb: u16,
        release: bool,
        motion: bool,
        col: u16,
        row: u16,
    ) -> bool {
        if let Some(id) = self.active.clone() {
            if let Some(pty) = self.ptys.get_mut(&id) {
                return pty.forward_mouse(cb, release, motion, col, row);
            }
        }
        false
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
        self.pending_initial_inputs.remove(&session.id);
        self.turn_submitted.remove(&session.id);
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
        if self.active_initial_input_pending() {
            return;
        }
        if let Some(id) = self.active.clone() {
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.send(bytes);
                if input_submits_turn(bytes) {
                    self.turn_submitted.insert(id);
                }
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
        if self.active_initial_input_pending() {
            return true;
        }
        if let Some(id) = self.active.clone() {
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.paste(text);
                if input_submits_turn(text.as_bytes()) {
                    self.turn_submitted.insert(id);
                }
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

/// Expand a leading `~` / `~/` to the user's home directory. Other paths are
/// returned unchanged (relative paths resolve against the process cwd later).
fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(input)
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

    fn session_in(id: &str, agent: Agent, cwd: &str, title: &str) -> Session {
        Session {
            id: id.into(),
            agent,
            cwd: PathBuf::from(cwd),
            file: PathBuf::new(),
            started_at: Some(chrono::Utc::now()),
            last_active: Some(chrono::Utc::now()),
            tokens: TokenUsage::default(),
            title: title.into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    fn write_handoff_fixture(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "mindplayer-app-handoff-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("claude.jsonl");
        std::fs::write(
            &transcript,
            r#"{"type":"user","message":{"role":"user","content":"continue deploy investigation"}}
{"type":"assistant","message":{"role":"assistant","content":"I found the failing health check in deploy.yaml."}}"#,
        )
        .unwrap();
        (dir, transcript)
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
    fn refresh_applies_token_updates_to_existing_row() {
        let mut app = app_with(vec![session("s1", Agent::Codex, false)]);
        assert_eq!(app.session_at(0).unwrap().tokens.total, 0);

        let (tx, rx) = mpsc::channel();
        tx.send(vec![ActivityUpdate {
            id: "s1".into(),
            last_active: Some(chrono::Utc::now()),
            tokens: TokenUsage {
                input: 7,
                cached: 2,
                output: 3,
                total: 10,
            },
            context_pct: None,
        }])
        .unwrap();
        app.refresh_rx = Some(rx);

        assert!(app.poll_refresh());
        assert_eq!(app.session_at(0).unwrap().tokens.total, 10);
        assert_eq!(app.visible_aggregate.codex.total, 10);
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
    fn search_filters_visible_sessions_by_label_or_title() {
        let mut labeled = session("a", Agent::Codex, false);
        labeled.title = "🏷 msk cohome".into();
        let mut titled = session("b", Agent::Claude, false);
        titled.title = "deploy rollback notes".into();
        let mut app = app_with(vec![labeled, titled]);

        app.begin_search();
        for c in "msk".chars() {
            app.search_push(c);
        }

        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.session_at(0).unwrap().id, "a");

        for _ in 0.."msk".len() {
            app.search_backspace();
        }
        for c in "rollback".chars() {
            app.search_push(c);
        }

        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.session_at(0).unwrap().id, "b");

        app.cancel_search();
        assert_eq!(app.visible.len(), 2);
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
    fn move_page_steps_and_clamps() {
        let mut app = app_with(
            (0..20)
                .map(|i| session(&format!("s{i}"), Agent::Codex, false))
                .collect(),
        );
        app.list_rows = 10; // PageUp/PageDown use a fixed 4-row step.
        assert_eq!(app.selected, 0);
        app.move_page(1);
        assert_eq!(app.selected, 4, "down one page step");
        app.move_page(1);
        assert_eq!(app.selected, 8, "down another page step");
        for _ in 0..4 {
            app.move_page(1);
        }
        assert_eq!(app.selected, 19, "clamp at last (no wrap)");
        app.move_page(-1);
        assert_eq!(app.selected, 15, "up one page step from last");
        app.move_page(-1);
        assert_eq!(app.selected, 11, "up another page step");
        app.move_page(-1);
        app.move_page(-1);
        app.move_page(-1);
        assert_eq!(app.selected, 0, "clamp at first (no wrap)");
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

    #[test]
    fn handoff_is_hidden_without_env_flag() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _handoff_env = experimental_handoff::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(experimental_handoff::ENV_FLAG);
        let mut app = app_with(vec![session_in(
            "claude-1",
            Agent::Claude,
            "/work",
            "finish deployment",
        )]);

        app.begin_handoff();

        assert!(app.handoff_picker.is_none());
        assert!(app.status.contains(experimental_handoff::ENV_FLAG));
    }

    #[test]
    fn handoff_queues_target_agent_with_initial_prompt() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _handoff_env = experimental_handoff::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp =
            std::env::temp_dir().join(format!("mp-handoff-label-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);
        std::env::set_var(experimental_handoff::ENV_FLAG, "1");
        let (dir, transcript) = write_handoff_fixture("queue");
        std::env::set_var(experimental_handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
        let mut source = session_in(
            "claude-1",
            Agent::Claude,
            "/work/project",
            "finish deployment",
        );
        source.file = transcript;
        let mut app = app_with(vec![source]);
        app.state.set_label("claude-1", "msk cohome");

        app.begin_handoff();
        assert_eq!(app.handoff_picker, Some(0));
        app.confirm_handoff(Agent::Codex);

        let pending = app.pending.as_ref().expect("handoff queues PTY spawn");
        assert!(pending.session_id.starts_with("handoff:claude:codex:"));
        assert_eq!(pending.command.program, "codex");
        assert_eq!(pending.command.cwd, PathBuf::from("/work/project"));
        let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
        assert!(input.contains("from claude to codex"));
        assert!(input.contains("session id: claude-1"));
        assert!(input.contains("read the handoff artifact"));
        assert!(input.contains("continue deploy investigation"));
        assert!(input.contains("failing health check"));
        assert!(input.ends_with('\r'));
        assert!(app.all_sessions.iter().any(
            |s| s.id.starts_with("handoff:claude:codex:") && s.title == "🏷 (handoff)msk cohome"
        ));
        assert!(app.state.pending_labels.iter().any(|p| p.agent == "codex"
            && p.cwd == std::path::Path::new("/work/project")
            && p.label == "(handoff)msk cohome"));

        std::env::remove_var(experimental_handoff::ENV_FLAG);
        std::env::remove_var(experimental_handoff::HANDOFF_DIR_ENV);
        std::env::remove_var("MINDPLAYER_STATE");
    }

    #[test]
    fn status_rank_orders_by_urgency() {
        // herdr-style rollup: most urgent (blocked) first, finished (done) last.
        use SessionStatus::*;
        assert!(status_rank(Blocked) < status_rank(Working));
        assert!(status_rank(Working) < status_rank(Idle));
        assert!(status_rank(Idle) < status_rank(Inactive));
        assert!(status_rank(Inactive) < status_rank(Ended));
    }

    #[test]
    fn working_hold_keeps_status_through_brief_silence() {
        let now = Instant::now();
        // Just produced output → working.
        assert!(working_within_hold(Some(now), now, WORKING_HOLD));
        // Quiet for less than the hold → still working (hysteresis, no bounce).
        assert!(working_within_hold(
            Some(now - Duration::from_secs(3)),
            now,
            WORKING_HOLD
        ));
        // Quiet past the hold → no longer working.
        assert!(!working_within_hold(
            Some(now - WORKING_HOLD - Duration::from_secs(1)),
            now,
            WORKING_HOLD
        ));
        // Never produced output → not working.
        assert!(!working_within_hold(None, now, WORKING_HOLD));
    }

    #[test]
    fn trusted_busy_marker_overrides_visible_idle_prompt() {
        let now = Instant::now();

        assert_eq!(
            classify_live_session_status(
                false,
                true,
                true,
                Some(now - Duration::from_secs(1)),
                now
            ),
            SessionStatus::Working
        );
        assert_eq!(
            classify_live_session_status(
                false,
                true,
                true,
                Some(now - BUSY_TRUST - Duration::from_secs(1)),
                now
            ),
            SessionStatus::Idle
        );
    }

    #[test]
    fn idle_prompt_overrides_recent_non_busy_output() {
        let now = Instant::now();

        assert_eq!(
            classify_live_session_status(false, true, false, Some(now), now),
            SessionStatus::Idle
        );
        assert_eq!(
            classify_live_session_status(false, false, false, Some(now), now),
            SessionStatus::Working
        );
    }

    #[test]
    fn blocked_prompt_has_status_priority() {
        let now = Instant::now();

        assert_eq!(
            classify_live_session_status(true, true, true, Some(now), now),
            SessionStatus::Blocked
        );
    }

    #[test]
    fn initial_terminal_paint_does_not_count_as_working_activity() {
        assert!(!should_stamp_activity(false, false));
        assert!(should_stamp_activity(true, false));
        assert!(should_stamp_activity(false, true));
    }

    #[test]
    fn handoff_label_prefixes_once() {
        assert_eq!(
            handoff_label("msk cohome").as_deref(),
            Some("(handoff)msk cohome")
        );
        assert_eq!(
            handoff_label("(handoff)msk cohome").as_deref(),
            Some("(handoff)msk cohome")
        );
        assert_eq!(handoff_label("   "), None);
    }

    #[test]
    fn only_submit_keys_mark_user_turn_submitted() {
        assert!(!input_submits_turn(b"a"));
        assert!(!input_submits_turn(b"\x1b[A"));
        assert!(input_submits_turn(b"\r"));
        assert!(input_submits_turn(b"hello\n"));
    }

    #[test]
    fn initial_input_waits_for_prompt() {
        assert!(should_send_initial_input(
            true,
            0,
            Duration::from_millis(10)
        ));
        assert!(!should_send_initial_input(
            false,
            0,
            Duration::from_secs(30)
        ));
        assert!(!should_send_initial_input(
            false,
            1,
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn busy_marker_is_only_trusted_while_output_is_recent() {
        // A screen "busy" marker is frozen at the last output, so it must be
        // gated on output recency: trusted within BUSY_TRUST, ignored after.
        let now = Instant::now();
        assert!(
            BUSY_TRUST > WORKING_HOLD,
            "busy grace must exceed the work hold"
        );
        // Just-finished turn with a marker still on screen → trust it.
        assert!(working_within_hold(
            Some(now - Duration::from_secs(5)),
            now,
            BUSY_TRUST
        ));
        // Finished long ago (e.g. 6 min) with a stale marker → do NOT trust it,
        // so the session reads idle/done instead of "working" forever.
        assert!(!working_within_hold(
            Some(now - Duration::from_secs(360)),
            now,
            BUSY_TRUST
        ));
    }

    #[test]
    fn ended_sessions_do_not_keep_recent_activity_alive() {
        let mut app = App::new();
        app.ended.insert("done".into());
        app.out_at.insert("done".into(), Instant::now());

        assert!(
            !app.any_recent_activity(),
            "ended PTYs keep their final frame, but must not keep working redraws alive"
        );
    }

    #[test]
    fn dir_input_repoints_scope_to_valid_dir() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-dirstate-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        // A real directory that exists on every machine.
        let target = std::env::temp_dir();
        let mut app = App::new();
        app.begin_dir_input();
        assert!(app.dir_input.is_some());
        // Replace the prefilled buffer with the target path.
        app.dir_input = Some(target.display().to_string());
        app.confirm_dir_input();

        assert!(app.dir_input.is_none(), "modal closes on success");
        match &app.scope {
            Scope::WorkingDir(p) => {
                assert_eq!(p, &target.canonicalize().unwrap_or(target.clone()));
            }
            other => panic!("expected WorkingDir scope, got {other:?}"),
        }
    }

    #[test]
    fn dir_input_rejects_nonexistent_dir() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-dirstate2-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = App::new();
        let original = app.scope.clone();
        app.begin_dir_input();
        app.dir_input = Some("/no/such/path/mindplayer-xyz".to_string());
        app.confirm_dir_input();

        // Invalid path: scope unchanged and modal stays open for correction.
        assert!(app.dir_input.is_some(), "modal stays open on bad path");
        assert_eq!(format!("{:?}", app.scope), format!("{original:?}"));
    }

    #[test]
    fn dir_input_blank_switches_to_global() {
        let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mp-dirstate3-{}.json", std::process::id()));
        std::env::set_var("MINDPLAYER_STATE", &tmp);

        let mut app = App::new();
        app.begin_dir_input();
        app.dir_input = Some("   ".to_string());
        app.confirm_dir_input();

        assert!(app.dir_input.is_none());
        assert!(matches!(app.scope, Scope::Global));
    }
}
