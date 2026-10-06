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

/// How an account is remembered as already asked about this run.
pub(crate) fn unusable_key(account: &Account) -> String {
    format!("{}/{}", account.provider.as_str(), account.name)
}

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
    /// The account `x` is waiting on a second press for. Erasing a login
    /// cannot be undone, so it is asked for rather than done on one key.
    pub confirm_remove: Option<String>,
}

/// Which account each session belongs to, worked out once for a redraw.
///
/// The session list can hold thousands of rows, and resolving an account means
/// a path comparison per account — cheap once, wasteful per row per frame.
pub(crate) struct AccountMarks {
    home: PathBuf,
    accounts: Vec<Account>,
    pane_accounts: HashMap<String, (Agent, String)>,
    /// Providers with more than one login, the only ones whose rows carry a name.
    several: Vec<Agent>,
}

/// The longest account name the session list gives room to before cutting it.
pub(crate) const ACCOUNT_COLUMN_MAX: usize = 12;

impl AccountMarks {
    /// The account this session belongs to and its position among its provider's logins, for every row of a provider with more than one login.
    pub(crate) fn label_for(&self, session: &Session) -> Option<(&str, usize)> {
        if !self.several.contains(&session.agent) {
            return None;
        }
        let owner = mindplayer_core::accounts::owner_of(
            &self.accounts,
            session.agent,
            &session.file,
            &self.home,
        );
        let owner_name = self
            .pane_accounts
            .get(&session.id)
            .filter(|(agent, _)| *agent == session.agent)
            .map(|(_, name)| name.as_str())
            .unwrap_or(&owner.name);
        self.accounts
            .iter()
            .filter(|a| a.provider == session.agent)
            .enumerate()
            .find(|(_, a)| a.name == owner_name)
            .map(|(i, a)| (a.name.as_str(), i))
    }

    /// The width of the account column: the longest name it can show, capped.
    pub(crate) fn width(&self) -> usize {
        self.accounts
            .iter()
            .filter(|a| self.several.contains(&a.provider))
            .map(|a| a.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(ACCOUNT_COLUMN_MAX)
    }
}

impl App {
    /// Account labels for the session list, or `None` when no provider has a second account and the column would be empty.
    pub(crate) fn account_marks(&self) -> Option<AccountMarks> {
        let several: Vec<Agent> = MULTI_ACCOUNT_AGENTS
            .into_iter()
            .filter(|agent| self.provider_has_several_logins(*agent))
            .collect();
        if several.is_empty() {
            return None;
        }
        Some(AccountMarks {
            home: limits_home_for_app(),
            accounts: self.accounts.clone(),
            pane_accounts: self.pane_accounts.clone(),
            several,
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

    pub(crate) fn persist_accounts(&mut self) {
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
        if provider == Agent::Codex {
            self.preferred_codex_home = None;
        }
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
        let name = self.accounts[i].name.clone();
        let asked = self
            .accounts_panel
            .as_ref()
            .and_then(|p| p.confirm_remove.clone());
        if asked.as_deref() != Some(name.as_str()) {
            if let Some(panel) = self.accounts_panel.as_mut() {
                panel.confirm_remove = Some(name.clone());
            }
            self.status =
                format!("x again to remove {name} and erase its login — any other key cancels");
            return;
        }

        // Erasing the slot is the point: leaving it behind is what made
        // removing an account look like it had not worked.
        let account = self.accounts.remove(i);
        match mindplayer_core::accounts::forget_login(&account, &limits_home_for_app()) {
            // Already out of the list; say what was left rather than put it
            // back and lose the removal too.
            Err(e) => self.set_error(e),
            Ok(()) => self.status = format!("removed {name} and erased its login"),
        }
        self.persist_accounts();
        self.accounts_cancel_remove();
        self.accounts_move(0);
    }

    /// Answer the prompt with yes: open a pane that signs this account in.
    pub fn confirm_unusable_prompt(&mut self) {
        let Some(account) = self.unusable_prompt.take() else {
            return;
        };
        if account.is_inherited() {
            self.status = format!(
                "{} {}: sign in to this one the way you normally do, outside MindPlayer",
                account.provider.as_str(),
                account.name
            );
            return;
        }
        self.request_login(&account);
        self.status = format!("signing in {} {}", account.provider.as_str(), account.name);
    }

    /// Answer with no: turn the account off so nothing reaches for it again.
    ///
    /// Off, not removed — the account and whatever it holds stay, and `d` on
    /// the Accounts screen turns it back on.
    pub fn decline_unusable_prompt(&mut self) {
        let Some(account) = self.unusable_prompt.take() else {
            return;
        };
        if let Some(found) = self
            .accounts
            .iter_mut()
            .find(|a| a.provider == account.provider && a.name == account.name)
        {
            found.disabled = true;
        }
        self.persist_accounts();
        self.status = format!(
            "{} {} is off — press u then d to turn it back on",
            account.provider.as_str(),
            account.name
        );
    }

    /// Put the prompt away without deciding; it is not asked again this run.
    pub fn dismiss_unusable_prompt(&mut self) {
        self.unusable_prompt = None;
    }

    /// Whether any popup or text entry holds the keyboard, so a prompt raised now would take a key meant for it.
    fn keyboard_taken(&self) -> bool {
        self.help_visible
            || self.accounts_panel.is_some()
            || self.category_menu.is_some()
            || self.category_picker.is_some()
            || self.handoff_picker.is_some()
            || self.new_picker.is_some()
            || self.new_label.is_some()
            || self.dir_input.is_some()
            || self.link_picker.is_some()
            || self.html_preview_picker.is_some()
            || self.html_preview_input.is_some()
            || self.catchup_confirm.is_some()
            || self.transition_report_input.is_some()
            || self.transition_report_review.is_some()
            || self.search_query.is_some()
    }

    /// Put the next waiting unavailable account to the user, but only while the list has the keyboard and nothing else does.
    pub(crate) fn raise_unusable_prompt(&mut self) -> bool {
        if self.unusable_prompt.is_some()
            || self.focus != Focus::List
            || self.screen != Screen::Main
            || self.keyboard_taken()
        {
            return false;
        }
        while !self.unusable_waiting.is_empty() {
            let account = self.unusable_waiting.remove(0);
            let current = self
                .accounts
                .iter()
                .find(|a| a.provider == account.provider && a.name == account.name)
                .cloned();
            let Some(current) = current else {
                continue;
            };
            if current.disabled || self.pane_count_on(&current) > 0 {
                continue;
            }
            if self.unusable_asked.insert(unusable_key(&current)) {
                self.unusable_prompt = Some(current);
                return true;
            }
        }
        false
    }

    /// Forget a pending `x`, so a stray second press cannot erase anything.
    pub fn accounts_cancel_remove(&mut self) {
        if let Some(panel) = self.accounts_panel.as_mut() {
            panel.confirm_remove = None;
        }
    }

    /// How many open panes belong to `account`.
    ///
    /// Open, not merely alive: a pane whose child has exited is still on
    /// screen and still that account's, and renaming or removing an account
    /// out from under it would leave a pane nothing can explain.
    pub(crate) fn pane_count_on(&self, account: &Account) -> usize {
        let home = limits_home_for_app();
        // A sign-in pane has no transcript to judge by, so it is known by the id it was opened under.
        let login_id = format!(
            "{LOGIN_PREFIX}{}:{}",
            account.provider.as_str(),
            account.name
        );
        let started_on = |id: &String| {
            self.pane_accounts
                .get(id)
                .is_some_and(|(agent, name)| *agent == account.provider && *name == account.name)
        };
        let recorded = self.panes.iter().filter(|id| started_on(id)).count();
        recorded
            + self
                .panes
                .iter()
                .filter(|id| !started_on(id))
                .filter_map(|id| self.all_sessions.iter().find(|s| &s.id == id))
                .filter(|s| {
                    s.id == login_id
                        || (s.agent == account.provider
                            && mindplayer_core::accounts::owner_of(
                                &self.accounts,
                                s.agent,
                                &s.file,
                                &home,
                            )
                            .name
                                == account.name)
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
        // Signing out first: a slot already holding a sign-in does not change
        // hands on `login` alone, which is what made swapping accounts mean
        // signing out by hand.
        self.request_relogin(&account);
    }
}
