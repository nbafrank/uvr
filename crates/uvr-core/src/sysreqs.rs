use std::collections::HashMap;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::sysreqs_rules;

/// A resolved system dependency with its apt/rpm package name.
#[derive(Debug, Clone)]
pub struct SysReq {
    /// The system package name, e.g. `"libxml2-dev"`.
    pub package: String,
}

/// Detect the Linux distribution from `/etc/os-release`, normalized onto the
/// vocabulary the sysreqs catalogs use. Returns `id-version` like
/// `"ubuntu-22.04"` or `"redhat-8"`.
pub fn detect_linux_distro() -> Option<String> {
    let os = crate::os_release::detect()?;
    // Both halves are required here, unlike P3M slug detection: the sysreqs
    // API and the vendored rules are keyed by `id-version`, and there is no
    // sensible query for a distro that publishes no version at all — the
    // caller skips the check, the safe direction to fail, since a wrong
    // answer only produces a wrong hint. Note this is a narrower net than it
    // looks: rolling releases do not necessarily land here. `archlinux`
    // containers report a snapshot `VERSION_ID` (e.g. `20260726.0.562117`)
    // and so reach the catalogs as `arch`/<snapshot>, which simply matches
    // nothing (reported by @gdevenyi on #209).
    if os.id.is_empty() || os.version_id.is_empty() {
        return None;
    }
    let (distribution, release) = normalize_distro(&os.id, &os.version_id);
    Some(format!("{distribution}-{release}"))
}

/// Map an `/etc/os-release` `(ID, VERSION_ID)` pair onto the names Posit's
/// sysreqs API and the vendored `r-system-requirements` rules are written in.
///
/// Both catalogs speak the Posit vocabulary, not the os-release one, and both
/// reject the raw values RHEL reports (#209):
///
/// ```text
/// ?distribution=rhel&release=8.10  -> {"code":14,"error":"Unsupported system"}
/// ?distribution=redhat&release=8   -> {"requirements":[{"name":"xml2", ...}]}
/// ```
///
/// The vendored rules agree — every RHEL entry is written as `redhat` with
/// `versions: ["8"]`, so a `rhel` / `8.10` host matched no rule either and the
/// local fallback had nothing to offer once the API had bowed out. RHEL hosts
/// therefore got no system dependencies from either source.
///
/// Only the RHEL family is truncated to its major: Ubuntu is keyed `22.04`,
/// openSUSE `15.6` and SLE `12.3` in both catalogs.
///
/// Mapping onto a name a catalog knows is not the same as full API coverage —
/// the two catalogs disagree about which distros they serve, and the local
/// rules remain the backstop (measurements by @gdevenyi on #209):
///
/// - `rockylinux` is served by the API for 9 and 10 but not 8, so Rocky/Alma 8
///   still resolves via the local rules and still prints the degraded-check
///   warning. That is the pre-existing behaviour, not a regression from this
///   mapping.
/// - `fedora` and `alpine` are rejected by the API at every release; they are
///   local-rules-only by design.
pub(crate) fn normalize_distro(id: &str, version_id: &str) -> (String, String) {
    let distribution = match id {
        // Oracle Linux is a straight RHEL rebuild and reports `ID="ol"`, which
        // neither catalog knows (#209). Note this only routes its *sysreqs*;
        // P3M binary selection keys off a separate slug in `downloader.rs`.
        "rhel" | "ol" => "redhat",
        // The RHEL rebuilds share Rocky's entries — same repos (crb /
        // powertools), same package names.
        "rocky" | "almalinux" => "rockylinux",
        "sles" => "sle",
        "opensuse-leap" | "opensuse-tumbleweed" => "opensuse",
        other => other,
    };
    let release = match distribution {
        "redhat" | "rockylinux" | "centos" | "fedora" => {
            version_id.split('.').next().unwrap_or(version_id)
        }
        _ => version_id,
    };
    (distribution.to_string(), release.to_string())
}

/// Ask a Posit catalog under a name it actually publishes.
///
/// Both of Posit's catalogs — the sysreqs API and the platform list at
/// `/__api__/status` that P3M binary selection reads — are keyed by the same
/// `(distribution, release)` vocabulary, and both have the same holes in it,
/// so both need this. The two sysreqs sources do not cover the same ground. Verified against
/// `/__api__/status`, which lists every platform Posit serves: the API has
/// `rockylinux` 9 and 10 but no 8, and `centos` 7 and 8 but no Stream 9 or
/// 10. The vendored rules cover all four, so those hosts were already getting
/// correct answers — they were just getting them from the fallback, with a
/// "check degraded" warning, for distros Posit knows perfectly well under
/// their RHEL name.
///
/// Applied only when querying a catalog. The vendored local rules
/// deliberately keep the unaliased pair, because the two are *not*
/// interchangeable there:
/// `rockylinux` 8 resolves `leptonica-devel` where `redhat` 8 resolves
/// nothing, and `centos` 8 says `libarchive-devel` where `redhat` 8 says
/// `libarchive`. Aliasing the local lookup too would trade a spurious warning
/// for wrong package names.
pub(crate) fn catalog_alias<'a>(distribution: &'a str, release: &'a str) -> (&'a str, &'a str) {
    match (distribution, release) {
        // Rocky/Alma 8. Posit publishes rockylinux from 9 on.
        ("rockylinux", "8") => ("redhat", "8"),
        // CentOS Stream. Posit's `centos` stops at 8; Stream tracks the
        // RHEL major of the same number.
        ("centos", r) if r.parse::<u32>().is_ok_and(|n| n >= 9) => ("redhat", r),
        _ => (distribution, release),
    }
}

/// Response from the Posit Package Manager sysreqs API.
#[derive(Debug, Deserialize)]
struct PpmSysreqsResponse {
    #[serde(default)]
    requirements: Vec<PpmRequirement>,
}

#[derive(Debug, Deserialize)]
struct PpmRequirement {
    #[serde(default)]
    name: String,
    #[serde(default)]
    requirements: PpmRequirementDetail,
}

#[derive(Debug, Default, Deserialize)]
struct PpmCommand {
    #[serde(default)]
    command: String,
}

#[derive(Debug, Default, Deserialize)]
struct PpmRequirementDetail {
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    pre_install: Vec<PpmCommand>,
    #[serde(default)]
    post_install: Vec<PpmCommand>,
}

/// One package's entry in the batched sysreqs index: its distro packages
/// plus any setup commands the API attaches to them (e.g. enabling EPEL
/// before a `dnf install`, or `R CMD javareconf` after one).
#[derive(Debug, Default)]
struct SysReqIndexEntry {
    packages: Vec<SysReq>,
    pre_install: Vec<String>,
    post_install: Vec<String>,
}

/// Index a batched (`all=true`) sysreqs response by R package name.
///
/// Packages absent from the map declare no system requirements — the API
/// only lists those that do.
///
/// Fallible on purpose: an infallible version that swallowed the parse error
/// into an empty map would make a malformed 200 response look identical to
/// "no package on this distro needs anything", silently and on every distro.
/// The per-package path this replaces mapped a parse failure to
/// `LookupFailed`; keep that.
fn parse_sysreqs_index(
    body: &str,
) -> std::result::Result<HashMap<String, SysReqIndexEntry>, serde_json::Error> {
    let mut index: HashMap<String, SysReqIndexEntry> = HashMap::new();
    let response: PpmSysreqsResponse = serde_json::from_str(body)?;
    for req in response.requirements {
        if req.name.is_empty() {
            continue;
        }
        let entry = index.entry(req.name).or_default();
        for pkg in req.requirements.packages {
            if !pkg.is_empty() {
                entry.packages.push(SysReq { package: pkg });
            }
        }
        for c in req.requirements.pre_install {
            if !c.command.is_empty() && !entry.pre_install.contains(&c.command) {
                entry.pre_install.push(c.command);
            }
        }
        for c in req.requirements.post_install {
            if !c.command.is_empty() && !entry.post_install.contains(&c.command) {
                entry.post_install.push(c.command);
            }
        }
    }
    Ok(index)
}

/// Detects the Posit PPM "Unsupported system" error body.
///
/// Response shape: `{"code":14,"error":"Unsupported system"}`. Match on the
/// error text rather than the status code, since we've only observed this on
/// non-success responses but don't want to couple to a specific HTTP code.
fn is_unsupported_system_body(body: &str) -> bool {
    body.contains("Unsupported system")
}

/// Outcome of the one-per-sync batched sysreqs fetch.
enum SysReqIndex {
    Supported(HashMap<String, SysReqIndexEntry>),
    UnsupportedDistro,
    LookupFailed,
}

/// Fetch every package's system requirements for `distro` in one request.
///
/// Replaces the per-package `all=false&pkgname=` loop: a 68-package sync
/// made 68 requests, where `all=true` answers in one (~150 KB, ~1125
/// entries for ubuntu 22.04).
async fn fetch_sysreqs_index(client: &reqwest::Client, distro: &str) -> SysReqIndex {
    let (distribution, release) = distro.split_once('-').unwrap_or((distro, ""));
    // #214: the batched endpoint speaks the same catalog vocabulary as the
    // per-package one it replaced — alias RHEL-family identities the API
    // doesn't serve onto the ones it does before asking.
    let (distribution, release) = catalog_alias(distribution, release);
    let url = "https://packagemanager.posit.co/__api__/repos/1/sysreqs";

    debug!("Fetching Posit sysreqs index (distro={distribution}, release={release})");

    let resp = client
        .get(url)
        .query(&[
            ("all", "true"),
            ("distribution", distribution),
            ("release", release),
        ])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            warn!("Posit sysreqs API request failed: {e}");
            return SysReqIndex::LookupFailed;
        }
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        if is_unsupported_system_body(&body) {
            debug!("Posit sysreqs API reports {distribution} is unsupported");
            return SysReqIndex::UnsupportedDistro;
        }
        warn!("Posit sysreqs API returned {status} for the {distribution} index");
        return SysReqIndex::LookupFailed;
    }

    match parse_sysreqs_index(&body) {
        Ok(index) => SysReqIndex::Supported(index),
        Err(e) => {
            warn!("Failed to parse Posit sysreqs index: {e}");
            SysReqIndex::LookupFailed
        }
    }
}

/// Check which packages are missing on the system.
/// Uses `dpkg -s` on Debian/Ubuntu, `rpm -q` on Fedora/RHEL/SUSE.
/// If neither package manager is found, returns an empty list (skip check).
pub fn filter_missing(packages: &[SysReq]) -> Vec<&SysReq> {
    let (cmd, args): (&str, &[&str]) = if which::which("dpkg").is_ok() {
        ("dpkg", &["-s"])
    } else if which::which("rpm").is_ok() {
        ("rpm", &["-q"])
    } else if which::which("apk").is_ok() {
        ("apk", &["info", "-e"])
    } else {
        debug!("No supported package manager (dpkg/rpm/apk) found, skipping sysreqs check");
        return vec![];
    };

    packages
        .iter()
        .filter(|req| {
            let output = std::process::Command::new(cmd)
                .args(args)
                .arg(&req.package)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match output {
                Ok(status) => !status.success(),
                Err(_) => false, // command failed to run — don't report as missing
            }
        })
        .collect()
}

/// Aggregate result of a sysreqs check across many packages.
#[derive(Debug, Default)]
pub struct SysReqsCheck {
    /// Missing system packages keyed by R package name.
    pub missing: HashMap<String, Vec<SysReq>>,
    /// Set when the Posit API reported the distro as unsupported.
    /// When true, the API contributed nothing — but the vendored local
    /// rules still ran, so `missing` may well be authoritative. Read this
    /// together with `local_resolved` before claiming the check was skipped.
    pub unsupported_distro: bool,
    /// Set when the single batched index fetch failed outright
    /// (network/HTTP/parse, #148). Every package was checked against the
    /// vendored local rules instead.
    pub lookup_failed: bool,
    /// Number of packages the vendored local rules resolved to at least one
    /// system package. Distinguishes "the fallback ran and found nothing
    /// missing" from "the fallback had nothing to say" — conflating those is
    /// what made a successful Alpine check report itself as skipped.
    pub local_resolved: usize,
    /// Deduplicated setup commands for the rules that produced `missing`.
    pub pre_install: Vec<String>,
    /// Deduplicated commands to run after the package install.
    pub post_install: Vec<String>,
}

/// R package to check sysreqs for.
#[derive(Debug, Clone)]
pub struct PackageSysReqQuery {
    /// Canonical R package name (e.g. `"xml2"`).
    pub name: String,
    /// Raw `SystemRequirements` field from DESCRIPTION, if any. Used only
    /// when the Posit API rejects the distribution and we fall back to
    /// the vendored `r-system-requirements` rules locally.
    pub system_requirements: Option<String>,
    /// True for Bioconductor-sourced packages. The Posit sysreqs API only
    /// covers CRAN and returns HTTP 500 for Bioc names, so looking one up in
    /// the fetched index would be a guaranteed non-match with zero
    /// information (#202) — Bioc packages go straight to the vendored local
    /// rules instead.
    pub bioc: bool,
}

/// Resolve and check system dependencies for a set of packages.
///
/// Flow:
/// 1. Fetch the whole distro's sysreqs index in a single batched request
///    (see `fetch_sysreqs_index`).
/// 2. If PPM reports `UnsupportedDistro` (e.g. Alpine) or the fetch failed
///    outright, fall back to the vendored `r-system-requirements` rules for
///    every package. The fallback matches each package's
///    `SystemRequirements` string against the local rule table. This is the
///    path that addresses issue #30 end-to-end.
/// 3. Bioc packages always go straight to the local rules, regardless of
///    the index outcome — the Posit API is CRAN-only and 500s for Bioc
///    names (#202).
/// 4. Filter the resolved deps through the installed package manager
///    (`dpkg`/`rpm`/`apk`) to surface only the ones that are actually
///    missing.
///
/// Returns both the missing-deps map and flags indicating whether PPM
/// rejected the distribution or the fetch failed (set true even when the
/// local fallback fills in results, so callers can mention the provenance if
/// they want).
pub async fn check_system_deps(
    client: &reqwest::Client,
    packages: &[PackageSysReqQuery],
    distro: &str,
) -> SysReqsCheck {
    let mut out = SysReqsCheck::default();

    // Skip the fetch entirely when every package in the sync is Bioc-sourced:
    // the index would never be consulted (see the loop below), so fetching
    // it would just be a wasted round trip that could also spuriously flag
    // `lookup_failed` on a network hiccup, even though nothing in this sync
    // actually depended on reaching the API (#202).
    let needs_index = packages.iter().any(|pkg| !pkg.bioc);

    // One request for the whole sync, not one per package.
    let index = if needs_index {
        match fetch_sysreqs_index(client, distro).await {
            SysReqIndex::Supported(idx) => Some(idx),
            SysReqIndex::UnsupportedDistro => {
                out.unsupported_distro = true;
                None
            }
            SysReqIndex::LookupFailed => {
                out.lookup_failed = true;
                None
            }
        }
    } else {
        None
    };

    apply_index(&mut out, packages, index.as_ref(), distro);

    out
}

/// Decide, per package, whether to use the fetched index or the vendored
/// local rules, and record the outcome.
///
/// Synchronous and free of I/O beyond `filter_missing`'s package-manager
/// queries, so it can be exercised directly with a hand-built `index`
/// instead of requiring network access — that is the whole reason this is
/// split out of `check_system_deps`.
fn apply_index(
    out: &mut SysReqsCheck,
    packages: &[PackageSysReqQuery],
    index: Option<&HashMap<String, SysReqIndexEntry>>,
    distro: &str,
) {
    for pkg in packages {
        // The Posit API is CRAN-only; Bioc names are absent from its index,
        // so check them against the vendored local rules directly (#202).
        let Some(index) = index.filter(|_| !pkg.bioc) else {
            check_pkg_local(out, pkg, distro);
            continue;
        };
        let Some(entry) = index.get(&pkg.name) else {
            // Absent from the index means the package declares no system
            // requirements, which is the common case.
            continue;
        };
        let missing = filter_missing(&entry.packages);
        if !missing.is_empty() {
            // Same rule as the local path: setup commands only matter when
            // something is actually missing — otherwise every sync on a
            // Rocky box would re-enable EPEL.
            //
            // Dedup keeps the first occurrence across packages, which assumes
            // rule commands are order-independent across rules — see the
            // ordering note on `sysreqs_rules::resolve_local` for the one
            // known (EOL-Ubuntu-only) case where that assumption bites.
            for cmd in &entry.pre_install {
                if !out.pre_install.contains(cmd) {
                    out.pre_install.push(cmd.clone());
                }
            }
            for cmd in &entry.post_install {
                if !out.post_install.contains(cmd) {
                    out.post_install.push(cmd.clone());
                }
            }
            out.missing
                .insert(pkg.name.clone(), missing.into_iter().cloned().collect());
        }
    }
}

/// Check one package's `SystemRequirements` against the vendored
/// `r-system-requirements` rules and record any missing system packages.
/// Shared by the unsupported-distro / lookup-failed fallback in
/// `apply_index` and the direct Bioc bypass in the same function.
fn check_pkg_local(out: &mut SysReqsCheck, pkg: &PackageSysReqQuery, distro: &str) {
    let (distribution, version) = distro.split_once('-').unwrap_or((distro, ""));
    let Some(sys_req_text) = pkg.system_requirements.as_deref() else {
        return;
    };
    let local = sysreqs_rules::resolve_local(sys_req_text, distribution, version);
    let resolved: Vec<SysReq> = local
        .packages
        .into_iter()
        .map(|package| SysReq { package })
        .collect();
    if resolved.is_empty() {
        return;
    }
    // Past this point the local rules produced a real answer for this
    // package, whether or not anything turns out to be missing.
    out.local_resolved += 1;
    let missing = filter_missing(&resolved);
    if !missing.is_empty() {
        // Setup commands only matter when something is actually missing —
        // otherwise every sync on a Rocky box would re-enable EPEL.
        for cmd in local.pre_install {
            if !out.pre_install.contains(&cmd) {
                out.pre_install.push(cmd);
            }
        }
        for cmd in local.post_install {
            if !out.post_install.contains(&cmd) {
                out.post_install.push(cmd);
            }
        }
        out.missing
            .insert(pkg.name.clone(), missing.into_iter().cloned().collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bioc_packages_skip_the_posit_api_entirely() {
        // #202: the Posit sysreqs API 500s for every Bioc name. Bioc-flagged
        // queries must go straight to local rules: no request is made, so
        // neither lookup_failed (API contact failed) nor unsupported_distro
        // can be set — on any machine, online or offline. If this test hangs
        // or flags lookup_failed, the API skip regressed.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let queries = vec![
            PackageSysReqQuery {
                name: "SummarizedExperiment".into(),
                system_requirements: None,
                bioc: true,
            },
            PackageSysReqQuery {
                name: "Rhtslib".into(),
                system_requirements: Some("libbz2 & liblzma & GNU make".into()),
                bioc: true,
            },
        ];
        let check = check_system_deps(&client, &queries, "ubuntu-22.04").await;
        assert!(!check.lookup_failed, "no API contact → no failed lookups");
        assert!(!check.unsupported_distro);
    }

    #[test]
    fn rhel_normalizes_to_the_posit_vocabulary() {
        // #209: UBI/RHEL report ID="rhel", VERSION_ID="8.10". Posit's API
        // answers only for redhat/8, and every vendored rule is written as
        // `redhat` with versions ["8"] — so the raw pair matched neither.
        assert_eq!(
            normalize_distro("rhel", "8.10"),
            ("redhat".to_string(), "8".to_string())
        );
        assert_eq!(
            normalize_distro("rocky", "9.3"),
            ("rockylinux".to_string(), "9".to_string())
        );
        assert_eq!(
            normalize_distro("almalinux", "10.0"),
            ("rockylinux".to_string(), "10".to_string())
        );
        assert_eq!(
            normalize_distro("sles", "12.3"),
            ("sle".to_string(), "12.3".to_string())
        );
    }

    #[test]
    fn oracle_linux_is_treated_as_a_rhel_rebuild() {
        // #209, from @gdevenyi's image sweep: `oraclelinux:8` reports
        // ID="ol", VERSION_ID="8.10" — unknown to both catalogs, so OL
        // resolved nothing at all.
        assert_eq!(
            normalize_distro("ol", "8.10"),
            ("redhat".to_string(), "8".to_string())
        );
        let (distribution, release) = normalize_distro("ol", "8.10");
        let pkgs = sysreqs_rules::resolve_local("libxml2 (>= 2.6.3)", &distribution, &release);
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-devel"),
            "expected libxml2-devel for ol-8.10, got {pkgs:?}"
        );
    }

    #[test]
    fn centos_keeps_its_own_identity() {
        // CentOS has its own rule entries (epel / powertools pre-installs) and
        // the API rejects it at every release, so it stays local-rules-only
        // rather than being folded into `redhat` here. Mapping Stream >= 9 to
        // `redhat` would upgrade it to API answers, but that is a behaviour
        // change with a 7/8 boundary to argue, not part of this fix.
        assert_eq!(
            normalize_distro("centos", "9"),
            ("centos".to_string(), "9".to_string())
        );
        let pkgs = sysreqs_rules::resolve_local("libxml2 (>= 2.6.3)", "centos", "9");
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-devel"),
            "expected the version-less centos rules to still resolve, got {pkgs:?}"
        );
    }

    #[test]
    fn non_rhel_distros_keep_their_full_version() {
        // Ubuntu is keyed "22.04" and openSUSE "15.6" in both catalogs;
        // truncating either to a major would match nothing.
        assert_eq!(
            normalize_distro("ubuntu", "22.04"),
            ("ubuntu".to_string(), "22.04".to_string())
        );
        assert_eq!(
            normalize_distro("opensuse-leap", "15.6"),
            ("opensuse".to_string(), "15.6".to_string())
        );
        assert_eq!(
            normalize_distro("debian", "12"),
            ("debian".to_string(), "12".to_string())
        );
        // Alpine keeps its patch version; `resolve_local` truncates it to
        // major.minor when matching rules (#30).
        assert_eq!(
            normalize_distro("alpine", "3.24.1"),
            ("alpine".to_string(), "3.24.1".to_string())
        );
    }

    #[test]
    fn normalized_rhel_resolves_against_the_local_rules() {
        // The other half of #209: with the normalized pair the vendored rules
        // finally match, so the local fallback works even when the API can't
        // be reached.
        let (distribution, release) = normalize_distro("rhel", "8.10");
        let pkgs = sysreqs_rules::resolve_local("libxml2 (>= 2.6.3)", &distribution, &release);
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-devel"),
            "expected libxml2-devel for rhel-8.10, got {pkgs:?}"
        );
        let gdal = sysreqs_rules::resolve_local("GDAL (>= 2.2.3)", &distribution, &release);
        assert!(
            gdal.packages.iter().any(|p| p == "gdal-devel"),
            "expected gdal-devel for rhel-8.10, got {gdal:?}"
        );
    }

    #[test]
    fn catalogs_are_asked_under_a_name_they_publish() {
        // Posit serves rockylinux from 9 on, and centos only through 8
        // (verified against /__api__/status). Asking under the RHEL name is
        // what turns a degraded-check warning back into a real answer.
        assert_eq!(catalog_alias("rockylinux", "8"), ("redhat", "8"));
        assert_eq!(catalog_alias("centos", "9"), ("redhat", "9"));
        assert_eq!(catalog_alias("centos", "10"), ("redhat", "10"));

        // Everything Posit does publish is left alone.
        assert_eq!(catalog_alias("rockylinux", "9"), ("rockylinux", "9"));
        assert_eq!(catalog_alias("rockylinux", "10"), ("rockylinux", "10"));
        assert_eq!(catalog_alias("centos", "7"), ("centos", "7"));
        assert_eq!(catalog_alias("centos", "8"), ("centos", "8"));
        assert_eq!(catalog_alias("redhat", "8"), ("redhat", "8"));
        assert_eq!(catalog_alias("ubuntu", "22.04"), ("ubuntu", "22.04"));
        // Not a release number: must not be mistaken for a Stream major.
        assert_eq!(catalog_alias("centos", "stream"), ("centos", "stream"));
    }

    #[test]
    fn the_local_rules_are_not_aliased() {
        // The alias exists for the API only. These pairs resolve *differently*
        // in the vendored rules, so aliasing the local lookup as well would
        // trade a spurious warning for wrong package names.
        assert!(
            sysreqs_rules::resolve_local("leptonica", "rockylinux", "8")
                .iter()
                .any(|p| p == "leptonica-devel"),
            "rockylinux 8 carries leptonica-devel"
        );
        assert!(
            sysreqs_rules::resolve_local("leptonica", "redhat", "8").is_empty(),
            "redhat 8 does not — aliasing the local lookup would lose it"
        );
        // Both halves of the libarchive divergence, so the test actually
        // guards the claim: aliasing the local lookup would swap a headers
        // package for a runtime one, and a source build would fail at
        // configure with nothing pointing at the cause.
        assert!(
            sysreqs_rules::resolve_local("libarchive", "centos", "8")
                .iter()
                .any(|p| p == "libarchive-devel"),
            "centos 8 wants the -devel package"
        );
        assert_eq!(
            sysreqs_rules::resolve_local("libarchive", "redhat", "8"),
            vec!["libarchive".to_string()],
            "redhat 8 names the runtime package, not the headers"
        );
    }

    #[test]
    fn oracle_linux_also_gets_rhel_binaries() {
        // The sysreqs half of this alias landed with #209; the binary half did
        // not, so OL hosts resolved their system deps correctly and then
        // compiled every package because `ol-8.10` maps to no PPM codename.
        // Both axes have to agree or the alias is only half applied.
        assert_eq!(
            crate::r_version::downloader::detect_posit_distro_slug_from_os_release(Some(
                "ID=ol\nVERSION_ID=8.10\n"
            )),
            "rhel-8"
        );
        assert_eq!(
            crate::registry::p3m::ppm_linux_codename("rhel-8"),
            Some("centos8")
        );
    }

    #[test]
    fn detect_distro_format() {
        // This test only makes assertions on Linux
        if cfg!(target_os = "linux") {
            if let Some(distro) = detect_linux_distro() {
                assert!(
                    distro.contains('-'),
                    "expected format 'id-version', got: {distro}"
                );
            }
        }
    }

    #[test]
    fn filter_missing_with_nonexistent_package() {
        if !cfg!(target_os = "linux") {
            return;
        }
        // `filter_missing` documents itself as a no-op without dpkg/rpm/apk,
        // so asserting it reports something requires one of them to exist.
        // The test used to assume every Linux has one and failed on Arch,
        // NixOS, Gentoo and friends — a false alarm for anyone developing
        // uvr there.
        if which::which("dpkg").is_err()
            && which::which("rpm").is_err()
            && which::which("apk").is_err()
        {
            eprintln!("skipping: no dpkg/rpm/apk on this system");
            return;
        }
        let reqs = vec![SysReq {
            package: "uvr-nonexistent-pkg-12345".to_string(),
        }];
        let missing = filter_missing(&reqs);
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn filter_missing_is_a_no_op_without_a_package_manager() {
        // The other half of the contract: with no supported package manager
        // we report nothing missing rather than guessing. Callers must treat
        // that as "unverified", not "verified clean" — see `check_system_deps`.
        if which::which("dpkg").is_ok()
            || which::which("rpm").is_ok()
            || which::which("apk").is_ok()
        {
            eprintln!("skipping: a supported package manager is present");
            return;
        }
        let reqs = vec![SysReq {
            package: "uvr-nonexistent-pkg-12345".to_string(),
        }];
        assert!(filter_missing(&reqs).is_empty());
    }

    #[test]
    fn detects_ppm_unsupported_system_body() {
        // Observed on Alpine across 3.15–3.21 (issue #30).
        assert!(is_unsupported_system_body(
            r#"{"code":14,"error":"Unsupported system"}"#
        ));
        assert!(!is_unsupported_system_body(r#"{"requirements":[]}"#));
        assert!(!is_unsupported_system_body(""));
    }

    #[test]
    fn local_fallback_resolves_alpine_xml2_requirements() {
        // Direct smoke test of the fallback path: given an Alpine-targeted
        // SystemRequirements string, the vendored rules should produce the
        // apk-compatible package name. This is the invariant issue #30 needs.
        let pkgs = sysreqs_rules::resolve_local("libxml2 (>= 2.9.0)", "alpine", "3.21");
        assert!(
            pkgs.packages.iter().any(|p| p == "libxml2-dev"),
            "expected libxml2-dev in fallback output, got {pkgs:?}"
        );
    }

    #[test]
    fn local_resolution_is_recorded_even_when_nothing_is_missing() {
        // The Alpine bug: the vendored rules DO resolve `libxml2` on
        // alpine-3.23.5, so a run where every resolved package is already
        // installed is a *successful* check, not a skipped one. The counter
        // is incremented before `filter_missing` runs, so this assertion is
        // independent of what is installed on the test host.
        let mut out = SysReqsCheck::default();
        let pkg = PackageSysReqQuery {
            name: "xml2".to_string(),
            system_requirements: Some(
                "libxml2: libxml2-dev (deb), libxml2-devel (rpm)".to_string(),
            ),
            bioc: false,
        };
        check_pkg_local(&mut out, &pkg, "alpine-3.23.5");
        assert_eq!(
            out.local_resolved, 1,
            "libxml2 resolves to libxml2-dev on alpine; that is a real answer"
        );
    }

    #[test]
    fn no_sysreqs_field_does_not_count_as_a_local_resolution() {
        let mut out = SysReqsCheck::default();
        let pkg = PackageSysReqQuery {
            name: "jsonlite".to_string(),
            system_requirements: None,
            bioc: false,
        };
        check_pkg_local(&mut out, &pkg, "alpine-3.23.5");
        assert_eq!(out.local_resolved, 0);
    }

    #[test]
    fn unmatched_sysreqs_text_does_not_count_as_a_local_resolution() {
        // A non-empty SystemRequirements string that matches no vendored rule
        // is the genuine "we could not check this" case and must stay
        // distinguishable from the two above.
        let mut out = SysReqsCheck::default();
        let pkg = PackageSysReqQuery {
            name: "madeup".to_string(),
            system_requirements: Some("a-library-no-rule-mentions-xyzzy".to_string()),
            bioc: false,
        };
        check_pkg_local(&mut out, &pkg, "alpine-3.23.5");
        assert_eq!(out.local_resolved, 0);
    }

    #[test]
    fn no_index_falls_back_to_local_rules_for_every_package() {
        // The unsupported-distro / lookup-failed case: `apply_index` gets
        // `index = None` and must run every package through the vendored
        // local rules rather than skipping them. Assert on `local_resolved`,
        // not `missing` — the counter is incremented before `filter_missing`
        // runs, so it holds regardless of what is installed on the test
        // host (the Alpine bug this whole check exists to catch).
        let mut out = SysReqsCheck::default();
        let packages = vec![PackageSysReqQuery {
            name: "xml2".to_string(),
            system_requirements: Some(
                "libxml2: libxml2-dev (deb), libxml2-devel (rpm)".to_string(),
            ),
            bioc: false,
        }];
        apply_index(&mut out, &packages, None, "alpine-3.23.5");
        assert_eq!(
            out.local_resolved, 1,
            "with no index, xml2 must be resolved via the local rules"
        );
    }

    #[test]
    fn a_package_present_in_the_index_is_not_also_checked_locally() {
        // The common, covered-distro case: when the index has an entry, it
        // is authoritative and the local rules must NOT run — even though
        // `system_requirements` here would resolve locally too, proving the
        // index path was actually taken instead of the fallback.
        let mut index = HashMap::new();
        index.insert(
            "curl".to_string(),
            SysReqIndexEntry {
                packages: vec![SysReq {
                    package: "libcurl4-openssl-dev".to_string(),
                }],
                ..Default::default()
            },
        );
        let mut out = SysReqsCheck::default();
        let packages = vec![PackageSysReqQuery {
            name: "curl".to_string(),
            system_requirements: Some("libxml2 (>= 2.6.3)".to_string()),
            bioc: false,
        }];
        apply_index(&mut out, &packages, Some(&index), "ubuntu-22.04");
        assert_eq!(
            out.local_resolved, 0,
            "curl is in the index, so the local rules must be bypassed entirely"
        );
    }

    #[test]
    fn a_package_absent_from_the_index_is_skipped_not_faulted() {
        // Absence from the index means "declares no system requirements",
        // per `parse_sysreqs_index`'s own contract — not an error, and not
        // a reason to fall back to local rules for that package.
        //
        // `system_requirements` is deliberately set to a string that WOULD
        // resolve locally (the same trigger already verified elsewhere in
        // this file to resolve on ubuntu-22.04) so this test can actually
        // tell "skipped" apart from "silently fell back to local rules" —
        // with `None` here, both behaviours produce identical output
        // (`check_pkg_local` no-ops on `None` regardless), making the
        // assertions vacuous.
        let mut index = HashMap::new();
        index.insert(
            "curl".to_string(),
            SysReqIndexEntry {
                packages: vec![SysReq {
                    package: "libcurl4-openssl-dev".to_string(),
                }],
                ..Default::default()
            },
        );
        let mut out = SysReqsCheck::default();
        let packages = vec![PackageSysReqQuery {
            name: "jsonlite".to_string(),
            system_requirements: Some("libxml2 (>= 2.6.3)".to_string()),
            bioc: false,
        }];
        apply_index(&mut out, &packages, Some(&index), "ubuntu-22.04");
        assert!(
            out.missing.is_empty(),
            "a package absent from the index must not produce a missing entry"
        );
        assert_eq!(
            out.local_resolved, 0,
            "a package absent from the index must not fall back to local rules"
        );
    }

    #[test]
    fn mixed_bioc_and_cran_packages_split_between_index_and_local_rules() {
        // The subtle case the `.filter(|_| !pkg.bioc)` line encodes: in a
        // single sync with a fetched index, the Bioc package must still go
        // to the local rules while the CRAN package alongside it uses the
        // index, even though both apply_index calls share the same `Some`
        // index.
        let mut index = HashMap::new();
        index.insert(
            "curl".to_string(),
            SysReqIndexEntry {
                packages: vec![SysReq {
                    package: "libcurl4-openssl-dev".to_string(),
                }],
                ..Default::default()
            },
        );
        let mut out = SysReqsCheck::default();
        let packages = vec![
            PackageSysReqQuery {
                name: "Rhtslib".to_string(),
                system_requirements: Some("libxml2 (>= 2.6.3)".to_string()),
                bioc: true,
            },
            PackageSysReqQuery {
                name: "curl".to_string(),
                system_requirements: Some("libxml2 (>= 2.6.3)".to_string()),
                bioc: false,
            },
        ];
        apply_index(&mut out, &packages, Some(&index), "ubuntu-22.04");
        assert_eq!(
            out.local_resolved, 1,
            "only the Bioc package (Rhtslib) should have gone through the local rules"
        );
    }

    #[test]
    fn batched_index_is_keyed_by_package_name() {
        // Shape of `?all=true&distribution=ubuntu&release=22.04`, which
        // returns every package that declares sysreqs for that distro
        // (~1125 entries, ~150 KB) in a single response.
        let body = r#"{"requirements":[
            {"name":"ABC.RAP","requirements":{"packages":["make"]}},
            {"name":"curl","requirements":{"packages":["libcurl4-openssl-dev","libssl-dev"]}}
        ]}"#;
        let index = parse_sysreqs_index(body).unwrap();
        assert_eq!(index.len(), 2);
        let curl: Vec<&str> = index["curl"]
            .packages
            .iter()
            .map(|r| r.package.as_str())
            .collect();
        assert_eq!(curl, vec!["libcurl4-openssl-dev", "libssl-dev"]);
    }

    #[test]
    fn packages_absent_from_the_index_have_no_sysreqs() {
        // The API only lists packages that declare sysreqs, so absence is
        // "needs nothing", not an error.
        let body =
            r#"{"requirements":[{"name":"curl","requirements":{"packages":["libssl-dev"]}}]}"#;
        let index = parse_sysreqs_index(body).unwrap();
        assert!(!index.contains_key("jsonlite"));
    }

    #[test]
    fn empty_package_names_are_skipped() {
        let body =
            r#"{"requirements":[{"name":"x","requirements":{"packages":["","libssl-dev"]}}]}"#;
        let index = parse_sysreqs_index(body).unwrap();
        let x: Vec<&str> = index["x"]
            .packages
            .iter()
            .map(|r| r.package.as_str())
            .collect();
        assert_eq!(x, vec!["libssl-dev"]);
    }

    #[test]
    fn batched_index_carries_setup_commands() {
        // Shape returned by the API for `sf` on rockylinux 9.
        let body = r#"{"requirements":[
            {"name":"sf","requirements":{
                "packages":["gdal3.4-devel"],
                "pre_install":[{"command":"dnf install -y epel-release"}]
            }},
            {"name":"rJava","requirements":{
                "packages":["default-jdk"],
                "post_install":[{"command":"R CMD javareconf"}]
            }}
        ]}"#;
        let index = parse_sysreqs_index(body).unwrap();
        assert_eq!(index["sf"].pre_install, vec!["dnf install -y epel-release"]);
        assert_eq!(index["rJava"].post_install, vec!["R CMD javareconf"]);
        assert!(index["sf"].post_install.is_empty());
    }

    #[test]
    fn api_setup_commands_are_collected_only_when_something_is_missing() {
        // Mirrors the local-path rule: an index entry with pre_install
        // commands must not leak them into `out.pre_install` unless
        // `filter_missing` actually reports something missing for that
        // package — otherwise every sync on a covered distro would
        // re-enable EPEL regardless of what's already installed.
        let mut index = HashMap::new();
        index.insert(
            "sf".to_string(),
            SysReqIndexEntry {
                packages: vec![],
                pre_install: vec!["dnf install -y epel-release".to_string()],
                post_install: vec![],
            },
        );
        let mut out = SysReqsCheck::default();
        let packages = vec![PackageSysReqQuery {
            name: "sf".to_string(),
            system_requirements: None,
            bioc: false,
        }];
        apply_index(&mut out, &packages, Some(&index), "rockylinux-9");
        assert!(
            out.pre_install.is_empty(),
            "sf has no packages, so nothing can be missing, so no setup command should be collected"
        );
    }

    #[test]
    fn api_setup_commands_are_deduplicated_across_packages() {
        // The real rockylinux-9 shape: every geospatial package's index entry
        // repeats `dnf install -y epel-release`. `apply_index` merges the
        // entries of all packages in one sync, so without its `contains`
        // guard the user would be shown — and asked to consent to — the same
        // root command once per package, and uvr would run it N times.
        //
        // Both packages must genuinely be missing something, since setup
        // commands are only collected when `filter_missing` reports a gap.
        if which::which("dpkg").is_err()
            && which::which("rpm").is_err()
            && which::which("apk").is_err()
        {
            // Without dpkg/rpm/apk (macOS/Windows dev boxes) `filter_missing`
            // reports nothing missing, so the collection path under test
            // never runs and the assertions below would be vacuous.
            return;
        }
        let entry = |pkg: &str| SysReqIndexEntry {
            packages: vec![SysReq {
                package: pkg.to_string(),
            }],
            pre_install: vec!["dnf install -y epel-release".to_string()],
            post_install: vec!["R CMD javareconf".to_string()],
        };
        let mut index = HashMap::new();
        index.insert(
            "sf".to_string(),
            entry("uvr-test-absent-gdal-devel-4e1d9f2a"),
        );
        index.insert(
            "terra".to_string(),
            entry("uvr-test-absent-geos-devel-4e1d9f2a"),
        );
        let mut out = SysReqsCheck::default();
        let packages = vec![
            PackageSysReqQuery {
                name: "sf".to_string(),
                system_requirements: None,
                bioc: false,
            },
            PackageSysReqQuery {
                name: "terra".to_string(),
                system_requirements: None,
                bioc: false,
            },
        ];
        apply_index(&mut out, &packages, Some(&index), "rockylinux-9");
        assert_eq!(
            out.missing.len(),
            2,
            "precondition: both packages must report a missing system package, \
             otherwise no setup command is collected at all"
        );
        assert_eq!(
            out.pre_install,
            vec!["dnf install -y epel-release".to_string()],
            "the shared pre_install command must appear exactly once"
        );
        assert_eq!(
            out.post_install,
            vec!["R CMD javareconf".to_string()],
            "the shared post_install command must appear exactly once"
        );
    }

    #[test]
    fn malformed_index_body_is_an_error_not_an_empty_map() {
        // A malformed or truncated 200 response must not be silently treated
        // as "no package on this distro needs anything" — that would be
        // indistinguishable from a genuinely empty, well-formed index. This
        // is what routes `fetch_sysreqs_index` to `LookupFailed` instead of
        // `Supported(HashMap::new())` on a broken body.
        assert!(parse_sysreqs_index("{not json").is_err());
    }
}
