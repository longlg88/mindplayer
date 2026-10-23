//! The Accounts screen is the only way to make a second login, so what it
//! refuses matters as much as what it creates.

use super::accounts_panel::{AccountRow, LOGIN_PREFIX};
use super::*;
use mindplayer_core::accounts::{Account, LaunchEnv, Role, DEFAULT_ACCOUNT, MULTI_ACCOUNT_AGENTS};

fn expected_codex_login_env(account: &Account) -> LaunchEnv {
    let mut env = account.launch_env();
    env.unset.extend(
        [
            "AI_GATEWAY_API_KEY",
            "TYPESAFE_API_KEY",
            "VERCEL_OIDC_TOKEN",
            "MINDPLAYER_LOCAL_ROUTER_TOKEN",
            "MINDPLAYER_ROUTER_AVAILABILITY",
            "MINDPLAYER_ROUTER_STATUS",
            "MINDPLAYER_CLAUDE_JEV_URL",
            "MINDPLAYER_CLAUDE_JEV_TOKEN",
            "MINDPLAYER_PER_PROMPT_CHOICE",
            "MINDPLAYER_JEV_DEFAULT",
        ]
        .map(String::from),
    );
    env
}

fn open() -> App {
    let mut app = App::new();
    app.accounts = MULTI_ACCOUNT_AGENTS.map(Account::inherited).to_vec();
    app.open_accounts();
    app
}

/// Put the cursor on the first account of `agent`.
fn select(app: &mut App, agent: Agent) {
    let at = app
        .account_rows()
        .iter()
        .position(|row| match row {
            AccountRow::Entry(i) => app.accounts[*i].provider == agent,
            AccountRow::Header(_) => false,
        })
        .expect("no account row for that provider");
    app.accounts_panel.as_mut().unwrap().selected = at;
}

#[test]
fn every_provider_that_can_hold_accounts_gets_a_heading() {
    let app = open();
    let headers: Vec<Agent> = app
        .account_rows()
        .iter()
        .filter_map(|row| match row {
            AccountRow::Header(agent) => Some(*agent),
            AccountRow::Entry(_) => None,
        })
        .collect();
    assert_eq!(headers, MULTI_ACCOUNT_AGENTS.to_vec());
    assert!(
        headers.contains(&Agent::Cursor),
        "every provider can hold a second login now"
    );
}

#[test]
fn the_cursor_opens_on_an_account_rather_than_a_heading() {
    let app = open();
    assert!(matches!(
        app.account_rows()[app.accounts_panel.as_ref().unwrap().selected],
        AccountRow::Entry(_)
    ));
}

#[test]
fn confirming_a_live_codex_identity_binds_the_selected_account() {
    let mut app = open();
    select(&mut app, Agent::Codex);
    let selected_row = app.accounts_panel.as_ref().unwrap().selected;
    let AccountRow::Entry(index) = app.account_rows()[selected_row] else {
        panic!("Codex account row expected")
    };
    app.accounts_panel.as_mut().unwrap().confirm_identity = Some((
        app.accounts[index].clone(),
        "codex-user-test-fingerprint".into(),
        std::env::temp_dir(),
    ));

    app.accounts_verify_identity();
    assert!(
        app.accounts[index].codex_identity.is_none(),
        "confirmation requires a fresh read"
    );
    app.complete_account_checks_for_test(Ok(mindplayer_core::limits::CodexIdentity {
        fingerprint: "codex-user-test-fingerprint".into(),
        email: "person@example.test".into(),
        workspace: "workspace-test".into(),
    }));

    assert_eq!(
        app.accounts[index].codex_identity.as_deref(),
        Some("codex-user-test-fingerprint")
    );
    assert!(app.status.contains("identity bound"));
    assert!(app
        .accounts_panel
        .as_ref()
        .unwrap()
        .confirm_identity
        .is_none());
}

#[test]
fn inherited_codex_sign_in_is_available_without_logging_anyone_out() {
    let mut app = open();
    select(&mut app, Agent::Codex);
    app.accounts_relogin();
    let pending = app
        .pending
        .as_ref()
        .expect("selected Codex login must be recoverable");
    assert_eq!(pending.command.program, "codex");
    assert_eq!(pending.command.args, ["login"]);
    assert_eq!(
        pending.command.env,
        expected_codex_login_env(&Account::inherited(Agent::Codex))
    );
}

#[test]
fn inherited_claude_sign_in_is_available_without_logging_anyone_out() {
    let mut app = open();
    select(&mut app, Agent::Claude);
    app.accounts_relogin();
    let pending = app
        .pending
        .as_ref()
        .expect("selected Claude login must be recoverable");
    assert_eq!(pending.command.program, "claude");
    assert_eq!(pending.command.args, ["auth", "login"]);
    assert_eq!(
        pending.command.env,
        Account::inherited(Agent::Claude).launch_env()
    );
}

#[test]
fn arbitrary_claude_label_can_request_identity_verification() {
    let mut app = super::tests::isolated_app();
    let mut account = Account::inherited(Agent::Claude);
    account.name = "research_lab_42".into();
    app.accounts = vec![account];
    app.open_accounts();
    select(&mut app, Agent::Claude);
    app.accounts_verify_identity();
    let message = app
        .accounts_panel
        .as_ref()
        .unwrap()
        .error
        .as_deref()
        .unwrap_or("");
    assert!(
        message.contains("로그인 확인 중"),
        "Claude verification was refused: {message}"
    );
}

#[test]
fn moving_stops_at_the_ends_instead_of_wrapping() {
    let mut app = open();
    app.accounts_move(-50);
    assert_eq!(app.accounts_panel.as_ref().unwrap().selected, 0);
    app.accounts_move(50);
    assert_eq!(
        app.accounts_panel.as_ref().unwrap().selected,
        app.account_rows().len() - 1
    );
}

#[test]
fn a_name_that_is_not_safe_is_refused_with_a_reason() {
    let mut app = open();
    select(&mut app, Agent::Codex);
    app.accounts_start_add();
    for c in "../escape".chars() {
        app.accounts_name_push(c);
    }
    app.accounts_confirm_add();

    assert!(
        app.accounts_panel.as_ref().unwrap().error.is_some(),
        "a refused name must say why"
    );
    assert_eq!(
        app.accounts.iter().filter(|a| !a.is_inherited()).count(),
        0,
        "a refused name still created an account"
    );
}

#[test]
fn a_name_already_taken_is_refused() {
    let mut app = open();
    select(&mut app, Agent::Codex);
    app.accounts_start_add();
    for c in DEFAULT_ACCOUNT.chars() {
        app.accounts_name_push(c);
    }
    app.accounts_confirm_add();
    assert!(app.accounts_panel.as_ref().unwrap().error.is_some());
    assert_eq!(
        app.accounts
            .iter()
            .filter(|a| a.provider == Agent::Codex)
            .count(),
        1
    );
}

#[test]
fn the_login_this_machine_already_had_cannot_be_dropped() {
    let mut app = open();
    select(&mut app, Agent::Claude);
    let before = app.accounts.len();
    app.accounts_remove();
    assert_eq!(app.accounts.len(), before);
    assert!(app.accounts_panel.as_ref().unwrap().error.is_some());
}

#[test]
fn an_account_can_be_turned_off_and_back_on() {
    let mut app = open();
    select(&mut app, Agent::Kiro);
    let i = app
        .accounts
        .iter()
        .position(|a| a.provider == Agent::Kiro)
        .unwrap();

    app.accounts_toggle_disabled();
    assert!(app.accounts[i].disabled);
    app.accounts_toggle_disabled();
    assert!(!app.accounts[i].disabled);
}

/// The bug this screen shipped with: two primaries left the choice to list
/// order, so a freshly signed-in account could never be reached.
#[test]
fn choosing_one_account_puts_the_others_of_that_provider_in_reserve() {
    let mut app = open();
    let home = limits_home_for_app();
    let second = Account::isolated(&home, Agent::Codex, "work").unwrap();
    app.accounts.push(second.clone());

    assert_eq!(
        app.account_for(Agent::Codex).name,
        DEFAULT_ACCOUNT,
        "the first account starts out in use"
    );

    let at = app
        .account_rows()
        .iter()
        .position(|row| match row {
            AccountRow::Entry(i) => app.accounts[*i].name == second.name,
            AccountRow::Header(_) => false,
        })
        .unwrap();
    app.accounts_panel.as_mut().unwrap().selected = at;
    app.accounts_make_primary();

    assert_eq!(
        app.account_for(Agent::Codex).name,
        second.name,
        "picking an account did not change which one a new session takes"
    );
    assert_eq!(
        app.accounts
            .iter()
            .filter(|a| a.provider == Agent::Codex && a.role == Role::Primary)
            .count(),
        1,
        "two primaries leave the choice to list order again"
    );
    // Another provider's choice is not disturbed by this one.
    assert_eq!(app.account_for(Agent::Claude).name, DEFAULT_ACCOUNT);
}

#[test]
fn an_account_that_is_off_cannot_be_chosen_or_started() {
    let mut app = open();
    select(&mut app, Agent::Kiro);
    app.accounts_toggle_disabled();

    app.accounts_make_primary();
    assert!(app.accounts_panel.as_ref().unwrap().error.is_some());

    app.accounts_start_session();
    assert!(
        app.pending.is_none(),
        "a session started on an account that is off"
    );
}

#[test]
fn explicit_primary_selection_overrides_the_startup_codex_home() {
    let mut app = open();
    let second = Account::isolated(&limits_home_for_app(), Agent::Codex, "work").unwrap();
    app.preferred_codex_home = Some(second.slot_path());
    app.accounts.push(second.clone());
    assert_eq!(app.account_for(Agent::Codex).name, second.name);

    select(&mut app, Agent::Codex);
    app.accounts_make_primary();

    assert_eq!(app.account_for(Agent::Codex).name, DEFAULT_ACCOUNT);
    assert!(app.preferred_codex_home.is_none());
}

#[test]
fn enter_starts_a_session_on_the_highlighted_account() {
    let mut app = open();
    let home = limits_home_for_app();
    let second = Account::isolated(&home, Agent::Codex, "work").unwrap();
    app.accounts.push(second.clone());
    let at = app
        .account_rows()
        .iter()
        .position(|row| match row {
            AccountRow::Entry(i) => app.accounts[*i].name == second.name,
            AccountRow::Header(_) => false,
        })
        .unwrap();
    app.accounts_panel.as_mut().unwrap().selected = at;

    app.accounts_start_session();

    let pending = app.pending.as_ref().expect("no session was queued");
    assert_eq!(
        pending.command.env,
        second.launch_env(),
        "the session started on a different account than the one highlighted"
    );
    assert_eq!(
        app.account_for(Agent::Codex).name,
        DEFAULT_ACCOUNT,
        "starting one session must not change which account the next one takes"
    );
}

/// Account labels can change; provider homes cannot. Codex persists absolute
/// rollout paths, and another process may still be using the original home.
mod renaming {
    use super::*;
    use mindplayer_core::accounts::{slot_dir, Slot};

    /// An app whose isolated slots live somewhere this test owns.
    fn with_slot(name: &str) -> (App, std::path::PathBuf) {
        let mut app = open();
        let home = limits_home_for_app();
        let account = Account::isolated(&home, Agent::Codex, name).unwrap();
        let dir = account.slot_path();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.json"), "{}").unwrap();
        app.accounts.push(account);
        let at = app
            .account_rows()
            .iter()
            .position(|row| match row {
                AccountRow::Entry(i) => app.accounts[*i].name == name,
                AccountRow::Header(_) => false,
            })
            .unwrap();
        app.accounts_panel.as_mut().unwrap().selected = at;
        (app, dir)
    }

    fn type_name(app: &mut App, name: &str) {
        app.accounts_start_rename();
        // The buffer starts at the current name so a typo is a correction.
        app.accounts_panel.as_mut().unwrap().new_name = Some(String::new());
        for c in name.chars() {
            app.accounts_name_push(c);
        }
        app.accounts_confirm_add();
    }

    #[test]
    fn renaming_starts_from_the_name_it_has_now() {
        let (mut app, dir) = with_slot("rn-start");
        app.accounts_start_rename();
        assert_eq!(
            app.accounts_panel.as_ref().unwrap().new_name.as_deref(),
            Some("rn-start")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn renaming_keeps_the_provider_home_and_absolute_session_paths() {
        let (mut app, old) = with_slot("rn-move-from");
        let rollout = old.join("sessions/record.jsonl");
        std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        std::fs::write(&rollout, "session history\n").unwrap();
        let index = old.join("absolute-rollout-path");
        std::fs::write(&index, rollout.to_string_lossy().as_bytes()).unwrap();
        let original_env = app.accounts.last().unwrap().launch_env();
        type_name(&mut app, "rn-move-to");

        let renamed = app
            .accounts
            .iter()
            .find(|a| a.name == "rn-move-to")
            .unwrap_or_else(|| {
                panic!(
                    "the account kept its old name; error was {:?}",
                    app.accounts_panel.as_ref().and_then(|p| p.error.clone())
                )
            });
        let Slot::Isolated { path } = &renamed.slot else {
            panic!("an isolated account lost its slot");
        };
        assert_eq!(
            path, &old,
            "changing a display name must not move the provider's home"
        );
        assert!(
            path.join("auth.json").is_file(),
            "the login was lost: {path:?}"
        );
        assert_eq!(renamed.launch_env(), original_env);
        assert!(rollout.is_file(), "the provider's indexed path disappeared");
        assert_eq!(
            std::fs::read_to_string(index).unwrap(),
            rollout.to_string_lossy()
        );
        assert!(!slot_dir(&limits_home_for_app(), Agent::Codex, "rn-move-to").exists());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn the_old_name_cannot_create_a_second_account_on_the_retained_home() {
        let (mut app, dir) = with_slot("rn-reuse-from");
        type_name(&mut app, "rn-reuse-to");
        let before = app.accounts.len();
        app.accounts_start_add();
        for c in "rn-reuse-from".chars() {
            app.accounts_name_push(c);
        }
        app.accounts_confirm_add();
        assert_eq!(
            app.accounts.len(),
            before,
            "the old name reused an existing login"
        );
        assert!(app.accounts_panel.as_ref().unwrap().error.is_some());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(slot_dir(
            &limits_home_for_app(),
            Agent::Codex,
            "rn-reuse-to",
        ));
    }

    #[test]
    fn the_inherited_account_is_only_relabelled() {
        let mut app = open();
        app.accounts_panel.as_mut().unwrap().selected = 1;
        assert!(app.accounts[0].is_inherited());
        type_name(&mut app, "personal");

        assert_eq!(app.accounts[0].name, "personal");
        assert!(
            app.accounts[0].is_inherited(),
            "relabelling turned it into a slot of its own"
        );
        assert_eq!(
            app.accounts[0]
                .launch_env()
                .set
                .first()
                .map(|(key, _)| key.as_str()),
            Some("CODEX_HOME"),
            "the inherited Codex login must stay pinned to the machine's default home"
        );
    }

    #[test]
    fn a_name_that_is_not_safe_or_already_taken_is_refused() {
        let (mut app, dir) = with_slot("rn-refuse");
        for bad in ["../escape", DEFAULT_ACCOUNT] {
            type_name(&mut app, bad);
            assert!(
                app.accounts_panel.as_ref().unwrap().error.is_some(),
                "`{bad}` was accepted as a new name"
            );
            assert!(
                app.accounts.iter().any(|a| a.name == "rn-refuse"),
                "`{bad}` renamed the account anyway"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_account_a_pane_is_running_on_is_not_renamed_underneath_it() {
        let (mut app, dir) = with_slot("rn-busy");
        let account = app.accounts.last().unwrap().clone();
        let mut live = super::super::tests::session("live-1", Agent::Codex, false);
        live.file = account.session_root(&limits_home_for_app()).join("r.jsonl");
        app.all_sessions.push(live);
        app.focus_or_add_pane("live-1");

        type_name(&mut app, "renamed");
        assert!(app.accounts.iter().any(|a| a.name == "rn-busy"));
        assert!(app.accounts_panel.as_ref().unwrap().error.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn renaming_relabels_usage_and_discards_an_old_refresh() {
        let (mut app, dir) = with_slot("rn-quota-from");
        let row = mindplayer_core::limits::QuotaRow {
            label: "codex weekly".into(),
            agent: Agent::Codex,
            account: "rn-quota-from".into(),
            used_percent: Some(42.0),
            ..Default::default()
        };
        let duplicate = mindplayer_core::limits::QuotaRow {
            account: "other-slot".into(),
            used_percent: None,
            detail: "same login as rn-quota-from".into(),
            ..row.clone()
        };
        app.limits = Some(vec![row.clone(), duplicate]);
        app.quota_cache = Some((vec![row], chrono::Utc::now()));
        app.unusable_waiting
            .push(app.accounts.last().unwrap().clone());
        let (tx, rx) = std::sync::mpsc::channel();
        app.limits_rx = Some(rx);
        app.limits_started = Some(std::time::Instant::now());
        app.limits_retry_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(60));

        type_name(&mut app, "rn-quota-to");

        assert_eq!(app.limits.as_ref().unwrap()[0].account, "rn-quota-to");
        assert_eq!(
            app.limits.as_ref().unwrap()[1].detail,
            "same login as rn-quota-to"
        );
        assert_eq!(app.unusable_waiting[0], *app.accounts.last().unwrap());
        assert_eq!(
            app.quota_cache.as_ref().unwrap().0[0].account,
            "rn-quota-to"
        );
        assert!(
            app.limits_rx.is_none(),
            "the old worker result was retained"
        );
        assert!(app.limits_started.is_none());
        assert!(app.limits_retry_at.is_none());
        assert!(app.limits_force_refresh);
        assert!(tx
            .send(UsageReading {
                rows: vec![],
                unusable: vec![]
            })
            .is_err());

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(app.accounts.last().unwrap().slot_path());
    }
}

#[test]
fn a_heading_is_not_an_account_to_act_on() {
    let mut app = open();
    app.accounts_panel.as_mut().unwrap().selected = 0;
    assert!(matches!(app.account_rows()[0], AccountRow::Header(_)));
    let before = app.accounts.clone();
    app.accounts_toggle_disabled();
    app.accounts_make_primary();
    app.accounts_remove();
    app.accounts_start_session();
    assert_eq!(app.accounts, before, "a heading changed an account");
    assert!(app.pending.is_none(), "a heading started a session");
}

#[test]
fn a_new_pane_opens_for_the_account_being_added() {
    let mut app = open();
    let home = limits_home_for_app();
    let account = Account::isolated(&home, Agent::Codex, "second").unwrap();
    app.accounts.push(account.clone());
    app.request_login(&account);

    let pending = app.pending.as_ref().expect("no pane was queued");
    assert!(pending.session_id.starts_with(LOGIN_PREFIX));
    assert_eq!(pending.command.args, vec!["login"]);
    assert_eq!(
        pending.command.env,
        expected_codex_login_env(&account),
        "a pane outside the slot would write over the existing login"
    );
    assert!(
        app.accounts_panel.is_none(),
        "the screen stayed over the pane it just opened"
    );
}

#[test]
fn opening_the_same_one_twice_reuses_its_row() {
    let mut app = open();
    let home = limits_home_for_app();
    let account = Account::isolated(&home, Agent::Codex, "second").unwrap();
    app.accounts.push(account.clone());
    app.request_login(&account);
    app.request_login(&account);
    assert_eq!(
        app.extra_sessions
            .iter()
            .filter(|s| s.id.starts_with(LOGIN_PREFIX))
            .count(),
        1
    );
}

/// A pane that carries no baseline adopts any new session of its agent and
/// cwd, so this one has to be refused explicitly or it takes another pane's
/// work.
#[test]
fn such_a_pane_never_turns_into_a_session() {
    let mut app = open();
    let home = limits_home_for_app();
    let account = Account::isolated(&home, Agent::Codex, "second").unwrap();
    app.accounts.push(account.clone());
    app.request_login(&account);

    let dir = app.cwd.clone();
    let mut real = super::tests::session("real-one", Agent::Codex, false);
    real.cwd = dir;
    real.started_at = Some(chrono::Utc::now());
    app.all_sessions.push(real);

    app.merge_extras();
    assert!(
        app.extra_sessions
            .iter()
            .any(|s| s.id.starts_with(LOGIN_PREFIX)),
        "the pane was re-keyed onto a session it did not create"
    );
    assert!(
        app.all_sessions.iter().any(|s| s.id == "real-one"),
        "the genuine session was swallowed"
    );
}

/// Reported from a pane's own log:
/// `ERROR: No saved session found with ID login:codex:personal`.
/// A sign-in pane has no transcript, so asking its provider to resume it makes
/// the provider answer that no such session exists.
mod a_row_we_invented_is_never_resumed {
    use super::*;
    use mindplayer_core::session::is_synthetic_id;

    #[test]
    fn every_invented_prefix_is_refused() {
        for id in [
            "new:codex:1",
            "handoff:codex:claude:2",
            "login:codex:personal",
        ] {
            assert!(is_synthetic_id(id), "`{id}` would be handed to `resume`");
        }
        assert!(
            !is_synthetic_id("01a095a0-af64-7cc2-94ac-f9c5ab5f0eb6"),
            "a real session id must still resume"
        );
    }

    #[test]
    fn a_sign_in_row_does_not_queue_a_resume() {
        let mut app = open();
        let home = limits_home_for_app();
        let account = Account::isolated(&home, Agent::Codex, "second").unwrap();
        app.accounts.push(account.clone());
        app.request_login(&account);
        // The pane it opened is what a later keypress would act on.
        app.pending = None;

        let row = app
            .all_sessions
            .iter()
            .find(|s| s.id.starts_with(LOGIN_PREFIX))
            .cloned()
            .expect("the sign-in pane has no row");
        app.all_sessions = vec![row.clone()];
        app.visible = vec![crate::app::Row::Session(0)];
        app.selected = 0;

        app.request_resume();

        assert!(
            app.pending.is_none(),
            "the sign-in row was queued for `resume`, which its provider refuses: {:?}",
            app.pending.as_ref().map(|p| p.command.args.clone())
        );
    }
}

/// Erasing a login cannot be undone, so `x` asks once before doing it.
mod erasing_a_login {
    use super::*;

    /// The limits home is per-process, not per-test, so two tests sharing a
    /// slot name share the directory one of them erases.
    fn app_with_a_spare(name: &str) -> (App, PathBuf) {
        let mut app = App::new();
        let home = crate::app::limits_home_for_app();
        let spare = Account::isolated(&home, Agent::Codex, name).unwrap();
        let path = spare.slot_path();
        let _ = std::fs::create_dir_all(&path);
        app.accounts = vec![Account::inherited(Agent::Codex), spare];
        app.open_accounts();
        select(&mut app, Agent::Codex);
        // Past the machine's own login, onto the spare.
        app.accounts_move(1);
        (app, path)
    }

    #[test]
    fn one_press_asks_and_erases_nothing() {
        let (mut app, path) = app_with_a_spare("one-press");
        app.accounts_remove();

        assert_eq!(
            app.accounts.len(),
            2,
            "the account was removed on one press"
        );
        assert!(path.exists(), "the login was erased on one press");
        assert!(app.status.contains("x again"), "{}", app.status);
    }

    #[test]
    fn a_second_press_removes_the_account_and_erases_its_login() {
        let (mut app, path) = app_with_a_spare("two-press");
        app.accounts_remove();
        app.accounts_remove();

        assert_eq!(app.accounts.len(), 1, "{:?}", app.accounts);
        assert!(!path.exists(), "the login survived: {path:?}");
    }

    #[test]
    fn anything_in_between_calls_it_off() {
        let (mut app, path) = app_with_a_spare("cancelled");
        app.accounts_remove();
        app.accounts_cancel_remove();
        app.accounts_remove();

        assert_eq!(
            app.accounts.len(),
            2,
            "a cancelled erase still went through"
        );
        assert!(path.exists());
    }

    /// A sign-in pane writes into the slot, so erasing it underneath the sign-in is refused like any other pane.
    #[test]
    fn a_login_pane_on_the_account_blocks_the_erase() {
        let (mut app, path) = app_with_a_spare("signing-in");
        let spare = app.accounts[1].clone();
        app.request_login(&spare);
        app.open_accounts();
        select(&mut app, Agent::Codex);
        app.accounts_move(1);
        app.accounts_remove();
        app.accounts_remove();

        assert_eq!(app.accounts.len(), 2, "erased under a running sign-in");
        assert!(
            path.exists(),
            "the slot a sign-in is writing to was deleted"
        );
    }

    #[test]
    fn the_machines_own_login_is_never_offered_for_erasing() {
        let (mut app, _) = app_with_a_spare("inherited");
        app.accounts_move(-1);
        app.accounts_remove();
        app.accounts_remove();
        assert_eq!(app.accounts.len(), 2, "the inherited login was removed");
    }
}

/// A new session has no transcript until the CLI writes one, so a path cannot
/// say which account it is on. Erasing that account then deleted the home of
/// a CLI that was running in it.
#[test]
fn a_session_still_starting_on_the_account_blocks_the_erase() {
    let mut app = App::new();
    let home = crate::app::limits_home_for_app();
    let spare = Account::isolated(&home, Agent::Codex, "starting").unwrap();
    let _ = std::fs::create_dir_all(spare.slot_path());
    app.accounts = vec![Account::inherited(Agent::Codex), spare.clone()];

    app.request_new_on(&spare, "");
    assert!(
        app.pane_count_on(&spare) > 0,
        "a session starting on this account is not counted, so erasing it would delete a live home"
    );
}

/// A provider with nothing behind it was reached for every few minutes
/// forever. It is asked about once, and a no turns it off.
mod unavailable_account_prompt {
    use super::*;

    fn app_with_prompt() -> (App, Account) {
        let mut app = App::new();
        let spare =
            Account::isolated(&crate::app::limits_home_for_app(), Agent::Kiro, "unused").unwrap();
        app.accounts = vec![Account::inherited(Agent::Kiro), spare.clone()];
        app.unusable_prompt = Some(spare.clone());
        (app, spare)
    }

    #[test]
    fn no_turns_the_account_off_and_keeps_it() {
        let (mut app, spare) = app_with_prompt();
        app.decline_unusable_prompt();

        let found = app
            .accounts
            .iter()
            .find(|a| a.name == spare.name)
            .expect("the account is kept, only turned off");
        assert!(found.disabled, "a no must stop anything reaching for it");
        assert!(app.unusable_prompt.is_none());
        assert!(app.status.contains("off"), "{}", app.status);
    }

    #[test]
    fn a_disabled_account_is_no_longer_probed() {
        let (mut app, spare) = app_with_prompt();
        app.decline_unusable_prompt();
        assert!(
            !app.probe_accounts().iter().any(|a| a.name == spare.name),
            "a declined account is still being reached for"
        );
    }

    #[test]
    fn yes_opens_a_pane_that_signs_it_in() {
        let (mut app, _) = app_with_prompt();
        app.confirm_unusable_prompt();
        assert!(app.unusable_prompt.is_none());
        assert!(
            app.pending.is_some() || !app.panes.is_empty(),
            "a yes must actually start the sign-in"
        );
    }

    #[test]
    fn any_other_key_puts_it_away_without_deciding() {
        let (mut app, spare) = app_with_prompt();
        app.dismiss_unusable_prompt();
        assert!(app.unusable_prompt.is_none());
        let found = app.accounts.iter().find(|a| a.name == spare.name).unwrap();
        assert!(!found.disabled, "dismissing must not turn anything off");
    }

    /// A question raised while a pane has the keyboard would take the next key typed into it.
    #[test]
    fn it_waits_while_a_pane_has_the_keyboard() {
        let (mut app, spare) = app_with_prompt();
        app.unusable_prompt = None;
        app.screen = Screen::Main;
        app.focus = Focus::Terminal;
        app.unusable_waiting.push(spare.clone());
        assert!(!app.raise_unusable_prompt());
        assert!(app.unusable_prompt.is_none(), "raised over a focused pane");
        app.focus = Focus::List;
        app.help_visible = true;
        assert!(
            !app.raise_unusable_prompt(),
            "raised over help, whose keys it would not get"
        );
        app.help_visible = false;
        assert!(app.raise_unusable_prompt());
        assert_eq!(
            app.unusable_prompt.as_ref().map(|a| a.name.clone()),
            Some(spare.name)
        );
    }

    #[test]
    fn yes_on_the_machines_own_login_opens_nothing() {
        let (mut app, _) = app_with_prompt();
        app.unusable_prompt = Some(Account::inherited(Agent::Kiro));
        app.confirm_unusable_prompt();
        assert!(
            app.pending.is_none(),
            "a sign-in was started on the machine's own login"
        );
        assert!(app.status.contains("outside MindPlayer"), "{}", app.status);
    }

    /// The refresh comes round every few minutes; a question that reappears
    /// each time is the complaint, not the fix.
    #[test]
    fn the_same_account_is_asked_about_once_a_run() {
        let (mut app, spare) = app_with_prompt();
        let key = format!("{}/{}", spare.provider.as_str(), spare.name);
        assert!(app.unusable_asked.insert(key.clone()));
        app.dismiss_unusable_prompt();
        assert!(
            !app.unusable_asked.insert(key),
            "the account would be asked about again on the next refresh"
        );
    }
}
