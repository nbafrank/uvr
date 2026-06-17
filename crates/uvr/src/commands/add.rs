use anyhow::{Context, Result};

use uvr_core::error::UvrError;
use uvr_core::manifest::{DependencySpec, DetailedDep};
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::registry::bioconductor::{default_release_for_r, BiocRegistry};
use uvr_core::registry::forgejo::parse_forgejo_parts;
use uvr_core::resolver::is_base_package;

use crate::ui;
use crate::ui::palette;

/// Parse `"pkg@>=1.0.0"` or `"user/repo@ref"` into (name, spec).
fn parse_add_spec(raw: &str, bioc: bool) -> Result<(String, DependencySpec)> {
    // Forgejo: explicit `forgejo::host/owner/repo[@ref]` prefix. Checked
    // before the bare `user/repo` heuristic below so a forgejo spec
    // doesn't get misclassified as a malformed GitHub spec.
    if raw.starts_with("forgejo::") {
        let Some(parsed) = parse_forgejo_parts(raw) else {
            anyhow::bail!(
                "Invalid Forgejo spec '{raw}'. Expected: forgejo::host/owner/repo or forgejo::host/owner/repo@ref"
            );
        };
        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(format!(
                "forgejo::{}/{}/{}",
                parsed.host, parsed.owner, parsed.repo
            )),
            rev: parsed.git_ref,
            ..Default::default()
        });
        return Ok((parsed.repo, spec));
    }

    // GitHub: contains '/'
    if raw.contains('/') {
        let (repo, git_ref) = if let Some(at) = raw.rfind('@') {
            (raw[..at].to_string(), Some(raw[at + 1..].to_string()))
        } else {
            (raw.to_string(), None)
        };

        // Validate user/repo format
        let parts: Vec<&str> = repo.split('/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            anyhow::bail!(
                "Invalid GitHub spec '{raw}'. Expected format: user/repo or user/repo@ref"
            );
        }

        let name = parts[1].to_string();

        // Validate package name characters
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
        {
            anyhow::bail!("Invalid package name '{name}' extracted from GitHub spec '{raw}'");
        }

        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(repo),
            rev: git_ref,
            ..Default::default()
        });
        return Ok((name, spec));
    }

    // CRAN/Bioc with optional version: "pkg@>=1.0.0"
    let (name, version) = if let Some(at) = raw.find('@') {
        (raw[..at].to_string(), Some(raw[at + 1..].to_string()))
    } else {
        (raw.to_string(), None)
    };

    // Validate CRAN/Bioc package name
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        anyhow::bail!("Invalid package name '{name}'");
    }

    let spec = if bioc {
        DependencySpec::Detailed(DetailedDep {
            bioc: Some(true),
            version,
            ..Default::default()
        })
    } else {
        match version {
            Some(v) => DependencySpec::Version(v),
            None => DependencySpec::Version("*".to_string()),
        }
    };

    Ok((name, spec))
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    packages: Vec<String>,
    dev: bool,
    bioc: bool,
    source: Option<String>,
    jobs: usize,
    timeout: Option<std::time::Duration>,
    no_lock: bool,
    no_install: bool,
) -> Result<()> {
    let mut project = Project::find_cwd().context("Not inside a uvr project")?;

    // If --source is provided, ensure it's in the manifest's [[sources]]
    if let Some(ref url) = source {
        let url_trimmed = url.trim_end_matches('/');
        let already_exists = project
            .manifest
            .sources
            .iter()
            .any(|s| s.url.trim_end_matches('/') == url_trimmed);
        if !already_exists {
            // Derive a short name from the URL hostname
            let name = url_trimmed
                .strip_prefix("https://")
                .or_else(|| url_trimmed.strip_prefix("http://"))
                .and_then(|s| s.split('/').next())
                .unwrap_or("custom")
                .to_string();
            project
                .manifest
                .sources
                .push(uvr_core::manifest::PackageSource {
                    name: name.clone(),
                    url: url_trimmed.to_string(),
                });
            println!(
                "{} Added source {} {}",
                palette::added(ui::glyph::add()),
                palette::pkg(&name),
                palette::dim(url_trimmed)
            );
        }
    }

    let mut parsed: Vec<(String, DependencySpec)> = packages
        .iter()
        .map(|p| parse_add_spec(p, bioc))
        .collect::<Result<Vec<_>>>()?;

    // For GitHub specs (`user/repo@ref`), the URL-derived basename is only a
    // provisional package name. R's actual package name lives in the
    // remote's DESCRIPTION's `Package:` field — and for some packages
    // those don't match (the `nbafrank/uvr-r` repo ships package `uvr`,
    // see uvr-r #8). Fetch the DESCRIPTION up-front so the manifest entry
    // is keyed by the real package name and matches what the resolver
    // will produce in the lockfile.
    //
    // Skipped under `--no-lock`: that flag's stated semantics are "write
    // uvr.toml only, no network work" — making an HTTP fetch here would
    // violate that contract and break offline scripted workflows. With
    // `--no-lock`, the manifest entry uses the URL-derived basename and
    // the user can edit it later if it diverges from the actual Package.
    if !no_lock {
        resolve_git_pkg_names(&mut parsed).await;
    }

    // Reject base/recommended packages that ship with R — they can't be installed from CRAN.
    for (name, _) in &parsed {
        if is_base_package(name) {
            anyhow::bail!(
                "'{}' is a base R package (ships with R itself) and cannot be installed separately.",
                name
            );
        }
    }

    for (name, spec) in &parsed {
        let is_new = project.manifest.add_dep(name.clone(), spec.clone(), dev);
        if is_new {
            println!(
                "{} {} {}",
                palette::added(ui::glyph::add()),
                palette::pkg(name),
                palette::version(format_spec(spec))
            );
        } else {
            println!(
                "{} {} {} {}",
                palette::upgraded(ui::glyph::change()),
                palette::pkg(name),
                palette::version(format_spec(spec)),
                palette::dim("(updated)"),
            );
        }
    }

    // Save the original manifest so we can roll back on resolution failure
    let manifest_path = project.manifest_path();
    let original_manifest = std::fs::read_to_string(&manifest_path).ok();

    project
        .save_manifest()
        .context("Failed to write uvr.toml")?;

    // #76 — `--no-lock` short-circuits before resolution; useful for
    // building uvr.toml programmatically (e.g. from a script generating
    // multiple `uvr add` calls in a row, then a single explicit
    // `uvr lock` + `uvr sync` at the end). `--no-install` keeps the
    // resolution but skips the install — same use case at a coarser
    // grain. `--no-lock` implies `--no-install` since there's no
    // lockfile to install from.
    if no_lock {
        ui::bullet_dim("Skipped lock + install (--no-lock).");
        return Ok(());
    }

    // Re-resolve → update lockfile (and roll back manifest on failure).
    let resolve_result = crate::commands::lock::resolve_and_lock(&project, false).await;
    if let Err(e) = resolve_result {
        // Roll back the manifest to its original state
        if let Some(original) = original_manifest {
            let _ = std::fs::write(&manifest_path, original);
            ui::warn("Rolled back uvr.toml — resolution failed.");
        }
        // #118: if a just-added package wasn't found, the failure may be a
        // wrong-channel mistake. Probe Bioconductor and, where it helps,
        // replace the (CRAN-flavored) not-found error with channel-aware
        // guidance — suggest `--bioc` for a CRAN miss that's on Bioconductor,
        // or explain a `--bioc` miss that isn't in the current release.
        if let Some(msg) = diagnose_not_found(&project, &parsed, &e).await {
            anyhow::bail!(msg);
        }
        return Err(e).context("Failed to resolve dependencies after add");
    }
    let lockfile = resolve_result.unwrap();

    if no_install {
        ui::bullet_dim("Skipped install (--no-install). Run `uvr sync` to install.");
        return Ok(());
    }

    crate::commands::sync::install_from_lockfile(&project, &lockfile, jobs, None, timeout)
        .await
        .context("Failed to install packages after add")?;

    Ok(())
}

/// Extract the package name from a `PackageNotFound` anywhere in `err`'s chain,
/// if that's what the resolution failed on. Returns `None` for any other error.
fn package_not_found_name(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|c| match c.downcast_ref::<UvrError>() {
            Some(UvrError::PackageNotFound(name)) => Some(name.clone()),
            _ => None,
        })
}

/// Build a channel-aware not-found message from the known facts, or `None` to
/// keep the default error. Pure decision logic (#118). `on_bioc` is the probe
/// result: `Some(true/false)` if Bioconductor was reachable, `None` if the
/// probe couldn't run (offline, CDN down, etc.).
///
/// - Added *without* `--bioc`: only override the default error when the probe
///   positively confirms the package is on Bioconductor — then suggest `--bioc`.
///   Otherwise keep the default CRAN-oriented error (it's the right channel).
/// - Added *with* `--bioc`: never fall back to the default error, because its
///   text hardcodes a CRAN-archive hint that's wrong for a Bioc request. Give a
///   Bioc-flavored message whether or not the probe succeeded.
fn bioc_not_found_message(
    name: &str,
    added_with_bioc: bool,
    on_bioc: Option<bool>,
    release: &str,
) -> Option<String> {
    match (added_with_bioc, on_bioc) {
        // CRAN add that's actually a Bioconductor package — point at `--bioc`.
        (false, Some(true)) => Some(format!(
            "'{name}' isn't on CRAN, but it's available on Bioconductor ({release}).\n  \
             → Install it with: uvr add {name} --bioc"
        )),
        // CRAN add, confirmed not on Bioc or probe unavailable — default error stands.
        (false, _) => None,
        // `--bioc` add, confirmed absent from the release — deprecated/removed.
        (true, Some(false)) => Some(format!(
            "'{name}' isn't in the current Bioconductor release ({release}) — it may have been \
             deprecated or removed. Check https://bioconductor.org/packages/{name}/ for its status."
        )),
        // `--bioc` add, probe unavailable or contradictory (`Some(true)` shouldn't
        // happen — if it were in the index it would have resolved). Either way,
        // don't surface the CRAN-archive hint for a Bioc request.
        (true, _) => Some(format!(
            "'{name}' couldn't be resolved from Bioconductor ({release}). It may not be in this \
             release, or the name may be misspelled — Bioconductor package names are case-sensitive."
        )),
    }
}

/// Pick the Bioconductor release to probe: an explicit `bioc_version` pin if
/// set, otherwise the release paired with the active R version (mirrors what
/// resolution uses). Defaults to the 4.4-era release when R can't be detected.
fn bioc_release_to_probe(project: &Project) -> String {
    if let Some(ref pinned) = project.manifest.project.bioc_version {
        return pinned.clone();
    }
    let r_constraint = project.manifest.project.r_version.as_deref();
    let r_ver = find_r_binary(r_constraint)
        .ok()
        .as_deref()
        .and_then(query_r_version);
    default_release_for_r(r_ver.as_deref().unwrap_or("4.4")).to_string()
}

/// On a resolution failure, if a *directly-added* package wasn't found, probe
/// Bioconductor and return channel-aware guidance (#118). Returns `None` (keep
/// the default error) only when the failure isn't a not-found, or the missing
/// package wasn't one the user just added.
///
/// Limitations: only the *first* `PackageNotFound` in the error chain is
/// diagnosed (a multi-package `uvr add A B` where both are wrong-channel misses
/// gets guidance for one). The Bioconductor probe (`find_r_binary` to pick the
/// release, plus a full index fetch) runs synchronously here — acceptable
/// because this is the already-failed path, not the hot path.
async fn diagnose_not_found(
    project: &Project,
    parsed: &[(String, DependencySpec)],
    err: &anyhow::Error,
) -> Option<String> {
    let name = package_not_found_name(err)?;
    // Only speak up for a package the user added in this command — a missing
    // transitive dep is a different problem and shouldn't get a `--bioc` nudge.
    let added_with_bioc = parsed
        .iter()
        .find(|(n, _)| n == &name)
        .map(|(_, spec)| spec.is_bioc())?;
    let release = bioc_release_to_probe(project);
    // Probe is best-effort: `None` means "couldn't check". For a `--bioc` add
    // we still return a Bioc-flavored message in that case, so the misleading
    // CRAN-archive hint never reaches the user even offline.
    let on_bioc = match crate::commands::util::build_client() {
        Ok(client) => BiocRegistry::fetch_release(&client, &release)
            .await
            .ok()
            .map(|bioc| bioc.contains(&name)),
        Err(_) => None,
    };
    bioc_not_found_message(&name, added_with_bioc, on_bioc, &release)
}

/// For each git-sourced dep (github or forgejo) in `parsed`, fetch the remote
/// DESCRIPTION and replace the URL-derived name with the actual `Package:`
/// field (uvr-r #8). Mutates in place. Best-effort — every failure path
/// (transport error, missing DESCRIPTION, malformed file) is logged
/// internally and the URL-derived name is kept. If *every* git spec
/// in the batch fails, surface a single user-facing warn so an offline
/// user knows manifest names may need a manual touch-up. Returns no
/// error: callers don't need to handle one.
async fn resolve_git_pkg_names(parsed: &mut [(String, DependencySpec)]) {
    use uvr_core::registry::github::parse_github_spec;

    let needs_resolve: Vec<usize> = parsed
        .iter()
        .enumerate()
        .filter_map(|(i, (_, spec))| match spec {
            DependencySpec::Detailed(d) if d.git.is_some() => Some(i),
            _ => None,
        })
        .collect();
    if needs_resolve.is_empty() {
        return;
    }

    let client = match crate::commands::util::build_client() {
        Ok(c) => c,
        Err(e) => {
            ui::warn(format!(
                "Could not build HTTP client for DESCRIPTION lookup ({e}); using repo basenames as package names. Edit uvr.toml manually if names differ."
            ));
            return;
        }
    };
    let total = needs_resolve.len();
    let mut fetch_failures = 0usize;
    for idx in needs_resolve {
        let (provisional_name, spec) = &parsed[idx];
        let DependencySpec::Detailed(d) = spec else {
            continue;
        };
        let Some(git) = d.git.as_deref() else {
            continue;
        };
        let git_ref_owned = d.rev.as_deref().unwrap_or("HEAD").to_string();

        // Build the raw-DESCRIPTION URL appropriate for the registry, and
        // attach an appropriate token if one is in the environment.
        let (desc_url, auth_header) = if let Some(body) = git.strip_prefix("forgejo::") {
            let parts: Vec<&str> = body.split('/').collect();
            if parts.len() != 3 || parts.iter().any(|s| s.is_empty()) {
                continue;
            }
            let host = parts[0];
            let url = format!(
                "https://{host}/api/v1/repos/{owner}/{repo}/raw/DESCRIPTION?ref={r}",
                owner = parts[1],
                repo = parts[2],
                r = git_ref_owned,
            );
            let auth =
                uvr_core::registry::forgejo::forgejo_token(host).map(|t| format!("token {t}"));
            (url, auth)
        } else {
            // github: `user/repo`
            let spec_str = format!("{git}@{git_ref_owned}");
            let Some((user, repo, resolved_ref)) = parse_github_spec(&spec_str) else {
                continue;
            };
            let url = format!(
                "https://raw.githubusercontent.com/{user}/{repo}/{resolved_ref}/DESCRIPTION"
            );
            // #95: attach a GitHub token when available so CI runners
            // walking renv.lock imports don't hit the 60 req/hr shared
            // unauthenticated rate limit.
            let auth = {
                let mut found: Option<String> = None;
                for var in ["GITHUB_PAT", "GITHUB_TOKEN"] {
                    if let Ok(v) = std::env::var(var) {
                        let t = v.trim().to_string();
                        if !t.is_empty() {
                            found = Some(format!("Bearer {t}"));
                            break;
                        }
                    }
                }
                found
            };
            (url, auth)
        };

        let mut req = client
            .get(&desc_url)
            .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));
        if let Some(auth) = auth_header {
            req = req.header("Authorization", auth);
        }

        match req.send().await.and_then(|r| r.error_for_status()) {
            Ok(resp) => {
                let text = resp.text().await.unwrap_or_default();
                let fields = uvr_core::dcf::parse_dcf_fields(&text);
                if let Some(actual) = fields.get("Package") {
                    let actual = actual.trim().to_string();
                    if !actual.is_empty() && actual != *provisional_name {
                        ui::bullet_dim(format!(
                            "{} → {} (Package: field in DESCRIPTION)",
                            palette::dim(provisional_name),
                            palette::pkg(&actual)
                        ));
                        parsed[idx].0 = actual;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "DESCRIPTION fetch failed for {git}@{git_ref_owned}: {e}; using {provisional_name} as the package name"
                );
                fetch_failures += 1;
            }
        }
    }
    // Surface a single user-facing warn when the network was completely
    // unreachable (offline / behind a proxy). Per-spec failures already
    // logged via tracing::warn — surface to user only when 100% failed.
    if fetch_failures == total {
        ui::warn(
            "Could not reach git host to look up DESCRIPTION fields; package names default to repo basenames. Edit uvr.toml if a name differs.",
        );
    }
}

fn format_spec(spec: &DependencySpec) -> String {
    match spec {
        DependencySpec::Version(v) => v.clone(),
        DependencySpec::Detailed(d) => {
            if let Some(git) = &d.git {
                let rev = d.rev.as_deref().unwrap_or("HEAD");
                format!("{git}@{rev}")
            } else if d.bioc.unwrap_or(false) {
                "[bioc]".to_string()
            } else {
                d.version.as_deref().unwrap_or("*").to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cran() {
        let (name, spec) = parse_add_spec("ggplot2@>=3.0.0", false).unwrap();
        assert_eq!(name, "ggplot2");
        assert!(matches!(spec, DependencySpec::Version(v) if v == ">=3.0.0"));
    }

    #[test]
    fn parse_github() {
        let (name, spec) = parse_add_spec("tidyverse/ggplot2@main", false).unwrap();
        assert_eq!(name, "ggplot2");
        assert!(spec.git().is_some());
    }

    #[test]
    fn parse_bioc() {
        let (name, spec) = parse_add_spec("DESeq2", true).unwrap();
        assert_eq!(name, "DESeq2");
        assert!(spec.is_bioc());
    }

    #[test]
    fn parse_invalid_github() {
        assert!(parse_add_spec("/", false).is_err());
        assert!(parse_add_spec("a//b", false).is_err());
        assert!(parse_add_spec("user/repo/extra", false).is_err());
    }

    #[test]
    fn parse_empty_name() {
        assert!(parse_add_spec("", false).is_err());
    }

    #[test]
    fn parse_forgejo_spec_cli() {
        let (name, spec) = parse_add_spec("forgejo::codefloe.com/pat-s/mypkg@main", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.git.as_deref(), Some("forgejo::codefloe.com/pat-s/mypkg"));
                assert_eq!(d.rev.as_deref(), Some("main"));
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_forgejo_spec_cli_no_ref() {
        let (name, spec) = parse_add_spec("forgejo::codefloe.com/pat-s/mypkg", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.rev, None);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_forgejo_spec_cli_rejects_bad_shape() {
        assert!(parse_add_spec("forgejo::codefloe.com/onlyone", false).is_err());
        assert!(parse_add_spec("forgejo::/pat-s/mypkg", false).is_err());
        assert!(parse_add_spec("forgejo::codefloe.com//mypkg", false).is_err());
    }

    #[test]
    fn package_not_found_name_extracts_from_chain() {
        // PackageNotFound wrapped in context (as resolve_and_lock produces it).
        let err = anyhow::Error::new(UvrError::PackageNotFound("DESeq2".into()))
            .context("Dependency resolution failed");
        assert_eq!(package_not_found_name(&err).as_deref(), Some("DESeq2"));
    }

    #[test]
    fn package_not_found_name_ignores_other_errors() {
        let err = anyhow::anyhow!("some unrelated failure").context("Dependency resolution failed");
        assert_eq!(package_not_found_name(&err), None);
        // A different UvrError variant is also not a not-found.
        let other = anyhow::Error::new(UvrError::NoMatchingVersion {
            package: "x".into(),
            constraint: ">=2".into(),
        });
        assert_eq!(package_not_found_name(&other), None);
    }

    #[test]
    fn bioc_message_suggests_bioc_for_cran_miss_on_bioc() {
        // #118: added without --bioc, but the probe confirms it IS on Bioconductor.
        let msg = bioc_not_found_message("DESeq2", false, Some(true), "3.20").unwrap();
        assert!(msg.contains("--bioc"), "should suggest the flag: {msg}");
        assert!(msg.contains("DESeq2") && msg.contains("3.20"));
    }

    #[test]
    fn bioc_message_explains_bioc_miss_without_cran_hint() {
        // The reported bug: added WITH --bioc, probe confirms not in the release.
        let msg = bioc_not_found_message("ImmuneSpaceR", true, Some(false), "3.20").unwrap();
        assert!(msg.contains("current Bioconductor release"));
        assert!(msg.contains("3.20"));
        // Must NOT push the misleading CRAN-archived advice.
        assert!(
            !msg.contains("--bioc"),
            "no flag suggestion for a bioc miss: {msg}"
        );
        assert!(!msg.to_lowercase().contains("cran"));
    }

    #[test]
    fn bioc_message_cran_miss_keeps_default_when_not_on_bioc_or_unknown() {
        // CRAN add: only override when the probe positively finds it on Bioc.
        assert_eq!(
            bioc_not_found_message("x", false, Some(false), "3.20"),
            None
        );
        assert_eq!(bioc_not_found_message("x", false, None, "3.20"), None);
    }

    #[test]
    fn release_to_probe_prefers_pinned_bioc_version() {
        // Pinned bioc_version wins with no R detection / network (fast path).
        use uvr_core::manifest::Manifest;
        use uvr_core::project::{ManifestSource, Project};
        let mut manifest = Manifest::new("t", Some(">=4.4.0".into()));
        manifest.project.bioc_version = Some("3.18".into());
        let project = Project {
            root: std::path::PathBuf::from("/tmp/does-not-matter"),
            manifest,
            manifest_source: ManifestSource::Toml,
        };
        assert_eq!(bioc_release_to_probe(&project), "3.18");
    }

    #[test]
    fn bioc_message_bioc_add_never_shows_cran_hint_even_offline() {
        // Finding #4: a --bioc add must never fall through to the CRAN-archive
        // hint, including when the probe couldn't run (None) or contradicts.
        for probe in [None, Some(true)] {
            let msg = bioc_not_found_message("ImmuneSpaceR", true, probe, "3.20")
                .expect("a --bioc miss must always produce a message");
            assert!(!msg.contains("--bioc"));
            assert!(
                !msg.to_lowercase().contains("cran"),
                "leaked CRAN hint: {msg}"
            );
            assert!(msg.contains("Bioconductor") && msg.contains("3.20"));
        }
    }
}
