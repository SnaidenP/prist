use thiserror::Error;

/// All Prist fallible operations return [`Result<T, PristError>`].
#[derive(Debug, Error)]
pub enum PristError {
    #[error("environment '{0}' not found")]
    EnvNotFound(String),

    #[error("environment '{0}' already exists; remove it first with `prist rm {0}`")]
    EnvAlreadyExists(String),

    #[error("version or channel '{0}' could not be resolved against the release feed")]
    UnresolvedRef(String),

    #[error("release feed fetch failed: {0}")]
    ReleaseFeed(String),

    #[error("release feed is empty or has no current release for platform '{0}'")]
    NoCurrentRelease(String),

    #[error("engine hash mismatch for {what}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        what: String,
        expected: String,
        actual: String,
    },

    #[error("engine artifact unavailable for hash '{0}'")]
    EngineArtifact(String),

    #[error("invalid reference '{0}': must be a semantic version (3.0.1), a channel (stable/beta/dev/master), or a 40-char commit hash")]
    InvalidRef(String),

    #[error("bare repository is missing or corrupt at {0}; run `prist repair`")]
    BareCorrupt(String),

    #[error("alternates file for env '{0}' is missing or points to a non-existent object store")]
    AlternatesBroken(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("git error: {0}")]
    Git(#[from] gix::discover::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl PristError {
    /// Wrap an arbitrary string message as a [`PristError::Other`].
    pub fn msg<S: Into<String>>(s: S) -> Self {
        PristError::Other(s.into())
    }
}

pub type Result<T> = std::result::Result<T, PristError>;
