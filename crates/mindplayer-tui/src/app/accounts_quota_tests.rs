//! Usage per login: where each reading comes from, and where it is shown.

use super::*;
use mindplayer_core::accounts::{Account, MULTI_ACCOUNT_AGENTS};
use mindplayer_core::limits::QuotaRow;

fn row(agent: Agent, account: &str, label: &str, used: f64) -> QuotaRow {
    QuotaRow {
        label: label.into(),
        agent,
        account: account.into(),
        used_percent: Some(used),
        ..Default::default()
    }
}

fn app_with_two_codex() -> (App, Account) {
    let mut app = App::new();
    let second = Account::isolated(&limits_home_for_app(), Agent::Codex, "other").unwrap();
    app.accounts = vec![Account::inherited(Agent::Codex), second.clone()];
    (app, second)
}

#[test]
fn every_login_that_could_serve_is_read_and_the_ones_that_cannot_are_not() {
    let (mut app, second) = app_with_two_codex();
    let probed: Vec<String> = app
        .probe_accounts()
        .iter()
        .map(|a| a.name.clone())
        .collect();
    assert!(probed.iter().any(|n| n == &second.name), "{probed:?}");
    app.accounts.push(Account::inherited(Agent::Cursor));
    assert!(
        app.probe_accounts()
            .iter()
            .any(|a| a.provider == Agent::Cursor),
        "cursor has a reading like every other provider"
    );

    app.accounts[1].disabled = true;
    assert!(
        !app.probe_accounts().iter().any(|a| a.name == second.name),
        "an account that is off still spent a request"
    );
}

/// Showing only the account a new session would take hid the one that was
/// actually exhausted — the account switched away from stays at 100% while the
/// fresh one it was switched to has no reading until it has run something.
#[test]
fn the_footer_shows_every_signed_in_login() {
    let (mut app, second) = app_with_two_codex();
    app.limits = Some(vec![
        row(Agent::Codex, "default", "codex", 10.0),
        row(Agent::Codex, &second.name, "codex", 90.0),
    ]);

    let footer = app.quota_rows();
    assert_eq!(
        footer.len(),
        2,
        "a configured login went missing: {footer:?}"
    );
    let seen: Vec<Option<f64>> = footer.iter().map(|r| r.used_percent).collect();
    assert!(
        seen.contains(&Some(10.0)) && seen.contains(&Some(90.0)),
        "{footer:?}"
    );
    assert_eq!(app.quota_rows_all().len(), footer.len());
}

/// Which login is in use decides where a new session goes, not which figures
/// are worth seeing — switching must not make an allowance disappear.
#[test]
fn switching_the_login_leaves_every_reading_on_screen() {
    let (mut app, second) = app_with_two_codex();
    app.limits = Some(vec![
        row(Agent::Codex, "default", "codex", 10.0),
        row(Agent::Codex, &second.name, "codex", 90.0),
    ]);
    let before = app.quota_rows().len();

    app.open_accounts();
    app.accounts_panel.as_mut().unwrap().selected = 2;
    app.accounts_make_primary();

    let footer = app.quota_rows();
    assert_eq!(
        footer.len(),
        before,
        "switching dropped a reading: {footer:?}"
    );
    let seen: Vec<Option<f64>> = footer.iter().map(|r| r.used_percent).collect();
    assert!(
        seen.contains(&Some(10.0)) && seen.contains(&Some(90.0)),
        "{footer:?}"
    );
}

/// A cache written before readings carried an account has no name on its rows;
/// dropping them would blank the footer on the first start after an upgrade.
#[test]
fn a_reading_from_before_accounts_existed_is_still_shown() {
    let mut app = App::new();
    app.accounts = MULTI_ACCOUNT_AGENTS.map(Account::inherited).to_vec();
    app.limits = Some(vec![QuotaRow {
        label: "kiro".into(),
        agent: Agent::Kiro,
        account: String::new(),
        used_percent: Some(52.0),
        ..Default::default()
    }]);
    assert_eq!(app.quota_rows().len(), 1);
}

mod where_a_reading_comes_from {
    use super::*;
    use mindplayer_core::limits::account_quota_rows;

    /// Codex reads its own rollouts, so an account with its own home and no
    /// sessions yet must say so rather than showing the other login's numbers.
    #[test]
    fn a_codex_account_reads_its_own_store() {
        let home = limits_home_for_app();
        let account = Account::isolated(&home, Agent::Codex, "empty").unwrap();
        std::fs::create_dir_all(account.slot_path()).unwrap();

        let rows = account_quota_rows(&account, &home);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].agent, Agent::Codex);
        assert_eq!(rows[0].account, "empty");
        assert!(
            rows[0].used_percent.is_none(),
            "an empty store reported a usage figure: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(account.slot_path());
    }

    /// The Keychain holds the login this machine came with. Showing its numbers
    /// under another account's name is a wrong answer wearing the right one.
    #[test]
    fn a_claude_account_of_its_own_never_shows_the_machine_logins_numbers() {
        let home = limits_home_for_app();
        let account = Account::isolated(&home, Agent::Claude, "second").unwrap();
        std::fs::create_dir_all(account.slot_path()).unwrap();

        let rows = account_quota_rows(&account, &home);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].used_percent.is_none(), "{rows:?}");
        assert!(
            rows[0].detail.contains("no credentials of its own"),
            "the row did not say why it is empty: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(account.slot_path());
    }

    /// Cursor's usage is read with a token the Keychain holds once per
    /// machine. A second Cursor login runs its turns fine, but borrowing that
    /// figure would label the machine login's usage as this account's.
    #[test]
    fn a_cursor_account_of_its_own_shows_no_figure_rather_than_the_machine_logins() {
        let home = limits_home_for_app();
        let account = Account::isolated(&home, Agent::Cursor, "second").unwrap();

        let rows = account_quota_rows(&account, &home);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].agent, Agent::Cursor);
        assert_eq!(rows[0].account, "second");
        assert!(
            rows[0].used_percent.is_none(),
            "a second Cursor login was given a figure it cannot have: {rows:?}"
        );
        assert!(
            rows[0].detail.contains("not available"),
            "the row does not say why it is empty: {rows:?}"
        );
    }

    #[test]
    fn rows_of_other_providers_never_ride_along() {
        let home = limits_home_for_app();
        let account = Account::isolated(&home, Agent::Kiro, "team").unwrap();
        let rows = account_quota_rows(&account, &home);
        assert!(
            rows.iter().all(|r| r.agent == Agent::Kiro),
            "a probe for one provider produced another's rows: {rows:?}"
        );
    }
}

mod asking_too_often {
    use super::*;
    use mindplayer_core::limits::rows_are_rate_limited;

    #[test]
    fn a_refusal_for_asking_too_often_is_recognised_from_the_rows() {
        let refused = QuotaRow {
            label: "claude".into(),
            agent: Agent::Claude,
            account: "default".into(),
            detail: "request failed: response status 429".into(),
            ..Default::default()
        };
        assert!(rows_are_rate_limited(&[refused]));
    }

    #[test]
    fn a_number_that_merely_contains_429_is_not_one() {
        let fine = QuotaRow {
            label: "claude".into(),
            agent: Agent::Claude,
            account: "default".into(),
            used_percent: Some(42.9),
            detail: "4290/10000".into(),
            ..Default::default()
        };
        assert!(!rows_are_rate_limited(&[fine]));
    }

    /// Codex reads a local file and Kiro runs its own CLI, so neither can be
    /// refused this way — treating them as such would back off for nothing.
    #[test]
    fn only_the_providers_reached_over_the_network_can_be_refused() {
        let local = QuotaRow {
            label: "codex".into(),
            agent: Agent::Codex,
            account: "default".into(),
            detail: "no rate_limits snapshot, 429 lines scanned".into(),
            ..Default::default()
        };
        assert!(!rows_are_rate_limited(&[local]));
    }
}
