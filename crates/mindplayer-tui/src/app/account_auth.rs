//! Provider-neutral account verification, kept off the input/render thread. A blocked
//! launch remains owned here until explicitly recovered or its pane is closed.
use super::*;
use mindplayer_core::accounts::Account;
use mindplayer_core::limits::{AccountIdentity, FailureKind, ProbeError};

type Reading = Result<AccountIdentity, ProbeError>;

#[derive(Default)]
pub(super) struct AccountAuth {
    checks: Vec<Check>,
    blocked: HashMap<String, (Account, PendingSpawn)>,
    approved: HashMap<String, Account>,
    logins: HashMap<String, Account>,
}

struct Check {
    account: Account,
    purpose: Purpose,
    cwd: PathBuf,
    result: Receiver<Reading>,
}

enum Purpose {
    Inspect,
    AfterLogin,
    Bind(String, PathBuf),
    Launch(PendingSpawn),
}

impl AccountAuth {
    pub(super) fn owns_launch(&self, id: &str) -> bool {
        self.blocked.contains_key(id) || self.approved.contains_key(id) || self.checks.iter().any(|check|
            matches!(&check.purpose, Purpose::Launch(pending) if pending.session_id == id)
        )
    }
    pub(super) fn cancel_inspection(&mut self) {
        self.checks
            .retain(|check| matches!(check.purpose, Purpose::Launch(_) | Purpose::AfterLogin));
    }
    pub(super) fn cancel(&mut self, id: &str) {
        self.blocked.remove(id);
        self.approved.remove(id);
        self.logins.remove(id);
        self.checks
            .retain(|check| !matches!(&check.purpose, Purpose::Launch(p) if p.session_id == id));
    }
}

fn identity_home_key(provider: Agent) -> Option<&'static str> {
    match provider {
        Agent::Codex => Some("CODEX_HOME"),
        Agent::Claude => Some("CLAUDE_CONFIG_DIR"),
        _ => None,
    }
}

#[cfg(not(test))]
fn account_home(account: &Account) -> Option<PathBuf> {
    let key = identity_home_key(account.provider)?;
    account
        .launch_env()
        .set
        .into_iter()
        .find(|(name, _)| name == key)
        .map(|(_, home)| home.into())
}

fn same_home(one: &Account, other: &Account) -> bool {
    one.provider == other.provider
        && identity_home_key(one.provider).is_some()
        && one.launch_env() == other.launch_env()
}

fn command_matches_account(command: &mindplayer_core::Command, account: &Account) -> bool {
    if identity_home_key(account.provider).is_none()
        || command.env.set.iter().any(|(key, _)| key == "HOME")
        || command.env.unset.iter().any(|key| key == "HOME")
    {
        return false;
    }
    let expected = account.launch_env();
    expected.set.iter().all(|(key, value)| {
        command
            .env
            .set
            .iter()
            .filter(|(name, _)| name == key)
            .count()
            == 1
            && command
                .env
                .set
                .iter()
                .any(|(name, actual)| name == key && actual == value)
            && !command.env.unset.contains(key)
    }) && expected.unset.iter().all(|key| {
        command.env.unset.contains(key) && !command.env.set.iter().any(|(name, _)| name == key)
    })
}

impl App {
    pub fn account_auth_message(&self, id: &str) -> Option<&'static str> {
        if self.account_auth.blocked.contains_key(id) {
            Some("로그인/계정 확인 필요 — Accounts(u)에서 이 계정의 v 또는 l")
        } else if self.account_auth.owns_launch(id) {
            Some("로그인 확인 중… 선택한 계정 홈의 사용자와 조직을 확인합니다.")
        } else {
            None
        }
    }
    #[cfg(test)]
    pub(super) fn complete_account_checks_for_test(&mut self, reading: Reading) {
        for check in &mut self.account_auth.checks {
            let (tx, rx) = mpsc::channel();
            tx.send(reading.clone()).unwrap();
            check.result = rx;
        }
        self.poll_account_auth();
    }

    pub(super) fn account_recovery_cwd(&self, account: &Account) -> PathBuf {
        if let Some((_, pending)) = self
            .focused_pane()
            .and_then(|id| self.account_auth.blocked.get(id))
            .filter(|(owner, _)| owner == account)
        {
            return pending.command.cwd.clone();
        }
        self.account_auth
            .blocked
            .iter()
            .filter(|(_, (owner, _))| owner == account)
            .min_by_key(|(id, _)| *id)
            .map(|(_, (_, pending))| pending.command.cwd.clone())
            .unwrap_or_else(|| match &self.scope {
                Scope::WorkingDir(dir) => dir.clone(),
                Scope::Global => self.cwd.clone(),
            })
    }

    fn start_account_check(&mut self, account: Account, purpose: Purpose) {
        if self
            .account_auth
            .logins
            .values()
            .any(|login| same_home(login, &account))
        {
            if let Purpose::Launch(pending) = purpose {
                self.hold_account_launch(&account, pending);
            }
            self.status = format!("{} 로그인 진행 중 — 완료 후 다시 확인합니다.", account.name);
            return;
        }
        let (tx, rx) = mpsc::channel();
        #[cfg(not(test))]
        let probe_account = account.clone();
        let probe_cwd = match &purpose {
            Purpose::Launch(pending) => pending.command.cwd.clone(),
            Purpose::Bind(_, cwd) => cwd.clone(),
            _ => self.account_recovery_cwd(&account),
        };
        let cwd = probe_cwd.clone();
        #[cfg(not(test))]
        thread::spawn(move || {
            let reading = match probe_account.provider {
                Agent::Codex => account_home(&probe_account)
                    .ok_or_else(|| {
                        ProbeError::new(
                            FailureKind::Unreadable,
                            "selected account has no resolvable credential home",
                        )
                    })
                    .and_then(|home| mindplayer_core::limits::codex_account_in(&home)),
                Agent::Claude => {
                    mindplayer_core::claude_auth::claude_account_for(&probe_account, &probe_cwd)
                }
                _ => Err(ProbeError::new(
                    FailureKind::Unreadable,
                    "unsupported identity provider",
                )),
            };
            let _ = tx.send(reading);
        });
        // Unit tests inject results explicitly; never read the developer's auth.
        #[cfg(test)]
        drop(tx);
        self.account_auth.checks.push(Check {
            account,
            purpose,
            cwd,
            result: rx,
        });
    }

    pub(super) fn inspect_account_identity(
        &mut self,
        account: &Account,
        bind: Option<(String, PathBuf)>,
    ) {
        if self.account_auth.checks.iter().any(|check| {
            check.account == *account
                && matches!(check.purpose, Purpose::Inspect | Purpose::Bind(_, _))
        }) {
            return;
        }
        // Another keypress supersedes the previous inspection/confirmation.
        self.account_auth.cancel_inspection();
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.confirm_identity = None;
            panel.error = Some(format!("{} 로그인 확인 중…", account.name));
        }
        self.start_account_check(
            account.clone(),
            bind.map_or(Purpose::Inspect, |(fingerprint, cwd)| {
                Purpose::Bind(fingerprint, cwd)
            }),
        );
    }

    /// Consume a one-use approval or hold this exact command for verification.
    pub(super) fn verify_account_launch(
        &mut self,
        account: &Account,
        pending: PendingSpawn,
    ) -> Option<PendingSpawn> {
        if self
            .account_auth
            .logins
            .values()
            .any(|login| same_home(login, account))
        {
            self.hold_account_launch(account, pending);
            return None;
        }
        if !command_matches_account(&pending.command, account) {
            self.status = format!(
                "{}: 실행 홈이 선택한 계정 홈과 다릅니다. 실행하지 않았습니다.",
                account.name
            );
            self.pane_error
                .insert(pending.session_id, self.status.clone());
            return None;
        }
        if self
            .account_auth
            .approved
            .remove(&pending.session_id)
            .as_ref()
            == Some(account)
        {
            return Some(pending);
        }
        self.account_auth.cancel(&pending.session_id);
        self.status = format!("{} 로그인 확인 중…", account.name);
        self.start_account_check(account.clone(), Purpose::Launch(pending));
        None
    }

    pub(super) fn hold_account_launch(&mut self, account: &Account, pending: PendingSpawn) {
        self.account_auth
            .blocked
            .insert(pending.session_id.clone(), (account.clone(), pending));
    }

    pub(super) fn track_account_login(&mut self, account: &Account, id: &str) {
        if identity_home_key(account.provider).is_some() {
            self.account_auth.cancel_inspection();
            // Starting login invalidates earlier authorization for this home,
            // including completed approvals waiting in the normal spawn queue.
            let checks = std::mem::take(&mut self.account_auth.checks);
            for check in checks {
                if same_home(&check.account, account) {
                    if let Purpose::Launch(pending) = check.purpose {
                        self.hold_account_launch(&check.account, pending);
                    }
                } else {
                    self.account_auth.checks.push(check);
                }
            }
            self.account_auth
                .approved
                .retain(|_, approved| !same_home(approved, account));
            let mut spawns = std::mem::take(&mut self.pending_queue);
            if let Some(pending) = self.pending.take() {
                spawns.push_front(pending);
            }
            for pending in spawns {
                if !pending.session_id.starts_with(accounts_panel::LOGIN_PREFIX)
                    && command_matches_account(&pending.command, account)
                {
                    self.hold_account_launch(account, pending);
                } else {
                    self.enqueue_spawn(pending);
                }
            }
            self.account_auth.logins.insert(id.into(), account.clone());
        }
    }

    pub(super) fn account_login_finished(&mut self, id: &str, succeeded: bool) {
        let Some(account) = self.account_auth.logins.remove(id) else {
            return;
        };
        if !succeeded {
            self.status = format!("{} 로그인 취소/실패 — 세션을 실행하지 않았습니다. Accounts(u)의 l로 다시 로그인하세요.", account.name);
            return;
        }
        self.limits_force_refresh = true;
        if self
            .account_auth
            .blocked
            .values()
            .any(|(owner, _)| owner == &account)
        {
            self.retry_account_launches(&account);
        } else {
            self.start_account_check(account, Purpose::AfterLogin);
        }
    }

    pub fn poll_account_auth(&mut self) -> bool {
        let checks = std::mem::take(&mut self.account_auth.checks);
        let mut changed = false;
        for check in checks {
            let reading = match check.result.try_recv() {
                Ok(reading) => reading,
                Err(mpsc::TryRecvError::Empty) => {
                    self.account_auth.checks.push(check);
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => Err(ProbeError::new(
                    FailureKind::Temporary,
                    "account verification stopped",
                )),
            };
            changed = true;
            // Never apply a late result by index. A changed binding needs a
            // new check, not loss of another launch's command or handoff input.
            let current = self
                .accounts
                .iter()
                .find(|account| {
                    account.provider == check.account.provider
                        && account.name == check.account.name
                        && account.slot == check.account.slot
                })
                .cloned();
            let Some(current) = current.filter(|account| !account.disabled) else {
                self.status = format!(
                    "{} 계정 설정이 바뀌어 실행을 중단했습니다.",
                    check.account.name
                );
                continue;
            };
            if current != check.account {
                match check.purpose {
                    Purpose::Launch(pending) => {
                        self.start_account_check(current, Purpose::Launch(pending))
                    }
                    Purpose::AfterLogin => self.start_account_check(current, Purpose::AfterLogin),
                    _ => {}
                }
                continue;
            }
            self.finish_account_check(check.account, check.purpose, reading, check.cwd);
        }
        changed
    }

    fn show_account_identity(
        &mut self,
        account: &Account,
        message: String,
        identity: Option<&AccountIdentity>,
        cwd: PathBuf,
    ) {
        // A late result must not redirect the user's next l/v to another
        // account. Only present in its own panel or its own foreground pane.
        if let Some(panel) = &self.accounts_panel {
            let selected = self
                .selected_account()
                .and_then(|index| self.accounts.get(index));
            if selected != Some(account) || panel.naming.is_some() {
                self.status = format!("{} — u에서 {} 선택 후 v/l", message, account.name);
                return;
            }
        } else {
            let owns_foreground = self.active.as_ref().is_some_and(|id| {
                self.all_sessions
                    .iter()
                    .find(|session| session.id == *id)
                    .map(|session| self.account_of_session(session))
                    .is_some_and(|owner| {
                        owner.provider == account.provider
                            && owner.name == account.name
                            && owner.slot == account.slot
                    })
            });
            if !owns_foreground || self.keyboard_taken() {
                self.status = format!("{} — u에서 {} 선택 후 v/l", message, account.name);
                return;
            }
            self.open_accounts();
        }
        let Some(index) = self.accounts.iter().position(|a| a == account) else {
            return;
        };
        let row = self
            .account_rows()
            .iter()
            .position(|r| *r == accounts_panel::AccountRow::Entry(index))
            .unwrap_or(1);
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.selected = row;
            panel.error = Some(message.clone());
            panel.confirm_identity =
                identity.map(|identity| (account.clone(), identity.fingerprint.clone(), cwd));
        }
        self.status = message;
    }

    fn retry_account_launches(&mut self, account: &Account) {
        let ids: Vec<_> = self
            .account_auth
            .blocked
            .iter()
            .filter(|(_, (a, _))| {
                a.provider == account.provider && a.name == account.name && a.slot == account.slot
            })
            .map(|(id, _)| id.clone())
            .collect();
        if !ids.is_empty() && self.owns_account_panel(account) {
            self.close_accounts();
        }
        for id in ids {
            if let Some((_, mut pending)) = self.account_auth.blocked.remove(&id) {
                if !self.account_continuation_may_focus(account, &id) {
                    pending.focus_after_spawn = false;
                }
                self.start_account_check(account.clone(), Purpose::Launch(pending));
            }
        }
    }

    fn owns_account_panel(&self, account: &Account) -> bool {
        self.accounts_panel
            .as_ref()
            .is_some_and(|panel| panel.naming.is_none())
            && self
                .selected_account()
                .and_then(|index| self.accounts.get(index))
                == Some(account)
    }

    fn account_continuation_may_focus(&self, account: &Account, id: &str) -> bool {
        if self.owns_account_panel(account) {
            return true;
        }
        if self.keyboard_taken() || self.focus != Focus::Terminal {
            return false;
        }
        self.active.as_deref() == Some(id)
            || self.active.as_ref().is_some_and(|active| {
                active.starts_with(&format!("login:{}:", account.provider.as_str()))
                    && self.pane_accounts.get(active).is_some_and(|(agent, name)| {
                        *agent == account.provider && *name == account.name
                    })
            })
    }

    fn finish_account_check(
        &mut self,
        account: Account,
        purpose: Purpose,
        reading: Reading,
        cwd: PathBuf,
    ) {
        match purpose {
            Purpose::Launch(mut pending) => {
                if let Ok(identity) = &reading {
                    if account.identity_fingerprint() == Some(&identity.fingerprint) {
                        if !self.account_continuation_may_focus(&account, &pending.session_id) {
                            pending.focus_after_spawn = false;
                        }
                        self.account_auth
                            .approved
                            .insert(pending.session_id.clone(), account);
                        self.enqueue_spawn(pending);
                        return;
                    }
                }
                self.hold_account_launch(&account, pending);
                self.present_account_reading(&account, reading, cwd);
            }
            Purpose::Inspect | Purpose::AfterLogin => {
                if let Ok(identity) = &reading {
                    if account.identity_fingerprint() == Some(&identity.fingerprint) {
                        self.status =
                            format!("{} 로그인 확인 완료 ({})", account.name, identity.email);
                        if self.owns_account_panel(&account) {
                            if let Some(panel) = self.accounts_panel.as_mut() {
                                panel.error = None;
                            }
                        }
                        self.retry_account_launches(&account);
                        return;
                    }
                }
                self.present_account_reading(&account, reading, cwd);
            }
            Purpose::Bind(expected, _) => {
                if let Ok(identity) = &reading {
                    if identity.fingerprint == expected {
                        let Some(index) = self.accounts.iter().position(|a| a == &account) else {
                            return;
                        };
                        self.accounts[index].set_identity_fingerprint(Some(expected));
                        if let Err(error) = mindplayer_core::accounts::save_accounts(
                            &limits_home_for_app(),
                            &self.accounts,
                        ) {
                            self.accounts[index] = account.clone();
                            self.show_account_identity(
                                &account,
                                format!("계정 저장 실패: {error}"),
                                None,
                                cwd,
                            );
                            return;
                        }
                        let bound = self.accounts[index].clone();
                        self.status =
                            format!("{} identity bound ({})", account.name, identity.email);
                        if self.owns_account_panel(&bound) {
                            if let Some(panel) = self.accounts_panel.as_mut() {
                                panel.error = None;
                                panel.confirm_identity = None;
                            }
                        }
                        self.retry_account_launches(&bound);
                        return;
                    }
                }
                self.present_account_reading(&account, reading, cwd);
            }
        }
    }

    fn present_account_reading(&mut self, account: &Account, reading: Reading, cwd: PathBuf) {
        match reading {
            Ok(identity) => {
                let warning = if account.identity_fingerprint().is_some() {
                    "저장된 로그인과 다름"
                } else {
                    "미등록 로그인"
                };
                self.show_account_identity(
                    account,
                    format!(
                        "{}: {} · {} / {} — 이 계정이 맞으면 v로 연결, 아니면 l로 로그인",
                        account.name, warning, identity.email, identity.workspace
                    ),
                    Some(&identity),
                    cwd,
                );
            }
            Err(error) => self.show_account_identity(
                account,
                format!(
                    "{}: {} — {}",
                    account.name,
                    error,
                    if error.kind == FailureKind::SignIn {
                        "l로 이 홈에 로그인 (기본 홈은 다른 앱과 공유)"
                    } else {
                        "v로 다시 확인; 로그인 변경은 l"
                    }
                ),
                None,
                cwd,
            ),
        }
    }
}

#[cfg(test)]
macro_rules! lifecycle_tests {
    ($module:ident, $provider:expr) => {
        mod $module {
            use super::*;

            fn provider() -> Agent {
                $provider
            }
            fn login_id() -> String {
                format!("login:{}:default", provider().as_str())
            }

            fn account() -> Account {
                let mut account = Account::inherited(provider());
                account.set_identity_fingerprint(Some("expected".into()));
                account
            }

            fn identity(fingerprint: &str) -> Reading {
                Ok(AccountIdentity {
                    fingerprint: fingerprint.into(),
                    email: "person@example.test".into(),
                    workspace: "workspace-test".into(),
                })
            }

            fn pending(account: &Account, id: &str) -> PendingSpawn {
                PendingSpawn {
                    command: mindplayer_core::new_session(
                        provider(),
                        std::env::temp_dir(),
                        account,
                    ),
                    session_id: id.into(),
                    initial_input: Some(b"handoff context\r".to_vec()),
                    focus_after_spawn: false,
                }
            }

            fn app(account: &Account) -> App {
                let mut app = super::super::tests::isolated_app();
                app.accounts = vec![account.clone()];
                app.open_accounts();
                app.accounts_panel.as_mut().unwrap().selected = app
                    .account_rows()
                    .iter()
                    .position(|row| matches!(row, accounts_panel::AccountRow::Entry(0)))
                    .unwrap();
                app
            }

            #[test]
            fn matching_identity_releases_exact_launch_once() {
                let account = account();
                let mut app = app(&account);
                assert!(app
                    .verify_account_launch(&account, pending(&account, "resume"))
                    .is_none());
                assert!(app.ptys.is_empty());
                app.complete_account_checks_for_test(identity("expected"));
                let ready = app.pending.take().unwrap();
                assert_eq!(ready.session_id, "resume");
                assert_eq!(
                    ready.initial_input.as_deref(),
                    Some(b"handoff context\r".as_slice())
                );
                assert!(!ready.focus_after_spawn);
                let verified = app.verify_account_launch(&account, ready).unwrap();
                assert!(
                    app.verify_account_launch(&account, verified).is_none(),
                    "approval must be one-use"
                );
            }

            #[test]
            fn signed_out_login_success_rechecks_then_continues_without_losing_input() {
                let account = account();
                let mut app = app(&account);
                app.verify_account_launch(&account, pending(&account, "resume"));
                app.complete_account_checks_for_test(Err(ProbeError::new(
                    FailureKind::SignIn,
                    "authentication required",
                )));
                assert!(app.pending.is_none());
                assert!(app
                    .accounts_panel
                    .as_ref()
                    .unwrap()
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("l로"));
                app.track_account_login(&account, &login_id());
                app.account_login_finished(&login_id(), true);
                assert!(
                    app.pending.is_none(),
                    "must recheck the launch, not trust login exit"
                );
                app.complete_account_checks_for_test(identity("expected"));
                assert_eq!(
                    app.pending.as_ref().unwrap().initial_input.as_deref(),
                    Some(b"handoff context\r".as_slice())
                );
                assert!(app.account_auth.blocked.is_empty());
            }

            #[test]
            fn mismatch_does_not_automatically_rebind_or_launch_and_confirmation_rechecks() {
                let account = account();
                let mut app = app(&account);
                app.verify_account_launch(&account, pending(&account, "resume"));
                app.complete_account_checks_for_test(identity("other"));
                assert_eq!(
                    app.accounts[0].identity_fingerprint(),
                    account.identity_fingerprint()
                );
                assert!(app.pending.is_none());
                assert!(app.status.contains("person@example.test"));
                app.accounts_verify_identity();
                app.complete_account_checks_for_test(identity("changed-again"));
                assert_eq!(
                    app.accounts[0].identity_fingerprint(),
                    account.identity_fingerprint()
                );
                assert!(app.pending.is_none());
                assert_eq!(
                    app.accounts_panel
                        .as_ref()
                        .unwrap()
                        .confirm_identity
                        .as_ref()
                        .unwrap()
                        .1,
                    "changed-again"
                );
            }

            #[test]
            fn failed_or_cancelled_login_never_starts_a_session() {
                let account = account();
                let mut app = app(&account);
                app.hold_account_launch(&account, pending(&account, "resume"));
                app.track_account_login(&account, &login_id());
                app.account_login_finished(&login_id(), false);
                assert!(app.account_auth.checks.is_empty());
                assert!(app.pending.is_none());
                assert_eq!(app.account_auth.blocked.len(), 1);
            }

            #[test]
            fn recovery_prefers_the_focused_held_project_for_the_selected_account() {
                let account = account();
                let mut app = app(&account);
                for (id, cwd) in [("a-held", "/project-a"), ("b-held", "/project-b")] {
                    let mut launch = pending(&account, id);
                    launch.command.cwd = PathBuf::from(cwd);
                    app.hold_account_launch(&account, launch);
                }
                app.focus_or_add_pane("b-held");
                app.scope = Scope::WorkingDir(PathBuf::from("/unrelated-scope"));
                app.accounts_verify_identity();
                assert_eq!(app.account_auth.checks[0].cwd, PathBuf::from("/project-b"));
                app.request_login(&account);
                assert_eq!(app.pending.as_ref().unwrap().command.cwd, PathBuf::from("/project-b"));
            }

            #[test]
            fn native_recovery_login_uses_the_held_project_not_the_list_scope() {
                let account = account();
                let mut app = app(&account);
                let mut launch = pending(&account, "project-b-resume");
                launch.command.cwd = PathBuf::from("/project-b");
                app.hold_account_launch(&account, launch);
                app.scope = Scope::WorkingDir(PathBuf::from("/project-a"));
                app.request_login(&account);
                assert_eq!(app.pending.as_ref().unwrap().command.cwd, PathBuf::from("/project-b"));
            }

            #[test]
            fn login_recovery_checks_each_held_launch_in_its_original_project() {
                let account = account();
                let mut app = app(&account);
                let mut launch = pending(&account, "project-b-resume");
                launch.command.cwd = PathBuf::from("/project-b");
                app.hold_account_launch(&account, launch);
                app.track_account_login(&account, &login_id());
                app.scope = Scope::WorkingDir(PathBuf::from("/project-a"));
                app.account_login_finished(&login_id(), true);
                assert!(app.account_auth.checks.iter().any(|check| {
                    matches!(&check.purpose, Purpose::Launch(launch) if launch.command.cwd == std::path::Path::new("/project-b"))
                }), "recovery must not gate project B behind an unrelated scope A check");
            }

            #[test]
            fn mismatch_confirmation_keeps_the_originating_project_after_scope_change() {
                let account = account();
                let mut app = app(&account);
                let mut launch = pending(&account, "project-b-resume");
                launch.command.cwd = PathBuf::from("/project-b");
                app.verify_account_launch(&account, launch);
                app.complete_account_checks_for_test(identity("other"));
                assert_eq!(app.accounts_panel.as_ref().unwrap().confirm_identity.as_ref().unwrap().2, PathBuf::from("/project-b"));
                app.scope = Scope::WorkingDir(PathBuf::from("/project-a"));
                app.accounts_verify_identity();
                assert_eq!(app.account_auth.checks[0].cwd, PathBuf::from("/project-b"));
                app.complete_account_checks_for_test(identity("other"));
                assert!(app.account_auth.checks.iter().any(|check| {
                    matches!(&check.purpose, Purpose::Launch(_)) && check.cwd == std::path::Path::new("/project-b")
                }));
                assert!(app.pending.is_none());
            }

            #[test]
            fn arbitrary_account_names_and_renames_do_not_change_identity_binding() {
                let mut account = account();
                account.name = "research_lab_42".into();
                let mut app = app(&account);
                app.verify_account_launch(&account, pending(&account, "named-resume"));
                app.complete_account_checks_for_test(identity("expected"));
                assert_eq!(app.pending.as_ref().unwrap().session_id, "named-resume");
                assert_eq!(app.accounts[0].identity_fingerprint(), Some("expected"));
            }

            #[test]
            fn logging_into_another_home_does_not_authorize_this_home() {
                let account = account();
                let mut app = app(&account);
                let mut other =
                    Account::isolated(&std::env::temp_dir(), provider(), "second-login").unwrap();
                other.set_identity_fingerprint(Some("expected".into()));
                app.accounts.push(other.clone());
                app.hold_account_launch(&account, pending(&account, "waiting-original"));
                app.track_account_login(&other, "login:other-home");
                app.account_login_finished("login:other-home", true);
                app.complete_account_checks_for_test(identity("expected"));
                assert!(app.pending.is_none());
                assert!(app.account_auth.blocked.contains_key("waiting-original"));
            }

            #[test]
            fn closing_pane_or_changing_home_discards_late_verification() {
                let account = account();
                let mut app = app(&account);
                app.verify_account_launch(&account, pending(&account, "closed"));
                app.remove_pane("closed");
                app.complete_account_checks_for_test(identity("expected"));
                assert!(app.pending.is_none());
                app.verify_account_launch(&account, pending(&account, "changed"));
                app.accounts[0].name = "renamed".into();
                app.complete_account_checks_for_test(identity("expected"));
                assert!(app.pending.is_none());
            }

            #[test]
            fn wrong_command_home_is_never_authorized() {
                let account = account();
                let mut app = app(&account);
                let mut launch = pending(&account, "wrong");
                launch.command.env.set.clear();
                launch.command.env.unset.clear();
                assert!(app.verify_account_launch(&account, launch).is_none());
                assert!(app.account_auth.checks.is_empty());
                assert!(app.status.contains("실행 홈"));
            }

            #[test]
            fn launch_must_preserve_credential_clears_and_native_home() {
                let account = account();
                let mut app = app(&account);
                for missing in &account.launch_env().unset {
                    let mut launch = pending(&account, "missing-clear");
                    launch.command.env.unset.retain(|key| key != missing);
                    assert!(app.verify_account_launch(&account, launch).is_none());
                    assert!(app.account_auth.checks.is_empty(), "missing {missing}");
                }
                let mut launch = pending(&account, "wrong-native-home");
                launch.command.env.set.push(("HOME".into(), "/different/home".into()));
                assert!(app.verify_account_launch(&account, launch).is_none());
                assert!(app.account_auth.checks.is_empty());
            }

            #[test]
            fn multiple_account_checks_preserve_the_spawn_queue_and_individual_inputs() {
                let account = account();
                let mut app = app(&account);
                for id in ["one", "two", "three"] {
                    let mut launch = pending(&account, id);
                    launch.initial_input = Some(id.as_bytes().to_vec());
                    app.verify_account_launch(&account, launch);
                }
                app.complete_account_checks_for_test(identity("expected"));
                for id in ["one", "two", "three"] {
                    let launch = app.pending.take().unwrap();
                    assert_eq!(launch.session_id, id);
                    assert_eq!(launch.initial_input.as_deref(), Some(id.as_bytes()));
                    assert!(app.verify_account_launch(&account, launch).is_some());
                    app.pending = app.pending_queue.pop_front();
                }
                assert!(app.pending.is_none());
            }

            #[test]
            fn fresh_login_preserves_the_requested_session_label_and_command() {
                let mut app = app(&account());
                app.request_new_fresh_login(provider(), "original label");
                assert!(app
                    .pending
                    .as_ref()
                    .unwrap()
                    .session_id
                    .starts_with(&format!("login:{}:", provider().as_str())));
                let (_, (account, launch)) = app.account_auth.blocked.iter().next().unwrap();
                assert_eq!(launch.command.env, account.launch_env());
                assert!(launch
                    .session_id
                    .starts_with(&format!("new:{}:", provider().as_str())));
                assert!(app
                    .all_sessions
                    .iter()
                    .any(|s| s.id == launch.session_id && s.title.contains("original label")));
            }

            #[test]
            fn starting_login_invalidates_inflight_and_ready_authorizations() {
                let account = account();
                let mut app = app(&account);
                app.verify_account_launch(&account, pending(&account, "inflight"));
                app.account_auth
                    .approved
                    .insert("ready".into(), account.clone());
                app.pending = Some(pending(&account, "ready"));
                app.track_account_login(&account, &login_id());
                app.complete_account_checks_for_test(identity("expected"));
                assert!(
                    app.pending.is_none(),
                    "pre-login ready spawn was still authorized"
                );
                assert!(app.account_auth.approved.is_empty());
                assert_eq!(app.account_auth.blocked.len(), 2);
                app.account_login_finished(&login_id(), false);
                assert!(app.account_auth.checks.is_empty());
            }

            #[test]
            fn dismissing_accounts_does_not_cancel_successful_login_recovery() {
                let account = account();
                let mut app = app(&account);
                app.hold_account_launch(&account, pending(&account, "resume"));
                app.track_account_login(&account, &login_id());
                app.account_login_finished(&login_id(), true);
                app.open_accounts();
                app.close_accounts();
                app.complete_account_checks_for_test(identity("expected"));
                app.complete_account_checks_for_test(identity("expected"));
                assert_eq!(
                    app.pending
                        .as_ref()
                        .map(|pending| pending.session_id.as_str()),
                    Some("resume")
                );
            }

            #[test]
            fn binding_does_not_discard_another_inflight_launch() {
                let account = account();
                let mut app = app(&account);
                app.hold_account_launch(&account, pending(&account, "first"));
                app.verify_account_launch(&account, pending(&account, "slower"));
                app.finish_account_check(account, Purpose::Bind("other".into(), std::env::temp_dir()), identity("other"), std::env::temp_dir());
                app.complete_account_checks_for_test(identity("other"));
                app.complete_account_checks_for_test(identity("other"));
                let mut ids: Vec<_> = app
                    .pending
                    .iter()
                    .chain(app.pending_queue.iter())
                    .map(|pending| pending.session_id.as_str())
                    .collect();
                ids.sort();
                assert_eq!(ids, ["first", "slower"]);
            }

            #[test]
            fn late_verification_does_not_redirect_another_account_panel() {
                let account = account();
                let mut app = app(&account);
                let other =
                    Account::isolated(&std::env::temp_dir(), provider(), "other-panel").unwrap();
                app.accounts.push(other.clone());
                app.accounts_move(1);
                app.finish_account_check(
                    account,
                    Purpose::Launch(pending(&other, "background")),
                    identity("mismatch"),
                    std::env::temp_dir(),
                );
                assert_eq!(
                    app.selected_account().map(|i| &app.accounts[i]),
                    Some(&other)
                );
                assert!(app
                    .accounts_panel
                    .as_ref()
                    .unwrap()
                    .confirm_identity
                    .is_none());
            }

            #[test]
            fn waiting_for_auth_never_adopts_another_processes_rollout() {
                let account = account();
                let mut app = app(&account);
                app.request_new_on(&account, "waiting for auth");
                let pending = app.pending.take().unwrap();
                let synthetic_id = pending.session_id.clone();
                let mut other =
                    super::super::tests::session("unrelated-rollout", provider(), false);
                other.started_at = Some(Utc::now());
                other.cwd = pending.command.cwd.clone();
                app.verify_account_launch(&account, pending);
                app.all_sessions.push(other);
                app.merge_extras();
                assert!(app.extra_sessions.iter().any(|s| s.id == synthetic_id));
                assert_eq!(app.active.as_deref(), Some(synthetic_id.as_str()));
            }

            #[test]
            fn synchronization_arriving_during_auth_survives_the_verified_spawn() {
                let account = account();
                let mut app = app(&account);
                app.all_sessions
                    .push(super::super::tests::session("resume", provider(), false));
                let mut launch = pending(&account, "resume");
                launch.command.program = "sh".into();
                launch.command.args = vec!["-c".into(), "cat".into()];
                app.queue_initial_input("resume".into(), b"peer sync\r".to_vec());
                app.account_auth.approved.insert("resume".into(), account);
                app.pending = Some(launch);
                app.spawn_pending();
                assert!(app.ptys.contains_key("resume"), "{}", app.status);
                assert_eq!(
                    app.pending_initial_inputs["resume"].bytes,
                    b"handoff context\n\n---\n\npeer sync\r"
                );
            }

            #[test]
            fn verification_during_login_cannot_release_work_after_cancellation() {
                let account = account();
                let mut app = app(&account);
                app.hold_account_launch(&account, pending(&account, "resume"));
                app.track_account_login(&account, &login_id());
                app.accounts_verify_identity();
                app.account_login_finished(&login_id(), false);
                app.complete_account_checks_for_test(identity("expected"));
                app.complete_account_checks_for_test(identity("expected"));
                assert!(app.pending.is_none());
                assert!(app.account_auth.approved.is_empty());
                assert_eq!(app.account_auth.blocked.len(), 1);
            }

            #[test]
            fn successful_background_recovery_preserves_other_account_edit_and_focus() {
                let account = account();
                let mut app = app(&account);
                let other =
                    Account::isolated(&std::env::temp_dir(), provider(), "other-panel").unwrap();
                app.accounts.push(other.clone());
                let mut launch = pending(&account, "resume");
                launch.focus_after_spawn = true;
                app.hold_account_launch(&account, launch);
                app.accounts_move(1);
                app.accounts_start_rename();
                app.accounts_name_push('z');
                let draft = app.accounts_panel.as_ref().unwrap().new_name.clone();
                app.finish_account_check(account, Purpose::AfterLogin, identity("expected"), std::env::temp_dir());
                app.complete_account_checks_for_test(identity("expected"));
                assert_eq!(app.accounts_panel.as_ref().unwrap().new_name, draft);
                assert_eq!(
                    app.selected_account().map(|i| &app.accounts[i]),
                    Some(&other)
                );
                assert!(!app.pending.as_ref().unwrap().focus_after_spawn);
            }
        }
    };
}

#[cfg(test)]
lifecycle_tests!(codex_tests, Agent::Codex);
#[cfg(test)]
lifecycle_tests!(claude_tests, Agent::Claude);

#[test]
fn claude_native_and_explicit_default_path_are_different_vaults() {
    let inherited = Account::inherited(Agent::Claude);
    let mut explicit = inherited.clone();
    explicit.slot = mindplayer_core::accounts::Slot::Isolated {
        path: PathBuf::from(std::env::var_os("HOME").unwrap()).join(".claude"),
    };
    assert!(!same_home(&inherited, &explicit));
    let command = mindplayer_core::new_session(Agent::Claude, std::env::temp_dir(), &explicit);
    assert!(!command_matches_account(&command, &inherited));
    assert!(command_matches_account(&command, &explicit));
}
