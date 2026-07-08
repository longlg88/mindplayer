//! User-editable prompt templates for "send this canned text into a live
//! session" features (catch-up, transition report, ...). Kept as plain text
//! files rather than compiled-in constants specifically so a prompt can be
//! rewritten at any time without a rebuild — the same sidecar philosophy as
//! `state.json`/`audit.jsonl`: read fresh off disk, never cached in memory
//! across the app's lifetime.

use std::path::{Path, PathBuf};

/// `~/.mindplayer/prompts/`, overridable via `MINDPLAYER_PROMPTS_DIR` (tests
/// redirect this so they never touch a real user's prompt files).
pub fn default_prompts_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MINDPLAYER_PROMPTS_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("prompts")
}

/// Load `<default_prompts_dir>/<name>.md`, seeding it with `default` on
/// first use.
pub fn load_prompt(name: &str, default: &str) -> String {
    load_prompt_from(&default_prompts_dir(), name, default)
}

/// Load `<dir>/<name>.md`. If the file doesn't exist yet, it's created with
/// `default`'s content so there is immediately something real to edit,
/// instead of the prompt only ever existing as a compiled-in string the
/// user has no way to find. Best-effort: any I/O failure (missing HOME,
/// permissions, read-only filesystem, ...) falls back to `default` in
/// memory rather than breaking the feature the prompt belongs to.
pub fn load_prompt_from(dir: &Path, name: &str, default: &str) -> String {
    let path = dir.join(format!("{name}.md"));
    if let Ok(existing) = std::fs::read_to_string(&path) {
        return existing.trim_end().to_string();
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&path, default);
    default.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mp-prompts-test-{name}-{}", std::process::id()))
    }

    #[test]
    fn load_prompt_seeds_the_file_with_the_default_on_first_use() {
        let dir = tmp_dir("seed");
        let text = load_prompt_from(&dir, "catchup", "hello default");
        assert_eq!(text, "hello default");
        let on_disk = std::fs::read_to_string(dir.join("catchup.md")).unwrap();
        assert_eq!(on_disk, "hello default");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_prompt_returns_edited_content_without_overwriting_it() {
        let dir = tmp_dir("edited");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("catchup.md"), "a user-edited prompt\n").unwrap();

        let text = load_prompt_from(&dir, "catchup", "the default, unused here");
        assert_eq!(text, "a user-edited prompt");
        // Loading again must not have touched the file back to the default.
        let on_disk = std::fs::read_to_string(dir.join("catchup.md")).unwrap();
        assert_eq!(on_disk, "a user-edited prompt\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_prompt_names_are_independent() {
        let dir = tmp_dir("independent");
        let a = load_prompt_from(&dir, "catchup", "catchup default");
        let b = load_prompt_from(&dir, "transition_report", "transition default");
        assert_eq!(a, "catchup default");
        assert_eq!(b, "transition default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
