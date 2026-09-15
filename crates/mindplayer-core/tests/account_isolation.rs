//! Pins the per-provider launch environment to what the CLIs actually do.
//!
//! The unit tests in `accounts.rs` can only check the table against itself. The
//! claim that matters is that a slot's environment makes a real CLI read a
//! different login, so these run the installed CLI twice — once untouched, once
//! with an empty slot's environment — and require the two answers to differ.
//!
//! A CLI that is not installed, or an inherited login that is signed out, skips
//! the case rather than failing: CI has neither.

use mindplayer_core::accounts::Account;
use mindplayer_core::session::Agent;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RUN_LIMIT: Duration = Duration::from_secs(20);

/// The command that reports which login a CLI is running on, and the marker its
/// answer carries when there is no login at all.
fn whoami(
    agent: Agent,
) -> (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
) {
    match agent {
        Agent::Codex => ("codex", &["login", "status"], &["not logged in"]),
        Agent::Claude => ("claude", &["auth", "status"], &["\"loggedin\": false"]),
        Agent::Kiro => ("kiro-cli", &["whoami"], &["not logged in"]),
        Agent::Cursor => ("agent", &["status"], &["not logged in"]),
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mp-isolation-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs a command to completion, killing it if it outlives the limit.
fn run(mut command: Command) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < RUN_LIMIT => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text.to_lowercase())
}

fn signed_out(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|m| text.contains(m))
}

#[test]
fn a_slot_environment_makes_each_cli_read_a_different_login() {
    let mut exercised = 0;
    for agent in mindplayer_core::accounts::MULTI_ACCOUNT_AGENTS {
        let (program, args, markers) = whoami(agent);
        if which(program).is_none() {
            eprintln!("skip {}: {program} is not installed", agent.as_str());
            continue;
        }

        let inherited = Account::inherited(agent);
        assert!(inherited.launch_env().is_empty());
        let Some(before) = run(base_command(program, args)) else {
            eprintln!("skip {}: {program} did not answer in time", agent.as_str());
            continue;
        };
        if signed_out(&before, markers) {
            eprintln!(
                "skip {}: this machine has no inherited login",
                agent.as_str()
            );
            continue;
        }

        let home = scratch(agent.as_str());
        let account = Account::isolated(&home, agent, "probe").unwrap();
        std::fs::create_dir_all(match &account.slot {
            mindplayer_core::accounts::Slot::Isolated { path } => path,
            mindplayer_core::accounts::Slot::Inherited => unreachable!(),
        })
        .unwrap();

        let mut command = base_command(program, args);
        let env = account.launch_env();
        for (key, value) in &env.set {
            command.env(key, value);
        }
        for key in &env.unset {
            command.env_remove(key);
        }
        let Some(after) = run(command) else {
            panic!("{program} answered untouched but hung under an isolated slot");
        };

        assert!(
            signed_out(&after, markers),
            "{}: an empty slot still reached a login, so the slot does not decide the account",
            agent.as_str()
        );
        exercised += 1;
        let _ = std::fs::remove_dir_all(&home);
    }
    eprintln!("verified isolation for {exercised} provider(s)");
}

/// The parent's environment is inherited on purpose: an ambient credential is
/// exactly what the slot has to overcome.
fn base_command(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}
