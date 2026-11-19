//! Holds a provider launch while that CLI's own updater runs, then releases it.
use super::*;
use std::io;
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::TryRecvError;

pub(super) const FRESH_FOR: Duration = Duration::from_secs(600);
pub(super) const UPDATE_BUDGET: Duration = Duration::from_secs(90);
const VERSION_BUDGET: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(2);
/// The descriptor number the lock-keeping shell holds the update lock on.
const KEPT_LOCK_FD: RawFd = 9;
/// Runs its arguments with the kept lock closed, so only this shell holds the lock.
const KEEP_LOCK_SCRIPT: &str = "\"$@\" 9<&-\nexit $?";
const CAPTURE_LIMIT: u64 = 64 * 1024;
const VERSION_MAX: usize = 64;
const REASON_MAX: usize = 60;
const POLL_STEP: Duration = Duration::from_millis(25);
const COVERED: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Omp];

/// How one child process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Exit {
    Success,
    Failed(Option<i32>),
    TimedOut,
    NotRun(String),
}

/// The exit and the bounded output of one child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ran {
    pub exit: Exit,
    pub output: String,
}

/// Runs `argv` for at most the given time, capturing output through the given file.
pub(super) type Runner =
    Arc<dyn Fn(&[String], Duration, &Path, Option<RawFd>) -> Ran + Send + Sync>;

/// What one update attempt observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UpdateRun {
    pub before: Option<String>,
    pub after: Option<String>,
    pub result: Result<(), String>,
}

/// How a background update job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Finished {
    Skipped,
    Ran(UpdateRun),
    Failed(String),
}

struct Job {
    program: &'static str,
    result: Receiver<Finished>,
}

/// The verdict for one launch that reached the gate.
pub(super) enum Admit {
    Go(PendingSpawn),
    Held { program: &'static str },
}

/// Launches freed by one finished job, with the status line to show.
pub(super) struct Done {
    pub program: &'static str,
    pub released: Vec<PendingSpawn>,
    pub notice: Option<String>,
}

pub(super) struct CliFreshness {
    enabled: bool,
    root: PathBuf,
    runner: Runner,
    budget: Duration,
    jobs: Vec<Job>,
    held: Vec<(&'static str, PendingSpawn)>,
    finished_at: HashMap<&'static str, Instant>,
    pub(super) notices: HashMap<String, String>,
}

impl CliFreshness {
    pub(super) fn new(enabled: bool, root: PathBuf, runner: Runner, budget: Duration) -> Self {
        Self {
            enabled,
            root,
            runner,
            budget,
            jobs: Vec::new(),
            held: Vec::new(),
            finished_at: HashMap::new(),
            notices: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The gate for a real run; unit tests get a disabled gate rooted in the temp directory.
    pub(super) fn from_env() -> Self {
        let root = if cfg!(test) {
            std::env::temp_dir().join(format!("mindplayer-test-cli-update-{}", std::process::id()))
        } else {
            mindplayer_core::private::data_root().join("cli-update")
        };
        let enabled =
            !cfg!(test) && switch_enabled(std::env::var("MINDPLAYER_CLI_UPDATE").ok().as_deref());
        Self::new(enabled, root, Arc::new(process_runner), UPDATE_BUDGET)
    }

    fn target(&self, pending: &PendingSpawn) -> Option<&'static str> {
        if !self.enabled
            || pending
                .session_id
                .starts_with(super::accounts_panel::LOGIN_PREFIX)
        {
            return None;
        }
        COVERED
            .iter()
            .map(|agent| agent.program())
            .find(|program| *program == pending.command.program)
    }

    fn is_fresh(&self, program: &'static str) -> bool {
        self.finished_at
            .get(program)
            .is_some_and(|at| at.elapsed() < FRESH_FOR)
            || stamp_age(&self.root, program).is_some_and(|age| age < FRESH_FOR)
    }

    fn start(&mut self, program: &'static str) -> io::Result<()> {
        let (root, runner, budget) = (self.root.clone(), self.runner.clone(), self.budget);
        let (tx, result) = mpsc::channel();
        thread::Builder::new()
            .name(format!("cli-update-{program}"))
            .spawn(move || {
                let _ = tx.send(run_job(&root, program, &runner, budget));
            })?;
        self.jobs.push(Job { program, result });
        Ok(())
    }

    /// Lets a launch through, or takes it into the holder while its CLI updates.
    pub(super) fn admit(&mut self, pending: PendingSpawn) -> Admit {
        let Some(program) = self.target(&pending) else {
            return Admit::Go(pending);
        };
        if !self.jobs.iter().any(|job| job.program == program)
            && (self.is_fresh(program) || self.start(program).is_err())
        {
            return Admit::Go(pending);
        }
        self.held.push((program, pending));
        Admit::Held { program }
    }

    /// Collects finished jobs and hands back the launches each one was holding.
    pub(super) fn poll(&mut self) -> Vec<Done> {
        let mut done = Vec::new();
        for job in std::mem::take(&mut self.jobs) {
            let finished = match job.result.try_recv() {
                Ok(finished) => finished,
                Err(TryRecvError::Empty) => {
                    self.jobs.push(job);
                    continue;
                }
                Err(TryRecvError::Disconnected) => Finished::Failed("updater stopped".into()),
            };
            self.finished_at.insert(job.program, Instant::now());
            let (released, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
                .into_iter()
                .partition(|(program, _)| *program == job.program);
            self.held = kept;
            done.push(Done {
                program: job.program,
                released: released.into_iter().map(|(_, pending)| pending).collect(),
                notice: status_for(job.program, &finished),
            });
        }
        done
    }

    pub(super) fn owns_launch(&self, id: &str) -> bool {
        self.held
            .iter()
            .any(|(_, pending)| pending.session_id == id)
    }

    pub(super) fn cancel_launch(&mut self, id: &str) {
        self.held.retain(|(_, pending)| pending.session_id != id);
        self.notices.remove(id);
    }

    pub(super) fn message(&self, id: &str) -> Option<&'static str> {
        self.held
            .iter()
            .find(|(_, pending)| pending.session_id == id)
            .map(|(program, _)| updating_text(program))
    }
}

/// True unless the switch value turns the gate off.
pub(super) fn switch_enabled(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "off" | "false")
    )
}

pub(super) fn checking_text(program: &str) -> String {
    format!("checking {program} for updates…")
}

fn updating_text(program: &str) -> &'static str {
    match program {
        "claude" => "updating claude…",
        "codex" => "updating codex…",
        _ => "updating omp…",
    }
}

fn status_for(program: &str, finished: &Finished) -> Option<String> {
    let failure = |reason: &str| {
        Some(format!(
            "{program} update failed — starting the installed version ({reason})"
        ))
    };
    match finished {
        Finished::Skipped => None,
        Finished::Failed(reason) => failure(reason),
        Finished::Ran(UpdateRun {
            result: Err(reason),
            ..
        }) => failure(reason),
        Finished::Ran(UpdateRun {
            before: Some(before),
            after: Some(after),
            ..
        }) if before != after => Some(format!("{program} updated {before} → {after}")),
        Finished::Ran(_) => None,
    }
}

/// Drops terminal escapes and control characters, folds whitespace, and bounds the length.
fn tidy(text: &str, max: usize) -> String {
    let mut plain = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next_if_eq(&'[').is_some() {
                while chars
                    .next_if(|n| !('\u{40}'..='\u{7e}').contains(n))
                    .is_some()
                {}
                chars.next();
            }
        } else if c.is_control() {
            plain.push(' ');
        } else {
            plain.push(c);
        }
    }
    let folded = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= max {
        return folded;
    }
    let mut cut: String = folded.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The version token of a `--version` line, or the bounded line itself.
pub(super) fn version_label(line: &str) -> String {
    let line = tidy(line, VERSION_MAX);
    line.split(|c: char| c.is_whitespace() || c == '/')
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|word| word.contains('.') && word.chars().any(|c| c.is_ascii_digit()))
        .map_or_else(|| line.clone(), str::to_string)
}

fn read_capped(path: &Path) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(CAPTURE_LIMIT).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// True once the child has exited, without collecting it, so its group ID stays reserved.
fn has_exited(pid: libc::pid_t) -> bool {
    // SAFETY: a zeroed siginfo_t is a valid value for waitid to fill in.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: `info` outlives the call and `pid` names this process's own child.
    let peeked = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
    // SAFETY: waitid filled `info`, or left it zeroed when the child still runs.
    peeked == 0 && unsafe { info.si_pid() } == pid
}

/// Asks the updater's process group to stop, then kills what is left after the grace period.
fn stop_group(child: &mut std::process::Child) {
    let group = child.id() as libc::pid_t;
    // SAFETY: killpg only signals the group this child leads.
    unsafe { libc::killpg(group, libc::SIGTERM) };
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline && !has_exited(group) {
        thread::sleep(POLL_STEP);
    }
    // SAFETY: the uncollected child keeps this group ID from naming another group.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs a command in a session of its own; with `lock`, a shell keeps that descriptor until the command ends.
pub(super) fn process_runner(
    argv: &[String],
    timeout: Duration,
    capture: &Path,
    lock: Option<RawFd>,
) -> Ran {
    let not_run = |why: String| Ran {
        exit: Exit::NotRun(why),
        output: String::new(),
    };
    let Some((program, args)) = argv.split_first() else {
        return not_run("empty command".into());
    };
    let out = match mindplayer_core::private::open_private(capture, false) {
        Ok(file) => file,
        Err(e) => return not_run(format!("capture file: {e}")),
    };
    let err = match out.try_clone() {
        Ok(file) => file,
        Err(e) => return not_run(format!("capture file: {e}")),
    };
    let mut command = match lock {
        Some(_) => {
            let mut shell = std::process::Command::new("/bin/sh");
            shell.args(["-c", KEEP_LOCK_SCRIPT, "sh"]).arg(program);
            shell
        }
        None => std::process::Command::new(program),
    };
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    // SAFETY: the hook runs between fork and exec and calls only async-signal-safe functions.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            let kept = match lock {
                Some(fd) if fd == KEPT_LOCK_FD => libc::fcntl(fd, libc::F_SETFD, 0),
                Some(fd) => libc::dup2(fd, KEPT_LOCK_FD),
                None => 0,
            };
            if kept == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let _ = std::fs::remove_file(capture);
            return not_run("not found on PATH".into());
        }
        Err(e) => {
            let _ = std::fs::remove_file(capture);
            return not_run(e.to_string());
        }
    };
    let deadline = Instant::now() + timeout;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Exit::Success,
            Ok(Some(status)) => break Exit::Failed(status.code()),
            Ok(None) if Instant::now() >= deadline => {
                stop_group(&mut child);
                break Exit::TimedOut;
            }
            Ok(None) => thread::sleep(POLL_STEP),
            Err(e) => {
                stop_group(&mut child);
                break Exit::NotRun(e.to_string());
            }
        }
    };
    let output = read_capped(capture);
    let _ = std::fs::remove_file(capture);
    Ran { exit, output }
}

pub(super) fn stamp_path(root: &Path, program: &str) -> PathBuf {
    root.join(format!("{program}.stamp"))
}

fn stamp_age(root: &Path, program: &str) -> Option<Duration> {
    let modified = std::fs::metadata(stamp_path(root, program))
        .and_then(|meta| meta.modified())
        .ok()?;
    std::time::SystemTime::now().duration_since(modified).ok()
}

fn touch_stamp(root: &Path, program: &str) {
    if let Ok(file) = mindplayer_core::private::open_private(&stamp_path(root, program), false) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Takes the exclusive lock for `program`, polling until `deadline`.
fn lock_until(root: &Path, program: &str, deadline: Instant) -> Result<std::fs::File, String> {
    let path = root.join(format!("{program}.lock"));
    let file = mindplayer_core::private::open_private(&path, true)
        .map_err(|e| tidy(&format!("update lock unavailable: {e}"), REASON_MAX))?;
    loop {
        // SAFETY: the descriptor stays open for as long as `file` lives.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock if Instant::now() < deadline => thread::sleep(POLL_STEP),
            io::ErrorKind::WouldBlock => return Err("update lock busy".into()),
            _ => return Err(tidy(&format!("update lock failed: {error}"), REASON_MAX)),
        }
    }
}

fn read_version(runner: &Runner, program: &str, left: Duration, capture: &Path) -> Option<String> {
    let argv = [program.to_string(), "--version".to_string()];
    let ran = runner(&argv, left.min(VERSION_BUDGET), capture, None);
    let line = ran.output.lines().map(str::trim).find(|l| !l.is_empty())?;
    (ran.exit == Exit::Success).then(|| version_label(line))
}

fn failure_reason(ran: &Ran, timeout: Duration) -> String {
    let detail = ran
        .output
        .lines()
        .rev()
        .map(|line| tidy(line, REASON_MAX))
        .find(|line| !line.is_empty());
    let head = match &ran.exit {
        Exit::Failed(Some(code)) => format!("exit {code}"),
        Exit::Failed(None) => "stopped by a signal".to_string(),
        Exit::TimedOut => format!("timed out after {}s", timeout.as_secs()),
        Exit::NotRun(why) => return tidy(why, REASON_MAX),
        Exit::Success => return String::new(),
    };
    match detail {
        Some(detail) if !matches!(ran.exit, Exit::TimedOut) => format!("{head}: {detail}"),
        _ => head,
    }
}

/// Runs one update attempt under the shared lock and stamp.
pub(super) fn run_job(root: &Path, program: &str, runner: &Runner, budget: Duration) -> Finished {
    let deadline = Instant::now() + budget;
    let left = || deadline.saturating_duration_since(Instant::now());
    let lock = match lock_until(root, program, deadline) {
        Ok(lock) => lock,
        Err(reason) => return Finished::Failed(reason),
    };
    if stamp_age(root, program).is_some_and(|age| age < FRESH_FOR) {
        return Finished::Skipped;
    }
    let capture = root.join(format!("{program}.out"));
    let before = read_version(runner, program, left(), &capture);
    let argv = [program.to_string(), "update".to_string()];
    let timeout = left();
    let ran = runner(&argv, timeout, &capture, Some(lock.as_raw_fd()));
    let result = match ran.exit {
        Exit::Success => Ok(()),
        _ => Err(failure_reason(&ran, timeout)),
    };
    let after = result
        .is_ok()
        .then(|| read_version(runner, program, left(), &capture))
        .flatten();
    touch_stamp(root, program);
    Finished::Ran(UpdateRun {
        before,
        after,
        result,
    })
}

impl App {
    /// Releases the launches of every finished update job; true when anything changed.
    pub fn poll_cli_freshness(&mut self) -> bool {
        let done = self.cli_freshness.poll();
        let changed = !done.is_empty();
        for Done {
            program,
            released,
            notice,
        } in done
        {
            match notice {
                Some(text) => {
                    for pending in &released {
                        self.cli_freshness
                            .notices
                            .insert(pending.session_id.clone(), text.clone());
                    }
                    self.status = text;
                }
                None if self.status == checking_text(program) => self.status.clear(),
                None => {}
            }
            for pending in released {
                self.enqueue_spawn(pending);
            }
        }
        changed
    }

    pub(super) fn gate_cli_update(&mut self, pending: PendingSpawn) -> Option<PendingSpawn> {
        match self.cli_freshness.admit(pending) {
            Admit::Go(pending) => Some(pending),
            Admit::Held { program } => {
                self.status = checking_text(program);
                None
            }
        }
    }

    /// Shows a released launch's update outcome once the account check has set its own status.
    pub(super) fn show_release_notice(&mut self, id: &str) {
        if let Some(text) = self.cli_freshness.notices.remove(id) {
            self.status = text;
        }
    }
}
