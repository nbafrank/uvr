use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use uvr_core::manifest::{DependencySpec, Manifest};
use uvr_core::project::{ManifestSource, Project};
use uvr_core::r_env::REnv;
use uvr_core::r_version::detector::{find_r_binary, find_r_binary_ignoring_pin, query_r_version};
use uvr_core::script_header::{self, ScriptHeader};

pub async fn run(
    script: Option<String>,
    r_version_override: Option<String>,
    with_packages: Vec<String>,
    args: Vec<String>,
) -> Result<()> {
    // A script carrying an inline dependency header runs *standalone*: the
    // surrounding project is ignored entirely, which is exactly what lets the
    // same file reproduce in any directory on anyone's machine (#181).
    let header = match script.as_deref() {
        Some(path) => read_header(path)?,
        None => None,
    };
    let script_mode = header.is_some();

    if let Some(constraint) = header.as_ref().and_then(|h| h.r.as_deref()) {
        // Parsed, but #183 is what makes it select an R. Saying so beats
        // running against whichever interpreter happens to be around and
        // letting the script fail somewhere less obvious. The constraint is
        // free text from the header — control-escape it, or a crafted `r`
        // value could smuggle ANSI sequences into uvr's own diagnostics.
        crate::ui::warn(format!(
            "script header pins R `{}`, which uvr does not honour yet (#183) — \
             running against the R resolved as usual",
            script_header::sanitize_for_display(constraint)
        ));
    }

    // Resolve project (optional — uvr run works outside a project too).
    // Skipped in script mode, so a headered script neither inherits the
    // project's R constraint nor creates its `.uvr/library/` as a side effect.
    let (project_library, project_r_constraint) = if script_mode {
        (None, None)
    } else {
        match Project::find_cwd() {
            Ok(p) => {
                p.ensure_library_dir()
                    .context("Failed to create .uvr/library/")?;
                let lib = p.library_path();
                let rv = p.manifest.project.r_version.clone();
                (Some(lib), rv)
            }
            Err(_) => (None, None),
        }
    };

    // --r-version flag takes priority over the project constraint.
    let effective_constraint = r_version_override
        .as_deref()
        .or(project_r_constraint.as_deref());

    // In script mode a `.r-version` pin is ignored along with the rest of the
    // project. `find_r_binary` walks up from the working directory to find
    // one, and the pin outranks every other signal — so honouring it would
    // let the directory a script happens to sit in choose its interpreter,
    // and with it the ephemeral environment's cache key.
    let r_binary = if script_mode {
        find_r_binary_ignoring_pin(effective_constraint)
    } else {
        find_r_binary(effective_constraint)
    }
    .context("R not found. Install R or use `uvr r install <version>`")?;

    // A headered script's dependencies join any `--with` packages in a single
    // ephemeral environment. `--with` takes bare names only.
    let mut deps: Vec<(String, DependencySpec)> = with_packages
        .into_iter()
        .map(|pkg| (pkg, DependencySpec::default()))
        .collect();
    if let Some(header) = &header {
        deps.extend(header.dependencies.iter().cloned());
    }

    // Script mode always builds an environment, even when the header declares
    // no packages: an empty isolated library is still the right answer, and is
    // not the same thing as falling back to whatever the machine provides.
    let with_env = if deps.is_empty() && !script_mode {
        None
    } else {
        // The R version is part of the --with cache key (see ensure_with_env).
        // Falling back to a default here would collapse every R version into
        // one cache entry, silently reusing ABI-incompatible compiled
        // packages — so refuse to proceed without a real version (#160).
        let r_ver = query_r_version(&r_binary).with_context(|| {
            format!(
                "Could not determine the version of R at {}; \
                 refusing to build a --with environment without a version-scoped cache key",
                r_binary.display()
            )
        })?;
        Some(ensure_with_env(&deps, &r_ver).await?)
    };

    let (library, with_library) = match with_env {
        // Script mode: the ephemeral environment replaces the project library
        // rather than sitting in front of it, so the script cannot quietly
        // satisfy an undeclared `library()` call from whatever happens to be
        // installed on this machine and then fail on the next one.
        // (`UVR_EXTRA_LIBS` is still appended by `REnv` — it is a deliberate
        // per-machine escape hatch, not ambient project state.)
        Some(env) if script_mode => (env, None),
        env => (project_library.unwrap_or_else(fallback_library), env),
    };

    // The isolated environment — library search path, shadowed system
    // libraries, and R's runtime lib dir. Built by `uvr_core::r_env` so that
    // `uvr run` and `uvr activate` export exactly the same set and cannot
    // drift apart.
    let site_profile = uvr_core::r_version::openmp::runtime_site_profile_for_binary(&r_binary);
    let r_env = REnv {
        r_binary,
        library,
        with_library,
        extra_libs: uvr_core::env_vars::extra_libs(),
        site_profile,
    };

    let mut cmd = Command::new(&r_env.r_binary);
    for (key, value) in r_env.vars() {
        cmd.env(key, value);
    }
    // Belt and braces alongside the blank `R_ENVIRON`: a process flag is
    // available here, but not to a sourced activation script.
    cmd.arg("--no-environ");

    if script_mode {
        // Set here rather than in `REnv::vars()` for the same reason as
        // `--no-environ` above: it is a property of *this* invocation, not of
        // the isolated environment `uvr activate` also exports. Activation
        // must never disable the user's startup profile.
        //
        // It is needed because environment variables alone do not isolate a
        // headered script. R still sources a startup profile, and uvr's own
        // project `.Rprofile` runs `.libPaths(unique(c(lib, .libPaths())))`,
        // which puts the surrounding project's library *ahead* of the
        // script's environment and quietly undoes the isolation the header
        // exists to provide; `~/.Rprofile` can do the same for the machine at
        // large. Pointing `R_PROFILE_USER` at the null device skips both, the
        // same way the installer does (`installer/r_cmd_install.rs`).
        //
        // `R_PROFILE` (the *site* profile) is not touched here: `REnv::vars()`
        // owns it, pointing it at uvr's OpenMP shim profile when the R needs
        // one (#261).
        cmd.env(
            "R_PROFILE_USER",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        );
    }

    if let Some(script_path) = &script {
        // Script mode — run as quietly as Rscript does. `Rscript foo.R` is
        // effectively `R --no-echo --no-restore --file=foo.R`; --no-echo
        // suppresses both the startup banner (#81) and R's echoing of every
        // parsed source line back to stdout with a `> ` prompt (#117).
        // (--quiet, used previously, only dropped the banner. The old
        // rationale — keeping prompts visible for browser() — doesn't hold:
        // --file mode is non-interactive, so browser() is a no-op either way.)
        cmd.arg("--no-echo");
        cmd.arg("--no-save");
        cmd.arg("--no-restore");
        cmd.arg(format!("--file={script_path}"));
        if !args.is_empty() {
            cmd.arg("--args");
            cmd.args(&args);
        }
    } else {
        // Interactive mode — keep the banner. It's part of the REPL
        // experience and a user typing `uvr run` (no script) expects
        // R's normal startup output.
        cmd.arg("--no-save");
    }

    let status = cmd.status().context("Failed to spawn R")?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        return Err(ScriptExitError(code).into());
    }

    Ok(())
}

/// Read a script's inline dependency header (see [`script_header::parse`]).
///
/// An unreadable path yields `None` rather than an error: `uvr run` has always
/// let R report a missing or unopenable script in its own words, and looking
/// for a header is no reason to start intercepting that.
fn read_header(path: &str) -> Result<Option<ScriptHeader>> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    script_header::parse(&source).map_err(|e| anyhow!("Invalid script header in {path}: {e}"))
}

/// Library used when `uvr run` is invoked outside a project.
fn fallback_library() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            // HOME-less environment (sandbox/CI): degrade to the system
            // temp dir instead of dropping a library tree into the
            // working directory (#161).
            std::env::temp_dir()
        })
        .join("uvr")
        .join("library")
}

/// Sort `deps` by name and collapse each package to one entry, so the same
/// set in any order — including a package named both in a header and via
/// `--with` — reuses one environment.
///
/// A bare name yields to a real spec for the same package (`--with ggplot2`
/// beside a header's `ggplot2>=3.4`). Two different real specs for one
/// package are an error: the manifest holds one spec per name, so one of
/// them would otherwise be dropped without a word.
fn canonical_deps(deps: &[(String, DependencySpec)]) -> Result<Vec<(String, DependencySpec)>> {
    let bare = DependencySpec::default();
    let mut sorted = deps.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<(String, DependencySpec)> = Vec::with_capacity(sorted.len());
    for (name, spec) in sorted {
        match out.last_mut() {
            Some((prev, prev_spec)) if *prev == name => {
                if *prev_spec == bare {
                    *prev_spec = spec;
                } else if spec != bare && spec != *prev_spec {
                    anyhow::bail!(
                        "`{}` is requested twice with different specs; keep one",
                        script_header::sanitize_for_display(&name)
                    );
                }
            }
            _ => out.push((name, spec)),
        }
    }
    Ok(out)
}

/// One package's contribution to [`with_env_key`].
///
/// A bare name contributes exactly the name, as it always has, so existing
/// environments keep their keys (#182). Anything more specific appends its
/// serialized spec after an `@`, so two headers differing only by version,
/// channel or ref get different environments. The JSON form is `uvr.toml`'s
/// own serde shape: fields that are unset are skipped, so a field added to
/// `DetailedDep` later leaves existing keys alone as long as it is skipped
/// when unset too.
fn cache_entry(name: &str, spec: &DependencySpec) -> String {
    if *spec == DependencySpec::default() {
        return name.to_string();
    }
    let spec = serde_json::to_string(spec).expect("a DependencySpec always serializes");
    format!("{name}@{spec}")
}

/// Cache key for an ephemeral environment: the directory name under
/// `<cache>/with-envs/`.
///
/// `packages` are [`cache_entry`] strings of [`canonical_deps`] output. The R
/// version is part of the key because compiled packages are ABI-bound to an
/// R minor (#160).
///
/// **This function's output is a compatibility surface.** Changing it silently
/// orphans every cached environment on every user's machine, so the tests pin
/// golden values: a bare package name must keep hashing exactly as it did
/// before #182 generalised the header grammar.
fn with_env_key(packages: &[String], r_version: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(r_version.as_bytes());
    for pkg in packages {
        hasher.update(b"\0");
        hasher.update(pkg.as_bytes());
    }
    format!("{:x}", hasher.finalize())[..12].to_string()
}

/// Ensure the `--with` and header dependencies are installed in a cached
/// environment. Returns the path to the cached library directory.
async fn ensure_with_env(deps: &[(String, DependencySpec)], r_version: &str) -> Result<PathBuf> {
    // Same package from the header and `--with` (or listed twice) is one
    // logical set — without this it would mint a second cache dir and
    // install everything again.
    let mut deps = canonical_deps(deps)?;
    let entries: Vec<String> = deps.iter().map(|(n, s)| cache_entry(n, s)).collect();
    let short_hash = with_env_key(&entries, r_version);

    let cache_dir = uvr_core::env_vars::cache_dir()
        .unwrap_or_else(|| {
            // HOME-less environment (sandbox/CI): degrade to the system temp
            // dir instead of scattering with-envs/ into the working directory.
            let fallback = std::env::temp_dir().join("uvr-cache");
            tracing::warn!(
                "HOME and UVR_CACHE_DIR are unset; using temporary --with env cache at {}",
                fallback.display()
            );
            fallback
        })
        .join("with-envs")
        .join(short_hash);

    let lib_dir = cache_dir.join(".uvr").join("library");

    // Created up front because both early exits below can skip the install
    // that would otherwise make it: an already-warm cache, and a script whose
    // header declares no packages at all. R must be handed a library path
    // that exists either way.
    std::fs::create_dir_all(&lib_dir)
        .with_context(|| format!("Failed to create {}", lib_dir.display()))?;

    // Check if all requested packages are already installed.
    let all_installed = |deps: &[(String, DependencySpec)]| {
        deps.iter()
            .all(|(pkg, _)| lib_dir.join(pkg).join("DESCRIPTION").exists())
    };
    if all_installed(&deps) {
        return Ok(lib_dir);
    }

    // A git spec is named after its repository until the remote DESCRIPTION
    // says otherwise (`satijalab/seurat` ships `Seurat`); the manifest must
    // use the real name, exactly as `uvr add` does. The cache key above stays
    // on the name as written, so a warm run never needs the network.
    // ponytail: a renamed git package costs this DESCRIPTION fetch on every
    // run; record the resolved names in the env dir if that ever matters.
    crate::commands::add::resolve_git_pkg_names(&mut deps).await?;
    if all_installed(&deps) {
        return Ok(lib_dir);
    }

    // Build a temporary manifest with the requested packages.
    let mut manifest = Manifest::new("__with__", None);
    for (pkg, spec) in &deps {
        manifest.add_dep(pkg.clone(), spec.clone(), false);
    }

    let project = Project {
        root: cache_dir.clone(),
        manifest,
        manifest_source: ManifestSource::Toml,
    };
    project
        .ensure_library_dir()
        .context("Failed to create --with cache library")?;
    project
        .save_manifest()
        .context("Failed to write --with manifest")?;

    // Resolve and install.
    //
    // The library target is pinned explicitly rather than left to the
    // project's own resolution: `Project::library_path()` honors
    // `UVR_LIBRARY` (#97), and this throwaway project only exists to
    // populate the ephemeral with-env cache. Without the override, a user
    // with `UVR_LIBRARY` exported — the audience that feature is for —
    // installs a script's declared packages into their shared library
    // instead, and the script then fails because `lib_dir`, which is what
    // R is actually pointed at, stays empty. Nothing prunes the shared
    // library afterwards either, so the stray packages accumulate.
    let lockfile = crate::commands::lock::resolve_and_lock(&project, false).await?;
    crate::commands::sync::install_from_lockfile(&project, &lockfile, 4, Some(&lib_dir), None)
        .await?;

    Ok(lib_dir)
}

/// Sentinel error that carries an R script's exit code.
/// `main` matches on this to forward the exact code to the shell.
#[derive(Debug)]
pub struct ScriptExitError(pub i32);

impl std::fmt::Display for ScriptExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "R script exited with code {}", self.0)
    }
}

impl std::error::Error for ScriptExitError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key `ensure_with_env` computes for `deps`.
    fn spec_key(deps: &[(&str, DependencySpec)], r: &str) -> String {
        let deps: Vec<_> = deps
            .iter()
            .map(|(n, s)| (n.to_string(), s.clone()))
            .collect();
        let entries: Vec<String> = canonical_deps(&deps)
            .unwrap()
            .iter()
            .map(|(n, s)| cache_entry(n, s))
            .collect();
        with_env_key(&entries, r)
    }

    /// The key for bare names — what `--with` and plain header entries give.
    fn key(pkgs: &[&str], r: &str) -> String {
        let deps: Vec<_> = pkgs
            .iter()
            .map(|p| (*p, DependencySpec::default()))
            .collect();
        spec_key(&deps, r)
    }

    /// The key for header entries, parsed as `uvr run` parses them.
    fn header_key(specs: &[&str], r: &str) -> String {
        let deps: Vec<_> = specs
            .iter()
            .map(|s| uvr_core::dep_spec::parse(s, false).unwrap())
            .collect();
        let deps: Vec<_> = deps.iter().map(|(n, s)| (n.as_str(), s.clone())).collect();
        spec_key(&deps, r)
    }

    #[test]
    fn the_same_dependency_set_reuses_one_environment() {
        // What "the ephemeral environment is cached and reused across runs of
        // the same header" (#181) actually rests on.
        assert_eq!(key(&["jsonlite"], "4.4.2"), key(&["jsonlite"], "4.4.2"));
        // Declaration order must not fork the cache.
        assert_eq!(key(&["a", "b"], "4.4.2"), key(&["b", "a"], "4.4.2"));
        // Nor must a duplicate: the same package named in the header and via
        // `--with` is one logical set, not a second environment.
        assert_eq!(
            key(&["jsonlite", "jsonlite"], "4.4.2"),
            key(&["jsonlite"], "4.4.2")
        );
    }

    #[test]
    fn different_dependency_sets_get_different_environments() {
        let base = key(&["jsonlite"], "4.4.2");
        assert_ne!(base, key(&["cli"], "4.4.2"));
        assert_ne!(base, key(&["jsonlite", "cli"], "4.4.2"));
        assert_ne!(base, key(&[], "4.4.2"));
        // ABI: compiled packages built for one R minor must not be reused
        // under another (#160).
        assert_ne!(base, key(&["jsonlite"], "4.5.1"));
    }

    #[test]
    fn a_bare_package_name_still_hashes_to_its_established_key() {
        // Golden value. `--with jsonlite` has resolved to this directory
        // since the ephemeral-env cache shipped; #181 routes header
        // dependencies through the same function, and #182 will generalise
        // the grammar. Any change here orphans every user's cache, so this
        // test exists to make that a deliberate decision rather than a
        // side effect.
        assert_eq!(key(&["jsonlite"], "4.4.2"), "c449740c65a0");
        // A set, not just one name: pins the separator and the ordering too.
        // Both values were computed on upstream/main before #182.
        assert_eq!(key(&["jsonlite", "cli"], "4.4.2"), "370056f5a939");
        assert_eq!(
            key(&["praise", "jsonlite", "ggplot2", "praise"], "4.5.1"),
            "5816a50b3060"
        );
        // The same names written in a header parse to the default spec and
        // land on the same keys.
        assert_eq!(header_key(&["jsonlite"], "4.4.2"), "c449740c65a0");
        assert_eq!(
            header_key(&["jsonlite", "cli", "cli@*"], "4.4.2"),
            "370056f5a939"
        );
    }

    #[test]
    fn headers_differing_only_by_spec_get_different_environments() {
        let bare = header_key(&["jsonlite"], "4.4.2");
        let v18 = header_key(&["jsonlite>=1.8"], "4.4.2");
        assert_ne!(bare, v18);
        assert_ne!(v18, header_key(&["jsonlite>=1.9"], "4.4.2"));
        // One constraint, two spellings: one environment.
        assert_eq!(v18, header_key(&["jsonlite@>=1.8"], "4.4.2"));
        // Channel and ref are part of the spec too.
        assert_ne!(
            header_key(&["limma"], "4.4.2"),
            header_key(&["limma (bioc)"], "4.4.2")
        );
        assert_ne!(
            header_key(&["rladies/praise@v1"], "4.4.2"),
            header_key(&["rladies/praise@main"], "4.4.2")
        );
        assert_ne!(
            header_key(&["praise"], "4.4.2"),
            header_key(&["rladies/praise"], "4.4.2")
        );
    }

    #[test]
    fn a_bare_name_yields_to_a_spec_for_the_same_package() {
        // `uvr run --with jsonlite script.R` whose header pins jsonlite: one
        // package, and the pin is what gets installed.
        let pinned = DependencySpec::Version(">=1.8".to_string());
        let deps = vec![
            ("jsonlite".to_string(), DependencySpec::default()),
            ("jsonlite".to_string(), pinned.clone()),
        ];
        assert_eq!(
            canonical_deps(&deps).unwrap(),
            vec![("jsonlite".to_string(), pinned.clone())]
        );
        let reversed: Vec<_> = deps.iter().rev().cloned().collect();
        assert_eq!(
            canonical_deps(&reversed).unwrap(),
            canonical_deps(&deps).unwrap()
        );
    }

    #[test]
    fn two_different_specs_for_one_package_are_an_error() {
        let deps = vec![
            (
                "jsonlite".to_string(),
                DependencySpec::Version(">=1.8".to_string()),
            ),
            (
                "jsonlite".to_string(),
                DependencySpec::Version("<1.8".to_string()),
            ),
        ];
        let err = canonical_deps(&deps).unwrap_err().to_string();
        assert!(err.contains("jsonlite"), "{err}");
        assert!(err.contains("different specs"), "{err}");
    }
}
