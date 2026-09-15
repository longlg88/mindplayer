//! Build the exact command used to resume or start a session.
//!
//! Verified invocations:
//! - Codex resume:  `codex resume <uuid>`            (run in the session cwd)
//! - Claude resume: `claude --resume <id>`           (run in the session cwd)
//! - Kiro resume:   `kiro-cli chat --resume-id <id>` (run in the session cwd)
//! - Cursor resume: `agent --resume <chatId>`        (run in the session cwd)
//! - New session:   `codex` / `claude` / `kiro-cli chat` / `agent`

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
        Agent::Kiro => vec![
            "chat".to_string(),
            "--resume-id".to_string(),
            id,
            KIRO_TRUST_FLAG.to_string(),
        ],
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
/// per-tool / MCP approval prompts, ever) — resume, a brand-new session, and a
/// handoff target all go through `resume()`/`new_session()` below, so this one
/// flag covers every way a kiro session can start.
const KIRO_TRUST_FLAG: &str = "--trust-all-tools";

/// Command to start a brand new session in `cwd`, on `account`'s login.
pub fn new_session(agent: Agent, cwd: PathBuf, account: &Account) -> Command {
    let args = match agent {
        // Kiro's chat lives under a subcommand; codex/claude launch bare.
        Agent::Kiro => vec!["chat".to_string(), KIRO_TRUST_FLAG.to_string()],
        Agent::Codex | Agent::Claude | Agent::Cursor => Vec::new(),
    };
    Command {
        program: agent.program().to_string(),
        args,
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
            vec!["chat", "--resume-id", "kid-3", "--trust-all-tools"],
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
            vec!["chat", "--trust-all-tools"],
            "a brand-new kiro session must start in trust mode too"
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
    fn the_existing_login_launches_with_no_environment_change() {
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let c = new_session(agent, PathBuf::from("/here"), &Account::inherited(agent));
            assert!(
                c.env.is_empty(),
                "{} gained an env it did not need",
                agent.as_str()
            );
        }
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
}
