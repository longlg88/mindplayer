//! The Accounts screen is the only way to make a second login, so what it
//! refuses matters as much as what it creates.

use super::accounts_panel::{AccountRow, LOGIN_PREFIX};
use super::*;
use mindplayer_core::accounts::{Account, Role, DEFAULT_ACCOUNT, MULTI_ACCOUNT_AGENTS};

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
        !headers.contains(&Agent::Cursor),
        "cursor cannot hold a second login yet, so offering one would lie"
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
        account.launch_env(),
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
