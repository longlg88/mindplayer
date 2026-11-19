use super::*;

/// Exercises native credential selection and stream provenance, not a fake
/// credentialId: only the transport is local, with synthetic isolated logins.
#[test]
#[ignore = "requires MINDPLAYER_TEST_OMP_BINARY; no external inference or real credentials"]
fn actual_omp_accounts_follow_native_selection_and_switches() {
    let executable = PathBuf::from(
        std::env::var_os("MINDPLAYER_TEST_OMP_BINARY").expect("set installed OMP path"),
    );
    assert!(executable.is_absolute() && executable.is_file());
    let mut fixture = active_archive::OfflineApp(
        Mp::launch_with_options(40, 140, false, false, false, false),
        Vec::new(),
    );
    let mp = &mut fixture.0;
    let root = mp._tmp.clone();
    durable_state(&root);
    let auth = root.join("synthetic-auth.json");
    let credentials = serde_json::json!({"version":1,"credentials":[
        {"provider":"mindplayer-oauth-fixture","credential":{
            "type":"oauth","access":"fixture-oauth-a","refresh":"fixture-refresh-a",
            "expires":4102444800000_u64,"email":"first@example.test","accountId":"fixture-a","orgName":"Fixture One"}},
        {"provider":"mindplayer-oauth-fixture","credential":{
            "type":"oauth","access":"fixture-oauth-b","refresh":"fixture-refresh-b",
            "expires":4102444800000_u64,"email":"second@example.test","accountId":"fixture-b","orgName":"Fixture Two"}},
        {"provider":"mindplayer-key-fixture","credential":{
            "type":"api_key","key":"fixture-api-key","source":"login"}}
    ]});
    std::fs::write(&auth, serde_json::to_vec(&credentials).unwrap()).unwrap();
    let imported = std::process::Command::new(&executable)
        .args(["auth", "import"])
        .arg(&auth)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("PI_CODING_AGENT_DIR", root.join("omp"))
        .current_dir(root.join("scope"))
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    std::fs::write(root.join("omp/config.yml"),
        "startup:\n  quiet: true\n  setupWizard: false\n  showSplash: false\nspelling:\n  autocomplete: 'off'\n").unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp_identity_provider.mjs");
    let receipt = root.join("identity-receipts.jsonl");
    let pin = root.join("identity-pin");
    let report_path = root.join("identity-report-path");
    write_omp_script(&root.join("bin/omp"), &format!(
        "printf '%s' \"$MINDPLAYER_OMP_ACCOUNT_FILE\" > '{report}'\nexec env -i HOME=\"$HOME\" PATH=\"$PATH\" TERM=\"$TERM\" PI_CODING_AGENT_DIR=\"$PI_CODING_AGENT_DIR\" MINDPLAYER_OMP_PICKER_READY=\"$MINDPLAYER_OMP_PICKER_READY\" MINDPLAYER_OMP_PICKER_SELECTED=\"$MINDPLAYER_OMP_PICKER_SELECTED\" MINDPLAYER_OMP_ACCOUNT_FILE=\"$MINDPLAYER_OMP_ACCOUNT_FILE\" MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID=\"$MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID\" MINDPLAYER_FIXTURE_RECEIPT='{receipt}' MINDPLAYER_FIXTURE_PIN='{pin}' '{executable}' --no-extensions --no-skills --no-rules --no-context-files --no-title --no-lsp --no-pty --no-tools --no-judge --no-spotlight -e '{fixture}' \"$@\"\n",
        report=report_path.display(), receipt=receipt.display(), pin=pin.display(), executable=executable.display(), fixture=fixture.display(),
    ));
    choose_omp(mp);
    mp.send(b"\r");
    mp.expect("Label / subject", STEP_TIMEOUT, "OMP label");
    mp.send(b"IDENTITY_PROBE\r");
    assert!(mp.wait_until(Duration::from_secs(45), |screen| {
        screen.contains("Choose a model and provider") || screen.contains("failed to start omp")
    }));
    assert!(
        mp.screen().contains("Choose a model and provider"),
        "{}",
        mp.screen()
    );
    // The two authenticated routes are sorted key first, OAuth second.
    mp.send(b"\x1b[B");
    mp.send(b"\r");
    mp.expect(
        "Selected mindplayer-oauth-fixture/native-identity-chat",
        STEP_TIMEOUT,
        "selected OAuth route",
    );
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| std::fs::read_to_string(&pin)
            .is_ok_and(|s| s == "second@example.test"))
    );
    mp.expect(
        "omp · 계정 미확인",
        STEP_TIMEOUT,
        "pin alone is not request evidence",
    );
    send_line(mp, "identify first");
    mp.expect(
        "omp · second@example.test",
        Duration::from_secs(30),
        "actual selected OAuth account, not first roster entry",
    );
    let receipts = || -> Vec<serde_json::Value> {
        std::fs::read_to_string(&receipt)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    assert_eq!(receipts().last().unwrap()["served"], "b");
    println!("NATIVE OMP SECOND ACCOUNT:\n{}", mp.screen());
    mp.send(b"\x18");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| screen
            .lines()
            .any(|row| row.contains("IDENTITY_PROBE") && row.contains("second@"))),
        "list must identify the same account"
    );
    mp.send(b"\x18");
    send_line(mp, "/fixture-account");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| std::fs::read_to_string(&pin)
            .is_ok_and(|s| s == "first@example.test"))
    );
    mp.expect(
        "omp · 계정 미확인",
        STEP_TIMEOUT,
        "explicit pin switch invalidates old identity",
    );
    send_line(mp, "identify switched");
    mp.expect(
        "omp · first@example.test",
        Duration::from_secs(30),
        "new actual account after native switch",
    );
    assert_eq!(receipts().last().unwrap()["served"], "a");
    println!("NATIVE OMP SWITCHED ACCOUNT:\n{}", mp.screen());
    send_line(mp, "/mindplayer-model");
    mp.expect(
        "Choose a model and provider",
        STEP_TIMEOUT,
        "switch to API-key route",
    );
    mp.send(b"\r");
    mp.expect(
        "Selected mindplayer-key-fixture/native-identity-key",
        STEP_TIMEOUT,
        "selected API-key route",
    );
    mp.expect(
        "omp · 계정 미확인",
        STEP_TIMEOUT,
        "model switch invalidates identity",
    );
    send_line(mp, "identify key");
    let path = PathBuf::from(std::fs::read_to_string(&report_path).unwrap());
    assert!(
        mp.wait_until(Duration::from_secs(30), |_| {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|value| {
                    value["state"] == "observed" && value["provider"] == "mindplayer-key-fixture"
                })
        }),
        "native API-key credential must be observed"
    );
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        report["email"].is_null(),
        "an API key must not inherit OAuth email"
    );
    let id = report["credentialId"].as_u64().unwrap();
    let label = if report["kind"] == "api_key" {
        format!("omp · API key #{id}")
    } else {
        format!("omp · credential #{id}")
    };
    mp.expect(
        &label,
        STEP_TIMEOUT,
        "native API-key identity without secret material",
    );
    assert_eq!(receipts().last().unwrap()["served"], "key");
    let public = std::fs::read_to_string(&path).unwrap();
    for secret in [
        "fixture-oauth-a",
        "fixture-oauth-b",
        "fixture-api-key",
        "fixture-refresh",
    ] {
        assert!(
            !public.contains(secret),
            "credential material leaked to sideband"
        );
    }
    println!("NATIVE OMP API KEY:\n{}", mp.screen());
    let last_account = format!(
        "{} · mindplayer-key-fixture",
        label.trim_start_matches("omp · ")
    );
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| stored_accounts(&root)
            == [last_account.as_str()]),
        "the transcript keeps only its last response account: {:?}",
        stored_accounts(&root)
    );
    let in_list = if report["kind"] == "api_key" {
        "API key #"
    } else {
        "credentia"
    };
    assert_account_survives_a_restart(mp, in_list);
}

/// Replaces the harness's empty state directory so the app can save a state file.
fn durable_state(root: &Path) {
    std::fs::remove_dir(root.join("state")).expect("replace the empty state fixture");
}

/// Every OMP account label the saved state holds.
fn stored_accounts(root: &Path) -> Vec<String> {
    std::fs::read(root.join("state"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|state| state["omp_account_sources"].as_object().cloned())
        .into_iter()
        .flat_map(|sources| sources.into_iter().map(|(_, records)| records))
        .flat_map(|records| records.as_array().cloned().unwrap_or_default())
        .filter_map(|record| record["label"].as_str().map(str::to_owned))
        .collect()
}

/// Quits from the focused OMP pane, restarts, and expects the unopened OMP row to show its account.
fn assert_account_survives_a_restart(mp: &mut Mp, account: &str) {
    mp.send(b"\x18");
    mp.quit_from_list();
    mp.restart_offline_fixture();
    mp.start_into_main_list_with_count(3, STARTUP_TIMEOUT);
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| screen
            .lines()
            .any(|row| row.contains(" omp ") && row.contains(account))),
        "a restart keeps the account of a row that was never reopened: {}",
        mp.screen()
    );
    assert!(
        mp.screen().contains("0 running"),
        "no session is open after the restart: {}",
        mp.screen()
    );
    println!("OMP ROW AFTER A RESTART:\n{}", mp.screen());
    mp.quit_from_list();
}

/// A scripted OMP stands in for the native observer: it writes one transcript and one response report.
#[test]
fn an_omp_row_keeps_its_last_response_account_after_a_restart() {
    let mut fixture = active_archive::OfflineApp(
        Mp::launch_with_options(40, 140, false, false, false, false),
        Vec::new(),
    );
    let mp = &mut fixture.0;
    let root = mp._tmp.clone();
    durable_state(&root);
    write_omp_script(
        &root.join("bin/omp"),
        concat!(
            "dir=\"$PI_CODING_AGENT_DIR/sessions/account-history\"\n",
            "mkdir -p \"$dir\"\n",
            "file=\"$dir/native-account.jsonl\"\n",
            "now=$(date -u +%Y-%m-%dT%H:%M:%SZ)\n",
            "printf '{\"type\":\"session\",\"id\":\"omp-account-history\",\"cwd\":\"%s\",\"timestamp\":\"%s\"}\\n' \"$PWD\" \"$now\" > \"$file\"\n",
            "printf '{\"type\":\"message\",\"timestamp\":\"%s\",\"message\":{\"role\":\"user\",\"content\":\"ACCOUNT_HISTORY_PROBE\"}}\\n' \"$now\" >> \"$file\"\n",
            "printf '{\"schema\":\"mindplayer.omp-account.v1\",\"launchId\":\"%s\",\"sessionId\":\"omp-account-history\",\"sourceFile\":\"%s\",\"state\":\"observed\",\"provider\":\"fixture-provider\",\"model\":\"fixture-model\",\"credentialId\":4,\"kind\":\"oauth\",\"email\":\"kept@example.test\"}' \"$MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID\" \"$file\" > \"$MINDPLAYER_OMP_ACCOUNT_FILE.staged\"\n",
            "mv \"$MINDPLAYER_OMP_ACCOUNT_FILE.staged\" \"$MINDPLAYER_OMP_ACCOUNT_FILE\"\n",
            "printf 'ready\\n' > \"$MINDPLAYER_OMP_PICKER_READY\"\n",
            "printf 'OMP-ACCOUNT-FIXTURE-READY\\n'\n",
            "exec cat\n",
        ),
    );
    choose_omp(mp);
    mp.send(b"\r");
    mp.expect("Label / subject", STEP_TIMEOUT, "OMP label");
    mp.send(b"ACCOUNT_HISTORY_PROBE\r");
    mp.expect(
        "OMP-ACCOUNT-FIXTURE-READY",
        STARTUP_TIMEOUT,
        "scripted OMP launched",
    );
    mp.expect(
        "omp · kept@example.test",
        STEP_TIMEOUT,
        "the running pane shows its response account",
    );
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| stored_accounts(&root)
            == ["kept@example.test · fixture-provider"]),
        "the response account is saved while the pane runs: {:?}",
        stored_accounts(&root)
    );
    assert_account_survives_a_restart(mp, "kept@");
}

fn send_line(mp: &mut Mp, line: &str) {
    mp.send(line.as_bytes());
    mp.expect(line, STEP_TIMEOUT, "native OMP editor input");
    mp.send(b"\r");
}
