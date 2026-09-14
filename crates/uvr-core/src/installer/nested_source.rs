use std::path::{Path, PathBuf};

use crate::error::{Result, UvrError};
use crate::lockfile::LockedPackage;

pub const MARKER_FILENAME: &str = "uvr-nested-source";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedProvenance {
    pub url: String,
    pub checksum: String,
    pub subdirectory: String,
}

impl NestedProvenance {
    pub fn from_locked(p: &LockedPackage) -> Result<Option<Self>> {
        crate::registry::github::validate_nested_lock_entry(p)?;
        let subdirectory = match p.subdirectory.as_deref() {
            Some(subdirectory) => subdirectory.to_string(),
            // A plain (non-nested) GitHub package still needs its commit
            // pinned. Without a marker the only thing left to compare is the
            // DESCRIPTION version, and a package that carries the same
            // development version across commits - `1.0.3.9001` for every
            // commit of a fork, say - then looks up to date forever, no matter
            // which commit the lockfile names.
            None => {
                if p.source != crate::lockfile::PackageSource::GitHub {
                    return Ok(None);
                }
                let has_commit = p
                    .checksum
                    .as_deref()
                    .and_then(|c| c.strip_prefix("git:"))
                    .is_some_and(crate::registry::github::is_full_commit_sha);
                if !has_commit || p.url.as_deref().unwrap_or_default().is_empty() {
                    return Ok(None);
                }
                String::new()
            }
        };
        Ok(Some(NestedProvenance {
            url: p.url.clone().unwrap_or_default(),
            checksum: p.checksum.clone().unwrap_or_default(),
            subdirectory,
        }))
    }

    // The same repository at the same commit holds a different package per subdirectory.
    pub fn cache_identity(&self) -> String {
        format!(
            "github|{}|{}|{}",
            self.url, self.checksum, self.subdirectory
        )
    }

    pub fn to_file_contents(&self) -> String {
        format!(
            "source=github\nurl={}\nchecksum={}\nsubdirectory={}\n",
            self.url, self.checksum, self.subdirectory
        )
    }

    pub fn parse(contents: &str) -> Option<Self> {
        let mut source: Option<String> = None;
        let mut url: Option<String> = None;
        let mut checksum: Option<String> = None;
        let mut subdirectory: Option<String> = None;
        for line in contents.lines().filter(|l| !l.is_empty()) {
            let (key, value) = line.split_once('=')?;
            let slot = match key {
                "source" => &mut source,
                "url" => &mut url,
                "checksum" => &mut checksum,
                "subdirectory" => &mut subdirectory,
                _ => return None,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(value.to_string());
        }
        let (source, url, checksum, subdirectory) = (source?, url?, checksum?, subdirectory?);
        // An empty subdirectory is the repository root: a plain GitHub package
        // rather than one nested in a monorepo.
        if source != "github"
            || url.is_empty()
            || (!subdirectory.is_empty() && !crate::subdirectory::is_valid(&subdirectory))
        {
            return None;
        }
        if !checksum
            .strip_prefix("git:")
            .is_some_and(crate::registry::github::is_full_commit_sha)
        {
            return None;
        }
        Some(NestedProvenance {
            url,
            checksum,
            subdirectory,
        })
    }
}

enum Marker {
    Absent,
    Invalid,
    Present(NestedProvenance),
}

pub fn marker_path(installed_pkg_dir: &Path) -> PathBuf {
    installed_pkg_dir.join("Meta").join(MARKER_FILENAME)
}

fn read_marker(installed_pkg_dir: &Path) -> Marker {
    let path = marker_path(installed_pkg_dir);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Marker::Absent,
        Err(_) => Marker::Invalid,
        Ok(md) if !md.is_file() => Marker::Invalid,
        Ok(_) => match std::fs::read_to_string(&path)
            .ok()
            .as_deref()
            .and_then(NestedProvenance::parse)
        {
            Some(p) => Marker::Present(p),
            None => Marker::Invalid,
        },
    }
}

pub fn read_provenance(installed_pkg_dir: &Path) -> Option<NestedProvenance> {
    match read_marker(installed_pkg_dir) {
        Marker::Present(provenance) => Some(provenance),
        _ => None,
    }
}

pub fn provenance_matches(installed_pkg_dir: &Path, expected: Option<&NestedProvenance>) -> bool {
    match (read_marker(installed_pkg_dir), expected) {
        (Marker::Absent, None) => true,
        (Marker::Present(found), Some(want)) => &found == want,
        _ => false,
    }
}

pub fn write_marker(installed_pkg_dir: &Path, provenance: &NestedProvenance) -> Result<()> {
    let meta_dir = installed_pkg_dir.join("Meta");
    match std::fs::symlink_metadata(&meta_dir) {
        Ok(md) if md.is_dir() => {}
        // An installed tree may carry attacker-chosen paths; never write through one.
        Ok(_) => {
            return Err(UvrError::Other(format!(
                "'{}' is not a directory",
                meta_dir.display()
            )))
        }
        Err(_) => std::fs::create_dir_all(&meta_dir)?,
    }
    let path = marker_path(installed_pkg_dir);
    remove_path(&path)?;
    std::fs::write(&path, provenance.to_file_contents())?;
    Ok(())
}

pub fn clear_marker(installed_pkg_dir: &Path) -> Result<()> {
    remove_path(&marker_path(installed_pkg_dir))
}

fn remove_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(md) if md.is_dir() => Ok(std::fs::remove_dir_all(path)?),
        Ok(_) => Ok(std::fs::remove_file(path)?),
    }
}

pub struct SelectedSource {
    _staging: tempfile::TempDir,
    path: PathBuf,
}

impl SelectedSource {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn prepare(
    archive: &Path,
    library: &Path,
    package_name: &str,
    subdirectory: &str,
) -> Result<SelectedSource> {
    std::fs::create_dir_all(library)?;
    let staging = tempfile::TempDir::new_in(library)?;
    unpack_archive(archive, staging.path())?;
    let repo_root = repository_root(staging.path())?;
    let path = select_package_dir(&repo_root, subdirectory, package_name)?;
    Ok(SelectedSource {
        _staging: staging,
        path,
    })
}

fn unpack_archive(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut ar = tar::Archive::new(super::tar_compat::LinkSizeFix::new(decoder));
    ar.set_overwrite(true);
    ar.set_preserve_permissions(true);
    ar.unpack(dest).map_err(|e| {
        UvrError::Other(format!(
            "Failed to extract repository archive {}: {e}",
            archive.display()
        ))
    })
}

fn repository_root(staging: &Path) -> Result<PathBuf> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(staging)? {
        let entry = entry?;
        entries.push((entry.path(), entry.file_type()?));
    }
    match entries.as_slice() {
        [(path, ft)] if ft.is_dir() => Ok(path.clone()),
        _ => Err(UvrError::Other(format!(
            "Repository archive must contain exactly one top-level directory, found {} entry/entries",
            entries.len()
        ))),
    }
}

// Segments are pushed one at a time: '/' is not a separator inside a canonicalized
// Windows verbatim path, and `validate` has already rejected '\\', ':', and '..'.
fn push_segments(base: &Path, subdirectory: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for segment in subdirectory.split('/') {
        path.push(segment);
    }
    path
}

fn select_package_dir(repo_root: &Path, subdirectory: &str, package_name: &str) -> Result<PathBuf> {
    crate::subdirectory::validate(subdirectory)?;
    let selected = push_segments(repo_root, subdirectory);
    let root = repo_root.canonicalize()?;
    let canonical = selected.canonicalize().map_err(|e| {
        UvrError::Other(format!(
            "Subdirectory '{subdirectory}' not found in the repository archive: {e}"
        ))
    })?;
    if !canonical.starts_with(&root) {
        return Err(UvrError::Other(format!(
            "Subdirectory '{subdirectory}' resolves outside the repository archive"
        )));
    }
    // A symlinked path component would silently select the repository root or another package.
    if canonical != push_segments(&root, subdirectory) {
        return Err(UvrError::Other(format!(
            "Subdirectory '{subdirectory}' is not a real directory of the repository archive"
        )));
    }
    if !std::fs::metadata(&canonical)?.is_dir() {
        return Err(UvrError::Other(format!(
            "Subdirectory '{subdirectory}' is not a directory"
        )));
    }
    let description = canonical.join("DESCRIPTION");
    let is_regular = std::fs::symlink_metadata(&description)
        .map(|md| md.is_file())
        .unwrap_or(false);
    if !is_regular {
        return Err(UvrError::Other(format!(
            "No DESCRIPTION file in subdirectory '{subdirectory}' of the repository archive"
        )));
    }
    let contents = std::fs::read_to_string(&description)?;
    let found = crate::dcf::parse_dcf_fields(&contents)
        .get("Package")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    if found != package_name {
        return Err(UvrError::Other(format!(
            "Subdirectory '{subdirectory}' contains package '{found}', expected '{package_name}'"
        )));
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::PackageSource;
    use std::io::Write;
    use tempfile::TempDir;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn url_for(sha: &str) -> String {
        format!("https://api.github.com/repos/o/r/tarball/{sha}")
    }

    fn provenance(sha: &str, subdirectory: &str) -> NestedProvenance {
        NestedProvenance {
            url: url_for(sha),
            checksum: format!("git:{sha}"),
            subdirectory: subdirectory.to_string(),
        }
    }

    #[test]
    fn from_locked_pins_a_plain_github_package() {
        // Regression: a package with no subdirectory got no marker, so
        // `is_installed` fell back to the DESCRIPTION version. A fork that
        // carries the same development version across commits then looks up to
        // date forever and a stale build is never replaced.
        let p = NestedProvenance::from_locked(&locked(SHA, None))
            .unwrap()
            .expect("a plain GitHub package should still pin its commit");
        assert_eq!(p.checksum, format!("git:{SHA}"));
        assert_eq!(p.subdirectory, "");
    }

    #[test]
    fn from_locked_ignores_a_non_github_source() {
        let mut pkg = locked(SHA, None);
        pkg.source = PackageSource::Cran;
        assert!(NestedProvenance::from_locked(&pkg).unwrap().is_none());
    }

    #[test]
    fn from_locked_ignores_a_package_without_a_commit() {
        let mut pkg = locked(SHA, None);
        pkg.checksum = Some("sha256:abc".to_string());
        assert!(NestedProvenance::from_locked(&pkg).unwrap().is_none());
    }

    #[test]
    fn marker_round_trips_an_empty_subdirectory() {
        let want = provenance(SHA, "");
        let parsed = NestedProvenance::parse(&want.to_file_contents())
            .expect("an empty subdirectory is the repository root, not invalid");
        assert_eq!(parsed, want);
    }

    fn locked(sha: &str, subdirectory: Option<&str>) -> LockedPackage {
        LockedPackage {
            name: "nested".to_string(),
            version: "0.1.0".to_string(),
            source: PackageSource::GitHub,
            raw_version: None,
            url: Some(url_for(sha)),
            checksum: Some(format!("git:{sha}")),
            subdirectory: subdirectory.map(str::to_string),
            requires: vec![],
            system_requirements: None,
            dev: false,
        }
    }

    fn description(name: &str) -> String {
        format!("Package: {name}\nVersion: 0.1.0\n")
    }

    fn make_pkg_dir(root: &Path, rel: &str, name: &str) -> PathBuf {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("DESCRIPTION"), description(name)).unwrap();
        dir
    }

    fn write_archive(path: &Path, entries: &[(&str, Option<&str>, u32)]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, contents, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(*mode);
            match contents {
                Some(body) => {
                    header.set_size(body.len() as u64);
                    builder
                        .append_data(&mut header, name, body.as_bytes())
                        .unwrap();
                }
                None => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    builder
                        .append_data(&mut header, name, std::io::empty())
                        .unwrap();
                }
            }
        }
        builder
            .into_inner()
            .unwrap()
            .finish()
            .unwrap()
            .flush()
            .unwrap();
    }

    #[test]
    fn provenance_from_locked_root_entry_is_none() {
        let mut root = locked(SHA, None);
        root.source = PackageSource::Cran;
        root.checksum = Some("md5:abc".to_string());
        root.url = Some("https://cran.r-project.org/x.tar.gz".to_string());
        assert_eq!(NestedProvenance::from_locked(&root).unwrap(), None);
    }

    #[test]
    fn provenance_from_locked_nested_entry_requires_canonical_identity() {
        assert_eq!(
            NestedProvenance::from_locked(&locked(SHA, Some("pkgs/nested"))).unwrap(),
            Some(provenance(SHA, "pkgs/nested"))
        );

        let mut short_commit = locked(SHA, Some("pkgs/nested"));
        short_commit.checksum = Some(format!("git:{}", &SHA[..7]));
        assert!(NestedProvenance::from_locked(&short_commit).is_err());

        assert!(NestedProvenance::from_locked(&locked(SHA, Some("../escape"))).is_err());
    }

    #[test]
    fn provenance_file_contents_round_trip() {
        let p = provenance(SHA, "pkgs/nested");
        assert_eq!(NestedProvenance::parse(&p.to_file_contents()), Some(p));
    }

    #[test]
    fn provenance_parse_is_strict() {
        let good = provenance(SHA, "pkgs/nested").to_file_contents();
        for bad in [
            good.replace("source=github", "source=gitlab"),
            good.replace(&format!("checksum=git:{SHA}"), "checksum=sha256:abc"),
            good.replace(
                &format!("checksum=git:{SHA}"),
                &format!("checksum=git:{}", &SHA[..7]),
            ),
            good.replace("subdirectory=pkgs/nested", "subdirectory=../escape"),
            format!("{good}extra=1\n"),
            format!("{good}url=other\n"),
            good.replace(&format!("url={}\n", url_for(SHA)), ""),
            good.replace("subdirectory=pkgs/nested\n", ""),
            "not a marker".to_string(),
        ] {
            if bad == good {
                continue;
            }
            assert!(
                NestedProvenance::parse(&bad).is_none(),
                "should reject {bad:?}"
            );
        }
        assert!(NestedProvenance::parse(&good.replace(&url_for(SHA), "")).is_none());
    }

    #[test]
    fn cache_identity_distinguishes_commit_and_subdirectory() {
        let base = provenance(SHA, "pkgs/nested").cache_identity();
        assert_ne!(base, provenance(OTHER_SHA, "pkgs/nested").cache_identity());
        assert_ne!(base, provenance(SHA, "pkgs/other").cache_identity());
        assert_eq!(base, provenance(SHA, "pkgs/nested").cache_identity());
    }

    #[test]
    fn provenance_matches_requires_exact_identity() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("nested");
        std::fs::create_dir_all(&pkg_dir).unwrap();

        assert!(provenance_matches(&pkg_dir, None));
        assert!(!provenance_matches(
            &pkg_dir,
            Some(&provenance(SHA, "pkgs/nested"))
        ));

        write_marker(&pkg_dir, &provenance(SHA, "pkgs/nested")).unwrap();
        assert!(provenance_matches(
            &pkg_dir,
            Some(&provenance(SHA, "pkgs/nested"))
        ));
        assert!(!provenance_matches(
            &pkg_dir,
            Some(&provenance(OTHER_SHA, "pkgs/nested"))
        ));
        assert!(!provenance_matches(
            &pkg_dir,
            Some(&provenance(SHA, "pkgs/other"))
        ));
        assert!(!provenance_matches(&pkg_dir, None));

        std::fs::write(marker_path(&pkg_dir), "source=github\n").unwrap();
        assert!(!provenance_matches(
            &pkg_dir,
            Some(&provenance(SHA, "pkgs/nested"))
        ));
        assert!(!provenance_matches(&pkg_dir, None));

        clear_marker(&pkg_dir).unwrap();
        assert!(provenance_matches(&pkg_dir, None));
        clear_marker(&pkg_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_marker_refuses_to_follow_planted_symlinks() {
        let tmp = TempDir::new().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, "keep").unwrap();

        let pkg_dir = tmp.path().join("nested");
        std::fs::create_dir_all(pkg_dir.join("Meta")).unwrap();
        std::os::unix::fs::symlink(&outside, marker_path(&pkg_dir)).unwrap();
        write_marker(&pkg_dir, &provenance(SHA, "pkgs/nested")).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert!(provenance_matches(
            &pkg_dir,
            Some(&provenance(SHA, "pkgs/nested"))
        ));

        let linked_meta = tmp.path().join("linked");
        std::fs::create_dir_all(linked_meta.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(linked_meta.join("elsewhere"), linked_meta.join("Meta"))
            .unwrap();
        assert!(write_marker(&linked_meta, &provenance(SHA, "pkgs/nested")).is_err());
    }

    #[test]
    fn prepare_selects_the_nested_package_directory() {
        let tmp = TempDir::new().unwrap();
        let archive = tmp.path().join("repo.tar.gz");
        write_archive(
            &archive,
            &[
                ("o-r-0123456/", None, 0o755),
                ("o-r-0123456/DESCRIPTION", Some(&description("root")), 0o644),
                (
                    "o-r-0123456/pkgs/nested/DESCRIPTION",
                    Some(&description("nested")),
                    0o644,
                ),
                (
                    "o-r-0123456/pkgs/nested/configure",
                    Some("#!/bin/sh\n"),
                    0o755,
                ),
            ],
        );
        let library = tmp.path().join("library");

        let selected = prepare(&archive, &library, "nested", "pkgs/nested").unwrap();
        assert!(selected.path().starts_with(&library));
        assert!(selected.path().ends_with("pkgs/nested"));
        assert_eq!(
            std::fs::read_to_string(selected.path().join("DESCRIPTION")).unwrap(),
            description("nested")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(selected.path().join("configure"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "build scripts must stay executable");
        }

        assert!(prepare(&archive, &library, "nested", "pkgs/missing").is_err());
        assert!(prepare(&archive, &library, "other", "pkgs/nested").is_err());
        assert!(prepare(&archive, &library, "nested", "../escape").is_err());
    }

    #[test]
    fn prepare_rejects_a_malformed_archive_top_level() {
        let tmp = TempDir::new().unwrap();
        let library = tmp.path().join("library");

        let two_roots = tmp.path().join("two.tar.gz");
        write_archive(
            &two_roots,
            &[
                ("a/pkg/DESCRIPTION", Some(&description("nested")), 0o644),
                ("b/pkg/DESCRIPTION", Some(&description("nested")), 0o644),
            ],
        );
        assert!(prepare(&two_roots, &library, "nested", "pkg").is_err());

        let file_root = tmp.path().join("file.tar.gz");
        write_archive(&file_root, &[("loose.txt", Some("x"), 0o644)]);
        assert!(prepare(&file_root, &library, "nested", "pkg").is_err());

        let empty = tmp.path().join("empty.tar.gz");
        write_archive(&empty, &[]);
        assert!(prepare(&empty, &library, "nested", "pkg").is_err());

        let not_an_archive = tmp.path().join("bogus.tar.gz");
        std::fs::write(&not_an_archive, b"definitely not gzip").unwrap();
        assert!(prepare(&not_an_archive, &library, "nested", "pkg").is_err());
    }

    #[test]
    fn select_package_dir_enforces_selection_rules() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("o-r-0123456");
        make_pkg_dir(&repo, "pkgs/nested", "nested");
        std::fs::create_dir_all(repo.join("pkgs/nodesc")).unwrap();
        std::fs::write(repo.join("pkgs/afile"), "x").unwrap();
        make_pkg_dir(&repo, "pkgs/wrongname", "somethingelse");

        assert!(select_package_dir(&repo, "pkgs/nested", "nested").is_ok());
        assert!(select_package_dir(&repo, "pkgs/nodesc", "nested").is_err());
        assert!(select_package_dir(&repo, "pkgs/afile", "nested").is_err());
        assert!(select_package_dir(&repo, "pkgs/wrongname", "somethingelse").is_ok());
        assert!(select_package_dir(&repo, "pkgs/wrongname", "nested").is_err());
        for unsafe_path in ["../outside", "/etc", "pkgs/../../outside", ""] {
            assert!(select_package_dir(&repo, unsafe_path, "nested").is_err());
        }
    }

    #[test]
    fn select_package_dir_pushes_native_components_and_returns_the_plain_path() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("o-r-0123456");
        let created = make_pkg_dir(&repo, "pkgs/deep/nested", "nested");

        let selected = select_package_dir(&repo, "pkgs/deep/nested", "nested").unwrap();
        assert_eq!(selected, created);
        assert_eq!(
            selected.as_os_str(),
            repo.join("pkgs").join("deep").join("nested").as_os_str()
        );
        assert_eq!(
            selected.strip_prefix(&repo).unwrap(),
            Path::new("pkgs").join("deep").join("nested")
        );

        let unnormalized = tmp.path().join(".").join("o-r-0123456");
        let selected = select_package_dir(&unnormalized, "pkgs/deep/nested", "nested").unwrap();
        assert_eq!(
            selected.as_os_str(),
            unnormalized
                .join("pkgs")
                .join("deep")
                .join("nested")
                .as_os_str()
        );
        assert_ne!(
            selected.as_os_str(),
            selected.canonicalize().unwrap().as_os_str()
        );
    }

    #[cfg(unix)]
    #[test]
    fn select_package_dir_rejects_symlink_escapes_and_symlinked_descriptions() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("o-r-0123456");
        let outside = make_pkg_dir(tmp.path(), "outside", "nested");
        std::fs::create_dir_all(repo.join("pkgs")).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join("pkgs/escape")).unwrap();
        assert!(select_package_dir(&repo, "pkgs/escape", "nested").is_err());

        std::fs::write(repo.join("DESCRIPTION"), description("nested")).unwrap();
        std::os::unix::fs::symlink("..", repo.join("pkgs/root")).unwrap();
        assert!(
            select_package_dir(&repo, "pkgs/root", "nested").is_err(),
            "a nested selection must never resolve to the repository root"
        );

        let real = make_pkg_dir(&repo, "pkgs/real", "nested");
        std::os::unix::fs::symlink(&real, repo.join("pkgs/alias")).unwrap();
        assert!(select_package_dir(&repo, "pkgs/alias", "nested").is_err());
        assert!(select_package_dir(&repo, "pkgs/real", "nested").is_ok());

        let linked_desc = repo.join("pkgs/linkdesc");
        std::fs::create_dir_all(&linked_desc).unwrap();
        std::os::unix::fs::symlink(outside.join("DESCRIPTION"), linked_desc.join("DESCRIPTION"))
            .unwrap();
        assert!(select_package_dir(&repo, "pkgs/linkdesc", "nested").is_err());
    }
}
