//! Filesystem helpers: atomic writes and cross-platform directory links.
//!
//! Atomic writes (spec section 5: "Resiliencia"): every control file is
//! written to a sibling temp file then renamed, so a crash mid-write never
//! leaves a half-written stamp/config.
//!
//! Links (spec section 4.4):
//! - Unix: `std::os::unix::fs::symlink`
//! - Windows: Directory Junctions via the `junction` crate — never symlinks,
//!   so no developer mode / admin is required.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Atomically write `bytes` to `dest` by writing to a temp sibling then
/// renaming. On Windows the rename replaces an existing target atomically;
/// on Unix we unlink first if needed to match semantics.
pub fn atomic_write(dest: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dirs for {}", dest.display()))?;
    }
    let tmp = tmp_sibling(dest)?;
    std::fs::write(&tmp, bytes).with_context(|| format!("writing temp file {}", tmp.display()))?;

    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(dest);
    }

    // On Windows, `rename` replaces an existing file atomically (when both
    // are on the same volume, which is guaranteed since `tmp` is a sibling).
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

/// Atomically write a UTF-8 string.
pub fn atomic_write_str(dest: &Path, contents: &str) -> anyhow::Result<()> {
    atomic_write(dest, contents.as_bytes())
}

/// Read a small state file, returning `None` if it does not exist.
pub fn read_if_exists(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn tmp_sibling(dest: &Path) -> anyhow::Result<PathBuf> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid destination path {}", dest.display()))?;
    let tmp = dir.join(format!(".{}.tmp.{}", name, std::process::id()));
    Ok(tmp)
}

/// Create a directory link at `link` pointing at `target`.
///
/// On Unix this is a symlink; on Windows this is a Directory Junction (no
/// admin/developer-mode required).
pub fn make_dir_link(link: &Path, target: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_dir(link);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).with_context(|| {
            format!(
                "creating symlink {} -> {}",
                link.display(),
                target.display()
            )
        })?;
    }
    #[cfg(windows)]
    {
        // Directory Junctions — no privilege required. The `junction` crate
        // errors if the link path already exists, so we removed it above.
        if let Err(e) = junction::create(target, link) {
            return Err(anyhow::anyhow!(
                "creating junction {} -> {}: {}",
                link.display(),
                target.display(),
                e
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        return Err(anyhow::anyhow!(
            "directory linking is not supported on this platform"
        ));
    }
    Ok(())
}

/// Read the target of a directory link (symlink on Unix, junction on Windows).
pub fn read_dir_link(link: &Path) -> anyhow::Result<Option<PathBuf>> {
    #[cfg(unix)]
    {
        match std::fs::read_link(link) {
            Ok(p) => Ok(Some(p)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(windows)]
    {
        // `junction::exists` returns Ok(false) for a non-junction path (e.g. a
        // plain directory or a missing path), and `get_target` returns the
        // target even if the target itself no longer exists.
        match junction::exists(link) {
            Ok(true) => match junction::get_target(link) {
                Ok(p) => Ok(Some(p)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(anyhow::anyhow!(
                    "reading junction {}: {}",
                    link.display(),
                    e
                )),
            },
            Ok(false) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::anyhow!(
                "reading junction {}: {}",
                link.display(),
                e
            )),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(None)
    }
}

/// Recursively remove a directory, tolerating it not existing.
pub fn remove_dir_all(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}
