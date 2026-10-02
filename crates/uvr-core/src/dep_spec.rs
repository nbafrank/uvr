//! The dependency-spec grammar: one string naming a package and its source.
//!
//! `uvr add` parses its arguments with this, and so does a script's inline
//! header (#182), so the two can never disagree about what a spec means.
//!
//! | Spec | Meaning |
//! |---|---|
//! | `ggplot2` | CRAN, any version |
//! | `ggplot2>=3.4`, `ggplot2@>=3.4` | CRAN, constrained |
//! | `DESeq2 (bioc)`, `DESeq2>=1.40 (bioc)` | Bioconductor — `--bioc`, per spec |
//! | `user/repo[@ref][#subdirectory=path]` | GitHub |
//! | `forgejo::host/owner/repo[@ref]` | Forgejo |
//! | `gitlab::host/group[/subgroup…]/project[@ref]` | GitLab |
//! | `git::<clone URL>[@ref]` | Any git host |
//! | `https://…/pkg_1.2.0.tar.gz` | Source tarball by URL |

use crate::error::{Result, UvrError};
use crate::manifest::{DependencySpec, DetailedDep};
use crate::package_name;
use crate::registry::forgejo::parse_forgejo_parts;
use crate::registry::git_generic::parse_git_parts;
use crate::registry::gitlab::parse_gitlab_parts;

/// The per-spec spelling of `uvr add --bioc`, which a script header has no
/// flag to express.
const BIOC_SUFFIX: &str = "(bioc)";

fn invalid(message: String) -> UvrError {
    UvrError::InvalidDependencySpec(message)
}

fn split_subdirectory_fragment(raw: &str) -> Result<(&str, Option<&str>)> {
    let Some((base, fragment)) = raw.split_once('#') else {
        return Ok((raw, None));
    };
    let Some(path) = fragment.strip_prefix("subdirectory=") else {
        return Err(invalid(format!(
            "Unsupported fragment '#{fragment}' in '{raw}'. Expected: \
             owner/repo[@revision]#subdirectory=path"
        )));
    };
    if let Err(e) = crate::subdirectory::validate(path) {
        return Err(invalid(format!("{e} (in '{raw}')")));
    }
    Ok((base, Some(path)))
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

/// Parse one spec into `(package name, spec)` — see the module docs for the
/// grammar. `bioc` is the `--bioc` flag; a `(bioc)` suffix sets it for one
/// spec.
pub fn parse(raw: &str, bioc: bool) -> Result<(String, DependencySpec)> {
    let raw = raw.trim();

    // A git source has no channel to pick, so a `(bioc)` suffix on one is
    // refused rather than silently dropped. (`--bioc` still passes over git
    // specs: it applies to a whole `uvr add` batch, which may mix kinds.)
    let (raw, bioc) = match raw.strip_suffix(BIOC_SUFFIX) {
        Some(rest) => {
            let rest = rest.trim_end();
            if rest.contains('/') || rest.contains("::") {
                return Err(invalid(format!(
                    "'{raw}': `{BIOC_SUFFIX}` marks a Bioconductor package, \
                     but '{rest}' is a git source"
                )));
            }
            (rest, true)
        }
        None => (raw, bioc),
    };

    // Any git host: `git::<clone URL>[@ref]` (#190). The URL can contain `/`,
    // so this comes before the GitHub heuristic too. The name is the
    // repository name until the DESCRIPTION lookup replaces it.
    if raw.starts_with("git::") {
        if raw.contains("#subdirectory=") {
            return Err(invalid(format!(
                "`#subdirectory=` is not supported for git:: sources yet (in '{}').",
                crate::auth::redact_url(raw)
            )));
        }
        let parsed = parse_git_parts(raw).map_err(|reason| {
            invalid(format!(
                "Invalid git spec '{}': {reason}. Expected: git::<clone URL>[@ref], with an \
                 https://, ssh:// or user@host:path URL",
                crate::auth::redact_url(raw)
            ))
        })?;
        let name = crate::registry::git_generic::repo_name(&parsed.url).to_string();
        if !package_name::is_valid(&name) {
            return Err(invalid(format!(
                "Invalid package name '{name}' extracted from git spec '{raw}'"
            )));
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
        if !crate::registry::url::is_source_tarball_url(raw) {
            return Err(invalid(format!(
                "Unsupported URL '{raw}'. Only direct source tarball URLs ending in .tar.gz or \
                 .tgz are supported. For a git repository use user/repo[@ref], \
                 forgejo::host/owner/repo[@ref], or gitlab::host/group/project[@ref]."
            )));
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
            return Err(invalid(format!(
                "Invalid Forgejo spec '{raw}'. Expected: forgejo::host/owner/repo or forgejo::host/owner/repo@ref"
            )));
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
            return Err(invalid(format!(
                "Invalid GitLab spec '{raw}'. Expected: gitlab::host/group/project or gitlab::host/group/subgroup/project[@ref]"
            )));
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
                return Err(invalid(format!(
                    "Unsupported git host '{host}' in '{raw}'. Supported specs: \
                     GitHub via user/repo[@ref], Forgejo via forgejo::host/owner/repo[@ref], \
                     GitLab via gitlab::host/group/project[@ref], any git host via \
                     git::https://host/path/repo.git[@ref].",
                    host = parts[0],
                )));
            }
            return Err(invalid(format!(
                "Invalid GitHub spec '{raw}'. Expected format: user/repo or user/repo@ref"
            )));
        }

        let name = match subdirectory {
            Some(sub) => sub.rsplit('/').next().unwrap_or(sub).to_string(),
            None => parts[1].to_string(),
        };

        // Validate package name characters
        if subdirectory.is_none() && !package_name::is_valid(&name) {
            return Err(invalid(format!(
                "Invalid package name '{name}' extracted from GitHub spec '{raw}'"
            )));
        }

        if subdirectory.is_some() {
            let full = match &git_ref {
                Some(r) => format!("{repo}@{r}"),
                None => repo.clone(),
            };
            if !crate::registry::github::is_valid_github_repo_spec(&full) {
                return Err(invalid(format!(
                    "Invalid GitHub spec '{raw}'. Expected format: \
                     owner/repo[@revision]#subdirectory=path"
                )));
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

    // CRAN/Bioc with an optional version. `uvr add` spells it `pkg@>=1.0`;
    // a header reads more naturally as `pkg>=1.0` or `pkg >= 1.0`. None of
    // `@ < > =` can appear in a package name, so the name ends at the first
    // of them either way, and an empty constraint (`pkg@`) means none.
    let (name, version) = match raw.find(['@', '<', '>', '=']) {
        Some(at) => {
            let rest = &raw[at..];
            let rest = rest.strip_prefix('@').unwrap_or(rest).trim();
            (raw[..at].trim_end(), Some(rest).filter(|v| !v.is_empty()))
        }
        None => (raw, None),
    };

    // Validate CRAN/Bioc package name
    if !package_name::is_valid(name) {
        return Err(invalid(format!("Invalid package name '{name}'")));
    }

    // Checked here, where the spec can still be named, rather than failing
    // later in the resolver as a semver error that names nothing.
    if let Some(v) = version {
        crate::resolver::parse_version_req(v)
            .map_err(|e| invalid(format!("Invalid version constraint '{v}' in '{raw}': {e}")))?;
    }
    let version = version.map(str::to_string);

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

    Ok((name.to_string(), spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(raw: &str) -> String {
        match parse(raw, false).unwrap() {
            (name, DependencySpec::Version(v)) if name == "ggplot2" => v,
            other => panic!("{raw}: unexpected {other:?}"),
        }
    }

    #[test]
    fn a_constraint_parses_the_same_in_every_spelling() {
        // `@` is `uvr add`'s spelling; the operator-first forms are what the
        // design writes in a header. All mean one thing.
        for raw in [
            "ggplot2@>=3.4",
            "ggplot2>=3.4",
            "ggplot2 >=3.4",
            "ggplot2@ >=3.4",
        ] {
            assert_eq!(version(raw), ">=3.4", "{raw}");
        }
        assert_eq!(version("ggplot2 >= 3.4"), ">= 3.4");
        assert_eq!(version("ggplot2==3.5.1"), "==3.5.1");
        assert_eq!(version("ggplot2<4"), "<4");
    }

    #[test]
    fn a_bare_name_or_empty_constraint_is_the_default_spec() {
        // The default spec is what `--with` uses, and what the with-env cache
        // key treats as "bare" — so `pkg@` must not mint a distinct entry.
        for raw in ["ggplot2", "ggplot2@", " ggplot2 ", "ggplot2@*"] {
            assert_eq!(
                parse(raw, false).unwrap(),
                ("ggplot2".to_string(), DependencySpec::default()),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_bad_constraint_is_refused_up_front_naming_the_spec() {
        let err = parse("ggplot2>>3", false).unwrap_err().to_string();
        assert!(err.contains("Invalid version constraint"), "{err}");
        assert!(err.contains("ggplot2>>3"), "{err}");
    }

    #[test]
    fn an_operator_does_not_rescue_a_bad_name() {
        for raw in ["my pkg>=1", ">=1", "@1.0", "ggplot2 (>= 3.4)", "pkg!=1"] {
            let err = parse(raw, false).unwrap_err().to_string();
            assert!(err.contains("Invalid package name"), "{raw}: {err}");
        }
    }

    #[test]
    fn the_bioc_suffix_is_the_bioc_flag() {
        assert_eq!(
            parse("DESeq2 (bioc)", false).unwrap(),
            parse("DESeq2", true).unwrap()
        );
        assert_eq!(parse("DESeq2(bioc)", false).unwrap().0, "DESeq2");
        let (name, spec) = parse("DESeq2>=1.40 (bioc)", false).unwrap();
        assert_eq!(name, "DESeq2");
        assert!(spec.is_bioc());
        assert_eq!(spec.version_req(), Some(">=1.40"));
    }

    #[test]
    fn the_bioc_suffix_on_a_git_source_is_refused() {
        for raw in [
            "user/repo (bioc)",
            "forgejo::codeberg.org/o/r (bioc)",
            "gitlab::gitlab.com/g/p (bioc)",
        ] {
            let err = parse(raw, false).unwrap_err().to_string();
            assert!(err.contains("git source"), "{raw}: {err}");
        }
    }

    #[test]
    fn git_specs_keep_their_existing_meaning() {
        let (name, spec) = parse("rladies/praise@v1.0.0", false).unwrap();
        assert_eq!(name, "praise");
        assert_eq!(spec.git(), Some("rladies/praise"));
        assert!(matches!(&spec, DependencySpec::Detailed(d) if d.rev.as_deref() == Some("v1.0.0")));

        let (name, spec) = parse("forgejo::codeberg.org/owner/pkg@main", false).unwrap();
        assert_eq!(name, "pkg");
        assert_eq!(spec.git(), Some("forgejo::codeberg.org/owner/pkg"));

        let (name, spec) = parse("gitlab::gitlab.com/group/sub/pkg", false).unwrap();
        assert_eq!(name, "pkg");
        assert_eq!(spec.git(), Some("gitlab::gitlab.com/group/sub/pkg"));
    }
}
