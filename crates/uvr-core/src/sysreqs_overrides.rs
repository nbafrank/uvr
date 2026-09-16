use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Operator-supplied alternatives to catalog package names. These only accept
/// verified installed providers; they never erase packages or guess aliases.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Overrides {
    #[serde(default)]
    overrides: Vec<Override>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    distro: String,
    package: String,
    installed: String,
    pkg_config: Option<String>,
    minimum_version: Option<String>,
}

impl Overrides {
    pub(crate) fn load() -> Result<Self> {
        let Some(path) = std::env::var_os("UVR_SYSREQS_OVERRIDES").filter(|p| !p.is_empty()) else {
            return Ok(Self::default());
        };
        Self::from_path(Path::new(&path))
    }

    fn from_path(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path).with_context(|| {
            format!("Cannot read system dependency overrides {}", path.display())
        })?;
        Self::parse(&source)
            .with_context(|| format!("Invalid system dependency overrides {}", path.display()))
    }

    pub(crate) fn parse(source: &str) -> Result<Self> {
        let policy: Self = toml::from_str(source)?;
        let mut seen = HashSet::new();
        for rule in &policy.overrides {
            for (field, value) in [
                ("distro", rule.distro.as_str()),
                ("package", rule.package.as_str()),
                ("installed", rule.installed.as_str()),
            ] {
                validate_token(field, value)?;
            }
            if !rule.distro.contains('-') {
                bail!("distro must include an explicit release, e.g. redhat-9");
            }
            if let Some(module) = &rule.pkg_config {
                validate_token("pkg_config", module)?;
            }
            if let Some(version) = &rule.minimum_version {
                validate_token("minimum_version", version)?;
                if rule.pkg_config.is_none() {
                    bail!("minimum_version requires pkg_config");
                }
            }
            if !seen.insert((&rule.distro, &rule.package)) {
                bail!("duplicate override for {} on {}", rule.package, rule.distro);
            }
        }
        Ok(policy)
    }

    pub(crate) fn satisfied_by(&self, distro: &str, package: &str) -> Option<String> {
        let rule = self
            .overrides
            .iter()
            .find(|rule| rule.distro == distro && rule.package == package)?;
        if !crate::sysreqs::has_installed_package(&rule.installed) {
            return None;
        }
        if let Some(module) = &rule.pkg_config {
            let mut command = Command::new("pkg-config");
            command.arg("--exists");
            if let Some(version) = &rule.minimum_version {
                command.arg(format!("--atleast-version={version}"));
            }
            if !command
                .arg(module)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
            {
                return None;
            }
        }
        let mut description = format!(
            "{package} satisfied by installed {} on {distro}",
            rule.installed
        );
        if let Some(module) = &rule.pkg_config {
            description.push_str(&format!(" (pkg-config {module}"));
            if let Some(version) = &rule.minimum_version {
                description.push_str(&format!(" >= {version}"));
            }
            description.push(')');
        }
        Some(description)
    }
}

fn validate_token(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-():~".contains(c))
    {
        bail!("{field} must be a nonempty name/version, not an option, path, or expression");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = r#"
[[overrides]]
distro = "redhat-9"
package = "legacy-devel"
installed = "modern-devel"
pkg_config = "modern"
minimum_version = "3.4"
"#;

    #[test]
    fn parse_scoped_policy_and_optional_version_guard() {
        let rules = Overrides::parse(RULE).unwrap();
        assert_eq!(rules.overrides.len(), 1);
        assert!(rules.satisfied_by("redhat-10", "legacy-devel").is_none());
        assert!(rules.satisfied_by("redhat-9", "legacy-libs").is_none());
        assert!(Overrides::parse("").unwrap().overrides.is_empty());
        assert!(Overrides::parse(
            &RULE.replace("pkg_config = \"modern\"\nminimum_version = \"3.4\"", "")
        )
        .is_ok());
    }

    #[test]
    fn reject_ambiguous_or_malformed_policy() {
        for source in [
            format!("{RULE}\n{RULE}"),
            RULE.replace("distro = \"redhat-9\"", "distro = \"redhat\""),
            RULE.replace("pkg_config = \"modern\"", ""),
            RULE.replace("minimum_version", "min_version"),
            RULE.replace("modern-devel", "--allowerasing"),
            RULE.replace("modern-devel", "/tmp/helper"),
            RULE.replace("legacy-devel", ""),
            RULE.replace("3.4", "3.4; true"),
        ] {
            assert!(Overrides::parse(&source).is_err(), "{source}");
        }
    }

    #[test]
    fn explicit_missing_or_invalid_config_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overrides.toml");
        assert!(Overrides::from_path(&path).is_err());
        std::fs::write(&path, "this is not TOML").unwrap();
        assert!(Overrides::from_path(&path).is_err());
        std::fs::write(&path, RULE).unwrap();
        assert_eq!(Overrides::from_path(&path).unwrap().overrides.len(), 1);
    }
}
