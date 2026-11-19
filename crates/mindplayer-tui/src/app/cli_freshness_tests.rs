use super::cli_freshness::*;
use super::*;
use mindplayer_core::accounts::Account;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::SystemTime;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mindplayer-cli-fresh-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fake {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    version: Arc<Mutex<String>>,
    senders: HashMap<String, Sender<Ran>>,
    runner: Runner,
}

impl Fake {
    fn new(programs: &[&str]) -> Fake {
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Default::default();
        let version = Arc::new(Mutex::new("tool 0.1.0".to_string()));
        let mut senders = HashMap::new();
        let mut receivers = HashMap::new();
        for program in programs {
            let (tx, rx) = mpsc::channel();
            senders.insert(program.to_string(), tx);
            receivers.insert(program.to_string(), Mutex::new(rx));
        }
        let runner: Runner = {
            let (calls, version) = (calls.clone(), version.clone());
            Arc::new(move |argv, _timeout, _capture, _lock| {
                calls.lock().unwrap().push(argv.to_vec());
                match argv.get(1).map(String::as_str) {
                    Some("--version") => Ran {
                        exit: Exit::Success,
                        output: format!("{}\n", version.lock().unwrap()),
                    },
                    Some("update") => receivers[&argv[0]].lock().unwrap().recv().unwrap_or(Ran {
                        exit: Exit::NotRun("fake closed".into()),
                        output: String::new(),
                    }),
                    other => panic!("unexpected fake call {other:?}"),
                }
            })
        };
        Fake {
            calls,
            version,
            senders,
            runner,
        }
    }

    fn set_version(&self, value: &str) {
        *self.version.lock().unwrap() = value.to_string();
    }

    fn finish(&self, program: &str, exit: Exit, output: &str) {
        self.senders[program]
            .send(Ran {
                exit,
                output: output.into(),
            })
            .unwrap();
    }

    fn updates(&self, program: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|argv| argv[0] == program && argv[1] == "update")
            .count()
    }

    fn total_calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn wait_updates(&self, program: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.updates(program) < count {
            assert!(Instant::now() < deadline, "{program} updater never started");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn launch_command(program: &str) -> PendingSpawn {
    launch_with("s", program)
}

fn launch_with(id: &str, program: &str) -> PendingSpawn {
    PendingSpawn {
        command: mindplayer_core::Command {
            program: program.into(),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            env: Default::default(),
        },
        session_id: id.into(),
        initial_input: None,
        focus_after_spawn: false,
    }
}

fn launch(agent: Agent, id: &str) -> PendingSpawn {
    let mut pending = launch_with(id, agent.program());
    pending.command =
        mindplayer_core::new_session(agent, std::env::temp_dir(), &Account::inherited(agent));
    pending
}

fn app_with(dir: &Scratch, fake: &Fake, enabled: bool) -> App {
    let mut app = super::tests::isolated_app();
    app.cli_freshness = CliFreshness::new(
        enabled,
        dir.0.join("cli-update"),
        fake.runner.clone(),
        Duration::from_secs(5),
    );
    app
}

fn request(app: &mut App, pending: PendingSpawn) {
    app.enqueue_spawn(pending);
    while app.pending.is_some() {
        app.spawn_pending();
    }
}

fn settle(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.poll_cli_freshness() {
        assert!(Instant::now() < deadline, "update job never finished");
        thread::sleep(Duration::from_millis(5));
    }
}

fn age_of(path: &Path) -> Duration {
    SystemTime::now()
        .duration_since(std::fs::metadata(path).unwrap().modified().unwrap())
        .unwrap_or_default()
}

fn write_stamp(dir: &Scratch, program: &str, age: Duration) {
    let root = dir.0.join("cli-update");
    std::fs::create_dir_all(&root).unwrap();
    let path = stamp_path(&root, program);
    std::fs::write(&path, "x").unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

#[test]
fn a_stale_cli_holds_its_launch_until_the_updater_finishes_then_releases_it() {
    let dir = Scratch::new("hold");
    let fake = Fake::new(&["codex"]);
    fake.set_version("codex-cli 0.1.0");
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "resume"));
    assert!(app.pending.is_none());
    assert!(app.ptys.is_empty());
    assert_eq!(app.status, "checking codex for updates…");
    assert_eq!(app.account_auth_message("resume"), Some("updating codex…"));
    fake.wait_updates("codex", 1);
    assert!(!app.poll_cli_freshness(), "still running");
    assert!(app.pending.is_none());
    fake.set_version("codex-cli 0.2.0");
    fake.finish("codex", Exit::Success, "");
    settle(&mut app);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "resume");
    assert_eq!(app.status, "codex updated 0.1.0 → 0.2.0");
    assert_eq!(app.account_auth_message("resume"), None);
    let released = app.pending.take().unwrap();
    assert!(matches!(app.cli_freshness.admit(released), Admit::Go(_)));
    assert_eq!(fake.updates("codex"), 1);
}

#[test]
fn an_unchanged_version_leaves_no_status_behind() {
    let dir = Scratch::new("same");
    let fake = Fake::new(&["claude"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Claude, "a"));
    fake.finish("claude", Exit::Success, "already current");
    settle(&mut app);
    assert!(app.status.is_empty(), "{}", app.status);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "a");
}

#[test]
fn two_launches_of_one_cli_start_one_job_and_release_in_order() {
    let dir = Scratch::new("two");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "first"));
    request(&mut app, launch(Agent::Codex, "second"));
    fake.wait_updates("codex", 1);
    assert!(app.pending.is_none() && app.pending_queue.is_empty());
    fake.finish("codex", Exit::Success, "");
    settle(&mut app);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "first");
    assert_eq!(app.pending_queue.front().unwrap().session_id, "second");
    assert_eq!(fake.updates("codex"), 1);
}

#[test]
fn a_fresh_stamp_skips_the_update() {
    let dir = Scratch::new("fresh");
    let fake = Fake::new(&["codex"]);
    write_stamp(&dir, "codex", Duration::from_secs(60));
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "a"));
    assert!(app.pending.is_none());
    assert!(
        !app.cli_freshness.owns_launch("a"),
        "a fresh CLI is never held"
    );
    assert_eq!(fake.total_calls(), 0);
}

#[test]
fn an_old_stamp_triggers_the_update_and_a_success_refreshes_it() {
    let dir = Scratch::new("old");
    let fake = Fake::new(&["codex"]);
    write_stamp(&dir, "codex", Duration::from_secs(11 * 60));
    let stamp = stamp_path(&dir.0.join("cli-update"), "codex");
    assert!(age_of(&stamp) > Duration::from_secs(600));
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "a"));
    fake.wait_updates("codex", 1);
    fake.finish("codex", Exit::Success, "");
    settle(&mut app);
    assert!(age_of(&stamp) < Duration::from_secs(60));
}

#[test]
fn a_missing_stamp_triggers_and_a_failed_attempt_still_refreshes_it() {
    let dir = Scratch::new("missing");
    let fake = Fake::new(&["codex"]);
    let stamp = stamp_path(&dir.0.join("cli-update"), "codex");
    assert!(!stamp.exists());
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "a"));
    fake.wait_updates("codex", 1);
    fake.finish("codex", Exit::Failed(Some(2)), "boom");
    settle(&mut app);
    assert!(age_of(&stamp) < Duration::from_secs(60));
    let released = app.pending.take().unwrap();
    assert!(
        matches!(app.cli_freshness.admit(released), Admit::Go(_)),
        "a failed attempt also counts for the 10 minutes"
    );
}

#[test]
fn an_unwritable_stamp_does_not_repeat_the_update_on_release() {
    let dir = Scratch::new("nostamp");
    let fake = Fake::new(&["codex"]);
    std::fs::write(dir.0.join("cli-update"), "a file, not a directory").unwrap();
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "a"));
    settle(&mut app);
    let released = app.pending.take().unwrap();
    assert!(matches!(app.cli_freshness.admit(released), Admit::Go(_)));
}

#[test]
fn a_failed_update_releases_the_launch_and_names_the_failure() {
    let dir = Scratch::new("fail");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "a"));
    fake.wait_updates("codex", 1);
    fake.finish(
        "codex",
        Exit::Failed(Some(2)),
        "first\n\u{1b}[31mboom: registry unreachable\u{1b}[0m\n",
    );
    settle(&mut app);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "a");
    assert!(
        app.status
            .starts_with("codex update failed — starting the installed version (exit 2"),
        "{}",
        app.status
    );
    assert!(
        app.status.contains("registry unreachable"),
        "{}",
        app.status
    );
    assert!(
        app.status.chars().all(|c| !c.is_control()),
        "{}",
        app.status
    );
}

#[test]
fn a_timed_out_update_releases_the_launch_and_says_so() {
    let dir = Scratch::new("timeout");
    let fake = Fake::new(&["omp"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Omp, "a"));
    fake.wait_updates("omp", 1);
    fake.finish("omp", Exit::TimedOut, "");
    settle(&mut app);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "a");
    assert!(
        app.status
            .starts_with("omp update failed — starting the installed version (timed out"),
        "{}",
        app.status
    );
}

#[test]
fn the_failure_notice_survives_the_account_check_that_follows_release() {
    let dir = Scratch::new("notice");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "a"));
    fake.finish("codex", Exit::Failed(Some(1)), "");
    settle(&mut app);
    let notice = app.status.clone();
    app.status = "work 로그인 확인 중…".into();
    app.show_release_notice("a");
    assert_eq!(app.status, notice);
    app.status = "later".into();
    app.show_release_notice("a");
    assert_eq!(app.status, "later", "a notice shows once per launch");
}

#[test]
fn another_cli_is_not_held_while_one_updates() {
    let dir = Scratch::new("other");
    let fake = Fake::new(&["codex", "claude"]);
    write_stamp(&dir, "claude", Duration::from_secs(5));
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "held"));
    fake.wait_updates("codex", 1);
    let other = launch(Agent::Claude, "free");
    assert!(matches!(app.cli_freshness.admit(other), Admit::Go(_)));
    assert_eq!(fake.updates("claude"), 0);
    assert!(app.cli_freshness.owns_launch("held"));
}

#[test]
fn a_second_stale_cli_updates_beside_the_first() {
    let dir = Scratch::new("beside");
    let fake = Fake::new(&["codex", "claude"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "x"));
    request(&mut app, launch(Agent::Claude, "y"));
    fake.wait_updates("codex", 1);
    fake.wait_updates("claude", 1);
    fake.finish("claude", Exit::Success, "");
    settle(&mut app);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "y");
    assert!(app.cli_freshness.owns_launch("x"));
}

#[test]
fn sign_in_panes_and_other_programs_are_never_held() {
    let dir = Scratch::new("never");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    for pending in [
        launch(Agent::Codex, "login:codex:default"),
        launch(Agent::Claude, "login:claude:work"),
        launch(Agent::Kiro, "k"),
        launch(Agent::Cursor, "c"),
        launch_command("sh"),
        launch_command("codex-cli"),
        launch_command("/usr/local/bin/codex"),
    ] {
        assert!(matches!(app.cli_freshness.admit(pending), Admit::Go(_)));
    }
    assert_eq!(fake.total_calls(), 0);
}

#[test]
fn closing_a_held_session_cancels_only_that_launch() {
    let dir = Scratch::new("close");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "closed"));
    request(&mut app, launch(Agent::Codex, "kept"));
    fake.wait_updates("codex", 1);
    app.remove_pane("closed");
    assert!(!app.cli_freshness.owns_launch("closed"));
    assert!(app.cli_freshness.owns_launch("kept"));
    fake.finish("codex", Exit::Success, "");
    settle(&mut app);
    assert_eq!(app.pending.as_ref().unwrap().session_id, "kept");
    assert!(app.pending_queue.is_empty());
}

#[test]
fn closing_the_only_held_session_spawns_nothing_later() {
    let dir = Scratch::new("closeall");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    request(&mut app, launch(Agent::Codex, "only"));
    fake.wait_updates("codex", 1);
    app.remove_pane("only");
    fake.finish("codex", Exit::Success, "");
    settle(&mut app);
    assert!(app.pending.is_none() && app.pending_queue.is_empty());
    assert!(app.ptys.is_empty());
}

#[test]
fn the_off_switch_values_disable_the_gate() {
    for off in ["0", "off", "false", "OFF", " False "] {
        assert!(!switch_enabled(Some(off)), "{off:?}");
    }
    for on in [None, Some("1"), Some("on"), Some("yes"), Some("")] {
        assert!(switch_enabled(on), "{on:?}");
    }
}

#[test]
fn a_disabled_gate_holds_nothing_and_tests_start_with_it_disabled() {
    let dir = Scratch::new("off");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, false);
    assert!(matches!(
        app.cli_freshness.admit(launch(Agent::Codex, "a")),
        Admit::Go(_)
    ));
    assert_eq!(fake.total_calls(), 0);
    assert!(!super::tests::isolated_app().cli_freshness.is_enabled());
}

#[test]
fn a_launch_waiting_for_the_lock_skips_its_run_once_the_stamp_is_fresh() {
    let dir = Scratch::new("lock");
    let fake = Fake::new(&["codex"]);
    let root = dir.0.join("cli-update");
    let lock = mindplayer_core::private::open_private(&root.join("codex.lock"), true).unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (runner, job_root) = (fake.runner.clone(), root.clone());
    let job = thread::spawn(move || run_job(&job_root, "codex", &runner, Duration::from_secs(5)));
    thread::sleep(Duration::from_millis(150));
    assert!(!job.is_finished(), "must wait for the other instance");
    write_stamp(&dir, "codex", Duration::ZERO);
    drop(lock);
    assert_eq!(job.join().unwrap(), Finished::Skipped);
    assert_eq!(fake.total_calls(), 0);
}

#[test]
fn a_lock_that_stays_busy_ends_the_job_as_a_failure_within_the_budget() {
    let dir = Scratch::new("busy");
    let fake = Fake::new(&["codex"]);
    let root = dir.0.join("cli-update");
    let lock = mindplayer_core::private::open_private(&root.join("codex.lock"), true).unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let started = Instant::now();
    let finished = run_job(&root, "codex", &fake.runner, Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(matches!(finished, Finished::Failed(ref why) if why.contains("lock")));
    assert_eq!(fake.total_calls(), 0);
    assert!(!stamp_path(&root, "codex").exists());
}

#[test]
fn the_job_writes_owner_only_files_and_runs_the_exact_argv() {
    let dir = Scratch::new("modes");
    let fake = Fake::new(&["omp"]);
    let root = dir.0.join("cli-update");
    fake.finish("omp", Exit::Success, "");
    let finished = run_job(&root, "omp", &fake.runner, Duration::from_secs(5));
    assert!(matches!(finished, Finished::Ran(_)));
    for name in ["omp.lock", "omp.stamp"] {
        let mode = std::fs::metadata(root.join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
    let calls = fake.calls.lock().unwrap().clone();
    let strings = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            strings(&["omp", "--version"]),
            strings(&["omp", "update"]),
            strings(&["omp", "--version"]),
        ]
    );
}

#[test]
fn a_held_launch_never_makes_the_spawn_loop_spin() {
    let dir = Scratch::new("spin");
    let fake = Fake::new(&["codex"]);
    let mut app = app_with(&dir, &fake, true);
    app.enqueue_spawn(launch(Agent::Codex, "a"));
    app.enqueue_spawn(launch(Agent::Codex, "b"));
    let mut rounds = 0;
    while app.pending.is_some() {
        app.spawn_pending();
        rounds += 1;
        assert!(rounds <= 2);
    }
    assert!(app.cli_freshness.owns_launch("a") && app.cli_freshness.owns_launch("b"));
}

fn stub(dir: &Scratch, name: &str, body: &str) -> String {
    let path = dir.0.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

fn run_stub(dir: &Scratch, program: &str, timeout: Duration) -> Ran {
    process_runner(
        &[program.to_string(), "update".to_string()],
        timeout,
        &dir.0.join("capture.out"),
        None,
    )
}

fn wait_for_pid(file: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "the stub never reported its pid");
        thread::sleep(Duration::from_millis(20));
    }
}

fn try_lock(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[test]
fn the_process_runner_starts_the_updater_in_a_session_of_its_own() {
    let dir = Scratch::new("run-session");
    let pid_file = dir.0.join("tool.pid");
    let program = stub(
        &dir,
        "tool",
        &format!("echo $$ > '{}'\nsleep 1", pid_file.display()),
    );
    let (capture, argv) = (dir.0.join("capture.out"), vec![program, "update".into()]);
    let run = thread::spawn(move || process_runner(&argv, Duration::from_secs(10), &capture, None));
    let pid = wait_for_pid(&pid_file);
    assert_eq!(
        unsafe { libc::getsid(pid) },
        pid,
        "an updater that leads its own session has no terminal to read or draw on"
    );
    assert_eq!(run.join().unwrap().exit, Exit::Success);
}

#[test]
fn a_timed_out_updater_is_asked_to_stop_before_it_is_killed() {
    let dir = Scratch::new("run-term");
    let marker = dir.0.join("stopped");
    let program = stub(
        &dir,
        "tool",
        &format!(
            "trap 'echo stopped > \"{}\"; exit 0' TERM\nwhile :; do sleep 0.05; done",
            marker.display()
        ),
    );
    let ran = run_stub(&dir, &program, Duration::from_secs(3));
    assert_eq!(ran.exit, Exit::TimedOut);
    assert!(
        marker.exists(),
        "the updater had the chance to stop cleanly"
    );
}

#[test]
fn the_updater_keeps_the_update_lock_after_the_app_lets_go_of_it() {
    use std::os::fd::AsRawFd;
    let dir = Scratch::new("run-lock");
    let lock_path = dir.0.join("tool.lock");
    let pid_file = dir.0.join("tool.pid");
    let program = stub(
        &dir,
        "tool",
        &format!("echo $$ > '{}'\nsleep 2", pid_file.display()),
    );
    let lock = mindplayer_core::private::open_private(&lock_path, true).unwrap();
    assert!(try_lock(&lock));
    let held = lock.as_raw_fd();
    let (capture, argv) = (dir.0.join("capture.out"), vec![program, "update".into()]);
    let run =
        thread::spawn(move || process_runner(&argv, Duration::from_secs(10), &capture, Some(held)));
    wait_for_pid(&pid_file);
    drop(lock);
    let other = mindplayer_core::private::open_private(&lock_path, true).unwrap();
    assert!(
        !try_lock(&other),
        "a second instance must wait while the first one's updater still runs"
    );
    assert_eq!(run.join().unwrap().exit, Exit::Success);
    assert!(try_lock(&other), "the lock is free once the updater exits");
}

#[test]
fn the_process_runner_reports_success_with_captured_output() {
    let dir = Scratch::new("run-ok");
    let program = stub(&dir, "tool", "echo \"ran $1\"; echo noise >&2; exit 0");
    let ran = run_stub(&dir, &program, Duration::from_secs(5));
    assert_eq!(ran.exit, Exit::Success);
    assert!(ran.output.contains("ran update") && ran.output.contains("noise"));
    assert!(!dir.0.join("capture.out").exists(), "capture file removed");
}

#[test]
fn the_process_runner_reports_a_non_zero_exit() {
    let dir = Scratch::new("run-fail");
    let program = stub(&dir, "tool", "echo oops >&2; exit 3");
    let ran = run_stub(&dir, &program, Duration::from_secs(5));
    assert_eq!(ran.exit, Exit::Failed(Some(3)));
    assert!(ran.output.contains("oops"));
}

#[test]
fn the_process_runner_gives_the_child_a_null_stdin_and_bounds_its_output() {
    let dir = Scratch::new("run-bounded");
    let program = stub(
        &dir,
        "tool",
        "if read line; then exit 9; fi\nhead -c 300000 /dev/zero | tr '\\0' x",
    );
    let ran = run_stub(&dir, &program, Duration::from_secs(10));
    assert_eq!(ran.exit, Exit::Success);
    assert_eq!(ran.output.len(), 64 * 1024);
}

#[test]
fn the_process_runner_reports_a_program_that_cannot_start() {
    let dir = Scratch::new("run-missing");
    let ran = run_stub(
        &dir,
        "/nonexistent/mindplayer-fixture-tool",
        Duration::from_secs(1),
    );
    assert!(matches!(ran.exit, Exit::NotRun(_)), "{ran:?}");
}

#[test]
fn a_timed_out_process_runner_kills_the_whole_process_group() {
    let dir = Scratch::new("run-timeout");
    let pid_file = dir.0.join("grandchild.pid");
    let program = stub(
        &dir,
        "tool",
        &format!("sleep 30 &\necho $! > '{}'\nwait", pid_file.display()),
    );
    let started = Instant::now();
    let ran = run_stub(&dir, &program, Duration::from_secs(3));
    assert_eq!(ran.exit, Exit::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(8));
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "grandchild {pid} survived");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_process_the_updater_leaves_behind_does_not_keep_the_update_lock() {
    use std::os::fd::AsRawFd;
    let dir = Scratch::new("run-leftover");
    let lock_path = dir.0.join("tool.lock");
    let pid_file = dir.0.join("leftover.pid");
    let program = stub(
        &dir,
        "tool",
        &format!("sleep 30 &\necho $! > '{}'\nexit 0", pid_file.display()),
    );
    let lock = mindplayer_core::private::open_private(&lock_path, true).unwrap();
    assert!(try_lock(&lock));
    let ran = process_runner(
        &[program, "update".to_string()],
        Duration::from_secs(10),
        &dir.0.join("capture.out"),
        Some(lock.as_raw_fd()),
    );
    assert_eq!(ran.exit, Exit::Success);
    let leftover = wait_for_pid(&pid_file);
    assert_eq!(
        unsafe { libc::kill(leftover, 0) },
        0,
        "the leftover still runs"
    );
    drop(lock);
    let other = mindplayer_core::private::open_private(&lock_path, true).unwrap();
    let free = try_lock(&other);
    unsafe { libc::kill(leftover, libc::SIGKILL) };
    assert!(free, "only the updater itself may hold the lock");
}

#[test]
fn an_updater_that_is_not_installed_fails_without_stopping_the_launch() {
    let dir = Scratch::new("run-absent");
    let lock = mindplayer_core::private::open_private(&dir.0.join("tool.lock"), true).unwrap();
    use std::os::fd::AsRawFd;
    let ran = process_runner(
        &[
            "/nonexistent/mindplayer-fixture-tool".to_string(),
            "update".to_string(),
        ],
        Duration::from_secs(5),
        &dir.0.join("capture.out"),
        Some(lock.as_raw_fd()),
    );
    assert!(
        matches!(ran.exit, Exit::NotRun(_) | Exit::Failed(Some(127))),
        "{ran:?}"
    );
}

#[test]
fn version_lines_reduce_to_the_version_token() {
    for (line, label) in [
        ("codex-cli 0.170.0", "0.170.0"),
        ("2.1.303 (Claude Code)", "2.1.303"),
        ("omp/14.0.1", "14.0.1"),
        ("nightly", "nightly"),
    ] {
        assert_eq!(version_label(line), label, "{line}");
    }
}
