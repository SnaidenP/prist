//! IDE integration (spec section 4.5).
//!
//! - **VS Code**: mutates `.vscode/settings.json` (JSONC-tolerant) to inject
//!   `dart.flutterSdkPath` and add watcher/search exclusions for Prist paths.
//! - **Android Studio / IntelliJ**: sets `flutter.sdk` in
//!   `android/local.properties` and injects `<ignored-roots>` into `.idea/`
//!   workspace XMLs to avoid aggressive reindexing.
//! - Appends Prist-managed entries to the project `.gitignore`.

use std::path::Path;

use anyhow::Context;
use serde_json::{Map, Value};

use crate::fs_util;
use crate::paths::PristHome;

/// The marker Prist writes next to entries it manages, so `prist clean` can
/// remove exactly its own additions.
pub const PRIST_MARKER_BEGIN: &str = "# prist-managed — begin";
pub const PRIST_MARKER_END: &str = "# prist-managed — end";

/// Apply all IDE integrations for `env_path` into `project_root`.
pub fn integrate(home: &PristHome, env_path: &Path, project_root: &Path) -> anyhow::Result<()> {
    vscode(env_path, project_root)?;
    intellij(env_path, project_root)?;
    gitignore(home, project_root)?;
    Ok(())
}

/// Remove Prist-managed IDE config from `project_root` (best effort).
pub fn revert(project_root: &Path) -> anyhow::Result<()> {
    vscode_revert(project_root)?;
    gitignore_revert(project_root)?;
    Ok(())
}

fn vscode(env_path: &Path, project_root: &Path) -> anyhow::Result<()> {
    let dir = project_root.join(".vscode");
    std::fs::create_dir_all(&dir)?;
    let settings = dir.join("settings.json");

    let mut root: Map<String, Value> = match std::fs::read_to_string(&settings) {
        Ok(contents) if !contents.trim().is_empty() => {
            // JSONC-tolerant parse (comments preserved-as-ignored) so we never
            // corrupt a user's commented settings file.
            let parsed = jsonc_parser::parse_to_serde_value(
                &contents,
                &jsonc_parser::ParseOptions::default(),
            )
            .with_context(|| format!("parsing {}", settings.display()))?;
            parsed
                .as_ref()
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default()
        }
        _ => Map::new(),
    };

    // dart.flutterSdkPath → the active env's path.
    root.insert(
        "dart.flutterSdkPath".to_string(),
        Value::String(env_path.to_string_lossy().into_owned()),
    );

    // Exclude Prist's home from file watcher + search to avoid reindexing.
    let home = home_for_exclusion(env_path);
    let exclude = || {
        Value::Object({
            let mut m = Map::new();
            m.insert(home.clone(), Value::Bool(true));
            m
        })
    };
    root.entry("files.watcherExclude").or_insert(exclude());
    root.entry("search.exclude").or_insert(exclude());

    let json = serde_json::to_string_pretty(&Value::Object(root))?;
    fs_util::atomic_write_str(&settings, &format!("{json}\n"))?;
    Ok(())
}

fn vscode_revert(project_root: &Path) -> anyhow::Result<()> {
    let settings = project_root.join(".vscode").join("settings.json");
    let contents = match std::fs::read_to_string(&settings) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let parsed =
        jsonc_parser::parse_to_serde_value(&contents, &jsonc_parser::ParseOptions::default())
            .with_context(|| format!("parsing {}", settings.display()))?;
    let mut root = parsed
        .as_ref()
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    root.remove("dart.flutterSdkPath");
    if root.is_empty() {
        let _ = std::fs::remove_file(&settings);
    } else {
        let json = serde_json::to_string_pretty(&Value::Object(root))?;
        fs_util::atomic_write_str(&settings, &format!("{json}\n"))?;
    }
    Ok(())
}

fn intellij(env_path: &Path, project_root: &Path) -> anyhow::Result<()> {
    // android/local.properties (only if an android/ dir exists, like IntelliJ expects).
    let android_dir = project_root.join("android");
    if android_dir.is_dir() {
        let props = android_dir.join("local.properties");
        let mut lines: Vec<String> = std::fs::read_to_string(&props)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with("flutter.sdk="))
            .map(String::from)
            .collect();
        lines.push(format!("flutter.sdk={}", env_path.to_string_lossy()));
        fs_util::atomic_write_str(&props, &format!("{}\n", lines.join("\n")))?;
    }

    // .idea/ignored-roots injection (best effort): ensure a workspace.xml has an
    // <ignored-roots> listing the prist home so IntelliJ doesn't reindex it.
    let idea = project_root.join(".idea");
    if idea.is_dir() {
        let workspace = idea.join("workspace.xml");
        if let Ok(xml) = std::fs::read_to_string(&workspace) {
            let root = home_for_exclusion(env_path);
            if !xml.contains(&root) {
                let injection = format!(
                    "  <ignored-roots>\n    <path value=\"{}\" />\n  </ignored-roots>\n",
                    root
                );
                let updated = if let Some(idx) = xml.find("</component>").or(xml.find("</project>"))
                {
                    let mut s = xml.clone();
                    s.insert_str(idx, &injection);
                    s
                } else {
                    xml
                };
                fs_util::atomic_write_str(&workspace, &updated)?;
            }
        }
    }
    Ok(())
}

fn gitignore(home: &PristHome, project_root: &Path) -> anyhow::Result<()> {
    let gitignore = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if existing.contains(PRIST_MARKER_BEGIN) {
        return Ok(());
    }
    let block = format!(
        "\n{PRIST_MARKER_BEGIN}\n.pristrc\n{}\n{PRIST_MARKER_END}\n",
        home.root().display()
    );
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&block);
    fs_util::atomic_write_str(&gitignore, &text)?;
    Ok(())
}

fn gitignore_revert(project_root: &Path) -> anyhow::Result<()> {
    let gitignore = project_root.join(".gitignore");
    let existing = match std::fs::read_to_string(&gitignore) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let Some(begin) = existing.find(PRIST_MARKER_BEGIN) else {
        return Ok(());
    };
    // Walk back to the start of the begin line (including any preceding
    // newline we added), then forward to the end of the end-marker line.
    let mut start = begin;
    if start > 0 && existing.as_bytes()[start - 1] == b'\n' {
        start -= 1;
    }
    let Some(end_rel) = existing[begin..].find(PRIST_MARKER_END) else {
        return Ok(());
    };
    let end_line_end = existing[begin + end_rel..]
        .find('\n')
        .map(|n| begin + end_rel + n + 1)
        .unwrap_or(existing.len());
    let mut text = existing.clone();
    text.replace_range(start..end_line_end, "");
    let text = text.trim_end_matches('\n').to_string() + "\n";
    fs_util::atomic_write_str(&gitignore, &text)?;
    Ok(())
}

/// Derive a glob exclusion pattern for the Prist home from the env path:
/// the env lives under `<prist home>/envs/<name>`, so the home is two levels up
/// from `<home>/envs/<name>`.
fn home_for_exclusion(env_path: &Path) -> String {
    let mut p = env_path.to_path_buf();
    // env_path = <home>/envs/<name>; pop twice to reach <home>.
    p.pop();
    p.pop();
    let s = p.to_string_lossy().into_owned();
    // Glob: "**/<home>/**"
    format!("**/{s}/**")
}
