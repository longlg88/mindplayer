//! Build the exact command used to resume or start a session.
//!
//! Verified invocations:
//! - Codex resume:  `codex resume <uuid>`            (run in the session cwd)
//! - Claude resume: `claude --resume <id>`           (run in the session cwd)
//! - Kiro resume:   `kiro-cli chat --resume-id <id>` (run in the session cwd)
//! - New session:   `codex` / `claude` / `kiro-cli chat` (run in the scope dir)

use crate::session::{Agent, Session};
use std::path::PathBuf;

/// A spawnable command: program, args, and the directory to run it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Command to resume an existing session, launched in its original `cwd`.
pub fn resume(session: &Session) -> Command {
    let id = session.id.clone();
    let args = match session.agent {
        Agent::Codex => vec!["resume".to_string(), id],
        Agent::Claude => vec!["--resume".to_string(), id],
        Agent::Kiro => vec!["chat".to_string(), "--resume-id".to_string(), id],
    };
    Command {
        program: session.agent.program().to_string(),
        args,
        cwd: session.cwd.clone(),
    }
}

/// Command to start a brand new session in `cwd`.
pub fn new_session(agent: Agent, cwd: PathBuf) -> Command {
    let args = match agent {
        // Kiro's chat lives under a subcommand; codex/claude launch bare.
        Agent::Kiro => vec!["chat".to_string()],
        Agent::Codex | Agent::Claude => Vec::new(),
    };
    Command {
        program: agent.program().to_string(),
        args,
        cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TokenUsage;

    fn session(agent: Agent, id: &str, cwd: &str) -> Session {
        Session {
            id: id.into(),
            agent,
            cwd: PathBuf::from(cwd),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            tokens: TokenUsage::default(),
            title: String::new(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    #[test]
    fn codex_resume_uses_uuid_and_cwd() {
        let c = resume(&session(Agent::Codex, "uuid-1", "/work"));
        assert_eq!(c.program, "codex");
        assert_eq!(c.args, vec!["resume", "uuid-1"]);
        assert_eq!(c.cwd, PathBuf::from("/work"));
    }

    #[test]
    fn claude_resume_uses_flag() {
        let c = resume(&session(Agent::Claude, "sid-2", "/work"));
        assert_eq!(c.program, "claude");
        assert_eq!(c.args, vec!["--resume", "sid-2"]);
    }

    #[test]
    fn new_session_has_no_args() {
        let c = new_session(Agent::Codex, PathBuf::from("/here"));
        assert_eq!(c.program, "codex");
        assert!(c.args.is_empty());
        assert_eq!(c.cwd, PathBuf::from("/here"));
    }

    #[test]
    fn kiro_resume_uses_resume_id_and_cli_binary() {
        let c = resume(&session(Agent::Kiro, "kid-3", "/work"));
        assert_eq!(c.program, "kiro-cli");
        assert_eq!(c.args, vec!["chat", "--resume-id", "kid-3"]);
        assert_eq!(c.cwd, PathBuf::from("/work"));
    }

    #[test]
    fn kiro_new_session_launches_chat() {
        let c = new_session(Agent::Kiro, PathBuf::from("/here"));
        assert_eq!(c.program, "kiro-cli");
        assert_eq!(c.args, vec!["chat"]);
    }
}
