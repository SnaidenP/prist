//! Per-project (`.pristrc`) and global (`config.json`) configuration.
//!
//! `.pristrc` lives at a project's repo root and pins the active environment
//! name for that project. The global `config.json` records the global default
//! environment (used when no `.pristrc` is found walking up from the cwd).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::paths::PristHome;

/// Per-project config (`.pristrc`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Name of the Prist environment this project is pinned to.
    pub env: Option<String>,
    /// Optional explicit Flutter ref the project was originally pinned to,
    /// kept for documentation/`prist doctor` purposes.
    pub flutter: Option<String>,
}

impl ProjectConfig {
    /// Read `.pristrc` from `path`. Returns a default (empty) config if the
    /// file does not exist.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) if contents.trim().is_empty() => Ok(Self::default()),
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("parsing project config at {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("reading project config at {}", path.display()))
            }
        }
    }

    /// Atomically write this config to `path`.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        crate::fs_util::atomic_write(path, json.as_bytes())?;
        Ok(())
    }
}

/// Global Prist config (`$PRIST_HOME/config.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Name of the environment marked as the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_env: Option<String>,
}

impl GlobalConfig {
    pub fn load(home: &PristHome) -> anyhow::Result<Self> {
        let path = home.config_file();
        match std::fs::read_to_string(&path) {
            Ok(contents) if contents.trim().is_empty() => Ok(Self::default()),
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("parsing global config at {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("reading global config at {}", path.display()))
            }
        }
    }

    pub fn save(&self, home: &PristHome) -> anyhow::Result<()> {
        let path = home.config_file();
        let json = serde_json::to_string_pretty(self)?;
        crate::fs_util::atomic_write(&path, json.as_bytes())?;
        Ok(())
    }
}

/// A resolved active-environment name plus where it came from.
#[derive(Debug, Clone)]
pub enum ActiveSource {
    /// Resolved from a `.pristrc` at this path.
    Project(PathBuf),
    /// Resolved from the global config.
    Global,
    /// No env pinned anywhere; `default` link would be used if present.
    None,
}
/// Resolve the active environment name for the current working directory.
///
/// Resolution order (spec section 2): walk up from cwd for `.pristrc`; if none
/// found, fall back to the global default; otherwise return `None` (the
/// `envs/default` link may still point somewhere).
pub fn resolve_active(home: &PristHome) -> anyhow::Result<(Option<String>, ActiveSource)> {
    if let Some(rc) = crate::paths::find_project_config(Path::new(".")) {
        let cfg = ProjectConfig::load(&rc)?;
        if let Some(name) = cfg.env {
            return Ok((Some(name), ActiveSource::Project(rc)));
        }
    }
    let global = GlobalConfig::load(home)?;
    if let Some(name) = global.default_env {
        return Ok((Some(name), ActiveSource::Global));
    }
    Ok((None, ActiveSource::None))
}

/// Per-environment metadata, stored at `envs/<name>/.prist-meta.json`. The name
/// is the user-chosen env id; the resolved version/channel/commit are kept as
/// metadata (not part of the name), so renaming a version later won't break
/// the names people already use in their `.pristrc` (spec section 3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvMeta {
    pub name: String,
    /// The original user-facing reference (e.g. `3.0.1`, `beta`, a commit hash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Flutter source commit hash the env is checked out at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Engine revision (`bin/internal/engine.version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl EnvMeta {
    fn path(env_path: &Path) -> PathBuf {
        env_path.join(".prist-meta.json")
    }

    pub fn load(env_path: &Path) -> anyhow::Result<Option<Self>> {
        let p = Self::path(env_path);
        match std::fs::read_to_string(&p) {
            Ok(s) if s.trim().is_empty() => Ok(None),
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, env_path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        crate::fs_util::atomic_write(&Self::path(env_path), json.as_bytes())?;
        Ok(())
    }
}
