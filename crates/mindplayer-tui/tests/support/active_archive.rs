//! Offline executable contracts: actual outer PTY, rendered acknowledgements,
//! native child ownership, durable restore, and immutable transcript sources.

use super::*;
use std::sync::mpsc;

const INDEX_TIMEOUT: Duration = Duration::from_secs(180);
const MEASURED_DURATION: Duration = Duration::from_secs(130);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const CATALOG_CYCLE: Duration = Duration::from_secs(60);
const OLD_ID: &str = "archive-native-0001";
const OLD_TITLE: &str = "MPARCHIVERESTORE";
const OUTPUT_CHILD: &str = "active_archive::continuous_output_child";

fn complete_evaluation_report() -> serde_json::Value {
    serde_json::json!({
        "children": 7, "archive_sources": 5_000, "measured_seconds": 130.0,
        "samples": 1_301, "p50_ms": 20.0, "p95_ms": 20.0, "p99_ms": 20.0, "max_ms": 20.0,
        "lost_inputs": 0, "duplicate_inputs": 0, "deadlocks": 0,
        "cpu_seconds": 13.0, "peak_sampled_tree_rss_kib": 1_024,
        "source_bytes_verified": true, "native_cleanup": true, "provider_requests": 0,
        "growth": {"ticks": 130, "files": 11, "initial_bytes": 11_000_000, "appended_bytes": 23_000_000},
        "refreshes": [
            {"cycle": 1, "created_seconds": 5.0, "observed_seconds": 60.0,
             "row_rendered": true, "restored": true, "native_children_after": 7},
            {"cycle": 2, "created_seconds": 65.0, "observed_seconds": 120.0,
             "row_rendered": true, "restored": true, "native_children_after": 7}
        ],
        "latency_timeline": (0..=1_300).map(|i| serde_json::json!({
            "at_seconds": f64::from(i) / 10.0, "ms": 20.0,
            "phase": if i == 600 || i == 1_200 { "archive_search" }
                else if i < 650 { "split" } else { "zoom" }
        })).collect::<Vec<_>>(),
        "resource_timeline": (0..=130).map(|i| serde_json::json!({
            "at_seconds": f64::from(i), "cpu_seconds": f64::from(i) / 10.0,
            "rss_kib": 1_024, "processes": 8
        })).collect::<Vec<_>>()
    })
}

fn check_evaluation_report(report: &serde_json::Value) -> std::process::Output {
    let fixture = unique_tmp();
    let log = fixture.join("evaluation.log");
    std::fs::write(&log, format!("ACTIVE_ARCHIVE_EVALUATION={report}\n"))
        .expect("write synthetic evaluator evidence");
    std::process::Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/verify-active-archive.sh"))
        .args(["--check-report", log.to_str().expect("fixture log path")])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("test tool PATH"))
        .env("HOME", &*fixture)
        .output()
        .expect("validate captured performance evidence without launching the app")
}

#[test]
fn performance_report_accepts_two_refresh_cycles_with_typing_and_growth() {
    let output = check_evaluation_report(&complete_evaluation_report());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn performance_report_rejects_a_short_measurement() {
    let mut report = complete_evaluation_report();
    report["measured_seconds"] = 1.88.into();
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("duration must be at least 130s"));
    assert!(!output.status.success());
}

#[test]
fn performance_report_rejects_a_missing_second_adoption() {
    let mut report = complete_evaluation_report();
    report["refreshes"].as_array_mut().unwrap().pop();
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("two catalog adoptions"));
    assert!(!output.status.success());
}

#[test]
fn performance_report_rejects_p99_above_100ms() {
    let mut report = complete_evaluation_report();
    report["p99_ms"] = 100.01.into();
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("p99 exceeds 100ms"));
    assert!(!output.status.success());
}

#[test]
fn performance_report_rejects_max_above_500ms() {
    let mut report = complete_evaluation_report();
    report["max_ms"] = 500.01.into();
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("max exceeds 500ms"));
    assert!(!output.status.success());
}

#[test]
fn performance_report_accepts_latency_at_the_exact_engineering_limits() {
    let mut report = complete_evaluation_report();
    for (i, sample) in report["latency_timeline"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        sample["ms"] = if i < 1_288 { 100.0 } else { 500.0 }.into();
    }
    for field in ["p50_ms", "p95_ms", "p99_ms"] {
        report[field] = 100.0.into();
    }
    report["max_ms"] = 500.0.into();
    let output = check_evaluation_report(&report);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn performance_report_rejects_an_idle_refresh_window() {
    let mut report = complete_evaluation_report();
    report["latency_timeline"]
        .as_array_mut()
        .unwrap()
        .retain(|point| !(58.0..=62.0).contains(&point["at_seconds"].as_f64().unwrap()));
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("typing must overlap each adoption"));
    assert!(!output.status.success());
}

#[test]
fn performance_report_rejects_missing_resource_samples() {
    let mut report = complete_evaluation_report();
    report["resource_timeline"] = serde_json::json!([]);
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("CPU/RSS timeline"));
    assert!(!output.status.success());
}

#[test]
fn performance_report_rejects_unchanged_transcripts() {
    let mut report = complete_evaluation_report();
    report["growth"]["appended_bytes"] = 0.into();
    let output = check_evaluation_report(&report);
    assert!(String::from_utf8_lossy(&output.stderr).contains("transcripts must grow"));
    assert!(!output.status.success());
}

/// Keep native fixture children bounded even if a screen assertion unwinds.
/// Passing cleanup is checked *before* this fallback can mask a product leak.
struct OfflineApp(Mp, Vec<u32>);

impl Drop for OfflineApp {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("OFFLINE_NATIVE_LAUNCHES={:?}", self.0.native_launches());
            eprintln!("OFFLINE_CONSUMED_INPUTS={:?}", input_tokens(&self.0));
            let logs = self.0._tmp.join("home/.mindplayer/logs");
            if let Ok(entries) = std::fs::read_dir(logs) {
                for entry in entries.flatten() {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        eprintln!(
                            "OFFLINE_CHILD_STDERR {}: {}",
                            entry.file_name().to_string_lossy(),
                            String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(8192)..])
                        );
                    }
                }
            }
        }
        let mut pids = self.0.native_launches();
        pids.extend(&self.1);
        pids.sort_unstable();
        pids.dedup();
        if matches!(self.0.child.try_wait(), Ok(None)) {
            let _ = self.0.writer.write_all(b"\x18q");
            let _ = self.0.writer.flush();
            let deadline = Instant::now() + Duration::from_secs(2);
            while matches!(self.0.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = self.0.child.kill();
            let _ = self.0.child.wait();
        }
        for pid in pids {
            if fixture_process_exists(pid) {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
            }
        }
    }
}

impl Mp {
    fn restart_offline_fixture(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_some(),
            "restart requires natural app exit"
        );
        assert_native_cleanup(self, &self.native_launches());
        let launches = self._tmp.join("native-launches.log");
        if launches.exists() {
            std::fs::remove_file(launches).unwrap();
        }
        for entry in std::fs::read_dir(&self.codex_marker_dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        assert_no_provider_requests(self);
        // Launch markers describe this run only. Preserve all provider-turn
        // logs so restarting cannot conceal a request from the first run.
        for name in ["private-ui", "ui.pid"] {
            let path = self.router_marker(name);
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        let (rows, cols) = self.parser.lock().unwrap().screen().size();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open restart outer PTY");
        let child = FixtureChild(
            pair.slave
                .spawn_command(self.command.clone())
                .expect("restart exact binary"),
        );
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        self.writer = pair.master.take_writer().unwrap();
        self.child = child;
        self._master = pair.master;
        self.parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        self.raw_output = Arc::new(Mutex::new(Vec::new()));
        let parser = Arc::clone(&self.parser);
        let raw = Arc::clone(&self.raw_output);
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 {
                    break;
                }
                raw.lock().unwrap().extend_from_slice(&bytes[..n]);
                parser.lock().unwrap().process(&bytes[..n]);
            }
        });
    }
}

fn offline_fixture(prepare: impl FnOnce(&Path)) -> OfflineApp {
    assert!(
        std::env::var_os("MINDPLAYER_TEST_REAL_JEV").is_none_or(|v| v != "1"),
        "offline evaluator refuses live Jev opt-in"
    );
    assert!(
        std::env::var_os("MINDPLAYER_TEST_PRIVATE_CODEX").is_none(),
        "offline evaluator requires synthetic Codex"
    );
    OfflineApp(
        Mp::launch_with_fixture_hooks(
            (60, 240),
            false,
            false,
            false,
            false,
            CodexFixture::Echo,
            (
                |root| {
                    // MINDPLAYER_STATE is a file; older smoke fixtures intentionally
                    // used an empty directory. These cases exercise durable state.
                    std::fs::remove_dir(root.join("state"))
                        .expect("remove empty task-owned state fixture");
                    prepare(root);
                },
                |_, _| {},
            ),
        ),
        Vec::new(),
    )
}

fn source_fingerprint(mp: &Mp) -> serde_json::Value {
    immutable_source_fingerprint(mp, &[])
}

fn immutable_source_fingerprint(mp: &Mp, growing: &[PathBuf]) -> serde_json::Value {
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "--eval", r#"
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
const root = process.argv[1], hash = createHash('sha256');
const growing = new Set(JSON.parse(process.argv[2]));
let count = 0;
function visit(relative) {
  for (const entry of readdirSync(join(root, relative), { withFileTypes: true }).sort((a,b) => a.name.localeCompare(b.name))) {
    const name = join(relative, entry.name);
    if (entry.isDirectory()) visit(name);
    else {
      if (!entry.isFile()) throw new Error('fixture source must be an ordinary file');
      if (growing.has(join(root, name))) continue;
      const bytes = readFileSync(join(root, name));
      hash.update(JSON.stringify([name, bytes.length]) + '\n').update(bytes);
      count++;
    }
  }
}

visit('');
process.stdout.write(JSON.stringify({ files: count, sha256: hash.digest('hex') }));
"#])
        .arg(mp._tmp.join("codex"))
        .arg(serde_json::to_string(growing).expect("explicit fixture-owned mutable sources"))
        .env_remove("NODE_OPTIONS")
        .output()
        .expect("hash exact synthetic transcript bytes");
    assert!(
        output.status.success(),
        "fixture hashing failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("fixture source SHA-256")
}

fn assert_no_provider_requests(mp: &Mp) {
    // Native Codex still prepares its local router while Jev is OFF. Its
    // existence is not a provider request; these are the fixture's actual
    // inference/judgment consumers, and neither may receive a turn here.
    for name in ["jev.jsonl", "routes.jsonl"] {
        assert!(
            std::fs::read(mp.router_marker(name))
                .unwrap_or_default()
                .is_empty(),
            "offline echo must never request a provider turn: {name}"
        );
    }
}

fn assert_native_cleanup(mp: &Mp, pids: &[u32]) {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let alive: Vec<_> = pids
            .iter()
            .copied()
            .filter(|pid| fixture_process_exists(*pid))
            .collect();
        if alive.is_empty() {
            return;
        }
        assert!(Instant::now() < deadline, "natural app exit leaked native fixture children {alive:?}; fallback cleanup has not run");
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            mp.child.process_id().is_some(),
            "outer fixture has an owned PID"
        );
    }
}

fn filter_archive(mp: &mut Mp, title: &str) {
    mp.send(b"a/");
    mp.expect("· archived · /", STEP_TIMEOUT, "archive search opened");
    mp.send(title.as_bytes());
    mp.expect(title, INDEX_TIMEOUT, "archive search result rendered");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |s| s
            .lines()
            .any(|l| l.contains(title) && !l.contains('/') && l.contains("codex"))),
        "matching archive row, rather than only typed search text, must render\n{}",
        mp.screen()
    );
}

#[test]
fn active_archive_enter_restores_the_original_native_id() {
    let mut fixture = offline_fixture(|root| {
        seed_codex_session_at(
            &root.join("codex"),
            &root.join("scope"),
            OLD_ID,
            OLD_TITLE,
            chrono::Utc::now() - chrono::Duration::days(60),
        );
    });
    let mp = &mut fixture.0;
    let originals = source_fingerprint(mp);
    mp.start_into_main_list_with_count(2, INDEX_TIMEOUT);
    assert!(
        !mp.screen().contains(OLD_TITLE),
        "old human activity is absent from Active"
    );
    filter_archive(mp, OLD_TITLE);
    mp.send(b"\r");
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "archive hydration resumed the native child",
    );
    assert_single_codex_resume(mp, &mp._tmp.join("home/.codex"), OLD_ID);
    assert!(
        !mp.screen().contains("· archived"),
        "restore returns to Active"
    );
    mp.send(b"\x18");
    fixture.1 = resources(mp.child.process_id().unwrap())
        .into_keys()
        .collect();
    mp.quit_from_list();
    assert_native_cleanup(mp, &fixture.1);
    assert_eq!(
        source_fingerprint(mp),
        originals,
        "restore never rewrites provider transcripts"
    );
    assert_no_provider_requests(mp);
}

#[test]
fn active_archive_restored_membership_survives_a_restart() {
    let mut fixture = offline_fixture(|root| {
        seed_codex_session_at(
            &root.join("codex"),
            &root.join("scope"),
            OLD_ID,
            OLD_TITLE,
            chrono::Utc::now() - chrono::Duration::days(60),
        );
    });
    let mp = &mut fixture.0;
    let originals = source_fingerprint(mp);
    mp.start_into_main_list_with_count(2, INDEX_TIMEOUT);
    filter_archive(mp, OLD_TITLE);
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "first restore completed");
    mp.send(b"\x18");
    fixture.1 = resources(mp.child.process_id().unwrap())
        .into_keys()
        .collect();
    mp.quit_from_list();
    assert_native_cleanup(mp, &fixture.1);
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(mp._tmp.join("state")).expect("durable isolated state"),
    )
    .unwrap();
    assert!(
        saved["last_used"][OLD_ID].is_string(),
        "restore persists actual human use: {saved}"
    );
    mp.restart_offline_fixture();
    mp.start_into_main_list_with_count(3, INDEX_TIMEOUT);
    mp.expect(
        OLD_TITLE,
        STEP_TIMEOUT,
        "restored session stays Active after restart",
    );
    mp.send(format!("/{OLD_TITLE}\r").as_bytes());
    mp.expect(PANE_READY, STEP_TIMEOUT, "restarted Active row resumes");
    assert_single_codex_resume(mp, &mp._tmp.join("home/.codex"), OLD_ID);
    mp.send(b"\x18");
    fixture.1 = resources(mp.child.process_id().unwrap())
        .into_keys()
        .collect();
    mp.quit_from_list();
    assert_native_cleanup(mp, &fixture.1);
    assert_eq!(
        source_fingerprint(mp),
        originals,
        "restart preserves source bytes"
    );
    assert_no_provider_requests(mp);
}

/// Executed only as a fake provider inside a native PTY. TTY echo is disabled:
/// an ACK proves the child consumed the input and painted a reply.
#[test]
#[ignore = "offline native child used by the executable latency evaluator"]
fn continuous_output_child() {
    let markers =
        PathBuf::from(std::env::var_os("MINDPLAYER_TEST_MARKER_DIR").expect("isolated markers"));
    let id = std::env::var("MINDPLAYER_PANE_ID").expect("native pane id");
    let root = markers.parent().expect("fixture root");
    assert!(
        root.join("continuous-output-fixture").is_file(),
        "only the isolated load fixture may run this child"
    );
    let status = std::process::Command::new("stty")
        .args(["raw", "-echo"])
        .status()
        .expect("disable native line echo");
    assert!(status.success(), "configure child terminal");
    let input_log = root.join(format!("input-{id}.log"));
    let heartbeat = root.join(format!("heartbeat-{id}"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut byte = [0];
        let mut line = Vec::new();
        let mut log = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(input_log)
            .unwrap();
        while stdin.read_exact(&mut byte).is_ok() {
            if matches!(byte[0], b'\r' | b'\n') {
                if !line.is_empty() {
                    let token = String::from_utf8(std::mem::take(&mut line)).unwrap();
                    writeln!(log, "{token}").unwrap();
                    log.flush().unwrap();
                    if tx.send(token).is_err() {
                        break;
                    }
                }
            } else {
                line.push(byte[0]);
            }
        }
    });
    let mut stdout = std::io::stdout().lock();
    let mut last = String::new();
    let mut frame = 0_u64;
    loop {
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(token) => last = token,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        write!(stdout, "\x1b[2;1H").unwrap();
        for row in 0..8 {
            write!(
                stdout,
                "\x1b[2KLOAD {frame:08} ROW {row} {}\r\n",
                "0123456789abcdef".repeat(6)
            )
            .unwrap();
        }
        // The same eight full-width load lines can scroll in a small split.
        // Paint the acknowledgement last, so it stays visible at every pane
        // geometry without reducing the continuous output byte volume.
        write!(stdout, "\x1b[H\x1b[2KPANE-READY ACK:{last}").unwrap();
        stdout.flush().unwrap();
        if frame.is_multiple_of(4) {
            let staged = root.join(format!(".heartbeat-{id}"));
            std::fs::write(&staged, frame.to_string()).unwrap();
            std::fs::rename(staged, &heartbeat).unwrap();
        }
        frame += 1;
    }
}

#[derive(Clone, Copy, Default)]
struct Resources {
    cpu_seconds: f64,
    rss_kib: u64,
}

/// Observe descendants, including the native UI and its local router/backend,
/// without inspecting command lines or any other user's process environment.
fn resources(root: u32) -> BTreeMap<u32, Resources> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,time=,rss="])
        .output()
        .expect("sample app process tree CPU/RSS");
    assert!(output.status.success(), "process tree sampling failed");
    let mut processes = BTreeMap::new();
    let text = String::from_utf8(output.stdout).unwrap();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "ps pid/ppid/time/rss fields: {line}");
        let pid = fields[0].parse::<u32>().expect("ps PID");
        let parent = fields[1].parse::<u32>().expect("ps parent PID");
        // ps also lists unrelated long-lived processes. procps can prefix
        // their CPU clocks with days even though the owned children are new.
        let (days, clock) = fields[2]
            .split_once('-')
            .map_or((0.0, fields[2]), |(days, clock)| {
                (days.parse::<f64>().expect("ps CPU days"), clock)
            });
        let cpu_seconds = clock
            .split(':')
            .try_fold(0.0, |total, part| {
                part.parse::<f64>().map(|value| total * 60.0 + value)
            })
            .expect("ps CPU clock")
            + days * 86_400.0;
        let rss_kib = fields[3].parse::<u64>().expect("ps RSS KiB");
        processes.insert(
            pid,
            (
                parent,
                Resources {
                    cpu_seconds,
                    rss_kib,
                },
            ),
        );
    }
    let mut tree = BTreeMap::from([(
        root,
        processes.get(&root).expect("outer app must be running").1,
    )]);
    loop {
        let previous = tree.len();
        for (&pid, &(parent, sample)) in &processes {
            if tree.contains_key(&parent) {
                tree.insert(pid, sample);
            }
        }
        if tree.len() == previous {
            return tree;
        }
    }
}

struct ResourceTracker {
    baseline: BTreeMap<u32, f64>,
    latest: BTreeMap<u32, f64>,
    peak_rss_kib: u64,
    peak_processes: usize,
}

impl ResourceTracker {
    fn new(sample: &BTreeMap<u32, Resources>) -> Self {
        let baseline = sample
            .iter()
            .map(|(&pid, value)| (pid, value.cpu_seconds))
            .collect();
        let mut tracker = Self {
            baseline,
            latest: BTreeMap::new(),
            peak_rss_kib: 0,
            peak_processes: 0,
        };
        tracker.observe(sample);
        tracker
    }

    fn observe(&mut self, sample: &BTreeMap<u32, Resources>) {
        for (&pid, value) in sample {
            self.latest
                .entry(pid)
                .and_modify(|cpu| *cpu = cpu.max(value.cpu_seconds))
                .or_insert(value.cpu_seconds);
        }
        self.peak_rss_kib = self
            .peak_rss_kib
            .max(sample.values().map(|v| v.rss_kib).sum());
        self.peak_processes = self.peak_processes.max(sample.len());
    }

    fn cpu_seconds(&self) -> f64 {
        self.latest
            .iter()
            .map(|(pid, latest)| latest - self.baseline.get(pid).unwrap_or(&0.0))
            .sum()
    }
}

fn heartbeat_values(mp: &Mp) -> BTreeMap<String, u64> {
    std::fs::read_dir(&mp._tmp)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name()?.to_str()?.to_string();
            if !name.starts_with("heartbeat-") {
                return None;
            }
            let value = std::fs::read_to_string(path).ok()?.parse().ok()?;
            Some((name, value))
        })
        .collect()
}

fn input_tokens(mp: &Mp) -> Vec<String> {
    let mut tokens = Vec::new();
    for entry in std::fs::read_dir(&mp._tmp).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("input-")
        {
            tokens.extend(
                std::fs::read_to_string(path)
                    .unwrap()
                    .lines()
                    .map(str::to_string),
            );
        }
    }
    tokens.sort();
    tokens
}

/// Only the fixture writes these append-only sources. Exact expected bytes
/// catch app rewrites too; excluding mutable files from the census alone would
/// silently hide corruption of the original prefix or fixture-owned suffix.
struct TranscriptGrowth {
    sources: Vec<(PathBuf, Vec<u8>)>,
    chunk: Vec<u8>,
    initial_bytes: usize,
    ticks: usize,
}

impl TranscriptGrowth {
    fn new(paths: Vec<PathBuf>) -> Self {
        let line = format!(
            "{}\n",
            serde_json::json!({
                "type": "response_item", "timestamp": chrono::Utc::now(),
                "payload": {"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "synthetic output ".repeat(1_024)}]}
            })
        );
        let chunk = line.into_bytes();
        let mut sources = Vec::new();
        for path in paths {
            let mut expected = std::fs::read(&path).expect("read seeded synthetic transcript");
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            while expected.len() < 1_048_576 {
                file.write_all(&chunk).unwrap();
                expected.extend_from_slice(&chunk);
            }
            sources.push((path, expected));
        }
        let initial_bytes = sources.iter().map(|(_, bytes)| bytes.len()).sum();
        Self {
            sources,
            chunk,
            initial_bytes,
            ticks: 0,
        }
    }

    fn append(&mut self) {
        for (path, expected) in &mut self.sources {
            std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(&self.chunk)
                .expect("append only to task-owned transcript");
            expected.extend_from_slice(&self.chunk);
        }
        self.ticks += 1;
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.sources.iter().map(|(path, _)| path.clone()).collect()
    }

    fn verify(&self) -> serde_json::Value {
        for (path, expected) in &self.sources {
            let actual = std::fs::read(path).expect("fixture transcript must still exist");
            assert!(
                actual == *expected,
                "app changed fixture-owned transcript bytes: {}",
                path.display()
            );
        }
        serde_json::json!({
            "files": self.sources.len(), "ticks": self.ticks, "initial_bytes": self.initial_bytes,
            "appended_bytes": self.ticks * self.chunk.len() * self.sources.len()
        })
    }
}

#[test]
fn transcript_growth_verifies_the_original_prefix_and_every_append() {
    let fixture = unique_tmp();
    let path = seed_codex_session(&fixture, &fixture, "growth", "GROWTH");
    let mut growth = TranscriptGrowth::new(vec![path]);
    growth.append();
    growth.append();
    let evidence = growth.verify();
    assert_eq!(evidence["ticks"], 2);
    assert!(evidence["appended_bytes"].as_u64().unwrap() > 32_768);
}

#[test]
#[should_panic(expected = "app changed fixture-owned transcript bytes")]
fn transcript_growth_rejects_a_rewritten_original_prefix() {
    use std::io::{Seek, SeekFrom};
    let fixture = unique_tmp();
    let path = seed_codex_session(&fixture, &fixture, "growth", "GROWTH");
    let mut growth = TranscriptGrowth::new(vec![path.clone()]);
    growth.append();
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"!").unwrap();
    growth.verify();
}

#[derive(Debug, serde::Serialize)]
struct LatencySample {
    at_seconds: f64,
    ms: f64,
    phase: &'static str,
}

fn measured_input(
    mp: &mut Mp,
    bytes: &[u8],
    expected: &str,
    started: Instant,
    phase: &'static str,
) -> LatencySample {
    let typed_at = Instant::now();
    mp.send(bytes);
    while !mp.screen().contains(expected) {
        assert!(
            typed_at.elapsed() < Duration::from_secs(2),
            "input deadlock/loss waiting for {expected:?} ({phase})\n{}",
            mp.screen()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    LatencySample {
        at_seconds: typed_at.duration_since(started).as_secs_f64(),
        ms: typed_at.elapsed().as_secs_f64() * 1_000.0,
        phase,
    }
}

fn archive_row(screen: &str, title: &str) -> bool {
    screen
        .lines()
        .any(|line| line.contains(title) && !line.contains('/') && line.contains("codex"))
}

fn measured_archive_search(
    mp: &mut Mp,
    title: &str,
    started: Instant,
    samples: &mut Vec<LatencySample>,
) -> bool {
    mp.send(b"a/");
    mp.expect(
        "· archived · /",
        STEP_TIMEOUT,
        "open measured Archive search",
    );
    let mut query = String::new();
    for byte in title.bytes() {
        query.push(char::from(byte));
        samples.push(measured_input(
            mp,
            &[byte],
            &format!("· archived · /{query}"),
            started,
            "archive_search",
        ));
    }
    let ready = mp.wait_until(Duration::from_millis(250), |screen| {
        archive_row(screen, title)
    });
    if !ready {
        eprintln!(
            "Archive row {title:?} missed the 250ms deadline:\n{}",
            mp.screen()
        );
    }
    ready
}

fn leave_archive_search(mp: &mut Mp) {
    mp.send(b"\x1b");
    mp.expect_absent("· archived · /", STEP_TIMEOUT, "leave Archive search");
    mp.send(b"a");
    mp.expect("· active", STEP_TIMEOUT, "return to Active");
}

/// Replace one explicitly selected synthetic child so each workload returns
/// to exactly 7/10 native children, even at the ten-pane limit.
fn restore_probe(
    mp: &mut Mp,
    replace: (&str, &str),
    probe: (&str, &str),
    started: Instant,
    samples: &mut Vec<LatencySample>,
) -> f64 {
    leave_archive_search(mp);
    let old_marker = mp.codex_marker_dir.join(format!("{}.txt", replace.0));
    let old_pid: u32 = std::fs::read_to_string(old_marker)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("PID="))
        .expect("owned synthetic PID")
        .parse()
        .unwrap();
    mp.send(format!("/{}\r", replace.1).as_bytes());
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "focus the synthetic child selected for replacement",
    );
    mp.send(b"\x18");
    mp.expect("· active", STEP_TIMEOUT, "detach selected synthetic child");
    mp.send(b"x");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(old_pid)),
        "selected synthetic child must exit before restore"
    );
    assert!(
        measured_archive_search(mp, probe.1, started, samples),
        "adopted source remains searchable"
    );
    let restored_at = Instant::now();
    let launches = mp.native_launches().len();
    mp.send(b"\r");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| {
            screen.contains(PANE_READY)
                && !screen.contains("· archived")
                && mp.native_launches().len() == launches + 1
                && heartbeat_values(mp).contains_key(&format!("heartbeat-{}", probe.0))
        }),
        "Archive restore must launch exactly one child and leave search\n{}",
        mp.screen()
    );
    let marker =
        std::fs::read_to_string(mp.codex_marker_dir.join(format!("{}.txt", probe.0))).unwrap();
    let args: Vec<_> = marker
        .lines()
        .filter_map(|line| line.strip_prefix("ARG="))
        .collect();
    assert!(
        args.windows(2).any(|args| args == ["resume", probe.0]),
        "restore must resume exact native identity: {args:?}"
    );
    restored_at.elapsed().as_secs_f64() * 1_000.0
}

fn observe_resources(
    root: u32,
    tracker: &mut ResourceTracker,
    started: Instant,
    timeline: &mut Vec<serde_json::Value>,
) -> BTreeMap<u32, Resources> {
    let current = resources(root);
    tracker.observe(&current);
    timeline.push(serde_json::json!({
        "at_seconds": started.elapsed().as_secs_f64(), "cpu_seconds": tracker.cpu_seconds(),
        "rss_kib": current.values().map(|p| p.rss_kib).sum::<u64>(), "processes": current.len()
    }));
    current
}

fn load_fixture(children: usize, archived: usize) -> (OfflineApp, TranscriptGrowth) {
    let mut growth = None;
    let fixture = offline_fixture(|root| {
        let mut growing: Vec<_> = walkdir::WalkDir::new(root.join("codex"))
            .into_iter()
            .map(|entry| entry.expect("enumerate synthetic active sources"))
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .collect();
        for i in 2..children {
            growing.push(seed_codex_session(
                &root.join("codex"),
                &root.join("scope"),
                &format!("active-native-{i:04}"),
                &format!("MPACTIVE{i:04}"),
            ));
        }
        let old = chrono::Utc::now() - chrono::Duration::days(90);
        for i in 0..archived {
            let path = seed_codex_session_at(
                &root.join("codex"),
                &root.join("scope"),
                &format!("archive-native-{i:05}"),
                &format!("MPHISTORY{i:05}"),
                old,
            );
            if i < 4 {
                growing.push(path);
            }
        }
        growth = Some(TranscriptGrowth::new(growing));
        std::fs::write(root.join("continuous-output-fixture"), b"offline\n").unwrap();
        let script = root.join("bin/codex");
        let original = std::fs::read_to_string(&script).unwrap();
        let replacement = format!(
            "exec \"$MINDPLAYER_SMOKE_TEST_BINARY\" --exact {OUTPUT_CHILD} --ignored --nocapture"
        );
        assert_eq!(
            original.matches("exec cat").count(),
            1,
            "replace only the native echo consumer"
        );
        write_script(&script, &original.replace("exec cat", &replacement));
        seed_private_codex(&root.join("home"), &script);
    });
    (
        fixture,
        growth.expect("large active and archived transcript fixtures"),
    )
}

fn launch_load_children(mp: &mut Mp, children: usize) {
    mp.send(b"v");
    mp.expect("space marks", STEP_TIMEOUT, "multi-select live fixtures");
    for i in 0..children {
        mp.send(b" ");
        mp.expect(
            &format!("{} marked", i + 1),
            STEP_TIMEOUT,
            "mark exact active row",
        );
        if i + 1 < children {
            mp.send(b"\x1b[B");
        }
    }
    mp.send(b"\r");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| mp.native_launches().len() == children
            && heartbeat_values(mp).len() == children),
        "all {children} native children must run before measurement\n{}",
        mp.screen()
    );
    assert_eq!(
        mp.native_launches().len(),
        children,
        "archive rows must never launch eagerly"
    );
}

fn load_fixture_restore_smoke(children: usize) {
    let (mut fixture, mut growth) = load_fixture(children, 20);
    let mp = &mut fixture.0;
    let immutable = immutable_source_fingerprint(mp, &growth.paths());
    mp.start_into_main_list_with_count(children, INDEX_TIMEOUT);
    launch_load_children(mp, children);
    let started = Instant::now();
    let mut samples = vec![measured_input(
        mp,
        b"BEFORE\r",
        "ACK:BEFORE",
        started,
        "split",
    )];
    growth.append();
    mp.send(b"\x18");
    mp.expect(
        "· active",
        STEP_TIMEOUT,
        "browse Archive with output children alive",
    );
    assert!(measured_archive_search(
        mp,
        "MPHISTORY00000",
        started,
        &mut samples
    ));
    restore_probe(
        mp,
        ("codex-alfa-0001", TITLE_A),
        ("archive-native-00000", "MPHISTORY00000"),
        started,
        &mut samples,
    );
    samples.push(measured_input(
        mp,
        b"AFTERFIRST\r",
        "ACK:AFTERFIRST",
        started,
        "split",
    ));
    growth.append();
    mp.send(b"\x18");
    mp.expect(
        "· active",
        STEP_TIMEOUT,
        "browse Archive again after the first restore",
    );
    assert!(measured_archive_search(
        mp,
        "MPHISTORY00001",
        started,
        &mut samples
    ));
    restore_probe(
        mp,
        ("archive-native-00000", "MPHISTORY00000"),
        ("archive-native-00001", "MPHISTORY00001"),
        started,
        &mut samples,
    );
    samples.push(measured_input(
        mp,
        b"AFTERSECOND\r",
        "ACK:AFTERSECOND",
        started,
        "split",
    ));
    assert_eq!(
        input_tokens(mp),
        ["AFTERFIRST", "AFTERSECOND", "BEFORE"],
        "search keys must never reach a native child"
    );
    let tree = resources(mp.child.process_id().unwrap());
    assert_eq!(
        mp.native_launches()
            .iter()
            .filter(|pid| tree.contains_key(pid))
            .count(),
        children
    );
    fixture.1 = tree.into_keys().collect();
    mp.send(b"\x18");
    mp.quit_from_list();
    assert_native_cleanup(mp, &fixture.1);
    growth.verify();
    assert_eq!(immutable_source_fingerprint(mp, &growth.paths()), immutable);
    assert_no_provider_requests(mp);
    assert!(samples
        .iter()
        .any(|sample| sample.phase == "archive_search"));
    // This is a short harness-mechanics smoke, deliberately not an MP09 pass.
    println!("LOAD_FIXTURE_SMOKE={children} children; two Archive restores; {} latency samples; exact input and source bytes; cleanup verified", samples.len());
}

#[test]
fn load_fixture_preserves_typing_during_restore_with_seven_children() {
    load_fixture_restore_smoke(7);
}

#[test]
fn load_fixture_preserves_typing_during_restore_with_ten_children() {
    load_fixture_restore_smoke(10);
}

fn latency_evaluation(children: usize, archived: usize) {
    let prepare_started = Instant::now();
    let (mut fixture, mut growth) = load_fixture(children, archived);
    let mp = &mut fixture.0;
    let originals = source_fingerprint(mp);
    assert_eq!(
        originals["files"],
        children + archived,
        "every synthetic transcript is hashed"
    );
    mp.start_into_main_list_with_count(children, INDEX_TIMEOUT);
    // A search for the last old source proves the full historical population
    // is browsable before the warm measurement (not just a seeded first page).
    filter_archive(mp, &format!("MPHISTORY{:05}", archived - 1));
    mp.send(b"\x1b");
    mp.expect_absent("· archived · /", STEP_TIMEOUT, "leave archive search");
    mp.send(b"a");
    mp.expect("· active", STEP_TIMEOUT, "return to Active");
    mp.quit_from_list();
    let cold_ms = prepare_started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        source_fingerprint(mp),
        originals,
        "cold indexing preserves all source bytes"
    );
    let warm_started = Instant::now();
    mp.restart_offline_fixture();
    mp.start_into_main_list_with_count(children, INDEX_TIMEOUT);
    let warm_ms = warm_started.elapsed().as_secs_f64() * 1000.0;
    launch_load_children(mp, children);
    let root_pid = mp.child.process_id().unwrap();
    let before = resources(root_pid);
    for pid in mp.native_launches() {
        assert!(
            before.contains_key(&pid),
            "native child {pid} belongs to the app tree"
        );
    }
    let mut tracker = ResourceTracker::new(&before);
    fixture.1 = tracker.latest.keys().copied().collect();
    let immutable_sources = immutable_source_fingerprint(mp, &growth.paths());
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut sent = Vec::new();
    let mut resource_timeline = Vec::new();
    let mut refreshes = Vec::new();
    let mut new_sources = Vec::new();
    let mut pending_probe = None;
    let mut replace_id = "codex-alfa-0001".to_string();
    let mut replace_title = TITLE_A.to_string();
    let mut live_ids: BTreeSet<_> = heartbeat_values(mp).into_keys().collect();
    let mut previous_frames = heartbeat_values(mp);
    let mut output_checks = 0;
    let mut next_sample = Instant::now();
    let mut next_growth = started + Duration::from_secs(1);
    let mut next_resource = started;
    let mut next_probe = started + Duration::from_secs(5);
    let mut next_search = started + CATALOG_CYCLE;
    let mut hidden_before = BTreeMap::new();
    let mut last_restore: Option<Instant> = None;
    while started.elapsed() < MEASURED_DURATION
        || refreshes.len() < 2
        || last_restore.is_some_and(|at| at.elapsed() < Duration::from_secs(2))
    {
        assert!(
            started.elapsed() < Duration::from_secs(240),
            "two periodic catalog adoptions did not finish within 240s; observed={refreshes:?}"
        );
        if Instant::now() >= next_growth {
            growth.append();
            next_growth = Instant::now() + Duration::from_secs(1);
        }
        if Instant::now() >= next_resource {
            observe_resources(root_pid, &mut tracker, started, &mut resource_timeline);
            fixture.1 = tracker.latest.keys().copied().collect();
            let frames = heartbeat_values(mp);
            if output_checks > 0 {
                for id in &live_ids {
                    if let Some(previous) = previous_frames.get(id) {
                        assert!(
                            frames.get(id).is_some_and(|frame| frame > previous),
                            "active output stalled for {id}: {previous} -> {:?}",
                            frames.get(id)
                        );
                    }
                }
            }
            previous_frames = frames;
            output_checks += 1;
            // Long runs need the current rendered grid, not an unbounded copy
            // of every load frame in the test runner heap.
            mp.raw_output.lock().unwrap().clear();
            next_resource = Instant::now() + Duration::from_secs(1);
        }
        if pending_probe.is_none() && refreshes.len() < 2 && Instant::now() >= next_probe {
            let cycle = refreshes.len() + 1;
            let id = format!("refresh-native-{cycle:04}");
            let title = format!("MPCATALOGCYCLE{cycle}");
            let path = seed_codex_session_at(
                &mp._tmp.join("codex"),
                &mp._tmp.join("scope"),
                &id,
                &title,
                chrono::Utc::now() - chrono::Duration::days(90),
            );
            new_sources.push((path.clone(), std::fs::read(&path).unwrap()));
            pending_probe = Some((id, title, started.elapsed().as_secs_f64()));
        }
        if Instant::now() < next_sample {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let zoom = started.elapsed() >= MEASURED_DURATION / 2;
        if zoom && hidden_before.is_empty() {
            hidden_before = heartbeat_values(mp);
        }
        if zoom != mp.screen().contains("ZOOMED") {
            mp.send(b"\x1a");
            if zoom {
                mp.expect(
                    "ZOOMED",
                    STEP_TIMEOUT,
                    "measure hidden children under zoom too",
                );
            } else {
                mp.expect_absent(
                    "ZOOMED",
                    STEP_TIMEOUT,
                    "measure all visible children in split",
                );
            }
        }
        let i = sent.len();
        let token = format!("E{children:02}{archived:05}{i:04}");
        let ack = format!("ACK:{token}");
        assert!(
            !mp.screen().contains(&ack),
            "each sample uses a previously unseen token"
        );
        samples.push(measured_input(
            mp,
            format!("{token}\r").as_bytes(),
            &ack,
            started,
            if zoom { "zoom" } else { "split" },
        ));
        sent.push(token);
        mp.send(b"\t");
        next_sample += SAMPLE_INTERVAL;
        if let Some((id, title, created_seconds)) = pending_probe
            .as_ref()
            .filter(|_| Instant::now() >= next_search)
        {
            mp.send(b"\x18");
            mp.expect(
                "· active",
                STEP_TIMEOUT,
                "Archive browsing overlaps live output and typing",
            );
            if measured_archive_search(mp, title, started, &mut samples) {
                let observed = Instant::now();
                let catalog_modified = std::fs::metadata(mp._tmp.join("session-catalog.json"))
                    .expect("catalog sidecar exists after adoption")
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                observe_resources(root_pid, &mut tracker, started, &mut resource_timeline);
                let restore_ms = restore_probe(
                    mp,
                    (&replace_id, &replace_title),
                    (id, title),
                    started,
                    &mut samples,
                );
                let current =
                    observe_resources(root_pid, &mut tracker, started, &mut resource_timeline);
                fixture.1 = tracker.latest.keys().copied().collect();
                let live_children = mp
                    .native_launches()
                    .iter()
                    .filter(|pid| current.contains_key(pid))
                    .count();
                assert_eq!(
                    live_children, children,
                    "restore returns to the exact workload size"
                );
                live_ids.remove(&format!("heartbeat-{replace_id}"));
                live_ids.insert(format!("heartbeat-{id}"));
                previous_frames = heartbeat_values(mp);
                last_restore = Some(Instant::now());
                next_resource = Instant::now() + Duration::from_secs(1);
                refreshes.push(serde_json::json!({
                    "cycle": refreshes.len() + 1, "source_id": id, "created_seconds": created_seconds,
                    "observed_seconds": observed.duration_since(started).as_secs_f64(),
                    "row_rendered": true, "restored": true, "restore_ms": restore_ms,
                    "native_children_after": live_children,
                    "catalog_modified_unix_ms": catalog_modified,
                    "restored_seconds": started.elapsed().as_secs_f64()
                }));
                println!("ACTIVE_ARCHIVE_REFRESH={}", refreshes.last().unwrap());
                replace_id = id.clone();
                replace_title = title.clone();
                next_probe = observed + Duration::from_secs(5);
                next_search = observed + CATALOG_CYCLE;
                pending_probe = None;
            } else {
                leave_archive_search(mp);
                mp.send(b"\x18");
                mp.expect(
                    PANE_READY,
                    STEP_TIMEOUT,
                    "resume typing while waiting for catalog adoption",
                );
                next_search = Instant::now() + Duration::from_secs(1);
            }
            // Do not burst overdue samples after an intentional UI operation.
            next_sample = Instant::now();
        }
    }
    observe_resources(root_pid, &mut tracker, started, &mut resource_timeline);
    fixture.1 = tracker.latest.keys().copied().collect();
    let measured_seconds = started.elapsed().as_secs_f64();
    let hidden_after = heartbeat_values(mp);
    for (id, frame) in hidden_before
        .iter()
        .filter(|(id, _)| live_ids.contains(*id))
    {
        assert!(
            hidden_after.get(id).is_some_and(|after| after > frame),
            "hidden child {id} must keep outputting: {frame} -> {:?}",
            hidden_after.get(id)
        );
    }
    sent.sort();
    assert_eq!(
        input_tokens(mp),
        sent,
        "all unique inputs consumed exactly once by native children"
    );
    let launches = mp.native_launches();
    assert_eq!(
        launches.len(),
        children + 2,
        "only two requested restores may add native launches"
    );
    assert_eq!(
        launches.iter().collect::<BTreeSet<_>>().len(),
        launches.len(),
        "no duplicate native launches"
    );
    mp.send(b"\x18");
    mp.quit_from_list();
    assert_native_cleanup(mp, &fixture.1);
    let growth_evidence = growth.verify();
    let mut mutable_sources = growth.paths();
    for (path, expected) in new_sources {
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected,
            "new catalog probe remains byte-exact"
        );
        mutable_sources.push(path);
    }
    assert_eq!(
        immutable_source_fingerprint(mp, &mutable_sources),
        immutable_sources,
        "all other provider sources remain byte-exact"
    );
    assert_no_provider_requests(mp);
    let mut latencies: Vec<_> = samples.iter().map(|sample| sample.ms).collect();
    latencies.sort_by(f64::total_cmp);
    let percentile = |q: usize| latencies[(latencies.len() * q).div_ceil(100) - 1];
    let report = serde_json::json!({
        "binary": mp.command.get_argv()[0].to_string_lossy(), "children": children, "archive_sources": archived,
        "samples": samples.len(), "split_samples": samples.iter().filter(|s| s.phase == "split").count(),
        "zoom_samples": samples.iter().filter(|s| s.phase == "zoom").count(),
        "p50_ms": percentile(50), "p95_ms": percentile(95), "p99_ms": percentile(99), "max_ms": latencies.last().unwrap(),
        "lost_inputs": 0, "duplicate_inputs": 0, "deadlocks": 0, "cpu_seconds": tracker.cpu_seconds(),
        "cpu_percent_of_one_core": tracker.cpu_seconds() / measured_seconds * 100.0,
        "peak_sampled_tree_rss_kib": tracker.peak_rss_kib,
        "peak_sampled_tree_processes": tracker.peak_processes,
        "measured_seconds": measured_seconds,
        "cold_setup_and_index_ms": cold_ms, "warm_start_ms": warm_ms,
        "initial_source_fingerprint": originals, "immutable_source_fingerprint": immutable_sources,
        "source_bytes_verified": true, "native_cleanup": true, "provider_requests": 0,
        "growth": growth_evidence, "refreshes": refreshes, "latency_timeline": samples,
        "resource_timeline": resource_timeline, "output_progress_checks": output_checks,
        "refresh_evidence": "new source row observed on screen after periodic adoption; no manual rescan",
        "replacement_policy": "close one explicitly selected synthetic child before each restore; return to exact workload size",
        "scope": "actual MindPlayer outer PTY; synthetic offline native children; no provider requests"
    });
    println!("ACTIVE_ARCHIVE_EVALUATION={report}");
    println!(
        "ACTIVE_ARCHIVE_SUMMARY={}",
        serde_json::json!({
            "children": children, "archive_sources": archived, "seconds": measured_seconds,
            "samples": report["samples"], "p50_ms": report["p50_ms"], "p95_ms": report["p95_ms"],
            "p99_ms": report["p99_ms"], "max_ms": report["max_ms"],
            "cpu_seconds": tracker.cpu_seconds(), "peak_rss_kib": tracker.peak_rss_kib,
            "refreshes": report["refreshes"].as_array().unwrap().len(), "lost": 0, "duplicates": 0, "deadlocks": 0
        })
    );
    let validation = check_evaluation_report(&report);
    assert!(
        validation.status.success(),
        "performance evidence rejected: {}",
        String::from_utf8_lossy(&validation.stderr)
    );
}

#[test]
#[ignore = "release-profile real outer-PTY latency evaluator; runs offline"]
fn active_archive_latency_7_children_5k_archive() {
    latency_evaluation(7, 5_000);
}

#[test]
#[ignore = "release-profile real outer-PTY latency evaluator; runs offline"]
fn active_archive_latency_7_children_50k_archive() {
    latency_evaluation(7, 50_000);
}

#[test]
#[ignore = "release-profile real outer-PTY latency evaluator; runs offline"]
fn active_archive_latency_10_children_5k_archive() {
    latency_evaluation(10, 5_000);
}

#[test]
#[ignore = "release-profile real outer-PTY latency evaluator; runs offline"]
fn active_archive_latency_10_children_50k_archive() {
    latency_evaluation(10, 50_000);
}
