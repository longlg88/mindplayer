//! Isolated active/archive contracts. Every source and sidecar lives in a tempdir.

use chrono::{DateTime, Duration, Utc};
use mindplayer_core::catalog::{self, CatalogSnapshot};
use mindplayer_core::discovery::SessionRoot;
use mindplayer_core::{Agent, Scope, Session, State, TokenUsage};
use std::collections::HashSet;
use std::fs::{self, File, FileTimes};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::{tempdir, TempDir};

fn now() -> DateTime<Utc> {
    "2026-06-20T12:00:00Z".parse().unwrap()
}

fn row() -> Session {
    Session {
        id: "session-a".into(),
        agent: Agent::Codex,
        cwd: "/work/project".into(),
        file: PathBuf::new(),
        started_at: Some(now() - Duration::days(60)),
        last_active: Some(now()),
        last_prompt_at: Some(now() - Duration::days(30)),
        tokens: TokenUsage::default(),
        title: "original title".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    }
}

struct Store {
    dir: TempDir,
    root: SessionRoot,
    cache: PathBuf,
}

impl Store {
    fn new(agent: Agent) -> Self {
        let dir = tempdir().unwrap();
        let root = SessionRoot {
            agent,
            dir: dir.path().join("account-a/sessions"),
        };
        let cache = dir.path().join("cache/catalog.json");
        Self { dir, root, cache }
    }

    fn codex(&self, id: &str, prompt: DateTime<Utc>) -> PathBuf {
        let file = self.root.dir.join(format!("{id}.jsonl"));
        write_codex(&file, id, "/work/project", prompt);
        file
    }

    fn archived_source(&self, id: &str) -> PathBuf {
        let old = now() - Duration::days(30);
        let (path, records) = match self.root.agent {
            Agent::Codex => return self.codex(id, old),
            Agent::Claude => (
                self.root.dir.join(format!("-work-project/{id}.jsonl")),
                vec![serde_json::json!({"type":"user","sessionId":id,
                    "cwd":"/work/project","timestamp":old,
                    "message":{"content":"original title"}})],
            ),
            Agent::Kiro => (
                self.root.dir.join(format!("{id}.json")),
                vec![serde_json::json!({"session_id":id,"cwd":"/work/project",
                    "created_at":old,"title":"original title"})],
            ),
            Agent::Cursor => (
                self.root.dir.join(format!("workspace/{id}/meta.json")),
                vec![
                    serde_json::json!({"hasConversation":true,"cwd":"/work/project",
                    "createdAtMs":old.timestamp_millis(),"title":"original title"}),
                ],
            ),
            Agent::Omp => (
                self.root.dir.join(format!("bucket/{id}.jsonl")),
                vec![serde_json::json!({"type":"session","id":id,
                    "cwd":"/work/project","timestamp":old,"title":"original title"})],
            ),
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = records
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{text}\n")).unwrap();
        path
    }

    fn scan(&self, state: &State) -> CatalogSnapshot {
        catalog::scan(
            &Scope::Global,
            std::slice::from_ref(&self.root),
            state,
            &HashSet::new(),
            &self.cache,
            now(),
        )
        .unwrap()
    }
}

fn write_codex(file: &Path, id: &str, cwd: &str, prompt: DateTime<Utc>) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let lines = [
        serde_json::json!({"type":"session_meta", "payload":{
            "id":id,"cwd":cwd,"timestamp":now()-Duration::days(60)
        }}),
        serde_json::json!({"type":"response_item","timestamp":prompt,"payload":{
            "type":"message","role":"user","content":[{"type":"input_text","text":"original title"}]
        }}),
        serde_json::json!({"type":"event_msg","timestamp":now(),"payload":{
            "type":"token_count","info":{"total_token_usage":{
                "input_tokens":12,"output_tokens":3,"total_tokens":15
            }}
        }}),
    ];
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(file, format!("{text}\n")).unwrap();
}

#[test]
fn legacy_state_migrates_and_recorded_use_survives_restart() {
    let mut state: State = serde_json::from_str(
        r#"{"version":1,"archived":["session-a"],"labels":{"session-a":"named"}}"#,
    )
    .unwrap();
    assert_eq!(state.active_retention_days, 7);
    assert!(state.last_used.is_empty());
    state.record_use("session-a", now());
    state.record_use("session-a", now() - Duration::days(1));
    assert!(!state.is_archived("session-a"));
    assert_eq!(
        state.last_used["session-a"],
        now(),
        "late events cannot age use backwards"
    );
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    state.save_to(&path).unwrap();
    let loaded = State::load_from(&path);
    assert_eq!(loaded.last_used["session-a"], now());
    assert_eq!(loaded.label_for("session-a"), Some("named"));
    assert!(loaded.is_active_session(&row(), now(), &HashSet::new()));
}

#[test]
fn seven_day_boundary_uses_human_recency_and_never_mtime() {
    let mut state = State::default();
    let protected = HashSet::new();
    let mut session = row();
    session.last_prompt_at = Some(now() - Duration::days(7));
    assert!(!state.is_active_session(&session, now(), &protected));
    session.last_prompt_at = Some(now() - Duration::days(7) + Duration::milliseconds(1));
    assert!(state.is_active_session(&session, now(), &protected));
    session.last_prompt_at = Some(now() - Duration::days(30));
    session.started_at = Some(now());
    assert!(
        !state.is_active_session(&session, now(), &protected),
        "creation is only a fallback"
    );
    state.record_use(&session.id, now() - Duration::days(40));
    session.last_prompt_at = Some(now() - Duration::hours(1));
    assert!(
        state.is_active_session(&session, now(), &protected),
        "external human use wins"
    );
    session.last_prompt_at = None;
    state.last_used.clear();
    assert!(state.is_active_session(&session, now(), &protected));
    session.started_at = None;
    assert!(!state.is_active_session(&session, now(), &protected));
}

#[test]
fn running_protection_precedes_archive_and_archive_precedes_in_progress() {
    let mut state = State::default();
    let session = row();
    state.set_in_progress(&session.id, true);
    assert!(state.is_active_session(&session, now(), &HashSet::new()));
    state.set_archived(&session.id, true);
    assert!(!state.is_active_session(&session, now(), &HashSet::new()));
    assert!(state.is_active_session(&session, now(), &HashSet::from([session.id.clone()])));
    state.active_retention_days = 0;
    assert!(!state.is_active_session(&session, now(), &HashSet::new()));
    state.set_archived(&session.id, false);
    state.set_in_progress(&session.id, false);
    assert!(
        state.is_active_session(&session, now(), &HashSet::new()),
        "zero disables age expiry"
    );
}

#[test]
fn cold_archive_is_metadata_only_and_warm_archive_is_not_parsed() {
    let store = Store::new(Agent::Codex);
    let old = store.codex("old", now() - Duration::days(30));
    let fresh = store.codex("fresh", now() - Duration::hours(1));
    let originals = [fs::read(&old).unwrap(), fs::read(&fresh).unwrap()];
    let mut state = State::default();
    state.set_label("old", "sidecar title");
    let cold = store.scan(&state);
    assert_eq!((cold.metadata_reads, cold.detail_reads), (2, 1));
    assert_eq!(cold.active.len(), 1);
    assert_eq!(cold.archive.len(), 1);
    assert_eq!(cold.active[0].id, "fresh");
    assert!(!cold.active[0].archived);
    assert_eq!(cold.active[0].tokens.total, 15);
    assert!(cold.archive[0].archived);
    assert_eq!(cold.archive[0].tokens, TokenUsage::default());
    assert_eq!(
        cold.archive[0].title, "original title",
        "TUI applies latest labels"
    );
    let warm = store.scan(&state);
    assert_eq!((warm.metadata_reads, warm.detail_reads), (0, 1));
    assert_eq!(warm.archive[0].file, old);
    let hydrated = catalog::hydrate(&warm.archive[0]).unwrap();
    assert_eq!(hydrated.file, old);
    assert_eq!(hydrated.tokens.total, 15);
    assert!(
        hydrated.archived,
        "hydration preserves the caller's view flag"
    );
    assert_eq!(fs::read(old).unwrap(), originals[0]);
    assert_eq!(fs::read(fresh).unwrap(), originals[1]);
    let _: CatalogSnapshot = CatalogSnapshot::default();
    assert!(!format!("{warm:?}").is_empty());
}

#[cfg(unix)]
struct RestorePermissions<'a> {
    path: &'a Path,
    permissions: fs::Permissions,
}

#[cfg(unix)]
impl Drop for RestorePermissions<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.path, self.permissions.clone());
    }
}

#[cfg(unix)]
fn assert_read_failure_recovery(agent: Agent) {
    use std::os::unix::fs::PermissionsExt;

    for previously_cached in [false, true] {
        let store = Store::new(agent);
        let failed = store.archived_source("recovered");
        let good = store.archived_source("unchanged");
        let good_bytes = fs::read(&good).unwrap();
        let state = State::default();
        if previously_cached {
            assert_eq!(store.scan(&state).archive.len(), 2);
            // A changed source forces a fresh read of a previously good entry.
            writeln!(File::options().append(true).open(&failed).unwrap()).unwrap();
        }
        let original = fs::read(&failed).unwrap();
        let fingerprint = fs::metadata(&failed).unwrap();
        let restore = RestorePermissions {
            path: &failed,
            permissions: fingerprint.permissions(),
        };
        fs::set_permissions(&failed, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(
            File::open(&failed).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the {agent:?} fixture must actually deny reads"
        );
        let denied = store.scan(&state);
        let still_denied = store.scan(&state);
        drop(restore);
        let restored = fs::metadata(&failed).unwrap();
        assert_eq!(restored.len(), fingerprint.len());
        assert_eq!(
            restored.modified().unwrap(),
            fingerprint.modified().unwrap()
        );
        let recovered = store.scan(&state);
        assert_eq!(
            recovered.archive.len(),
            2,
            "{agent:?} must recover at identical len/mtime; previously_cached={previously_cached}"
        );
        assert_eq!((recovered.metadata_reads, recovered.detail_reads), (1, 0));
        assert!(recovered
            .archive
            .iter()
            .any(|session| session.file == failed));
        assert_eq!(denied.archive.len(), 1);
        assert_eq!(denied.archive[0].file, good);
        assert_eq!(denied.metadata_reads, if previously_cached { 1 } else { 2 });
        assert_eq!(still_denied.archive.len(), 1);
        assert_eq!(still_denied.metadata_reads, 1, "only failed reads retry");
        let warm = store.scan(&state);
        assert_eq!(warm.archive.len(), 2);
        assert_eq!((warm.metadata_reads, warm.detail_reads), (0, 0));
        assert_eq!(fs::read(&failed).unwrap(), original);
        assert_eq!(fs::read(good).unwrap(), good_bytes);
    }
}

#[cfg(unix)]
#[test]
fn codex_read_failure_recovers_without_fingerprint_change() {
    assert_read_failure_recovery(Agent::Codex);
}

#[cfg(unix)]
#[test]
fn claude_read_failure_recovers_without_fingerprint_change() {
    assert_read_failure_recovery(Agent::Claude);
}

#[cfg(unix)]
#[test]
fn kiro_read_failure_recovers_without_fingerprint_change() {
    assert_read_failure_recovery(Agent::Kiro);
}

#[cfg(unix)]
#[test]
fn cursor_read_failure_recovers_without_fingerprint_change() {
    assert_read_failure_recovery(Agent::Cursor);
}

#[cfg(unix)]
#[test]
fn omp_read_failure_recovers_without_fingerprint_change() {
    assert_read_failure_recovery(Agent::Omp);
}

#[test]
fn successful_non_session_reads_remain_cached_for_every_provider() {
    for agent in [
        Agent::Codex,
        Agent::Claude,
        Agent::Kiro,
        Agent::Cursor,
        Agent::Omp,
    ] {
        let store = Store::new(agent);
        let path = store.archived_source("empty");
        fs::write(&path, "{}\n").unwrap();
        let state = State::default();
        let cold = store.scan(&state);
        assert!(
            cold.active.is_empty() && cold.archive.is_empty(),
            "{agent:?}"
        );
        assert_eq!(cold.metadata_reads, 1, "{agent:?}");
        let cached: serde_json::Value =
            serde_json::from_slice(&fs::read(&store.cache).unwrap()).unwrap();
        assert_eq!(cached["version"], 2);
        assert_eq!(cached["entries"].as_array().unwrap().len(), 1);
        assert!(
            cached["entries"][0]["metadata"].is_null(),
            "successful absence is persisted"
        );
        let modified = fs::metadata(&store.cache).unwrap().modified().unwrap();
        let warm = store.scan(&state);
        assert!(
            warm.active.is_empty() && warm.archive.is_empty(),
            "{agent:?}"
        );
        assert_eq!(
            warm.metadata_reads, 0,
            "{agent:?} successful absence stays cached"
        );
        assert_eq!(
            fs::metadata(&store.cache).unwrap().modified().unwrap(),
            modified
        );
    }
}

#[test]
fn legacy_negative_cache_retries_without_reparsing_successful_metadata() {
    let store = Store::new(Agent::Codex);
    let recovered = store.archived_source("recovered");
    store.archived_source("unchanged");
    let state = State::default();
    store.scan(&state);
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(&store.cache).unwrap()).unwrap();
    legacy["version"] = serde_json::json!(1);
    for entry in legacy["entries"].as_array_mut().unwrap() {
        if entry["metadata"]["id"] == "recovered" {
            entry["metadata"] = serde_json::Value::Null;
        }
    }
    fs::write(&store.cache, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let retry = store.scan(&state);
    assert_eq!(
        retry.archive.len(),
        2,
        "old failed reads cannot remain permanently hidden"
    );
    assert_eq!(
        retry.metadata_reads, 1,
        "keep successful legacy metadata warm"
    );
    assert!(retry
        .archive
        .iter()
        .any(|session| session.file == recovered));
    let migrated: serde_json::Value =
        serde_json::from_slice(&fs::read(&store.cache).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2);
    assert_eq!(store.scan(&state).metadata_reads, 0);
}

#[test]
fn legacy_successful_absence_is_retried_only_once() {
    let store = Store::new(Agent::Codex);
    let path = store.archived_source("empty");
    fs::write(&path, "{}\n").unwrap();
    let state = State::default();
    store.scan(&state);
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(&store.cache).unwrap()).unwrap();
    legacy["version"] = serde_json::json!(1);
    fs::write(&store.cache, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let retry = store.scan(&state);
    assert!(retry.active.is_empty() && retry.archive.is_empty());
    assert_eq!(retry.metadata_reads, 1);
    assert_eq!(store.scan(&state).metadata_reads, 0);
}

#[test]
fn legacy_use_is_bound_to_all_sources_before_catalog_classification() {
    let store = Store::new(Agent::Codex);
    store.codex("same-id", now() - Duration::days(30));
    let other_root = SessionRoot {
        agent: Agent::Codex,
        dir: store.dir.path().join("account-b/sessions"),
    };
    write_codex(
        &other_root.dir.join("same-id.jsonl"),
        "same-id",
        "/work/other",
        now() - Duration::days(30),
    );
    let mut state = State::default();
    state.record_use("same-id", now());
    let before = serde_json::to_value(&state).unwrap();
    let roots = [store.root.clone(), other_root];
    for scope in [Scope::Global, Scope::WorkingDir("/work/project".into())] {
        let snapshot =
            catalog::scan(&scope, &roots, &state, &HashSet::new(), &store.cache, now()).unwrap();
        assert!(
            snapshot.active.is_empty(),
            "ambiguous legacy use cannot promote either source"
        );
        assert_eq!(snapshot.detail_reads, 0);
        assert_eq!(
            snapshot.archive.len(),
            if matches!(scope, Scope::Global) { 2 } else { 1 }
        );
        assert_eq!(snapshot.use_sources.get("same-id"), Some(&Vec::new()));
    }
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        before,
        "scan only binds a cloned state"
    );
}

#[test]
fn legacy_use_binding_is_returned_for_unique_out_of_scope_source() {
    let store = Store::new(Agent::Codex);
    let path = store.archived_source("unique");
    store.scan(&State::default());
    let mut state = State::default();
    state.record_use("unique", now());
    let snapshot = catalog::scan(
        &Scope::WorkingDir("/work/other".into()),
        std::slice::from_ref(&store.root),
        &state,
        &HashSet::new(),
        &store.cache,
        now(),
    )
    .unwrap();
    assert!(snapshot.active.is_empty() && snapshot.archive.is_empty());
    assert_eq!(
        snapshot.metadata_reads, 0,
        "warm metadata still participates in migration"
    );
    assert_eq!(snapshot.use_sources["unique"].len(), 1);
    let binding = serde_json::to_value(&snapshot.use_sources["unique"][0]).unwrap();
    assert_eq!(binding["file"], serde_json::to_value(path).unwrap());
    assert!(state.last_used_sources.is_empty());
}

#[test]
fn source_bound_use_survives_state_reload_with_a_warm_catalog() {
    let store = Store::new(Agent::Codex);
    let first = store.archived_source("same-id");
    let other = SessionRoot {
        agent: Agent::Codex,
        dir: store.dir.path().join("account-b/sessions"),
    };
    let second = other.dir.join("same-id.jsonl");
    write_codex(
        &second,
        "same-id",
        "/work/project",
        now() - Duration::days(30),
    );
    let roots = [store.root.clone(), other];
    let scan = |state: &State| {
        catalog::scan(
            &Scope::Global,
            &roots,
            state,
            &HashSet::new(),
            &store.cache,
            now(),
        )
        .unwrap()
    };
    let cold = scan(&State::default());
    assert_eq!(cold.archive.len(), 2);
    let mut state = State::default();
    state.record_session_use(
        cold.archive
            .iter()
            .find(|session| session.file == first)
            .unwrap(),
        now(),
    );
    let path = store.dir.path().join("state.json");
    state.save_to(&path).unwrap();
    let state = State::load_from(&path);
    for _ in 0..2 {
        let warm = scan(&state);
        assert_eq!((warm.metadata_reads, warm.detail_reads), (0, 1));
        assert_eq!(warm.active.len(), 1);
        assert_eq!(warm.active[0].file, first);
        assert_eq!(warm.archive.len(), 1);
        assert_eq!(warm.archive[0].file, second);
        assert!(
            warm.use_sources.is_empty(),
            "existing source bindings need no migration"
        );
    }
}

#[cfg(unix)]
#[test]
fn unreadable_source_defers_legacy_migration_until_metadata_recovers() {
    use std::os::unix::fs::PermissionsExt;

    let store = Store::new(Agent::Codex);
    store.archived_source("same-id");
    let failed = store.root.dir.join("other-source.jsonl");
    write_codex(
        &failed,
        "same-id",
        "/work/project",
        now() - Duration::days(30),
    );
    let restore = RestorePermissions {
        path: &failed,
        permissions: fs::metadata(&failed).unwrap().permissions(),
    };
    fs::set_permissions(&failed, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        File::open(&failed).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let mut state = State::default();
    state.record_use("same-id", now());
    let incomplete = store.scan(&state);
    drop(restore);
    assert!(incomplete.active.is_empty());
    assert_eq!(incomplete.archive.len(), 1);
    assert!(
        incomplete.use_sources.is_empty(),
        "unreadable metadata is not proof of uniqueness"
    );
    let recovered = store.scan(&state);
    assert!(recovered.active.is_empty());
    assert_eq!(recovered.archive.len(), 2);
    assert_eq!(recovered.metadata_reads, 1);
    assert_eq!(recovered.use_sources.get("same-id"), Some(&Vec::new()));
}

#[test]
fn scoped_claude_census_does_not_resolve_legacy_use() {
    let store = Store::new(Agent::Claude);
    let path = store.archived_source("same-id");
    let other = store.root.dir.join("-work-other/same-id.jsonl");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("/work/project", "/work/other");
    fs::write(other, text).unwrap();
    let mut state = State::default();
    state.record_use("same-id", now());
    let partial = catalog::scan(
        &Scope::WorkingDir("/work/project".into()),
        std::slice::from_ref(&store.root),
        &state,
        &HashSet::new(),
        &store.cache,
        now(),
    )
    .unwrap();
    assert!(
        partial.active.is_empty(),
        "a scoped census cannot establish source uniqueness"
    );
    assert_eq!(partial.archive.len(), 1);
    assert!(
        partial.use_sources.is_empty(),
        "defer persistence until a complete census"
    );
    let complete = store.scan(&state);
    assert!(complete.active.is_empty());
    assert_eq!(complete.archive.len(), 2);
    assert_eq!(complete.use_sources.get("same-id"), Some(&Vec::new()));
}

#[cfg(unix)]
#[test]
fn unreadable_store_defers_legacy_migration_for_every_provider() {
    use std::os::unix::fs::PermissionsExt;

    for agent in [
        Agent::Codex,
        Agent::Claude,
        Agent::Kiro,
        Agent::Cursor,
        Agent::Omp,
    ] {
        let readable = Store::new(agent);
        readable.archived_source("same-id");
        let hidden = Store::new(agent);
        hidden.archived_source("same-id");
        let restore = RestorePermissions {
            path: &hidden.root.dir,
            permissions: fs::metadata(&hidden.root.dir).unwrap().permissions(),
        };
        fs::set_permissions(&hidden.root.dir, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(
            fs::read_dir(&hidden.root.dir).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let roots = [readable.root.clone(), hidden.root.clone()];
        let mut state = State::default();
        state.record_use("same-id", now());
        let scan = || {
            catalog::scan(
                &Scope::Global,
                &roots,
                &state,
                &HashSet::new(),
                &readable.cache,
                now(),
            )
            .unwrap()
        };
        let incomplete = scan();
        drop(restore);
        assert!(
            incomplete.active.is_empty(),
            "{agent:?} incomplete enumeration cannot establish uniqueness"
        );
        assert_eq!(incomplete.archive.len(), 1);
        assert!(incomplete.use_sources.is_empty());
        let recovered = scan();
        assert!(recovered.active.is_empty());
        assert_eq!(recovered.archive.len(), 2);
        assert_eq!(recovered.metadata_reads, 1);
        assert_eq!(recovered.use_sources.get("same-id"), Some(&Vec::new()));
    }
}

#[cfg(unix)]
fn assert_listable_store_defers_legacy_migration(
    readable: &Store,
    hidden: &Store,
    restricted: &Path,
    denied_child: &Path,
) {
    use std::os::unix::fs::PermissionsExt;

    let agent = hidden.root.agent;
    let restore = RestorePermissions {
        path: restricted,
        permissions: fs::metadata(restricted).unwrap().permissions(),
    };
    fs::set_permissions(restricted, fs::Permissions::from_mode(0o400)).unwrap();
    let listed = fs::read_dir(restricted)
        .expect("the restricted directory must remain listable")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("listing every entry must succeed independently of child metadata");
    assert!(listed.iter().any(|entry| entry.path() == denied_child));
    assert_eq!(
        fs::metadata(denied_child).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "{agent:?}: the listed child must deny metadata access"
    );

    let roots = [readable.root.clone(), hidden.root.clone()];
    let mut state = State::default();
    state.record_use("same-id", now());
    let scan = || {
        catalog::scan(
            &Scope::Global,
            &roots,
            &state,
            &HashSet::new(),
            &readable.cache,
            now(),
        )
        .unwrap()
    };
    for attempt in 0..2 {
        let incomplete = scan();
        assert!(
            incomplete.use_sources.is_empty(),
            "{agent:?}: denied child metadata cannot establish uniqueness"
        );
        assert!(incomplete.active.is_empty());
        assert_eq!(incomplete.archive.len(), 1);
        assert_eq!(incomplete.metadata_reads, usize::from(attempt == 0));
        assert_eq!(incomplete.detail_reads, 0);
    }
    drop(restore);
    assert!(fs::metadata(denied_child).is_ok());
    let recovered = scan();
    assert!(recovered.active.is_empty());
    assert_eq!(recovered.archive.len(), 2);
    assert_eq!((recovered.metadata_reads, recovered.detail_reads), (1, 0));
    assert_eq!(recovered.use_sources.get("same-id"), Some(&Vec::new()));
    let warm = scan();
    assert!(warm.active.is_empty());
    assert_eq!(warm.archive.len(), 2);
    assert_eq!((warm.metadata_reads, warm.detail_reads), (0, 0));
}

#[cfg(unix)]
#[test]
fn listable_claude_root_with_denied_bucket_metadata_defers_legacy_migration() {
    let readable = Store::new(Agent::Claude);
    readable.archived_source("same-id");
    let hidden = Store::new(Agent::Claude);
    let source = hidden.archived_source("same-id");
    assert_listable_store_defers_legacy_migration(
        &readable,
        &hidden,
        &hidden.root.dir,
        source.parent().unwrap(),
    );
}

#[cfg(unix)]
#[test]
fn listable_store_with_denied_file_metadata_defers_legacy_migration_for_every_provider() {
    for agent in [
        Agent::Codex,
        Agent::Claude,
        Agent::Kiro,
        Agent::Cursor,
        Agent::Omp,
    ] {
        let readable = Store::new(agent);
        readable.archived_source("same-id");
        let hidden = Store::new(agent);
        let source = hidden.archived_source("same-id");
        assert_listable_store_defers_legacy_migration(
            &readable,
            &hidden,
            source.parent().unwrap(),
            &source,
        );
    }
}

#[test]
fn changed_metadata_reactivates_age_archive_but_not_explicit_archive() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let mut state = State::default();
    assert_eq!(store.scan(&state).archive.len(), 1);
    write_codex(&path, "old", "/work/project", now());
    let changed = store.scan(&state);
    assert_eq!((changed.metadata_reads, changed.detail_reads), (1, 1));
    assert_eq!(changed.active.len(), 1);
    state.set_archived("old", true);
    let explicitly_archived = store.scan(&state);
    assert_eq!(
        (
            explicitly_archived.metadata_reads,
            explicitly_archived.detail_reads
        ),
        (0, 0)
    );
    assert_eq!(explicitly_archived.archive.len(), 1);
}

#[test]
fn mtime_only_change_never_promotes_and_removed_sources_leave_catalog() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let state = State::default();
    let cold = store.scan(&state);
    let before = fs::read(&path).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            FileTimes::new().set_modified(std::time::SystemTime::from(now() + Duration::days(2))),
        )
        .unwrap();
    let touched = store.scan(&state);
    assert_eq!((touched.metadata_reads, touched.detail_reads), (1, 0));
    assert_eq!(touched.archive.len(), 1);
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::remove_file(&path).unwrap();
    assert!(catalog::hydrate(&cold.archive[0]).is_err());
    assert!(store.scan(&state).archive.is_empty());
}

#[test]
fn cache_missing_corrupt_or_wrong_version_rebuilds_from_originals() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let before = fs::read(&path).unwrap();
    let state = State::default();
    store.scan(&state);
    for broken in ["{corrupt", r#"{"version":4294967295,"entries":[]}"#] {
        fs::write(&store.cache, broken).unwrap();
        let rebuilt = store.scan(&state);
        assert_eq!((rebuilt.metadata_reads, rebuilt.detail_reads), (1, 0));
        assert_eq!(rebuilt.archive[0].id, "old");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&store.cache).unwrap()).is_ok()
        );
    }
    fs::remove_file(&store.cache).unwrap();
    assert_eq!(store.scan(&state).metadata_reads, 1);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn cache_isolated_by_full_scope_roots_and_account_source_path() {
    let store = Store::new(Agent::Codex);
    let first = store.codex("same-id", now() - Duration::days(30));
    let other_root = SessionRoot {
        agent: Agent::Codex,
        dir: store.dir.path().join("account-b/sessions"),
    };
    let second = other_root.dir.join("same-id.jsonl");
    write_codex(
        &second,
        "same-id",
        "/work/other",
        now() - Duration::days(30),
    );
    let state = State::default();
    let protected = HashSet::new();
    store.scan(&state);
    let other = catalog::scan(
        &Scope::Global,
        std::slice::from_ref(&other_root),
        &state,
        &protected,
        &store.cache,
        now(),
    )
    .unwrap();
    assert_eq!(other.archive.len(), 1);
    assert_eq!(other.archive[0].file, second);
    let both = [store.root.clone(), other_root, store.root.clone()];
    let all = catalog::scan(
        &Scope::Global,
        &both,
        &state,
        &protected,
        &store.cache,
        now(),
    )
    .unwrap();
    assert_eq!(
        all.archive.len(),
        2,
        "same native id in two accounts retains both paths"
    );
    assert!(all.archive.iter().any(|s| s.file == first));
    for cwd in ["/work/project", "/work/other", "/work/absent"] {
        let scoped = catalog::scan(
            &Scope::WorkingDir(cwd.into()),
            &both,
            &state,
            &protected,
            &store.cache,
            now(),
        )
        .unwrap();
        assert!(scoped.archive.iter().all(|s| s.cwd == Path::new(cwd)));
        assert_eq!(scoped.archive.len(), usize::from(cwd != "/work/absent"));
    }
}

#[test]
fn hydration_rejects_corruption_and_changed_native_identity() {
    for agent in [Agent::Codex, Agent::Claude] {
        let store = Store::new(agent);
        let path = if agent == Agent::Codex {
            store.codex("session-a", now() - Duration::days(30))
        } else {
            let path = store.root.dir.join("-work-project/session-a.jsonl");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"type":"user","sessionId":"session-a","cwd":"/work/project","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello"}}"#).unwrap();
            path
        };
        let session = store.scan(&State::default()).archive.remove(0);
        let bytes = fs::read(&path).unwrap();
        for corrupt in ["{invalid transcript", "{}", r#"{"type":"assistant"}"#] {
            fs::write(&path, corrupt).unwrap();
            assert!(
                catalog::hydrate(&session).is_err(),
                "{agent:?} must not fabricate from filename for {corrupt:?}"
            );
        }
        let changed = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("session-a", "session-b");
        fs::write(&path, changed).unwrap();
        assert!(
            catalog::hydrate(&session).is_err(),
            "native id changed for {agent:?}"
        );
        let changed = String::from_utf8(bytes)
            .unwrap()
            .replace("/work/project", "/work/replaced");
        fs::write(&path, changed).unwrap();
        assert!(
            catalog::hydrate(&session).is_err(),
            "source cwd changed for {agent:?}"
        );
    }
}

#[test]
fn hydration_refreshes_prompt_timestamp_from_disk_instead_of_echoing_old_metadata() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let metadata = store.scan(&State::default()).archive.remove(0);
    write_codex(&path, "old", "/work/project", now() - Duration::days(40));
    let fresh = catalog::hydrate(&metadata).unwrap();
    assert_eq!(fresh.last_prompt_at, Some(now() - Duration::days(40)));
}

#[test]
fn claude_cold_metadata_skips_usage_and_tool_results_do_not_refresh_human_time() {
    let store = Store::new(Agent::Claude);
    let path = store.root.dir.join("-work-project/session-a.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(&path).unwrap();
    writeln!(file, "{}", serde_json::json!({"type":"user","sessionId":"session-a","cwd":"/work/project","timestamp":now()-Duration::days(30),"message":{"content":"old human prompt"}})).unwrap();
    for _ in 0..5000 {
        writeln!(file, "{}", serde_json::json!({"type":"assistant","timestamp":now(),"message":{"usage":{"input_tokens":2,"output_tokens":3},"content":"assistant output"}})).unwrap();
    }
    writeln!(file, "{}", serde_json::json!({"type":"user","timestamp":now(),"message":{"content":[{"type":"tool_result","content":"automated output"}]}})).unwrap();
    drop(file);
    let before = fs::read(&path).unwrap();
    let cold = store.scan(&State::default());
    assert_eq!((cold.metadata_reads, cold.detail_reads), (1, 0));
    assert_eq!(
        cold.archive[0].last_prompt_at,
        Some(now() - Duration::days(30))
    );
    assert_eq!(cold.archive[0].tokens.total, 0);
    let full = catalog::hydrate(&cold.archive[0]).unwrap();
    assert_eq!(full.tokens.total, 25000);
    assert_eq!(full.last_prompt_at, cold.archive[0].last_prompt_at);
    assert_eq!(store.scan(&State::default()).metadata_reads, 0);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn protected_archive_is_fully_loaded_without_mutating_explicit_state() {
    let store = Store::new(Agent::Codex);
    store.codex("old", now() - Duration::days(30));
    let mut state = State::default();
    state.set_archived("old", true);
    let result = catalog::scan(
        &Scope::Global,
        &[store.root],
        &state,
        &HashSet::from(["old".into()]),
        &store.cache,
        now(),
    )
    .unwrap();
    assert_eq!(result.active.len(), 1);
    assert!(result.archive.is_empty());
    assert_eq!(result.detail_reads, 1);
    assert!(!result.active[0].archived);
    assert!(state.is_archived("old"));
}

#[cfg(unix)]
#[test]
fn catalog_write_is_private_and_does_not_narrow_existing_parent() {
    use std::os::unix::fs::PermissionsExt;
    let store = Store::new(Agent::Codex);
    store.codex("old", now() - Duration::days(30));
    fs::create_dir_all(store.cache.parent().unwrap()).unwrap();
    fs::set_permissions(
        store.cache.parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    store.scan(&State::default());
    assert_eq!(
        fs::metadata(&store.cache).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(store.cache.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn partial_root_and_scope_scans_keep_other_cached_coverage() {
    let store = Store::new(Agent::Codex);
    store.codex("first", now() - Duration::days(30));
    let other = SessionRoot {
        agent: Agent::Codex,
        dir: store.dir.path().join("account-b/sessions"),
    };
    write_codex(
        &other.dir.join("other.jsonl"),
        "other",
        "/work/other",
        now() - Duration::days(30),
    );
    let state = State::default();
    let protected = HashSet::new();
    let roots = [store.root.clone(), other.clone()];
    let initial = catalog::scan(
        &Scope::Global,
        &roots,
        &state,
        &protected,
        &store.cache,
        now(),
    )
    .unwrap();
    assert_eq!(initial.metadata_reads, 2);
    let partial = catalog::scan(
        &Scope::WorkingDir("/work/other".into()),
        &[other],
        &state,
        &protected,
        &store.cache,
        now(),
    )
    .unwrap();
    assert_eq!(partial.archive.len(), 1);
    assert_eq!(partial.metadata_reads, 0);
    let again = catalog::scan(
        &Scope::Global,
        &roots,
        &state,
        &protected,
        &store.cache,
        now(),
    )
    .unwrap();
    assert_eq!(again.archive.len(), 2);
    assert_eq!(
        again.metadata_reads, 0,
        "partial coverage must not evict another root"
    );
}

#[test]
fn claude_lossy_cwd_collision_is_checked_and_fallback_contexts_are_separate() {
    let store = Store::new(Agent::Claude);
    let bucket = store.root.dir.join("-work-a-b");
    fs::create_dir_all(&bucket).unwrap();
    fs::write(bucket.join("known.jsonl"), r#"{"type":"user","sessionId":"known","cwd":"/work/a.b","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello"}}"#).unwrap();
    fs::write(bucket.join("legacy.jsonl"), r#"{"type":"user","sessionId":"legacy","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello"}}"#).unwrap();
    let state = State::default();
    let roots = [store.root.clone()];
    let scan = |cwd: &str| {
        catalog::scan(
            &Scope::WorkingDir(cwd.into()),
            &roots,
            &state,
            &HashSet::new(),
            &store.cache,
            now(),
        )
        .unwrap()
    };
    assert_eq!(scan("/work/a.b").archive.len(), 2);
    let collision = scan("/work/a/b");
    assert_eq!(collision.archive.len(), 1);
    assert_eq!(collision.archive[0].id, "legacy");
    assert_eq!(collision.archive[0].cwd, Path::new("/work/a/b"));
    assert_eq!(scan("/work/a.b").metadata_reads, 0);
    let global = store.scan(&state);
    assert_eq!(global.archive.len(), 2);
    assert_eq!(
        global
            .archive
            .iter()
            .find(|s| s.id == "legacy")
            .unwrap()
            .cwd,
        Path::new("/work/a/b")
    );
}

#[test]
fn known_human_time_survives_tail_only_output_and_bounded_full_parser() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("recent", now() - Duration::hours(1));
    let mut file = File::options().append(true).open(&path).unwrap();
    for _ in 0..6000 {
        writeln!(file, "{}", serde_json::json!({"type":"event_msg","timestamp":now(),"payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":99}}}})).unwrap();
    }
    drop(file);
    let snapshot = store.scan(&State::default());
    assert_eq!(
        snapshot.active.len(),
        1,
        "full tail parsing cannot erase the known human prompt from the head"
    );
    assert_eq!(
        snapshot.active[0].last_prompt_at,
        Some(now() - Duration::hours(1))
    );
    assert_eq!(snapshot.active[0].tokens.total, 99);
}

#[test]
fn changed_source_keeps_observed_human_turn_after_it_moves_out_of_both_windows() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let output = format!(
        "{}\n",
        serde_json::json!({"type":"event_msg","timestamp":now(),"payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":99}}}})
    );
    let append_output = || {
        let mut file = File::options().append(true).open(&path).unwrap();
        for _ in 0..6000 {
            file.write_all(output.as_bytes()).unwrap();
        }
    };
    let state = State::default();
    append_output();
    assert_eq!(store.scan(&state).archive.len(), 1);
    writeln!(File::options().append(true).open(&path).unwrap(), "{}", serde_json::json!({"type":"response_item","timestamp":now(),"payload":{"type":"message","role":"user","content":[{"text":"new human turn"}]}})).unwrap();
    let external_use = store.scan(&state);
    assert_eq!(external_use.active.len(), 1);
    assert_eq!(external_use.active[0].last_prompt_at, Some(now()));
    append_output();
    let before = fs::read(&path).unwrap();
    let output_only = store.scan(&state);
    assert_eq!(
        (output_only.metadata_reads, output_only.detail_reads),
        (1, 1)
    );
    assert_eq!(output_only.active.len(), 1);
    assert_eq!(output_only.active[0].last_prompt_at, Some(now()));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn noop_scan_does_not_rewrite_raw_metadata_cache() {
    let store = Store::new(Agent::Codex);
    store.codex("old", now() - Duration::days(30));
    let mut state = State::default();
    store.scan(&state);
    let before = fs::read(&store.cache).unwrap();
    let modified = fs::metadata(&store.cache).unwrap().modified().unwrap();
    state.set_label("old", "new display label");
    let again = store.scan(&state);
    assert_eq!((again.metadata_reads, again.detail_reads), (0, 0));
    assert_eq!(again.archive[0].title, "original title");
    assert_eq!(fs::read(&store.cache).unwrap(), before);
    assert_eq!(
        fs::metadata(&store.cache).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn injected_preambles_are_not_fresh_human_activity() {
    for agent in [Agent::Codex, Agent::Claude] {
        let store = Store::new(agent);
        let path = if agent == Agent::Codex {
            store.codex("old", now() - Duration::days(30))
        } else {
            let path = store.root.dir.join("-work-project/old.jsonl");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{}\n", serde_json::json!({"type":"user","sessionId":"old","cwd":"/work/project","timestamp":now()-Duration::days(30),"message":{"content":"original title"}}))).unwrap();
            path
        };
        let injected = if agent == Agent::Codex {
            serde_json::json!({"type":"response_item","timestamp":now(),"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /work/project"}]}})
        } else {
            serde_json::json!({"type":"user","timestamp":now(),"message":{"content":"# CLAUDE.md instructions for /work/project"}})
        };
        writeln!(
            File::options().append(true).open(path).unwrap(),
            "{injected}"
        )
        .unwrap();
        let snapshot = store.scan(&State::default());
        assert_eq!(
            snapshot.active.len(),
            0,
            "injected {agent:?} context is not a human prompt"
        );
        assert_eq!(
            snapshot.archive[0].last_prompt_at,
            Some(now() - Duration::days(30))
        );
    }
}

#[test]
fn sidecars_and_omp_use_their_native_source_for_metadata_and_hydration() {
    for agent in [Agent::Kiro, Agent::Cursor, Agent::Omp] {
        let store = Store::new(agent);
        let (path, text) = match agent {
            Agent::Kiro => (store.root.dir.join("session-a.json"), serde_json::json!({"session_id":"session-a","cwd":"/work/project","created_at":now()-Duration::days(30),"updated_at":now(),"title":"original title","session_state":{"rts_model_state":{"context_usage_percentage":42.5}}}).to_string()),
            Agent::Cursor => (store.root.dir.join("workspace/session-a/meta.json"), serde_json::json!({"hasConversation":true,"cwd":"/work/project","createdAtMs":(now()-Duration::days(30)).timestamp_millis(),"updatedAtMs":now().timestamp_millis(),"title":"original title"}).to_string()),
            Agent::Omp => (store.root.dir.join("bucket/session-a.jsonl"), format!("{}\n{}\n{}\n", serde_json::json!({"type":"session","id":"session-a","cwd":"/work/project","timestamp":now()-Duration::days(30),"title":"original title"}), serde_json::json!({"type":"message","timestamp":now()-Duration::days(30),"message":{"role":"user","content":"first prompt"}}), serde_json::json!({"type":"model_usage","timestamp":now(),"usage":{"input":7,"output":3,"totalTokens":10}}))),
            _ => unreachable!("sidecar/OMP test providers"),
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let original = fs::read(&path).unwrap();
        let cold = store.scan(&State::default());
        assert_eq!(
            cold.archive.len(),
            1,
            "{agent:?} update time alone is not human use"
        );
        assert_eq!((cold.metadata_reads, cold.detail_reads), (1, 0));
        let metadata = &cold.archive[0];
        assert_eq!(metadata.file, path);
        assert_eq!(metadata.tokens.total, 0);
        let full = catalog::hydrate(metadata).unwrap();
        assert_eq!(full.id, "session-a");
        assert_eq!(full.file, path);
        if agent == Agent::Kiro {
            assert_eq!(full.context_pct, Some(42.5));
        }
        if agent == Agent::Omp {
            assert_eq!(full.tokens.total, 10);
        }
        let mut state = State::default();
        state.record_use("session-a", now());
        let promoted = store.scan(&state);
        assert_eq!((promoted.metadata_reads, promoted.detail_reads), (0, 1));
        assert_eq!(promoted.active.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
        assert!(catalog::hydrate(metadata).is_err());
    }
}

#[test]
fn changed_length_invalidates_even_when_mtime_is_unchanged() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let state = State::default();
    store.scan(&state);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let mut file = File::options().append(true).open(&path).unwrap();
    writeln!(file, "{}", serde_json::json!({"type":"response_item","timestamp":now(),"payload":{"type":"message","role":"user","content":[{"text":"new human prompt"}]}})).unwrap();
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    drop(file);
    let changed = store.scan(&state);
    assert_eq!((changed.metadata_reads, changed.detail_reads), (1, 1));
    assert_eq!(changed.active[0].last_prompt_at, Some(now()));
}

#[test]
fn cache_path_cannot_overwrite_a_source_and_failed_write_cleans_its_temp_file() {
    let store = Store::new(Agent::Codex);
    let path = store.codex("old", now() - Duration::days(30));
    let before = fs::read(&path).unwrap();
    assert!(catalog::scan(
        &Scope::Global,
        std::slice::from_ref(&store.root),
        &State::default(),
        &HashSet::new(),
        &path,
        now()
    )
    .is_err());
    assert_eq!(fs::read(path).unwrap(), before);
    fs::create_dir_all(&store.cache).unwrap();
    assert!(catalog::scan(
        &Scope::Global,
        &[store.root],
        &State::default(),
        &HashSet::new(),
        &store.cache,
        now()
    )
    .is_err());
    assert_eq!(
        fs::read_dir(store.cache.parent().unwrap()).unwrap().count(),
        1,
        "no temporary catalog survives an unsuccessful rename"
    );
}

#[test]
#[ignore = "large isolated filesystem evaluator; run explicitly in the release profile"]
fn large_archive_catalog_5k_and_50k_warm_reads_are_bounded() {
    let store = Store::new(Agent::Codex);
    let active_path = store.codex("active", now());
    let active_bytes = fs::read(&active_path).unwrap();
    let state = State::default();
    let mut populated = 0;
    for size in [5_000, 50_000] {
        for index in populated..size {
            let text = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"old-{index}\",\"cwd\":\"/work/project\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}}}\n"
            );
            fs::write(store.root.dir.join(format!("old-{index}.jsonl")), text).unwrap();
        }
        let cold = store.scan(&state);
        assert_eq!(cold.active.len(), 1);
        assert_eq!(cold.archive.len(), size);
        assert_eq!(
            cold.metadata_reads,
            size - populated + usize::from(populated == 0)
        );
        assert_eq!(cold.detail_reads, 1);
        drop(cold);
        let warm = store.scan(&state);
        assert_eq!((warm.metadata_reads, warm.detail_reads), (0, 1));
        assert_eq!(warm.archive.len(), size);
        assert!(warm
            .archive
            .iter()
            .all(|session| session.archived && session.tokens == TokenUsage::default()));
        assert_eq!(warm.active[0].file, active_path);
        assert_eq!(fs::read(&active_path).unwrap(), active_bytes);
        for index in 0..size {
            let original = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"old-{index}\",\"cwd\":\"/work/project\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}}}\n"
            );
            assert_eq!(
                fs::read(store.root.dir.join(format!("old-{index}.jsonl"))).unwrap(),
                original.as_bytes()
            );
        }
        eprintln!(
            "archive={size} warm_metadata_reads={} detail_reads={}",
            warm.metadata_reads, warm.detail_reads
        );
        populated = size;
    }
}
