//! Two things that only go wrong once MindPlayer has been running a while:
//! the Kiro probe never finishing, and a rate-limited row showing the refusal
//! instead of the figure it had a moment ago.

use mindplayer_core::limits::{merge_with_last_good, QuotaRow};
use mindplayer_core::session::Agent;

fn row(agent: Agent, account: &str, label: &str, used: Option<f64>, detail: &str) -> QuotaRow {
    QuotaRow {
        label: label.into(),
        agent,
        account: account.into(),
        used_percent: used,
        detail: detail.into(),
        ..Default::default()
    }
}

/// Reported after hours of use: `claude — rate limited by the account API
/// (HTTP 429)`. The account's limit is spent by every Claude tool on the
/// machine, so a refusal is ordinary; showing it in place of the figure read
/// minutes earlier is not.
#[test]
fn a_refused_reading_shows_the_figure_it_had_before() {
    let fresh = vec![row(
        Agent::Claude,
        "default",
        "claude",
        None,
        "rate limited by the account API (HTTP 429)",
    )];
    let stored = vec![row(Agent::Claude, "default", "claude wk", Some(26.0), "")];

    let (rows, used_cache) = merge_with_last_good(&fresh, &stored);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].used_percent, Some(26.0), "{rows:?}");
    assert!(
        used_cache,
        "the footer has to say the figure is not current"
    );
}

/// The cache written before readings carried an account has no name on its
/// rows. A provider had exactly one login then — the one this machine came
/// with — so that is the one it speaks for, and refusing it outright left the
/// first refusal after an upgrade with nothing to fall back to.
#[test]
fn a_cache_from_before_accounts_speaks_for_the_login_this_machine_came_with() {
    let fresh = vec![row(Agent::Claude, "default", "claude", None, "429")];
    let stored = vec![row(Agent::Claude, "", "claude wk", Some(26.0), "")];

    let (rows, used_cache) = merge_with_last_good(&fresh, &stored);
    assert_eq!(rows[0].used_percent, Some(26.0), "{rows:?}");
    assert!(used_cache);
}

/// It speaks for that one only. A named second login is a different
/// subscription, and lending it another's figure is a wrong answer wearing the
/// right name.
#[test]
fn it_does_not_speak_for_a_named_login() {
    let fresh = vec![row(Agent::Kiro, "team", "kiro", None, "timed out")];
    let stored = vec![row(Agent::Kiro, "", "kiro", Some(52.0), "")];

    let (rows, used_cache) = merge_with_last_good(&fresh, &stored);
    assert_eq!(rows[0].used_percent, None, "{rows:?}");
    assert!(!used_cache);
}

/// Reported: `kiro — kiro-cli timed out`, permanently.
///
/// Kiro has no usage command outside `chat`, and starting a chat is the whole
/// cost. Measured on a developer machine: `kiro-cli chat --no-interactive
/// /help` took 29.1s and the same with `/usage` 31.1s, so the query itself is
/// about 2s and the rest is startup. A 15s budget could never finish.
#[test]
fn the_kiro_probe_is_given_longer_than_starting_a_chat_takes() {
    let budget = mindplayer_core::limits::kiro_usage_timeout();
    assert!(
        budget >= std::time::Duration::from_secs(45),
        "measured startup alone was ~29s and the whole probe ~31s; {budget:?} cannot finish"
    );
}
