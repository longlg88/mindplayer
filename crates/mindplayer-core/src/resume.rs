//! Build the exact command used to resume or start a session.
//!
//! Verified invocations:
//! - Codex resume:  `codex resume <uuid>`            (run in the session cwd)
//! - Claude resume: `claude --resume <id>`           (run in the session cwd)
//! - Kiro resume:   `kiro-cli chat --resume-id <id> --trust-all-tools --v3`
//! - Cursor resume: `agent --resume <chatId>`        (run in the session cwd)
//! - New session:   `codex` / `claude` / `kiro-cli chat --trust-all-tools --v3` / `agent`

use crate::accounts::{Account, LaunchEnv};
use crate::session::{Agent, Session};
use std::path::PathBuf;

/// A spawnable command: program, args, the directory to run it in, and the
/// environment that decides which login the CLI runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Empty for the login already on this machine, which is reached by
    /// changing nothing.
    pub env: LaunchEnv,
}

/// Command to resume an existing session, launched in its original `cwd`.
///
/// `account` must be the one whose store holds this session: its transcript
/// lives under that account's home, so resuming on another account finds
/// nothing to resume.
pub fn resume(session: &Session, account: &Account) -> Command {
    let id = session.id.clone();
    let args = match session.agent {
        Agent::Codex => vec!["resume".to_string(), id],
        Agent::Claude => vec!["--resume".to_string(), id],
        Agent::Kiro => {
            let mut args = vec!["chat".to_string(), "--resume-id".to_string(), id];
            args.extend(KIRO_FLAGS.iter().map(|f| f.to_string()));
            args
        }
        Agent::Cursor => vec!["--resume".to_string(), id],
    };
    Command {
        program: session.agent.program().to_string(),
        args,
        cwd: session.cwd.clone(),
        env: account.launch_env(),
    }
}

/// Kiro launches every session with every tool pre-trusted (user-requested: no
/// per-tool / MCP approval prompts, ever) and on the v3 agent engine — resume,
/// a brand-new session, and a handoff target all go through
/// `resume()`/`new_session()` below, so these cover every way a kiro session
/// can start.
///
/// `--v3` is the CLI's own shorthand for `--agent-engine v3`; v2 is its
/// default. A v3 session records itself as `sess_<id>.history` rather than the
/// `<id>.json` sidecar discovery collects, so sessions started this way do not
/// appear in the list — accepted deliberately (2026-09-16).
const KIRO_FLAGS: [&str; 2] = ["--trust-all-tools", "--v3"];

/// Command to start a brand new session in `cwd`, on `account`'s login.
pub fn new_session(agent: Agent, cwd: PathBuf, account: &Account) -> Command {
    let args = match agent {
        // Kiro's chat lives under a subcommand; codex/claude launch bare.
        Agent::Kiro => {
            let mut args = vec!["chat".to_string()];
            args.extend(KIRO_FLAGS.iter().map(|f| f.to_string()));
            args
        }
        Agent::Codex | Agent::Claude | Agent::Cursor => Vec::new(),
    };
    Command {
        program: agent.program().to_string(),
        args,
        cwd,
        env: account.launch_env(),
    }
}

/// Command that signs `account` in, run in its own pane.
///
/// Signing in is the provider's own flow — a browser round trip for most of
/// them — so it runs as a pane rather than something MindPlayer reimplements.
/// Taken from each CLI's own `--help`: `codex login`, `claude auth login`,
/// `kiro-cli user login`, `agent login`.
pub fn login(agent: Agent, cwd: PathBuf, account: &Account) -> Command {
    let args = match agent {
        Agent::Codex => vec!["login".to_string()],
        Agent::Claude => vec!["auth".to_string(), "login".to_string()],
        Agent::Kiro => vec!["user".to_string(), "login".to_string()],
        Agent::Cursor => vec!["login".to_string()],
    };
    Command {
        program: agent.program().to_string(),
        args,
        cwd,
        env: account.launch_env(),
    }
}

/// The words each CLI uses to sign out, from its own `--help`: `codex logout`,
/// `claude auth logout`, `kiro-cli user logout`, `agent logout`.
fn logout_args(agent: Agent) -> Vec<String> {
    match agent {
        Agent::Codex => vec!["logout".to_string()],
        Agent::Claude => vec!["auth".to_string(), "logout".to_string()],
        Agent::Kiro => vec!["user".to_string(), "logout".to_string()],
        Agent::Cursor => vec!["logout".to_string()],
    }
}

/// What to do in the browser before a sign-in, so it asks which account.
///
/// Signing a slot out does not sign the browser out, and a browser still signed
/// in approves that account without asking — which is how two slots ended up on
/// one account. The site is named only where it is known: Kiro signs in through
/// an organisation's own start URL, so it gets the general wording.
pub fn browser_hint(agent: Agent) -> String {
    let site = match agent {
        Agent::Codex => Some("chatgpt.com"),
        Agent::Claude => Some("claude.ai"),
        Agent::Cursor => Some("cursor.com"),
        Agent::Kiro => None,
    };
    let what = site.map_or_else(
        || format!("{} in your browser", agent.as_str()),
        |site| format!("{site} in your browser"),
    );
    format!(
        "Before signing in: sign out of {what}, or open the link in a private window. \
         A browser still signed in approves that account without asking."
    )
}

/// Command that signs `account` out and straight back in, run in its own pane.
///
/// A slot already holding a sign-in does not change hands on `login` alone, so
/// swapping which account a slot runs on meant signing out by hand first. The
/// sign-out is why this refuses the login the machine already had: that one is
/// not MindPlayer's to clear.
///
/// Signing out does not sign the browser out, so the provider's page may
/// approve the same account again without asking. This offers the chance to
/// choose; it cannot make the choice.
///
/// # Errors
/// Returns the reason when `account` is the login this machine came with.
pub fn relogin(cwd: PathBuf, account: &Account) -> Result<Command, String> {
    if account.is_inherited() {
        return Err("the login this machine came with is signed out where you made it".into());
    }
    let agent = account.provider;
    let program = agent.program();
    // One pane, two commands: the shell runs them in order and `exec` hands the
    // pane to the sign-in, so the pane is the sign-in rather than its parent.
    // Printed first so it sits right above the sign-in link; the hint has no single quote to escape.
    let line = format!(
        "printf '%s\\n\\n' '{}'; {} {} >/dev/null 2>&1; exec {} {}",
        browser_hint(agent),
        program,
        logout_args(agent).join(" "),
        program,
        login(agent, cwd.clone(), account).args.join(" ")
    );
    Ok(Command {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), line],
        cwd,
        env: account.launch_env(),
    })
}

/// Command that signs a fresh home in and then starts a session there, all in
/// one pane.
///
/// This is how a new session reaches another account without touching the
/// homes other panes are running on: nothing is signed out, the new home simply
/// holds whichever account the sign-in lands on. If the sign-in is abandoned the
/// session never starts, because the two are joined with `&&`.
pub fn login_then_start(cwd: PathBuf, account: &Account) -> Command {
    let agent = account.provider;
    let program = agent.program();
    let login_args = login(agent, cwd.clone(), account).args.join(" ");
    let start_args = new_session(agent, cwd.clone(), account).args.join(" ");
    // Printed first so it sits right above the sign-in link; the hint has no single quote to escape.
    let line = format!(
        "printf '%s\\n\\n' '{}'; {} {} && exec {} {}",
        browser_hint(agent),
        program,
        login_args,
        program,
        start_args
    );
    Command {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), line.trim_end().to_string()],
        cwd,
        env: account.launch_env(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Slot;
    use crate::session::TokenUsage;

    fn session(agent: Agent, id: &str, cwd: &str) -> Session {
        Session {
            id: id.into(),
            agent,
            cwd: PathBuf::from(cwd),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: TokenUsage::default(),
            title: String::new(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    #[test]
    fn codex_resume_uses_uuid_and_cwd() {
        let c = resume(
            &session(Agent::Codex, "uuid-1", "/work"),
            &Account::inherited(Agent::Codex),
        );
        assert_eq!(c.program, "codex");
        assert_eq!(c.args, vec!["resume", "uuid-1"]);
        assert_eq!(c.cwd, PathBuf::from("/work"));
    }

    #[test]
    fn claude_resume_uses_flag() {
        let c = resume(
            &session(Agent::Claude, "sid-2", "/work"),
            &Account::inherited(Agent::Claude),
        );
        assert_eq!(c.program, "claude");
        assert_eq!(c.args, vec!["--resume", "sid-2"]);
    }

    #[test]
    fn new_session_has_no_args() {
        let c = new_session(
            Agent::Codex,
            PathBuf::from("/here"),
            &Account::inherited(Agent::Codex),
        );
        assert_eq!(c.program, "codex");
        assert!(c.args.is_empty());
        assert_eq!(c.cwd, PathBuf::from("/here"));
    }

    #[test]
    fn kiro_resume_uses_resume_id_and_cli_binary() {
        let c = resume(
            &session(Agent::Kiro, "kid-3", "/work"),
            &Account::inherited(Agent::Kiro),
        );
        assert_eq!(c.program, "kiro-cli");
        assert_eq!(
            c.args,
            vec!["chat", "--resume-id", "kid-3", "--trust-all-tools", "--v3"],
            "resuming a kiro session must stay in trust mode — no per-tool/MCP approval prompts"
        );
        assert_eq!(c.cwd, PathBuf::from("/work"));
    }

    #[test]
    fn kiro_new_session_launches_chat() {
        let c = new_session(
            Agent::Kiro,
            PathBuf::from("/here"),
            &Account::inherited(Agent::Kiro),
        );
        assert_eq!(c.program, "kiro-cli");
        assert_eq!(
            c.args,
            vec!["chat", "--trust-all-tools", "--v3"],
            "a brand-new kiro session must start in trust mode too"
        );
    }

    /// Every way a kiro session can start goes through one of these two, so
    /// the engine cannot be set on some paths and not others.
    #[test]
    fn every_kiro_launch_carries_the_same_flags() {
        let account = Account::inherited(Agent::Kiro);
        let started = new_session(Agent::Kiro, PathBuf::from("/here"), &account);
        let resumed = resume(&session(Agent::Kiro, "kid-9", "/work"), &account);

        for (what, args) in [("new", &started.args), ("resume", &resumed.args)] {
            for flag in KIRO_FLAGS {
                assert!(
                    args.contains(&flag.to_string()),
                    "a {what} kiro session launched without {flag}: {args:?}"
                );
            }
        }
        assert!(
            KIRO_FLAGS.contains(&"--v3"),
            "the engine flag is what this pins; without it these assertions say nothing"
        );
    }

    #[test]
    fn cursor_resume_uses_agent_resume_flag() {
        let c = resume(
            &session(Agent::Cursor, "cursor-chat-4", "/work"),
            &Account::inherited(Agent::Cursor),
        );
        assert_eq!(c.program, "agent");
        assert_eq!(c.args, vec!["--resume", "cursor-chat-4"]);
        assert_eq!(c.cwd, PathBuf::from("/work"));
    }

    #[test]
    fn cursor_new_session_launches_bare_agent() {
        let c = new_session(
            Agent::Cursor,
            PathBuf::from("/here"),
            &Account::inherited(Agent::Cursor),
        );
        assert_eq!(c.program, "agent");
        assert!(c.args.is_empty());
    }

    #[test]
    fn signing_in_happens_inside_the_slot_it_is_for() {
        let home = PathBuf::from("/tmp/mp-login-test");
        let account = Account::isolated(&home, Agent::Claude, "second").unwrap();
        let c = login(Agent::Claude, PathBuf::from("/here"), &account);
        assert_eq!(c.program, "claude");
        assert_eq!(c.args, vec!["auth", "login"]);
        assert_eq!(
            c.env,
            account.launch_env(),
            "signing in without the slot's environment would overwrite the existing login"
        );
    }

    #[test]
    fn each_cli_is_signed_in_with_its_own_words() {
        let home = PathBuf::from("/tmp/mp-login-test");
        let expected = [
            (Agent::Codex, "codex", vec!["login"]),
            (Agent::Kiro, "kiro-cli", vec!["user", "login"]),
            (Agent::Cursor, "agent", vec!["login"]),
        ];
        for (agent, program, args) in expected {
            let c = login(agent, PathBuf::from("/here"), &Account::inherited(agent));
            assert_eq!(c.program, program);
            assert_eq!(c.args, args, "{} login args", agent.as_str());
        }
        let _ = home;
    }

    #[test]
    fn the_existing_non_codex_login_launches_with_no_environment_change() {
        for agent in [Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let c = new_session(agent, PathBuf::from("/here"), &Account::inherited(agent));
            assert!(
                c.env.is_empty(),
                "{} gained an env it did not need",
                agent.as_str()
            );
        }
    }

    #[test]
    fn the_existing_codex_login_is_pinned_to_the_default_home() {
        let c = new_session(
            Agent::Codex,
            PathBuf::from("/here"),
            &Account::inherited(Agent::Codex),
        );
        assert_eq!(
            c.env.set.first().map(|(key, _)| key.as_str()),
            Some("CODEX_HOME")
        );
    }

    #[test]
    fn a_named_account_rides_along_with_the_command() {
        let home = PathBuf::from("/tmp/mp-resume-test");
        let account = Account::isolated(&home, Agent::Codex, "overflow").unwrap();
        let Slot::Isolated { path } = &account.slot else {
            panic!("an isolated account lost its slot");
        };

        let started = new_session(Agent::Codex, PathBuf::from("/here"), &account);
        assert_eq!(
            started.env.set,
            vec![(
                "CODEX_HOME".to_string(),
                path.to_string_lossy().into_owned()
            )]
        );

        let resumed = resume(&session(Agent::Codex, "uuid-9", "/work"), &account);
        assert_eq!(
            resumed.env, started.env,
            "resuming must land on the same login that started the session"
        );
    }

    /// The hint is wrapped in single quotes for the shell, so it must never
    /// carry one of its own.
    #[test]
    fn every_browser_hint_is_safe_inside_single_quotes() {
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let hint = browser_hint(agent);
            assert!(
                !hint.contains('\''),
                "{} hint would break the shell line: {hint}",
                agent.as_str()
            );
        }
    }

    /// A site is named only where it is known; Kiro signs in through an
    /// organisation's own start URL.
    #[test]
    fn the_hint_names_a_site_only_where_one_is_known() {
        assert!(browser_hint(Agent::Codex).contains("chatgpt.com"));
        assert!(browser_hint(Agent::Claude).contains("claude.ai"));
        assert!(browser_hint(Agent::Cursor).contains("cursor.com"));
        let kiro = browser_hint(Agent::Kiro);
        assert!(!kiro.contains(".com") && !kiro.contains(".ai"), "{kiro}");
    }

    /// Runs the real line against a stand-in `codex` on the child's own PATH, so
    /// nothing on this machine is signed in or out.
    #[cfg(unix)]
    fn run_with_stub(stub: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "mp-login-stub-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("codex");
        std::fs::write(&bin, format!("#!/bin/sh\n{stub}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let account = Account::isolated(&dir, Agent::Codex, "probe").unwrap();
        let command = login_then_start(dir.clone(), &account);
        assert_eq!(command.program, "sh");
        let out = std::process::Command::new("sh")
            .args(&command.args)
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn the_hint_comes_first_then_the_sign_in_then_the_session() {
        let out = run_with_stub(
            r#"if [ "$1" = login ]; then echo "stub login"; else echo "stub start $*"; fi"#,
        );
        let hint = out.find("chatgpt.com").expect("the hint was never printed");
        let login = out.find("stub login").expect("the sign-in never ran");
        let start = out.find("stub start").expect("the session never started");
        assert!(hint < login && login < start, "wrong order:\n{out}");
    }

    /// Abandoning the sign-in must not leave a session running on no account.
    #[cfg(unix)]
    #[test]
    fn a_sign_in_that_does_not_finish_starts_nothing() {
        let out = run_with_stub(
            r#"if [ "$1" = login ]; then echo "stub login"; exit 1; fi; echo "stub start""#,
        );
        assert!(out.contains("stub login"), "{out}");
        assert!(
            !out.contains("stub start"),
            "a session started although the sign-in failed:\n{out}"
        );
    }
}
