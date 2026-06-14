//! Embed a trustworthy version string at build time.
//!
//! A locally-built binary used to report the workspace `Cargo.toml` version
//! (`0.1.0`) no matter how new the source was, so `--version` couldn't tell you
//! whether you had the latest. Resolve it instead from, in order:
//!   1. `MINDPLAYER_VERSION` env override (CI / installer may set it),
//!   2. the crate's `CARGO_PKG_VERSION` for dirty local builds,
//!   3. `git describe --tags --always --dirty` (e.g. `0.2.2`, or
//!      `0.2.2-3-gabc123` when ahead of the last tag, `-dirty` if uncommitted),
//!   4. the crate's `CARGO_PKG_VERSION` (e.g. a source tarball with no git).
//!
//! The chosen value is exposed to the crate as `env!("MINDPLAYER_VERSION")`.

use std::path::Path;
use std::process::Command;

fn main() {
    // Rebuild when the checked-out commit or tags move so the version stays fresh.
    for p in ["../../.git/HEAD", "../../.git/refs/tags"] {
        if Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    println!("cargo:rerun-if-env-changed=MINDPLAYER_VERSION");

    let version = std::env::var("MINDPLAYER_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(version_from_git_or_package)
        .unwrap_or_else(|| cargo_pkg_version().unwrap_or_else(|| "0.0.0".into()));

    println!("cargo:rustc-env=MINDPLAYER_VERSION={version}");
}

fn version_from_git_or_package() -> Option<String> {
    let described = git_describe()?;
    if described.contains("-dirty") {
        cargo_pkg_version().or(Some(described))
    } else {
        Some(described)
    }
}

fn cargo_pkg_version() -> Option<String> {
    std::env::var("CARGO_PKG_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_start_matches('v')
        .to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
