//! Command implementations and dispatch (spec section 3).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::{self, Cli, Command, ProxyArgs};
use crate::config::{self, ActiveSource, EnvMeta, GlobalConfig, ProjectConfig};
use crate::engine;
use crate::error::PristError;
use crate::git_ops;
use crate::ide;
use crate::paths::PristHome;
use crate::releases::{Platform, Release, ReleaseFeed};

use colored::Colorize;
use dialoguer::theme::SimpleTheme;
use dialoguer::Select;

type Result<T> = anyhow::Result<T>;

const BIN_DIR: &str = "bin";

/// Entry point: dispatch a parsed CLI invocation.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    init_tracing(cli.verbose);
    let home = cli::resolve_home(&cli)?;
    home.ensure()?;
    match cli.command {
        Command::Create {
            name,
            reference,
            precache,
        } => create(&home, name, reference, precache).await?,
        Command::Use { env, global } => use_env(&home, env, global)?,
        Command::Ls => ls(&home)?,
        Command::Releases => releases().await?,
        Command::Rm { env, force } => rm(&home, env, force)?,
        Command::Clean => clean(&home)?,
        Command::Doctor => doctor(&home)?,
        Command::Repair => repair(&home)?,
        Command::Update => update()?,
        Command::Upgrade { env, reference } => upgrade(&home, env, reference).await?,
        Command::Completions { shell } => completions(shell)?,
        Command::Flutter(args) => proxy(&home, "flutter", &args)?,
        Command::Dart(args) => proxy(&home, "dart", &args)?,
        Command::Pub(args) => proxy(&home, "pub", &args)?,
    }
    Ok(())
}

fn init_tracing(verbose: u8) {
    let filter = match verbose {
        0 => "prist=warn",
        1 => "prist=info",
        2 => "prist=debug",
        _ => "prist=trace",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("prist/", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .build()
        .expect("building reqwest client")
}

/// `prist create <name> [reference]`
async fn create(
    home: &PristHome,
    name: String,
    reference: Option<String>,
    precache: bool,
) -> Result<()> {
    let start = std::time::Instant::now();
    validate_env_name(&name)?;
    let env_path = home.env(&name);
    if env_path.exists() {
        return Err(PristError::EnvAlreadyExists(name).into());
    }

    let client = http_client();

    let target_ref = match &reference {
        Some(r) => r.clone(),
        None => {
            if resolve_release(&client, &name).await.is_ok() {
                name.clone()
            } else {
                "stable".to_string()
            }
        }
    };

    let release = resolve_release(&client, &target_ref).await?;
    let commit = release.commit_hash().map(|s| s.to_string());

    let version_label = release.version.as_deref().unwrap_or_else(|| {
        release
            .channel
            .as_deref()
            .unwrap_or(commit.as_deref().unwrap_or("HEAD"))
    });

    let bare = git_ops::ensure_bare(&home.git_bare(), commit.as_deref())?;
    git_ops::create_env_from_bare(&bare, &env_path, commit.as_deref())?;

    let engine_hash = git_ops::read_engine_version(&env_path);
    if let Some(hash) = &engine_hash {
        if precache {
            // Eagerly download engine + Dart SDK + platform artifacts.
            let _ = engine::ensure_engine(home, &env_path, hash);
        } else {
            // Only link if the engine is already in the shared cache;
            // otherwise defer the download to the first `prist flutter` call.
            let _ = engine::try_link_engine(home, &env_path, hash);
        }
    }

    let resolved_version = git_ops::read_flutter_version(&env_path).or(release.version.clone());

    let meta = EnvMeta {
        name: name.clone(),
        reference: Some(target_ref),
        channel: release.channel.clone(),
        version: resolved_version,
        commit: commit.clone(),
        engine_hash: engine_hash.clone(),
        created_at: Some(now_iso()),
    };
    meta.save(&env_path)?;

    // If no global default environment is active, automatically activate this new environment as global default.
    let global_cfg = GlobalConfig::load(home).ok();
    let has_global_default = global_cfg
        .as_ref()
        .and_then(|c| c.default_env.as_ref())
        .is_some();
    if !has_global_default {
        let _ = use_env(home, Some(name.clone()), true);
    }

    let elapsed = start.elapsed().as_secs_f32();
    println!(
        "{} Created '{}' ({}) {}",
        "✓".green().bold(),
        name.bold(),
        version_label,
        format!("({:.1?}s)", elapsed).dimmed()
    );
    Ok(())
}

/// `prist use [env] [--global]`
fn use_env(home: &PristHome, env: Option<String>, global: bool) -> Result<()> {
    let start = std::time::Instant::now();
    let target_env = match env {
        Some(e) => e,
        None => {
            let (active, _source) = config::resolve_active(home)?;
            match active {
                Some(e) => e,
                None => select_env_interactively(home, "? Select environment:")?,
            }
        }
    };

    let env_path = home.env(&target_env);
    if !env_path.is_dir() {
        return Err(PristError::EnvNotFound(target_env).into());
    }

    let engine_hash = git_ops::read_engine_version(&env_path);
    if let Some(hash) = &engine_hash {
        // Only link the engine if already cached; don't force a download
        // just because the user switched environments.
        if let Err(e) = engine::try_link_engine(home, &env_path, hash) {
            tracing::warn!(error = %e, "engine link on use failed");
        }
    }

    if global {
        let mut cfg = GlobalConfig::load(home)?;
        cfg.default_env = Some(target_env.clone());
        cfg.save(home)?;
        let _ = fs_unlink(&home.default_env_link());
        crate::fs_util::make_dir_link(&home.default_env_link(), &env_path)?;
        install_shims(home);
    } else {
        let rc = Path::new(crate::paths::PROJECT_CONFIG_NAME);
        let cfg = ProjectConfig {
            env: Some(target_env.clone()),
            flutter: EnvMeta::load(&env_path)
                .ok()
                .flatten()
                .and_then(|m| m.version.or(m.channel).or(m.commit)),
        };
        cfg.save(rc)?;
    }

    let project_root = std::env::current_dir().map_err(|e| PristError::msg(format!("cwd: {e}")))?;
    if let Err(e) = ide::integrate(home, &env_path, &project_root) {
        tracing::warn!(error = %e, "IDE integration partially failed");
    }

    let elapsed = start.elapsed().as_secs_f32();
    let scope = if global { "global default" } else { "project" };
    println!(
        "{} Activated {} ({}) {}",
        "✓".green().bold(),
        target_env.bold(),
        scope,
        format!("({:.2?}s)", elapsed).dimmed()
    );
    Ok(())
}

/// `prist ls`
fn ls(home: &PristHome) -> Result<()> {
    let global = GlobalConfig::load(home)?;
    let (active, source) = config::resolve_active(home)?;

    let mut rows: Vec<(String, String, String)> = Vec::new();

    for entry in std::fs::read_dir(home.envs())? {
        let entry = entry?;
        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        if name == "default" {
            continue;
        }
        let env_path = entry.path();
        let meta = EnvMeta::load(&env_path).ok().flatten();
        let descriptor = describe_meta(&meta, &env_path);
        let mut flags = String::new();
        if global.default_env.as_deref() == Some(&*name) {
            flags.push_str("global");
        }
        if active.as_deref() == Some(&*name) {
            if !flags.is_empty() {
                flags.push(',');
            }
            flags.push_str(match &source {
                ActiveSource::Project(_) => "active",
                ActiveSource::Global => "active*",
                ActiveSource::None => "",
            });
        }
        rows.push((name.into_owned(), descriptor, flags));
    }

    if rows.is_empty() {
        println!("{}", "No environments found.".dimmed());
        return Ok(());
    }

    let name_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(4).max(4);
    let desc_w = rows.iter().map(|r| r.1.len()).max().unwrap_or(7).max(7);

    for (name, desc, flags) in &rows {
        let is_active = flags.contains("active");
        let is_global = flags.contains("global");

        if is_active {
            println!(
                "{} {:<width1$}  {:<width2$}  {}",
                "✓".green().bold(),
                name.bold(),
                desc,
                "(active)".dimmed(),
                width1 = name_w,
                width2 = desc_w,
            );
        } else if is_global {
            println!(
                "  {:<width1$}  {:<width2$}  {}",
                name,
                desc,
                "(global)".dimmed(),
                width1 = name_w,
                width2 = desc_w,
            );
        } else {
            println!(
                "  {:<width1$}  {:<width2$}",
                name.dimmed(),
                desc.dimmed(),
                width1 = name_w,
                width2 = desc_w,
            );
        }
    }
    Ok(())
}

/// `prist releases`
async fn releases() -> Result<()> {
    let client = http_client();
    let feed = ReleaseFeed::fetch(&client, Platform::host()).await?;
    println!(
        "  {:<10} {:<16} {:<10} {}",
        "CHANNEL".bold(),
        "VERSION".bold(),
        "DATE".bold(),
        "COMMIT".bold()
    );
    println!("  {:-<10}  {:-<16}  {:-<10}  {:-<7}", "", "", "", "");
    for r in feed.releases.iter().take(50) {
        let ch = r.channel.as_deref().unwrap_or("-");
        let styled_ch = match ch {
            "stable" => ch.green().bold(),
            "beta" => ch.yellow().bold(),
            "dev" => ch.magenta(),
            _ => ch.normal(),
        };
        println!(
            "  {:<10} {:<16} {:<10} {}",
            styled_ch,
            r.version.as_deref().unwrap_or("-").bold(),
            r.release_date
                .as_deref()
                .unwrap_or("-")
                .get(..10)
                .unwrap_or("-")
                .dimmed(),
            r.hash.as_deref().unwrap_or("-").dimmed(),
        );
    }
    Ok(())
}

/// `prist rm [env] [--force]`
fn rm(home: &PristHome, env: Option<String>, force: bool) -> Result<()> {
    let start = std::time::Instant::now();
    let target_env = match env {
        Some(e) => e,
        None => select_env_interactively(home, "? Select an environment to remove:")?,
    };

    let env_path = home.env(&target_env);
    if !env_path.is_dir() {
        return Err(PristError::EnvNotFound(target_env).into());
    }
    if !force
        && !confirm(&format!(
            "Remove environment '{}'?",
            target_env.bold().red()
        ))
    {
        println!("  Aborted.");
        return Ok(());
    }
    crate::fs_util::remove_dir_all(&env_path)?;
    let mut cfg = GlobalConfig::load(home)?;
    if cfg.default_env.as_deref() == Some(target_env.as_str()) {
        cfg.default_env = None;
        cfg.save(home)?;
        let _ = fs_unlink(&home.default_env_link());
    }
    let elapsed = start.elapsed().as_secs_f32();
    println!(
        "{} Removed '{}' {}",
        "✓".green().bold(),
        target_env,
        format!("({:.2?}s)", elapsed).dimmed()
    );
    Ok(())
}

/// `prist clean`
fn clean(home: &PristHome) -> Result<()> {
    let start = std::time::Instant::now();
    let cwd = std::env::current_dir().map_err(|e| PristError::msg(format!("cwd: {e}")))?;
    let rc = cwd.join(crate::paths::PROJECT_CONFIG_NAME);
    let _ = std::fs::remove_file(&rc);
    if let Err(e) = ide::revert(&cwd) {
        tracing::warn!(error = %e, "IDE revert had issues");
    }
    let _ = home;
    let elapsed = start.elapsed().as_secs_f32();
    println!(
        "{} Cleaned project config {}",
        "✓".green().bold(),
        format!("({:.2?}s)", elapsed).dimmed()
    );
    Ok(())
}

/// `prist doctor`
fn doctor(home: &PristHome) -> Result<()> {
    let start = std::time::Instant::now();
    let mut issues = 0usize;

    let git_check = std::process::Command::new("git").arg("--version").output();
    if let Ok(out) = git_check {
        if out.status.success() {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let ver_clean = ver.trim_start_matches("git version ");
            println!("{} Git {}", "✓".green().bold(), ver_clean);
        } else {
            println!("{} Git check failed", "✗".red().bold());
            issues += 1;
        }
    } else {
        println!("{} Git missing", "✗".red().bold());
        issues += 1;
    }

    let bare = home.git_bare();
    if !git_ops::is_bare_repo(&bare) {
        println!("{} Bare repository missing", "✗".red().bold());
        issues += 1;
    } else {
        println!("{} Shared bare repository", "✓".green().bold());
    }

    for entry in std::fs::read_dir(home.envs())? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "default" {
            continue;
        }
        let env_path = entry.path();
        let alt_ok = git_ops::read_alternates(&env_path)
            .map(|alt| alt.iter().all(|p| p.join("pack").is_dir() || p.is_dir()))
            .unwrap_or(false);
        let flutter_ok = env_path.join(BIN_DIR).join("flutter").exists()
            || env_path.join(BIN_DIR).join("flutter.bat").exists();

        if alt_ok && flutter_ok {
            println!("{} Environment '{}'", "✓".green().bold(), name_str);
        } else {
            println!(
                "{} Environment '{}' issues found",
                "✗".red().bold(),
                name_str
            );
            issues += 1;
        }
    }

    let elapsed = start.elapsed().as_secs_f32();
    if issues == 0 {
        println!(
            "{} All checks passed {}",
            "✓".green().bold(),
            format!("({:.2?}s)", elapsed).dimmed()
        );
    } else {
        println!(
            "{} {} issue(s) found {}",
            "✗".yellow().bold(),
            issues,
            format!("({:.2?}s)", elapsed).dimmed()
        );
    }
    Ok(())
}

fn select_env_interactively(home: &PristHome, prompt_msg: &str) -> Result<String> {
    let mut envs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.envs()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name != "default" && entry.path().is_dir() {
                let meta = EnvMeta::load(&entry.path()).ok().flatten();
                let desc = describe_meta(&meta, &entry.path());
                envs.push((name, desc));
            }
        }
    }

    if envs.is_empty() {
        return Err(PristError::msg(
            "No environments installed yet. Run `prist create <name> <version>`.",
        )
        .into());
    }

    let items: Vec<String> = envs
        .iter()
        .map(|(n, d)| format!("{:<16}  ({})", n.bold(), d.dimmed()))
        .collect();

    if !std::io::stdout().is_terminal() {
        return Err(PristError::msg("Interactive selection requires a terminal. Please specify the environment name explicitly.").into());
    }

    println!("{}", prompt_msg.cyan().bold());
    let selection = Select::with_theme(&SimpleTheme)
        .items(&items)
        .default(0)
        .interact()?;

    Ok(envs[selection].0.clone())
}

/// `prist repair`
fn repair(home: &PristHome) -> Result<()> {
    let start = std::time::Instant::now();
    let bare = home.git_bare();
    if !git_ops::is_bare_repo(&bare) {
        git_ops::ensure_bare(&bare, None)?;
    } else {
        std::fs::write(
            bare.join("config"),
            format!(
                "{}\n[gc]\n\tauto = 0\n",
                std::fs::read_to_string(bare.join("config")).unwrap_or_default()
            ),
        )?;
    }
    let bare_objects = bare.join("objects");
    for entry in std::fs::read_dir(home.envs())? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "default" {
            continue;
        }
        let env_path = entry.path();

        let alt = git_ops::read_alternates(&env_path);
        let needs_alt_repair = alt.map(|a| !a.iter().all(|p| p.is_dir())).unwrap_or(true);
        if needs_alt_repair {
            git_ops::write_alternates(&env_path, &bare_objects)?;
        }

        let flutter_bin = env_path.join(BIN_DIR).join("flutter");
        let flutter_bat = env_path.join(BIN_DIR).join("flutter.bat");
        if !flutter_bin.exists() && !flutter_bat.exists() {
            let target_ref =
                git_ops::read_flutter_version(&env_path).unwrap_or_else(|| "stable".to_string());
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&env_path)
                .args(["checkout", "-f", &target_ref])
                .output();
        }

        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&env_path)
            .args(["remote", "set-url", "origin", git_ops::FLUTTER_REPO_URL])
            .output();

        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&env_path)
            .args([
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ])
            .output();

        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&env_path)
            .args(["update-ref", "refs/remotes/origin/stable", "HEAD"])
            .output();

        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&env_path)
            .args(["checkout", "-B", "stable"])
            .output();

        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&env_path)
            .args(["branch", "--set-upstream-to", "origin/stable"])
            .output();
    }
    let elapsed = start.elapsed().as_secs_f32();
    println!(
        "{} Repaired environments {}",
        "✓".green().bold(),
        format!("({:.2?}s)", elapsed).dimmed()
    );
    Ok(())
}

/// `prist update` (Phase 4): self-update from GitHub releases.
fn update() -> Result<()> {
    let start = std::time::Instant::now();
    let current = env!("CARGO_PKG_VERSION");

    let updater = self_update::backends::github::Update::configure()
        .repo_owner("SnaidenP")
        .repo_name("prist")
        .bin_name("prist")
        .show_download_progress(false)
        .show_output(false)
        .no_confirm(true)
        .current_version(current)
        .build()
        .map_err(|e| PristError::msg(format!("self_update config: {e}")))?;

    let status = updater
        .update()
        .map_err(|e| PristError::msg(format!("self_update: {e}")))?;

    let elapsed = start.elapsed().as_secs_f32();
    if status.updated() {
        println!(
            "{} Updated {} → {} {}",
            "✓".green().bold(),
            format!("v{current}").dimmed(),
            format!("v{}", status.version()).green().bold(),
            format!("({elapsed:.1}s)").dimmed()
        );
    } else {
        println!(
            "{} Already up to date {}",
            "✓".green().bold(),
            format!("(v{current})").dimmed()
        );
    }
    Ok(())
}

/// `prist completions <shell>`
fn completions(shell: clap_complete::Shell) -> Result<()> {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "prist", &mut std::io::stdout());
    Ok(())
}

/// `prist upgrade [env] [reference]`
async fn upgrade(home: &PristHome, env: Option<String>, reference: Option<String>) -> Result<()> {
    let start = std::time::Instant::now();
    let target_env = match env {
        Some(e) => e,
        None => {
            let (active, _source) = config::resolve_active(home)?;
            match active {
                Some(e) => e,
                None => select_env_interactively(home, "? Select environment to upgrade:")?,
            }
        }
    };

    let env_path = home.env(&target_env);
    if !env_path.is_dir() {
        return Err(PristError::EnvNotFound(target_env).into());
    }

    let meta = EnvMeta::load(&env_path).ok().flatten();
    let target_ref = match reference {
        Some(r) => r,
        None => meta
            .as_ref()
            .and_then(|m| m.reference.clone().or(m.channel.clone()))
            .unwrap_or_else(|| "stable".to_string()),
    };

    let client = http_client();
    let release = resolve_release(&client, &target_ref).await?;
    let commit = release.commit_hash().map(|s| s.to_string());

    let version_label = release.version.as_deref().unwrap_or_else(|| {
        release
            .channel
            .as_deref()
            .unwrap_or(commit.as_deref().unwrap_or("HEAD"))
    });

    let _bare = git_ops::ensure_bare(&home.git_bare(), commit.as_deref())?;

    let checkout_ref = commit.as_deref().unwrap_or(&target_ref);
    let checkout_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&env_path)
        .args(["checkout", "-f", checkout_ref])
        .status()
        .map_err(|e| PristError::msg(format!("failed to checkout {checkout_ref}: {e}")))?;

    if !checkout_status.success() {
        return Err(PristError::msg(format!("git checkout {checkout_ref} failed")).into());
    }

    let engine_hash = git_ops::read_engine_version(&env_path);
    if let Some(hash) = &engine_hash {
        let _ = engine::ensure_engine(home, &env_path, hash);
    }

    let resolved_version = git_ops::read_flutter_version(&env_path).or(release.version.clone());

    let new_meta = EnvMeta {
        name: target_env.clone(),
        reference: Some(target_ref),
        channel: release.channel.clone(),
        version: resolved_version,
        commit: commit.clone(),
        engine_hash: engine_hash.clone(),
        created_at: meta
            .as_ref()
            .and_then(|m| m.created_at.clone())
            .or_else(|| Some(now_iso())),
    };
    new_meta.save(&env_path)?;

    let elapsed = start.elapsed().as_secs_f32();
    println!(
        "{} Upgraded '{}' to {} {}",
        "✓".green().bold(),
        target_env.bold(),
        version_label,
        format!("({:.1?}s)", elapsed).dimmed()
    );
    Ok(())
}

/// `prist flutter|dart|pub <args>` — transparent proxy (spec 3).
fn proxy(home: &PristHome, tool: &str, args: &ProxyArgs) -> Result<()> {
    let (env_name, _source) = config::resolve_active(home)?;
    let env_path = match env_name {
        Some(name) => {
            let p = home.env(&name);
            if !p.is_dir() {
                return Err(PristError::EnvNotFound(name).into());
            }
            p
        }
        None => {
            // Fall back to default link target first
            match crate::fs_util::read_dir_link(&home.default_env_link())? {
                Some(p) if p.is_dir() => p,
                _ => {
                    let mut envs = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(home.envs()) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if name != "default" && entry.path().is_dir() {
                                envs.push(name);
                            }
                        }
                    }

                    if envs.is_empty() {
                        return Err(PristError::msg(
                            "No environments installed. Run `prist create <name> <version>`.",
                        )
                        .into());
                    }

                    if envs.len() == 1 {
                        let target = &envs[0];
                        let _ = use_env(home, Some(target.clone()), true);
                        home.env(target)
                    } else if std::io::stdout().is_terminal() {
                        let selected = select_env_interactively(
                            home,
                            "? No active environment. Select one to activate:",
                        )?;
                        let _ = use_env(home, Some(selected.clone()), true);
                        home.env(&selected)
                    } else {
                        return Err(PristError::msg(
                            "no active environment. Run `prist use <name>`.",
                        )
                        .into());
                    }
                }
            }
        }
    };

    let engine_hash = git_ops::read_engine_version(&env_path);
    if let Some(hash) = &engine_hash {
        if let Err(e) = engine::ensure_engine(home, &env_path, hash) {
            tracing::warn!(error = %e, "engine auto-population on proxy failed");
        }
    }

    let (program, extra_arg) = resolve_tool(&env_path, tool);
    if !program.exists() {
        return Err(PristError::msg(format!(
            "{} not found in {} (is the env fully created?)",
            tool,
            env_path.display()
        ))
        .into());
    }

    let mut cmd = std::process::Command::new(&program);
    if let Some(a) = extra_arg {
        cmd.arg(a);
    }
    for a in &args.args {
        cmd.arg(a);
    }
    cmd.env("FLUTTER_ROOT", &env_path);
    cmd.env("FLUTTER_GIT_URL", crate::git_ops::FLUTTER_REPO_URL);
    // Prepend the env's bin/ (and dart-sdk bin/) to PATH so proxied tools find dart.
    // Also remove the prist shim bin directory from PATH so Flutter doesn't see
    // our shim scripts and warn that `flutter`/`dart` on PATH aren't inside the
    // SDK checkout.
    let path = std::env::var("PATH").unwrap_or_default();
    let path_sep = if cfg!(windows) { ";" } else { ":" };
    let prist_bin = home.root().join(BIN_DIR);
    let filtered_path: String = path
        .split(path_sep)
        .filter(|entry| {
            let entry_path = Path::new(entry);
            // Compare canonicalized paths when possible, fall back to
            // case-insensitive comparison on Windows.
            if let (Ok(a), Ok(b)) = (entry_path.canonicalize(), prist_bin.canonicalize()) {
                a != b
            } else {
                #[cfg(windows)]
                {
                    !entry.eq_ignore_ascii_case(&prist_bin.to_string_lossy())
                }
                #[cfg(not(windows))]
                {
                    entry_path != prist_bin
                }
            }
        })
        .collect::<Vec<_>>()
        .join(path_sep);
    let new_path = format!(
        "{}{}{}{}{}",
        env_path.join(BIN_DIR).display(),
        path_sep,
        env_path
            .join(BIN_DIR)
            .join("cache")
            .join("dart-sdk")
            .join(BIN_DIR)
            .display(),
        path_sep,
        filtered_path
    );
    cmd.env("PATH", new_path);
    cmd.env("FLUTTER_ALREADY_WAITED", "true");
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    let status = cmd
        .status()
        .map_err(|e| PristError::msg(format!("failed to run {}: {e}", program.display())))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Resolve the on-disk program for a proxied tool. `pub` is delegated to
/// `dart pub`. Returns (program, optional leading arg).
fn resolve_tool(env_path: &Path, tool: &str) -> (PathBuf, Option<&'static str>) {
    match tool {
        "flutter" => (flutter_bin(env_path), None),
        "dart" => (dart_bin(env_path), None),
        "pub" => (dart_bin(env_path), Some("pub")),
        _ => (env_path.join(BIN_DIR).join(tool), None),
    }
}

#[cfg(unix)]
fn flutter_bin(env_path: &Path) -> PathBuf {
    env_path.join(BIN_DIR).join("flutter")
}
#[cfg(windows)]
fn flutter_bin(env_path: &Path) -> PathBuf {
    env_path.join(BIN_DIR).join("flutter.bat")
}

fn dart_bin(env_path: &Path) -> PathBuf {
    let candidates = if cfg!(windows) {
        vec![
            env_path.join(BIN_DIR).join("dart.exe"),
            env_path.join(BIN_DIR).join("dart.bat"),
            env_path
                .join(BIN_DIR)
                .join("cache")
                .join("dart-sdk")
                .join(BIN_DIR)
                .join("dart.exe"),
        ]
    } else {
        vec![
            env_path.join(BIN_DIR).join("dart"),
            env_path
                .join(BIN_DIR)
                .join("cache")
                .join("dart-sdk")
                .join(BIN_DIR)
                .join("dart"),
        ]
    };

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    candidates[0].clone()
}

// --- helpers ---------------------------------------------------------------

async fn resolve_release(client: &reqwest::Client, reference: &str) -> Result<Release> {
    // Skip the network fetch for commit hashes and the master channel.
    if reference == "master" {
        return Ok(Release::master());
    }
    if crate::releases::looks_like_commit_hash(reference) {
        return Ok(Release::for_commit(reference));
    }
    let feed = ReleaseFeed::fetch(client, Platform::host()).await?;
    Ok(feed.resolve(reference)?)
}

fn validate_env_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "default"
        || name.contains(std::path::MAIN_SEPARATOR)
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(PristError::msg(
            "invalid env name: must be non-empty, not 'default', and contain no path separators",
        )
        .into());
    }
    Ok(())
}

#[allow(dead_code)]
fn describe_release(release: &Release, commit: Option<&str>) -> String {
    if let Some(v) = &release.version {
        return v.clone();
    }
    if let Some(c) = &release.channel {
        return c.clone();
    }
    commit.unwrap_or("HEAD").to_string()
}

fn describe_meta(meta: &Option<EnvMeta>, env_path: &Path) -> String {
    if let Some(m) = meta {
        if let Some(v) = &m.version {
            return v.clone();
        }
        if let Some(c) = &m.channel {
            return c.clone();
        }
        if let Some(commit) = &m.commit {
            return format!("{}…", &commit[..7.min(commit.len())]);
        }
    }
    // Fall back to reading the version from the checkout.
    git_ops::read_flutter_version(env_path).unwrap_or_else(|| "unknown".into())
}

fn confirm(prompt: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{} [y/N] ", prompt);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

#[cfg(unix)]
fn fs_unlink(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
}
#[cfg(windows)]
fn fs_unlink(path: &Path) -> std::io::Result<()> {
    // A junction is removed with rmdir, not unlink.
    std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path))
}
/// Create/update shim scripts in the prist bin directory so `flutter`,
/// `dart`, and `pub` are available directly from any terminal.
///
/// The prist bin dir (e.g. `%LOCALAPPDATA%\prist\bin`) is already on the
/// user's PATH from the installer. We write `.bat` shims there that forward
/// to the active env's binaries via `prist flutter`, `prist dart`, etc.
fn install_shims(home: &PristHome) {
    let bin_dir = home.root().join(BIN_DIR);
    let _ = std::fs::create_dir_all(&bin_dir);

    // Windows .bat shims
    #[cfg(windows)]
    {
        let shims = [
            ("flutter.bat", "flutter"),
            ("dart.bat", "dart"),
            ("pub.bat", "pub"),
        ];
        for (file, tool) in shims {
            let path = bin_dir.join(file);
            let content = format!("@echo off\r\nprist {tool} %*\r\n");
            let _ = std::fs::write(&path, content);
        }
    }

    // Unix shell shims
    #[cfg(unix)]
    {
        let shims = ["flutter", "dart", "pub"];
        for tool in shims {
            let path = bin_dir.join(tool);
            let content = format!("#!/bin/sh\nexec prist {tool} \"$@\"\n");
            let _ = std::fs::write(&path, content);
            let _ = std::fs::set_permissions(
                &path,
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            );
        }
    }
}
