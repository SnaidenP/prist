//! Path resolution for the Prist home layout and per-project config.
//!
//! Layout (see spec section 2):
//! ```text
//! ~/.prist/                       (Unix) | %LOCALAPPDATA%\prist\ (Windows)
//! ├── shared/
//! │   ├── git_bare.git/
//! │   └── engines/<engine_hash>/
//! ├── envs/<env_name>/
//! └── config.toml
//! ```

use std::path::{Path, PathBuf};

use anyhow::Context;

/// The resolved Prist home directory and its derived paths.
#[derive(Debug, Clone)]
pub struct PristHome {
    pub root: PathBuf,
}

impl PristHome {
    /// Locate the Prist home. Honors `PRIST_HOME` if set, otherwise falls back to
    /// `$LOCALAPPDATA\prist` on Windows and `$HOME/.prist` elsewhere.
    pub fn find() -> anyhow::Result<Self> {
        let root = if let Ok(custom) = std::env::var("PRIST_HOME") {
            PathBuf::from(custom)
        } else if let Ok(local) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local).join("prist")
        } else {
            dirs::home_dir()
                .context("could not determine the user home directory")?
                .join(".prist")
        };
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn shared(&self) -> PathBuf {
        self.root.join("shared")
    }

    /// Central bare git repository shared across every environment.
    pub fn git_bare(&self) -> PathBuf {
        self.shared().join("git_bare.git")
    }

    /// Per-engine-artifact store, indexed by commit hash.
    pub fn engines(&self) -> PathBuf {
        self.shared().join("engines")
    }

    pub fn engine(&self, hash: &str) -> PathBuf {
        self.engines().join(hash)
    }

    pub fn envs(&self) -> PathBuf {
        self.root.join("envs")
    }

    pub fn env(&self, name: &str) -> PathBuf {
        self.envs().join(name)
    }

    /// The `default` link that points at the globally active environment.
    pub fn default_env_link(&self) -> PathBuf {
        self.envs().join("default")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }

    /// Ensure the Prist home skeleton (`root`, `shared`, `shared/git_bare.git`,
    /// `shared/engines`, `envs`) exists.
    pub fn ensure(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.root())
            .with_context(|| format!("creating prist home at {}", self.root.display()))?;
        std::fs::create_dir_all(self.engines())
            .with_context(|| format!("creating engines dir at {}", self.engines().display()))?;
        std::fs::create_dir_all(self.envs())
            .with_context(|| format!("creating envs dir at {}", self.envs().display()))?;
        Ok(())
    }
}

/// Walk upward from `start` looking for a `.pristrc` file. Returns the path to
/// the first one found, or `None` up to the filesystem root.
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    loop {
        let candidate = current.join(".pristrc");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Name of the per-project config file written at the repo root.
pub const PROJECT_CONFIG_NAME: &str = ".pristrc";
