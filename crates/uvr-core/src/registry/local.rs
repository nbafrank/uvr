//! Packages built from a local source directory (`path = "..."`).

use std::collections::BTreeSet;
use std::path::Path;

use semver::Version;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::manifest::RemoteEntry;
use crate::registry::PackageInfo;

/// Read the package at `root.join(path)`: its DESCRIPTION identity, the
/// `Remotes:` it declares, and its install-time dependency names (the same
/// triple the git resolvers return). `root` is the manifest directory; an
/// absolute `path` ignores it. The lock records `path` exactly as given.
pub fn resolve_local_package(
    root: &Path,
    path: &str,
) -> Result<(PackageInfo, Vec<RemoteEntry>, BTreeSet<String>)> {
    let dir = root.join(path);
    if !dir.is_dir() {
        return Err(UvrError::Other(format!(
            "local package directory '{path}' does not exist (looked in {})",
            dir.display()
        )));
    }
    let not_a_package = |why: &str| {
        UvrError::Other(format!(
            "'{path}' is not an R package: {why} (looked in {})",
            dir.display()
        ))
    };
    let text = std::fs::read_to_string(dir.join("DESCRIPTION"))
        .map_err(|_| not_a_package("it has no readable DESCRIPTION file"))?;
    let fields = crate::dcf::parse_dcf_fields(&text);
    let name = fields
        .get("Package")
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| not_a_package("its DESCRIPTION has no `Package:` field"))?;
    if !crate::package_name::is_valid(&name) {
        return Err(not_a_package(&format!(
            "its DESCRIPTION declares an invalid `Package:` name '{name}'"
        )));
    }
    let raw_version = fields
        .get("Version")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| not_a_package("its DESCRIPTION has no `Version:` field"))?;
    let version = Version::parse(&crate::resolver::normalize_version(&raw_version))
        .map_err(|e| not_a_package(&format!("unparseable `Version:` '{raw_version}' ({e})")))?;

    let requires = crate::registry::github::parse_description_deps(&fields);
    let install_dependencies = requires.iter().map(|d| d.name.clone()).collect();
    let remotes = fields
        .get("Remotes")
        .map(|r| crate::manifest::parse_remotes_field_rich(r))
        .unwrap_or_default();

    Ok((
        PackageInfo {
            name,
            version,
            source: PackageSource::Local {
                path: path.to_string(),
            },
            // Nothing to checksum or download: sync builds the directory.
            checksum: None,
            requires,
            url: String::new(),
            raw_version: Some(raw_version),
            // No tarball exists for sync to read this from later.
            system_requirements: fields.get("SystemRequirements").cloned(),
            subdirectory: None,
        },
        remotes,
        install_dependencies,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pkg(dir: &Path, description: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("DESCRIPTION"), description).unwrap();
    }

    #[test]
    fn reads_identity_deps_and_remotes_from_description() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        write_pkg(
            &tmp.path().join("mypkg"),
            "Package: mypkg\nVersion: 1.2-3\n\
             Depends: R (>= 4.1), methods\nImports: rlang (>= 1.0.0), other\n\
             LinkingTo: cpp11\nSuggests: testthat\n\
             SystemRequirements: libxml2\nRemotes: owner/other\n",
        );

        let (info, remotes, deps) = resolve_local_package(&root, "../mypkg").unwrap();
        assert_eq!(info.name, "mypkg");
        assert_eq!(info.version.to_string(), "1.2.3");
        assert_eq!(info.raw_version.as_deref(), Some("1.2-3"));
        assert_eq!(
            info.source,
            PackageSource::Local {
                path: "../mypkg".into()
            }
        );
        assert!(info.url.is_empty() && info.checksum.is_none());
        assert_eq!(info.system_requirements.as_deref(), Some("libxml2"));
        let names: Vec<&str> = info.requires.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["rlang", "other", "cpp11"]);
        assert!(deps.contains("cpp11") && !deps.contains("testthat"));
        assert!(matches!(
            remotes.as_slice(),
            [RemoteEntry::Source(s)] if s.repository == "owner/other"
        ));
    }

    #[test]
    fn absolute_path_ignores_the_manifest_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("abs");
        write_pkg(&pkg, "Package: abs\nVersion: 0.1\n");
        let abs = pkg.to_str().unwrap();
        let (info, _, _) = resolve_local_package(Path::new("/nonexistent-root"), abs).unwrap();
        assert_eq!(info.name, "abs");
        assert_eq!(info.source, PackageSource::Local { path: abs.into() });
    }

    #[test]
    fn missing_directory_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_local_package(tmp.path(), "../nope")
            .unwrap_err()
            .to_string();
        assert!(err.contains("'../nope' does not exist"), "{err}");
    }

    #[test]
    fn a_directory_that_is_not_a_package_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("empty")).unwrap();
        write_pkg(&tmp.path().join("noname"), "Title: nothing\nVersion: 1.0\n");
        write_pkg(&tmp.path().join("noversion"), "Package: noversion\n");
        write_pkg(
            &tmp.path().join("badname"),
            "Package: bad name\nVersion: 1.0\n",
        );
        for (path, why) in [
            ("empty", "no readable DESCRIPTION"),
            ("noname", "no `Package:` field"),
            ("noversion", "no `Version:` field"),
            ("badname", "invalid `Package:` name"),
        ] {
            let err = resolve_local_package(tmp.path(), path)
                .unwrap_err()
                .to_string();
            assert!(err.contains("is not an R package"), "{err}");
            assert!(err.contains(why), "{path}: {err}");
        }
    }
}
