//! Share Codex preferences without sharing its credentials or session store.
//!
//! `CODEX_HOME` relocates both config and authentication. MindPlayer keeps the
//! home isolated, but connects its config to the user's canonical `~/.codex`
//! config before each launch. Old account-local configs remain recoverable.

use std::io;
use std::path::Path;

/// Prepare only `config.toml`; never copy/link auth, history, or the whole home.
/// Missing canonical config leaves the account's existing setup untouched.
pub fn prepare(codex_home: &Path, home: &Path) -> io::Result<()> {
    let source = home.join(".codex/config.toml");
    let target = codex_home.join("config.toml");
    if source == target {
        return Ok(());
    }
    let canonical_source = match source.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !canonical_source.is_file() {
        return Err(io::Error::other("shared Codex config is not a file"));
    }
    if target.canonicalize().ok().as_ref() == Some(&canonical_source) {
        return Ok(());
    }
    #[cfg(unix)]
    {
        link_with_backup(&source, &target)
    }
    #[cfg(not(unix))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sharing Codex config requires symlink support",
        ))
    }
}

#[cfg(unix)]
fn link_with_backup(source: &Path, target: &Path) -> io::Result<()> {
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_BACKUP: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().expect("config has a parent");
    crate::private::create_dir_private(parent)?;
    // Serialize migrations by separate MindPlayer processes. Keep the lock
    // file: unlinking it would let another process lock a different inode.
    let lock = crate::private::open_private(&parent.join(".mindplayer-config.lock"), true)?;
    loop {
        // SAFETY: the file descriptor remains live until this function returns.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    // Another pane may have finished preparing this account while we waited.
    if target.canonicalize().ok() == Some(source.canonicalize()?) {
        return Ok(());
    }
    // Creating the private directory exclusively avoids collisions across
    // concurrent panes/processes and protects even an old world-readable config.
    let staging = loop {
        let candidate = parent.join(format!(
            ".mindplayer-config-backup-{}-{}",
            std::process::id(),
            NEXT_BACKUP.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => break candidate,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let previous = staging.join("config.toml");
    let replacement = staging.join("shared-config.toml");
    let result = (|| {
        match fs::symlink_metadata(target) {
            Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
                // Preserve the contents, not a relative symlink whose base
                // would change inside the backup directory. Do not truncate
                // the original, including when it points outside this slot.
                if !fs::metadata(target)?.is_file() {
                    return Err(io::Error::other(
                        "account Codex config is not a regular file",
                    ));
                }
                // Nonblocking open plus descriptor validation closes the race
                // where a checked symlink is replaced with a FIFO/device.
                let mut old = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
                    .open(target)?;
                if !old.metadata()?.is_file() {
                    return Err(io::Error::other(
                        "account Codex config is not a regular file",
                    ));
                }
                let mut backup = crate::private::open_private(&previous, false)?;
                io::copy(&mut old, &mut backup)?;
                backup.sync_all()?;
            }
            Ok(_) => return Err(io::Error::other("account Codex config is not a file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        symlink(source, &replacement)?;
        fs::rename(&replacement, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&replacement);
    }
    // An existing config leaves its backup directory behind. A fresh slot
    // needs no backup, so this removes only the now-empty staging directory.
    let _ = fs::remove_dir(&staging);
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let slot = home.join(".mindplayer/accounts/codex/other");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(&slot).unwrap();
        (tmp, home, slot)
    }

    #[test]
    fn shares_latest_config_but_preserves_auth_and_history() {
        let (_tmp, home, slot) = fixture();
        let source = home.join(".codex/config.toml");
        fs::write(&source, "model = \"latest\"\n").unwrap();
        fs::write(slot.join("config.toml"), "model = \"old\"\n").unwrap();
        fs::write(slot.join("auth.json"), "isolated credentials").unwrap();
        fs::write(home.join(".codex/auth.json"), "inherited credentials").unwrap();
        fs::create_dir(slot.join("sessions")).unwrap();
        fs::write(slot.join("sessions/session.jsonl"), "isolated history").unwrap();

        prepare(&slot, &home).unwrap();
        assert_eq!(fs::read_link(slot.join("config.toml")).unwrap(), source);
        let backups: Vec<_> = fs::read_dir(&slot)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mindplayer-config-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(backups[0].path().join("config.toml")).unwrap(),
            "model = \"old\"\n"
        );
        fs::write(&source, "model = \"newer\"\n").unwrap();
        prepare(&slot, &home).unwrap();
        assert_eq!(
            fs::read_to_string(slot.join("config.toml")).unwrap(),
            "model = \"newer\"\n"
        );
        assert_eq!(
            fs::read_to_string(slot.join("auth.json")).unwrap(),
            "isolated credentials"
        );
        assert_eq!(
            fs::read_to_string(slot.join("sessions/session.jsonl")).unwrap(),
            "isolated history"
        );
    }

    #[test]
    fn a_fresh_slot_does_not_inherit_credentials() {
        let (_tmp, home, slot) = fixture();
        fs::write(home.join(".codex/config.toml"), "model = \"latest\"\n").unwrap();
        fs::write(home.join(".codex/auth.json"), "inherited credentials").unwrap();
        prepare(&slot, &home).unwrap();
        assert!(slot.join("config.toml").is_symlink());
        assert!(!slot.join("auth.json").exists());
        assert_eq!(fs::read_dir(&slot).unwrap().count(), 2);
    }

    #[test]
    fn no_canonical_config_preserves_existing_preferences() {
        let (_tmp, home, slot) = fixture();
        fs::write(slot.join("config.toml"), "local").unwrap();
        prepare(&slot, &home).unwrap();
        assert_eq!(
            fs::read_to_string(slot.join("config.toml")).unwrap(),
            "local"
        );
        assert!(!slot.join("config.toml").is_symlink());
    }

    #[test]
    fn inherited_home_and_existing_shared_links_are_unchanged() {
        let (_tmp, home, slot) = fixture();
        let source = home.join(".codex/config.toml");
        fs::write(&source, "shared").unwrap();
        prepare(&home.join(".codex"), &home).unwrap();
        assert!(!source.is_symlink());
        std::os::unix::fs::symlink(&source, slot.join("config.toml")).unwrap();
        prepare(&slot, &home).unwrap();
        assert_eq!(fs::read_dir(&slot).unwrap().count(), 1);
    }

    #[test]
    fn a_directory_is_not_replaced_or_deleted() {
        let (_tmp, home, slot) = fixture();
        fs::write(home.join(".codex/config.toml"), "shared").unwrap();
        fs::create_dir(slot.join("config.toml")).unwrap();
        assert!(prepare(&slot, &home).is_err());
        assert!(slot.join("config.toml").is_dir());
        assert_eq!(fs::read_dir(&slot).unwrap().count(), 2);
    }

    #[test]
    fn relative_symlink_preferences_have_a_readable_backup() {
        let (_tmp, home, slot) = fixture();
        fs::write(home.join(".codex/config.toml"), "shared").unwrap();
        fs::write(slot.join("local.toml"), "local preferences").unwrap();
        std::os::unix::fs::symlink("local.toml", slot.join("config.toml")).unwrap();
        prepare(&slot, &home).unwrap();
        let backup = fs::read_dir(&slot)
            .unwrap()
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mindplayer-config-backup-")
            })
            .unwrap()
            .path()
            .join("config.toml");
        assert_eq!(fs::read_to_string(backup).unwrap(), "local preferences");
        assert_eq!(
            fs::read_to_string(slot.join("local.toml")).unwrap(),
            "local preferences"
        );
    }

    #[test]
    fn a_symlink_to_a_fifo_is_refused_without_opening_it() {
        use std::os::unix::ffi::OsStrExt;
        let (_tmp, home, slot) = fixture();
        fs::write(home.join(".codex/config.toml"), "shared").unwrap();
        let fifo = slot.join("pipe");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid, NUL-terminated path inside the test fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::symlink(&fifo, slot.join("config.toml")).unwrap();
        assert!(prepare(&slot, &home).is_err());
        assert_eq!(fs::read_link(slot.join("config.toml")).unwrap(), fifo);
    }

    #[test]
    fn concurrent_preparation_keeps_one_original_backup() {
        let (_tmp, home, slot) = fixture();
        fs::write(home.join(".codex/config.toml"), "shared").unwrap();
        fs::write(slot.join("config.toml"), "old preferences").unwrap();
        std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| prepare(&slot, &home)))
                .collect();
            for thread in threads {
                thread.join().unwrap().unwrap();
            }
        });
        let backups: Vec<_> = fs::read_dir(&slot)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mindplayer-config-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(backups[0].path().join("config.toml")).unwrap(),
            "old preferences"
        );
        assert_eq!(
            fs::read_to_string(slot.join("config.toml")).unwrap(),
            "shared"
        );
    }
}
