//! The Accounts screen: one place to see and change which logins exist.
//!
//! Managing accounts is rare, so it gets its own screen rather than rows in the
//! footer, which has to stay small enough to leave the panes room. Starting a
//! session stays automatic — see `App::account_for`.

use super::*;
use mindplayer_core::accounts::{
    save_accounts, Account, AccountError, Role, Slot, MULTI_ACCOUNT_AGENTS,
};

/// Marks a pane that is signing an account in rather than running a session.
pub(crate) const LOGIN_PREFIX: &str = "login:";

/// One line of the Accounts screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountRow {
    /// A provider's name, above its accounts.
    Header(Agent),
    /// An account, by its index in `App::accounts`.
    Entry(usize),
}

/// What a typed name will be used for. One field rather than two, so there is
/// no state where the screen is both adding and renaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameFor {
    /// A new account of this provider.
    NewAccount(Agent),
    /// The account at this index in `App::accounts`.
    Rename(usize),
}

/// State of the Accounts screen (`u`).
#[derive(Debug, Clone, Default)]
pub struct AccountsPanel {
    pub selected: usize,
    /// `Some(buffer)` while a name is being typed.
    pub new_name: Option<String>,
    /// What that name is for.
    pub naming: Option<NameFor>,
    /// Why the last action did not happen, shown until the next one.
    pub error: Option<String>,
}

/// Which account each session belongs to, worked out once for a redraw.
///
/// The session list can hold thousands of rows, and resolving an account means
/// a path comparison per account — cheap once, wasteful per row per frame.
pub(crate) struct AccountMarks {
    home: PathBuf,
    accounts: Vec<Account>,
    /// The account each provider currently starts sessions on.
    in_use: Vec<(Agent, String)>,
}

impl AccountMarks {
    /// The account this session belongs to, when that is not the one its
    /// provider starts new sessions on.
    ///
    /// Saying so on every row would repeat one fact thousands of times; saying
    /// it only where it differs makes the row that is elsewhere the one that
    /// stands out — which is the question a reader actually has.
    pub(crate) fn label_for(&self, session: &Session) -> Option<&str> {
        let owner = mindplayer_core::accounts::owner_of(
            &self.accounts,
            session.agent,
            &session.file,
            &self.home,
        );
        let current = self
            .in_use
            .iter()
            .find(|(agent, _)| *agent == session.agent)
            .map(|(_, name)| name.as_str())?;
        if owner.name == current {
            return None;
        }
        self.accounts
            .iter()
            .find(|a| a.provider == session.agent && a.name == owner.name)
            .map(|a| a.name.as_str())
    }
}

impl App {
    /// Account labels for the session list, or `None` when no provider has a
    /// second account — then every row would carry the same answer.
    pub(crate) fn account_marks(&self) -> Option<AccountMarks> {
        let has_a_choice = MULTI_ACCOUNT_AGENTS.iter().any(|agent| {
            self.accounts
                .iter()
                .filter(|a| a.provider == *agent)
                .count()
                > 1
        });
        if !has_a_choice {
            return None;
        }
        Some(AccountMarks {
            home: limits_home_for_app(),
            accounts: self.accounts.clone(),
            in_use: MULTI_ACCOUNT_AGENTS
                .iter()
                .map(|agent| (*agent, self.account_for(*agent).name))
                .collect(),
        })
    }

    /// True when this provider has more than one login, and a row therefore
    /// has to say which one it is about.
    pub(crate) fn provider_has_several_logins(&self, agent: Agent) -> bool {
        self.accounts.iter().filter(|a| a.provider == agent).count() > 1
    }

    /// The logins a usage refresh should read, which is every one that could
    /// serve a turn.
    ///
    /// An account that is off is skipped: it cannot be reached, and asking
    /// anyway spends a request budget that is per account, not per process.
    pub(crate) fn probe_accounts(&self) -> Vec<Account> {
        self.accounts
            .iter()
            .filter(|a| !a.disabled)
            .cloned()
            .collect()
    }

    /// The Accounts screen's lines, providers in a fixed order so the list does
    /// not reshuffle between redraws.
    pub(crate) fn account_rows(&self) -> Vec<AccountRow> {
        let mut rows = Vec::new();
        for agent in MULTI_ACCOUNT_AGENTS {
            rows.push(AccountRow::Header(agent));
            for (i, account) in self.accounts.iter().enumerate() {
                if account.provider == agent {
                    rows.push(AccountRow::Entry(i));
                }
            }
        }
        rows
    }

    pub fn open_accounts(&mut self) {
        self.accounts_panel = Some(AccountsPanel {
            // Open on the first account rather than a header, so the first
            // keypress acts on something.
            selected: 1,
            ..Default::default()
        });
    }

    pub fn close_accounts(&mut self) {
        self.accounts_panel = None;
    }

    pub fn accounts_move(&mut self, delta: isize) {
        let len = self.account_rows().len();
        let Some(panel) = self.accounts_panel.as_mut() else {
            return;
        };
        if len == 0 {
            return;
        }
        let next = panel.selected as isize + delta;
        panel.selected = next.clamp(0, len as isize - 1) as usize;
    }

    /// The provider the cursor is under, whether it sits on a header or an
    /// account. Adding needs a provider even when no account is highlighted.
    fn selected_provider(&self) -> Option<Agent> {
        let panel = self.accounts_panel.as_ref()?;
        match self.account_rows().get(panel.selected)? {
            AccountRow::Header(agent) => Some(*agent),
            AccountRow::Entry(i) => self.accounts.get(*i).map(|a| a.provider),
        }
    }

    fn selected_account(&self) -> Option<usize> {
        let panel = self.accounts_panel.as_ref()?;
        match self.account_rows().get(panel.selected)? {
            AccountRow::Entry(i) => Some(*i),
            AccountRow::Header(_) => None,
        }
    }

    fn set_error(&mut self, message: impl Into<String>) {
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.error = Some(message.into());
        }
    }

    fn clear_error(&mut self) {
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.error = None;
        }
    }

    fn persist_accounts(&mut self) {
        if let Err(e) = save_accounts(&limits_home_for_app(), &self.accounts) {
            self.set_error(format!("could not save accounts: {e}"));
        }
    }

    /// Stop reaching for this account without forgetting it.
    pub fn accounts_toggle_disabled(&mut self) {
        self.clear_error();
        let Some(i) = self.selected_account() else {
            return;
        };
        self.accounts[i].disabled = !self.accounts[i].disabled;
        self.persist_accounts();
    }

    /// Make this the account new sessions of its provider start on.
    ///
    /// Exactly one account per provider is primary. Two of them left the
    /// choice to whichever came first in the list, which is an order nobody
    /// can see or change — so picking one puts the rest in reserve.
    pub fn accounts_make_primary(&mut self) {
        self.clear_error();
        let Some(i) = self.selected_account() else {
            return;
        };
        if self.accounts[i].disabled {
            self.set_error("this account is off — turn it back on with d first");
            return;
        }
        let provider = self.accounts[i].provider;
        for (j, account) in self.accounts.iter_mut().enumerate() {
            if account.provider == provider {
                account.role = if j == i {
                    Role::Primary
                } else {
                    Role::Fallback
                };
            }
        }
        self.persist_accounts();
        let name = self.accounts[i].name.clone();
        self.status = format!("new {} sessions will use {name}", provider.as_str());
    }

    /// Start a session on the highlighted account now, without changing which
    /// one the next session takes.
    pub fn accounts_start_session(&mut self) {
        self.clear_error();
        let Some(i) = self.selected_account() else {
            return;
        };
        let account = self.accounts[i].clone();
        if account.disabled {
            self.set_error("this account is off — turn it back on with d first");
            return;
        }
        self.accounts_panel = None;
        self.request_new_on(&account, "");
    }

    /// Forget an account. The login this machine already had is not ours to
    /// remove, and an account a pane is running on cannot be taken away from
    /// it mid-session.
    pub fn accounts_remove(&mut self) {
        self.clear_error();
        let Some(i) = self.selected_account() else {
            return;
        };
        if self.accounts[i].is_inherited() {
            self.set_error("the login already on this machine cannot be removed here");
            return;
        }
        if self.pane_count_on(&self.accounts[i]) > 0 {
            self.set_error("a pane is running on this account — close it first");
            return;
        }
        // The slot's directory stays: it holds a real login, and deleting it
        // would sign the user out of something MindPlayer did not create.
        self.accounts.remove(i);
        self.persist_accounts();
        self.accounts_move(0);
    }

    /// How many open panes belong to `account`.
    ///
    /// Open, not merely alive: a pane whose child has exited is still on
    /// screen and still that account's, and renaming or removing an account
    /// out from under it would leave a pane nothing can explain.
    pub(crate) fn pane_count_on(&self, account: &Account) -> usize {
        let home = limits_home_for_app();
        self.panes
            .iter()
            .filter_map(|id| self.all_sessions.iter().find(|s| &s.id == id))
            .filter(|s| {
                s.agent == account.provider
                    && mindplayer_core::accounts::owner_of(&self.accounts, s.agent, &s.file, &home)
                        .name
                        == account.name
            })
            .count()
    }

    pub fn accounts_start_add(&mut self) {
        self.clear_error();
        let Some(agent) = self.selected_provider() else {
            return;
        };
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.new_name = Some(String::new());
            panel.naming = Some(NameFor::NewAccount(agent));
        }
    }

    /// Rename the highlighted account, starting from what it is called now so
    /// a typo is a correction rather than a retype.
    pub fn accounts_start_rename(&mut self) {
        self.clear_error();
        let Some(i) = self.selected_account() else {
            return;
        };
        let current = self.accounts[i].name.clone();
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.new_name = Some(current);
            panel.naming = Some(NameFor::Rename(i));
        }
    }

    pub fn accounts_cancel_add(&mut self) {
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.new_name = None;
            panel.naming = None;
        }
    }

    pub fn accounts_name_push(&mut self, c: char) {
        if let Some(panel) = self.accounts_panel.as_mut() {
            if let Some(name) = panel.new_name.as_mut() {
                name.push(c);
            }
        }
    }

    pub fn accounts_name_backspace(&mut self) {
        if let Some(panel) = self.accounts_panel.as_mut() {
            if let Some(name) = panel.new_name.as_mut() {
                name.pop();
            }
        }
    }

    /// Create the account and open a pane that signs it in.
    ///
    /// The slot's directory is made first: the CLI writes its login there, and
    /// a provider pointed at a path that does not exist reports a broken
    /// install rather than an empty account.
    pub fn accounts_confirm_add(&mut self) {
        let (name, target) = {
            let Some(panel) = self.accounts_panel.as_ref() else {
                return;
            };
            let (Some(name), Some(target)) = (panel.new_name.clone(), panel.naming) else {
                return;
            };
            (name.trim().to_string(), target)
        };
        let agent = match target {
            NameFor::NewAccount(agent) => agent,
            NameFor::Rename(i) => return self.finish_rename(i, &name),
        };

        let home = limits_home_for_app();
        let account = match Account::isolated(&home, agent, &name) {
            Ok(account) => account,
            Err(e) => {
                self.set_error(e.to_string());
                return;
            }
        };
        if self
            .accounts
            .iter()
            .any(|a| a.provider == agent && a.name == account.name)
        {
            self.set_error(AccountError::Duplicate(account.name).to_string());
            return;
        }
        if let Err(e) = mindplayer_core::private::create_dir_private(&account.slot_path()) {
            self.set_error(format!("could not make a home for this account: {e}"));
            return;
        }

        self.accounts.push(account.clone());
        self.persist_accounts();
        self.accounts_cancel_add();
        self.clear_error();
        self.request_login(&account);
        // Signing in does not decide anything by itself, and a pane that just
        // says "logged in" leaves the next step to be guessed. Say it.
        self.status = format!(
            "signing in {} {} — when it finishes, close this pane and press u then w to use it",
            account.provider.as_str(),
            account.name
        );
    }

    /// Give the account at `i` a new name.
    ///
    /// For an isolated account the name is its directory, so the directory
    /// moves with it — keeping the two in step is what stops a later account
    /// of the old name from landing on this one's login. The inherited account
    /// owns no directory, so there it is only a label.
    fn finish_rename(&mut self, i: usize, name: &str) {
        let Some(account) = self.accounts.get(i).cloned() else {
            return;
        };
        if name == account.name {
            self.accounts_cancel_add();
            return;
        }
        if let Err(e) = mindplayer_core::accounts::check_account_name(name) {
            self.set_error(e.to_string());
            return;
        }
        if self
            .accounts
            .iter()
            .any(|a| a.provider == account.provider && a.name == name)
        {
            self.set_error(AccountError::Duplicate(name.to_string()).to_string());
            return;
        }
        if self.pane_count_on(&account) > 0 {
            self.set_error("a pane is running on this account — close it first");
            return;
        }

        if let Slot::Isolated { path } = &account.slot {
            let moved =
                mindplayer_core::accounts::slot_dir(&limits_home_for_app(), account.provider, name);
            if moved.exists() {
                self.set_error("a directory of that name is already there");
                return;
            }
            // A slot that was never written (a sign-in abandoned before it
            // finished) has nothing to move; the new path is made on demand.
            if path.exists() {
                if let Err(e) = std::fs::rename(path, &moved) {
                    self.set_error(format!("could not move this account's home: {e}"));
                    return;
                }
            }
            self.accounts[i].slot = Slot::Isolated { path: moved };
        }
        self.accounts[i].name = name.to_string();
        self.persist_accounts();
        self.accounts_cancel_add();
        self.clear_error();
        self.status = format!(
            "{} {} is now {name}",
            account.provider.as_str(),
            account.name
        );
    }

    /// Sign the highlighted account in again, for one whose login has lapsed.
    pub fn accounts_relogin(&mut self) {
        self.clear_error();
        let Some(i) = self.selected_account() else {
            return;
        };
        let account = self.accounts[i].clone();
        if account.is_inherited() {
            self.set_error("sign in to this one the way you normally do, outside MindPlayer");
            return;
        }
        self.request_login(&account);
    }
}
