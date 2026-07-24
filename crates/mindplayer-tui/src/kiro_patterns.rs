//! Kiro-cli's screen-text fallback patterns, moved out of hardcoded Rust into
//! an optional data file — the other half of the herdr-inspired hook work.
//! Claude/Codex now get a confirmed status straight from an installed hook
//! (see `agent_hooks.rs`); kiro-cli has no approval-wait hook to switch to
//! (confirmed against its own docs), so its screen-text guess is what's
//! left. At least *that* can be data instead of code: adding support for a
//! new kiro-cli UI wording no longer requires recompiling and reinstalling
//! mindplayer — just editing `~/.mindplayer/kiro-patterns.json`.
//!
//! Deliberately NOT a general rule engine (no regions/priority/gate-nesting
//! like herdr's TOML manifests) — kiro's fallback is exactly two shapes of
//! rule (an AND/OR "blocked" match, an "contains any of these" idle match),
//! so that's all this expresses. `text_looks_busy`'s BUSY_MARKERS stay
//! hardcoded Rust: they're codex/claude wording, not kiro's, and out of
//! scope for this change.

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

/// One "blocked" shape: every `requires_all` substring must be present
/// somewhere in the tail, AND at least one `requires_any` substring must
/// also be present. Multiple rules are OR'd together — any one matching is
/// enough.
#[derive(Debug, Clone, Deserialize)]
pub struct BlockedRule {
    pub requires_all: Vec<String>,
    pub requires_any: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KiroPatterns {
    #[serde(default)]
    pub blocked: Vec<BlockedRule>,
    /// A bare-cursor-adjacent idle placeholder unique to kiro-cli's own input
    /// box (e.g. "Ask Kiro anything…") — checked case-insensitively against
    /// each already-lowercased tail line, alongside the shared prompt-cursor
    /// and codex/claude placeholder checks in `pty.rs::text_looks_idle`.
    #[serde(default)]
    pub idle_contains_any: Vec<String>,
}

impl Default for KiroPatterns {
    /// Reproduces today's hardcoded behavior exactly — this is the fallback
    /// when no override file exists, not a stripped-down default.
    fn default() -> Self {
        Self {
            blocked: vec![
                BlockedRule {
                    requires_all: vec!["requires approval".into()],
                    requires_any: vec![
                        "❯ yes".into(),
                        "trust, always allow".into(),
                        "no (tab to edit)".into(),
                    ],
                },
                BlockedRule {
                    requires_all: vec!["always allow in this session".into()],
                    requires_any: vec![
                        "❯ yes".into(),
                        "trust, always allow".into(),
                        "no (tab to edit)".into(),
                    ],
                },
            ],
            idle_contains_any: vec!["ask kiro".into()],
        }
    }
}

fn patterns_path() -> PathBuf {
    if let Ok(p) = std::env::var("MINDPLAYER_KIRO_PATTERNS") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("kiro-patterns.json")
}

static PATTERNS: OnceLock<KiroPatterns> = OnceLock::new();

/// Loaded once per process (kiro's UI doesn't change mid-run, so there's no
/// need to re-read on every classification tick). A missing or unparsable
/// override file silently falls back to [`KiroPatterns::default`] — this is
/// a convenience escape hatch, not something that should ever be able to
/// break status classification if the file is malformed.
pub fn patterns() -> &'static KiroPatterns {
    PATTERNS.get_or_init(|| {
        std::fs::read_to_string(patterns_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    })
}

/// `tail` is already trimmed+lowercased (see `pty.rs::bottom_lines`).
pub fn matches_blocked(tail: &[String]) -> bool {
    patterns().blocked.iter().any(|rule| {
        rule.requires_all
            .iter()
            .all(|needle| tail.iter().any(|l| l.contains(needle.as_str())))
            && rule
                .requires_any
                .iter()
                .any(|needle| tail.iter().any(|l| l.contains(needle.as_str())))
    })
}

/// `line` is already trimmed+lowercased.
pub fn matches_idle(line: &str) -> bool {
    patterns()
        .idle_contains_any
        .iter()
        .any(|needle| line.contains(needle.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_lowercase()).collect()
    }

    #[test]
    fn default_matches_todays_hardcoded_approval_shape() {
        let patterns = KiroPatterns::default();
        let matches = |t: &[String]| {
            patterns.blocked.iter().any(|rule| {
                rule.requires_all
                    .iter()
                    .all(|needle| t.iter().any(|l| l.contains(needle.as_str())))
                    && rule
                        .requires_any
                        .iter()
                        .any(|needle| t.iter().any(|l| l.contains(needle.as_str())))
            })
        };
        assert!(matches(&tail(&[
            "web_search requires approval",
            "❯ yes",
            "no (tab to edit)"
        ])));
        assert!(matches(&tail(&[
            "always allow in this session?",
            "trust, always allow"
        ])));
        assert!(
            !matches(&tail(&["requires approval"])),
            "needs a choice line too"
        );
        assert!(!matches(&tail(&["❯ yes"])), "needs an approval line too");
    }

    #[test]
    fn default_idle_matches_kiro_placeholder() {
        let patterns = KiroPatterns::default();
        assert!(patterns
            .idle_contains_any
            .iter()
            .any(|n| "ask kiro anything...".contains(n.as_str())));
    }

    #[test]
    fn malformed_override_falls_back_to_default_shape_not_a_crash() {
        // parse_or_default mirrors `patterns()`'s fallback without touching
        // the process-global OnceLock (so this test doesn't race others).
        let parse_or_default =
            |raw: &str| -> KiroPatterns { serde_json::from_str(raw).unwrap_or_default() };
        let d = parse_or_default("{not json");
        assert_eq!(d.blocked.len(), KiroPatterns::default().blocked.len());
    }

    #[test]
    fn override_can_replace_rules_entirely() {
        let raw = r#"{"blocked":[{"requires_all":["needs an ok"],"requires_any":["y/n"]}],"idle_contains_any":["type here"]}"#;
        let custom: KiroPatterns = serde_json::from_str(raw).unwrap();
        assert_eq!(custom.blocked.len(), 1);
        assert_eq!(custom.idle_contains_any, vec!["type here".to_string()]);
    }
}
