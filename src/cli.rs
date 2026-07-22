//! Command-line interface (spec section 3).
//!
//! Environments are addressed by a **user-chosen name**, not by version — so
//! `prist create music_app 3.0.1` creates env `music_app` at Flutter 3.0.1.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

#[derive(Debug, Parser)]
#[command(
    name = "prist",
    version,
    about = "A Flutter version manager written in Rust — fast, deduplicated, no symlinks on Windows.",
    long_about = "Prist installs and switches Flutter versions using a single shared bare git \
                  repository plus per-environment worktrees deduplicated via git alternates. \
                  See `prist <command> --help` for each command."
)]
pub struct Cli {
    /// Override the Prist home directory (default: %LOCALAPPDATA%\\prist on Windows,
    /// ~/.prist elsewhere).
    #[arg(long, env = "PRIST_HOME", global = true)]
    pub prist_home: Option<PathBuf>,

    /// Increase verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct ProxyArgs {
    /// Arguments forwarded verbatim to the proxied tool (flutter / dart / pub).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<std::ffi::OsString>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new named environment at a Flutter version / channel / commit.
    Create {
        /// Name to give this environment (e.g. `music_app`). Must be unique.
        name: String,
        /// Flutter reference: a version (`3.0.1`), a channel (`stable`/`beta`/
        /// `dev`/`master`), or a 40-char commit hash. Defaults to `stable`.
        reference: Option<String>,
    },

    /// Activate an environment in the current project (or globally with -g).
    Use {
        /// Name of the environment to activate.
        env: String,
        /// Set the environment as the global default instead of project-local.
        #[arg(short = 'g', long)]
        global: bool,
    },

    /// List installed environments, marking the global and project-active ones.
    Ls,

    /// Show a paginated table of available Flutter versions from the feed.
    Releases,

    /// Remove a local environment (the shared bare repo is left untouched).
    Rm {
        /// Name of the environment to remove.
        env: String,
        /// Skip the confirmation prompt.
        #[arg(short = 'f', long)]
        force: bool,
    },

    /// Remove Prist configuration from the current project (deletes `.pristrc`).
    Clean,

    /// Verify the integrity of the bare repo and per-env alternates.
    Doctor,

    /// Rebuild the bare repo and/or alternates when `doctor` reports issues.
    Repair,

    /// Self-update the `prist` binary from GitHub releases.
    Update,

    /// Print shell completion script for the given shell.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },

    /// Proxy to the active environment's `flutter`. Passes all args through.
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Flutter(ProxyArgs),

    /// Proxy to the active environment's `dart`. Passes all args through.
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Dart(ProxyArgs),

    /// Proxy to the active environment's `pub`. Passes all args through.
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Pub(ProxyArgs),
}

/// Resolve the Prist home from CLI override or the default discovery.
pub fn resolve_home(cli: &Cli) -> anyhow::Result<crate::paths::PristHome> {
    if let Some(p) = &cli.prist_home {
        return Ok(crate::paths::PristHome { root: p.clone() });
    }
    crate::paths::PristHome::find()
}
