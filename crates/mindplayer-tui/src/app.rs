//! Application state machine: scope select -> scanning -> main, plus the
//! embedded PTY lifecycle and archive actions.

use crate::{handoff, orchestration, pty::PtySession};
use chrono::{DateTime, Utc};
use mindplayer_core::{
    refresh_activity_and_usage, resume, scan, sort_by_recency, tokens::human_tokens,
    touched_recently, Agent, Aggregate, ScanConfig, Scope, Session, State, TokenUsage,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
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

pub const MAX_PANES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneLayout {
    Single,
    Horizontal,
    Vertical,
}

/// A drag-to-copy text selection inside ONE live pane, in pane-relative 0-based
/// cells. Scoped to a single pane so copying never bleeds into a neighbor pane
/// (the whole point — native terminal selection spans the full row across the
/// split; this does not).
#[derive(Debug, Clone)]
pub struct PaneSelection {
    pub pane_id: String,
    /// (row, col) where the drag started.
    pub anchor: (u16, u16),
    /// (row, col) of the current drag end.
    pub cursor: (u16, u16),
}

impl PaneSelection {
    /// Normalized row-major bounds `(start_row, start_col, end_row, end_col)`,
    /// inclusive of both endpoint cells.
    pub fn bounds(&self) -> (u16, u16, u16, u16) {
        let (a, c) = (self.anchor, self.cursor);
        let (sr, sc, er, ec) = if a <= c {
            (a.0, a.1, c.0, c.1)
        } else {
            (c.0, c.1, a.0, a.1)
        };
        (sr, sc, er, ec)
    }
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

fn agent_rank(agent: Agent) -> u8 {
    match agent {
        Agent::Codex => 0,
        Agent::Claude => 1,
        Agent::Kiro => 2,
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
const INITIAL_INPUT_OUTPUT_TIMEOUT: Duration = Duration::from_secs(3);
const INITIAL_INPUT_ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(10);
const ORCHESTRATION_MARKER_READ_BYTES: u64 = 1024 * 1024;
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

fn should_send_initial_input(looks_idle: bool, output_seq: u64, queued_for: Duration) -> bool {
    looks_idle
        || (output_seq > 0 && queued_for >= INITIAL_INPUT_OUTPUT_TIMEOUT)
        || queued_for >= INITIAL_INPUT_ABSOLUTE_TIMEOUT
}

fn should_stamp_activity(turn_submitted: bool, looks_busy: bool) -> bool {
    turn_submitted || looks_busy
}

fn input_submits_turn(bytes: &[u8]) -> bool {
    bytes.iter().any(|b| matches!(b, b'\r' | b'\n'))
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
    /// Number of leading `visible` rows touched within the last 24h (the rest
    /// are older). Computed in `rebuild_visible` so the list renderer draws
    /// the section headers from one source of truth.
    pub recent_count: usize,
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
    /// Multi-select mode (toggled with `v`). Only in this mode does Space mark
    /// sessions; a plain Enter outside it always opens a single session. Keeps
    /// the default one-session-at-a-time flow free of accidental multi-launch.
    pub multi_select: bool,
    /// Session ids the user has marked in the list (Space) to launch together
    /// as live panes with a single key. Cleared after a bulk launch.
    pub marked: HashSet<String>,
    /// All concurrently-running (or recently-ended) sessions, keyed by id, so
    /// switching between sessions keeps the others running in the background.
    pub ptys: HashMap<String, PtySession>,
    /// The focused live pane id. Multi-pane state lives in `panes`/`focused`;
    /// this keeps legacy single-pane routing paths small.
    pub active: Option<String>,
    pub panes: Vec<String>,
    pub focused: usize,
    pub layout: PaneLayout,
    /// When true, the live view shows ONLY the focused pane at full size
    /// instead of the split grid — the small panes in a multi-pane split can be
    /// hard to read. Tab / ctrl-w still cycle which pane is focused (and shown)
    /// while zoomed; ctrl-z toggles back to the split view.
    pub zoomed: bool,
    pub pane_sizes: HashMap<String, (u16, u16)>,
    /// Per-pane inner terminal bounds `(x, y, rows, cols)` from the latest
    /// render. Drag-copy uses this to copy the pane under the mouse, not an
    /// adjacent pane sharing the same Ghostty row.
    pub pane_bounds: HashMap<String, (u16, u16, u16, u16)>,
    /// Active drag-to-copy selection inside the focused pane (None when idle).
    pub selection: Option<PaneSelection>,
    /// Text waiting to be pushed to the system clipboard (drained by the event
    /// loop, which writes the OSC 52 sequence to the terminal).
    pub pending_clipboard: Option<String>,
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
    /// Last time MindPlayer injected peer-lane context into a native session.
    /// This prevents repeated sync prompts while switching between lanes when
    /// no peer lane has changed.
    thread_sync_at: HashMap<String, DateTime<Utc>>,
    /// Initial prompt payloads waiting for a newly-spawned agent's input prompt.
    pending_initial_inputs: HashMap<String, DeferredInitialInput>,
    /// Set when a resume is requested; consumed once the right-pane size is known.
    pub pending: Option<PendingSpawn>,
    /// Additional PTY spawns queued behind `pending`, used by orchestration.
    pending_queue: VecDeque<PendingSpawn>,
    /// codex/claude picker for a new session; None when hidden.
    pub new_picker: Option<usize>,
    /// Target-agent picker for a cross-agent handoff.
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
    /// Orchestration wizard opened by `o`.
    pub orchestration: Option<orchestration::Draft>,
    /// Orchestration broadcast prompt opened by `b`.
    pub broadcast: Option<orchestration::BroadcastDraft>,
    /// Main-mediated dispatch prompt opened by `m`.
    pub dispatch: Option<orchestration::BroadcastDraft>,
    /// Manual dispatch-block paste prompt opened by `M`.
    pub dispatch_apply: Option<orchestration::BroadcastDraft>,
    /// Keyboard shortcut help overlay opened by `?`.
    pub help_visible: bool,
    /// Orchestration root waiting for child lanes to become idle before
    /// synthesis is sent to the main lane.
    pending_synthesis_root: Option<String>,
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
    orchestration_cycles: HashMap<String, u64>,

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
    pub focus_after_spawn: bool,
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
            recent_count: 0,
            show_archived: false,
            show_subagents: false,
            hero_visible: false,
            list_rows: 0,
            focus: Focus::List,
            multi_select: false,
            marked: HashSet::new(),
            ptys: HashMap::new(),
            active: None,
            panes: Vec::new(),
            focused: 0,
            layout: PaneLayout::Horizontal,
            zoomed: false,
            pane_sizes: HashMap::new(),
            pane_bounds: HashMap::new(),
            selection: None,
            pending_clipboard: None,
            ended: HashSet::new(),
            out_seq: HashMap::new(),
            out_at: HashMap::new(),
            turn_submitted: HashSet::new(),
            thread_sync_at: HashMap::new(),
            pending_initial_inputs: HashMap::new(),
            pending: None,
            pending_queue: VecDeque::new(),
            new_picker: None,
            handoff_picker: None,
            new_label: None,
            new_agent: None,
            label_target: None,
            dir_input: None,
            orchestration: None,
            broadcast: None,
            dispatch: None,
            dispatch_apply: None,
            help_visible: false,
            pending_synthesis_root: None,
            search_query: None,
            new_counter: 0,
            extra_sessions: Vec::new(),
            new_baselines: HashMap::new(),
            orchestration_cycles: HashMap::new(),
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
                Focus::Terminal => self.panes.is_empty() || self.active_pty().is_none(),
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

    pub fn scope_label(&self) -> String {
        match self.scope {
            Scope::Global => "global".to_string(),
            Scope::WorkingDir(_) => format!("working dir ({})", self.cwd.display()),
        }
    }

    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    /// Move from the scan summary into the main view.
    pub fn open_main(&mut self) {
        self.screen = Screen::Main;
    }

    // --- list management --------------------------------------------------

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

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        format!("{}…", s.chars().take(keep).collect::<String>())
    }
}

fn trim_submit(bytes: &mut Vec<u8>) {
    while matches!(bytes.last(), Some(b'\r' | b'\n')) {
        bytes.pop();
    }
}

mod handoff_sync;
mod modals;
mod orchestration_lanes;
mod orchestration_ui;
mod pane;
mod selection;
mod session_list;
mod spawn;

#[cfg(test)]
mod tests;
