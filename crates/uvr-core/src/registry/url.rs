//! Direct source-tarball dependencies: `pkg = { url = "https://…/pkg_1.0.tar.gz" }` (#189).
//!
//! Lock time downloads the tarball, checks that it is an R *source* package,
//! and pins its bytes with a `sha256:` checksum. Sync downloads it again
//! through the normal downloader, which refuses bytes that do not match.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use semver::Version;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::manifest::RemoteEntry;
use crate::registry::PackageInfo;

/// Is `spec` an http(s) URL whose path ends in `.tar.gz` or `.tgz`?
pub fn is_source_tarball_url(spec: &str) -> bool {
    reqwest::Url::parse(spec).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && (url.path().ends_with(".tar.gz") || url.path().ends_with(".tgz"))
    })
}

/// Download `url` and describe the source package in it: the package (with
/// its `sha256:` checksum), its DESCRIPTION `Remotes:`, and its install-time
/// dependency names (which bind nested `Remotes:` entries, as for git sources).
///
/// No credentials are sent. A private host fails with its plain HTTP status.
pub async fn resolve_url_package(
    client: &reqwest::Client,
    url: &str,
) -> Result<(PackageInfo, Vec<RemoteEntry>, BTreeSet<String>)> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(UvrError::Other(format!(
            "Failed to download {url}: HTTP {status}"
        )));
    }
    // ponytail: whole tarball in memory; stream to a temp file if huge source packages show up.
    let bytes = resp.bytes().await?;
    describe_source_tarball(url, &bytes)
}

fn describe_source_tarball(
    url: &str,
    bytes: &[u8],
) -> Result<(PackageInfo, Vec<RemoteEntry>, BTreeSet<String>)> {
    let fields = source_description(bytes).map_err(|reason| {
        UvrError::Other(format!(
            "{url} is not an R source package tarball: {reason}"
        ))
    })?;
    let name = fields["Package"].clone();
    if !crate::package_name::is_valid(&name) {
        return Err(UvrError::Other(format!(
            "The DESCRIPTION in {url} declares an invalid `Package:` name '{name}'."
        )));
    }
    let raw_version = fields["Version"].clone();
    let version =
        Version::parse(&crate::resolver::normalize_version(&raw_version)).map_err(|e| {
            UvrError::Other(format!(
                "The DESCRIPTION in {url} has an unparseable `Version:` '{raw_version}': {e}"
            ))
        })?;
    let remotes = fields
        .get("Remotes")
        .map(|field| crate::manifest::parse_remotes_field_rich(field))
        .unwrap_or_default();
    let install_dependencies =
        crate::registry::github::parse_description_install_dependency_names(&fields);
    Ok((
        PackageInfo {
            name,
            version,
            source: PackageSource::Url,
            checksum: Some(crate::checksum::sha256_hex(bytes)),
            requires: crate::registry::github::parse_description_deps(&fields),
            url: url.to_string(),
            raw_version: Some(raw_version),
            system_requirements: None,
            subdirectory: None,
        },
        remotes,
        install_dependencies,
    ))
}

/// DESCRIPTION fields of a gzip tar holding exactly one top-level directory
/// with a DESCRIPTION that names `Package:` and `Version:` and has no
/// `Built:` field. The error is the reason, phrased to follow "…tarball: ".
fn source_description(bytes: &[u8]) -> std::result::Result<BTreeMap<String, String>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Err("the download is not gzip-compressed (is the URL a web page?)".into());
    }
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(crate::installer::tar_compat::LinkSizeFix::new(decoder));
    let unreadable = |e: std::io::Error| format!("it is not a readable tar archive ({e})");
    let mut top_level = BTreeSet::new();
    let mut description = None;
    for entry in archive.entries().map_err(unreadable)? {
        let mut entry = entry.map_err(unreadable)?;
        if matches!(
            entry.header().entry_type(),
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader
        ) {
            continue;
        }
        let path = entry.path().map_err(unreadable)?.into_owned();
        let mut parts = path
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir));
        let Some(top) = parts.next() else {
            continue;
        };
        top_level.insert(top.as_os_str().to_owned());
        let rest: Vec<_> = parts.collect();
        if let [file] = rest.as_slice() {
            if file.as_os_str() == "DESCRIPTION" {
                let mut raw = Vec::new();
                entry.read_to_end(&mut raw).map_err(unreadable)?;
                description = Some(String::from_utf8_lossy(&raw).into_owned());
            }
        }
    }
    if top_level.len() != 1 {
        return Err(format!(
            "expected exactly one top-level directory, found {}",
            top_level.len()
        ));
    }
    let description =
        description.ok_or("there is no DESCRIPTION file in its top-level directory")?;
    let fields = crate::dcf::parse_dcf_fields(&description);
    for field in ["Package", "Version"] {
        if fields.get(field).is_none_or(|v| v.is_empty()) {
            return Err(format!("its DESCRIPTION has no `{field}:` field"));
        }
    }
    if fields.contains_key("Built") {
        return Err(
            "it is a pre-built binary package (its DESCRIPTION has a `Built:` field); \
             point the URL at the source tarball instead"
                .into(),
        );
    }
    Ok(fields)
}

/// Serve `body` with `status` (e.g. `"200 OK"`) to every request, on a
/// loopback port. Returns the base URL. The server thread lives until the
/// test process exits.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn serve_for_test(status: &'static str, body: Vec<u8>) -> String {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut socket in listener.incoming().flatten() {
            let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            // Read the whole request head before answering.
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let _ = write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(&body);
        }
    });
    base
}

/// A gzip tar with `files` (path, contents) — test tarballs.
#[cfg(test)]
pub(crate) fn tarball_for_test(files: &[(&str, &str)]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut builder = tar::Builder::new(&mut enc);
        for (path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, contents.as_bytes()).unwrap();
        }
        builder.finish().unwrap();
    }
    enc.finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.org/tpkg_1.2-0.tar.gz";
    const DESCRIPTION: &str = "Package: tpkg\nVersion: 1.2-0\nImports: cli (>= 3.0.0), utils\n\
                               LinkingTo: cpp11\nSuggests: testthat\n\
                               Remotes: r-lib/cli\n";

    fn describe(files: &[(&str, &str)]) -> Result<PackageInfo> {
        describe_source_tarball(URL, &tarball_for_test(files)).map(|(info, _, _)| info)
    }

    fn rejection(files: &[(&str, &str)]) -> String {
        describe(files).unwrap_err().to_string()
    }

    #[test]
    fn sniffs_only_http_source_tarball_urls() {
        for ok in [
            "https://example.org/tpkg_1.2.0.tar.gz",
            "http://127.0.0.1:8080/a/tpkg_1.2.0.tgz",
            "https://example.org/tpkg_1.2.0.tar.gz?token=x",
        ] {
            assert!(is_source_tarball_url(ok), "{ok}");
        }
        for bad in [
            "https://example.org/tpkg_1.2.0.zip",
            "https://example.org/",
            "https://example.org/download?file=tpkg.tar.gz",
            "ftp://example.org/tpkg_1.2.0.tar.gz",
            "file:///tmp/tpkg_1.2.0.tar.gz",
            "tpkg_1.2.0.tar.gz",
            "user/repo",
        ] {
            assert!(!is_source_tarball_url(bad), "{bad}");
        }
    }

    #[test]
    fn describes_a_source_tarball() {
        let bytes = tarball_for_test(&[
            ("tpkg/DESCRIPTION", DESCRIPTION),
            ("tpkg/R/tpkg.R", "f <- function() 1\n"),
        ]);
        let (info, remotes, install_dependencies) = describe_source_tarball(URL, &bytes).unwrap();
        assert_eq!(info.name, "tpkg");
        assert_eq!(info.version.to_string(), "1.2.0");
        assert_eq!(info.raw_version.as_deref(), Some("1.2-0"));
        assert_eq!(info.source, PackageSource::Url);
        assert_eq!(info.url, URL);
        assert_eq!(info.checksum, Some(crate::checksum::sha256_hex(&bytes)));
        let requires: Vec<&str> = info.requires.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(requires, ["cli", "cpp11"]);
        assert_eq!(remotes.len(), 1);
        assert!(install_dependencies.contains("cpp11"));
        assert!(!install_dependencies.contains("testthat"));
    }

    #[test]
    fn accepts_dot_prefixed_member_paths() {
        let info = describe(&[("./tpkg/DESCRIPTION", "Package: tpkg\nVersion: 1.0\n")]).unwrap();
        assert_eq!(info.name, "tpkg");
    }

    #[test]
    fn rejects_a_built_binary_package() {
        let msg = rejection(&[(
            "tpkg/DESCRIPTION",
            "Package: tpkg\nVersion: 1.0\nBuilt: R 4.5.0; ; 2025-01-15; unix\n",
        )]);
        assert!(msg.contains("pre-built binary"), "{msg}");
        assert!(msg.contains(URL), "{msg}");
    }

    #[test]
    fn rejects_non_gzip_downloads() {
        let err = describe_source_tarball(URL, b"<!DOCTYPE html><html></html>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an R source package tarball"), "{err}");
        assert!(err.contains("not gzip-compressed"), "{err}");

        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut enc, b"just some gzip text, no tar").unwrap();
        let err = describe_source_tarball(URL, &enc.finish().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an R source package tarball"), "{err}");
    }

    #[test]
    fn rejects_bad_layouts_and_descriptions() {
        let two_dirs = rejection(&[
            ("a/DESCRIPTION", "Package: a\nVersion: 1.0\n"),
            ("b/DESCRIPTION", "Package: b\nVersion: 1.0\n"),
        ]);
        assert!(
            two_dirs.contains("exactly one top-level directory"),
            "{two_dirs}"
        );

        let no_description = rejection(&[("tpkg/README", "hi")]);
        assert!(
            no_description.contains("no DESCRIPTION"),
            "{no_description}"
        );

        let nested_only = rejection(&[("tpkg/inst/DESCRIPTION", "Package: x\nVersion: 1\n")]);
        assert!(nested_only.contains("no DESCRIPTION"), "{nested_only}");

        let no_version = rejection(&[("tpkg/DESCRIPTION", "Package: tpkg\n")]);
        assert!(no_version.contains("`Version:`"), "{no_version}");

        let bad_name = rejection(&[("tpkg/DESCRIPTION", "Package: bad name\nVersion: 1.0\n")]);
        assert!(bad_name.contains("invalid `Package:`"), "{bad_name}");
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn resolve_downloads_and_pins_the_served_bytes() {
        let bytes = tarball_for_test(&[("tpkg/DESCRIPTION", "Package: tpkg\nVersion: 1.0\n")]);
        let url = format!(
            "{}/tpkg_1.0.tar.gz",
            serve_for_test("200 OK", bytes.clone())
        );
        let (info, remotes, _) = resolve_url_package(&reqwest::Client::new(), &url)
            .await
            .unwrap();
        assert_eq!(info.name, "tpkg");
        assert_eq!(info.url, url);
        assert_eq!(info.checksum, Some(crate::checksum::sha256_hex(&bytes)));
        assert!(remotes.is_empty());
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn resolve_reports_http_errors_plainly() {
        let url = format!(
            "{}/tpkg_1.0.tar.gz",
            serve_for_test("403 Forbidden", b"denied".to_vec())
        );
        let err = resolve_url_package(&reqwest::Client::new(), &url)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(&url), "{err}");
        assert!(err.contains("403"), "{err}");
    }
}
