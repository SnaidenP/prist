//! Git operations via gitoxide (spec sections 4.2, 4.4).
//!
//! Two layers:
//! - **Central bare repo** (`shared/git_bare.git`): one full clone of
//!   `flutter/flutter`. `gc.auto 0` is forced so `git gc` never invalidates the
//!   alternates of derived environments.
//! - **Per-env worktree**: a local clone of the bare repo, with
//!   `.git/objects/info/alternates` pointing back at the bare object store
//!   (the dedup), checked out at the resolved release commit.
//!
//! Phase 1 also exposes [`clone_remote`] (clone the remote directly, no dedup)
//! for parity testing.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gix::clone::PrepareFetch;
use gix::open::Options as OpenOptions;
use gix::progress::Discard;

use crate::error::{PristError, Result};

/// The upstream Flutter repository.
pub const FLUTTER_REPO_URL: &str = "https://github.com/flutter/flutter.git";

fn flag() -> AtomicBool {
    AtomicBool::new(false)
}

/// Does `path` look like an existing bare repository? Bare repos have `HEAD`,
/// `objects`, `refs` and `config` directly under the repo dir, with no worktree.
pub fn is_bare_repo(path: &Path) -> bool {
    path.join("HEAD").is_file()
        && path.join("objects").is_dir()
        && path.join("refs").is_dir()
        && path.join("config").is_file()
}

/// Force `gc.auto 0` on the bare repo so automatic pruning can never remove
/// objects that derived environments reference via alternates (spec 4.2).
fn set_gc_auto_zero(bare_path: &Path) -> Result<()> {
    let config_path = bare_path.join("config");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    if existing.contains("[gc]") && existing.contains("auto") {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("[gc]\n\tauto = 0\n");
    std::fs::write(&config_path, text)?;
    Ok(())
}

/// Prepare a clone, fetch it, then check out `commit` (or the remote HEAD if
/// `commit` is `None`) into a worktree at `dst`.
fn fetch_and_checkout(mut prep: PrepareFetch, commit: Option<&str>) -> Result<()> {
    let (mut checkout, _outcome) = prep
        .fetch_then_checkout(Discard, &flag())
        .map_err(|e| PristError::msg(format!("git fetch failed: {e}")))?;

    // Detach HEAD to the target commit *before* checking out the worktree, so
    // `main_worktree` materializes exactly that commit. The commit is present in
    // the fetched history (release commits are ancestors of the default branch).
    if let Some(c) = commit {
        let git_dir = checkout.repo().git_dir().to_path_buf();
        std::fs::write(git_dir.join("HEAD"), format!("{c}\n"))?;
    }

    checkout
        .main_worktree(Discard, &flag())
        .map_err(|e| PristError::msg(format!("git checkout failed: {e}")))?;
    Ok(())
}

/// Phase 1: clone `url` directly into `dst` (worktree), checked out at `commit`.
pub fn clone_remote(url: &str, dst: &Path, commit: Option<&str>) -> Result<PathBuf> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let prep = PrepareFetch::new(
        url,
        dst,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        OpenOptions::default(),
    )
    .map_err(|e| PristError::msg(format!("preparing clone: {e}")))?;
    fetch_and_checkout(prep, commit)?;
    Ok(dst.to_path_buf())
}

/// Ensure the central bare repo exists and contains `commit`. Clones from the
/// upstream if missing; re-clones if the bare exists but is missing `commit`.
///
/// TODO: replace the re-clone with an incremental fetch via gix's connection
/// API so warm-cache installs stay close to O(1).
pub fn ensure_bare(bare_path: &Path, commit: Option<&str>) -> Result<PathBuf> {
    if let Some(parent) = bare_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if !is_bare_repo(bare_path) {
        clone_bare(bare_path)?;
    } else if let Some(c) = commit {
        let repo =
            gix::open(bare_path).map_err(|e| PristError::msg(format!("opening bare repo: {e}")))?;
        if !has_object(&repo, c) {
            tracing::info!(commit = c, "commit missing from bare; re-cloning");
            std::fs::remove_dir_all(bare_path)?;
            clone_bare(bare_path)?;
        }
    }
    set_gc_auto_zero(bare_path)?;
    Ok(bare_path.to_path_buf())
}

fn clone_bare(bare_path: &Path) -> Result<()> {
    tracing::info!(url = FLUTTER_REPO_URL, dest = %bare_path.display(), "bare cloning flutter");

    // Try gix first (in-process, no external dependency).
    let mut prep = PrepareFetch::new(
        FLUTTER_REPO_URL,
        bare_path,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        OpenOptions::default(),
    )
    .map_err(|e| PristError::msg(format!("preparing bare clone: {e}")))?;

    match prep.fetch_only(Discard, &flag()) {
        Ok((_repo, _outcome)) => return Ok(()),
        Err(e) => {
            tracing::warn!(error = %e, "gix bare fetch failed; falling back to system git");
            // Clean up any partial state left by gix before retrying with git.
            let _ = std::fs::remove_dir_all(bare_path);
        }
    }

    // Fallback: shell out to system `git clone --bare`.
    // System git handles proxies, TLS, retries, and large repos more robustly
    // than gix's built-in HTTP transport.
    println!("  Falling back to system git for clone...");
    let output = std::process::Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg("--progress")
        .arg(FLUTTER_REPO_URL)
        .arg(bare_path)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| PristError::msg(format!("failed to run git: {e}")))?;

    if !output.status.success() {
        return Err(PristError::msg(format!(
            "git clone --bare failed (exit {:?})",
            output.status.code()
        )));
    }

    Ok(())
}

/// Does the repository's object store contain the object with hex id `hash`?
fn has_object(repo: &gix::Repository, hash: &str) -> bool {
    let Ok(oid) = gix::ObjectId::from_hex(hash.as_bytes()) else {
        return false;
    };
    use gix::prelude::Find;
    let mut buf = Vec::new();
    matches!(repo.objects.try_find(&oid, &mut buf), Ok(Some(_)))
}

/// Phase 2: create a per-environment worktree at `env_path` as a local clone of
/// the bare repo, deduplicated via alternates, checked out at `commit`.
pub fn create_env_from_bare(
    bare_path: &Path,
    env_path: &Path,
    commit: Option<&str>,
) -> Result<PathBuf> {
    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracing::info!(src = %bare_path.display(), dest = %env_path.display(), "local clone for env");

    // Try gix first, then fall back to system git.
    let url = bare_path.to_string_lossy().into_owned();
    let prep = PrepareFetch::new(
        url.as_str(),
        env_path,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        OpenOptions::default(),
    )
    .map_err(|e| PristError::msg(format!("preparing env clone: {e}")))?;

    match fetch_and_checkout(prep, commit) {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(error = %e, "gix local clone failed; falling back to system git");
            let _ = std::fs::remove_dir_all(env_path);

            // Fallback: system git clone + checkout.
            let mut cmd = std::process::Command::new("git");
            cmd.arg("clone")
                .arg("--local")
                .arg(bare_path)
                .arg(env_path)
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());

            let output = cmd
                .output()
                .map_err(|e| PristError::msg(format!("failed to run git clone: {e}")))?;

            if !output.status.success() {
                return Err(PristError::msg(format!(
                    "git clone --local failed (exit {:?})",
                    output.status.code()
                )));
            }

            // Detach HEAD to the target commit.
            if let Some(c) = commit {
                let checkout = std::process::Command::new("git")
                    .arg("-C")
                    .arg(env_path)
                    .arg("checkout")
                    .arg("--detach")
                    .arg(c)
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .output()
                    .map_err(|e| PristError::msg(format!("failed to run git checkout: {e}")))?;

                if !checkout.status.success() {
                    return Err(PristError::msg(format!(
                        "git checkout {} failed (exit {:?})",
                        c,
                        checkout.status.code()
                    )));
                }
            }
        }
    }

    write_alternates(env_path, &bare_path.join("objects"))?;
    Ok(env_path.to_path_buf())
}

/// Write `.git/objects/info/alternates` in the env so it shares objects with
/// the bare repo (spec 4.2).
pub fn write_alternates(env_path: &Path, bare_objects: &Path) -> Result<()> {
    let alt_dir = env_path.join(".git").join("objects").join("info");
    std::fs::create_dir_all(&alt_dir)?;
    let alt_file = alt_dir.join("alternates");
    std::fs::write(&alt_file, format!("{}\n", bare_objects.display()))?;
    Ok(())
}

/// Read the alternates file for an env, if present.
pub fn read_alternates(env_path: &Path) -> Option<Vec<PathBuf>> {
    let alt = env_path
        .join(".git")
        .join("objects")
        .join("info")
        .join("alternates");
    std::fs::read_to_string(&alt)
        .ok()
        .map(|s| s.lines().map(PathBuf::from).collect())
}

/// Read the engine revision pinned by an env (`bin/internal/engine.version`).
pub fn read_engine_version(env_path: &Path) -> Option<String> {
    let p = env_path.join("bin").join("internal").join("engine.version");
    std::fs::read_to_string(&p)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read the Flutter version pinned by an env (`bin/internal/version` or the
/// `version` file under the checkout).
pub fn read_flutter_version(env_path: &Path) -> Option<String> {
    for rel in [
        "bin/internal/version",
        "version",
        "packages/flutter/pubspec.yaml",
    ] {
        let p = env_path.join(rel);
        if let Ok(s) = std::fs::read_to_string(&p) {
            if rel.ends_with("pubspec.yaml") {
                if let Some(line) = s.lines().find(|l| l.starts_with("version:")) {
                    return Some(line.trim_start_matches("version:").trim().to_string());
                }
            } else {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}
