use crate::app::SessionStatus;
use serde_json::Value;
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const READ_LIMIT: usize = 128 * 1024;
const BUFFER_LIMIT: usize = 256 * 1024;

/// Incrementally follows the active Codex JSONL session without replaying its history.
#[derive(Clone)]
pub struct TurnStatusReader {
    file: PathBuf,
    identity: Option<FileIdentity>,
    offset: u64,
    partial: Vec<u8>,
    status: Option<SessionStatus>,
    turn_id: Option<String>,
    allow_initial_start: bool,
    discarding: bool,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
}

impl TurnStatusReader {
    /// Start at EOF. Existing session history must not complete a newly spawned pane.
    pub fn new(file: PathBuf) -> Self {
        Self::with_start(file, false)
    }

    /// Start at byte zero for a synthetic session that has just discovered its real file.
    pub fn new_from_start(file: PathBuf) -> Self {
        Self::with_start(file, true)
    }

    /// A newly discovered source must not replay turns preceding local input
    /// or interruption that happened while its pane still had a synthetic ID.
    pub fn new_from_start_since(file: PathBuf, cutoff: chrono::DateTime<chrono::Utc>) -> Self {
        let mut reader = Self::new_from_start(file);
        reader.cutoff = Some(cutoff);
        reader
    }

    fn with_start(file: PathBuf, from_start: bool) -> Self {
        let (identity, offset) = fs::metadata(&file)
            .map(|metadata| {
                (
                    Some(FileIdentity::from(&metadata)),
                    if from_start { 0 } else { metadata.len() },
                )
            })
            .unwrap_or((None, 0));
        Self {
            file,
            identity,
            offset,
            partial: Vec::new(),
            status: None,
            turn_id: None,
            allow_initial_start: from_start || identity.is_none(),
            discarding: false,
            cutoff: None,
        }
    }

    /// Read at most one bounded tail chunk and return whether the visible status changed.
    pub fn poll(&mut self) -> bool {
        let metadata = match fs::metadata(&self.file) {
            Ok(metadata) => metadata,
            Err(_) => {
                let changed = self.status.is_some();
                let allow_initial_start = self.identity.is_none() && self.allow_initial_start;
                self.identity = None;
                self.offset = 0;
                self.partial.clear();
                self.status = None;
                self.turn_id = None;
                self.allow_initial_start = allow_initial_start;
                self.discarding = false;
                return changed;
            }
        };
        let identity = FileIdentity::from(&metadata);
        if self.identity != Some(identity) || metadata.len() < self.offset {
            let first_discovery = self.identity.is_none() && self.allow_initial_start;
            let changed = self.status.is_some();
            self.identity = Some(identity);
            self.offset = if first_discovery { 0 } else { metadata.len() };
            self.partial.clear();
            self.status = None;
            self.turn_id = None;
            self.discarding = false;
            self.allow_initial_start = false;
            if !first_discovery {
                return changed;
            }
        }
        if metadata.len() == self.offset {
            return false;
        }

        let Ok(mut file) = File::open(&self.file) else {
            return false;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let to_read = (metadata.len() - self.offset).min(READ_LIMIT as u64) as usize;
        let mut bytes = vec![0; to_read];
        let Ok(read) = file.read(&mut bytes) else {
            return false;
        };
        self.offset += read as u64;
        self.partial.extend_from_slice(&bytes[..read]);

        let mut changed = false;
        while let Some(newline) = self.partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=newline).collect();
            if !self.discarding && newline <= BUFFER_LIMIT {
                changed |= self.consume_line(&line[..newline]);
            }
            self.discarding = false;
        }
        if self.partial.len() > BUFFER_LIMIT {
            self.partial.clear();
            self.discarding = true;
        }
        changed
    }

    /// Forget the current turn at a new local submission or interruption.
    pub fn reset_at_end(&mut self) {
        self.status = None;
        self.turn_id = None;
        self.partial.clear();
        self.discarding = false;
        match fs::metadata(&self.file) {
            Ok(metadata) => {
                self.identity = Some(FileIdentity::from(&metadata));
                self.offset = metadata.len();
            }
            Err(_) => {
                self.identity = None;
                self.offset = 0;
            }
        }
    }

    pub fn status(&self) -> Option<SessionStatus> {
        self.status
    }

    /// The source path used by this reader, for reconciling updated Session paths.
    pub fn file(&self) -> &Path {
        &self.file
    }

    fn consume_line(&mut self, line: &[u8]) -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return false;
        };
        let Some(object) = value.as_object() else {
            return false;
        };
        if object.get("type").and_then(Value::as_str) != Some("event_msg") {
            return false;
        }
        if let Some(cutoff) = self.cutoff {
            let timestamp = object
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
            if timestamp.is_none_or(|t| t.timestamp_millis() < cutoff.timestamp_millis()) {
                return false;
            }
        }
        let Some(payload) = object.get("payload").and_then(Value::as_object) else {
            return false;
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("task_started") => {
                let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) else {
                    return false;
                };
                self.turn_id = Some(turn_id.to_owned());
                self.set_status(SessionStatus::Working)
            }
            Some("turn_aborted") => {
                self.turn_id = None;
                self.set_status(SessionStatus::Idle)
            }
            Some("task_complete") => {
                let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) else {
                    return false;
                };
                if self.turn_id.as_deref() != Some(turn_id) {
                    return false;
                }
                if bad_completion(payload) {
                    self.turn_id = None;
                    return self.set_status(SessionStatus::Idle);
                }
                self.set_status(SessionStatus::Completed)
            }
            _ => false,
        }
    }

    fn set_status(&mut self, status: SessionStatus) -> bool {
        let changed = self.status != Some(status);
        self.status = Some(status);
        changed
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl From<&Metadata> for FileIdentity {
    fn from(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                modified: metadata.modified().ok(),
            }
        }
    }
}

fn bad_completion(object: &serde_json::Map<String, Value>) -> bool {
    bad_fields(object)
        || object
            .get("payload")
            .and_then(Value::as_object)
            .is_some_and(bad_fields)
}

fn bad_fields(object: &serde_json::Map<String, Value>) -> bool {
    ["status", "error"]
        .iter()
        .any(|key| object.get(*key).is_some_and(contains_bad_marker))
}

fn contains_bad_marker(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            let value = value.to_ascii_lowercase();
            ["failed", "canceled", "aborted"]
                .iter()
                .any(|bad| value.contains(bad))
        }
        Value::Array(values) => values.iter().any(contains_bad_marker),
        Value::Object(values) => values.values().any(contains_bad_marker),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mindplayer-turn-status-{name}-{stamp}.jsonl"))
    }

    fn append(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn starts_at_eof_and_tracks_matching_turn() {
        let path = temp_file("lifecycle");
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"old"}}
"#,
        );
        let mut reader = TurnStatusReader::new(path.clone());
        assert_eq!(reader.status(), None);
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"new"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"old"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"new"}}
"#,
        );
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Completed));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn ignores_unpaired_bad_and_non_task_events() {
        let path = temp_file("guards");
        let mut reader = TurnStatusReader::new(path.clone());
        append(
            &path,
            r#"{"type":"assistant","text":"task_started"}
{"type":"task_complete","turn_id":"x"}
"#,
        );
        assert!(!reader.poll());
        assert_eq!(reader.status(), None);
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"x"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"x","status":"failed"}}
"#,
        );
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Idle));
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}
"#,
        );
        assert!(!reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Idle));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn preserves_partial_lines_and_resets_on_submission() {
        let path = temp_file("partial");
        let mut reader = TurnStatusReader::new(path.clone());
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"x"}}"#,
        );
        assert!(!reader.poll());
        append(&path, "\n");
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Working));
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"x"}}
"#,
        );
        reader.reset_at_end();
        assert_eq!(reader.status(), None);
        assert!(!reader.poll());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn truncation_clears_old_turn_and_starts_from_new_eof() {
        let path = temp_file("truncate");
        let mut reader = TurnStatusReader::new(path.clone());
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"x"}}
"#,
        );
        assert!(reader.poll());
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(0).unwrap();
        assert!(reader.poll());
        assert_eq!(reader.status(), None);
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"x"}}
"#,
        );
        assert!(!reader.poll());
        assert_eq!(reader.status(), None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_file_invalidates_visible_status() {
        let path = temp_file("missing");
        let mut reader = TurnStatusReader::new(path.clone());
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"x"}}
"#,
        );
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Working));
        fs::remove_file(&path).unwrap();
        assert!(reader.poll());
        assert_eq!(reader.status(), None);
    }

    #[test]
    fn deleted_then_restored_file_does_not_replay_old_history() {
        let path = temp_file("restored");
        let mut reader = TurnStatusReader::new(path.clone());
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"old"}}
"#,
        );
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Working));
        fs::remove_file(&path).unwrap();
        assert!(reader.poll());
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"old"}}
"#,
        );
        assert!(!reader.poll());
        assert_eq!(reader.status(), None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn newly_discovered_file_obeys_the_pre_adoption_input_boundary() {
        let path = temp_file("adoption-cutoff");
        append(
            &path,
            r#"{"timestamp":"2026-10-12T10:00:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"old"}}
{"timestamp":"2026-10-12T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"old"}}
"#,
        );
        let cutoff = "2026-10-12T10:00:01Z".parse().unwrap();
        let mut reader = TurnStatusReader::new_from_start_since(path.clone(), cutoff);
        assert!(!reader.poll());
        assert_eq!(
            reader.status(),
            None,
            "old turn must not finish after interruption/adoption"
        );
        append(
            &path,
            r#"{"timestamp":"2026-10-12T10:00:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"new"}}
{"timestamp":"2026-10-12T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"new"}}
"#,
        );
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Completed));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn new_from_start_reads_a_synthetic_session_file() {
        let path = temp_file("from-start");
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"real"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"real"}}
"#,
        );
        let mut reader = TurnStatusReader::new_from_start(path.clone());
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Completed));
        assert_eq!(reader.file(), path.as_path());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn embedded_event_json_and_partial_utf8_are_not_falsely_parsed() {
        let path = temp_file("utf8");
        let mut reader = TurnStatusReader::new(path.clone());
        append(
            &path,
            r#"{"type":"assistant","text":"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"fake\"}}"}
"#,
        );
        assert!(!reader.poll());
        let line =
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"é\"}}\n";
        let split = line.find('é').unwrap() + 1;
        append_bytes(&path, &line.as_bytes()[..split]);
        assert!(!reader.poll());
        append_bytes(&path, &line.as_bytes()[split..]);
        assert!(reader.poll());
        assert_eq!(reader.status(), Some(SessionStatus::Working));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn oversized_fragment_is_discarded_until_newline() {
        let path = temp_file("oversized");
        let mut reader = TurnStatusReader::new(path.clone());
        let oversized = format!(
            "{{\"type\":\"assistant\",\"text\":\"{}\"}}\n",
            "x".repeat(BUFFER_LIMIT + 1)
        );
        append(&path, &oversized);
        for _ in 0..4 {
            reader.poll();
        }
        append(
            &path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"after"}}
"#,
        );
        for _ in 0..3 {
            reader.poll();
        }
        assert_eq!(reader.status(), Some(SessionStatus::Working));
        fs::remove_file(path).unwrap();
    }

    fn append_bytes(path: &Path, bytes: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }
}
