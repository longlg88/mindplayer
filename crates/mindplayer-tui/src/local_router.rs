//! Native launch preparation and legacy routing-environment sanitization.

use anyhow::{bail, Result};
use mindplayer_core::Command;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
const AVAILABILITY_ENV: &str = "MINDPLAYER_ROUTER_AVAILABILITY";
const STATUS_ENV: &str = "MINDPLAYER_ROUTER_STATUS";
const PER_PROMPT_ENV: &str = "MINDPLAYER_PER_PROMPT_CHOICE";
/// Set by the router only when a Jev credential loaded; a value inherited from MindPlayer's own environment is dropped.
const JEV_DEFAULT_ENV: &str = "MINDPLAYER_JEV_DEFAULT";
const ROUTING_ENV: &[&str] = &[
    "AI_GATEWAY_API_KEY",
    "TYPESAFE_API_KEY",
    "VERCEL_OIDC_TOKEN",
    "MINDPLAYER_LOCAL_ROUTER_TOKEN",
    "MINDPLAYER_CLAUDE_JEV_URL",
    "MINDPLAYER_CLAUDE_JEV_TOKEN",
    AVAILABILITY_ENV,
    STATUS_ENV,
    PER_PROMPT_ENV,
    JEV_DEFAULT_ENV,
];

/// Keep native programs unresolved until spawn, preserving account and arguments.
/// This also prepares commands before login_then_start_with builds its shell.
pub(crate) fn prepare(command: &Command) -> Result<Command> {
    reject_legacy_router_launcher(command)?;
    // OMP owns its provider credentials, including gateway keys.
    if command.program == "omp" {
        return prepare_omp_at(command, &mindplayer_core::private::data_root());
    }
    Ok(native_command(command))
}

/// A new OMP pane must be new even when the user's native config enables
/// autoResume. A per-process overlay leaves the user's configuration intact.
fn prepare_omp_at(command: &Command, root: &Path) -> Result<Command> {
    if command.args != ["-e", mindplayer_core::resume::OMP_PICKER_MARKER] {
        return Ok(command.clone());
    }
    const CONTENT: &[u8] = b"autoResume: false\n";
    mindplayer_core::private::create_dir_private(root)?;
    let config = root.join("omp-new-session-v1.yml");
    if !config.exists() {
        let stage = root.join(format!(
            ".omp-new-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = mindplayer_core::private::create_new_private(&stage)?;
        let result = (|| -> Result<()> {
            file.write_all(CONTENT)?;
            file.sync_all()?;
            match fs::hard_link(&stage, &config) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error.into()),
            }
        })();
        let _ = fs::remove_file(&stage);
        result?;
    }
    if !fs::symlink_metadata(&config)?.file_type().is_file() || fs::read(&config)? != CONTENT {
        bail!("OMP new-session configuration is invalid");
    }
    let mut prepared = command.clone();
    prepared
        .args
        .extend(["--config".into(), config.to_string_lossy().into_owned()]);
    Ok(prepared)
}

fn native_command(command: &Command) -> Command {
    let mut native = command.clone();
    native
        .env
        .set
        .retain(|(key, _)| !ROUTING_ENV.contains(&key.as_str()));
    for key in ROUTING_ENV {
        if !native.env.unset.iter().any(|unset| unset.as_str() == *key) {
            native.env.unset.push((*key).into());
        }
    }
    native
}

// The core's generated login shell has one prepared exec after native login.
// Inspect that command only; prompt arguments after it remain opaque.
fn login_exec(command: &Command) -> Option<&str> {
    (command.program == "sh" && command.args.len() == 2 && command.args[0] == "-c")
        .then(|| {
            command.args[1]
                .split_once(" && exec ")
                .map(|(_, exec)| exec)
        })
        .flatten()
}

// Decode only words emitted by shell_word; never evaluate the shell or inspect
// a prompt argument as syntax. Paths may contain spaces, quotes and flag text.
fn first_shell_word(input: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in input.char_indices() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\'' {
            quoted = !quoted;
        } else if ch == '\\' && !quoted {
            escaped = true;
        } else if ch == ' ' && !quoted {
            return Some((word, &input[index + 1..]));
        } else {
            word.push(ch);
        }
    }
    (!quoted && !escaped && !word.is_empty()).then_some((word, ""))
}

fn reject_legacy_router_launcher(command: &Command) -> Result<()> {
    let direct = command.program == "node"
        && command.args.first().is_some_and(|script| {
            Path::new(script)
                .file_name()
                .is_some_and(|name| name == "claude-run.mjs" || name == "run.mjs")
        });
    let login = login_exec(command)
        .and_then(|exec| {
            let (mut program, mut rest) = first_shell_word(exec)?;
            if program == "env" {
                loop {
                    (program, rest) = first_shell_word(rest)?;
                    if !program.contains('=') {
                        break;
                    }
                }
            }
            (program == "node")
                .then(|| first_shell_word(rest))
                .flatten()
        })
        .is_some_and(|(script, _)| {
            Path::new(&script)
                .file_name()
                .is_some_and(|name| name == "claude-run.mjs" || name == "run.mjs")
        });
    if direct || login {
        bail!("Jev 실행기는 더 이상 지원하지 않습니다. 설치된 CLI로 세션을 다시 여세요.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mindplayer_core::{
        accounts::{Account, LaunchEnv},
        Agent,
    };
    use std::path::PathBuf;

    #[test]
    fn omp_new_session_uses_fresh_overlay_and_keeps_its_credentials() {
        let dir = std::env::temp_dir().join(format!(
            "mp-omp-new-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let account = Account::inherited(Agent::Omp);
        let mut command = mindplayer_core::new_session(Agent::Omp, dir.clone(), &account);
        command
            .env
            .set
            .push(("AI_GATEWAY_API_KEY".into(), "synthetic".into()));
        let prepared = prepare_omp_at(&command, &dir).unwrap();
        assert_eq!(prepared.program, "omp");
        assert_eq!(prepared.env, command.env);
        assert_eq!(&prepared.args[..2], &command.args);
        assert_eq!(prepared.args[2], "--config");
        assert_eq!(
            std::fs::read(&prepared.args[3]).unwrap(),
            b"autoResume: false\n"
        );
        assert_eq!(prepare_omp_at(&prepared, &dir).unwrap(), prepared);
        let mut resume = command.clone();
        resume.args = vec!["--resume".into(), "/tmp/example.jsonl".into()];
        assert_eq!(prepare_omp_at(&resume, &dir).unwrap(), resume);
        fs::remove_dir_all(dir).unwrap();
    }

    fn assert_native_commands() {
        let cwd: PathBuf = "/tmp/native project".into();
        let mut commands = Vec::new();
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let account = Account::inherited(agent);
            let start = mindplayer_core::new_session(agent, cwd.clone(), &account);
            commands.push(start.clone());
            commands.push(mindplayer_core::login(agent, cwd.clone(), &account));
            let prepared = prepare(&start).unwrap();
            let login = mindplayer_core::resume::login_then_start_with(&account, prepared);
            assert!(!login.args[1].contains("exec node "));
            commands.push(login);
        }
        for args in [
            vec!["resume", "thread-id", "--sandbox", "danger-full-access"],
            vec!["--ask-for-approval", "never", "handoff ' $ opaque prompt"],
            vec!["logout"],
            vec!["app-server"],
        ] {
            commands.push(Command {
                program: "codex".into(),
                args: args.into_iter().map(String::from).collect(),
                cwd: cwd.clone(),
                env: LaunchEnv::default(),
            });
        }
        for mut command in commands {
            command.env.set.extend([
                ("CODEX_HOME".into(), "/account/unchanged".into()),
                ("CLAUDE_CONFIG_DIR".into(), "/claude/unchanged".into()),
                ("OPENAI_API_KEY".into(), "native-account-key".into()),
                ("ANTHROPIC_API_KEY".into(), "native-provider-key".into()),
                ("MINDPLAYER_UNRELATED".into(), "keep".into()),
            ]);
            let clean_set = command.env.set.clone();
            // Duplicates must also disappear; these are dedicated to routing.
            for key in ROUTING_ENV.iter().chain(ROUTING_ENV) {
                command
                    .env
                    .set
                    .push(((*key).into(), "routing-canary".into()));
            }
            command.env.unset = vec!["UNRELATED".into(), STATUS_ENV.into()];
            let expected_unset: Vec<String> = ["UNRELATED", STATUS_ENV]
                .into_iter()
                .chain(ROUTING_ENV.iter().copied().filter(|key| *key != STATUS_ENV))
                .map(String::from)
                .collect();
            let original = command.clone();
            let prepared = prepare(&command).unwrap();
            assert_eq!(prepared.program, original.program);
            assert_eq!(prepared.args, original.args);
            assert_eq!(prepared.cwd, original.cwd);
            assert_eq!(prepared.env.set, clean_set);
            assert_eq!(prepared.env.unset, expected_unset);
            assert_eq!(
                prepare(&prepared).unwrap(),
                prepared,
                "native preparation must be idempotent"
            );
            assert_eq!(command, original, "the input command is never mutated");
        }
    }

    #[test]
    fn native_mode_needs_no_node_assets_private_ui_or_status_channel() {
        const CHILD_HOME: &str = "MP_NATIVE_BOUNDARY_TEST_HOME";
        if let Some(home) = std::env::var_os(CHILD_HOME) {
            let data_root = PathBuf::from(home).join(".mindplayer");
            assert_eq!(mindplayer_core::private::data_root(), data_root);
            assert!(!data_root.exists());
            assert_native_commands();
            assert!(
                !data_root.exists(),
                "Native must not stage or allocate anything"
            );
            return;
        }
        // Isolate HOME/PATH in a fresh test process, without racing getenv in
        // parallel tests or touching the real private UI/credentials/runtime.
        let root = std::env::temp_dir().join(format!(
            "mp-native-boundary-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_router::tests::native_mode_needs_no_node_assets_private_ui_or_status_channel",
                "--nocapture",
            ])
            .env("HOME", &root)
            .env("PATH", "")
            .env(CHILD_HOME, &root)
            .env("AI_GATEWAY_API_KEY", "ambient-routing-canary")
            .env("TYPESAFE_API_KEY", "ambient-routing-canary")
            .output();
        fs::remove_dir_all(root).unwrap();
        let output = result.unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn native_login_shells_keep_auth_arguments_and_scrub_routing_state() {
        for agent in [Agent::Codex, Agent::Claude] {
            let account = Account::inherited(agent);
            let start = mindplayer_core::new_session(agent, "/tmp/a folder".into(), &account);
            let start = prepare(&start).unwrap();
            let mut login = mindplayer_core::resume::login_then_start_with(&account, start);
            let original = login.clone();
            for key in ROUTING_ENV {
                login
                    .env
                    .set
                    .push(((*key).into(), "stale-routing-canary".into()));
            }
            let prepared = prepare(&login).unwrap();
            assert_eq!(prepared.program, original.program);
            assert_eq!(prepared.args, original.args);
            assert_eq!(prepared.cwd, original.cwd);
            assert_eq!(prepared.env.set, original.env.set);
            for key in ROUTING_ENV {
                assert!(prepared
                    .env
                    .unset
                    .iter()
                    .any(|unset| unset.as_str() == *key));
            }
        }
    }

    #[test]
    fn native_preparation_rejects_old_router_launchers_but_not_opaque_prompts() {
        for (agent, script) in [(Agent::Codex, "run.mjs"), (Agent::Claude, "claude-run.mjs")] {
            for directory in ["/old/runtime", "/old/a folder", "/old/'quoted"] {
                let old = Command {
                    program: "node".into(),
                    args: vec![format!("{directory}/{script}"), "--mindplayer".into()],
                    cwd: "/tmp/a folder".into(),
                    env: LaunchEnv::default(),
                };
                let login = mindplayer_core::resume::login_then_start_with(
                    &Account::inherited(agent),
                    old.clone(),
                );
                assert!(prepare(&old).is_err());
                assert!(prepare(&login).is_err());
                let native = Command {
                    program: agent.program().into(),
                    args: vec![format!("explain {script} --mindplayer --cwd /tmp")],
                    ..old
                };
                assert!(prepare(&native).is_ok());
                let login = mindplayer_core::resume::login_then_start_with(
                    &Account::inherited(agent),
                    native,
                );
                assert!(prepare(&login).is_ok());
            }
        }
    }
}
