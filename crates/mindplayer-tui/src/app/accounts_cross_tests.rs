//! Working across accounts: telling sessions of different logins apart, and
//! carrying a conversation from one login to another.
//!
//! A session cannot be resumed on an account whose home never held it, so
//! these two are the whole answer to "my old sessions are still on the old
//! account" — see which is which, and hand one over when you want it moved.

use super::tests::session;
use super::*;
use mindplayer_core::accounts::{Account, MULTI_ACCOUNT_AGENTS};

fn app_with_two_codex() -> (App, Account, Account) {
    let mut app = App::new();
    let home = limits_home_for_app();
    let kept = Account::inherited(Agent::Codex);
    let second = Account::isolated(&home, Agent::Codex, "other").unwrap();
    app.accounts = vec![kept.clone(), second.clone()];
    (app, kept, second)
}

/// Marks in the session list.
mod which_account {
    use super::*;

    #[test]
    fn a_new_pane_shows_its_selected_account_before_a_transcript_exists() {
        let (mut app, _, second) = app_with_two_codex();
        app.request_new_on(&second, "isolated");
        let pending = app.extra_sessions.last().unwrap();
        assert!(pending.file.as_os_str().is_empty());
        let marks = app.account_marks().unwrap();
        assert_eq!(marks.label_for(pending), Some((second.name.as_str(), 1)));
    }

    #[test]
    fn nothing_is_marked_while_every_provider_has_one_login() {
        let mut app = App::new();
        app.accounts = MULTI_ACCOUNT_AGENTS.map(Account::inherited).to_vec();
        assert!(
            app.account_marks().is_none(),
            "a column that says the same thing on every row is noise"
        );
    }

    /// A blank meant "the account in use", which nothing on screen said, so every row of a provider with two logins names its own.
    #[test]
    fn every_row_of_a_provider_with_two_logins_names_its_account() {
        let (mut app, kept, second) = app_with_two_codex();
        app.accounts.push(Account::inherited(Agent::Claude));
        let home = limits_home_for_app();
        let marks = app.account_marks().expect("two accounts and no marks");

        let mut on_kept = session("a", Agent::Codex, false);
        on_kept.file = kept.session_root(&home).join("2026/09/15/a.jsonl");
        assert_eq!(marks.label_for(&on_kept), Some((kept.name.as_str(), 0)));

        let mut on_second = session("b", Agent::Codex, false);
        on_second.file = second.session_root(&home).join("2026/09/15/b.jsonl");
        assert_eq!(marks.label_for(&on_second), Some(("other", 1)));

        let lone = session("c", Agent::Claude, false);
        assert_eq!(
            marks.label_for(&lone),
            None,
            "a provider with one login has nothing to tell apart"
        );
        assert_eq!(marks.width(), kept.name.len().max("other".len()));
    }

    #[test]
    fn switching_the_account_in_use_does_not_rename_a_row() {
        let (mut app, kept, second) = app_with_two_codex();
        let home = limits_home_for_app();
        app.open_accounts();
        // Put the cursor on the second account and make it the one in use.
        app.accounts_panel.as_mut().unwrap().selected = 2;
        app.accounts_make_primary();

        let marks = app.account_marks().unwrap();
        let mut old = session("a", Agent::Codex, false);
        old.file = kept.session_root(&home).join("2026/09/15/a.jsonl");
        assert_eq!(marks.label_for(&old), Some((kept.name.as_str(), 0)));

        let mut new = session("b", Agent::Codex, false);
        new.file = second.session_root(&home).join("2026/09/15/b.jsonl");
        assert_eq!(marks.label_for(&new), Some((second.name.as_str(), 1)));
    }
}

/// Carrying a conversation to another login.
mod handing_over {
    use super::*;

    #[test]
    fn every_usable_login_is_a_target() {
        let (mut app, _, second) = app_with_two_codex();
        app.accounts.push(Account::inherited(Agent::Claude));
        app.accounts.push(Account::inherited(Agent::Kiro));
        app.accounts.push(Account::inherited(Agent::Cursor));

        let targets = app.handoff_targets();
        assert!(
            targets
                .iter()
                .any(|a| a.provider == Agent::Codex && a.name == second.name),
            "the second codex login cannot be handed to: {targets:?}"
        );
        for agent in MULTI_ACCOUNT_AGENTS {
            assert!(
                targets.iter().any(|a| a.provider == agent),
                "{} is not a handoff target: {targets:?}",
                agent.as_str()
            );
        }
    }

    #[test]
    fn the_primary_login_leads_the_handoff_targets() {
        let (mut app, kept, second) = app_with_two_codex();
        app.accounts.push(Account::inherited(Agent::Claude));

        let targets = app.handoff_targets();
        let codex: Vec<_> = targets
            .into_iter()
            .filter(|account| account.provider == Agent::Codex)
            .map(|account| account.name)
            .collect();

        assert_eq!(codex, vec![kept.name.clone(), second.name.clone()]);

        app.open_accounts();
        app.accounts_panel.as_mut().unwrap().selected = 2;
        app.accounts_make_primary();

        let targets = app.handoff_targets();
        let codex: Vec<_> = targets
            .into_iter()
            .filter(|account| account.provider == Agent::Codex)
            .map(|account| account.name)
            .collect();
        assert_eq!(codex, vec![second.name, kept.name]);
    }

    #[test]
    fn a_login_that_is_off_is_not_offered() {
        let (mut app, _, second) = app_with_two_codex();
        app.accounts[1].disabled = true;
        assert!(
            !app.handoff_targets().iter().any(|a| a.name == second.name),
            "an account that is off was offered as a target"
        );
    }

    #[test]
    fn a_session_can_be_handed_to_another_login_of_its_own_provider() {
        let (mut app, kept, second) = app_with_two_codex();
        let home = limits_home_for_app();
        let mut source = session("src", Agent::Codex, false);
        source.cwd = std::env::temp_dir();
        source.file = kept.session_root(&home).join("2026/09/15/src.jsonl");
        std::fs::create_dir_all(source.file.parent().unwrap()).unwrap();
        std::fs::write(
            &source.file,
            concat!(
                r#"{"timestamp":"2026-09-15T01:00:00Z","type":"session_meta","payload":{"id":"src","timestamp":"2026-09-15T01:00:00Z","cwd":"/tmp"}}"#,
                "\n",
                r#"{"timestamp":"2026-09-15T01:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"carry this over"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        app.all_sessions = vec![source.clone()];
        app.visible = vec![crate::app::Row::Session(0)];
        app.selected = 0;

        app.handoff_picker = Some(0);
        app.confirm_handoff_on(&second);

        let pending = app.pending.as_ref().unwrap_or_else(|| {
            panic!(
                "handing over to another login of the same provider was refused: {}",
                app.status
            )
        });
        assert_eq!(
            pending.command.env,
            second.launch_env(),
            "the new session started on the wrong login"
        );
        let _ = std::fs::remove_file(&source.file);
    }

    #[test]
    fn handing_a_session_to_the_login_it_is_already_on_is_refused() {
        let (mut app, kept, _) = app_with_two_codex();
        let home = limits_home_for_app();
        let mut source = session("src2", Agent::Codex, false);
        source.file = kept.session_root(&home).join("2026/09/15/src2.jsonl");
        app.all_sessions = vec![source];
        app.visible = vec![crate::app::Row::Session(0)];
        app.selected = 0;

        app.handoff_picker = Some(0);
        app.confirm_handoff_on(&kept);

        assert!(
            app.pending.is_none(),
            "a handoff to the session's own login started anyway"
        );
        assert!(
            app.status.contains("already on"),
            "the refusal did not say why: {}",
            app.status
        );
    }
}
