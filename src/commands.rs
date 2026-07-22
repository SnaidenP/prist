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

type Result<T> = anyhow::Result<T>;

const BIN_DIR: &str = "bin";

/// Entry point: dispatch a parsed CLI invocation.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    init_tracing(cli.verbose);
    let home = cli::resolve_home(&cli)?;
    home.ensure()?;
    match cli.command {
        Command::Create { name, reference } => create(&home, name, reference).await?,
        Command::Use { env, global } => use_env(&home, env, global)?,
        Command::Ls => ls(&home)?,
        Command::Releases => releases().await?,
        Command::Rm { env, force } => rm(&home, env, force)?,
        Command::Clean => clean(&home)?,
        Command::Doctor => doctor(&home)?,
        Command::Repair => repair(&home)?,
        Command::Update => update()?,
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
async fn create(home: &PristHome, name: String, reference: Option<String>) -> Result<()> {
    let reference = reference.as_deref().unwrap_or("stable");
    validate_env_name(&name)?;
    let env_path = home.env(&name);
    if env_path.exists() {
        return Err(PristError::EnvAlreadyExists(name).into());
    }

    let client = http_client();
    let release = resolve_release(&client, reference).await?;
    let commit = release.commit_hash().map(|s| s.to_string());

    println!(
        "Creating environment '{}' at Flutter {}",
        name,
        release.version.as_deref().unwrap_or_else(|| {
            release
                .channel
                .as_deref()
                .unwrap_or(commit.as_deref().unwrap_or("HEAD"))
        })
    );

    // Phase 2: dedup via the central bare repo + alternates.
    let bare = git_ops::ensure_bare(&home.git_bare(), commit.as_deref())?;
    git_ops::create_env_from_bare(&bare, &env_path, commit.as_deref())?;

    // Phase 2: shared engine cache + link.
    let engine_hash = git_ops::read_engine_version(&env_path);
    if let Some(hash) = &engine_hash {
        match engine::ensure_engine(home, &env_path, hash) {
            Ok(()) => tracing::info!(engine = hash, "engine ready"),
            Err(e) => {
                tracing::warn!(error = %e, "engine setup failed; flutter will populate it on first run")
            }
        }
    }

    let meta = EnvMeta {
        name: name.clone(),
        reference: Some(reference.to_string()),
        channel: release.channel.clone(),
        version: release.version.clone(),
        commit: commit.clone(),
        engine_hash: engine_hash.clone(),
        created_at: Some(now_iso()),
    };
    meta.save(&env_path)?;

    println!(
        "Created '{}' in {} ({}). Run `prist use {}` to activate it.",
        name,
        env_path.display(),
        describe_release(&release, commit.as_deref()),
        name
    );
    Ok(())
}

/// `prist use <env> [--global]`
fn use_env(home: &PristHome, env: String, global: bool) -> Result<()> {
    let env_path = home.env(&env);
    if !env_path.is_dir() {
        return Err(PristError::EnvNotFound(env).into());
    }

    if global {
        let mut cfg = GlobalConfig::load(home)?;
        cfg.default_env = Some(env.clone());
        cfg.save(home)?;
        // Update the `envs/default` junction/symlink to this env.
        let _ = fs_unlink(&home.default_env_link());
        crate::fs_util::make_dir_link(&home.default_env_link(), &env_path)?;
        println!("Set '{}' as the global default environment.", env);
    } else {
        let rc = Path::new(crate::paths::PROJECT_CONFIG_NAME);
        let cfg = ProjectConfig {
            env: Some(env.clone()),
            flutter: EnvMeta::load(&env_path)
                .ok()
                .flatten()
                .and_then(|m| m.version.or(m.channel).or(m.commit)),
        };
        cfg.save(rc)?;
        println!(
            "Activated '{}' for the current project (wrote {}).",
            env,
            rc.display()
        );
    }

    // Spec 4.5: `use` triggers IDE config mutation.
    let project_root = std::env::current_dir().map_err(|e| PristError::msg(format!("cwd: {e}")))?;
    if let Err(e) = ide::integrate(home, &env_path, &project_root) {
        tracing::warn!(error = %e, "IDE integration partially failed");
    }
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
        println!("No environments yet. Create one with `prist create <name> <version>`.");
        return Ok(());
    }

    let name_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(4).max(4);
    let desc_w = rows.iter().map(|r| r.1.len()).max().unwrap_or(7).max(7);
    println!(
        "{:<width1$}  {:<width2$}  FLAGS",
        "NAME",
        "FLUTTER",
        width1 = name_w,
        width2 = desc_w
    );
    for (name, desc, flags) in &rows {
        println!(
            "{:<width1$}  {:<width2$}  {}",
            name,
            desc,
            flags,
            width1 = name_w,
            width2 = desc_w
        );
    }
    Ok(())
}

/// `prist releases`
async fn releases() -> Result<()> {
    let client = http_client();
    let feed = ReleaseFeed::fetch(&client, Platform::host()).await?;
    println!("{:<10} {:<16} {:<10} COMMIT", "CHANNEL", "VERSION", "DATE");
    for r in feed.releases.iter().take(50) {
        println!(
            "{:<10} {:<16} {:<10} {}",
            r.channel.as_deref().unwrap_or("-"),
            r.version.as_deref().unwrap_or("-"),
            r.release_date
                .as_deref()
                .unwrap_or("-")
                .get(..10)
                .unwrap_or("-"),
            r.hash.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

/// `prist rm <env> [--force]`
fn rm(home: &PristHome, env: String, force: bool) -> Result<()> {
    let env_path = home.env(&env);
    if !env_path.is_dir() {
        return Err(PristError::EnvNotFound(env).into());
    }
    if !force && !confirm(&format!("Remove environment '{}'?", env)) {
        println!("aborted");
        return Ok(());
    }
    crate::fs_util::remove_dir_all(&env_path)?;
    // Clear global default if it pointed here.
    let mut cfg = GlobalConfig::load(home)?;
    if cfg.default_env.as_deref() == Some(env.as_str()) {
        cfg.default_env = None;
        cfg.save(home)?;
        let _ = fs_unlink(&home.default_env_link());
    }
    println!("Removed '{}'.", env);
    Ok(())
}

/// `prist clean`
fn clean(home: &PristHome) -> Result<()> {
    let cwd = std::env::current_dir().map_err(|e| PristError::msg(format!("cwd: {e}")))?;
    let rc = cwd.join(crate::paths::PROJECT_CONFIG_NAME);
    let _ = std::fs::remove_file(&rc);
    if let Err(e) = ide::revert(&cwd) {
        tracing::warn!(error = %e, "IDE revert had issues");
    }
    let _ = home;
    println!("Removed Prist configuration from {}.", cwd.display());
    Ok(())
}

/// `prist doctor`
fn doctor(home: &PristHome) -> Result<()> {
    let mut issues = 0usize;
    let bare = home.git_bare();
    if !git_ops::is_bare_repo(&bare) {
        println!("✗ bare repo missing at {}", bare.display());
        issues += 1;
    } else {
        let gc_ok = std::fs::read_to_string(bare.join("config"))
            .map(|c| c.contains("[gc]") && c.contains("auto"))
            .unwrap_or(false);
        if gc_ok {
            println!("✓ bare repo present, gc.auto disabled");
        } else {
            println!("✗ bare repo missing gc.auto=0 (run `prist repair`)");
            issues += 1;
        }
    }

    for entry in std::fs::read_dir(home.envs())? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "default" {
            continue;
        }
        let env_path = entry.path();
        let alt_ok = git_ops::read_alternates(&env_path)
            .map(|alt| alt.iter().all(|p| p.join("pack").is_dir() || p.is_dir()))
            .unwrap_or(false);
        let flutter_ok = env_path.join(BIN_DIR).join("flutter").exists()
            || env_path.join(BIN_DIR).join("flutter.bat").exists();
        let engine_ok = git_ops::read_engine_version(&env_path)
            .map(|h| engine::is_cached(home, &h))
            .unwrap_or(true);
        if alt_ok && flutter_ok && engine_ok {
            println!("✓ env '{}' healthy", name);
        } else {
            if !alt_ok {
                println!("✗ env '{}' has broken alternates", name);
            }
            if !flutter_ok {
                println!("✗ env '{}' missing bin/flutter", name);
            }
            if !engine_ok {
                println!("✗ env '{}' engine not cached", name);
            }
            issues += 1;
        }
    }
    if issues == 0 {
        println!("prist doctor: no issues found");
    } else {
        println!(
            "prist doctor: {} issue(s) found — run `prist repair`",
            issues
        );
    }
    Ok(())
}

/// `prist repair`
fn repair(home: &PristHome) -> Result<()> {
    let bare = home.git_bare();
    if !git_ops::is_bare_repo(&bare) {
        println!("Rebuilding bare repo…");
        git_ops::ensure_bare(&bare, None)?;
    } else {
        // Ensure gc.auto is set even on a pre-existing bare.
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
        if name.to_string_lossy() == "default" {
            continue;
        }
        let env_path = entry.path();
        let alt = git_ops::read_alternates(&env_path);
        let needs_repair = alt.map(|a| !a.iter().all(|p| p.is_dir())).unwrap_or(true);
        if needs_repair {
            println!("Repairing alternates for '{}'…", name.to_string_lossy());
            git_ops::write_alternates(&env_path, &bare_objects)?;
        }
    }
    println!("repair complete");
    Ok(())
}

/// `prist update` (Phase 4): self-update from GitHub releases.
fn update() -> Result<()> {
    let bin = std::env::current_exe().map_err(|e| PristError::msg(format!("current exe: {e}")))?;
    let status = self_update::backends::github::Update::configure()
        .repo_owner("SnaidenP")
        .repo_name("prist")
        .bin_name("prist")
        .show_download_progress(true)
        .show_output(true)
        .current_version(env!("CARGO_PKG_VERSION"))
        .build()
        .map_err(|e| PristError::msg(format!("self_update config: {e}")))?
        .update()
        .map_err(|e| PristError::msg(format!("self_update: {e}")))?;
    if status.updated() {
        println!("Updated prist to the latest release. Re-run your command.");
    } else {
        println!(
            "prist is already up to date ({}).",
            env!("CARGO_PKG_VERSION")
        );
    }
    let _ = bin;
    Ok(())
}

/// `prist completions <shell>`
fn completions(shell: clap_complete::Shell) -> Result<()> {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "prist", &mut std::io::stdout());
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
            // Fall back to the `default` link target.
            match crate::fs_util::read_dir_link(&home.default_env_link())? {
                Some(p) if p.is_dir() => p,
                _ => {
                    return Err(PristError::msg(
                        "no active environment. Run `prist use <name>` or `prist create <name> <version>`.",
                    ).into());
                }
            }
        }
    };

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
    // Prepend the env's bin/ (and dart-sdk bin/) to PATH so proxied tools find dart.
    let path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!(
        "{}{}{}{}{}",
        env_path.join(BIN_DIR).display(),
        std::path::MAIN_SEPARATOR,
        env_path
            .join(BIN_DIR)
            .join("cache")
            .join("dart-sdk")
            .join(BIN_DIR)
            .display(),
        std::path::MAIN_SEPARATOR,
        path
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
#[cfg(unix)]
fn dart_bin(env_path: &Path) -> PathBuf {
    env_path.join(BIN_DIR).join("dart")
}
#[cfg(windows)]
fn dart_bin(env_path: &Path) -> PathBuf {
    env_path.join(BIN_DIR).join("dart.exe")
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
