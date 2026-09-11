//! Application state machine: scope select -> scanning -> main, plus the
//! embedded PTY lifecycle and archive actions.

use crate::{handoff, pty::PtySession, text_input};
use chrono::{DateTime, Utc};
use mindplayer_core::{
    refresh_activity_and_usage, resume, scan, sort_by_recency,
    tokens::{apportion, human_tokens},
    touched_recently, Agent, Aggregate, ScanConfig, Scope, Session, State, TokenUsage,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// One background `.html`-candidate sweep's results: per scanned pane id, that
/// pane's raw [`pane::scan_html_candidates`] output (`(path, mtime)` pairs).
/// Named so [`App::html_scan_rx`] and the sweep thread share one non-trivial
/// channel type without tripping `clippy::type_complexity`.
type HtmlScanBatch = Vec<(String, Vec<(PathBuf, SystemTime)>)>;

/// A finished category sync: the prompt to inject, plus the new watermark per
/// peer. Named so the channel type stays readable and `clippy::type_complexity`
/// stays quiet.
type CategorySyncResult = (
    String,
    Result<(handoff::PreparedHandoff, Vec<(String, u64)>), String>,
);

/// A finished conversation-log batch. The worker owns a clone of the index
/// while it runs and hands the whole thing back, so the UI thread never sees a
/// half-updated index.
pub(crate) struct ConvoIngestResult {
    pub(crate) index: crate::convo_log::ConvoIndex,
    pub(crate) turns: usize,
}

/// The open "copy a link" picker: the links found, which row the cursor is on,
/// and how far back the answer they came from was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPicker {
    pub links: Vec<String>,
    pub selected: usize,
    pub turns_ago: usize,
}

/// Background refresh result for one already-discovered session.
struct ActivityUpdate {
    id: String,
    last_active: Option<DateTime<Utc>>,
    last_prompt_at: Option<DateTime<Utc>>,
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

/// Not a UI preference — a multi-select launch can pick more sessions than
/// fit comfortably on screen (20+ is routine), and the point of multi-select
/// launch is to show ALL of them, not an arbitrary small slice. This is a
/// resource safety ceiling only (each pane spawns a real child process),
/// sized well above any observed real selection.
pub const MAX_PANES: usize = 32;

/// Cells the usage bar occupies. The footer's left half already carries the
/// status text and the working directory, so this is sized to read as a bar
/// while leaving those legible on a normal-width terminal.
pub const USAGE_BAR_CELLS: usize = 12;

/// One agent's share of the measured tokens.
///
/// `glyph` differs per agent on purpose: color alone would carry the whole
/// distinction, and the bar has to stay readable in a monochrome terminal and
/// for a viewer who cannot separate the hues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSegment {
    pub label: &'static str,
    pub glyph: char,
    pub cells: usize,
    pub percent: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneLayout {
    Single,
    Horizontal,
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

/// State of the `t` category picker. Targets are captured when it opens so a
/// background rescan mid-pick can't retarget the assignment.
#[derive(Debug, Clone)]
pub struct CategoryPicker {
    /// Session ids this pick will apply to — one row, or every marked row.
    pub targets: Vec<String>,
    /// Highlighted row: `0..n` existing categories, then new, then clear.
    pub selected: usize,
    /// `Some(buffer)` while typing a new category's name.
    pub new_name: Option<String>,
}

/// State of the category menu (`t` on a category header). Rows are fixed:
/// auto-sync, sync now, rename, remove.
#[derive(Debug, Clone)]
pub struct CategoryMenu {
    pub cat_id: String,
    pub selected: usize,
    /// `Some(buffer)` while typing a new name.
    pub rename: Option<String>,
    /// Removing drops the whole grouping, so it asks first.
    pub confirm_remove: bool,
}

impl CategoryMenu {
    pub const ROWS: usize = 4;
    pub const AUTO_SYNC: usize = 0;
    pub const SYNC_NOW: usize = 1;
    pub const RENAME: usize = 2;
    pub const REMOVE: usize = 3;
}

/// One row of the session list. Category headers are rows rather than decoration
/// so the cursor can land on them — that is what lets `←`/`→` fold a topic while
/// `→` on a session still resumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A category header. `None` is the "no category" bucket, only emitted once
    /// at least one real category exists (otherwise every row would sit under a
    /// pointless header).
    Header(Option<String>),
    /// Index into `all_sessions`.
    Session(usize),
}

impl Row {
    pub fn session_index(&self) -> Option<usize> {
        match self {
            Row::Session(i) => Some(*i),
            Row::Header(_) => None,
        }
    }

    pub fn is_header(&self) -> bool {
        matches!(self, Row::Header(_))
    }
}

/// How a session in the list is doing right now, for the status badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Live PTY paused at a confirm/approval prompt — needs the user.
    Blocked,
    /// Live PTY producing output right now.
    Working,
    /// Live PTY, but quiet (running, nothing happening).
    Idle,
    /// Child has exited; final frame kept. See [`status_rank`] — this is the
    /// single highest-priority status, ahead of even Blocked.
    Ended,
    /// Not running inside MindPlayer (a history entry).
    Inactive,
}

/// Sort rank for the herdr-style rollup: a session that just finished (Ended)
/// is the single highest priority — there's nothing left for the agent to do,
/// everything left is on the user, which outranks even an approval prompt or
/// a still-working turn. Historical, never-live sessions (Inactive) rank
/// last since there's no fresh event demanding attention. Recency breaks ties
/// within a rank.
fn status_rank(s: SessionStatus) -> u8 {
    match s {
        SessionStatus::Ended => 0,
        SessionStatus::Blocked => 1,
        SessionStatus::Working => 2,
        SessionStatus::Idle => 3,
        SessionStatus::Inactive => 4,
    }
}

/// Resolve a stored walker id to an index into [`crate::walker::ALL`]: `None`
/// (never picked) and an unknown id both land on the default.
///
/// Pulled out of [`App::new_in`] so this can be tested without pointing
/// `MINDPLAYER_STATE` at a temp file. That env mutation is what made the first
/// version of the default-character test flaky: `std::env::set_var` races with
/// any other thread reading the environment, and background threads other tests
/// leave running do exactly that (the same hazard behind CI's `--test-threads=1`).
fn resolve_walker(stored: Option<&str>) -> usize {
    crate::walker::index_of(stored.unwrap_or(crate::walker::DEFAULT_ID))
}

fn agent_rank(agent: Agent) -> u8 {
    match agent {
        Agent::Codex => 0,
        Agent::Claude => 1,
        Agent::Kiro => 2,
        Agent::Cursor => 3,
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

/// Decide what to audit for a session's status this poll, given its previous
/// computed status (`None` = first sighting) and its current one. Returns
/// `Some((from, to))` to log a transition, or `None` to stay silent. A first
/// sighting is silent on purpose — the session's birth is already captured by
/// `SessionOpen`, so only genuine tick-to-tick changes reach the log. Pulled
/// out of [`App::poll_status_transitions`] so the change-only rule can be
/// tested without a real `PtySession`.
fn status_transition(
    prev: Option<SessionStatus>,
    now: SessionStatus,
) -> Option<(SessionStatus, SessionStatus)> {
    match prev {
        Some(prev) if prev != now => Some((prev, now)),
        _ => None,
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
    /// Index into [`crate::walker::ALL`] for the character walking the strip
    /// on the browse screens. Resolved from `state.walker` at startup, so an
    /// unknown/absent id lands on the default (rubber duck).
    pub walker_choice: usize,
    /// `Some(index)` while the scope screen's character picker is open — the
    /// index is the highlighted row, only committed to `walker_choice` (and
    /// persisted) on enter.
    pub walker_picker: Option<usize>,
    /// Open category picker (`t`). Rows are the existing categories plus a
    /// "new category" entry and a "remove from category" entry.
    pub category_picker: Option<CategoryPicker>,
    /// In-flight category sync (see `spawn_category_sync_for`). Reading peer
    /// transcripts happens off the main thread — doing it inline froze the UI
    /// when thread-sync did the same thing.
    pub(crate) category_sync_rx: Option<Receiver<CategorySyncResult>>,
    /// Open category menu (`t` on a header).
    pub category_menu: Option<CategoryMenu>,
    pub cwd: PathBuf,
    pub scope: Scope,
    pub cfg: ScanConfig,
    pub state: State,
    /// Where `state` is written. Held explicitly rather than re-resolved from
    /// `MINDPLAYER_STATE` on every save: tests can point it at a temp file with
    /// no env mutation at all, which is what stops a test from ever writing to
    /// the developer's real ~/.mindplayer/state.json (it has happened twice).
    pub state_path: PathBuf,
    /// Conversation-log directory, an explicit field for the same reason
    /// `state_path` is: a test must never be able to append to the real log.
    pub convo_dir: PathBuf,
    pub convo_index: crate::convo_log::ConvoIndex,
    /// In-flight background ingest batch; one at a time, cleared by the poll.
    pub(crate) convo_rx: Option<Receiver<ConvoIngestResult>>,
    /// Turns appended since launch. Nothing renders this yet; it is what the
    /// ingest tests assert progress against.
    pub convo_ingested: usize,
    /// When the pending-scan last ran, so it stays interval-gated rather than
    /// stat-ing every session on every tick.
    pub(crate) convo_last_scan: Option<Instant>,

    /// Full in-scope scan (drives the aggregate / scan numbers).
    pub all_sessions: Vec<Session>,
    /// Totals over the full in-scope scan (drives the post-scan "Collected"
    /// screen). Includes archived + sub-agent sessions.
    pub aggregate: Aggregate,
    /// Totals over just the currently-visible rows (drives the status bar, so
    /// its count/tokens match the list the user is looking at).
    pub visible_aggregate: Aggregate,
    /// The rows shown, after the archived / sub-agent view filters: category
    /// headers interleaved with session rows. Sessions are held as indices into
    /// `all_sessions` (not clones) to keep the refresh cheap.
    pub visible: Vec<Row>,
    pub selected: usize,
    /// Sessions each category holds, keyed by category id (`None` = the
    /// uncategorized bucket). Filled by `rebuild_visible` so a *folded* header
    /// can still show its tally — counting visible rows would report zero.
    pub category_counts: HashMap<Option<String>, usize>,
    /// Number of leading `visible` rows (headers included) belonging to
    /// categories touched within the last 24h; the rest are older. Computed in
    /// `rebuild_visible` so the list renderer draws the section headers from one
    /// source of truth.
    pub recent_count: usize,
    pub show_archived: bool,
    /// Show spawned helper/sub-agent sessions (hidden by default).
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
    /// When `Some`, the HTML-open path-input popup is open and holds the
    /// path typed so far (mirrors `dir_input`).
    pub html_preview_input: Option<String>,
    /// Inline error shown inside the still-open popup when the last submitted
    /// path didn't resolve to a file (or the browser failed to launch);
    /// cleared whenever the popup is freshly opened or the input changes.
    pub html_preview_error: Option<String>,
    /// Per-session id, the currently-known unpreviewed `.html` candidates found
    /// in that session's cwd, ranked most-recently-modified first. Refreshed by
    /// [`Self::poll_html_candidates`]; an absent/empty entry means no candidates.
    /// Drives both the passive per-pane "🌐 N new" badge and the Ctrl-P picker.
    pub html_candidates: HashMap<String, Vec<PathBuf>>,
    /// Per-session id, paths already offered/previewed together with the mtime
    /// they had when last seen. A candidate is suppressed while its current
    /// mtime is `<=` the seen mtime, so a file re-edited after being
    /// dismissed/previewed (its mtime advancing) reappears as a fresh candidate.
    /// A plain set can't express "seen at time T", which the reappear-on-edit
    /// rule (and its unit test) require — hence the nested mtime map.
    pub html_seen: HashMap<String, HashMap<PathBuf, SystemTime>>,
    /// When the next `.html`-candidate poll is due. Gates
    /// [`Self::poll_html_candidates`] to an interval (a few seconds) instead of
    /// every `run()` tick, so the periodic directory walk stays cheap. `None`
    /// means "run on the next poll".
    pub html_candidates_due: Option<Instant>,
    /// Latest hook-derived status per pane id (Claude/Codex only — see
    /// `agent_hooks`), refreshed on the same interval gate as
    /// `hook_status_due`. Read by [`Self::session_status`] in place of the
    /// screen-text heuristic whenever an entry exists here.
    pub hook_status: HashMap<String, crate::agent_hooks::HookReading>,
    /// When the next hook-state-file poll is due. Same purpose as
    /// `html_candidates_due` — bounds the per-tick filesystem reads to an
    /// interval instead of every frame.
    pub hook_status_due: Option<Instant>,
    /// When `Some`, the Ctrl-P candidate picker is open; the value is the
    /// selected index into the focused pane's `html_candidates` list. Mirrors
    /// `new_picker`/`handoff_picker`.
    pub html_preview_picker: Option<usize>,
    /// Links found in a recent answer, waiting to be picked from. Only opened
    /// when there are two or more — a single link is copied outright, since 17
    /// of 22 link-bearing answers in this project's own history had exactly one
    /// and a picker for one item is a step that buys nothing.
    pub link_picker: Option<LinkPicker>,
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
    /// Last computed [`SessionStatus`] per live session, so
    /// [`Self::poll_status_transitions`] can log an `AuditEvent::SessionStatusChange`
    /// only when the status actually changes tick-to-tick (never every poll).
    /// First sighting of a session is seeded silently — its birth is already
    /// captured by `SessionOpen`.
    last_status: HashMap<String, SessionStatus>,
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
    /// Additional PTY spawns queued behind `pending`, e.g. multi-select launch.
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
    /// Set by `c` on a Working/Blocked session, holding its id, while the
    /// "send anyway?" confirm is up. Idle sessions skip this and send at once.
    pub catchup_confirm: Option<String>,
    /// Opened by `Ctrl-T` from a live pane (single or multi). Holds the
    /// one-line "topic / RUNBOOK §n / files" text typed so far; sent to the
    /// *focused* pane's own CLI on confirm, wrapped in a fixed instruction
    /// template — never to every pane, unlike broadcast.
    pub transition_report_input: Option<String>,
    /// Set once the one-line input above is confirmed: the fully assembled
    /// prompt (template + typed specifics), shown read-only by default.
    /// Reuses `BroadcastDraft` purely as a text+cursor buffer so switching
    /// into edit mode gets multi-line editing for free.
    pub transition_report_review: Option<text_input::BroadcastDraft>,
    /// False = read-only preview (enter sends as-is, `e` edits); true = the
    /// buffer above is directly editable (enter still sends; shift/alt/ctrl-enter
    /// inserts a newline instead, same as broadcast/dispatch).
    pub transition_report_review_editing: bool,
    /// Resolved once at construction (see [`audit_path_for_app`]) so every
    /// instrumentation call site logs to the same place without re-resolving
    /// `MINDPLAYER_AUDIT`/`cfg!(test)` on every keypress.
    pub(crate) audit_path: PathBuf,
    /// Resolved once at construction (see [`prompts_dir_for_app`]) — the
    /// directory `load_prompt` calls read/seed catchup.md, transition_report.md, etc. from.
    pub(crate) prompts_dir: PathBuf,
    /// Subscription rate-limit windows for the usage popup. `None` until the
    /// first fetch lands; the fetch does network I/O so it never blocks a keypress.
    pub limits: Option<mindplayer_core::limits::Limits>,
    pub(crate) limits_rx: Option<Receiver<mindplayer_core::limits::Limits>>,
    /// When the in-flight fetch started, so a wedged one can be abandoned.
    pub(crate) limits_started: Option<Instant>,
    /// Keyboard shortcut help overlay opened by `?`.
    pub help_visible: bool,
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
    /// New sessions closed with `x` before their rollout file ever appeared.
    /// The agent may still write that file, and the scan would then show the
    /// closed session as a brand-new row — so these are kept just long enough
    /// to archive the disk session on arrival. Never rendered.
    closed_extras: Vec<Session>,

    /// Why a pane's child exited, for the ones that exited without drawing
    /// anything. Keyed by session id; empty for panes that failed visibly.
    pub(crate) pane_error: HashMap<String, String>,

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
    /// In-flight background peer-lane transcript read for a thread-sync
    /// prompt (see `prepare_thread_sync_for`). Reading + parsing peer
    /// transcripts can take real time on a large/long-lived session, so it
    /// runs off the main thread — otherwise every re-entry into such a
    /// session would freeze the whole UI (input and rendering both) until
    /// the read finished. Keyed by the target session id so a stale result
    /// can never land on a pane the user has since switched away from.
    thread_sync_rx: Option<Receiver<(String, Result<handoff::PreparedHandoff, String>)>>,
    /// In-flight background `.html`-candidate scan sweep (see
    /// `spawn_html_scan`/`apply_html_scan`). Walking each live pane's cwd
    /// (`scan_html_candidates`) is a blocking filesystem walk that takes long
    /// enough to visibly stall input/rendering when a session's cwd is the root
    /// of a large monorepo — so, exactly like `thread_sync_rx`, the walk runs on
    /// a background thread and the finished batch is picked up non-blocking on
    /// the main thread. Each entry is `(pane_id, raw scan_html_candidates
    /// results for that pane)`; `None` means no sweep is in flight.
    html_scan_rx: Option<Receiver<HtmlScanBatch>>,
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
    /// Keystrokes the user typed into this pane while `bytes` was still
    /// waiting to go out. Held here instead of forwarded to the pty so they
    /// never interleave with (and corrupt) the queued prompt; replayed in
    /// order once `flush_initial_inputs` actually sends it.
    held_input: Vec<u8>,
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
        // Loaded before the literal so the stored character id can be resolved
        // to an index up front (an unknown id falls back to the default).
        let state_path = mindplayer_core::state::default_state_path();
        let state = State::load_from(&state_path);
        let convo_dir = convo_dir_for_app();
        let walker_choice = resolve_walker(state.walker.as_deref());
        App {
            screen: Screen::ScopeSelect,
            scope_choice: 0,
            walker_choice,
            walker_picker: None,
            category_picker: None,
            category_sync_rx: None,
            category_menu: None,
            scope: Scope::WorkingDir(cwd.clone()),
            cwd,
            cfg: ScanConfig::from_env(),
            state,
            state_path,
            convo_index: crate::convo_log::load_index(&convo_dir),
            convo_dir,
            convo_rx: None,
            convo_ingested: 0,
            convo_last_scan: None,
            all_sessions: Vec::new(),
            aggregate: Aggregate::default(),
            visible_aggregate: Aggregate::default(),
            visible: Vec::new(),
            selected: 0,
            recent_count: 0,
            category_counts: HashMap::new(),
            show_archived: false,
            hero_visible: false,
            list_rows: 0,
            focus: Focus::List,
            multi_select: false,
            marked: HashSet::new(),
            ptys: HashMap::new(),
            html_preview_input: None,
            html_preview_error: None,
            html_candidates: HashMap::new(),
            html_seen: HashMap::new(),
            html_candidates_due: None,
            hook_status: HashMap::new(),
            hook_status_due: None,
            html_preview_picker: None,
            link_picker: None,
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
            last_status: HashMap::new(),
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
            catchup_confirm: None,
            transition_report_input: None,
            transition_report_review: None,
            transition_report_review_editing: false,
            audit_path: audit_path_for_app(),
            prompts_dir: prompts_dir_for_app(),
            limits: None,
            limits_rx: None,
            limits_started: None,
            help_visible: false,
            search_query: None,
            new_counter: 0,
            extra_sessions: Vec::new(),
            new_baselines: HashMap::new(),
            closed_extras: Vec::new(),
            pane_error: HashMap::new(),
            pty_rows: 24,
            pty_cols: 80,
            pty_x: 0,
            pty_y: 0,
            scan_rx: None,
            refresh_rx: None,
            bg_rescan_rx: None,
            rescan_due: None,
            thread_sync_rx: None,
            html_scan_rx: None,
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

    /// Persist the sidecar state to [`Self::state_path`]. A failed write is
    /// non-fatal everywhere it is called — the change still applies for this
    /// run, it just is not remembered.
    pub(crate) fn save_state(&self) -> anyhow::Result<()> {
        self.state.save_to(&self.state_path)
    }

    // --- walking character ------------------------------------------------

    /// The character currently walking the browse-screen strip.
    pub fn walker(&self) -> &'static crate::walker::Character {
        crate::walker::get(self.walker_choice)
    }

    /// Open the scope screen's character picker, starting on the current pick.
    pub fn open_walker_picker(&mut self) {
        self.walker_picker = Some(self.walker_choice);
    }

    /// Close without committing — `walker_choice` keeps its previous value.
    pub fn cancel_walker_picker(&mut self) {
        self.walker_picker = None;
    }

    /// Move the picker highlight, wrapping at both ends like the session list.
    pub fn move_walker_pick(&mut self, delta: isize) {
        let Some(cur) = self.walker_picker else {
            return;
        };
        let len = crate::walker::ALL.len() as isize;
        let next = (cur as isize + delta).rem_euclid(len);
        self.walker_picker = Some(next as usize);
    }

    /// Commit the highlighted character and persist it. A failed save is
    /// non-fatal: the pick still applies for this run, it just won't be
    /// remembered — same treatment the rest of the sidecar state gets.
    pub fn confirm_walker_pick(&mut self) {
        let Some(pick) = self.walker_picker.take() else {
            return;
        };
        self.walker_choice = pick;
        self.state.walker = Some(crate::walker::get(pick).id.to_string());
        let _ = self.save_state();
        self.status = format!("character: {}", crate::walker::get(pick).name);
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

    /// [`Self::scope_label`] trimmed to the last two path components, for the
    /// footer — where the usage bar and this share one row. The scan screen
    /// keeps the full path: there it has a row to itself, and that is the point
    /// at which knowing exactly which tree is being scanned matters.
    pub fn scope_label_short(&self) -> String {
        match self.scope {
            Scope::Global => "global".to_string(),
            Scope::WorkingDir(_) => {
                let tail: Vec<_> = self.cwd.components().rev().take(2).collect();
                let tail: PathBuf = tail.into_iter().rev().collect();
                format!("working dir ({})", tail.display())
            }
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
    /// Everything before the usage bar.
    pub fn summary_head(&self) -> String {
        let a = &self.visible_aggregate;
        format!(
            "{} sessions · {} tok ",
            a.session_count(),
            human_tokens(a.total.total),
        )
    }

    /// Everything after the usage bar and its labels.
    ///
    /// Account quotas used to be crammed in here as text. They now get a row
    /// each below this one ([`Self::quota_rows`]) so each is gauged rather than
    /// read, which leaves this line to say only where the counts came from.
    pub fn summary_tail(&self) -> String {
        format!(" · {}", self.scope_label_short())
    }

    /// One row per account window for the footer, or empty before the first
    /// reading lands. Sessions of an agent that reports no quota still get a
    /// row, so a configured agent never silently vanishes from the footer.
    pub fn quota_rows(&self) -> Vec<mindplayer_core::limits::QuotaRow> {
        self.limits
            .as_ref()
            .map(mindplayer_core::limits::Limits::quota_rows)
            .unwrap_or_default()
    }

    /// The share of measured tokens each agent holds, as bar cells.
    ///
    /// A share, not a magnitude: cumulative token totals have no ceiling to be
    /// a percentage of, so the only honest bar is one where the parts add up to
    /// the whole shown. Agents whose tokens are not read at all — kiro — are
    /// left out rather than drawn as zero, and the caller still labels them so
    /// the gap is visible instead of implied.
    pub fn usage_segments(&self) -> Vec<UsageSegment> {
        let a = &self.visible_aggregate;
        let measured: Vec<(&'static str, char, u64)> = [
            ("claude", '█', a.claude.total),
            ("codex", '▓', a.codex.total),
        ]
        .into_iter()
        .filter(|(_, _, total)| *total > 0)
        .collect();
        let totals: Vec<u64> = measured.iter().map(|(_, _, t)| *t).collect();
        let cells = apportion(&totals, USAGE_BAR_CELLS);
        // Percentages get the same treatment as the cells so the labels add up
        // to 100 rather than to 99 or 101 after independent rounding.
        let percents = apportion(&totals, 100);
        measured
            .into_iter()
            .enumerate()
            .map(|(i, (label, glyph, _))| UsageSegment {
                label,
                glyph,
                cells: cells[i],
                percent: percents[i],
            })
            .collect()
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Snake-case label for an audit event's `focus` field.
fn focus_label(focus: Focus) -> &'static str {
    match focus {
        Focus::List => "list",
        Focus::Terminal => "terminal",
    }
}

/// Snake-case label for an audit event's status `from`/`to` fields.
fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Blocked => "blocked",
        SessionStatus::Working => "working",
        SessionStatus::Idle => "idle",
        SessionStatus::Ended => "ended",
        SessionStatus::Inactive => "inactive",
    }
}

/// The home directory the rate-limit fetch reads credentials and rollouts from.
///
/// Guarded like [`convo_dir_for_app`] and for a sharper reason: under `cargo
/// test` the real home would send `limits::fetch` into the user's login Keychain
/// (`security find-generic-password`) and, once a subscription token exists
/// there, into a live authenticated request to api.anthropic.com. A unit test
/// must not touch either. `MINDPLAYER_LIMITS_HOME` lets a test opt into a
/// fixture home deliberately.
fn limits_home_for_app() -> PathBuf {
    if cfg!(test) {
        std::env::var("MINDPLAYER_LIMITS_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!("mindplayer-test-home-{}", std::process::id()))
            })
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
    }
}

/// The conversation-log directory an `App` writes to, mirroring
/// [`audit_path_for_app`]'s test isolation exactly: real build → the shared
/// `~/.mindplayer/convo`; under `cargo test` → a per-process temp directory, so
/// a test that never opted in can't append turns to the real user's log.
fn convo_dir_for_app() -> PathBuf {
    if cfg!(test) {
        std::env::var("MINDPLAYER_CONVO_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!("mindplayer-test-convo-{}", std::process::id()))
            })
    } else {
        crate::convo_log::default_dir()
    }
}

/// The audit-log path an `App` logs to. In a real build this is always
/// `mindplayer_core::default_audit_path()`. Under `cargo test`, `cfg!(test)`
/// is true for every test in this crate regardless of whether that
/// particular test knows anything about auditing — so it defaults to a
/// per-process temp file instead, and the real `~/.mindplayer/audit.jsonl`
/// is never touched by a test that never opted in. A test that wants to
/// assert on actual audit content still sets `MINDPLAYER_AUDIT` itself
/// (same pattern `MINDPLAYER_STATE` already uses for `state.json`).
fn audit_path_for_app() -> PathBuf {
    if cfg!(test) {
        std::env::var("MINDPLAYER_AUDIT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!(
                    "mindplayer-test-audit-{}.jsonl",
                    std::process::id()
                ))
            })
    } else {
        mindplayer_core::default_audit_path()
    }
}

/// The prompt-templates directory an `App` reads from, mirroring
/// [`audit_path_for_app`]'s test-isolation reasoning exactly: real build →
/// `mindplayer_core::default_prompts_dir()`; under `cargo test` → a
/// per-process temp directory, so a test that never opted in (by setting
/// `MINDPLAYER_PROMPTS_DIR` itself) can't seed/overwrite a real user's
/// `~/.mindplayer/prompts/*.md`.
fn prompts_dir_for_app() -> PathBuf {
    if cfg!(test) {
        std::env::var("MINDPLAYER_PROMPTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!("mindplayer-test-prompts-{}", std::process::id()))
            })
    } else {
        mindplayer_core::default_prompts_dir()
    }
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

mod convo_ingest;
mod handoff_sync;
mod link_copy;
mod modals;
mod pane;
mod selection;
mod session_list;
mod spawn;

#[cfg(test)]
mod tests;
