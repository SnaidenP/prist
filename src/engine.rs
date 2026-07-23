//! Flutter engine caching (spec section 4.3).
//!
//! The engine artifacts normally downloaded into `bin/cache/` on the first
//! `flutter` invocation are stored once per engine revision under
//! `shared/engines/<hash>/` and linked (junction on Windows, symlink on Unix)
//! into each environment's `bin/cache/`. State files like `engine.stamp` are
//! written atomically (temp file + rename) so concurrent builds in a monorepo
//! never see a half-written stamp.

use std::path::{Path, PathBuf};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use crate::error::PristError;
use crate::fs_util;
use crate::paths::PristHome;
use crate::releases::Platform;

type Result<T> = anyhow::Result<T>;

/// Where the shared engine artifacts for `hash` live.
pub fn engine_dir(home: &PristHome, hash: &str) -> PathBuf {
    home.engine(hash)
}

fn stamp_path(engine_dir: &Path) -> PathBuf {
    engine_dir.join("engine.stamp")
}

/// Is the engine for `hash` already materialized in the shared store?
pub fn is_cached(home: &PristHome, hash: &str) -> bool {
    let dir = engine_dir(home, hash);
    dir.is_dir()
        && fs_util::read_if_exists(&stamp_path(&dir))
            .ok()
            .flatten()
            .map(|s| s.trim() == hash)
            .unwrap_or(false)
}

/// Atomically write the `engine.stamp` recording `hash`.
pub fn write_stamp(engine_dir: &Path, hash: &str) -> Result<()> {
    std::fs::create_dir_all(engine_dir)?;
    fs_util::atomic_write_str(&stamp_path(engine_dir), hash)?;
    Ok(())
}

/// Link an env's `bin/cache/` to the shared engine store for `hash`, so the
/// engine is shared instead of duplicated (junction on Windows, symlink on
/// Unix — see spec 4.3/4.4). We only link at the `bin/cache/` level, never
/// deeper, to avoid confusing Dart's internal tools (spec risk note 7).
pub fn link_engine_cache(home: &PristHome, env_path: &Path, hash: &str) -> Result<()> {
    let target = engine_dir(home, hash);
    let link = env_path.join("bin").join("cache");
    if link.exists() {
        fs_util::remove_dir_all(&link)?;
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    fs_util::make_dir_link(&link, &target)?;
    Ok(())
}

/// Try to link the engine cache if it already exists in the shared store.
/// Does **not** download anything — returns `Ok(false)` when the engine is not
/// yet cached so callers can decide whether to proceed.
pub fn try_link_engine(home: &PristHome, env_path: &Path, hash: &str) -> Result<bool> {
    if is_cached(home, hash) {
        link_engine_cache(home, env_path, hash)?;
        return Ok(true);
    }
    Ok(false)
}

/// Ensure the engine for `hash` is in the shared store and linked into `env`.
///
/// Strategy: let the environment's own `flutter` tool populate its `bin/cache/`
/// (Flutter's own download logic is the source of truth for URLs + checksums),
/// then relocate `bin/cache/*` into the shared store and link it back. This
/// keeps Prist correct across Flutter's ever-changing artifact layout while
/// still providing the dedup + atomic-stamp + link guarantees the spec wants.
pub fn ensure_engine(home: &PristHome, env_path: &Path, hash: &str) -> Result<()> {
    let dir = engine_dir(home, hash);
    if is_cached(home, hash) {
        link_engine_cache(home, env_path, hash)?;
        return Ok(());
    }

    tracing::info!(engine = hash, "populating engine cache via flutter tool");
    populate_via_flutter(env_path)?;

    std::fs::create_dir_all(&dir)?;
    relocate_cache(env_path, &dir)?;
    write_stamp(&dir, hash)?;
    link_engine_cache(home, env_path, hash)?;
    Ok(())
}

/// Run `<env>/bin/flutter --version` and `<env>/bin/flutter precache` so the Flutter tool
/// downloads its full engine, Dart SDK, and platform artifacts into `<env>/bin/cache/`.
fn populate_via_flutter(env_path: &Path) -> Result<()> {
    let flutter_name = if cfg!(windows) {
        "flutter.bat"
    } else {
        "flutter"
    };
    let flutter = env_path.join("bin").join(flutter_name);
    let flutter_str = flutter
        .to_str()
        .ok_or_else(|| PristError::msg("env path is not valid UTF-8"))?;

    // 1. Bootstrap Dart SDK & basic engine version stamp
    let status_ver = std::process::Command::new(flutter_str)
        .arg("--version")
        .arg("--suppress-analytics")
        .current_dir(env_path)
        .status()
        .map_err(|e| PristError::msg(format!("failed to run flutter --version: {e}")))?;
    if !status_ver.success() {
        return Err(anyhow::anyhow!(format!(
            "flutter --version exited with {status_ver}"
        )));
    }

    // 2. Precache full Flutter engine binaries, material fonts, sky_engine & platform artifacts
    let _ = std::process::Command::new(flutter_str)
        .arg("precache")
        .arg("--suppress-analytics")
        .current_dir(env_path)
        .status();

    Ok(())
}

/// Move the contents of `<env>/bin/cache/` into the shared engine dir.
fn relocate_cache(env_path: &Path, engine_dir: &Path) -> Result<()> {
    let cache = env_path.join("bin").join("cache");
    if !cache.is_dir() {
        return Err(anyhow::anyhow!(format!(
            "expected bin/cache at {} after flutter --version, found none",
            cache.display()
        )));
    }
    for entry in std::fs::read_dir(&cache)? {
        let entry = entry?;
        let from = entry.path();
        let name = entry.file_name();
        let to = engine_dir.join(&name);
        // Rename is O(1) on the same volume; fall back to recursive copy.
        if std::fs::rename(&from, &to).is_err() {
            copy_dir_recursive(&from, &to)?;
            fs_util::remove_dir_all(&from)?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// A single downloadable artifact with an optional SHA-256 for verification.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub relative_path: String,
    pub url: String,
    pub sha256: Option<String>,
}

/// Concurrently download a set of artifacts into `dest_root`, verifying each
/// checksum when provided. Returns the list of written paths. Uses tokio +
/// reqwest for parallelism (spec 4.3) and `indicatif` for progress.
pub async fn download_artifacts(
    client: &reqwest::Client,
    dest_root: &Path,
    artifacts: &[Artifact],
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dest_root)?;
    let mp = MultiProgress::new();
    let style = ProgressStyle::with_template(
        "{prefix:<24} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec})",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar());

    let tasks = artifacts.iter().map(move |a| {
        let client = client.clone();
        let dest_root = dest_root.to_path_buf();
        let a = a.clone();
        let mp = mp.clone();
        let style = style.clone();
        tokio::spawn(async move {
            let dest = dest_root.join(&a.relative_path);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            let pb = mp.add(ProgressBar::new(0));
            pb.set_prefix(a.relative_path.clone());
            pb.set_style(style);

            let resp = client
                .get(&a.url)
                .send()
                .await
                .map_err(|e| PristError::msg(format!("GET {}: {e}", a.url)))?;
            if !resp.status().is_success() {
                return Err(anyhow::anyhow!(format!(
                    "{} returned HTTP {}",
                    a.url,
                    resp.status()
                )));
            }
            let total = resp.content_length().unwrap_or(0);
            pb.set_length(total);

            let mut hasher = Sha256::new();
            let mut file = tempfile::NamedTempFile::new_in(&dest_root)?;
            use futures_util::StreamExt;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| PristError::msg(format!("stream: {e}")))?;
                hasher.update(&chunk);
                pb.inc(chunk.len() as u64);
                std::io::Write::write_all(&mut file, &chunk)
                    .map_err(|e| PristError::msg(format!("write: {e}")))?;
            }
            pb.finish_and_clear();

            let got = hex::encode(hasher.finalize());
            if let Some(expected) = &a.sha256 {
                if got != *expected {
                    return Err(anyhow::anyhow!(format!(
                        "checksum mismatch for {}: expected {}, got {}",
                        a.relative_path, expected, got
                    )));
                }
            }
            file.persist(&dest)
                .map_err(|e| PristError::msg(format!("persist {}: {e}", dest.display())))?;
            Ok(dest)
        })
    });

    let mut written = Vec::new();
    for task in tasks {
        let path = task
            .await
            .map_err(|e| PristError::msg(format!("download task panicked: {e}")))??;
        written.push(path);
    }
    Ok(written)
}

/// Convenience: download a single file to `dest` with optional checksum.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    sha256: Option<&str>,
) -> Result<()> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| PristError::msg(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(format!(
            "{url} returned HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| PristError::msg(format!("read body: {e}")))?;

    if let Some(expected) = sha256 {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let got = hex::encode(hasher.finalize());
        if got != expected {
            return Err(anyhow::anyhow!(format!(
                "checksum mismatch for {url}: expected {expected}, got {got}"
            )));
        }
    }
    fs_util::atomic_write(dest, &bytes)?;
    Ok(())
}

/// The host platform's engine artifact bucket segment, used when constructing
/// direct engine URLs (e.g. `linux-x64`, `darwin-x64`, `windows-x64`).
pub fn host_engine_segment(platform: Platform) -> &'static str {
    match platform {
        Platform::Linux => "linux-x64",
        Platform::Macos => "darwin-x64",
        Platform::Windows => "windows-x64",
    }
}
