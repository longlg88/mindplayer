//! Opt-in, sanitized verification of the configured Codex profiles.
use mindplayer_core::{accounts::load_accounts, limits, session::Agent};
use std::path::PathBuf;

#[test]
#[ignore = "refreshes real Codex account state; prints only binding/equality booleans, never credentials or identity fields"]
fn live_codex_required_identity_checks() {
    let home = std::env::var_os("MINDPLAYER_LIVE_ACCOUNTS_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("HOME or MINDPLAYER_LIVE_ACCOUNTS_HOME");
    let mut fingerprints = Vec::new();
    for account in load_accounts(&home)
        .into_iter()
        .filter(|a| a.provider == Agent::Codex && !a.disabled)
    {
        let codex_home = account
            .launch_env()
            .set
            .into_iter()
            .find(|(key, _)| key == "CODEX_HOME")
            .map(|(_, path)| PathBuf::from(path))
            .expect("selected account home");
        let reading = limits::codex_account_in(&codex_home);
        assert!(
            reading.is_ok(),
            "{} required identity check failed: {:?}",
            account.name,
            reading.err().map(|error| error.kind)
        );
        let identity = reading.unwrap();
        println!("{}: identity_available=true stored_binding={} matches_binding={} same_as_previous_home={}",
            account.name, account.codex_identity.is_some(),
            account.codex_identity.as_deref() == Some(identity.fingerprint.as_str()),
            fingerprints.contains(&identity.fingerprint));
        fingerprints.push(identity.fingerprint);
    }
    assert!(!fingerprints.is_empty(), "no enabled Codex accounts");
}

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
    let measured: Vec<_> = rows
        .iter()
        .map(|row| (row.used_percent, row.resets.clone()))
        .collect();
    limits::mark_shared_logins(&mut rows);
    for (row, (used, resets)) in rows.iter().zip(measured) {
        assert_eq!(row.used_percent, used, "shared identity erased a gauge");
        assert_eq!(row.resets, resets, "shared identity erased a reset");
    }
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
