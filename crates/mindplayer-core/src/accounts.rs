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

/// Router service state to remove at native auth and credential boundaries.
/// Do not scrub Codex's shared launch environment, which also starts Node.
pub(crate) const ROUTER_SERVICE_ENV: &[&str] = &[
    "AI_GATEWAY_API_KEY",
    "TYPESAFE_API_KEY",
    "VERCEL_OIDC_TOKEN",
    "MINDPLAYER_LOCAL_ROUTER_TOKEN",
    "MINDPLAYER_ROUTER_AVAILABILITY",
    "MINDPLAYER_ROUTER_STATUS",
    "MINDPLAYER_CLAUDE_JEV_URL",
    "MINDPLAYER_CLAUDE_JEV_TOKEN",
    "MINDPLAYER_PER_PROMPT_CHOICE",
    "MINDPLAYER_JEV_DEFAULT",
];

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
    /// Codex's live, opaque account fingerprint explicitly bound to this
    /// display account. Missing means the login has not been verified yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_identity: Option<String>,
    /// Claude's native account fingerprint explicitly bound to this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_identity: Option<String>,
}

/// The environment change that selects an account: what to set, and what to
/// clear so an ambient credential cannot decide which account serves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEnv {
    pub set: Vec<(String, String)>,
    pub unset: Vec<String>,
}

impl LaunchEnv {
    /// True when launching needs no environment changes.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.unset.is_empty()
    }

    pub(crate) fn without_router_service_env(mut self) -> Self {
        self.set
            .retain(|(key, _)| !ROUTER_SERVICE_ENV.contains(&key.as_str()));
        for key in ROUTER_SERVICE_ENV {
            if !self.unset.iter().any(|unset| unset == key) {
                self.unset.push((*key).into());
            }
        }
        self
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

/// Reserve a new, empty login home without reusing any registered name or path.
/// A rename keeps its original path reserved even while that home is absent.
/// The final directory is created exclusively, so concurrent callers and
/// unregistered files, directories or symlinks cannot share a credential home.
///
/// # Errors
/// Returns creation errors other than an occupied candidate, which is skipped.
pub fn reserve_fresh_account(
    home: &Path,
    provider: Agent,
    registered: &[Account],
) -> std::io::Result<Account> {
    crate::private::create_dir_private(&accounts_dir(home, provider))?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(crate::private::DIR_MODE);
    }
    for n in 2..=u64::MAX {
        let name = format!("{}-{n}", provider.as_str());
        let account = Account::isolated(home, provider, &name)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let path = account.slot_path();
        if registered.iter().any(|existing| {
            (existing.provider == provider && existing.name == name) || existing.slot_path() == path
        }) {
            continue;
        }
        match builder.create(&path) {
            Ok(()) => return Ok(account),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no fresh account home is available",
    ))
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
            codex_identity: None,
            claude_identity: None,
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
            codex_identity: None,
            claude_identity: None,
        })
    }

    /// The explicitly saved binding for this provider, never the display name.
    pub fn identity_fingerprint(&self) -> Option<&str> {
        match self.provider {
            Agent::Codex => self.codex_identity.as_deref(),
            Agent::Claude => self.claude_identity.as_deref(),
            _ => None,
        }
    }

    /// Change only this provider's binding; other providers have no binding yet.
    pub fn set_identity_fingerprint(&mut self, fingerprint: Option<String>) {
        match self.provider {
            Agent::Codex => self.codex_identity = fingerprint,
            Agent::Claude => self.claude_identity = fingerprint,
            _ => {}
        }
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
    /// Inherited accounts use the native default, not an ambient named slot.
    /// Claude must UNSET its config override: explicitly setting `~/.claude`
    /// selects a different, path-hashed macOS Keychain service.
    pub fn launch_env(&self) -> LaunchEnv {
        if self.provider == Agent::Claude && self.is_inherited() {
            return claude_launch_env(None);
        }
        let path = match &self.slot {
            Slot::Inherited if self.provider == Agent::Codex => {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
            }
            Slot::Inherited => return LaunchEnv::default(),
            Slot::Isolated { path } => Some(path.clone()),
        };
        let Some(path) = path else {
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
            Agent::Claude => claude_launch_env(Some(&path)),
            // Kiro derives its paths from HOME, so the whole home moves; the XDG
            // variables carry the same move on Linux. Measured: with an ambient
            // KIRO_API_KEY still set, an empty home reports itself authenticated.
            Agent::Kiro => LaunchEnv {
                set: vec![
                    ("HOME".into(), dir.clone()),
                    ("KIRO_DATA_DIR".into(), join(&path, ".local/share/kiro-cli")),
                    ("KIRO_HOME".into(), join(&path, ".kiro")),
                    ("XDG_CACHE_HOME".into(), join(&path, ".cache")),
                    ("XDG_CONFIG_HOME".into(), join(&path, ".config")),
                    ("XDG_DATA_HOME".into(), join(&path, ".local/share")),
                    ("XDG_STATE_HOME".into(), join(&path, ".local/state")),
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
                    ("CURSOR_CONFIG_DIR".into(), join(&path, ".cursor")),
                    ("CURSOR_DATA_DIR".into(), join(&path, ".cursor")),
                    ("HOME".into(), dir.clone()),
                    ("XDG_CACHE_HOME".into(), join(&path, ".cache")),
                    ("XDG_CONFIG_HOME".into(), join(&path, ".config")),
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

/// The same account selection for native status probes and interactive launches.
pub(crate) fn claude_launch_env(config_dir: Option<&Path>) -> LaunchEnv {
    let mut env = LaunchEnv {
        set: config_dir
            .map(|path| {
                vec![(
                    "CLAUDE_CONFIG_DIR".into(),
                    path.to_string_lossy().into_owned(),
                )]
            })
            .unwrap_or_default(),
        unset: vec![
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
            "ANTHROPIC_API_KEY".into(),
            "ANTHROPIC_AUTH_TOKEN".into(),
            "CLAUDE_CODE_OAUTH_TOKEN".into(),
            "CLAUDE_CODE_OAUTH_REFRESH_TOKEN".into(),
            "CLAUDE_CODE_OAUTH_SCOPES".into(),
            "ANTHROPIC_CUSTOM_HEADERS".into(),
            "ANTHROPIC_BASE_URL".into(),
            "CLAUDE_CODE_USE_BEDROCK".into(),
            "CLAUDE_CODE_USE_VERTEX".into(),
            "CLAUDE_CODE_USE_FOUNDRY".into(),
            "CLAUDE_CODE_USE_ANTHROPIC_AWS".into(),
            "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD".into(),
            "CLAUDE_CODE_USE_MANTLE".into(),
        ],
    };
    if config_dir.is_none() {
        env.unset.push("CLAUDE_CONFIG_DIR".into());
    }
    env.without_router_service_env()
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
    accounts_from_saved(saved)
}

/// The saved list as another MindPlayer left it, or `None` when there is no file or it does not parse, so a broken file is never read as an empty list.
pub fn read_saved_accounts(home: &Path) -> Option<Vec<Account>> {
    let raw = std::fs::read_to_string(accounts_path(home)).ok()?;
    let saved: Vec<Account> = serde_json::from_str(&raw).ok()?;
    Some(accounts_from_saved(saved))
}

/// Lay this MindPlayer's edits (`ours` against `base`, what it last read or wrote) onto a list another one saved since, matching by provider and slot.
pub fn merge_accounts(base: &[Account], ours: &[Account], theirs: &[Account]) -> Vec<Account> {
    // Both sides are compared as a reader sees them, so a second primary held only in memory is not a promotion.
    let base = &accounts_from_saved(base.to_vec());
    let ours = &accounts_from_saved(ours.to_vec());
    let key = |a: &Account| (a.provider, a.slot.clone());
    let mut out = theirs.to_vec();
    for gone in base
        .iter()
        .filter(|b| !ours.iter().any(|a| key(a) == key(b)))
    {
        out.retain(|t| key(t) != key(gone));
    }
    for a in ours {
        let Some(b) = base.iter().find(|b| key(b) == key(a)) else {
            if !out.iter().any(|t| key(t) == key(a)) {
                out.push(a.clone());
            }
            continue;
        };
        // An account the other side removed stays removed, since its login directory is already gone.
        let Some(t) = out.iter_mut().find(|t| key(t) == key(a)) else {
            continue;
        };
        // Every field is named here, so a new one cannot be left out of the merge without a build error.
        let Account {
            provider: _,
            slot: _,
            role: _,
            name: _,
            disabled: _,
            codex_identity: _,
            claude_identity: _,
        } = a;
        // Only the fields this side changed are laid on, so the other side's edits to the rest survive.
        if a.name != b.name {
            t.name.clone_from(&a.name);
        }
        if a.disabled != b.disabled {
            t.disabled = a.disabled;
        }
        if a.codex_identity != b.codex_identity {
            t.codex_identity.clone_from(&a.codex_identity);
        }
        if a.claude_identity != b.claude_identity {
            t.claude_identity.clone_from(&a.claude_identity);
        }
        // A demotion here only ever comes with a promotion, so only the promotion is replayed.
        if a.role == Role::Primary && b.role != Role::Primary {
            for t in out.iter_mut().filter(|t| t.provider == a.provider) {
                t.role = if key(t) == key(a) {
                    Role::Primary
                } else {
                    Role::Fallback
                };
            }
        }
    }
    accounts_from_saved(out)
}

/// The list as a reader sees a saved one: the inherited logins present, bad names dropped, one primary per provider.
pub fn accounts_from_saved(saved: Vec<Account>) -> Vec<Account> {
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

/// Delete the sign-in an isolated account keeps, and the home holding it.
///
/// Removing an account from the list left its sign-in on disk, so the login
/// information was never actually gone — which is the thing someone removing
/// an account is usually trying to be rid of.
///
/// Only a directory MindPlayer itself laid down under `accounts/<provider>/`
/// is removed. The login this machine came with lives in the provider's own
/// home and is not MindPlayer's to delete; a slot pointing anywhere else is
/// refused rather than trusted, because this deletes a directory whole.
///
/// # Errors
/// Returns the reason when the slot is not one of ours, or the delete fails.
pub fn forget_login(account: &Account, home: &Path) -> Result<(), String> {
    let Slot::Isolated { path } = &account.slot else {
        return Err("the login already on this machine is not MindPlayer's to delete".into());
    };
    // Rename changes the display label, not the provider's directory. Keep
    // deletion bounded to one safe slot directly under this provider's root.
    let parent = accounts_dir(home, account.provider);
    let safe_slot = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| check_name(name).is_ok());
    if path.parent() != Some(parent.as_path()) || !safe_slot {
        return Err(format!(
            "this account's home is not one MindPlayer made ({}), so it is left alone",
            path.display()
        ));
    }
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(path).map_err(|e| format!("could not delete the sign-in: {e}"))
}

/// How long to wait for a provider to report which account it is running as.
const WHOAMI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The command that reports which account a provider is running as, and the
/// phrase its answer carries when there is none.
///
/// Every pair was read off the real CLI twice — once on a home holding no
/// account and once on a home holding one — so the phrase separates the two
/// rather than merely appearing in one of them.
fn whoami_command(agent: Agent) -> (&'static str, &'static [&'static str], &'static str) {
    let (args, marker): (&'static [&'static str], &'static str) = match agent {
        Agent::Codex => (&["login", "status"], "not logged in"),
        Agent::Claude => (&["auth", "status"], "\"loggedin\": false"),
        Agent::Kiro => (&["whoami"], "not logged in"),
        Agent::Cursor => (&["status"], "not logged in"),
    };
    (agent.program(), args, marker)
}

/// Whether `account` has no account behind it, asked of the provider itself.
///
/// `Some(true)` only when the provider says outright that it has none.
/// Anything else — a CLI that is missing, one that hangs, one that answers
/// something unexpected — is `None`: a usage reading fails for reasons that
/// have nothing to do with this, and acting on a guess would interrupt
/// someone over a dropped network.
pub fn signed_out(account: &Account, home: &Path) -> Option<bool> {
    let (program, args, marker) = whoami_command(account.provider);
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let env = account.launch_env().without_router_service_env();
    for name in &env.unset {
        command.env_remove(name);
    }
    for (name, value) in &env.set {
        command.env(name, value);
    }

    let mut child = command.spawn().ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < WHOAMI_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let mut answer = String::from_utf8_lossy(&out.stdout).into_owned();
    answer.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(answer.to_lowercase().contains(marker))
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
        .unwrap_or_else(|| {
            accounts
                .iter()
                .find(|account| account.provider == agent && account.is_inherited())
                .cloned()
                .unwrap_or_else(|| Account::inherited(agent))
        })
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
    // A symlinked file keeps its link: the replacement lands on the file the link points at.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    // Other MindPlayers re-read this file every few seconds, so it is replaced whole rather than rewritten in place.
    let staged = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let written = (|| {
        use std::io::Write;
        let mut file = match crate::private::create_new_private(&staged) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&staged)?;
                crate::private::create_new_private(&staged)?
            }
            other => other?,
        };
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&staged, &path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written
}

#[cfg(test)]
mod claude_account_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn native_auth_signed_out_fixture_child() {
        let Some(home) = std::env::var_os("MINDPLAYER_AUTH_ENV_FIXTURE") else {
            return;
        };
        let home = PathBuf::from(home);
        for provider in [Agent::Codex, Agent::Claude] {
            let account = Account::isolated(&home, provider, "synthetic").unwrap();
            assert_eq!(signed_out(&account, &home), Some(true));
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_auth_signed_out_checks_remove_service_environment() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        for (program, marker) in [
            ("codex", "not logged in"),
            ("claude", "\"loggedIn\": false"),
        ] {
            let path = home.path().join(program);
            std::fs::write(
                &path,
                format!("#!/bin/sh\n/usr/bin/env > \"$HOME/{program}.env\"\nprintf '%s\\n' '{marker}'\n"),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "accounts::claude_account_tests::native_auth_signed_out_fixture_child",
                "--test-threads=1",
            ])
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", home.path())
            .env("MINDPLAYER_AUTH_ENV_FIXTURE", home.path());
        for key in [
            "AI_GATEWAY_API_KEY",
            "TYPESAFE_API_KEY",
            "VERCEL_OIDC_TOKEN",
            "MINDPLAYER_LOCAL_ROUTER_TOKEN",
            "MINDPLAYER_ROUTER_AVAILABILITY",
            "MINDPLAYER_ROUTER_STATUS",
            "MINDPLAYER_CLAUDE_JEV_URL",
            "MINDPLAYER_CLAUDE_JEV_TOKEN",
            "MINDPLAYER_PER_PROMPT_CHOICE",
            "MINDPLAYER_JEV_DEFAULT",
        ] {
            command.env(key, "synthetic-service-canary");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let leaked: Vec<_> = ["codex.env", "claude.env"]
            .into_iter()
            .filter(|file| {
                std::fs::read_to_string(home.path().join(file))
                    .unwrap()
                    .contains("synthetic-service-canary")
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "service environment leaked to {leaked:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_auth_service_environment_stays_out_of_claude_but_reaches_codex_router() {
        let home = tempfile::tempdir().unwrap();
        for provider in [Agent::Codex, Agent::Claude] {
            for account in [
                Account::inherited(provider),
                Account::isolated(home.path(), provider, "synthetic").unwrap(),
            ] {
                let mut command = std::process::Command::new("/usr/bin/env");
                command.env_clear();
                for key in [
                    "AI_GATEWAY_API_KEY",
                    "TYPESAFE_API_KEY",
                    "VERCEL_OIDC_TOKEN",
                    "MINDPLAYER_LOCAL_ROUTER_TOKEN",
                    "MINDPLAYER_ROUTER_AVAILABILITY",
                    "MINDPLAYER_ROUTER_STATUS",
                    "MINDPLAYER_CLAUDE_JEV_URL",
                    "MINDPLAYER_CLAUDE_JEV_TOKEN",
                    "MINDPLAYER_PER_PROMPT_CHOICE",
                    "MINDPLAYER_JEV_DEFAULT",
                ] {
                    command.env(key, "synthetic-service-canary");
                }
                let env = account.launch_env();
                for key in &env.unset {
                    command.env_remove(key);
                }
                command.envs(env.set);
                let output = command.output().unwrap();
                assert!(output.status.success());
                let environment = String::from_utf8(output.stdout).unwrap();
                let count = environment.matches("=synthetic-service-canary").count();
                assert_eq!(
                    count,
                    if provider == Agent::Codex { 10 } else { 0 },
                    "{provider:?} {:?} service environment",
                    account.slot
                );
            }
        }
    }

    #[test]
    fn claude_identity_survives_an_account_file_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let account: Account = serde_json::from_value(serde_json::json!({
            "provider": "claude",
            "name": "default",
            "slot": {"kind": "inherited"},
            "claude_identity": "claude-user-opaque",
            "codex_identity": "preserve-other-binding"
        }))
        .unwrap();
        save_accounts(home.path(), &[account]).unwrap();
        let restored = load_accounts(home.path())
            .into_iter()
            .find(|account| account.provider == Agent::Claude)
            .unwrap();
        let stored = serde_json::to_value(restored).unwrap();
        assert_eq!(stored["claude_identity"], "claude-user-opaque");
        assert_eq!(stored["codex_identity"], "preserve-other-binding");
    }

    #[test]
    fn old_accounts_remain_unbound_without_changing_codex_bindings() {
        for provider in ["codex", "claude"] {
            let account: Account = serde_json::from_value(serde_json::json!({
                "provider": provider,
                "name": "default",
                "slot": {"kind": "inherited"},
                "codex_identity": "old-codex-binding"
            }))
            .unwrap();
            let stored = serde_json::to_value(account).unwrap();
            assert_eq!(stored["codex_identity"], "old-codex-binding");
            assert!(stored.get("claude_identity").is_none());
        }
    }

    #[test]
    fn generic_binding_accessors_select_only_the_accounts_provider() {
        for provider in MULTI_ACCOUNT_AGENTS {
            let mut account = Account::inherited(provider);
            account.codex_identity = Some("old-codex".into());
            account.claude_identity = Some("old-claude".into());
            let expected = match provider {
                Agent::Codex => Some("old-codex"),
                Agent::Claude => Some("old-claude"),
                _ => None,
            };
            assert_eq!(account.identity_fingerprint(), expected);

            account.set_identity_fingerprint(Some("new-binding".into()));
            if matches!(provider, Agent::Codex | Agent::Claude) {
                assert_eq!(account.identity_fingerprint(), Some("new-binding"));
            } else {
                assert_eq!(account.identity_fingerprint(), None);
            }
            if provider != Agent::Codex {
                assert_eq!(account.codex_identity.as_deref(), Some("old-codex"));
            }
            if provider != Agent::Claude {
                assert_eq!(account.claude_identity.as_deref(), Some("old-claude"));
            }
            account.set_identity_fingerprint(None);
            assert_eq!(account.identity_fingerprint(), None);
        }
    }

    #[test]
    fn constructors_and_legacy_files_start_without_an_inferred_binding() {
        let home = tempfile::tempdir().unwrap();
        for provider in [Agent::Codex, Agent::Claude] {
            let legacy: Account = serde_json::from_value(serde_json::json!({
                "provider": provider.as_str(),
                "name": "arbitrary-label",
                "slot": {"kind": "inherited"}
            }))
            .unwrap();
            for account in [
                Account::inherited(provider),
                Account::isolated(home.path(), provider, "arbitrary-label").unwrap(),
                legacy,
            ] {
                assert_eq!(account.identity_fingerprint(), None);
                assert_eq!(account.codex_identity, None);
                assert_eq!(account.claude_identity, None);
            }
        }
    }

    #[test]
    fn an_unmatched_claude_session_retains_its_registered_binding() {
        let home = tempfile::tempdir().unwrap();
        let mut inherited = Account::inherited(Agent::Claude);
        inherited.set_identity_fingerprint(Some("claude-user-bound".into()));
        let owner = owner_of(
            &[inherited],
            Agent::Claude,
            Path::new("/another/store/session.jsonl"),
            home.path(),
        );
        assert_eq!(owner.identity_fingerprint(), Some("claude-user-bound"));
    }

    #[test]
    fn inherited_claude_preserves_the_unhashed_native_default_vault() {
        let env = Account::inherited(Agent::Claude).launch_env();
        assert!(env.set.is_empty());
        for key in ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"] {
            assert!(env.unset.iter().any(|name| name == key), "{key} survived");
        }
    }

    #[test]
    fn both_claude_slots_clear_ambient_auth_and_gateway_routing() {
        let home = tempfile::tempdir().unwrap();
        let inherited = Account::inherited(Agent::Claude);
        let isolated = Account::isolated(home.path(), Agent::Claude, "other").unwrap();
        for account in [inherited, isolated] {
            let env = account.launch_env();
            for key in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
                "CLAUDE_CODE_OAUTH_SCOPES",
                "ANTHROPIC_CUSTOM_HEADERS",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_CODE_USE_ANTHROPIC_AWS",
                "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
                "CLAUDE_CODE_USE_MANTLE",
            ] {
                assert!(env.unset.iter().any(|name| name == key), "{key} survived");
            }
            assert!(!env.set.iter().any(|(name, _)| name == "HOME"));
        }
    }
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
    fn inherited_codex_is_pinned_to_the_default_home() {
        let account = Account::inherited(Agent::Codex);
        let env = account.launch_env();
        let expected = std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".codex")
                .to_string_lossy()
                .into_owned()
        });
        assert_eq!(
            env.set,
            expected
                .map(|path| vec![("CODEX_HOME".into(), path)])
                .unwrap_or_default()
        );
        assert!(env.unset.contains(&"CODEX_API_KEY".to_string()));
    }

    #[test]
    fn inherited_kiro_and_cursor_accounts_keep_their_ambient_login() {
        for agent in [Agent::Kiro, Agent::Cursor] {
            assert!(
                Account::inherited(agent).launch_env().is_empty(),
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
    fn identity_binding_round_trips_and_old_account_files_load_unverified() {
        let home = tmp();
        let mut account = Account::inherited(Agent::Codex);
        account.codex_identity = Some("codex-user-opaque".into());
        save_accounts(&home, &[account.clone()]).unwrap();
        let restored = load_accounts(&home)
            .into_iter()
            .find(|entry| entry.provider == Agent::Codex)
            .unwrap();
        assert_eq!(
            restored.codex_identity.as_deref(),
            Some("codex-user-opaque")
        );

        std::fs::write(
            accounts_path(&home),
            r#"[{"provider":"codex","name":"default","slot":{"kind":"inherited"},"role":"primary","disabled":false}]"#,
        )
        .unwrap();
        let legacy = load_accounts(&home)
            .into_iter()
            .find(|entry| entry.provider == Agent::Codex)
            .unwrap();
        assert_eq!(legacy.codex_identity, None);
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
        assert_eq!(
            owner.launch_env(),
            Account::inherited(Agent::Claude).launch_env()
        );
    }

    #[test]
    fn an_unmatched_codex_session_keeps_the_registered_inherited_identity() {
        let home = tmp();
        let mut inherited = Account::inherited(Agent::Codex);
        inherited.codex_identity = Some("codex-user-bound".into());
        let accounts = vec![inherited];
        let session = PathBuf::from("/an/overridden/codex/store/rollout.jsonl");

        let owner = owner_of(&accounts, Agent::Codex, &session, &home);

        assert_eq!(owner.codex_identity.as_deref(), Some("codex-user-bound"));
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
            codex_identity: None,
            claude_identity: None,
        };
        let inner = Account {
            provider: Agent::Codex,
            name: "inner".into(),
            slot: Slot::Isolated {
                path: home.join("slots/sessions/deep"),
            },
            role: Role::Primary,
            disabled: false,
            codex_identity: None,
            claude_identity: None,
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

#[cfg(test)]
mod fresh_home_reservation_tests {
    use super::*;
    use std::fs;

    #[test]
    fn registered_names_and_renamed_absent_paths_are_reserved() {
        let home = tempfile::tempdir().unwrap();
        for provider in MULTI_ACCOUNT_AGENTS {
            let mut named = Account::isolated(home.path(), provider, "original").unwrap();
            named.name = format!("{}-2", provider.as_str());
            let mut renamed =
                Account::isolated(home.path(), provider, &format!("{}-3", provider.as_str()))
                    .unwrap();
            renamed.name = "renamed".into();
            renamed.disabled = true;
            let registered = [named, renamed];

            let fresh = reserve_fresh_account(home.path(), provider, &registered).unwrap();

            assert_eq!(fresh.name, format!("{}-4", provider.as_str()));
            assert!(fresh.slot_path().is_dir());
            assert!(!slot_dir(home.path(), provider, &registered[0].name).exists());
            for account in &registered {
                assert!(!account.slot_path().exists(), "reserved homes stay absent");
            }
        }
    }

    #[test]
    fn a_renamed_present_home_keeps_its_sentinel() {
        let home = tempfile::tempdir().unwrap();
        let mut renamed = Account::isolated(home.path(), Agent::Codex, "codex-2").unwrap();
        renamed.name = "renamed".into();
        crate::private::create_dir_private(&renamed.slot_path()).unwrap();
        let sentinel = renamed.slot_path().join("sentinel");
        fs::write(&sentinel, b"synthetic existing login").unwrap();

        let fresh =
            reserve_fresh_account(home.path(), Agent::Codex, std::slice::from_ref(&renamed))
                .unwrap();

        assert_eq!(fresh.name, "codex-3");
        assert_ne!(fresh.slot_path(), renamed.slot_path());
        assert_eq!(fs::read(sentinel).unwrap(), b"synthetic existing login");
        assert_eq!(fs::read_dir(fresh.slot_path()).unwrap().count(), 0);
    }

    #[test]
    fn a_path_registered_to_another_provider_is_also_reserved() {
        let home = tempfile::tempdir().unwrap();
        let mut registered = Account::isolated(home.path(), Agent::Codex, "codex-2").unwrap();
        registered.provider = Agent::Claude;
        registered.name = "elsewhere".into();

        let fresh =
            reserve_fresh_account(home.path(), Agent::Codex, std::slice::from_ref(&registered))
                .unwrap();

        assert_eq!(fresh.name, "codex-3");
        assert!(!registered.slot_path().exists());
    }

    #[test]
    fn orphan_directories_and_files_are_skipped_without_changing_them() {
        let home = tempfile::tempdir().unwrap();
        let orphan = slot_dir(home.path(), Agent::Codex, "codex-2");
        crate::private::create_dir_private(&orphan).unwrap();
        fs::write(orphan.join("sentinel"), b"synthetic orphan login").unwrap();
        let occupied_file = slot_dir(home.path(), Agent::Codex, "codex-3");
        fs::write(&occupied_file, b"occupied file").unwrap();

        let fresh = reserve_fresh_account(home.path(), Agent::Codex, &[]).unwrap();

        assert_eq!(fresh.name, "codex-4");
        assert_eq!(
            fs::read(orphan.join("sentinel")).unwrap(),
            b"synthetic orphan login"
        );
        assert_eq!(fs::read(occupied_file).unwrap(), b"occupied file");
        assert_eq!(fs::read_dir(fresh.slot_path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn live_and_dangling_symlinks_are_skipped_without_following_them() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let home = tempfile::tempdir().unwrap();
        let parent = accounts_dir(home.path(), Agent::Codex);
        crate::private::create_dir_private(&parent).unwrap();
        let target = home.path().join("sentinel-home");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(target.join("sentinel"), b"synthetic symlink target").unwrap();
        let live = parent.join("codex-2");
        let dangling = parent.join("codex-3");
        let absent_target = home.path().join("absent-target");
        symlink(&target, &live).unwrap();
        symlink(&absent_target, &dangling).unwrap();

        let fresh = reserve_fresh_account(home.path(), Agent::Codex, &[]).unwrap();

        assert_eq!(fresh.name, "codex-4");
        assert_eq!(fs::read_link(live).unwrap(), target);
        assert_eq!(fs::read_link(dangling).unwrap(), absent_target);
        assert!(!absent_target.exists());
        assert_eq!(
            fs::read(target.join("sentinel")).unwrap(),
            b"synthetic symlink target"
        );
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(fs::read_dir(fresh.slot_path()).unwrap().count(), 0);
    }

    #[test]
    fn concurrent_allocations_claim_distinct_empty_homes() {
        use std::collections::HashSet;
        use std::sync::Barrier;

        let home = tempfile::tempdir().unwrap();
        let ready = Barrier::new(8);
        let accounts = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        ready.wait();
                        reserve_fresh_account(home.path(), Agent::Codex, &[]).unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });

        let paths: HashSet<_> = accounts.iter().map(Account::slot_path).collect();
        let names: HashSet<_> = accounts.iter().map(|account| &account.name).collect();
        assert_eq!(paths.len(), 8);
        assert_eq!(names.len(), 8);
        for path in paths {
            assert!(path.is_dir());
            assert_eq!(fs::read_dir(path).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_homes_and_parents_are_private_and_existing_parents_keep_their_mode() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o755)).unwrap();

        let fresh = reserve_fresh_account(home.path(), Agent::Codex, &[]).unwrap();

        for path in [
            home.path().join(".mindplayer"),
            home.path().join(".mindplayer/accounts"),
            accounts_dir(home.path(), Agent::Codex),
            fresh.slot_path(),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(home.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn other_io_errors_are_returned_without_changing_the_blocker() {
        let home = tempfile::tempdir().unwrap();
        let blocker = home.path().join(".mindplayer");
        fs::write(&blocker, b"not a directory").unwrap();
        let expected = crate::private::create_dir_private(&accounts_dir(home.path(), Agent::Codex))
            .unwrap_err();

        let error = reserve_fresh_account(home.path(), Agent::Codex, &[]).unwrap_err();

        assert_eq!(error.kind(), expected.kind());
        assert_eq!(error.raw_os_error(), expected.raw_os_error());
        assert_eq!(fs::read(blocker).unwrap(), b"not a directory");
    }
}

/// Erasing a login deletes a directory whole, so what it will delete matters.
#[cfg(test)]
mod forget_login_tests {
    use super::*;

    fn home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mp-forget-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_accounts_own_home_is_deleted() {
        let home = home();
        let account = Account::isolated(&home, Agent::Codex, "spare").unwrap();
        let path = account.slot_path();
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("marker"), b"x").unwrap();

        forget_login(&account, &home).unwrap();
        assert!(!path.exists(), "the account's home survived: {path:?}");
    }

    #[test]
    fn the_machines_own_login_is_refused() {
        let home = home();
        let err = forget_login(&Account::inherited(Agent::Codex), &home).unwrap_err();
        assert!(err.contains("not MindPlayer's to delete"), "{err}");
    }

    #[test]
    fn a_renamed_account_still_owns_its_original_home() {
        let home = home();
        let mut account = Account::isolated(&home, Agent::Codex, "before-rename").unwrap();
        let path = account.slot_path();
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("auth.json"), "test login").unwrap();
        account.name = "after-rename".into();
        forget_login(&account, &home).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_provider_root_is_never_an_account_home() {
        let home = home();
        let root = accounts_dir(&home, Agent::Codex);
        std::fs::create_dir_all(&root).unwrap();
        let mut account = Account::isolated(&home, Agent::Codex, "spare").unwrap();
        account.slot = Slot::Isolated { path: root.clone() };
        assert!(forget_login(&account, &home).is_err());
        assert!(root.exists());
    }

    /// The path is checked against where MindPlayer puts slots rather than
    /// trusted, because this deletes a directory and everything under it.
    #[test]
    fn a_home_somewhere_else_is_refused_and_left_alone() {
        let home = home();
        let elsewhere = home.join("not-ours");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let account = Account {
            provider: Agent::Codex,
            name: "spare".into(),
            slot: Slot::Isolated {
                path: elsewhere.clone(),
            },
            role: Role::Primary,
            disabled: false,
            codex_identity: None,
            claude_identity: None,
        };

        let err = forget_login(&account, &home).unwrap_err();
        assert!(err.contains("not one MindPlayer made"), "{err}");
        assert!(
            elsewhere.exists(),
            "a directory outside the slots was deleted"
        );
    }

    #[test]
    fn a_home_that_is_already_gone_is_not_an_error() {
        let home = home();
        let account = Account::isolated(&home, Agent::Codex, "never-made").unwrap();
        forget_login(&account, &home).unwrap();
    }
}

/// Several MindPlayers share one accounts file, so a save keeps the others' edits and a reader never takes a broken file for an empty list.
#[cfg(test)]
mod shared_file_tests {
    use super::*;

    fn slot(home: &Path, name: &str) -> Account {
        Account::isolated(home, Agent::Codex, name).unwrap()
    }

    fn names(accounts: &[Account]) -> Vec<&str> {
        accounts.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn a_save_keeps_what_another_mindplayer_saved_since() {
        let home = tempfile::tempdir().unwrap();
        let base = accounts_from_saved(vec![slot(home.path(), "a"), slot(home.path(), "c")]);
        // This one renames `a` and removes `c`; the other one adds `b` meanwhile.
        let mut ours = base.clone();
        ours.iter_mut().find(|x| x.name == "a").unwrap().name = "a-renamed".into();
        ours.retain(|x| x.name != "c");
        let mut theirs = base.clone();
        theirs.push(slot(home.path(), "b"));

        let merged = merge_accounts(&base, &ours, &theirs);
        let merged = names(&merged);
        assert!(merged.contains(&"a-renamed"), "{merged:?}");
        assert!(
            merged.contains(&"b"),
            "the other MindPlayer's account was dropped: {merged:?}"
        );
        assert!(
            !merged.contains(&"a") && !merged.contains(&"c"),
            "{merged:?}"
        );
    }

    #[test]
    fn an_account_the_other_side_changed_keeps_that_change() {
        let home = tempfile::tempdir().unwrap();
        let base = accounts_from_saved(vec![slot(home.path(), "a"), slot(home.path(), "b")]);
        let mut ours = base.clone();
        ours.iter_mut().find(|x| x.name == "a").unwrap().disabled = true;
        let mut theirs = base.clone();
        theirs.iter_mut().find(|x| x.name == "b").unwrap().name = "b-renamed".into();

        let merged = merge_accounts(&base, &ours, &theirs);
        assert!(
            merged.iter().any(|x| x.name == "a" && x.disabled),
            "{merged:?}"
        );
        assert!(merged.iter().any(|x| x.name == "b-renamed"), "{merged:?}");
        assert!(!merged.iter().any(|x| x.name == "b"), "{merged:?}");
    }

    #[test]
    fn making_one_primary_here_wins_over_the_saved_primary() {
        let home = tempfile::tempdir().unwrap();
        let mut a = slot(home.path(), "a");
        a.role = Role::Fallback;
        let base = accounts_from_saved(vec![a]);
        let mut ours = base.clone();
        for account in ours.iter_mut().filter(|x| x.provider == Agent::Codex) {
            account.role = if account.name == "a" {
                Role::Primary
            } else {
                Role::Fallback
            };
        }
        let merged = merge_accounts(&base, &ours, &base);
        let primary: Vec<&str> = merged
            .iter()
            .filter(|x| x.provider == Agent::Codex && x.role == Role::Primary)
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(primary, vec!["a"]);
    }

    fn codex_primaries(accounts: &[Account]) -> Vec<&str> {
        accounts
            .iter()
            .filter(|x| x.provider == Agent::Codex && x.role == Role::Primary)
            .map(|x| x.name.as_str())
            .collect()
    }

    #[test]
    fn fields_the_other_side_changed_survive_an_edit_here() {
        let home = tempfile::tempdir().unwrap();
        let base = accounts_from_saved(vec![slot(home.path(), "k")]);
        let mut ours = base.clone();
        ours.iter_mut().find(|x| x.name == "k").unwrap().disabled = true;
        let mut theirs = base.clone();
        let bound = theirs.iter_mut().find(|x| x.name == "k").unwrap();
        bound.name = "k2".into();
        bound.codex_identity = Some("codex-user-bound".into());

        let merged = merge_accounts(&base, &ours, &theirs);
        let k = merged
            .iter()
            .find(|x| x.slot == slot(home.path(), "k").slot)
            .unwrap();
        assert_eq!(k.name, "k2", "{merged:?}");
        assert_eq!(k.codex_identity.as_deref(), Some("codex-user-bound"));
        assert!(k.disabled);
    }

    #[test]
    fn turning_off_the_old_primary_keeps_their_new_primary() {
        let home = tempfile::tempdir().unwrap();
        let mut y = slot(home.path(), "y");
        y.role = Role::Fallback;
        let base = accounts_from_saved(vec![y]);
        let mut ours = base.clone();
        ours.iter_mut()
            .find(|x| x.name == DEFAULT_ACCOUNT && x.provider == Agent::Codex)
            .unwrap()
            .disabled = true;
        let mut theirs = base.clone();
        for x in theirs.iter_mut().filter(|x| x.provider == Agent::Codex) {
            x.role = if x.name == "y" {
                Role::Primary
            } else {
                Role::Fallback
            };
        }

        let merged = merge_accounts(&base, &ours, &theirs);
        assert_eq!(codex_primaries(&merged), vec!["y"], "{merged:?}");
    }

    #[test]
    fn promoting_an_account_they_removed_leaves_their_primary() {
        let home = tempfile::tempdir().unwrap();
        let mut x = slot(home.path(), "x");
        x.role = Role::Fallback;
        let base = accounts_from_saved(vec![x]);
        let mut ours = base.clone();
        for a in ours.iter_mut().filter(|a| a.provider == Agent::Codex) {
            a.role = if a.name == "x" {
                Role::Primary
            } else {
                Role::Fallback
            };
        }
        let theirs: Vec<Account> = base.iter().filter(|a| a.name != "x").cloned().collect();

        let merged = merge_accounts(&base, &ours, &theirs);
        assert_eq!(
            codex_primaries(&merged),
            vec![DEFAULT_ACCOUNT],
            "{merged:?}"
        );
    }

    #[test]
    fn a_broken_file_is_not_read_as_an_empty_list() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read_saved_accounts(home.path()), None);
        std::fs::create_dir_all(home.path().join(".mindplayer")).unwrap();
        std::fs::write(accounts_path(home.path()), "[{\"provider\":\"codex\",\"na").unwrap();
        assert_eq!(read_saved_accounts(home.path()), None);
    }

    #[test]
    fn a_save_replaces_the_file_whole_and_owner_only() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".mindplayer")).unwrap();
        std::fs::write(accounts_path(home.path()), "[]").unwrap();
        save_accounts(home.path(), &[slot(home.path(), "whole")]).unwrap();
        let read = read_saved_accounts(home.path()).expect("a whole file reads");
        assert!(read.iter().any(|a| a.name == "whole"));
        assert_eq!(read, accounts_from_saved(vec![slot(home.path(), "whole")]));
        let leftovers: Vec<_> = std::fs::read_dir(home.path().join(".mindplayer"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the staged file was left behind: {leftovers:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(accounts_path(home.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
