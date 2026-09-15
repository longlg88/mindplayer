//! Several logins per provider.
//!
//! A provider's CLI keeps one login per credential store, and on macOS Claude
//! and Cursor share a single keychain slot, so a second account cannot simply
//! be logged in alongside the first. What every CLI does support is being
//! pointed at a different config home, which is what an [`Account`] is: a name,
//! and the environment that makes the CLI read one login instead of another.
//!
//! The login already on this machine stays where it is and is named
//! [`Slot::Inherited`]. Credentials are never copied into a slot: a copy that
//! refreshes rotates the token out from under the original holder, which
//! reaches the first holder as an authentication failure it cannot explain.

use crate::session::Agent;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Providers that can hold more than one account.
pub const MULTI_ACCOUNT_AGENTS: [Agent; 4] =
    [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor];

/// The name given to the login that already exists on this machine.
pub const DEFAULT_ACCOUNT: &str = "default";

/// Which login an account reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Slot {
    /// The login already on this machine, wherever the CLI normally keeps it.
    Inherited,
    /// A directory holding this account's own login, and nothing else's.
    Isolated { path: PathBuf },
}

/// Whether an account is reached for first or only when the others cannot serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Primary,
    Fallback,
}

/// One login, and the environment that selects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub provider: Agent,
    pub name: String,
    pub slot: Slot,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub disabled: bool,
}

/// The environment change that selects an account: what to set, and what to
/// clear so an ambient credential cannot decide which account serves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEnv {
    pub set: Vec<(String, String)>,
    pub unset: Vec<String>,
}

impl LaunchEnv {
    /// True when launching needs no change at all, which is the inherited login.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.unset.is_empty()
    }
}

/// Why a name cannot be used for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountError {
    /// The name would have to become a directory and cannot.
    BadName(String),
    /// The provider cannot hold a second account yet.
    Unsupported(Agent),
    /// Two accounts of one provider claimed the same name.
    Duplicate(String),
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountError::BadName(name) => {
                write!(
                    f,
                    "`{name}` cannot be an account name: use letters, digits, `-` or `_`"
                )
            }
            AccountError::Unsupported(agent) => write!(
                f,
                "{} keeps one login per machine, so a second account is not available yet",
                agent.as_str()
            ),
            AccountError::Duplicate(name) => write!(f, "an account named `{name}` already exists"),
        }
    }
}

impl std::error::Error for AccountError {}

/// Accepts only names that are safe as a single directory component.
///
/// The name becomes a path segment under the accounts directory, so anything
/// that could climb out of it, or name a different slot on a case-insensitive
/// filesystem, is refused rather than sanitised.
fn check_name(name: &str) -> Result<(), AccountError> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(AccountError::BadName(name.to_string()))
    }
}

/// Accepts a name for an account, or says why it cannot be one.
///
/// # Errors
/// [`AccountError::BadName`] when the name could not be a directory component.
pub fn check_account_name(name: &str) -> Result<(), AccountError> {
    check_name(name)
}

/// The directory an isolated account of this name would keep its login in.
pub fn slot_dir(home: &Path, provider: Agent, name: &str) -> PathBuf {
    accounts_dir(home, provider).join(name)
}

/// Where a provider's isolated slots live: `<home>/.mindplayer/accounts/<provider>/`.
pub fn accounts_dir(home: &Path, provider: Agent) -> PathBuf {
    home.join(".mindplayer")
        .join("accounts")
        .join(provider.as_str())
}

/// The file listing every account. Usage readings are not stored here; they
/// change far more often and already have their own cache.
pub fn accounts_path(home: &Path) -> PathBuf {
    home.join(".mindplayer").join("accounts.json")
}

impl Account {
    /// The login already on this machine.
    pub fn inherited(provider: Agent) -> Self {
        Account {
            provider,
            name: DEFAULT_ACCOUNT.to_string(),
            slot: Slot::Inherited,
            role: Role::Primary,
            disabled: false,
        }
    }

    /// A named account with its own login directory under `home`.
    pub fn isolated(home: &Path, provider: Agent, name: &str) -> Result<Self, AccountError> {
        if !MULTI_ACCOUNT_AGENTS.contains(&provider) {
            return Err(AccountError::Unsupported(provider));
        }
        check_name(name)?;
        if name == DEFAULT_ACCOUNT {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        Ok(Account {
            provider,
            name: name.to_string(),
            slot: Slot::Isolated {
                path: accounts_dir(home, provider).join(name),
            },
            role: Role::Primary,
            disabled: false,
        })
    }

    /// True for the login this machine already had before any slot existed.
    pub fn is_inherited(&self) -> bool {
        matches!(self.slot, Slot::Inherited)
    }

    /// The directory holding this account's login, empty for the inherited one
    /// (whose login is wherever the CLI already keeps it).
    pub fn slot_path(&self) -> PathBuf {
        match &self.slot {
            Slot::Inherited => PathBuf::new(),
            Slot::Isolated { path } => path.clone(),
        }
    }

    /// The environment that makes this provider's CLI read this account.
    ///
    /// The inherited login is reached by changing nothing, so it returns an
    /// empty change: clearing ambient credentials there would take away a
    /// login the user is deliberately running on.
    pub fn launch_env(&self) -> LaunchEnv {
        let Slot::Isolated { path } = &self.slot else {
            return LaunchEnv::default();
        };
        let dir = path.to_string_lossy().into_owned();
        match self.provider {
            Agent::Codex => LaunchEnv {
                set: vec![("CODEX_HOME".into(), dir)],
                unset: vec![
                    "CODEX_ACCESS_TOKEN".into(),
                    "CODEX_API_KEY".into(),
                    "OPENAI_API_KEY".into(),
                ],
            },
            Agent::Claude => LaunchEnv {
                set: vec![("CLAUDE_CONFIG_DIR".into(), dir)],
                unset: vec![
                    "ANTHROPIC_API_KEY".into(),
                    "ANTHROPIC_AUTH_TOKEN".into(),
                    "CLAUDE_CODE_OAUTH_TOKEN".into(),
                ],
            },
            // Kiro derives its paths from HOME, so the whole home moves; the XDG
            // variables carry the same move on Linux. Measured: with an ambient
            // KIRO_API_KEY still set, an empty home reports itself authenticated.
            Agent::Kiro => LaunchEnv {
                set: vec![
                    ("HOME".into(), dir.clone()),
                    ("KIRO_DATA_DIR".into(), join(path, ".local/share/kiro-cli")),
                    ("KIRO_HOME".into(), join(path, ".kiro")),
                    ("XDG_CACHE_HOME".into(), join(path, ".cache")),
                    ("XDG_CONFIG_HOME".into(), join(path, ".config")),
                    ("XDG_DATA_HOME".into(), join(path, ".local/share")),
                    ("XDG_STATE_HOME".into(), join(path, ".local/state")),
                ],
                unset: vec!["KIRO_API_KEY".into()],
            },
            // Measured: the Cursor CLI decides which login it is on from HOME
            // alone — an untouched home reports the machine's login, an empty
            // one reports none. The rest pins the choice rather than leaving
            // it to a default: the config and data directories so nothing is
            // read from the machine's, and the credential store so the login
            // is written beside them instead of somewhere shared.
            Agent::Cursor => LaunchEnv {
                set: vec![
                    ("AGENT_CLI_CREDENTIAL_STORE".into(), "file".into()),
                    ("CURSOR_CONFIG_DIR".into(), join(path, ".cursor")),
                    ("CURSOR_DATA_DIR".into(), join(path, ".cursor")),
                    ("HOME".into(), dir.clone()),
                    ("XDG_CACHE_HOME".into(), join(path, ".cache")),
                    ("XDG_CONFIG_HOME".into(), join(path, ".config")),
                ],
                unset: vec![
                    "CURSOR_API_BASE_URL".into(),
                    "CURSOR_API_ENDPOINT".into(),
                    "CURSOR_API_KEY".into(),
                    "CURSOR_AUTH_TOKEN".into(),
                ],
            },
        }
    }

    /// Where this account's sessions are written, which is where discovery has
    /// to look for them.
    ///
    /// Confirmed per provider: Claude reports `<config dir>/projects` as its
    /// own projects directory, and `CODEX_HOME` is the directory Codex calls
    /// `~/.codex`, whose sessions live in `sessions/`.
    pub fn session_root(&self, fallback_home: &Path) -> PathBuf {
        let base = match &self.slot {
            Slot::Inherited => return inherited_session_root(self.provider, fallback_home),
            Slot::Isolated { path } => path.clone(),
        };
        match self.provider {
            Agent::Codex => base.join("sessions"),
            Agent::Claude => base.join("projects"),
            Agent::Kiro => base.join(".kiro").join("sessions").join("cli"),
            Agent::Cursor => base.join(".cursor").join("chats"),
        }
    }
}

fn join(base: &Path, tail: &str) -> String {
    let mut path = base.to_path_buf();
    for part in tail.split('/') {
        path.push(part);
    }
    path.to_string_lossy().into_owned()
}

fn inherited_session_root(provider: Agent, home: &Path) -> PathBuf {
    match provider {
        Agent::Codex => home.join(".codex").join("sessions"),
        Agent::Claude => home.join(".claude").join("projects"),
        Agent::Kiro => home.join(".kiro").join("sessions").join("cli"),
        Agent::Cursor => home.join(".cursor").join("chats"),
    }
}

/// Every account, with the inherited login present for each provider even when
/// nothing has been saved yet.
///
/// A machine that has never opened this screen still has one working account
/// per provider, so the list is never empty and the footer never reads as
/// though nothing is configured.
pub fn load_accounts(home: &Path) -> Vec<Account> {
    let saved: Vec<Account> = std::fs::read_to_string(accounts_path(home))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    for agent in MULTI_ACCOUNT_AGENTS {
        if !saved
            .iter()
            .any(|a| a.provider == agent && a.is_inherited())
        {
            out.push(Account::inherited(agent));
        }
    }
    out.extend(saved.into_iter().filter(|a| check_name(&a.name).is_ok()));
    one_primary_per_provider(&mut out);
    out
}

/// Leave exactly one primary per provider, keeping the first.
///
/// Two primaries make the choice depend on list order, which is an order
/// nobody can see or change — the second account then can never be reached.
/// A file written before this rule existed is repaired on the way in.
fn one_primary_per_provider(accounts: &mut [Account]) {
    for agent in MULTI_ACCOUNT_AGENTS {
        let mut seen = false;
        for account in accounts.iter_mut().filter(|a| a.provider == agent) {
            if account.role != Role::Primary {
                continue;
            }
            if seen {
                account.role = Role::Fallback;
            }
            seen = true;
        }
    }
}

/// Which account wrote `file`, judged by where it sits.
///
/// Resuming has to land on the account that started the session, because the
/// transcript only exists under that account's home. The path is the evidence
/// for that, and it stays right for sessions that predate any of this.
///
/// The longest matching root wins, so a slot nested inside another store
/// cannot be mistaken for the store containing it. Nothing matching means the
/// inherited login: that is where every session lived before slots existed.
pub fn owner_of(accounts: &[Account], agent: Agent, file: &Path, fallback_home: &Path) -> Account {
    accounts
        .iter()
        .filter(|account| account.provider == agent)
        .filter_map(|account| {
            let root = account.session_root(fallback_home);
            file.starts_with(&root)
                .then(|| (root.components().count(), account))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, account)| account.clone())
        .unwrap_or_else(|| Account::inherited(agent))
}

/// Replace the stored list. The inherited accounts are stored too, so a role
/// or disabled flag set on one survives a restart.
pub fn save_accounts(home: &Path, accounts: &[Account]) -> std::io::Result<()> {
    let path = accounts_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(accounts)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mp-accounts-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_login_already_on_this_machine_is_reached_by_changing_nothing() {
        for agent in MULTI_ACCOUNT_AGENTS {
            let account = Account::inherited(agent);
            assert!(
                account.launch_env().is_empty(),
                "{} inherited slot must not touch the environment",
                agent.as_str()
            );
        }
    }

    #[test]
    fn an_isolated_codex_account_points_codex_at_its_own_home() {
        let home = tmp();
        let account = Account::isolated(&home, Agent::Codex, "work").unwrap();
        let env = account.launch_env();
        let dir = accounts_dir(&home, Agent::Codex).join("work");
        assert_eq!(
            env.set,
            vec![("CODEX_HOME".to_string(), dir.to_string_lossy().into_owned())]
        );
        assert_eq!(account.session_root(&home), dir.join("sessions"));
    }

    #[test]
    fn an_isolated_claude_account_points_claude_at_its_own_config_dir() {
        let home = tmp();
        let account = Account::isolated(&home, Agent::Claude, "personal").unwrap();
        let dir = accounts_dir(&home, Agent::Claude).join("personal");
        assert_eq!(
            account.launch_env().set,
            vec![(
                "CLAUDE_CONFIG_DIR".to_string(),
                dir.to_string_lossy().into_owned()
            )]
        );
        // `claude auth status` reports this exact path as its projects directory.
        assert_eq!(account.session_root(&home), dir.join("projects"));
    }

    #[test]
    fn an_isolated_kiro_account_moves_the_whole_home() {
        let home = tmp();
        let account = Account::isolated(&home, Agent::Kiro, "team").unwrap();
        let env = account.launch_env();
        let dir = accounts_dir(&home, Agent::Kiro).join("team");
        let set: Vec<&str> = env.set.iter().map(|(k, _)| k.as_str()).collect();
        assert!(set.contains(&"HOME"), "kiro follows HOME: {set:?}");
        for name in [
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
        ] {
            assert!(set.contains(&name), "{name} missing from {set:?}");
        }
        assert_eq!(
            env.set
                .iter()
                .find(|(k, _)| k == "HOME")
                .map(|(_, v)| v.as_str()),
            Some(dir.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn an_ambient_key_cannot_decide_which_account_serves() {
        let home = tmp();
        // Measured: an empty kiro home with KIRO_API_KEY still set reports
        // "Authenticated with API key" instead of "Not logged in".
        let kiro = Account::isolated(&home, Agent::Kiro, "team").unwrap();
        assert!(kiro
            .launch_env()
            .unset
            .contains(&"KIRO_API_KEY".to_string()));

        let codex = Account::isolated(&home, Agent::Codex, "work").unwrap();
        for name in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"] {
            assert!(
                codex.launch_env().unset.contains(&name.to_string()),
                "{name} would survive into an isolated codex slot"
            );
        }

        let claude = Account::isolated(&home, Agent::Claude, "work").unwrap();
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert!(
                claude.launch_env().unset.contains(&name.to_string()),
                "{name} would survive into an isolated claude slot"
            );
        }
    }

    /// Measured: the Cursor CLI reads its login from HOME — an untouched home
    /// reports the machine's login and an empty one reports none. The config
    /// and data directories and the credential store are pinned alongside so
    /// the choice does not rest on a default.
    #[test]
    fn an_isolated_cursor_account_moves_the_whole_home() {
        let home = tmp();
        let account = Account::isolated(&home, Agent::Cursor, "work").unwrap();
        let env = account.launch_env();
        let dir = accounts_dir(&home, Agent::Cursor).join("work");

        assert_eq!(
            env.set
                .iter()
                .find(|(k, _)| k == "HOME")
                .map(|(_, v)| v.as_str()),
            Some(dir.to_string_lossy().as_ref())
        );
        let set: Vec<&str> = env.set.iter().map(|(k, _)| k.as_str()).collect();
        for name in [
            "CURSOR_CONFIG_DIR",
            "CURSOR_DATA_DIR",
            "AGENT_CLI_CREDENTIAL_STORE",
        ] {
            assert!(set.contains(&name), "{name} missing from {set:?}");
        }
        assert_eq!(
            env.set
                .iter()
                .find(|(k, _)| k == "AGENT_CLI_CREDENTIAL_STORE")
                .map(|(_, v)| v.as_str()),
            Some("file"),
            "the login must be written beside the slot, not somewhere shared"
        );
        for name in ["CURSOR_API_KEY", "CURSOR_AUTH_TOKEN"] {
            assert!(
                env.unset.contains(&name.to_string()),
                "{name} would decide the account instead of the slot"
            );
        }
        assert_eq!(
            account.session_root(&home),
            dir.join(".cursor").join("chats")
        );
    }

    /// Every provider can hold a second login now, so nothing is refused for
    /// being unsupported.
    #[test]
    fn no_provider_is_left_out() {
        let home = tmp();
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            assert!(MULTI_ACCOUNT_AGENTS.contains(&agent), "{agent:?}");
            assert!(
                Account::isolated(&home, agent, "second").is_ok(),
                "{agent:?}"
            );
        }
    }

    #[test]
    fn a_name_that_could_climb_out_of_the_accounts_directory_is_refused() {
        let home = tmp();
        for name in [
            "..",
            ".",
            "a/b",
            "../escape",
            "",
            "  ",
            "-lead",
            "wörk",
            "a\0b",
        ] {
            assert!(
                Account::isolated(&home, Agent::Codex, name).is_err(),
                "`{name}` was accepted as an account name"
            );
        }
        assert!(Account::isolated(&home, Agent::Codex, "work-2_b").is_ok());
    }

    #[test]
    fn the_default_name_is_reserved_for_the_existing_login() {
        let home = tmp();
        assert_eq!(
            Account::isolated(&home, Agent::Codex, DEFAULT_ACCOUNT),
            Err(AccountError::Duplicate(DEFAULT_ACCOUNT.to_string()))
        );
    }

    #[test]
    fn a_machine_with_nothing_saved_still_has_one_account_per_provider() {
        let home = tmp();
        let accounts = load_accounts(&home);
        assert_eq!(accounts.len(), MULTI_ACCOUNT_AGENTS.len());
        for agent in MULTI_ACCOUNT_AGENTS {
            let found = accounts.iter().find(|a| a.provider == agent).unwrap();
            assert!(found.is_inherited());
            assert_eq!(found.name, DEFAULT_ACCOUNT);
        }
    }

    #[test]
    fn a_saved_list_survives_a_restart_without_growing_a_second_default() {
        let home = tmp();
        let mut accounts = load_accounts(&home);
        accounts.push(Account::isolated(&home, Agent::Codex, "overflow").unwrap());
        if let Some(first) = accounts.iter_mut().find(|a| a.provider == Agent::Codex) {
            first.role = Role::Fallback;
        }
        save_accounts(&home, &accounts).unwrap();

        let reloaded = load_accounts(&home);
        assert_eq!(reloaded.len(), accounts.len(), "{reloaded:?}");
        assert_eq!(
            reloaded
                .iter()
                .filter(|a| a.provider == Agent::Codex && a.is_inherited())
                .count(),
            1
        );
        assert_eq!(
            reloaded
                .iter()
                .find(|a| a.provider == Agent::Codex && a.is_inherited())
                .map(|a| a.role),
            Some(Role::Fallback)
        );
    }

    #[test]
    fn a_session_is_owned_by_the_account_whose_store_it_sits_in() {
        let home = tmp();
        let work = Account::isolated(&home, Agent::Codex, "work").unwrap();
        let accounts = vec![Account::inherited(Agent::Codex), work.clone()];

        let in_work = work.session_root(&home).join("2026/09/15/rollout-a.jsonl");
        assert_eq!(
            owner_of(&accounts, Agent::Codex, &in_work, &home).name,
            "work"
        );

        let inherited = home.join(".codex/sessions/2026/09/15/rollout-b.jsonl");
        assert_eq!(
            owner_of(&accounts, Agent::Codex, &inherited, &home).name,
            DEFAULT_ACCOUNT
        );
    }

    #[test]
    fn a_session_from_before_any_slot_existed_still_resolves() {
        let home = tmp();
        let stray = PathBuf::from("/somewhere/else/rollout-c.jsonl");
        let owner = owner_of(&[], Agent::Claude, &stray, &home);
        assert!(owner.is_inherited());
        assert!(owner.launch_env().is_empty());
    }

    #[test]
    fn a_store_nested_inside_another_is_not_mistaken_for_it() {
        let home = tmp();
        let outer = Account {
            provider: Agent::Codex,
            name: "outer".into(),
            slot: Slot::Isolated {
                path: home.join("slots"),
            },
            role: Role::Primary,
            disabled: false,
        };
        let inner = Account {
            provider: Agent::Codex,
            name: "inner".into(),
            slot: Slot::Isolated {
                path: home.join("slots/sessions/deep"),
            },
            role: Role::Primary,
            disabled: false,
        };
        let accounts = vec![outer, inner.clone()];
        let file = inner.session_root(&home).join("rollout-d.jsonl");
        assert_eq!(
            owner_of(&accounts, Agent::Codex, &file, &home).name,
            "inner",
            "the shallower store swallowed a session that is not its own"
        );
    }

    #[test]
    fn one_provider_cannot_claim_another_providers_session() {
        let home = tmp();
        let codex = Account::isolated(&home, Agent::Codex, "work").unwrap();
        let file = codex.session_root(&home).join("rollout-e.jsonl");
        let owner = owner_of(&[codex], Agent::Claude, &file, &home);
        assert!(owner.is_inherited());
        assert_eq!(owner.provider, Agent::Claude);
    }

    #[test]
    fn a_stored_name_that_is_not_safe_is_dropped_on_load() {
        let home = tmp();
        let path = accounts_path(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let forged = r#"[{"provider":"codex","name":"../../escape","slot":{"kind":"isolated","path":"/tmp/x"}}]"#;
        std::fs::write(&path, forged).unwrap();
        let accounts = load_accounts(&home);
        assert!(
            accounts.iter().all(|a| a.name != "../../escape"),
            "{accounts:?}"
        );
    }
}
