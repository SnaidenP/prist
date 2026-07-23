//! Release-feed parsing and version resolution (spec section 4.1).
//!
//! The Flutter release feed lives at:
//! `https://storage.googleapis.com/flutter_infra_release/releases/releases_<platform>.json`
//!
//! The serde model is intentionally tolerant of unknown fields: serde ignores
//! fields it doesn't know by default, and every known field is `Option` +
//! `#[serde(default)]` so a missing or newly-shaped entry never breaks Prist.

use serde::{Deserialize, Serialize};

use crate::error::{PristError, Result};

/// Host operating system family used to pick the release feed and engine
/// artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    /// The host platform Prist is running on.
    pub fn host() -> Self {
        #[cfg(target_os = "linux")]
        {
            Platform::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Platform::Macos
        }
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            compile_error!("prist only supports linux, macos and windows hosts");
        }
    }

    /// The feed filename segment, e.g. `linux`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::Macos => "macos",
            Platform::Windows => "windows",
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The known Flutter release channels.
pub const CHANNELS: &[&str] = &["stable", "beta", "dev"];

/// Whether `s` names a known channel.
pub fn is_channel(s: &str) -> bool {
    CHANNELS.contains(&s) || s == "master"
}

/// Whether `s` looks like a 40-char hex git commit hash.
pub fn looks_like_commit_hash(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// One entry in the Flutter release feed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Release {
    /// 40-char commit hash of the `flutter/flutter` repo at this release.
    #[serde(default)]
    pub hash: Option<String>,
    /// Channel: stable / beta / dev.
    #[serde(default)]
    pub channel: Option<String>,
    /// Semantic-ish version string, e.g. `3.44.7` or `3.47.0-0.1.pre`.
    #[serde(default)]
    pub version: Option<String>,
    /// Dart SDK version string (free-form).
    #[serde(default)]
    pub dart_sdk_version: Option<String>,
    /// Dart SDK arch: x64 / arm64.
    #[serde(default)]
    pub dart_sdk_arch: Option<String>,
    /// Release date in RFC 3339 / ISO 8601.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Archive path relative to `base_url`.
    #[serde(default)]
    pub archive: Option<String>,
    /// SHA-256 of the archive.
    #[serde(default)]
    pub sha256: Option<String>,
}

impl Release {
    /// A synthetic release for the `master` channel (no feed entry, no
    /// archive — cloned straight from the repo HEAD).
    pub fn master() -> Self {
        Release {
            channel: Some("master".into()),
            version: Some("master".into()),
            ..Default::default()
        }
    }

    /// A synthetic release pinned to an exact commit hash (e.g. user passed a
    /// 40-char hash directly).
    pub fn for_commit(hash: &str) -> Self {
        Release {
            hash: Some(hash.to_string()),
            ..Default::default()
        }
    }

    pub fn commit_hash(&self) -> Option<&str> {
        self.hash.as_deref()
    }

    pub fn archive_url(&self, feed: &ReleaseFeed) -> Option<String> {
        let base = feed.base_url.as_deref()?;
        let archive = self.archive.as_deref()?;
        Some(format!("{}/{}", base.trim_end_matches('/'), archive))
    }
}

/// `current_release` map: channel → current commit hash.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CurrentRelease {
    #[serde(default)]
    pub stable: Option<String>,
    #[serde(default)]
    pub beta: Option<String>,
    #[serde(default)]
    pub dev: Option<String>,
}

/// Top-level release-feed document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseFeed {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub current_release: CurrentRelease,
    #[serde(default)]
    pub releases: Vec<Release>,
}

/// Base URL for the release feed (the GCS bucket root).
pub const FEED_BASE: &str = "https://storage.googleapis.com/flutter_infra_release/releases";

impl ReleaseFeed {
    /// URL of the per-platform feed document.
    pub fn feed_url(platform: Platform) -> String {
        format!("{}/releases_{}.json", FEED_BASE, platform.as_str())
    }

    /// Fetch and parse the feed for `platform`.
    pub async fn fetch(client: &reqwest::Client, platform: Platform) -> Result<Self> {
        let url = Self::feed_url(platform);
        tracing::info!(%url, "fetching release feed");
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| PristError::ReleaseFeed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(PristError::ReleaseFeed(format!(
                "feed {} returned HTTP {}",
                url,
                resp.status()
            )));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| PristError::ReleaseFeed(e.to_string()))?;
        let feed: ReleaseFeed = serde_json::from_str(&body)
            .map_err(|e| PristError::ReleaseFeed(format!("feed parse failed: {e}")))?;
        Ok(feed)
    }

    /// The current commit hash for a channel, if known.
    pub fn current_hash(&self, channel: &str) -> Option<&str> {
        match channel {
            "stable" => self.current_release.stable.as_deref(),
            "beta" => self.current_release.beta.as_deref(),
            "dev" => self.current_release.dev.as_deref(),
            _ => None,
        }
    }

    /// Find the feed entry whose commit hash matches `hash`.
    pub fn find_by_hash(&self, hash: &str) -> Option<&Release> {
        self.releases
            .iter()
            .find(|r| r.hash.as_deref() == Some(hash))
    }

    /// Find the first (newest) feed entry with an exact version match.
    pub fn find_by_version(&self, version: &str) -> Option<&Release> {
        self.releases
            .iter()
            .find(|r| r.version.as_deref() == Some(version))
    }

    /// Find the newest feed entry on a given channel.
    pub fn newest_on_channel(&self, channel: &str) -> Option<&Release> {
        self.releases
            .iter()
            .find(|r| r.channel.as_deref() == Some(channel))
    }

    /// Resolve a user-facing reference (channel / version / commit hash /
    /// `master`) into a concrete [`Release`].
    ///
    /// - `stable` / `beta` / `dev` → the current release of that channel.
    /// - `master` → a synthetic release that means "clone repo HEAD".
    /// - `3.0.1` → the first feed entry with that exact version.
    /// - a 40-char hex hash → a synthetic release pinned to that commit.
    pub fn resolve(&self, reference: &str) -> Result<Release> {
        if reference == "master" {
            return Ok(Release::master());
        }
        if is_channel(reference) {
            let hash = self
                .current_hash(reference)
                .ok_or_else(|| PristError::NoCurrentRelease(reference.into()))?;
            let rel = self
                .find_by_hash(hash)
                .ok_or_else(|| PristError::UnresolvedRef(reference.into()))?;
            return Ok(rel.clone());
        }
        if looks_like_commit_hash(reference) {
            return Ok(Release::for_commit(reference));
        }
        // Otherwise treat it as a version string.
        if let Some(rel) = self.find_by_version(reference) {
            return Ok(rel.clone());
        }
        let with_v = if reference.starts_with('v') {
            reference.to_string()
        } else {
            format!("v{reference}")
        };
        let without_v = reference.trim_start_matches('v');
        if let Some(rel) = self
            .find_by_version(&with_v)
            .or_else(|| self.find_by_version(without_v))
        {
            return Ok(rel.clone());
        }

        // If not in the JSON feed, treat reference as a valid Git tag / version ref directly
        Ok(Release {
            version: Some(reference.to_string()),
            hash: Some(reference.to_string()),
            ..Default::default()
        })
    }
}
