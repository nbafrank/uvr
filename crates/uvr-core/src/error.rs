use thiserror::Error;

#[derive(Debug, Error)]
pub enum UvrError {
    #[error("Project manifest not found (no uvr.toml in current directory or any parent)")]
    ManifestNotFound,

    #[error("Manifest parse error: {0}")]
    ManifestParse(String),

    #[error("Lockfile parse error: {0}")]
    LockfileParse(String),

    /// A script's inline dependency header could not be read.
    ///
    /// Rendered bare because the caller knows the file and prefixes it —
    /// "Invalid script header in <file>: <this>".
    #[error("{0}")]
    ScriptHeaderParse(String),

    /// A dependency spec (`uvr add` argument or script-header entry) is
    /// malformed. Rendered bare: the message already quotes the spec.
    #[error("{0}")]
    InvalidDependencySpec(String),

    #[error("Package not found: {0}. If this package was recently archived from CRAN, try installing from the CRAN GitHub mirror: uvr add cran/{0}@master")]
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

    #[error("Network error: {}", describe_network_error(.0))]
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

/// Render a `reqwest::Error` with its underlying cause.
///
/// `reqwest::Error`'s own `Display` prints only the outermost layer, so a
/// rejected certificate arrives as a bare "error sending request for url
/// (...)" — indistinguishable from being offline, from DNS failure, or
/// from the host being down. The actual reason (rustls'
/// `InvalidCertificate(UnknownIssuer)`, a connection reset, a timeout)
/// sits in the `source()` chain and never reaches the user, so someone
/// behind a corporate TLS-inspecting proxy is told their network is
/// broken rather than that their CA isn't trusted (#227, and the reason
/// #201 took two rounds to diagnose).
///
/// Certificate failures additionally get a pointer to the fix, since
/// "unknown issuer" is not self-explanatory to most users.
fn describe_network_error(err: &reqwest::Error) -> String {
    // Depth cap: the chain comes from the local networking stack rather
    // than remote input, so this is belt-and-braces, not a live risk.
    const MAX_DEPTH: usize = 8;
    let mut parts = vec![err.to_string()];
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source.filter(|_| parts.len() < MAX_DEPTH) {
        let text = cause.to_string();
        // Skip layers that just restate what we already printed.
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ")
}

/// Whether an error message describes a certificate/TLS trust failure
/// rather than an ordinary connectivity problem. Used by the CLI to
/// attach remediation advice at the point the error is rendered, rather
/// than baking a multi-line hint into `Display` — these errors are often
/// embedded mid-sentence in a larger warning.
pub fn looks_like_certificate_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    // Deliberately NOT "tls handshake": that is rustls' generic message for
    // any connection closed before the handshake completes — a firewall
    // blocking the port, a protocol mismatch, or nothing listening on TLS
    // all produce it. Routing those to "install your CA certificate" sends
    // the user to fix the wrong thing, which is the same failure this
    // function exists to prevent, pointed the other way.
    ["certificate", "unknownissuer", "cert verify", "certverify"]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_failures_are_recognized() {
        // #227: these are the shapes rustls/reqwest produce for an
        // untrusted CA — each must be distinguishable from being offline.
        for msg in [
            "error sending request: invalid peer certificate: UnknownIssuer",
            "invalid peer certificate: Expired",
            "cert verify failed",
        ] {
            assert!(looks_like_certificate_failure(msg), "{msg}");
        }
    }

    #[test]
    fn ordinary_network_failures_are_not_mislabelled() {
        // A genuine connectivity problem must not be reported as a trust
        // problem — that would send users to fix the wrong thing.
        for msg in [
            "error sending request: connection refused",
            "dns error: failed to lookup address information",
            "operation timed out",
            "connection reset by peer (os error 104)",
            // rustls' generic pre-handshake close: a blocked port or a
            // server not speaking TLS, not necessarily an untrusted CA.
            "tls handshake eof",
        ] {
            assert!(!looks_like_certificate_failure(msg), "{msg}");
        }
    }
}
