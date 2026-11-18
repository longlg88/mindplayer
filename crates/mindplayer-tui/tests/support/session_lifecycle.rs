//! Native creation, catalog adoption, and exact-owner removal through the real TUI.

use super::*;
use active_archive::OfflineApp;

const TARGET: &str = "MPLIFECYCLETARGET";
const REAL_ID: &str = "mpnewcod-real-0001";
const READY: &str = "LIFECYCLE-NATIVE-READY";

struct LifecycleApp(OfflineApp);

impl Drop for LifecycleApp {
    fn drop(&mut self) {
        // Release fixture probes even when an assertion unwinds. OfflineApp then
        // quits MindPlayer and reaps only receipt-owned native children.
        let _ = std::fs::remove_file(self.0 .0._tmp.join("home/.codex/hold-account-response"));
    }
}

fn launch() -> LifecycleApp {
    LifecycleApp(OfflineApp(
        Mp::launch_with_fixture_hooks(
            (60, 240),
            false,
            false,
            true,
            false,
            CodexFixture::AuthRecovery,
            (
                |root| {
                    std::fs::remove_dir(root.join("state")).unwrap();
                    std::fs::rename(root.join("bin/codex"), root.join("bin/codex-probe")).unwrap();
                    std::fs::write(
                        root.join("lifecycle-native.mjs"),
                        r#"import { appendFileSync, existsSync, mkdirSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
const root = process.argv[2];
const args = process.argv.slice(3);
const pane = process.env.MINDPLAYER_PANE_ID;
if (!pane || !process.env.CODEX_HOME) throw new Error('explicit native ownership required');
const receipt = { pid: process.pid, ppid: process.ppid, pane, args,
  home: process.env.CODEX_HOME, cwd: process.cwd() };
writeFileSync(join(root, `native-${process.pid}.json`), JSON.stringify(receipt));
appendFileSync(join(root, 'native-launches.log'), `${process.pid}\n`);
writeFileSync(join(root, 'codex-markers', `${pane}.txt`), `CODEX_HOME=${receipt.home}\nPID=${process.pid}\n`);
process.stdout.write('LIFECYCLE-NATIVE-READY\r\n');
let published = false;
const started = Date.now();
const timer = setInterval(() => {
  if (!published && !args.includes('resume') && existsSync(join(root, 'publish-transcript'))) {
    const file = join(root, 'codex', 'lifecycle', 'mpnewcod-real-0001.jsonl');
    mkdirSync(dirname(file), { recursive: true });
    const timestamp = new Date().toISOString();
    const rows = [
      { type: 'session_meta', timestamp, payload: { id: 'mpnewcod-real-0001', cwd: process.cwd(), timestamp } },
      { type: 'response_item', timestamp, payload: { type: 'message', role: 'user',
        content: [{ type: 'input_text', text: 'MPLIFECYCLETARGET' }] } },
    ];
    writeFileSync(`${file}.pending`, rows.map(row => JSON.stringify(row)).join('\n') + '\n');
    renameSync(`${file}.pending`, file);
    writeFileSync(join(root, 'transcript-published'), String(process.pid));
    published = true;
  }
  if (existsSync(join(root, `exit-${process.pid}`))) {
    writeFileSync(join(root, `exited-${process.pid}`), 'natural native exit\n');
    clearInterval(timer);
    process.exit(0);
  }
  if (Date.now() - started > 90000) process.exit(93);
}, 10);
"#,
                    )
                    .unwrap();
                    write_script(
                        &root.join("bin/codex"),
                        &format!(
                            "#!/bin/sh\ncase \" $* \" in *app-server*|*' login '*|*' logout '*) exec '{}' \"$@\" ;; esac\nexec node '{}' '{}' \"$@\"\n",
                            root.join("bin/codex-probe").display(),
                            root.join("lifecycle-native.mjs").display(),
                            root.display(),
                        ),
                    );
                },
                |_, _| {},
            ),
        ),
        Vec::new(),
    ))
}

fn create(mp: &mut Mp) {
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "open native account picker");
    mp.expect(
        "▶ codex · default",
        STEP_TIMEOUT,
        "explicit inherited fixture account",
    );
    mp.send(b"\r");
    mp.expect("Label / subject", STEP_TIMEOUT, "label the transient owner");
    mp.send(format!("{TARGET}\r").as_bytes());
}

fn receipt(mp: &Mp, pid: u32) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(mp._tmp.join(format!("native-{pid}.json"))).unwrap())
        .unwrap()
}

fn launched(mp: &Mp) -> u32 {
    mp.expect(READY, STEP_TIMEOUT, "native PATH child actually started");
    let launches = mp.native_launches();
    assert_eq!(
        launches.len(),
        1,
        "creation launches exactly one native child"
    );
    let pid = launches[0];
    let launch = receipt(mp, pid);
    assert_eq!(
        launch["home"],
        mp._tmp.join("home/.codex").to_string_lossy().as_ref()
    );
    assert_eq!(
        launch["cwd"],
        mp._tmp.join("scope").to_string_lossy().as_ref()
    );
    assert_eq!(launch["pane"], "new:codex:1");
    assert_eq!(launch["ppid"], mp.child.process_id().unwrap());
    assert!(!launch["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "resume"));
    assert!(
        fixture_process_exists(pid),
        "receipt-owned native process is live"
    );
    mp.assert_native_without_router();
    pid
}

fn target_rows(screen: &str) -> Vec<&str> {
    screen
        .lines()
        .filter(|line| line.contains(TARGET) && line.contains("codex"))
        .collect()
}

fn assert_target(mp: &Mp, id_fragment: &str) {
    let screen = mp.screen();
    let rows = target_rows(&screen);
    assert_eq!(
        rows.len(),
        1,
        "exactly one target row after adoption:\n{screen}"
    );
    assert!(
        rows[0].contains(id_fragment),
        "expected target identity {id_fragment}:\n{screen}"
    );
}

fn select(mp: &mut Mp, title: &str) {
    for _ in 0..20 {
        let screen = mp.screen();
        let lines: Vec<_> = screen.lines().collect();
        let target = lines
            .iter()
            .position(|line| line.contains(title) && line.contains("codex"))
            .unwrap_or_else(|| panic!("missing selectable row {title}:\n{screen}"));
        let selected = lines
            .iter()
            .position(|line| line.contains('▶'))
            .unwrap_or_else(|| panic!("missing selected row:\n{screen}"));
        if target == selected {
            return;
        }
        let previous = lines[selected].to_string();
        mp.send(if target < selected {
            b"\x1b[A"
        } else {
            b"\x1b[B"
        });
        assert!(
            mp.wait_until(STEP_TIMEOUT, |screen| screen
                .lines()
                .find(|line| line.contains('▶'))
                .is_some_and(|line| line != previous)),
            "selection must acknowledge each navigation key:\n{}",
            mp.screen()
        );
    }
    panic!("could not select {title}:\n{}", mp.screen());
}

fn refresh(mp: &mut Mp, cycle: usize) {
    // A newly discovered neighbor is the adoption barrier. Merely seeing the
    // old target row or a scope status message would not prove a scan finished.
    let title = format!("MPLIFEREFRESH{cycle}");
    let sentinel_cwd = mp._tmp.join("sentinel-scope");
    std::fs::create_dir_all(&sentinel_cwd).unwrap();
    seed_codex_session(
        &mp._tmp.join("codex"),
        &sentinel_cwd,
        &format!("lifecycle-refresh-{cycle}"),
        &title,
    );
    // Use a different cwd so the new Codex source cannot be claimed as the
    // pending conversation. Empty directory input refreshes global scope;
    // `r` measures usage and Ctrl-R only probes accounts.
    mp.send(b"d");
    mp.send(&vec![
        0x7f;
        mp._tmp.join("scope").to_string_lossy().chars().count()
    ]);
    mp.send(b"\r");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| screen
            .lines()
            .any(|line| line.contains(&title) && line.contains("codex"))),
        "catalog refresh must adopt sentinel {cycle}:\n{}",
        mp.screen()
    );
}

fn absent(mp: &Mp) {
    assert!(
        target_rows(&mp.screen()).is_empty(),
        "removed owner must not reappear:\n{}",
        mp.screen()
    );
    mp.expect(TITLE_A, STEP_TIMEOUT, "unrelated isolated source survives");
    mp.expect(TITLE_B, STEP_TIMEOUT, "unrelated inherited source survives");
}

#[test]
fn native_codex_unwritten_refresh_keeps_one_row_then_exit_and_x_remove_it() {
    let mut fixture = launch();
    let mp = &mut fixture.0 .0;
    mp.start_into_main_list();
    create(mp);
    let pid = launched(mp);
    mp.send(b"\x11");
    mp.expect("multi-select", STEP_TIMEOUT, "Ctrl-Q returns to list");
    assert!(
        fixture_process_exists(pid),
        "Ctrl-Q hides rather than stops the child"
    );
    refresh(mp, 1);
    assert_target(mp, "new:code");
    assert!(!mp
        ._tmp
        .join("codex/lifecycle/mpnewcod-real-0001.jsonl")
        .exists());
    std::fs::write(mp._tmp.join(format!("exit-{pid}")), "exit\n").unwrap();
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(pid)),
        "native fixture must exit before x"
    );
    assert!(
        mp._tmp.join(format!("exited-{pid}")).is_file(),
        "child acknowledged natural exit"
    );
    select(mp, TARGET);
    mp.send(b"x");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| target_rows(screen).is_empty()),
        "x removes ended placeholder:\n{}",
        mp.screen()
    );
    refresh(mp, 2);
    absent(mp);
    mp.quit_from_list();
    mp.restart_offline_fixture();
    mp.start_into_main_list_with_count(4, STARTUP_TIMEOUT);
    absent(mp);
    mp.quit_from_list();
}

#[test]
fn native_codex_delayed_transcript_replaces_placeholder_and_x_archives_only_target() {
    let mut fixture = launch();
    let mp = &mut fixture.0 .0;
    mp.start_into_main_list();
    create(mp);
    let pid = launched(mp);
    mp.send(b"\x11");
    mp.expect("multi-select", STEP_TIMEOUT, "hide live native child");
    refresh(mp, 1);
    assert_target(mp, "new:code");
    assert!(fixture_process_exists(pid));
    std::fs::write(mp._tmp.join("publish-transcript"), "release\n").unwrap();
    assert!(mp.wait_until(STEP_TIMEOUT, |_| mp
        ._tmp
        .join("transcript-published")
        .is_file()));
    let transcript = mp._tmp.join(format!("codex/lifecycle/{REAL_ID}.jsonl"));
    let original = std::fs::read(&transcript).unwrap();
    refresh(mp, 2);
    assert_target(mp, "mpnewcod");
    assert!(
        !mp.screen().contains("new:code"),
        "matched transient identity is gone"
    );
    select(mp, TARGET);
    mp.send(b"x");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(pid)),
        "x stops the exact native owner"
    );
    assert!(mp.wait_until(STEP_TIMEOUT, |screen| target_rows(screen).is_empty()));
    refresh(mp, 3);
    absent(mp);
    assert_eq!(
        std::fs::read(&transcript).unwrap(),
        original,
        "archive never deletes native transcript bytes"
    );
    mp.quit_from_list();
    mp.restart_offline_fixture();
    mp.start_into_main_list_with_count(5, STARTUP_TIMEOUT);
    absent(mp);
    mp.send(b"a");
    mp.expect(
        "· archived",
        STEP_TIMEOUT,
        "inspect durable archive after restart",
    );
    mp.expect(TARGET, STEP_TIMEOUT, "only target moved to archive");
    assert_target(mp, "mpnewcod");
    assert!(
        !mp.screen().contains(TITLE_B),
        "neighbor remains Active, not archived"
    );
    mp.send(b"a");
    mp.expect(
        TITLE_B,
        STEP_TIMEOUT,
        "neighbor still selectable after restart",
    );
    select(mp, TITLE_B);
    mp.send(b"\r");
    mp.expect(
        READY,
        STEP_TIMEOUT,
        "surviving neighbor still resumes natively",
    );
    let neighbor = mp.native_launches();
    assert_eq!(neighbor.len(), 1);
    assert!(receipt(mp, neighbor[0])["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "codex-brvo-0002"));
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "return to list for natural quit",
    );
    mp.quit_from_list();
    assert!(mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(neighbor[0])));
}

#[test]
fn native_codex_x_during_held_auth_cancels_creation_without_launching_it_later() {
    let mut fixture = launch();
    let mp = &mut fixture.0 .0;
    mp.start_into_main_list();
    let home = mp._tmp.join("home/.codex");
    std::fs::write(home.join("hold-account-response"), "hold\n").unwrap();
    create(mp);
    assert!(mp.wait_until(STEP_TIMEOUT, |_| mp
        .codex_auth_events(&home, "account-request")
        .iter()
        .any(|row| row["refresh"] == true)));
    let request = mp
        .codex_auth_events(&home, "account-request")
        .into_iter()
        .find(|row| row["refresh"] == true)
        .unwrap();
    let probe_pid = u32::try_from(request["pid"].as_u64().unwrap()).unwrap();
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "list remains responsive during held auth",
    );
    select(mp, TARGET);
    mp.send(b"x");
    assert!(mp.wait_until(STEP_TIMEOUT, |screen| target_rows(screen).is_empty()));
    mp.assert_no_session_child();
    std::fs::remove_file(home.join("hold-account-response")).unwrap();
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(probe_pid)),
        "released auth worker is reaped"
    );
    refresh(mp, 1);
    absent(mp);
    select(mp, TITLE_B);
    mp.send(b"\r");
    mp.expect(
        READY,
        STEP_TIMEOUT,
        "subsequent explicit neighbor request completes auth",
    );
    let launches = mp.native_launches();
    assert_eq!(
        launches.len(),
        1,
        "cancelled creation must never launch after auth completion"
    );
    let launch = receipt(mp, launches[0]);
    assert_eq!(launch["pane"], "codex-brvo-0002");
    assert!(launch["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "resume"));
    mp.send(b"\x18");
    mp.expect("multi-select", STEP_TIMEOUT, "neighbor returns to list");
    refresh(mp, 2);
    absent(mp);
    mp.quit_from_list();
    assert!(mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(launches[0])));
    mp.restart_offline_fixture();
    mp.start_into_main_list_with_count(4, STARTUP_TIMEOUT);
    absent(mp);
    mp.quit_from_list();
}
