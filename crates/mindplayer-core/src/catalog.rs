//! Rebuildable session metadata with full parsing limited to the active working set.
//!
//! Call scans on a worker: enumeration still visits the supplied stores. Cache
//! entries retain raw provider titles and original paths; callers apply current
//! labels and thread/category state after accepting a worker result.

use crate::discovery::{self, ParseMode, Scope, SessionRoot, SessionSource};
use crate::session::{Agent, Session};
use crate::state::{SourceUse, State};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const CATALOG_VERSION: u32 = 2;
static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);

/// A worker's raw session lists and counts of source parser invocations.
#[derive(Default, Debug)]
pub struct CatalogSnapshot {
    /// Fully parsed sessions; every row has `archived == false`.
    pub active: Vec<Session>,
    /// Lightweight metadata; every row has `archived == true`.
    pub archive: Vec<Session>,
    /// Sources inspected with bounded metadata reads during this scan.
    pub metadata_reads: usize,
    /// Active sources inspected with their full provider parser during this scan.
    pub detail_reads: usize,
    /// Newly resolved legacy-use bindings from the complete metadata census.
    /// Merge only into still-unbound IDs; a newer explicit use takes precedence.
    pub use_sources: BTreeMap<String, Vec<SourceUse>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct SourceKey {
    agent: Agent,
    file: PathBuf,
    // Claude's lossy project encoding cannot serve as a cwd identity. A
    // scope-supplied fallback must never leak into a different scoped/global scan.
    cwd_override: Option<PathBuf>,
}

impl From<&SessionSource> for SourceKey {
    fn from(source: &SessionSource) -> Self {
        Self {
            agent: source.agent,
            file: source.file.clone(),
            cwd_override: source.cwd_override.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    modified: DateTime<Utc>,
    len: u64,
}

impl Fingerprint {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            modified: metadata.modified().ok()?.into(),
            len: metadata.len(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    source: SourceKey,
    fingerprint: Fingerprint,
    // A successful read may find no session; I/O failures never become entries.
    metadata: Option<Session>,
}

#[derive(Serialize, Deserialize)]
struct Catalog {
    version: u32,
    entries: Vec<Entry>,
}

impl Catalog {
    fn load(path: &Path) -> Option<Self> {
        let mut catalog: Self = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        if !matches!(catalog.version, 1 | CATALOG_VERSION)
            || catalog.entries.iter().any(|entry| {
                entry.metadata.as_ref().is_some_and(|session| {
                    session.file != entry.source.file
                        || session.agent != entry.source.agent
                        || session.id.trim().is_empty()
                        || !session.cwd.is_absolute()
                })
            })
        {
            return None;
        }
        if catalog.version == 1 {
            // Version 1 conflated unreadable sources with valid empty sources.
            // Retry those entries once while retaining successful warm metadata.
            catalog.entries.retain(|entry| entry.metadata.is_some());
        }
        Some(catalog)
    }

    fn save(&self, path: &Path) -> Result<()> {
        let suffix = NEXT_WRITE.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("json.tmp.{}.{suffix}", std::process::id()));
        let mut file = crate::private::create_new_private(&tmp)
            .with_context(|| format!("creating catalog temporary file {}", tmp.display()))?;
        let result = (|| -> Result<()> {
            serde_json::to_writer(&mut file, self)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.with_context(|| format!("saving catalog {}", path.display()))
    }
}

/// Enumerate the requested stores, reuse unchanged metadata, and hydrate Active only.
///
/// Classification uses the supplied state/time rather than filesystem mtime.
/// Out-of-scope/root entries remain cached for other scans but never enter this
/// snapshot. A missing, malformed, or incompatible cache is rebuilt from sources.
/// Original transcripts and State are never written. Run this on a worker thread.
///
/// # Errors
/// Returns an error if the private cache cannot be persisted, or if its path
/// would overwrite a session source. Individual invalid sources are omitted.
pub fn scan(
    scope: &Scope,
    roots: &[SessionRoot],
    state: &State,
    protected: &HashSet<String>,
    cache_path: &Path,
    now: DateTime<Utc>,
) -> Result<CatalogSnapshot> {
    let cache_path = if cache_path.is_absolute() {
        cache_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(cache_path)
    };
    let (sources, enumerated_all) = discovery::gather_sources(scope, roots);
    let cache_target = fs::canonicalize(&cache_path).unwrap_or_else(|_| cache_path.clone());
    ensure!(
        !sources.iter().any(|source| {
            fs::canonicalize(&source.file).is_ok_and(|path| path == cache_target)
        }),
        "catalog path must not overwrite a session source"
    );
    let loaded = Catalog::load(&cache_path);
    let mut dirty = loaded
        .as_ref()
        .is_none_or(|catalog| catalog.version != CATALOG_VERSION);
    let mut entries: HashMap<SourceKey, Entry> = loaded
        .into_iter()
        .flat_map(|catalog| catalog.entries)
        .map(|entry| (entry.source.clone(), entry))
        .collect();
    let mut snapshot = CatalogSnapshot::default();
    let mut metadata_census = Vec::new();
    // Claude's scoped enumeration excludes other project buckets. Such a scan
    // cannot establish that an ID-only use belongs to one unique source.
    let mut complete_census = enumerated_all
        && (!matches!(scope, Scope::WorkingDir(_))
            || !roots.iter().any(|root| root.agent == Agent::Claude));
    for source in sources {
        let key = SourceKey::from(&source);
        let Some(fingerprint) = Fingerprint::read(&source.file) else {
            dirty |= entries.remove(&key).is_some();
            complete_census = false;
            continue;
        };
        let unchanged = entries
            .get(&key)
            .is_some_and(|entry| entry.fingerprint == fingerprint);
        if !unchanged {
            snapshot.metadata_reads += 1;
            let Ok(mut metadata) = discovery::parse_source(&source, ParseMode::Metadata) else {
                // Permissions and transient read errors can recover without
                // changing len/mtime. Retry on the next scan, including when a
                // previous successful entry existed for this source.
                dirty |= entries.remove(&key).is_some();
                complete_census = false;
                continue;
            };
            if let (Some(current), Some(previous)) = (
                metadata.as_mut(),
                entries.get(&key).and_then(|entry| entry.metadata.as_ref()),
            ) {
                if current.id == previous.id && current.cwd == previous.cwd {
                    // Append-only output may move a known human turn outside
                    // both windows. Keep that observation for this same source.
                    current.last_prompt_at = current.last_prompt_at.max(previous.last_prompt_at);
                }
            }
            // If a writer changed the source during parsing, retry next scan
            // rather than caching a mixed read as an unchanged fingerprint.
            if Fingerprint::read(&source.file).as_ref() != Some(&fingerprint) {
                entries.remove(&key);
                dirty = true;
                complete_census = false;
                continue;
            }
            entries.insert(
                key.clone(),
                Entry {
                    source: key.clone(),
                    fingerprint,
                    metadata,
                },
            );
            dirty = true;
        }
        if let Some(session) = entries.get(&key).and_then(|entry| entry.metadata.clone()) {
            metadata_census.push((key, session));
        }
    }
    let mut classified_state = state.clone();
    if complete_census {
        classified_state
            .bind_legacy_use_sources(metadata_census.iter().map(|(_, session)| session));
        snapshot.use_sources = classified_state
            .last_used_sources
            .iter()
            .filter(|(id, _)| !state.last_used_sources.contains_key(*id))
            .map(|(id, uses)| (id.clone(), uses.clone()))
            .collect();
    }
    // Defer unresolved legacy migration without guessing a source or persisting
    // an incomplete census as a permanent ambiguous-use tombstone.
    for id in classified_state.last_used.keys() {
        classified_state
            .last_used_sources
            .entry(id.clone())
            .or_default();
    }
    for (key, mut session) in metadata_census {
        if matches!(scope, Scope::WorkingDir(cwd) if session.cwd != *cwd) {
            continue;
        }
        if classified_state.is_active_session(&session, now, protected) {
            snapshot.detail_reads += 1;
            let Ok(mut full) = hydrate(&session) else {
                // A racing replacement/missing source cannot be advertised as
                // a successful active refresh. Its next scan retries metadata.
                entries.remove(&key);
                dirty = true;
                continue;
            };
            // The catalog retains observed human activity for classification
            // when full usage's bounded tail no longer contains that turn.
            full.last_prompt_at = full.last_prompt_at.max(session.last_prompt_at);
            session = full;
        }
        session.archived = !classified_state.is_active_session(&session, now, protected);
        if session.archived {
            session.tokens = Default::default();
            snapshot.archive.push(session);
        } else {
            snapshot.active.push(session);
        }
    }
    discovery::sort_by_recency(&mut snapshot.active);
    discovery::sort_by_recency(&mut snapshot.archive);
    if dirty {
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_by(|a, b| {
            a.source
                .agent
                .as_str()
                .cmp(b.source.agent.as_str())
                .then(a.source.file.cmp(&b.source.file))
                .then(a.source.cwd_override.cmp(&b.source.cwd_override))
        });
        Catalog {
            version: CATALOG_VERSION,
            entries,
        }
        .save(&cache_path)?;
    }
    Ok(snapshot)
}

/// Fully refresh exactly one source, preserving its native id, account path and view flag.
///
/// # Errors
/// Returns an error for a missing/unreadable or invalid source, changed native
/// id/cwd/provider, or any source that the provider parser cannot refresh.
pub fn hydrate(session: &Session) -> Result<Session> {
    let source = SessionSource {
        agent: session.agent,
        file: session.file.clone(),
        cwd_override: Some(session.cwd.clone()),
    };
    let mut full = discovery::parse_source(&source, ParseMode::Full)
        .with_context(|| format!("reading session source {}", session.file.display()))?
        .with_context(|| format!("cannot hydrate session source {}", session.file.display()))?;
    ensure!(
        full.id == session.id
            && full.agent == session.agent
            && full.file == session.file
            && full.cwd == session.cwd,
        "session source identity changed at {}",
        session.file.display()
    );
    full.archived = session.archived;
    Ok(full)
}
