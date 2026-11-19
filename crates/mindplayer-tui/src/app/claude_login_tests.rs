//! How a finished Claude login is judged against the stored bindings.

use super::accounts_panel::AccountRow;
use super::*;
use mindplayer_core::accounts::{read_saved_accounts, Account};
use mindplayer_core::limits::{AccountIdentity, FailureKind, ProbeError};

const ORG: &str = "0123abcd-4567-89ef-0123-456789abcdef";

fn live(fingerprint: &str, plan: &str) -> Result<AccountIdentity, ProbeError> {
    Ok(AccountIdentity {
        fingerprint: fingerprint.into(),
        email: "person@example.test".into(),
        workspace: ORG.into(),
        plan: plan.into(),
    })
}

fn bound(name: &str, fingerprint: &str, plan: &str) -> Account {
    let mut account = Account::isolated(&limits_home_for_app(), Agent::Claude, name).unwrap();
    account.set_identity_fingerprint(Some(fingerprint.into()));
    account.set_claude_login_facts(ORG, plan);
    account
}

fn unbound(name: &str) -> Account {
    Account::isolated(&limits_home_for_app(), Agent::Claude, name).unwrap()
}

fn select(app: &mut App, name: &str) {
    let at = app
        .account_rows()
        .iter()
        .position(|row| matches!(row, AccountRow::Entry(i) if app.accounts[*i].name == name))
        .expect("account row");
    app.accounts_panel.as_mut().unwrap().selected = at;
}

fn app_with(accounts: &[Account], selected: &str) -> App {
    let mut app = super::tests::isolated_app();
    app.accounts.extend(accounts.iter().cloned());
    app.accounts_base = app.accounts.clone();
    app.open_accounts();
    select(&mut app, selected);
    app
}

fn login_id(name: &str) -> String {
    format!("login:claude:{name}")
}

/// Run the check that follows a finished login pane and hand it `reading`.
fn after_login(app: &mut App, name: &str, reading: Result<AccountIdentity, ProbeError>) {
    let account = app
        .accounts
        .iter()
        .find(|a| a.name == name)
        .unwrap()
        .clone();
    app.track_account_login(&account, &login_id(name));
    app.account_login_finished(&login_id(name), true);
    app.complete_account_checks_for_test(reading);
}

fn named<'a>(app: &'a App, name: &str) -> &'a Account {
    app.accounts.iter().find(|a| a.name == name).unwrap()
}

fn panel_error(app: &App) -> String {
    app.accounts_panel
        .as_ref()
        .and_then(|panel| panel.error.clone())
        .unwrap_or_default()
}

fn saved(app: &App, name: &str) -> Option<Account> {
    read_saved_accounts(&app.accounts_home)?
        .into_iter()
        .find(|a| a.name == name)
}

/// Add a Claude account the way the Accounts screen does and leave the panel closed, as the login pane does.
fn add_account(app: &mut App, name: &str) {
    app.accounts_panel.as_mut().unwrap().selected = 1;
    let at = app
        .account_rows()
        .iter()
        .position(|row| matches!(row, AccountRow::Header(Agent::Claude)))
        .unwrap();
    app.accounts_panel.as_mut().unwrap().selected = at;
    app.accounts_start_add();
    for c in name.chars() {
        app.accounts_name_push(c);
    }
    app.accounts_confirm_add();
}

fn reopen(app: &mut App, name: &str) {
    app.open_accounts();
    select(app, name);
}

#[test]
fn the_same_login_is_confirmed_with_its_email_and_plan_and_the_facts_are_stored() {
    let mut account = bound("cl-same", "fp-same", "claude_team");
    account.claude_workspace = None;
    account.claude_plan = None;
    let mut app = app_with(&[account], "cl-same");
    after_login(&mut app, "cl-same", live("fp-same", "claude_team"));
    assert_eq!(
        app.status,
        "cl-same 로그인 확인 완료 (person@example.test · team)"
    );
    assert!(!app.status.contains(ORG));
    assert_eq!(panel_error(&app), "");
    let stored = named(&app, "cl-same");
    assert_eq!(stored.claude_workspace.as_deref(), Some(ORG));
    assert_eq!(stored.claude_plan.as_deref(), Some("claude_team"));
    assert_eq!(
        saved(&app, "cl-same").unwrap().claude_workspace.as_deref(),
        Some(ORG)
    );
}

#[test]
fn a_changed_plan_is_refreshed_silently_when_the_login_is_the_same() {
    let mut app = app_with(&[bound("cl-plan", "fp-plan", "claude_pro")], "cl-plan");
    after_login(&mut app, "cl-plan", live("fp-plan", "claude_max"));
    assert_eq!(
        app.status,
        "cl-plan 로그인 확인 완료 (person@example.test · max)"
    );
    assert_eq!(
        named(&app, "cl-plan").claude_plan.as_deref(),
        Some("claude_max")
    );
    assert_eq!(
        saved(&app, "cl-plan").unwrap().claude_plan.as_deref(),
        Some("claude_max")
    );
    assert_eq!(
        named(&app, "cl-plan").identity_fingerprint(),
        Some("fp-plan")
    );
}

#[test]
fn a_profile_without_a_plan_does_not_erase_a_known_one() {
    let mut app = app_with(&[bound("cl-keep", "fp-keep", "claude_max")], "cl-keep");
    after_login(&mut app, "cl-keep", live("fp-keep", "unknown"));
    assert_eq!(
        named(&app, "cl-keep").claude_plan.as_deref(),
        Some("claude_max")
    );
}

#[test]
fn a_login_equal_to_another_accounts_is_not_bound_and_v_still_binds_it() {
    let other = bound("cl-first", "fp-shared", "claude_max");
    let mut app = app_with(
        &[other, bound("cl-second", "fp-second", "claude_team")],
        "cl-second",
    );
    after_login(&mut app, "cl-second", live("fp-shared", "claude_max"));
    let expected = "cl-second: 이 로그인은 cl-first 과 같은 구독(person@example.test · max)입니다 — 다른 조직이어야 하면 l 로 다시 로그인, 그대로 쓰려면 v";
    assert_eq!(app.status, expected);
    assert_eq!(panel_error(&app), expected);
    assert!(!expected.contains(ORG));
    assert_eq!(
        named(&app, "cl-second").identity_fingerprint(),
        Some("fp-second")
    );
    assert!(app.accounts.iter().any(|a| a.name == "cl-second"));

    app.accounts_verify_identity();
    app.complete_account_checks_for_test(live("fp-shared", "claude_max"));
    assert_eq!(
        named(&app, "cl-second").identity_fingerprint(),
        Some("fp-shared")
    );
    assert_eq!(
        named(&app, "cl-second").claude_plan.as_deref(),
        Some("claude_max")
    );
}

#[test]
fn an_unbound_account_that_is_not_a_fresh_add_is_never_cancelled() {
    let other = bound("cl-owner", "fp-owned", "claude_pro");
    let mut app = app_with(&[other, unbound("cl-old")], "cl-old");
    after_login(&mut app, "cl-old", live("fp-owned", "claude_pro"));
    assert!(
        panel_error(&app).contains("이 로그인은 cl-owner 과 같은 구독(person@example.test · pro)")
    );
    assert!(app.accounts.iter().any(|a| a.name == "cl-old"));
    assert_eq!(named(&app, "cl-old").identity_fingerprint(), None);
}

#[test]
fn a_fresh_add_that_lands_on_an_existing_subscription_is_cancelled_and_erased() {
    let mut app = app_with(
        &[bound("cl-owner2", "fp-owned2", "claude_max")],
        "cl-owner2",
    );
    add_account(&mut app, "cl-fresh-dup");
    let fresh = named(&app, "cl-fresh-dup").clone();
    let slot = fresh.slot_path();
    assert!(slot.exists(), "the home is made before the login runs");
    assert!(app.status.contains("등록"), "{}", app.status);
    assert!(!app.status.contains("u then w"), "{}", app.status);

    app.account_login_finished(&login_id("cl-fresh-dup"), true);
    app.complete_account_checks_for_test(live("fp-owned2", "claude_max"));

    assert_eq!(
        app.status,
        "cl-fresh-dup: 이 로그인은 cl-owner2 과 같은 구독(max)입니다 — 등록을 취소했습니다. 다른 조직으로 추가하려면 브라우저에서 조직을 바꾼 뒤 a 를 누르세요"
    );
    assert!(!app.status.contains(ORG));
    assert!(!app.accounts.iter().any(|a| a.name == "cl-fresh-dup"));
    assert!(saved(&app, "cl-fresh-dup").is_none());
    assert!(!slot.exists(), "the login home is erased");
    assert!(app.accounts.iter().any(|a| a.name == "cl-owner2"));
    assert!(!app.panes.contains(&login_id("cl-fresh-dup")));
}

#[test]
fn a_fresh_add_with_a_new_login_is_bound_at_once() {
    let mut app = app_with(
        &[bound("cl-owner3", "fp-owned3", "claude_pro")],
        "cl-owner3",
    );
    add_account(&mut app, "cl-fresh-ok");
    let slot = named(&app, "cl-fresh-ok").slot_path();
    app.account_login_finished(&login_id("cl-fresh-ok"), true);
    app.complete_account_checks_for_test(live("fp-new", "claude_team"));

    assert_eq!(
        app.status,
        "cl-fresh-ok 등록 완료 (person@example.test · team)"
    );
    let account = named(&app, "cl-fresh-ok");
    assert_eq!(account.identity_fingerprint(), Some("fp-new"));
    assert_eq!(account.claude_workspace.as_deref(), Some(ORG));
    assert_eq!(account.claude_plan.as_deref(), Some("claude_team"));
    let on_disk = saved(&app, "cl-fresh-ok").unwrap();
    assert_eq!(on_disk.identity_fingerprint(), Some("fp-new"));
    assert!(slot.exists());

    // Once bound it is an ordinary account: a later different login is a change, not a fresh registration.
    reopen(&mut app, "cl-fresh-ok");
    after_login(&mut app, "cl-fresh-ok", live("fp-elsewhere", "claude_max"));
    assert!(
        panel_error(&app).contains("team 이어야 하는데"),
        "{}",
        panel_error(&app)
    );
    assert_eq!(
        named(&app, "cl-fresh-ok").identity_fingerprint(),
        Some("fp-new")
    );
    let _ = std::fs::remove_dir_all(slot);
}

#[test]
fn a_fresh_add_with_a_held_launch_is_not_erased_under_it() {
    let mut app = app_with(
        &[bound("cl-owner4", "fp-owned4", "claude_max")],
        "cl-owner4",
    );
    add_account(&mut app, "cl-fresh-held");
    let fresh = named(&app, "cl-fresh-held").clone();
    let launch = PendingSpawn {
        command: mindplayer_core::new_session(Agent::Claude, std::env::temp_dir(), &fresh),
        session_id: "held-launch".into(),
        initial_input: None,
        focus_after_spawn: false,
    };
    app.hold_account_launch(&fresh, launch);
    reopen(&mut app, "cl-fresh-held");
    app.account_login_finished(&login_id("cl-fresh-held"), true);
    app.complete_account_checks_for_test(live("fp-owned4", "claude_max"));
    assert!(app.accounts.iter().any(|a| a.name == "cl-fresh-held"));
    assert!(
        app.status.contains("이 로그인은 cl-owner4 과 같은 구독"),
        "{}",
        app.status
    );
    assert!(fresh.slot_path().exists());
    let _ = std::fs::remove_dir_all(fresh.slot_path());
}

#[test]
fn a_login_that_differs_from_the_bound_one_says_which_plan_was_expected() {
    let mut app = app_with(&[bound("cl-moved", "fp-moved", "claude_team")], "cl-moved");
    after_login(&mut app, "cl-moved", live("fp-other-org", "claude_max"));
    let expected = "cl-moved: team 이어야 하는데 person@example.test · max 로 로그인했습니다 — 맞으면 v, 아니면 l";
    assert_eq!(app.status, expected);
    assert_eq!(panel_error(&app), expected);
    assert_eq!(
        named(&app, "cl-moved").identity_fingerprint(),
        Some("fp-moved")
    );
    assert_eq!(
        named(&app, "cl-moved").claude_plan.as_deref(),
        Some("claude_team")
    );
    assert!(app
        .accounts_panel
        .as_ref()
        .unwrap()
        .confirm_identity
        .is_some());
}

#[test]
fn a_bound_account_without_a_known_plan_says_the_stored_login_was_expected() {
    let mut app = app_with(&[bound("cl-noplan", "fp-noplan", "unknown")], "cl-noplan");
    after_login(&mut app, "cl-noplan", live("fp-else", "unknown"));
    assert_eq!(
        app.status,
        "cl-noplan: 저장된 로그인 이어야 하는데 person@example.test · unknown plan 로 로그인했습니다 — 맞으면 v, 아니면 l"
    );
}

#[test]
fn an_unbound_account_that_is_not_fresh_gets_the_prompt_with_email_and_plan() {
    let mut app = app_with(&[unbound("cl-prompt")], "cl-prompt");
    after_login(
        &mut app,
        "cl-prompt",
        live("fp-prompt", "claude_enterprise"),
    );
    let expected = "cl-prompt: 미등록 로그인 · person@example.test · enterprise — 이 계정이 맞으면 v로 연결, 아니면 l로 로그인";
    assert_eq!(app.status, expected);
    assert_eq!(panel_error(&app), expected);
    assert_eq!(named(&app, "cl-prompt").identity_fingerprint(), None);
}

#[test]
fn v_inspection_applies_the_same_decision() {
    let mut app = app_with(
        &[
            bound("cl-insp-owner", "fp-insp-owner", "claude_pro"),
            bound("cl-insp", "fp-insp", "claude_team"),
        ],
        "cl-insp",
    );
    app.accounts_verify_identity();
    app.complete_account_checks_for_test(live("fp-insp", "claude_team"));
    assert_eq!(
        app.status,
        "cl-insp 로그인 확인 완료 (person@example.test · team)"
    );
    app.accounts_verify_identity();
    app.complete_account_checks_for_test(live("fp-insp-owner", "claude_pro"));
    assert!(
        app.status
            .contains("이 로그인은 cl-insp-owner 과 같은 구독"),
        "{}",
        app.status
    );
    app.accounts_panel.as_mut().unwrap().confirm_identity = None;
    app.accounts_verify_identity();
    app.complete_account_checks_for_test(live("fp-insp-new", "claude_max"));
    assert!(app.status.contains("team 이어야 하는데"), "{}", app.status);
}

#[test]
fn a_launch_check_keeps_a_mismatching_launch_held_with_the_new_wording() {
    let account = bound("cl-launch", "fp-launch", "claude_team");
    let mut app = app_with(std::slice::from_ref(&account), "cl-launch");
    let account = named(&app, "cl-launch").clone();
    let pending = |id: &str| PendingSpawn {
        command: mindplayer_core::new_session(Agent::Claude, std::env::temp_dir(), &account),
        session_id: id.into(),
        initial_input: None,
        focus_after_spawn: false,
    };
    assert!(app
        .verify_account_launch(&account, pending("l-bad"))
        .is_none());
    app.complete_account_checks_for_test(live("fp-launch-other", "claude_max"));
    assert!(app.pending.is_none());
    assert!(app.status.contains("team 이어야 하는데"), "{}", app.status);
    assert_eq!(
        named(&app, "cl-launch").identity_fingerprint(),
        Some("fp-launch")
    );

    // A matching check releases the launch and refreshes the stored plan.
    let mut app = app_with(std::slice::from_ref(&account), "cl-launch");
    let account = named(&app, "cl-launch").clone();
    assert!(app
        .verify_account_launch(&account, pending("l-good"))
        .is_none());
    app.complete_account_checks_for_test(live("fp-launch", "claude_max"));
    assert_eq!(app.pending.as_ref().unwrap().session_id, "l-good");
    assert_eq!(
        named(&app, "cl-launch").claude_plan.as_deref(),
        Some("claude_max")
    );
    let ready = app.pending.take().unwrap();
    let stale = account;
    assert!(
        app.verify_account_launch(&stale, ready).is_some(),
        "an approval survives the facts refresh that happened during the check"
    );
}

#[test]
fn a_failed_reading_never_changes_the_account() {
    let mut app = app_with(&[bound("cl-fail", "fp-fail", "claude_team")], "cl-fail");
    let before = named(&app, "cl-fail").clone();
    after_login(
        &mut app,
        "cl-fail",
        Err(ProbeError::new(
            FailureKind::SignIn,
            "authentication required",
        )),
    );
    assert_eq!(named(&app, "cl-fail"), &before);
}

#[test]
fn re_login_of_a_bound_account_names_the_plan_and_preselects_the_organization() {
    let mut app = app_with(&[bound("cl-relog", "fp-relog", "claude_team")], "cl-relog");
    app.accounts_relogin();
    assert_eq!(
        app.status,
        "cl-relog: team 로 로그인하세요 — 브라우저가 다른 조직을 보이면 조직을 바꾼 뒤 승인하세요"
    );
    let command = &app.pending.as_ref().expect("login queued").command;
    assert_eq!(command.program, "claude");
    assert_eq!(
        command.args,
        vec![
            "--settings".to_string(),
            format!("{{\"forceLoginOrgUUID\":\"{ORG}\"}}"),
            "auth".to_string(),
            "login".to_string()
        ]
    );
}

#[test]
fn re_login_without_a_known_plan_keeps_the_plain_status_and_command() {
    for account in [
        unbound("cl-relog-plain"),
        bound("cl-relog-plain", "fp", "unknown"),
    ] {
        let mut account = account;
        account.claude_workspace = None;
        let mut app = app_with(&[account], "cl-relog-plain");
        app.accounts_relogin();
        assert!(!app.status.contains("로 로그인하세요"), "{}", app.status);
        let command = &app.pending.as_ref().unwrap().command;
        assert_eq!(command.args, vec!["auth", "login"]);
    }
}

#[test]
fn a_confirmation_that_finds_the_stored_login_again_reports_it_as_confirmed() {
    let mut app = app_with(&[bound("cl-back", "fp-back", "claude_team")], "cl-back");
    let account = named(&app, "cl-back").clone();
    app.accounts_panel.as_mut().unwrap().confirm_identity =
        Some((account, "fp-earlier".into(), std::env::temp_dir()));
    app.accounts_verify_identity();
    app.complete_account_checks_for_test(live("fp-back", "claude_team"));
    assert_eq!(
        app.status,
        "cl-back 로그인 확인 완료 (person@example.test · team)"
    );
    assert_eq!(
        named(&app, "cl-back").identity_fingerprint(),
        Some("fp-back")
    );
}
