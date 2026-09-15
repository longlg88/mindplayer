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
    assert!(
        app.probe_accounts()
            .iter()
            .any(|a| a.provider == Agent::Cursor),
        "cursor holds one login but still has a reading"
    );

    app.accounts[1].disabled = true;
    assert!(
        !app.probe_accounts().iter().any(|a| a.name == second.name),
        "an account that is off still spent a request"
    );
}

/// The footer has room for one line per provider, and the question it answers
/// is how much is left where the next session goes.
#[test]
fn the_footer_shows_only_the_login_a_new_session_would_use() {
    let (mut app, second) = app_with_two_codex();
    app.limits = Some(vec![
        row(Agent::Codex, "default", "codex", 10.0),
        row(Agent::Codex, &second.name, "codex", 90.0),
    ]);

    let footer = app.quota_rows();
    assert_eq!(footer.len(), 1, "{footer:?}");
    assert_eq!(footer[0].used_percent, Some(10.0));

    let everything = app.quota_rows_all();
    assert_eq!(everything.len(), 2, "the Accounts screen needs them all");
}

#[test]
fn switching_the_login_switches_which_reading_the_footer_shows() {
    let (mut app, second) = app_with_two_codex();
    app.limits = Some(vec![
        row(Agent::Codex, "default", "codex", 10.0),
        row(Agent::Codex, &second.name, "codex", 90.0),
    ]);
    app.open_accounts();
    app.accounts_panel.as_mut().unwrap().selected = 2;
    app.accounts_make_primary();

    let footer = app.quota_rows();
    assert_eq!(
        footer.first().and_then(|r| r.used_percent),
        Some(90.0),
        "the footer kept showing the account no longer in use: {footer:?}"
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
