use thiserror::Error;

#[derive(Debug, Error)]
pub enum UvrError {
    #[error("Project manifest not found (no uvr.toml in current directory or any parent)")]
    ManifestNotFound,

    #[error("Manifest parse error: {0}")]
    ManifestParse(String),

    #[error("Lockfile parse error: {0}")]
    LockfileParse(String),

    #[error("Package not found: {0}")]
    PackageNotFound(String),

    #[error("Version conflict for package '{package}': required {required}, but {conflicting} is already selected")]
    VersionConflict {
        package: String,
        required: String,
        conflicting: String,
    },

    #[error("No version of '{package}' satisfies constraint '{constraint}'")]
    NoMatchingVersion { package: String, constraint: String },

    #[error("Checksum mismatch for {package}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        package: String,
        expected: String,
        actual: String,
    },

    #[error("R not found on PATH. Install R or use `uvr r install <version>`")]
    RNotFound,

    #[error("R version constraint '{constraint}' not satisfied by any installed R ({installed})")]
    RVersionUnsatisfied {
        constraint: String,
        installed: String,
    },

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Semver parse error: {0}")]
    Semver(#[from] semver::Error),

    #[error("Unsupported platform: {0}")]
    UnsupportedPlatform(String),

    #[error("R CMD INSTALL failed for {package} (exit code {code})")]
    InstallFailed { package: String, code: i32 },

    #[error("Circular dependency detected involving: {0}")]
    CircularDependency(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, UvrError>;
