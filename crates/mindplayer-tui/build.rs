//! Embed a trustworthy version string at build time.
//!
//! A locally-built binary used to report the workspace `Cargo.toml` version
//! (`0.1.0`) no matter how new the source was, so `--version` couldn't tell you
//! whether you had the latest. Resolve it instead from, in order:
//!   1. `MINDPLAYER_VERSION` env override (CI / installer may set it),
//!   2. `git describe --tags --always --dirty` (e.g. `0.2.2`, or
//!      `0.2.2-3-gabc123` when ahead of the last tag, `-dirty` if uncommitted),
//!   3. the crate's `CARGO_PKG_VERSION` (e.g. a source tarball with no git).
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
        .or_else(git_describe)
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()));

    println!("cargo:rustc-env=MINDPLAYER_VERSION={version}");
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
