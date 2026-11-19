//! Per-launch, token-free observations from the native OMP credential attempt.

use mindplayer_core::resume::Command;
use serde::Deserialize;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const EXTENSION: &str = include_str!("omp_account.js");
const SCHEMA: &str = "mindplayer.omp-account.v1";
// Shared with the extension; a larger file is never parsed or retained.
const MAX_BYTES: usize = 8192;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_METADATA_BYTES: usize = 512;
const MAX_CREDENTIAL_ID: u64 = 9_007_199_254_740_991;
const FILE_ENV: &str = "MINDPLAYER_OMP_ACCOUNT_FILE";
const LAUNCH_ENV: &str = "MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID";

#[derive(Debug)]
pub(crate) struct OmpAccount {
    directory: PathBuf,
    file: PathBuf,
    launch_id: String,
    previous: Vec<u8>,
    label: Option<String>,
}

impl OmpAccount {
    /// Install a private observer without changing picker or native CLI selection.
    pub(crate) fn prepare(command: &mut Command) -> io::Result<Option<Self>> {
        Self::prepare_in(command, &std::env::temp_dir())
    }

    fn prepare_in(command: &mut Command, root: &Path) -> io::Result<Option<Self>> {
        if command.program != "omp" {
            return Ok(None);
        }
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let (directory, launch_id) = loop {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let launch_id = format!("{}-{stamp}-{id}", std::process::id());
            let directory = root.join(format!("mindplayer-omp-account-{launch_id}"));
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => break (directory, launch_id),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        // Own the directory before any fallible work so every early return cleans up.
        let tracker = Self {
            file: directory.join("account.json"),
            directory,
            launch_id,
            previous: Vec::new(),
            label: None,
        };
        let extension = tracker.directory.join("omp_account.js");
        let extension_arg = utf8_path(&extension)?.to_owned();
        let file_env = utf8_path(&tracker.file)?.to_owned();
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&extension)?
            .write_all(EXTENSION.as_bytes())?;

        command.args.extend(["-e".to_owned(), extension_arg]);
        command
            .env
            .set
            .retain(|(key, _)| key != FILE_ENV && key != LAUNCH_ENV);
        command.env.set.extend([
            (FILE_ENV.to_owned(), file_env),
            (LAUNCH_ENV.to_owned(), tracker.launch_id.clone()),
        ]);
        Ok(Some(tracker))
    }

    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Called only by the application's bounded, interval-gated pane poll.
    pub(crate) fn poll(&mut self) -> bool {
        let mut bytes = [0_u8; MAX_BYTES + 1];
        let read = self.read_report(&mut bytes);
        let next = match read {
            Ok(len) => {
                let bytes = &bytes[..len];
                if bytes == self.previous {
                    return false;
                }
                self.previous.clear();
                self.previous.extend_from_slice(bytes);
                observed_label(bytes, &self.launch_id)
            }
            Err(_) => {
                self.previous.clear();
                None
            }
        };
        if next == self.label {
            return false;
        }
        self.label = next;
        true
    }

    fn read_report(&self, bytes: &mut [u8; MAX_BYTES + 1]) -> io::Result<usize> {
        let metadata = fs::symlink_metadata(&self.file)?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
            return Err(io::Error::other("invalid OMP account report size or type"));
        }
        let mut file = fs::File::open(&self.file)?;
        let mut len = 0;
        while len < bytes.len() {
            let count = file.read(&mut bytes[len..])?;
            if count == 0 {
                return Ok(len);
            }
            len += count;
        }
        Err(io::Error::other("OMP account report exceeds size limit"))
    }
}

impl Drop for OmpAccount {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn utf8_path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "OMP account path is not UTF-8"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Report {
    schema: String,
    launch_id: String,
    session_id: String,
    state: State,
    provider: Option<String>,
    model: Option<String>,
    credential_id: Option<u64>,
    kind: Option<Kind>,
    email: Option<String>,
    account_id: Option<String>,
    org_id: Option<String>,
    org_name: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum State {
    Observed,
    Unresolved,
    Unsupported,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Oauth,
    ApiKey,
    Credential,
}

fn observed_label(bytes: &[u8], launch_id: &str) -> Option<String> {
    let report: Report = serde_json::from_slice(bytes).ok()?;
    if report.schema != SCHEMA
        || report.launch_id != launch_id
        || !identifier(&report.session_id)
        || !matches!(report.state, State::Observed)
        || [
            &report.email,
            &report.account_id,
            &report.org_id,
            &report.org_name,
            &report.reason,
        ]
        .into_iter()
        .flatten()
        .any(|value| value.len() > MAX_METADATA_BYTES)
    {
        return None;
    }
    let provider = report.provider.as_deref().filter(|s| identifier(s))?;
    report.model.as_deref().filter(|s| identifier(s))?;
    let id = report
        .credential_id
        .filter(|id| (1..=MAX_CREDENTIAL_ID).contains(id))?;
    let provider = provider.trim();
    let identity = match report.kind.unwrap_or(Kind::Credential) {
        Kind::ApiKey => format!("API key #{id}"),
        Kind::Credential => format!("credential #{id}"),
        Kind::Oauth => {
            let account = report
                .email
                .as_deref()
                .and_then(public_text)
                .or_else(|| report.account_id.as_deref().and_then(public_text));
            let workspace = report
                .org_name
                .as_deref()
                .and_then(public_text)
                .or_else(|| report.org_id.as_deref().and_then(public_text));
            match (account, workspace) {
                (Some(account), Some(workspace)) => format!("{account} · {workspace}"),
                (Some(identity), None) | (None, Some(identity)) => identity,
                (None, None) => format!("credential #{id}"),
            }
        }
    };
    Some(format!("{identity} · {provider}"))
}

fn identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && !value.chars().any(unsafe_character)
}

fn unsafe_character(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

fn public_text(value: &str) -> Option<String> {
    let mut text: String = value.chars().filter(|&c| !unsafe_character(c)).collect();
    text.truncate(text.trim_end().len());
    let leading = text.len() - text.trim_start().len();
    text.drain(..leading);
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;

    fn command() -> Command {
        Command {
            program: "omp".into(),
            args: vec!["--resume".into(), "/work/native-session.jsonl".into()],
            cwd: PathBuf::from("/work"),
            env: Default::default(),
        }
    }

    fn tracker() -> OmpAccount {
        OmpAccount::prepare(&mut command()).unwrap().unwrap()
    }

    fn report(tracker: &OmpAccount) -> Value {
        json!({
            "schema": SCHEMA,
            "launchId": tracker.launch_id,
            "sessionId": "native-session-id",
            "state": "observed",
            "provider": "anthropic",
            "model": "native-model",
            "credentialId": 7,
            "kind": "oauth",
            "email": "person@example.test",
            "orgName": "Workspace"
        })
    }

    fn publish(tracker: &OmpAccount, report: &Value) {
        fs::write(&tracker.file, serde_json::to_vec(report).unwrap()).unwrap();
    }

    #[test]
    fn launches_are_private_isolated_and_removed_on_drop() {
        let mut first = tracker();
        let mut second = tracker();
        assert_ne!(first.directory, second.directory);
        assert_ne!(first.launch_id, second.launch_id);
        assert_eq!(
            fs::metadata(&first.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(first.directory.join("omp_account.js"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let first_report = report(&first);
        publish(&first, &first_report);
        assert!(first.poll());
        publish(&second, &first_report);
        assert!(!second.poll());
        assert_eq!(second.label(), None);
        publish(&second, &report(&second));
        assert!(second.poll());
        let directory = first.directory.clone();
        drop(first);
        assert!(!directory.exists());
        assert!(second.directory.exists());
    }

    #[test]
    fn rotation_unknown_and_missing_reports_replace_cached_identity() {
        let mut tracker = tracker();
        let mut value = report(&tracker);
        publish(&tracker, &value);
        assert!(tracker.poll());
        assert_eq!(
            tracker.label(),
            Some("person@example.test · Workspace · anthropic")
        );
        assert!(!tracker.poll());
        value["provider"] = json!("openai");
        value["credentialId"] = json!(12);
        value["kind"] = json!("api_key");
        publish(&tracker, &value);
        assert!(tracker.poll());
        assert_eq!(tracker.label(), Some("API key #12 · openai"));
        value["state"] = json!("unresolved");
        publish(&tracker, &value);
        assert!(tracker.poll());
        assert_eq!(tracker.label(), None);
        value["state"] = json!("observed");
        value["kind"] = json!("credential");
        publish(&tracker, &value);
        assert!(tracker.poll());
        assert_eq!(tracker.label(), Some("credential #12 · openai"));
        fs::remove_file(&tracker.file).unwrap();
        assert!(tracker.poll());
        assert_eq!(tracker.label(), None);
        publish(&tracker, &value);
        assert!(tracker.poll());
        value["state"] = json!("unsupported");
        publish(&tracker, &value);
        assert!(tracker.poll());
        assert_eq!(tracker.label(), None);
    }

    #[test]
    fn invalid_metadata_clears_a_previously_observed_label() {
        let mut tracker = tracker();
        let valid = report(&tracker);
        for (field, invalid) in [
            ("launchId", json!("another-launch")),
            ("schema", json!("another-schema")),
            ("state", json!("active")),
            ("sessionId", json!("")),
            ("sessionId", json!("s".repeat(MAX_IDENTIFIER_BYTES + 1))),
            ("provider", json!("")),
            ("provider", json!("anthropic\nspoof")),
            ("model", Value::Null),
            ("credentialId", json!(0)),
            ("credentialId", json!(-1)),
            ("credentialId", json!(1.5)),
            ("credentialId", json!(MAX_CREDENTIAL_ID + 1)),
            ("email", json!("x".repeat(MAX_METADATA_BYTES + 1))),
            ("orgName", json!(42)),
            ("kind", json!("invented")),
            ("accessToken", json!("must-not-be-accepted")),
        ] {
            publish(&tracker, &valid);
            assert!(tracker.poll(), "restore before {field}");
            let mut invalid_report = valid.clone();
            invalid_report[field] = invalid;
            publish(&tracker, &invalid_report);
            assert!(tracker.poll(), "clear after {field}");
            assert_eq!(tracker.label(), None, "invalid {field}");
        }
        for malformed in [b"not json".to_vec(), vec![b' '; MAX_BYTES + 1]] {
            publish(&tracker, &valid);
            assert!(tracker.poll());
            fs::write(&tracker.file, malformed).unwrap();
            assert!(tracker.poll());
            assert_eq!(tracker.label(), None);
        }
    }

    #[test]
    fn public_labels_remove_terminal_controls_and_bidi_overrides() {
        let tracker = tracker();
        let mut value = report(&tracker);
        value["email"] = json!("person\u{202e}@example.test\n\u{001b}");
        value["orgName"] = json!("Work\u{2066}space\r");
        assert_eq!(
            observed_label(&serde_json::to_vec(&value).unwrap(), &tracker.launch_id),
            Some("person@example.test · Workspace · anthropic".into())
        );
    }

    #[test]
    fn non_omp_launches_do_not_require_an_observer_directory() {
        let fixture = tracker();
        for program in ["codex", "claude", "agent", "kiro-cli", "sh"] {
            let mut command = command();
            command.program = program.into();
            let before = command.clone();
            assert!(
                OmpAccount::prepare_in(&mut command, &fixture.directory.join("missing"))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(command, before);
        }
    }

    #[test]
    fn failed_preparation_does_not_mutate_the_command() {
        let fixture = tracker();
        let root = fixture.directory.join("missing");
        let mut command = command();
        let before = command.clone();
        assert!(OmpAccount::prepare_in(&mut command, &root).is_err());
        assert_eq!(command, before);
        assert!(!root.exists());
    }

    // APFS rejects this fixture pathname before preparation can run.
    #[cfg(target_os = "linux")]
    #[test]
    fn failed_preparation_leaves_no_directory_or_command_mutation() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = tracker();
        let root = fixture
            .directory
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        fs::create_dir(&root).unwrap();
        let mut command = command();
        let before = command.clone();
        assert!(OmpAccount::prepare_in(&mut command, &root).is_err());
        assert_eq!(command, before);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert!(OmpAccount::prepare_in(&mut command, &root.join("missing")).is_err());
        assert_eq!(command, before);
    }
}
