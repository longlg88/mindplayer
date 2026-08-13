//! Keeping mindplayer's own records readable only by their owner.
//!
//! Everything under `~/.mindplayer` is a record of the user's sessions: the
//! conversation log holds whole transcripts, the state file holds what each
//! session is called, the audit log holds when they worked. The default `umask`
//! of 022 made all of it world-readable.
//!
//! Every writer goes through here: the conversation log, the state file, the
//! audit log, handoff artifacts, PTY stderr logs, the staged credential config,
//! and the prompt directory. Four hand-rolled versions of this rule used to sit
//! in as many files, which is how the conversation log ended up being the one
//! that missed it.
//!
//! Two separate jobs, deliberately kept apart:
//!
//! - [`open_private`] creates *new* files owner-only. It never changes anything
//!   that already exists, because the paths it is handed are not always ours:
//!   `MINDPLAYER_STATE`, `MINDPLAYER_AUDIT` and `MINDPLAYER_CONVO_DIR` can point
//!   anywhere, and tests point them at the system temp directory. An earlier
//!   version narrowed the parent of every path it opened, which turned a `1777`
//!   `/tmp` into a `0700` directory owned by whoever ran mindplayer first.
//! - [`migrate_data_root`] narrows what is already on disk, and only inside
//!   mindplayer's own directory. That is the one place a chmod is warranted,
//!   because rewriting the thousands of transcripts written before this rule
//!   existed is not an option — making the directory unenterable is.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Directory mode: the owner may enter and list, nobody else.
pub const DIR_MODE: u32 = 0o700;
/// File mode: the owner may read and write, nobody else.
pub const FILE_MODE: u32 = 0o600;

/// `~/.mindplayer` — the directory this module is allowed to narrow.
pub fn data_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".mindplayer")
}

/// Create `dir` and any missing parents, owner-only.
///
/// The mode applies to directories this call creates and to those only:
/// `DirBuilder` sets it at creation time, so a directory that was already there
/// keeps whatever it had.
pub fn create_dir_private(dir: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(DIR_MODE);
    }
    b.create(dir)
}

/// Open `path` for writing as an owner-only file.
///
/// The mode applies on creation, so a file that already exists keeps its own —
/// [`migrate_data_root`] is what fixes those. `O_NOFOLLOW` refuses a symlink
/// outright rather than writing through it, the same guard `limits.rs` spells
/// out for its credential file.
pub fn open_private(path: &Path, append: bool) -> io::Result<fs::File> {
    match open_at(path, append) {
        // The directory is missing far more rarely than it is present, so it is
        // created on the failure path rather than stat-ed on every write.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                create_dir_private(dir)?;
            }
            open_at(path, append)
        }
        other => other,
    }
}

fn open_at(path: &Path, append: bool) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.create(true);
    if append {
        opts.append(true);
    } else {
        opts.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(FILE_MODE);
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
}

/// Create `path` as a brand-new owner-only file, failing if anything is already
/// there.
///
/// For files that are written once and never appended to — a handoff artifact,
/// a staged credential config. `O_EXCL` refuses to open a path someone else
/// pre-created, which is what keeps a pre-made `0666` file (or a symlink to
/// one) from receiving the contents at its own mode.
pub fn create_new_private(path: &Path) -> io::Result<fs::File> {
    if let Some(dir) = path.parent() {
        create_dir_private(dir)?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(FILE_MODE);
    }
    opts.open(path)
}

/// Narrow mindplayer's own directory and everything directly inside it.
///
/// Run once at startup. Files written before this rule existed keep their old
/// mode — there can be thousands of them — but a `0700` directory is enough:
/// nobody else can enter it to reach them. Subdirectories (`convo`, `handoffs`,
/// `prompts`) are narrowed too, for the same reason.
///
/// Failures are returned, not swallowed, so a caller can say so; nothing here
/// is fatal to starting up.
pub fn migrate_data_root(root: &Path) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    tighten(root)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            tighten(&entry.path())?;
        }
    }
    Ok(())
}

/// Narrow one existing path to owner-only, leaving setuid/setgid/sticky alone.
///
/// A missing path is not an error — callers use this to fix up whatever is
/// there. Anything else (a directory we cannot traverse, say) is, because the
/// file staying world-readable is exactly what the caller needs to hear about.
pub fn tighten(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let current = meta.permissions().mode();
        let want = if meta.is_dir() { DIR_MODE } else { FILE_MODE };
        // Only the permission bits are replaced. Masking the comparison but
        // writing the whole mode is how the first version silently dropped the
        // sticky bit off every directory it touched.
        let next = (current & !0o777) | want;
        if current != next {
            fs::set_permissions(path, fs::Permissions::from_mode(next))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mp-private-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn mode_of(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn a_new_file_and_the_directories_made_for_it_are_owner_only() {
        let dir = scratch("new");
        let nested = dir.join("a").join("b");
        let file = nested.join("log.jsonl");
        let _f = open_private(&file, true).unwrap();
        for d in [&dir, &dir.join("a"), &nested] {
            assert_eq!(mode_of(d), DIR_MODE, "{}", d.display());
        }
        assert_eq!(mode_of(&file), FILE_MODE);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The bug that made the first version dangerous: it narrowed the parent of
    /// whatever path it was handed. Those paths are not always ours — the env
    /// overrides and the tests point them at shared directories — so a `1777`
    /// temp directory came back `0700` with its sticky bit gone.
    #[test]
    fn opening_a_file_never_touches_a_directory_that_already_existed() {
        let dir = scratch("foreign");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o1777)).unwrap();
        let before = mode_of(&dir);

        let _f = open_private(&dir.join("x.jsonl"), true).unwrap();

        assert_eq!(
            mode_of(&dir),
            before,
            "an existing directory must be left alone"
        );
        assert_eq!(before, 0o1777, "including its sticky bit");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlink_is_refused_rather_than_written_through() {
        let dir = scratch("symlink");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target");
        fs::write(&target, b"").unwrap();
        let link = dir.join("link.jsonl");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = open_private(&link, true).unwrap_err();
        assert!(
            matches!(err.raw_os_error(), Some(libc::ELOOP) | Some(libc::EMLINK)),
            "expected the open to refuse the symlink, got {err:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// What protects the transcripts written before this rule existed: the
    /// files keep their old mode, and the directory they sit in stops anyone
    /// else from reaching them.
    #[test]
    fn migrating_narrows_our_own_directory_and_its_subdirectories() {
        let root = scratch("migrate");
        let convo = root.join("convo");
        fs::create_dir_all(&convo).unwrap();
        let old = convo.join("old.jsonl");
        fs::write(&old, b"transcript").unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&convo, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o644)).unwrap();

        migrate_data_root(&root).unwrap();

        assert_eq!(mode_of(&root), DIR_MODE);
        assert_eq!(mode_of(&convo), DIR_MODE);
        assert_eq!(mode_of(&old), 0o644, "files are left as they are");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pre_created_file_is_refused_rather_than_written_at_its_own_mode() {
        let dir = scratch("exclusive");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artifact.md");
        fs::write(&path, b"planted").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

        let err = create_new_private(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"planted", "left untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_exclusive_file_is_owner_only() {
        let dir = scratch("exclusive-new");
        let path = dir.join("a").join("artifact.md");
        let _f = create_new_private(&path).unwrap();
        assert_eq!(mode_of(&path), FILE_MODE);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrating_a_missing_root_is_not_an_error() {
        assert!(migrate_data_root(&scratch("absent")).is_ok());
    }

    #[test]
    fn tightening_keeps_the_bits_above_the_permission_bits() {
        let dir = scratch("sticky");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o1755)).unwrap();
        tighten(&dir).unwrap();
        assert_eq!(
            mode_of(&dir),
            0o1700,
            "sticky bit survives, permissions narrow"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tightening_a_missing_path_is_not_an_error() {
        assert!(tighten(&scratch("gone").join("nope")).is_ok());
    }
}
