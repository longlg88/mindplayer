//! Opt-in, sanitized verification of the configured Codex profiles.
use mindplayer_core::{accounts::load_accounts, limits, session::Agent};
use std::path::PathBuf;

#[test]
#[ignore = "reads real signed-in Codex accounts; no raw email, token, or workspace ID is printed"]
fn live_codex_account_rows() {
    let home = std::env::var_os("MINDPLAYER_LIVE_ACCOUNTS_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("HOME or MINDPLAYER_LIVE_ACCOUNTS_HOME");
    let accounts: Vec<_> = load_accounts(&home)
        .into_iter()
        .filter(|account| account.provider == Agent::Codex && !account.disabled)
        .collect();
    assert!(!accounts.is_empty(), "no enabled Codex accounts");
    let mut rows = Vec::new();
    for account in &accounts {
        let reading = limits::account_quota_rows(account, &home);
        assert!(
            reading.iter().all(|row| row.failure.is_none()),
            "probe failed for {}",
            account.name
        );
        rows.extend(reading);
    }
    limits::mark_shared_logins(&mut rows);
    for row in rows {
        println!(
            "{}: {} used={:?} detail={:?} identity_known={} blocked={} spend_cap={}",
            row.account,
            row.label,
            row.used_percent,
            row.detail,
            row.identity.is_some(),
            row.blocked,
            row.spend_cap
        );
    }
}
