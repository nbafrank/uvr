use anyhow::{Context, Result};

use uvr_core::error::UvrError;
use uvr_core::manifest::{DependencySpec, DetailedDep};
use uvr_core::package_name;
use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, query_r_version};
use uvr_core::registry::bioconductor::{default_release_for_r, BiocRegistry};
use uvr_core::registry::forgejo::parse_forgejo_parts;
use uvr_core::registry::git_generic::parse_git_parts;
use uvr_core::registry::gitlab::parse_gitlab_parts;
use uvr_core::resolver::is_base_package;

use crate::ui;
use crate::ui::palette;

fn split_subdirectory_fragment(raw: &str) -> Result<(&str, Option<&str>)> {
    let Some((base, fragment)) = raw.split_once('#') else {
        return Ok((raw, None));
    };
    let Some(path) = fragment.strip_prefix("subdirectory=") else {
        anyhow::bail!(
            "Unsupported fragment '#{fragment}' in '{raw}'. Expected: \
             owner/repo[@revision]#subdirectory=path"
        );
    };
    if let Err(e) = uvr_core::subdirectory::validate(path) {
        anyhow::bail!("{e} (in '{raw}')");
    }
    Ok((base, Some(path)))
}

/// Parse `"pkg@>=1.0.0"`, `"user/repo@ref"`, or `"user/repo@ref#subdirectory=path"` into (name, spec).
fn parse_add_spec(raw: &str, bioc: bool) -> Result<(String, DependencySpec)> {
    // Any git host: `git::<clone URL>[@ref]` (#190). The URL can contain `/`,
    // so this comes before the GitHub heuristic too. The name is the
    // repository name until the DESCRIPTION lookup replaces it.
    if raw.starts_with("git::") {
        if raw.contains("#subdirectory=") {
            anyhow::bail!(
                "`#subdirectory=` is not supported for git:: sources yet (in '{}').",
                uvr_core::auth::redact_url(raw)
            );
        }
        let parsed = parse_git_parts(raw).map_err(|reason| {
            anyhow::anyhow!(
                "Invalid git spec '{}': {reason}. Expected: git::<clone URL>[@ref], with an \
                 https://, ssh:// or user@host:path URL",
                uvr_core::auth::redact_url(raw)
            )
        })?;
        let name = uvr_core::registry::git_generic::repo_name(&parsed.url).to_string();
        if !package_name::is_valid(&name) {
            anyhow::bail!("Invalid package name '{name}' extracted from git spec '{raw}'");
        }
        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(format!("git::{}", parsed.url)),
            rev: parsed.git_ref,
            ..Default::default()
        });
        return Ok((name, spec));
    }

    // Direct source tarball (#189). Checked first: a URL contains '/' and
    // would otherwise be reported as a malformed GitHub spec.
    if raw.starts_with("https://") || raw.starts_with("http://") {
        if !uvr_core::registry::url::is_source_tarball_url(raw) {
            anyhow::bail!(
                "Unsupported URL '{raw}'. Only direct source tarball URLs ending in .tar.gz or \
                 .tgz are supported. For a git repository use user/repo[@ref], \
                 forgejo::host/owner/repo[@ref], or gitlab::host/group/project[@ref]."
            );
        }
        let spec = DependencySpec::Detailed(DetailedDep {
            url: Some(raw.to_string()),
            ..Default::default()
        });
        return Ok((url_name_hint(raw), spec));
    }

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

    // GitLab: explicit `gitlab::host/group[/subgroup...]/project[@ref]`
    // prefix. Checked before the bare `user/repo` heuristic below for the
    // same reason as Forgejo above. GitLab projects can live under nested
    // groups, so the parsed spec carries a full namespace path rather than
    // a single owner segment.
    if raw.starts_with("gitlab::") {
        let Some(parsed) = parse_gitlab_parts(raw) else {
            anyhow::bail!(
                "Invalid GitLab spec '{raw}'. Expected: gitlab::host/group/project or gitlab::host/group/subgroup/project[@ref]"
            );
        };
        let name = parsed.project_name().to_string();
        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(format!("gitlab::{}/{}", parsed.host, parsed.project_path)),
            rev: parsed.git_ref,
            ..Default::default()
        });
        return Ok((name, spec));
    }

    // GitHub: contains '/'
    if raw.contains('/') {
        let (base, subdirectory) = split_subdirectory_fragment(raw)?;
        let (repo, git_ref) = if let Some(at) = base.rfind('@') {
            (base[..at].to_string(), Some(base[at + 1..].to_string()))
        } else {
            (base.to_string(), None)
        };

        // Validate user/repo format
        let parts: Vec<&str> = repo.split('/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            // A first segment containing a dot looks like a hostname
            // (e.g. `gitlab.com/user/repo`, `git.local:3000/u/r`): the real
            // problem is a missing explicit prefix, not a malformed GitHub
            // spec — say so instead of the misleading "Invalid GitHub spec"
            // (#145). Forgejo and GitLab hosts can't be auto-detected from
            // a bare `host/owner/repo` shape, so both require their `::`
            // prefix.
            if parts[0].contains('.') {
                anyhow::bail!(
                    "Unsupported git host '{host}' in '{raw}'. Supported specs: \
                     GitHub via user/repo[@ref], Forgejo via forgejo::host/owner/repo[@ref], \
                     GitLab via gitlab::host/group/project[@ref], any git host via \
                     git::https://host/path/repo.git[@ref].",
                    host = parts[0],
                );
            }
            anyhow::bail!(
                "Invalid GitHub spec '{raw}'. Expected format: user/repo or user/repo@ref"
            );
        }

        let name = match subdirectory {
            Some(sub) => sub.rsplit('/').next().unwrap_or(sub).to_string(),
            None => parts[1].to_string(),
        };

        // Validate package name characters
        if subdirectory.is_none() && !package_name::is_valid(&name) {
            anyhow::bail!("Invalid package name '{name}' extracted from GitHub spec '{raw}'");
        }

        if subdirectory.is_some() {
            let full = match &git_ref {
                Some(r) => format!("{repo}@{r}"),
                None => repo.clone(),
            };
            if !uvr_core::registry::github::is_valid_github_repo_spec(&full) {
                anyhow::bail!(
                    "Invalid GitHub spec '{raw}'. Expected format: \
                     owner/repo[@revision]#subdirectory=path"
                );
            }
        }

        let spec = DependencySpec::Detailed(DetailedDep {
            git: Some(repo),
            rev: git_ref,
            subdirectory: subdirectory.map(str::to_string),
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
    if !package_name::is_valid(&name) {
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

/// `pkg` from `…/pkg_1.2.0.tar.gz`. A placeholder only: the tarball's
/// DESCRIPTION names the package, unless `--no-lock` skips the download.
fn url_name_hint(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file
        .strip_suffix(".tar.gz")
        .or_else(|| file.strip_suffix(".tgz"))
        .unwrap_or(file);
    stem.split('_').next().unwrap_or(stem).to_string()
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
        // uvr.toml names a repository; its secret lives in the environment.
        if uvr_core::auth::has_userinfo(url) {
            let host = url.split('/').nth(2).unwrap_or_default();
            let key = uvr_core::auth::env_key(host.rsplit('@').next().unwrap_or_default());
            anyhow::bail!(
                "--source {} has credentials in the URL, which would be saved to uvr.toml. \
                 Pass the URL without them, and set UVR_REPO_TOKEN_{key}, or \
                 UVR_REPO_USER_{key} and UVR_REPO_PASSWORD_{key}.",
                uvr_core::auth::redact_url(url)
            );
        }
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

    for url in parsed.iter().filter_map(|(_, spec)| spec.url()) {
        if url.starts_with("http://") {
            ui::warn(format!(
                "{url} uses plain http. uvr.lock pins its sha256, but the first download is \
                 not protected in transit."
            ));
        }
    }

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
    // A URL tarball (#189) gets the same treatment, except that its file
    // name must already be a valid package name under `--no-lock`.
    if !no_lock {
        resolve_git_pkg_names(&mut parsed).await?;
        resolve_url_pkg_names(&mut parsed).await?;
    } else if let Some((name, spec)) = parsed
        .iter()
        .find(|(name, spec)| spec.url().is_some() && !package_name::is_valid(name))
    {
        anyhow::bail!(
            "Cannot tell the package name of {} from its file name ('{name}') without \
             downloading it. Run without --no-lock, or add it to uvr.toml by hand as \
             `<name> = {{ url = \"...\" }}`.",
            spec.url().unwrap_or_default()
        );
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
    let mut resolve_result = crate::commands::lock::resolve_and_lock(&project, false).await;

    // A CRAN add that failed only because the package lives on Bioconductor
    // is a question uvr can already answer — so answer it instead of asking
    // the user to retype the command with `--bioc`. This matters most from
    // `uvr::add()` inside an R session, where "run `uvr add sva --bioc`" is
    // advice the caller can't act on without leaving R.
    if let (Err(ref e), false) = (&resolve_result, bioc) {
        if let Some(missing) = package_not_found_name(e) {
            let original_spec = parsed
                .iter()
                .find(|(n, _)| n == &missing)
                .map(|(_, spec)| spec);
            if let Some(spec) = original_spec.filter(|s| s.git().is_none()) {
                if probe_bioc(&project, &missing).await == Some(true) {
                    let release = bioc_release_to_probe(&project);
                    ui::bullet_dim(format!(
                        "'{missing}' isn't on CRAN — adding it from Bioconductor {release}."
                    ));
                    project
                        .manifest
                        .add_dep(missing.clone(), bioc_fallback_spec(spec), dev);
                    project
                        .save_manifest()
                        .context("Failed to write uvr.toml")?;
                    resolve_result = crate::commands::lock::resolve_and_lock(&project, false).await;
                }
            }
        }
    }

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
/// Build the manifest spec used when a CRAN-missing package is re-added from
/// Bioconductor: the switch is CRAN→Bioconductor, not constrained→
/// unconstrained, so the user's version constraint (if any) carries over.
fn bioc_fallback_spec(original: &DependencySpec) -> DependencySpec {
    DependencySpec::Detailed(DetailedDep {
        version: original
            .version_req()
            .filter(|v| *v != "*")
            .map(str::to_string),
        bioc: Some(true),
        ..Default::default()
    })
}

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
    let on_bioc = probe_bioc(project, &name).await;
    bioc_not_found_message(&name, added_with_bioc, on_bioc, &release)
}

/// Is `name` in the Bioconductor release this project resolves against?
///
/// Best-effort: `None` means the probe couldn't run (offline, CDN down), which
/// callers must treat as "unknown", never as "no" — `diagnose_not_found` still
/// emits Bioc-flavored guidance for a `--bioc` add in that case so the
/// misleading CRAN-archive hint never reaches the user.
async fn probe_bioc(project: &Project, name: &str) -> Option<bool> {
    let release = bioc_release_to_probe(project);
    let client = crate::commands::util::build_client().ok()?;
    BiocRegistry::fetch_release(&client, &release)
        .await
        .ok()
        .map(|bioc| bioc.contains(name))
}

/// For each git-sourced dep (github, forgejo, gitlab, or git::) in `parsed`, fetch the remote
/// DESCRIPTION and replace the URL-derived name with the actual `Package:`
/// field (uvr-r #8). Mutates in place. Best-effort — every failure path
/// (transport error, missing DESCRIPTION, malformed file) is logged
/// internally and the URL-derived name is kept. If *every* git spec
/// in the batch fails, surface a single user-facing warn so an offline
/// user knows manifest names may need a manual touch-up. A `subdirectory`
/// spec is the exception: a failed lookup errors instead.
async fn resolve_git_pkg_names(parsed: &mut [(String, DependencySpec)]) -> Result<()> {
    use uvr_core::auth::GitHost;
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
        return Ok(());
    }
    let has_nested = needs_resolve
        .iter()
        .any(|&i| parsed[i].1.subdirectory().is_some());

    let client = match crate::commands::util::build_client() {
        Ok(c) => c,
        Err(e) => {
            if has_nested {
                anyhow::bail!("Could not build HTTP client for DESCRIPTION lookup: {e}");
            }
            ui::warn(format!(
                "Could not build HTTP client for DESCRIPTION lookup ({e}); using repo basenames as package names. Edit uvr.toml manually if names differ."
            ));
            return Ok(());
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
        let subdirectory = d.subdirectory.clone();

        // `git::` (#190): the DESCRIPTION of the fetched commit. The fetch
        // goes into the download cache, where `uvr lock` finds it.
        if let Some(url) = git.strip_prefix("git::") {
            use uvr_core::registry::git_generic;
            let found = async {
                let commit = git_generic::fetch_commit_sha(url, &git_ref_owned).await?;
                let cache_dir = uvr_core::env_vars::cache_dir_or_temp();
                git_generic::resolve_git_package_at_commit_bound(&cache_dir, url, &commit, true)
                    .await
            }
            .await;
            match found {
                Ok((info, _, _)) if info.name != *provisional_name => {
                    ui::bullet_dim(format!(
                        "{} → {} (Package: field in DESCRIPTION)",
                        palette::dim(provisional_name),
                        palette::pkg(&info.name)
                    ));
                    parsed[idx].0 = info.name;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        "DESCRIPTION lookup failed for {git}@{git_ref_owned}: {e}; using {provisional_name} as the package name"
                    );
                    fetch_failures += 1;
                }
            }
            continue;
        }

        // Build the raw-DESCRIPTION URL appropriate for the registry. The
        // host's token, if any, goes with it (#187).
        let (desc_url, host) = if let Some(body) = git.strip_prefix("forgejo::") {
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
            (url, GitHost::Forgejo(host))
        } else if let Some(body) = git.strip_prefix("gitlab::") {
            let parts: Vec<&str> = body.split('/').collect();
            if parts.len() < 3 || parts.iter().any(|s| s.is_empty()) {
                continue;
            }
            let host = parts[0];
            let project_id = urlencoding::encode(&parts[1..].join("/")).into_owned();
            let url = format!(
                "https://{host}/api/v4/projects/{project_id}/repository/files/DESCRIPTION/raw?ref={r}",
                r = git_ref_owned,
            );
            (url, GitHost::GitLab(host))
        } else {
            // github: `user/repo`
            let spec_str = format!("{git}@{git_ref_owned}");
            let Some((user, repo, resolved_ref)) = parse_github_spec(&spec_str) else {
                continue;
            };
            let url = match subdirectory.as_deref() {
                Some(sub) => format!(
                    "https://raw.githubusercontent.com/{user}/{repo}/{resolved_ref}/{}/DESCRIPTION",
                    uvr_core::subdirectory::encode_segments(sub)
                ),
                None => format!(
                    "https://raw.githubusercontent.com/{user}/{repo}/{resolved_ref}/DESCRIPTION"
                ),
            };
            // #95: a GitHub token also keeps CI runners that walk renv.lock
            // imports under the 60 req/hr shared unauthenticated rate limit.
            (url, GitHost::GitHub)
        };

        let req = client
            .get(&desc_url)
            .header("User-Agent", concat!("uvr/", env!("CARGO_PKG_VERSION")));

        match host.send(req).await.and_then(|r| r.error_for_status()) {
            Ok(resp) => {
                let text = resp.text().await.unwrap_or_default();
                let fields = uvr_core::dcf::parse_dcf_fields(&text);
                let actual = fields
                    .get("Package")
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty());
                match (actual, subdirectory.as_deref()) {
                    (None, Some(sub)) => anyhow::bail!(
                        "DESCRIPTION at {git}@{git_ref_owned} in '{sub}' has no `Package:` field; \
                         cannot identify the package at that subdirectory."
                    ),
                    (Some(actual), sub) => {
                        if let Some(sub) = sub {
                            if !package_name::is_valid(&actual) {
                                anyhow::bail!(
                                    "DESCRIPTION at {git}@{git_ref_owned} in '{sub}' declares an \
                                     invalid `Package:` name '{actual}'."
                                );
                            }
                        }
                        if actual != *provisional_name {
                            ui::bullet_dim(format!(
                                "{} → {} (Package: field in DESCRIPTION)",
                                palette::dim(provisional_name),
                                palette::pkg(&actual)
                            ));
                            parsed[idx].0 = actual;
                        }
                    }
                    (None, None) => {}
                }
            }
            Err(e) => {
                if let Some(sub) = subdirectory.as_deref() {
                    anyhow::bail!(
                        "Failed to fetch DESCRIPTION for {git}@{git_ref_owned} in '{sub}': {e}"
                    );
                }
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
    Ok(())
}

/// Download each URL spec (#189), reject anything that is not an R source
/// tarball, and key it by its DESCRIPTION `Package:`. Unlike the git lookup
/// above this is not best-effort: the download is the validation.
async fn resolve_url_pkg_names(parsed: &mut [(String, DependencySpec)]) -> Result<()> {
    if parsed.iter().all(|(_, spec)| spec.url().is_none()) {
        return Ok(());
    }
    let client = crate::commands::util::build_client()?;
    for (name, spec) in parsed.iter_mut() {
        let Some(url) = spec.url() else {
            continue;
        };
        let (info, _, _) = uvr_core::registry::url::resolve_url_package(&client, url).await?;
        if info.name != *name {
            ui::bullet_dim(format!(
                "{} → {} (Package: field in DESCRIPTION)",
                palette::dim(&*name),
                palette::pkg(&info.name)
            ));
            *name = info.name;
        }
    }
    Ok(())
}

fn format_spec(spec: &DependencySpec) -> String {
    match spec {
        DependencySpec::Version(v) => v.clone(),
        DependencySpec::Detailed(d) => {
            if let Some(url) = &d.url {
                url.clone()
            } else if let Some(git) = &d.git {
                let rev = d.rev.as_deref().unwrap_or("HEAD");
                match d.subdirectory.as_deref() {
                    Some(sub) => format!("{git}@{rev}#subdirectory={sub}"),
                    None => format!("{git}@{rev}"),
                }
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
    fn parse_github_subdirectory_fragment() {
        let (name, spec) =
            parse_add_spec("owner/repo@v1.0#subdirectory=pkgs/nested", false).unwrap();
        assert_eq!(name, "nested");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.git.as_deref(), Some("owner/repo"));
                assert_eq!(d.rev.as_deref(), Some("v1.0"));
                assert_eq!(d.subdirectory.as_deref(), Some("pkgs/nested"));
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_github_subdirectory_without_revision() {
        let (name, spec) = parse_add_spec("owner/repo#subdirectory=nested", false).unwrap();
        assert_eq!(name, "nested");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.git.as_deref(), Some("owner/repo"));
                assert_eq!(d.rev, None);
                assert_eq!(d.subdirectory.as_deref(), Some("nested"));
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_github_root_spec_has_no_subdirectory() {
        let (_, spec) = parse_add_spec("tidyverse/ggplot2@main", false).unwrap();
        assert_eq!(spec.subdirectory(), None);
        assert_eq!(format_spec(&spec), "tidyverse/ggplot2@main");
    }

    #[test]
    fn format_spec_shows_the_package_directory() {
        let (_, spec) = parse_add_spec("owner/repo@v1.0#subdirectory=pkgs/nested", false).unwrap();
        assert_eq!(
            format_spec(&spec),
            "owner/repo@v1.0#subdirectory=pkgs/nested"
        );
    }

    #[test]
    fn parse_accepts_safe_nested_paths_that_are_not_package_names() {
        for (raw, sub) in [
            ("owner/repo@main#subdirectory=pkgs/my pkg", "pkgs/my pkg"),
            ("owner/repo#subdirectory=a+b", "a+b"),
        ] {
            let (name, spec) = parse_add_spec(raw, false).unwrap_or_else(|e| {
                panic!("should accept {raw}: {e}");
            });
            assert_eq!(spec.subdirectory(), Some(sub));
            assert_eq!(name, sub.rsplit('/').next().unwrap());
            assert_eq!(spec.git(), Some("owner/repo"));
        }
    }

    #[test]
    fn parse_gates_root_and_cran_names_on_the_shared_charset() {
        for ok in ["data.table", "owner/my-pkg_1"] {
            assert!(parse_add_spec(ok, false).is_ok(), "should accept {ok}");
        }
        for bad in [
            "bad+name",
            "my pkg",
            "..",
            "owner/bad+name",
            "owner/my pkg",
            "owner/..",
        ] {
            assert!(parse_add_spec(bad, false).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn parse_rejects_bad_subdirectory_fragments() {
        for bad in [
            "owner/repo#subdirectory=",
            "owner/repo#subdirectory=../escape",
            "owner/repo#subdirectory=/abs",
            "owner/repo#subdirectory=a//b",
            "owner/repo#subdirectory=C:/pkg",
            "owner/repo#subdir=nested",
            "owner/repo#pull/12",
            "owner/repo@#subdirectory=nested",
            "owner/@main#subdirectory=nested",
            "/repo@main#subdirectory=nested",
        ] {
            assert!(parse_add_spec(bad, false).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn parse_rejects_subdirectory_on_non_github_hosts() {
        for bad in [
            "gitlab::gitlab.com/g/p#subdirectory=nested",
            "forgejo::codefloe.com/o/r#subdirectory=nested",
            "git::https://git.corp.example/team/repo.git#subdirectory=nested",
            "nested#subdirectory=nested",
        ] {
            assert!(parse_add_spec(bad, false).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn parse_bioc() {
        let (name, spec) = parse_add_spec("DESeq2", true).unwrap();
        assert_eq!(name, "DESeq2");
        assert!(spec.is_bioc());
    }

    #[test]
    fn bioc_fallback_preserves_version_constraint() {
        let (_, spec) = parse_add_spec("sva@>=3.50.0", false).unwrap();
        let fallback = bioc_fallback_spec(&spec);
        assert!(fallback.is_bioc());
        assert_eq!(fallback.version_req(), Some(">=3.50.0"));
    }

    #[test]
    fn bioc_fallback_leaves_bare_add_unconstrained() {
        let (_, spec) = parse_add_spec("sva", false).unwrap();
        let fallback = bioc_fallback_spec(&spec);
        assert!(fallback.is_bioc());
        // A bare add is `Version("*")`; the fallback must not serialize a
        // literal `version = "*"` into uvr.toml.
        assert_eq!(fallback.version_req(), None);
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
    fn parse_host_looking_spec_gets_unsupported_host_error() {
        // #145: a bare host prefix must not produce the misleading
        // "Invalid GitHub spec" error.
        let err = parse_add_spec("gitlab.com/user/repo", false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unsupported git host 'gitlab.com'"),
            "unexpected message: {msg}"
        );
        assert!(msg.contains("user/repo"), "should list GitHub form: {msg}");
        assert!(msg.contains("forgejo::"), "should list Forgejo form: {msg}");
        assert!(msg.contains("gitlab::"), "should list GitLab form: {msg}");
        assert!(
            msg.contains("git::https://"),
            "should list the git:: form: {msg}"
        );
        assert!(!msg.contains("Invalid GitHub spec"), "misleading: {msg}");
    }

    #[test]
    fn parse_source_tarball_urls() {
        for (raw, hint) in [
            ("https://example.org/tpkg_1.2.0.tar.gz", "tpkg"),
            ("http://127.0.0.1:8080/dl/data.pkg_0.1-2.tgz", "data.pkg"),
            ("https://example.org/a/b/tpkg.tar.gz?token=x", "tpkg"),
        ] {
            let (name, spec) = parse_add_spec(raw, false).unwrap();
            assert_eq!(name, hint, "{raw}");
            assert_eq!(spec.url(), Some(raw));
            assert_eq!(spec.git(), None);
            assert_eq!(format_spec(&spec), raw);
        }
    }

    #[test]
    fn parse_rejects_urls_that_are_not_source_tarballs() {
        for bad in [
            "https://example.org/index.html",
            "https://github.com/user/repo",
            "https://example.org/tpkg_1.2.0.zip",
            "http://",
        ] {
            let msg = parse_add_spec(bad, false).unwrap_err().to_string();
            assert!(msg.contains("Unsupported URL"), "{bad}: {msg}");
            assert!(msg.contains(".tar.gz"), "{bad}: {msg}");
            assert!(!msg.contains("Invalid GitHub spec"), "{bad}: {msg}");
        }
    }

    #[test]
    fn url_sniffing_leaves_other_specs_alone() {
        // `forgejo::`/`gitlab::` specs and bare host paths keep their own
        // parsers and messages; only an http(s) scheme selects the URL path.
        let (_, spec) = parse_add_spec("forgejo::codefloe.com/pat-s/mypkg", false).unwrap();
        assert_eq!(spec.url(), None);
        let (_, spec) = parse_add_spec("gitlab::gitlab.com/g/mypkg", false).unwrap();
        assert_eq!(spec.url(), None);
        let (_, spec) = parse_add_spec("owner/repo.tar.gz", false).unwrap();
        assert_eq!(spec.url(), None);
        let msg = parse_add_spec("example.org/pkg_1.0.tar.gz/x", false)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("Unsupported git host"), "{msg}");
    }

    #[test]
    fn parse_non_host_bad_spec_keeps_github_error() {
        // No dot in the first segment → still the plain GitHub-spec error.
        let msg = parse_add_spec("user/repo/extra", false)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("Invalid GitHub spec"), "unexpected: {msg}");
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
    fn parse_gitlab_spec_cli() {
        let (name, spec) = parse_add_spec("gitlab::gitlab.com/my-group/mypkg@main", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.git.as_deref(), Some("gitlab::gitlab.com/my-group/mypkg"));
                assert_eq!(d.rev.as_deref(), Some("main"));
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_gitlab_spec_cli_nested_subgroup() {
        let (name, spec) =
            parse_add_spec("gitlab::gitlab.com/group/subgroup/mypkg@main", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(
                    d.git.as_deref(),
                    Some("gitlab::gitlab.com/group/subgroup/mypkg")
                );
            }
            other => panic!("expected Detailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_gitlab_spec_cli_no_ref() {
        let (name, spec) = parse_add_spec("gitlab::gitlab.com/my-group/mypkg", false).unwrap();
        assert_eq!(name, "mypkg");
        match spec {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.rev, None);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_gitlab_spec_cli_rejects_bad_shape() {
        assert!(parse_add_spec("gitlab::gitlab.com/onlyone", false).is_err());
        assert!(parse_add_spec("gitlab::/my-group/mypkg", false).is_err());
        assert!(parse_add_spec("gitlab::gitlab.com//mypkg", false).is_err());
    }

    #[test]
    fn parse_generic_git_spec_cli() {
        for (raw, git, rev) in [
            (
                "git::https://git.corp.example/team/mypkg.git@abc123",
                "git::https://git.corp.example/team/mypkg.git",
                Some("abc123"),
            ),
            (
                "git::https://git.corp.example/team/mypkg.git",
                "git::https://git.corp.example/team/mypkg.git",
                None,
            ),
            (
                "git::git@bitbucket.org:team/mypkg.git@v1.0",
                "git::git@bitbucket.org:team/mypkg.git",
                Some("v1.0"),
            ),
            (
                "git::ssh://git@host:2222/team/mypkg@feature/x",
                "git::ssh://git@host:2222/team/mypkg",
                Some("feature/x"),
            ),
        ] {
            let (name, spec) = parse_add_spec(raw, false).unwrap();
            assert_eq!(name, "mypkg", "{raw}");
            let DependencySpec::Detailed(d) = &spec else {
                panic!("expected Detailed, got {spec:?}");
            };
            assert_eq!(d.git.as_deref(), Some(git), "{raw}");
            assert_eq!(d.rev.as_deref(), rev, "{raw}");
            assert_eq!(d.subdirectory, None);
            assert_eq!(
                format_spec(&spec),
                format!("{git}@{}", rev.unwrap_or("HEAD"))
            );
        }
    }

    #[test]
    fn parse_generic_git_spec_cli_rejects_bad_specs() {
        for (bad, reason) in [
            ("git::https://tok@git.corp.example/r.git", "credentials"),
            ("git::-oProxyCommand=touch", "cannot start with `-`"),
            (
                "git::https://host/r.git#subdirectory=pkg",
                "not supported for git::",
            ),
            ("git::ftp://host/r.git", "not supported"),
            ("git::https://host/r.git@", "not a valid git ref"),
            ("git::https://host", "no repository path"),
            (
                "git::https://host/my+pkg.git",
                "Invalid package name 'my+pkg'",
            ),
        ] {
            let msg = parse_add_spec(bad, false).unwrap_err().to_string();
            assert!(msg.contains(reason), "{bad}: {msg}");
            assert!(!msg.contains("tok@"), "{bad}: {msg}");
        }
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
