use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, UvrError};

/// Top-level `uvr.toml` structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub project: ProjectMeta,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DependencySpec>,

    #[serde(
        rename = "dev-dependencies",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub dev_dependencies: BTreeMap<String, DependencySpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<PackageSource>,

    /// Optional `[activate]` block — shell-activation preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<ActivateMeta>,
}

/// `[activate]` — how `source .uvr/activate` behaves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ActivateMeta {
    /// Prefix the shell prompt with the project name while activated.
    ///
    /// Off by default: a mutated prompt is contentious, and plenty of users
    /// build their own from a framework that would fight with us.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMeta {
    pub name: String,

    /// SemVer requirement, e.g. `">=4.0.0"`
    #[serde(default)]
    pub r_version: Option<String>,

    /// Explicit Bioconductor release, e.g. `"3.18"`.
    /// When omitted, auto-detected from the active R version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bioc_version: Option<String>,

    #[serde(default)]
    pub description: Option<String>,

    /// Created with `uvr init --bare`: the project ships only `uvr.toml`
    /// and `.uvr/library/`. `.Rprofile`, `.gitignore`, activation shims,
    /// IDE config, and the companion package are all skipped, and the
    /// library is reachable through `uvr run` only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bare: bool,
}

/// Either a bare version string (`">=3.0.0"`, `"*"`) or a detailed table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DependencySpec {
    Version(String),
    Detailed(DetailedDep),
}

impl DependencySpec {
    pub fn version_req(&self) -> Option<&str> {
        match self {
            DependencySpec::Version(v) => Some(v),
            DependencySpec::Detailed(d) => d.version.as_deref(),
        }
    }

    pub fn is_bioc(&self) -> bool {
        match self {
            DependencySpec::Version(_) => false,
            DependencySpec::Detailed(d) => d.bioc.unwrap_or(false),
        }
    }

    pub fn git(&self) -> Option<&str> {
        match self {
            DependencySpec::Detailed(d) => d.git.as_deref(),
            _ => None,
        }
    }

    pub fn subdirectory(&self) -> Option<&str> {
        match self {
            DependencySpec::Detailed(d) => d.subdirectory.as_deref(),
            _ => None,
        }
    }
}

impl Default for DependencySpec {
    fn default() -> Self {
        DependencySpec::Version("*".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DetailedDep {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// `true` = Bioconductor package
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bioc: Option<bool>,

    /// `"user/repo"` — GitHub source
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,

    /// Require the fetched DESCRIPTION `Package:` to equal the manifest key.
    /// This preserves an explicit `Remotes:` alias even when it has the same
    /// spelling as the repository basename.
    #[serde(default, skip_serializing_if = "is_false")]
    pub exact: bool,

    /// branch / tag / commit SHA
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdirectory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageSource {
    pub name: String,
    pub url: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl std::str::FromStr for Manifest {
    type Err = crate::error::UvrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        // First parse to Value so we can detect dotted-key traps before
        // serde silently discards them.
        let raw: toml::Value =
            toml::from_str(s).map_err(|e| crate::error::UvrError::ManifestParse(e.to_string()))?;

        // Validate [dependencies] and [dev-dependencies]: every value must be
        // a string (bare version) or a table whose keys are known DetailedDep
        // fields {version, bioc, git, exact, rev, subdirectory}. A TOML table-header entry like
        // `[dependencies.data.table]` creates a nested table under key `data`
        // with a sub-key `table` — not a valid DetailedDep field. This check
        // catches that case before serde silently resolves the wrong package.
        //
        // NOTE: `VALID_DEP_KEYS` must list every field of `DetailedDep`. A
        // field added to that struct without updating this slice will cause
        // valid manifests to be rejected — keep them in sync.
        const VALID_DEP_KEYS: &[&str] = &["version", "bioc", "git", "exact", "rev", "subdirectory"];

        for section in &["dependencies", "dev-dependencies"] {
            if let Some(toml::Value::Table(deps)) = raw.get(*section) {
                for (key, val) in deps {
                    if let toml::Value::Table(inner) = val {
                        // A valid DetailedDep table has only known field keys.
                        // Any other key signals a dotted-key trap: the user
                        // wrote `[dependencies.org.Hs.eg.db]` which TOML parsed
                        // as key=`org`, sub-table={Hs: {eg: {db: ...}}}.
                        //
                        // Walk the nested table chain to reconstruct the full
                        // dotted package name (e.g. `org.Hs.eg.db` not just
                        // `org.Hs`), so the error message shows the correct
                        // name to quote.
                        let unknown: Vec<&str> = inner
                            .keys()
                            .map(String::as_str)
                            .filter(|k| !VALID_DEP_KEYS.contains(k))
                            .collect();
                        if !unknown.is_empty() {
                            // Walk into nested tables to collect the full name.
                            // Stop when we reach a leaf or a valid DetailedDep.
                            let full_name = {
                                let mut parts = vec![key.as_str()];
                                let mut cur = inner;
                                loop {
                                    // Find the first non-DetailedDep key at this level
                                    let next = cur
                                        .keys()
                                        .map(String::as_str)
                                        .find(|k| !VALID_DEP_KEYS.contains(k));
                                    match next {
                                        Some(k) => {
                                            parts.push(k);
                                            match cur.get(k) {
                                                Some(toml::Value::Table(t)) => cur = t,
                                                _ => break,
                                            }
                                        }
                                        None => break,
                                    }
                                }
                                parts.join(".")
                            };
                            return Err(crate::error::UvrError::ManifestParse(format!(
                                "dependency `{key}` in [{section}] contains dotted sub-key(s): \
                                 {unknown:?}.\n\
                                 \n\
                                 This is caused by a dotted TOML key: \
                                 `[{section}.{full_name}]` is parsed by TOML as \
                                 package `{key}` with nested sub-keys, not as \
                                 package `{full_name}`.\n\
                                 \n\
                                 If you meant to declare package `{full_name}`, \
                                 use a quoted key in the flat [{section}] table:\n\
                                 \n\
                                 \t[{section}]\n\
                                 \t\"{full_name}\" = \"*\"\n\
                                 \n\
                                 Or run: uvr add {full_name}"
                            )));
                        }
                        validate_subdirectory_entry(key, section, inner)?;
                    }
                }
            }
        }

        let manifest: Manifest =
            toml::from_str(s).map_err(|e| crate::error::UvrError::ManifestParse(e.to_string()))?;
        manifest.validate_detailed_dependencies()?;
        Ok(manifest)
    }
}

impl Manifest {
    pub fn new(name: impl Into<String>, r_version: Option<String>) -> Self {
        Manifest {
            project: ProjectMeta {
                name: name.into(),
                r_version,
                bioc_version: None,
                description: None,
                bare: false,
            },
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            sources: Vec::new(),
            activate: None,
        }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        s.parse()
    }

    /// Parse an R `DESCRIPTION` file (DCF format) into a `Manifest`.
    ///
    /// - `Imports:` → `dependencies`
    /// - `Suggests:` → `dev_dependencies`
    /// - `Depends: R (>= x.y.z)` → `project.r_version`
    /// - Non-R entries in `Depends:` are merged into `dependencies`
    /// - `Remotes:` entries override matching `Imports:` / `Depends:` /
    ///   `Suggests:` entries with supported git-source specs.
    pub fn from_description_str(content: &str) -> Result<Self> {
        let fields = parse_dcf(content);

        let name = fields
            .get("Package")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        let r_version = fields
            .get("Depends")
            .and_then(|deps| parse_r_version_from_depends(deps));

        let mut dependencies = BTreeMap::new();
        let mut dev_dependencies = BTreeMap::new();

        if let Some(imports) = fields.get("Imports") {
            for (pkg, spec) in parse_dep_field(imports) {
                dependencies.insert(pkg, spec);
            }
        }
        if let Some(depends) = fields.get("Depends") {
            for (pkg, spec) in parse_dep_field(depends) {
                if pkg != "R" {
                    dependencies.insert(pkg, spec);
                }
            }
        }
        if let Some(suggests) = fields.get("Suggests") {
            for (pkg, spec) in parse_dep_field(suggests) {
                dev_dependencies.insert(pkg, spec);
            }
        }

        if let Some(remotes) = fields.get("Remotes") {
            for entry in parse_remotes_field_rich(remotes) {
                let source = match entry {
                    RemoteEntry::Source(source) => source,
                    RemoteEntry::Unsupported {
                        entry,
                        reason,
                        bound: true,
                    } => {
                        return Err(UvrError::ManifestParse(format!(
                            "unsupported bound Remotes entry '{entry}': {reason}. Correct or remove \
                             the alias; nested GitHub remotes must use `PackageName=owner/repo:path`."
                        )));
                    }
                    RemoteEntry::Unsupported {
                        entry,
                        reason,
                        bound: false,
                    } => {
                        tracing::warn!(
                            "Ignoring unbound Remotes entry '{entry}' while importing DESCRIPTION: \
                             {reason}"
                        );
                        continue;
                    }
                };

                let resolved = if source.explicit_name {
                    source.name.clone()
                } else if let Some(name) =
                    match_remote_pkg_name(&source.name, &dependencies, &dev_dependencies)
                {
                    name
                } else if source.subdirectory.is_some() {
                    return Err(UvrError::ManifestParse(format!(
                        "unaliased nested Remotes entry for '{}' cannot be matched to an \
                         Imports, Depends, or Suggests package. Add an explicit binding such as \
                         `PackageName={}`.",
                        source.repository,
                        remote_source_target(&source)
                    )));
                } else {
                    source.name.clone()
                };
                let target = if dev_dependencies.contains_key(&resolved) {
                    &mut dev_dependencies
                } else {
                    &mut dependencies
                };
                let existing_version = target
                    .get(&resolved)
                    .and_then(|s| s.version_req())
                    .filter(|v| *v != "*")
                    .map(str::to_string);
                let spec = match source.dependency_spec() {
                    DependencySpec::Detailed(d)
                        if d.version.is_none() && existing_version.is_some() =>
                    {
                        DependencySpec::Detailed(DetailedDep {
                            version: existing_version,
                            ..d
                        })
                    }
                    other => other,
                };
                target.insert(resolved, spec);
            }
        }

        Ok(Manifest {
            project: ProjectMeta {
                name,
                r_version,
                bioc_version: None,
                description: fields.get("Title").cloned(),
                bare: false,
            },
            dependencies,
            dev_dependencies,
            sources: Vec::new(),
            activate: None,
        })
    }

    pub fn from_description_file(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_description_str(&s)
    }

    pub fn to_toml_string(&self) -> Result<String> {
        self.validate_detailed_dependencies()?;
        toml::to_string_pretty(self).map_err(UvrError::TomlSer)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let s = self.to_toml_string()?;
        atomic_write(path, s.as_bytes())
    }

    /// Add or update a dependency. Returns `true` if a new dep was added.
    pub fn add_dep(&mut self, name: String, spec: DependencySpec, dev: bool) -> bool {
        let map = if dev {
            &mut self.dev_dependencies
        } else {
            &mut self.dependencies
        };
        let new = !map.contains_key(&name);
        map.insert(name, spec);
        new
    }

    pub fn remove_dep(&mut self, name: &str) -> bool {
        let a = self.dependencies.remove(name).is_some();
        let b = self.dev_dependencies.remove(name).is_some();
        a || b
    }

    fn validate_detailed_dependencies(&self) -> Result<()> {
        for (section, dependencies) in [
            ("dependencies", &self.dependencies),
            ("dev-dependencies", &self.dev_dependencies),
        ] {
            for (name, spec) in dependencies {
                let DependencySpec::Detailed(dep) = spec else {
                    continue;
                };
                validate_detailed_dependency(name, section, dep)?;
            }
        }
        Ok(())
    }
}

fn validate_detailed_dependency(name: &str, section: &str, dep: &DetailedDep) -> Result<()> {
    if dep.exact {
        let git = dep
            .git
            .as_deref()
            .map(str::trim)
            .filter(|git| !git.is_empty())
            .ok_or_else(|| {
                UvrError::ManifestParse(format!(
                    "dependency `{name}` in [{section}]: `exact = true` requires a `git` source."
                ))
            })?;
        let spec = match dep.rev.as_deref() {
            Some(rev) => format!("{git}@{rev}"),
            None => git.to_string(),
        };
        let valid = if git.starts_with("forgejo::") {
            crate::registry::forgejo::parse_forgejo_parts(&spec).is_some()
        } else if git.starts_with("gitlab::") {
            crate::registry::gitlab::parse_gitlab_parts(&spec).is_some()
        } else {
            crate::registry::github::is_valid_github_repo_spec(&spec)
        };
        if !valid {
            return Err(UvrError::ManifestParse(format!(
                "dependency `{name}` in [{section}]: `exact = true` requires a valid supported \
                 git source and revision, got `{spec}`."
            )));
        }
    }

    let Some(path) = dep.subdirectory.as_deref() else {
        return Ok(());
    };
    let git = dep.git.as_deref().ok_or_else(|| {
        UvrError::ManifestParse(format!(
            "dependency `{name}` in [{section}]: `subdirectory` requires a `git` source."
        ))
    })?;
    if git.starts_with("forgejo::") || git.starts_with("gitlab::") {
        return Err(UvrError::ManifestParse(format!(
            "dependency `{name}` in [{section}]: `subdirectory` is only supported for \
             GitHub sources (`git = \"owner/repo\"`), not `{git}`."
        )));
    }
    let spec = match dep.rev.as_deref() {
        Some(rev) => format!("{git}@{rev}"),
        None => git.to_string(),
    };
    if !crate::registry::github::is_valid_github_repo_spec(&spec) {
        return Err(UvrError::ManifestParse(format!(
            "dependency `{name}` in [{section}]: `{spec}` is not a valid GitHub source for a \
             `subdirectory` package. Expected `git = \"owner/repo\"` with an optional \
             `rev = \"revision\"`."
        )));
    }
    crate::subdirectory::validate(path)
        .map_err(|e| UvrError::ManifestParse(format!("dependency `{name}` in [{section}]: {e}")))
}

fn validate_subdirectory_entry(key: &str, section: &str, inner: &toml::value::Table) -> Result<()> {
    match inner.get("subdirectory") {
        Some(value) if !value.is_str() => Err(UvrError::ManifestParse(format!(
            "dependency `{key}` in [{section}]: `subdirectory` must be a string."
        ))),
        _ => Ok(()),
    }
}

/// Parse a DCF (Debian Control File) string into a `BTreeMap<field, value>`.
/// Delegates to the shared `dcf::parse_dcf_fields` implementation.
fn parse_dcf(content: &str) -> BTreeMap<String, String> {
    crate::dcf::parse_dcf_fields(content)
}

/// Parse a comma-separated R dependency field (Imports, Suggests, Depends).
/// Returns `(package_name, DependencySpec)` pairs, skipping blank entries.
fn parse_dep_field(field: &str) -> Vec<(String, DependencySpec)> {
    let mut result = Vec::new();
    for entry in field.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, spec) = if let Some(paren) = entry.find('(') {
            let name = entry[..paren].trim().to_string();
            let inner = entry[paren + 1..entry.rfind(')').unwrap_or(entry.len())].trim();
            // Convert ">=3.0.0" or ">= 3.0.0" → ">=3.0.0"
            let version: String = inner.chars().filter(|c| !c.is_whitespace()).collect();
            (name, DependencySpec::Version(version))
        } else {
            (entry.to_string(), DependencySpec::Version("*".to_string()))
        };
        if !name.is_empty() {
            result.push((name, spec));
        }
    }
    result
}

fn match_remote_pkg_name(
    url_pkg: &str,
    dependencies: &BTreeMap<String, DependencySpec>,
    dev_dependencies: &BTreeMap<String, DependencySpec>,
) -> Option<String> {
    let known = |name: &str| dependencies.contains_key(name) || dev_dependencies.contains_key(name);
    if known(url_pkg) {
        return Some(url_pkg.to_string());
    }
    for suffix in ["-r", "_r", ".r", "-R", "_R", ".R"] {
        if let Some(stripped) = url_pkg.strip_suffix(suffix) {
            if !stripped.is_empty() && known(stripped) {
                return Some(stripped.to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteProvider {
    GitHub,
    Forgejo,
    Gitlab,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSource {
    pub name: String,
    pub explicit_name: bool,
    pub provider: RemoteProvider,
    pub repository: String,
    pub requested_ref: Option<String>,
    pub subdirectory: Option<String>,
}

impl RemoteSource {
    pub fn git_spec(&self) -> String {
        match self.provider {
            RemoteProvider::GitHub => self.repository.clone(),
            RemoteProvider::Forgejo => format!("forgejo::{}", self.repository),
            RemoteProvider::Gitlab => format!("gitlab::{}", self.repository),
        }
    }

    fn dependency_spec(&self) -> DependencySpec {
        DependencySpec::Detailed(DetailedDep {
            git: Some(self.git_spec()),
            exact: self.explicit_name,
            rev: self.requested_ref.clone(),
            subdirectory: self.subdirectory.clone(),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEntry {
    Source(RemoteSource),
    Unsupported {
        entry: String,
        reason: String,
        bound: bool,
    },
}

pub(crate) type CompatibleRemote = (String, String, Option<String>);

pub(crate) fn compatible_remote_entries(
    entries: Vec<RemoteEntry>,
) -> Result<Vec<CompatibleRemote>> {
    let mut compatible = Vec::new();
    for entry in entries {
        match entry {
            RemoteEntry::Source(source) if source.subdirectory.is_some() => {
                return Err(UvrError::Other(format!(
                    "legacy Remotes tuple cannot represent nested package '{}' from '{}'; use \
                     the rich RemoteEntry resolver API",
                    source.name,
                    remote_source_target(&source)
                )));
            }
            RemoteEntry::Source(source) => {
                let git = source.git_spec();
                compatible.push((source.name, git, source.requested_ref));
            }
            RemoteEntry::Unsupported {
                entry,
                reason,
                bound: true,
            } => {
                return Err(UvrError::Other(format!(
                    "legacy Remotes tuple cannot represent unsupported bound entry '{entry}': \
                     {reason}; use the rich RemoteEntry resolver API"
                )));
            }
            RemoteEntry::Unsupported { bound: false, .. } => {}
        }
    }
    Ok(compatible)
}

fn remote_source_target(source: &RemoteSource) -> String {
    let mut target = source.git_spec();
    if let Some(subdirectory) = &source.subdirectory {
        target.push(':');
        target.push_str(subdirectory);
    }
    if let Some(requested_ref) = &source.requested_ref {
        target.push('@');
        target.push_str(requested_ref);
    }
    target
}

pub(crate) fn parse_remotes_field_rich(field: &str) -> Vec<RemoteEntry> {
    let mut result = Vec::new();
    for entry in field
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (provider, body, explicit_name) = match split_remote_prefix_and_alias(entry) {
            Ok(Some(parts)) => parts,
            Ok(None) => continue,
            Err(reason) => {
                result.push(RemoteEntry::Unsupported {
                    entry: entry.to_string(),
                    reason,
                    bound: true,
                });
                continue;
            }
        };

        let parsed = match provider {
            RemoteProvider::GitHub => parse_github_remote(entry, body, explicit_name),
            RemoteProvider::Forgejo => parse_forgejo_remote(entry, body, explicit_name),
            RemoteProvider::Gitlab => parse_gitlab_remote(entry, body, explicit_name),
        };
        if let Some(parsed) = parsed {
            result.push(parsed);
        }
    }
    result
}

type RemotePrefixAlias<'a> = (RemoteProvider, &'a str, Option<String>);

fn split_remote_prefix_and_alias(
    entry: &str,
) -> std::result::Result<Option<RemotePrefixAlias<'_>>, String> {
    let (provider, body, alias_first) = if let Some(body) = entry.strip_prefix("github::") {
        (RemoteProvider::GitHub, body, false)
    } else if let Some(body) = entry.strip_prefix("forgejo::") {
        (RemoteProvider::Forgejo, body, false)
    } else if let Some(body) = entry.strip_prefix("gitlab::") {
        (RemoteProvider::Gitlab, body, false)
    } else {
        (RemoteProvider::GitHub, entry, true)
    };

    let (explicit_name, target) = split_remote_alias(body)?;
    if !alias_first {
        return Ok(Some((provider, target, explicit_name)));
    }

    let (provider, target) = if let Some(body) = target.strip_prefix("github::") {
        (RemoteProvider::GitHub, body)
    } else if let Some(body) = target.strip_prefix("forgejo::") {
        (RemoteProvider::Forgejo, body)
    } else if let Some(body) = target.strip_prefix("gitlab::") {
        (RemoteProvider::Gitlab, body)
    } else if target.contains("::") {
        if explicit_name.is_some() {
            return Err("the bound alias uses an unsupported remote provider".to_string());
        }
        return Ok(None);
    } else {
        (RemoteProvider::GitHub, target)
    };
    Ok(Some((provider, target, explicit_name)))
}

fn split_remote_alias(body: &str) -> std::result::Result<(Option<String>, &str), String> {
    let Some((name, target)) = body.split_once('=') else {
        return Ok((None, body.trim()));
    };
    if name.contains('/') {
        return Ok((None, body.trim()));
    }
    let name = name.trim();
    if !crate::package_name::is_valid(name) {
        return Err(format!("'{name}' is not a valid explicit package alias"));
    }
    if !target.contains('/') {
        return Err(format!(
            "the bound alias '{name}' has a malformed remote target"
        ));
    }
    Ok((Some(name.to_string()), target.trim()))
}

fn github_remote_is_bound(explicit_name: bool, target: &str) -> bool {
    if explicit_name {
        return true;
    }
    let path = target.split(['@', '#']).next().unwrap_or(target).trim();
    let Some((_owner, tail)) = path.split_once('/') else {
        return false;
    };
    tail.contains('/') || tail.contains(':')
}

fn unsupported_remote(entry: &str, reason: impl Into<String>, bound: bool) -> RemoteEntry {
    RemoteEntry::Unsupported {
        entry: entry.to_string(),
        reason: reason.into(),
        bound,
    }
}

fn parse_github_remote(
    entry: &str,
    target: &str,
    explicit_name: Option<String>,
) -> Option<RemoteEntry> {
    let bound = github_remote_is_bound(explicit_name.is_some(), target);
    if target.contains('#') {
        return Some(unsupported_remote(
            entry,
            "GitHub pull-request revisions (`#<number>`) are not supported",
            bound,
        ));
    }

    let (path, requested_ref) = match target.split_once('@') {
        Some((path, requested_ref))
            if crate::registry::github::is_valid_git_ref(requested_ref.trim()) =>
        {
            (path.trim(), Some(requested_ref.trim().to_string()))
        }
        Some(_) => {
            return Some(unsupported_remote(
                entry,
                "the GitHub revision is empty, malformed, or unsupported",
                bound,
            ));
        }
        None => (target.trim(), None),
    };

    let Some((owner, tail)) = path.split_once('/') else {
        return explicit_name
            .map(|_| unsupported_remote(entry, "expected a GitHub owner/repository path", true));
    };
    let (repo, subdirectory) = if let Some((repo, subdirectory)) = tail.split_once(':') {
        if repo.contains('/') {
            return Some(unsupported_remote(
                entry,
                "the canonical colon form is owner/repository:subdirectory",
                true,
            ));
        }
        (repo, Some(subdirectory))
    } else if let Some((repo, subdirectory)) = tail.split_once('/') {
        (repo, Some(subdirectory))
    } else {
        (tail, None)
    };

    let repository = format!("{owner}/{repo}");
    if !crate::registry::github::is_valid_github_repo_spec(&repository) {
        return Some(unsupported_remote(
            entry,
            "the GitHub owner/repository path is malformed",
            bound,
        ));
    }
    let subdirectory = match subdirectory {
        Some(path) => match crate::subdirectory::validate(path) {
            Ok(()) => Some(path.to_string()),
            Err(error) => return Some(unsupported_remote(entry, error.to_string(), true)),
        },
        None => None,
    };
    let name = explicit_name.clone().unwrap_or_else(|| {
        subdirectory
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .unwrap_or(repo)
            .to_string()
    });

    Some(RemoteEntry::Source(RemoteSource {
        name,
        explicit_name: explicit_name.is_some(),
        provider: RemoteProvider::GitHub,
        repository,
        requested_ref,
        subdirectory,
    }))
}

fn non_github_remote_is_bound(provider: RemoteProvider, explicit_name: bool, target: &str) -> bool {
    if explicit_name {
        return true;
    }
    let path = target.split(['@', '#']).next().unwrap_or(target).trim();
    let Some((_host, project)) = path.split_once('/') else {
        return false;
    };
    if project.contains(':') {
        return true;
    }
    provider == RemoteProvider::Forgejo && project.split('/').count() > 2
}

fn parse_remote_path_and_ref<'a>(
    entry: &str,
    target: &'a str,
    provider: &str,
    bound: bool,
) -> std::result::Result<(&'a str, Option<String>), RemoteEntry> {
    if target.contains('#') {
        return Err(unsupported_remote(
            entry,
            format!("{provider} `#` revision and pull-request forms are not supported"),
            bound,
        ));
    }
    match target.split_once('@') {
        Some((path, requested_ref))
            if crate::registry::github::is_valid_git_ref(requested_ref.trim()) =>
        {
            Ok((path.trim(), Some(requested_ref.trim().to_string())))
        }
        Some(_) => Err(unsupported_remote(
            entry,
            format!("the {provider} revision is empty, malformed, or unsupported"),
            bound,
        )),
        None => Ok((target.trim(), None)),
    }
}

fn parse_forgejo_remote(
    entry: &str,
    body: &str,
    explicit_name: Option<String>,
) -> Option<RemoteEntry> {
    let bound = non_github_remote_is_bound(RemoteProvider::Forgejo, explicit_name.is_some(), body);
    let (path, requested_ref) = match parse_remote_path_and_ref(entry, body, "Forgejo", bound) {
        Ok(parts) => parts,
        Err(unsupported) => return Some(unsupported),
    };
    let Some(parsed) = crate::registry::forgejo::parse_forgejo_parts(path) else {
        return Some(unsupported_remote(
            entry,
            "the Forgejo target is malformed or uses an unsupported package directory",
            bound,
        ));
    };
    Some(RemoteEntry::Source(RemoteSource {
        name: explicit_name.clone().unwrap_or_else(|| parsed.repo.clone()),
        explicit_name: explicit_name.is_some(),
        provider: RemoteProvider::Forgejo,
        repository: format!("{}/{}/{}", parsed.host, parsed.owner, parsed.repo),
        requested_ref,
        subdirectory: None,
    }))
}

fn parse_gitlab_remote(
    entry: &str,
    body: &str,
    explicit_name: Option<String>,
) -> Option<RemoteEntry> {
    let path = body.split(['@', '#']).next().unwrap_or(body).trim();
    if path.contains("/-/") {
        return Some(unsupported_remote(
            entry,
            "GitLab package directories (`/-/<path>`) are unsupported; package-directory targets are only supported on GitHub",
            true,
        ));
    }

    let bound = non_github_remote_is_bound(RemoteProvider::Gitlab, explicit_name.is_some(), body);
    let (path, requested_ref) = match parse_remote_path_and_ref(entry, body, "GitLab", bound) {
        Ok(parts) => parts,
        Err(unsupported) => return Some(unsupported),
    };
    let Some(parsed) = crate::registry::gitlab::parse_gitlab_parts(path) else {
        return Some(unsupported_remote(
            entry,
            "the GitLab target is malformed or uses an unsupported package directory",
            bound,
        ));
    };
    Some(RemoteEntry::Source(RemoteSource {
        name: explicit_name
            .clone()
            .unwrap_or_else(|| parsed.project_name().to_string()),
        explicit_name: explicit_name.is_some(),
        provider: RemoteProvider::Gitlab,
        repository: format!("{}/{}", parsed.host, parsed.project_path),
        requested_ref,
        subdirectory: None,
    }))
}

/// Extract R version constraint from a `Depends:` field value.
/// e.g. `"R (>= 4.0.0), methods"` → `Some(">=4.0.0")`
fn parse_r_version_from_depends(depends: &str) -> Option<String> {
    for entry in depends.split(',') {
        let entry = entry.trim();
        if let Some(stripped) = entry.strip_prefix('R') {
            let rest = stripped.trim();
            if rest.is_empty() || rest.starts_with('(') {
                if let Some(paren) = entry.find('(') {
                    let inner = entry[paren + 1..entry.rfind(')').unwrap_or(entry.len())].trim();
                    let version: String = inner.chars().filter(|c| !c.is_whitespace()).collect();
                    if !version.is_empty() {
                        return Some(version);
                    }
                }
                return None;
            }
        }
    }
    None
}

/// Write `data` to `path` atomically via a temp file in the same directory.
/// Uses `tempfile::NamedTempFile` for a unique temp name, then renames.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.persist(path)
        .map_err(|e| crate::error::UvrError::Io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[project]
name = "my-project"
r_version = ">=4.0.0"

[dependencies]
ggplot2 = ">=3.0.0"
dplyr = "*"

[dependencies.DESeq2]
bioc = true

[dependencies.myPkg]
git = "user/repo"
rev = "main"
"#;

    #[test]
    fn round_trip() {
        let m: Manifest = SAMPLE.parse().expect("parse");
        assert_eq!(m.project.name, "my-project");
        assert_eq!(m.project.r_version.as_deref(), Some(">=4.0.0"));

        let ggplot2 = m.dependencies.get("ggplot2").unwrap();
        assert!(matches!(ggplot2, DependencySpec::Version(v) if v == ">=3.0.0"));

        let deseq2 = m.dependencies.get("DESeq2").unwrap();
        assert!(deseq2.is_bioc());

        let my_pkg = m.dependencies.get("myPkg").unwrap();
        assert_eq!(my_pkg.git(), Some("user/repo"));

        // Re-serialize and re-parse
        let toml_str = m.to_toml_string().expect("serialize");
        let m2: Manifest = toml_str.parse().expect("reparse");
        assert_eq!(m, m2);
    }

    const DESCRIPTION_SAMPLE: &str = r#"Package: myanalysis
Title: My Analysis Project
Version: 0.1.0
Depends:
    R (>= 4.1.0),
    methods
Imports:
    ggplot2 (>= 3.4.0),
    dplyr,
    stringr
Suggests:
    testthat (>= 3.0.0),
    knitr
"#;

    #[test]
    fn description_basic() {
        let m = Manifest::from_description_str(DESCRIPTION_SAMPLE).expect("parse");
        assert_eq!(m.project.name, "myanalysis");
        assert_eq!(m.project.r_version.as_deref(), Some(">=4.1.0"));
        assert_eq!(
            m.project.description.as_deref(),
            Some("My Analysis Project")
        );
    }

    #[test]
    fn description_imports_as_deps() {
        let m = Manifest::from_description_str(DESCRIPTION_SAMPLE).expect("parse");
        // ggplot2 with version constraint
        let gg = m.dependencies.get("ggplot2").unwrap();
        assert!(matches!(gg, DependencySpec::Version(v) if v == ">=3.4.0"));
        // dplyr without version
        let dp = m.dependencies.get("dplyr").unwrap();
        assert!(matches!(dp, DependencySpec::Version(v) if v == "*"));
        // methods from Depends (non-R entry)
        assert!(m.dependencies.contains_key("methods"));
    }

    #[test]
    fn description_suggests_as_dev_deps() {
        let m = Manifest::from_description_str(DESCRIPTION_SAMPLE).expect("parse");
        let tt = m.dev_dependencies.get("testthat").unwrap();
        assert!(matches!(tt, DependencySpec::Version(v) if v == ">=3.0.0"));
        assert!(m.dev_dependencies.contains_key("knitr"));
    }

    #[test]
    fn description_no_r_in_deps() {
        let m = Manifest::from_description_str(DESCRIPTION_SAMPLE).expect("parse");
        assert!(!m.dependencies.contains_key("R"));
        assert!(!m.dev_dependencies.contains_key("R"));
    }

    #[test]
    fn description_remotes_override_imports_with_git_source() {
        let dcf = r#"Package: AQmap
Imports:
    airquality,
    dplyr,
    handyr
Remotes:
    B-Nilson/airquality,
    github::B-Nilson/handyr@dev,
    gitlab::other/thing,
    gitlab::gitlab.com/my-group/mypkg@v1.0,
    pkg=user/repo@v1.0
"#;
        let m = Manifest::from_description_str(dcf).expect("parse");

        // airquality: bare user/repo
        let aq = m.dependencies.get("airquality").unwrap();
        assert_eq!(aq.git(), Some("B-Nilson/airquality"));

        // handyr: github:: prefix + @ref
        let hr = m.dependencies.get("handyr").unwrap();
        assert_eq!(hr.git(), Some("B-Nilson/handyr"));
        if let DependencySpec::Detailed(d) = hr {
            assert_eq!(d.rev.as_deref(), Some("dev"));
        } else {
            panic!("handyr should be a detailed git dep");
        }

        // dplyr: not in Remotes, stays as "*"
        let dp = m.dependencies.get("dplyr").unwrap();
        assert!(matches!(dp, DependencySpec::Version(v) if v == "*"));

        // gitlab:: is parsed, but a real spec needs host/group/project at
        // minimum — "other/thing" has only two segments (host + project,
        // no group), so it fails validation and "thing" correctly does
        // not appear as a dep.
        assert!(!m.dependencies.contains_key("thing"));

        // gitlab:: with a valid host/group/project shape does parse.
        let ml = m.dependencies.get("mypkg").unwrap();
        assert_eq!(ml.git(), Some("gitlab::gitlab.com/my-group/mypkg"));
        if let DependencySpec::Detailed(d) = ml {
            assert_eq!(d.rev.as_deref(), Some("v1.0"));
        } else {
            panic!("mypkg should be a detailed git dep");
        }

        // pkg=user/repo → explicit pkg name with @ref
        let pk = m.dependencies.get("pkg").unwrap();
        assert_eq!(pk.git(), Some("user/repo"));
        if let DependencySpec::Detailed(d) = pk {
            assert_eq!(d.rev.as_deref(), Some("v1.0"));
        }
    }

    #[test]
    fn description_remotes_override_preserves_version_constraint() {
        // #132 — a Remotes entry declares the *source* for a dep, not its
        // version. The constraint from the Imports line must survive the
        // override so the resolver's pre-resolved check can still validate
        // the git version against it.
        let dcf = r#"Package: thing
Imports:
    foo (>= 2.0.0),
    bar
Remotes:
    user/foo,
    user/bar
"#;
        let m = Manifest::from_description_str(dcf).expect("parse");

        let foo = m.dependencies.get("foo").unwrap();
        assert_eq!(foo.git(), Some("user/foo"));
        assert_eq!(
            foo.version_req(),
            Some(">=2.0.0"),
            "Imports version constraint should survive the Remotes override"
        );

        // Unconstrained ("*") deps should not grow a version field.
        let bar = m.dependencies.get("bar").unwrap();
        assert_eq!(bar.git(), Some("user/bar"));
        assert!(bar.version_req().is_none());
    }

    #[test]
    fn description_remotes_companion_suffix_binds_to_runtime_dep() {
        // Companion #2 to the Suggests-side test: when the existing dep
        // came from `Imports:` (runtime), the git source should land on
        // dependencies, not dev_dependencies. Reviewer flagged this code
        // path as untested — different target-selection branch.
        let dcf = r#"Package: thing
Imports:
    uvr (>= 0.1.0)
Remotes:
    nbafrank/uvr-r
"#;
        let m = Manifest::from_description_str(dcf).expect("parse");
        assert!(
            !m.dependencies.contains_key("uvr-r"),
            "uvr-r should not be a runtime dep, got {:?}",
            m.dependencies
        );
        assert!(
            !m.dev_dependencies.contains_key("uvr"),
            "uvr should not have moved to dev-deps, got {:?}",
            m.dev_dependencies
        );
        let uvr = m
            .dependencies
            .get("uvr")
            .expect("uvr should remain a runtime dep");
        assert_eq!(uvr.git(), Some("nbafrank/uvr-r"));
    }

    #[test]
    fn description_remotes_companion_suffix_binds_to_existing_dep() {
        // #68 — `Remotes: nbafrank/uvr-r` paired with `Suggests: uvr` should
        // bind to the `uvr` dev-dep, not create a new `uvr-r` runtime dep.
        let dcf = r#"Package: templates
Suggests:
    uvr (>= 0.1.0)
Remotes:
    nbafrank/uvr-r
"#;
        let m = Manifest::from_description_str(dcf).expect("parse");

        // No spurious runtime dep was created.
        assert!(
            !m.dependencies.contains_key("uvr-r"),
            "uvr-r should not be a runtime dep, got {:?}",
            m.dependencies
        );

        // The Suggests entry survives as a dev-dep with git source merged in.
        let uvr = m
            .dev_dependencies
            .get("uvr")
            .expect("uvr should remain a dev-dep");
        assert_eq!(uvr.git(), Some("nbafrank/uvr-r"));
    }

    #[test]
    fn description_remotes_underscore_r_suffix_binds() {
        let dcf = r#"Package: thing
Imports:
    foo
Remotes:
    user/foo_r
"#;
        let m = Manifest::from_description_str(dcf).expect("parse");
        assert!(!m.dependencies.contains_key("foo_r"));
        assert_eq!(m.dependencies.get("foo").unwrap().git(), Some("user/foo_r"));
    }

    #[test]
    fn description_remotes_unmatched_falls_back_to_url_name() {
        let dcf = r#"Package: thing
Imports:
    foo
Remotes:
    user/totally-unrelated
"#;
        let m = Manifest::from_description_str(dcf).expect("parse");
        assert_eq!(
            m.dependencies.get("totally-unrelated").unwrap().git(),
            Some("user/totally-unrelated")
        );
        let foo = m.dependencies.get("foo").unwrap();
        assert!(foo.git().is_none(), "foo should remain version-only");
    }

    #[test]
    fn description_import_rejects_bound_unsupported_alias() {
        let error = Manifest::from_description_str(
            "Package: parent\nImports: Alias\n\
             Remotes: Alias=url::https://example.com/pkg.tar.gz\n",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unsupported bound Remotes entry"), "{error}");
        assert!(error.contains("Alias=url::"), "{error}");
    }

    #[test]
    fn description_import_skips_unbound_unsupported_root_hint() {
        let manifest = Manifest::from_description_str(
            "Package: parent\nImports: root\nRemotes: owner/root#42\n",
        )
        .unwrap();
        assert_eq!(
            manifest.dependencies.get("root"),
            Some(&DependencySpec::Version("*".to_string()))
        );
    }

    #[test]
    fn description_import_rejects_unmatched_unaliased_nested_remote() {
        let error = Manifest::from_description_str(
            "Package: parent\nImports: ActualPackage\n\
             Remotes: owner/mono/packages/different\n",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unaliased nested Remotes entry"), "{error}");
        assert!(error.contains("PackageName="), "{error}");
    }

    #[test]
    fn description_import_matches_unaliased_nested_remote_to_declared_dependency() {
        let manifest = Manifest::from_description_str(
            "Package: parent\nImports: NestedPkg\n\
             Remotes: owner/mono/packages/NestedPkg@main\n",
        )
        .unwrap();
        let dependency = manifest.dependencies.get("NestedPkg").unwrap();
        assert_eq!(dependency.git(), Some("owner/mono"));
        assert_eq!(dependency.subdirectory(), Some("packages/NestedPkg"));
    }

    #[test]
    fn description_differing_root_alias_survives_manifest_round_trip() {
        let imported = Manifest::from_description_str(
            "Package: parent\nImports: Alias\nRemotes: Alias=owner/repo@main\n",
        )
        .unwrap();
        let serialized = imported.to_toml_string().unwrap();
        let reparsed: Manifest = serialized.parse().unwrap();
        let dependency = reparsed.dependencies.get("Alias").unwrap();
        assert_eq!(dependency.git(), Some("owner/repo"));
        assert_eq!(dependency.version_req(), None);
        let DependencySpec::Detailed(dependency) = dependency else {
            panic!("expected detailed dependency");
        };
        assert!(dependency.exact);
        assert_eq!(dependency.rev.as_deref(), Some("main"));
    }

    #[test]
    fn description_same_name_root_alias_survives_manifest_round_trip() {
        let imported = Manifest::from_description_str(
            "Package: parent\nImports: repo\nRemotes: repo=owner/repo@main\n",
        )
        .unwrap();
        let serialized = imported.to_toml_string().unwrap();
        assert!(serialized.contains("exact = true"), "{serialized}");
        let reparsed: Manifest = serialized.parse().unwrap();
        let DependencySpec::Detailed(dependency) = reparsed.dependencies.get("repo").unwrap()
        else {
            panic!("expected detailed dependency");
        };
        assert!(dependency.exact);
    }

    #[test]
    fn description_unaliased_root_remains_inexact() {
        let imported = Manifest::from_description_str(
            "Package: parent\nImports: repo\nRemotes: owner/repo@main\n",
        )
        .unwrap();
        let DependencySpec::Detailed(dependency) = imported.dependencies.get("repo").unwrap()
        else {
            panic!("expected detailed dependency");
        };
        assert!(!dependency.exact);
        assert!(!imported.to_toml_string().unwrap().contains("exact ="));
    }

    #[test]
    fn description_explicit_alias_marker_is_provider_neutral() {
        let imported = Manifest::from_description_str(
            "Package: parent\nImports: GithubAlias, ForgejoAlias, GitlabAlias\n\
             Remotes: GithubAlias=owner/repo, \
             ForgejoAlias=forgejo::code.example/team/repo, \
             GitlabAlias=gitlab::gitlab.example/group/repo\n",
        )
        .unwrap();
        for name in ["GithubAlias", "ForgejoAlias", "GitlabAlias"] {
            let DependencySpec::Detailed(dependency) = imported.dependencies.get(name).unwrap()
            else {
                panic!("expected detailed dependency for {name}");
            };
            assert!(dependency.exact, "explicit alias marker missing for {name}");
        }
    }

    #[test]
    fn description_without_package_yields_empty_name() {
        let dcf = "Imports:\n    dplyr\n";
        let m = Manifest::from_description_str(dcf).expect("parse");
        assert_eq!(m.project.name, "");
    }

    #[test]
    fn bioc_version_round_trip() {
        let toml = r#"
[project]
name = "bioc-test"
r_version = ">=4.3.0"
bioc_version = "3.18"

[dependencies.DESeq2]
bioc = true
"#;
        let m: Manifest = toml.parse().expect("parse");
        assert_eq!(m.project.bioc_version.as_deref(), Some("3.18"));

        let serialized = m.to_toml_string().expect("serialize");
        let m2: Manifest = serialized.parse().expect("reparse");
        assert_eq!(m, m2);
    }

    #[test]
    fn bioc_version_omitted() {
        // bioc_version should be None when not specified (backward compat)
        let m: Manifest = SAMPLE.parse().expect("parse");
        assert!(m.project.bioc_version.is_none());
        // And not serialized
        let s = m.to_toml_string().expect("serialize");
        assert!(!s.contains("bioc_version"));
    }

    #[test]
    fn description_no_depends() {
        let content = "Package: minimal\nImports: ggplot2\n";
        let m = Manifest::from_description_str(content).expect("parse");
        assert_eq!(m.project.name, "minimal");
        assert!(m.project.r_version.is_none());
        assert!(m.dependencies.contains_key("ggplot2"));
    }

    #[test]
    fn add_remove_dep() {
        let mut m = Manifest::new("test", None);
        assert!(m.add_dep("ggplot2".into(), DependencySpec::Version("*".into()), false));
        assert!(!m.add_dep(
            "ggplot2".into(),
            DependencySpec::Version(">=3.0.0".into()),
            false
        ));
        assert!(m.remove_dep("ggplot2"));
        assert!(!m.remove_dep("ggplot2"));
    }

    #[test]
    fn parse_remotes_field_keeps_forgejo_and_gitlab() {
        let field = "forgejo::codefloe.com/pat-s/mypkg@v0.1.0, github::user/a, \
                     gitlab::other/x, gitlab::gitlab.com/my-group/my-sub/thing@v2.0";
        let v = parse_remotes_field_rich(field);
        let sources: Vec<&RemoteSource> = v
            .iter()
            .filter_map(|entry| match entry {
                RemoteEntry::Source(source) => Some(source),
                RemoteEntry::Unsupported { .. } => None,
            })
            .collect();
        let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
        // "other/x" has only two segments (host + project, no group) —
        // not a valid gitlab spec — so it's dropped, not because gitlab
        // is unsupported.
        assert_eq!(names, vec!["mypkg", "a", "thing"]);

        // The forgejo entry stores the full `forgejo::host/owner/repo` in
        // the `git` field, with the ref split into `rev`.
        assert_eq!(sources[0].git_spec(), "forgejo::codefloe.com/pat-s/mypkg");
        assert_eq!(sources[0].requested_ref.as_deref(), Some("v0.1.0"));

        // The gitlab entry keeps its nested subgroup and revision separately.
        assert_eq!(
            sources[2].git_spec(),
            "gitlab::gitlab.com/my-group/my-sub/thing"
        );
        assert_eq!(sources[2].requested_ref.as_deref(), Some("v2.0"));
    }

    #[test]
    fn github_remotes_keep_repository_ref_and_subdirectory_separate() {
        let entries = parse_remotes_field_rich(
            "SlashPkg=owner/mono/packages/slash@feature/x, \
             ColonPkg=github::owner/mono:packages/colon/more@v2.0, \
             github::LegacyPkg=owner/mono/r/legacy, owner/mono/r/inferred",
        );
        let sources: Vec<&RemoteSource> = entries
            .iter()
            .map(|entry| match entry {
                RemoteEntry::Source(source) => source,
                other => panic!("expected source, got {other:?}"),
            })
            .collect();
        assert_eq!(sources.len(), 4);

        assert_eq!(sources[0].name, "SlashPkg");
        assert!(sources[0].explicit_name);
        assert_eq!(sources[0].provider, RemoteProvider::GitHub);
        assert_eq!(sources[0].repository, "owner/mono");
        assert_eq!(sources[0].requested_ref.as_deref(), Some("feature/x"));
        assert_eq!(sources[0].subdirectory.as_deref(), Some("packages/slash"));

        assert_eq!(sources[1].name, "ColonPkg");
        assert_eq!(sources[1].repository, "owner/mono");
        assert_eq!(sources[1].requested_ref.as_deref(), Some("v2.0"));
        assert_eq!(
            sources[1].subdirectory.as_deref(),
            Some("packages/colon/more")
        );

        assert_eq!(sources[2].name, "LegacyPkg");
        assert_eq!(sources[2].subdirectory.as_deref(), Some("r/legacy"));
        assert_eq!(sources[3].name, "inferred");
        assert!(!sources[3].explicit_name);
        assert_eq!(sources[3].subdirectory.as_deref(), Some("r/inferred"));
    }

    #[test]
    fn compatibility_remotes_adapter_rejects_nested_entries() {
        let error = compatible_remote_entries(parse_remotes_field_rich(
            "owner/root@main, github::owner/mono/pkgs/nested@v1",
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("cannot represent nested package"), "{error}");
    }

    #[test]
    fn compatibility_remotes_adapter_rejects_bound_unsupported_entries() {
        let error = compatible_remote_entries(parse_remotes_field_rich(
            "Alias=forgejo::code.example/team/pkg@",
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("unsupported bound entry"), "{error}");
        assert!(error.contains("revision"), "{error}");
    }

    #[test]
    fn compatibility_remotes_adapter_skips_unbound_unsupported_entries() {
        let compatible =
            compatible_remote_entries(parse_remotes_field_rich("owner/root#42")).unwrap();
        assert!(compatible.is_empty());
    }

    #[test]
    fn compatibility_remotes_adapter_keeps_representable_roots() {
        let compatible = compatible_remote_entries(parse_remotes_field_rich(
            "owner/root@main, Alias=forgejo::code.example/team/pkg@dev, \
             gitlab::gitlab.example/group/project@release",
        ))
        .unwrap();
        assert_eq!(
            compatible,
            vec![
                (
                    "root".to_string(),
                    "owner/root".to_string(),
                    Some("main".to_string())
                ),
                (
                    "Alias".to_string(),
                    "forgejo::code.example/team/pkg".to_string(),
                    Some("dev".to_string())
                ),
                (
                    "project".to_string(),
                    "gitlab::gitlab.example/group/project".to_string(),
                    Some("release".to_string())
                ),
            ]
        );
    }

    #[test]
    fn malformed_or_unsupported_github_remotes_record_binding() {
        let entries = parse_remotes_field_rich(
            "owner/root#42, Alias=owner/root@*release, \
             owner/mono/pkgs/nested#7, owner/mono/pkgs/../escape, \
             AliasUrl=url::https://example.com/pkg.tar.gz, \
             AliasBad=not-a-remote, url::https://example.com/pkg.tar.gz",
        );
        assert_eq!(entries.len(), 6);
        let unsupported: Vec<(&str, bool)> = entries
            .iter()
            .map(|entry| match entry {
                RemoteEntry::Unsupported { entry, bound, .. } => (entry.as_str(), *bound),
                other => panic!("expected unsupported entry, got {other:?}"),
            })
            .collect();
        assert_eq!(unsupported[0], ("owner/root#42", false));
        assert_eq!(unsupported[1], ("Alias=owner/root@*release", true));
        assert_eq!(unsupported[2], ("owner/mono/pkgs/nested#7", true));
        assert_eq!(unsupported[3], ("owner/mono/pkgs/../escape", true));
        assert_eq!(
            unsupported[4],
            ("AliasUrl=url::https://example.com/pkg.tar.gz", true)
        );
        assert_eq!(unsupported[5], ("AliasBad=not-a-remote", true));
    }

    #[test]
    fn explicit_aliases_are_preserved_for_all_git_providers() {
        let entries = parse_remotes_field_rich(
            "GitHubAlias=github::owner/repo@main, \
             ForgejoAlias=forgejo::code.example/team/repo@dev, \
             GitlabAlias=gitlab::gitlab.example/group/repo@release",
        );
        let sources: Vec<&RemoteSource> = entries
            .iter()
            .map(|entry| match entry {
                RemoteEntry::Source(source) => source,
                other => panic!("expected source, got {other:?}"),
            })
            .collect();
        assert_eq!(sources.len(), 3);
        for source in sources {
            assert!(source.explicit_name);
        }
    }

    #[test]
    fn unaliased_gitlab_package_directory_is_bound_unsupported() {
        let entries = parse_remotes_field_rich("gitlab::host/group/repo/-/subdir");
        assert!(matches!(
            entries.as_slice(),
            [RemoteEntry::Unsupported {
                bound: true,
                reason,
                ..
            }] if reason.contains("package directories")
        ));
    }

    #[test]
    fn aliased_gitlab_package_directory_is_bound_unsupported() {
        let entries = parse_remotes_field_rich("Alias=gitlab::host/group/repo/-/subdir");
        assert!(matches!(
            entries.as_slice(),
            [RemoteEntry::Unsupported {
                bound: true,
                reason,
                ..
            }] if reason.contains("package directories")
        ));
    }

    #[test]
    fn malformed_explicit_aliases_fail_closed_for_all_git_providers() {
        let entries = parse_remotes_field_rich(
            "GithubEmpty=owner/repo@, GithubHash=owner/repo@main#7, \
             ForgejoEmpty=forgejo::code.example/team/repo@, \
             ForgejoHash=forgejo::code.example/team/repo@main#frag, \
             ForgejoColon=forgejo::code.example/team/repo:path, \
             ForgejoSlash=forgejo::code.example/team/repo/path, \
             GitlabEmpty=gitlab::gitlab.example/group/repo@, \
             GitlabHash=gitlab::gitlab.example/group/repo@main#frag, \
             GitlabColon=gitlab::gitlab.example/group/repo:path",
        );
        assert_eq!(entries.len(), 9);
        for entry in entries {
            match entry {
                RemoteEntry::Unsupported { bound: true, .. } => {}
                other => panic!("expected bound unsupported entry, got {other:?}"),
            }
        }
    }

    #[test]
    fn malformed_unbound_root_hints_remain_unbound_for_all_git_providers() {
        let entries = parse_remotes_field_rich(
            "owner/repo@, forgejo::code.example/team/repo@, \
             gitlab::gitlab.example/group/repo@",
        );
        assert_eq!(entries.len(), 3);
        for entry in entries {
            match entry {
                RemoteEntry::Unsupported { bound: false, .. } => {}
                other => panic!("expected unbound unsupported entry, got {other:?}"),
            }
        }
    }

    #[test]
    fn remote_parser_applies_alias_provider_fragment_ref_and_directory_order() {
        let entries = parse_remotes_field_rich(
            "Alias=github::owner/mono:packages/nested@feature/x, \
             Rejected=github::owner/mono:packages/nested@feature/x#12",
        );
        let RemoteEntry::Source(source) = &entries[0] else {
            panic!("expected source");
        };
        assert_eq!(source.name, "Alias");
        assert_eq!(source.repository, "owner/mono");
        assert_eq!(source.subdirectory.as_deref(), Some("packages/nested"));
        assert_eq!(source.requested_ref.as_deref(), Some("feature/x"));
        assert!(matches!(
            &entries[1],
            RemoteEntry::Unsupported { bound: true, .. }
        ));
    }

    #[test]
    fn description_colon_remote_preserves_dependency_constraint_and_directory() {
        let manifest = Manifest::from_description_str(
            "Package: parent\nImports: NestedPkg (>= 1.2.0)\n\
             Remotes: NestedPkg=owner/mono:packages/nested@main\n",
        )
        .unwrap();
        let DependencySpec::Detailed(dependency) = manifest.dependencies.get("NestedPkg").unwrap()
        else {
            panic!("NestedPkg should be a detailed dependency");
        };
        assert_eq!(dependency.git.as_deref(), Some("owner/mono"));
        assert_eq!(dependency.rev.as_deref(), Some("main"));
        assert_eq!(dependency.subdirectory.as_deref(), Some("packages/nested"));
        assert_eq!(dependency.version.as_deref(), Some(">=1.2.0"));
    }

    // --- Dotted-key trap detection ---

    #[test]
    fn dotted_key_single_dot_is_rejected() {
        // `[dependencies.data.table]` → TOML key `data`, sub-key `table`
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dependencies.data.table]
version = "*"
"#;
        let err = toml.parse::<Manifest>().unwrap_err().to_string();
        assert!(
            err.contains("data") && err.contains("table"),
            "expected dotted-key error mentioning 'data' and 'table', got: {err}"
        );
        assert!(
            err.contains("dotted TOML key") || err.contains("sub-key"),
            "expected actionable error message, got: {err}"
        );
    }

    #[test]
    fn dotted_key_r_prefix_is_rejected() {
        // `[dependencies.R.utils]` → key `R`, sub-key `utils`
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dependencies.R.utils]
version = "*"
"#;
        let err = toml.parse::<Manifest>().unwrap_err().to_string();
        assert!(
            err.contains("utils"),
            "expected mention of 'utils', got: {err}"
        );
    }

    #[test]
    fn dotted_key_bioc_flag_is_rejected() {
        // `[dependencies.org.Hs.eg.db]` with `bioc = true` silently drops flag.
        // The error must name the *full* package (`org.Hs.eg.db`), not just the
        // first two segments (`org.Hs`), so the user can follow the advice verbatim.
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dependencies.org.Hs.eg.db]
bioc = true
"#;
        let err = toml.parse::<Manifest>().unwrap_err().to_string();
        assert!(
            err.contains("org.Hs.eg.db"),
            "error must name the full package 'org.Hs.eg.db', got: {err}"
        );
        assert!(
            err.contains("uvr add org.Hs.eg.db"),
            "error must suggest 'uvr add org.Hs.eg.db', got: {err}"
        );
    }

    #[test]
    fn dotted_key_four_segments_reconstructed() {
        // TxDb.Hsapiens.UCSC.hg38.knownGene — five segments, common Bioc annotation package.
        // The advice must name the full package, not just the first two segments.
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dependencies.TxDb.Hsapiens.UCSC.hg38.knownGene]
bioc = true
"#;
        let err = toml.parse::<Manifest>().unwrap_err().to_string();
        assert!(
            err.contains("TxDb.Hsapiens.UCSC.hg38.knownGene"),
            "error must name the full package, got: {err}"
        );
        assert!(
            err.contains("uvr add TxDb.Hsapiens.UCSC.hg38.knownGene"),
            "error must suggest correct uvr add command, got: {err}"
        );
    }

    #[test]
    fn dotted_key_dev_dependencies_is_rejected() {
        // Same trap applies to [dev-dependencies]
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dev-dependencies.shiny.test]
version = "*"
"#;
        let err = toml.parse::<Manifest>().unwrap_err().to_string();
        assert!(err.contains("shiny") || err.contains("test"), "got: {err}");
    }

    #[test]
    fn quoted_dot_package_names_are_accepted() {
        // Quoted keys `"data.table"`, `"R.utils"`, `"org.Hs.eg.db"` must parse fine.
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dependencies]
"data.table" = "*"
"R.utils" = { version = "*" }
"shiny.i18n" = "*"

[dependencies."org.Hs.eg.db"]
bioc = true
"#;
        let m: Manifest = toml.parse().expect("quoted dot-names must parse");
        assert!(
            m.dependencies.contains_key("data.table"),
            "data.table missing"
        );
        assert!(m.dependencies.contains_key("R.utils"), "R.utils missing");
        assert!(
            m.dependencies.contains_key("shiny.i18n"),
            "shiny.i18n missing"
        );
        let org = m.dependencies.get("org.Hs.eg.db").unwrap();
        assert!(org.is_bioc(), "org.Hs.eg.db should be bioc=true");
    }

    #[test]
    fn subdirectory_round_trip() {
        let toml = r#"
[project]
name = "test"

[dependencies.nested]
git = "owner/repo"
rev = "main"
subdirectory = "pkgs/nested"

[dependencies.rooted]
git = "owner/other"
"#;
        let m: Manifest = toml.parse().expect("parse");
        let nested = m.dependencies.get("nested").unwrap();
        assert_eq!(nested.git(), Some("owner/repo"));
        assert_eq!(nested.subdirectory(), Some("pkgs/nested"));
        assert_eq!(m.dependencies.get("rooted").unwrap().subdirectory(), None);

        let serialized = m.to_toml_string().expect("serialize");
        assert!(serialized.contains(r#"subdirectory = "pkgs/nested""#));
        let m2: Manifest = serialized.parse().expect("reparse");
        assert_eq!(m, m2);
    }

    #[test]
    fn subdirectory_inline_table_parses() {
        let toml = r#"
[project]
name = "test"

[dependencies]
nested = { git = "owner/repo", subdirectory = "pkgs/nested" }
"#;
        let m: Manifest = toml.parse().expect("parse");
        assert_eq!(
            m.dependencies.get("nested").unwrap().subdirectory(),
            Some("pkgs/nested")
        );
    }

    #[test]
    fn subdirectory_rejects_unsafe_paths() {
        for bad in ["../escape", "/abs", "a\\\\b", "", "a//b", "."] {
            let toml = format!(
                "[project]\nname = \"t\"\n\n[dependencies.nested]\ngit = \"owner/repo\"\n\
                 subdirectory = \"{bad}\"\n"
            );
            let parsed = toml.parse::<Manifest>();
            assert!(parsed.is_err(), "should reject {bad}");
            let err = parsed.unwrap_err().to_string();
            assert!(err.contains("subdirectory"), "{bad}: {err}");
        }
    }

    #[test]
    fn exact_binding_requires_a_valid_git_source() {
        let cases = [
            "exact = true\n",
            "git = \"owner/repo\"\nrev = \"\"\nexact = true\n",
            "git = \"owner/repo/extra\"\nexact = true\n",
            "git = \"gitlab::host/group/repo/-/subdir\"\nexact = true\n",
        ];
        for dependency in cases {
            let toml = format!("[project]\nname = \"t\"\n\n[dependencies.repo]\n{dependency}");
            let error = toml.parse::<Manifest>().unwrap_err().to_string();
            assert!(error.contains("exact = true"), "{error}");
            assert!(error.contains("requires a"), "{error}");
        }
    }

    #[test]
    fn subdirectory_requires_a_github_git_source() {
        let no_git = r#"
[project]
name = "t"

[dependencies.nested]
version = "*"
subdirectory = "pkgs/nested"
"#;
        let err = no_git.parse::<Manifest>().unwrap_err().to_string();
        assert!(err.contains("requires a `git` source"), "got: {err}");

        for host in ["gitlab::gitlab.com/g/p", "forgejo::codefloe.com/o/r"] {
            let toml = format!(
                "[project]\nname = \"t\"\n\n[dependencies.nested]\ngit = \"{host}\"\n\
                 subdirectory = \"pkgs/nested\"\n"
            );
            let err = toml.parse::<Manifest>().unwrap_err().to_string();
            assert!(err.contains("only supported for GitHub"), "got: {err}");
        }
    }

    #[test]
    fn subdirectory_rejects_malformed_github_sources() {
        for (git, rev) in [
            ("owner/repo", Some("")),
            ("owner/repo", Some("a b")),
            ("", None),
            ("repo", None),
            ("owner/", None),
            ("/repo", None),
            ("owner/repo/extra", None),
        ] {
            let rev_line = rev.map(|r| format!("rev = \"{r}\"\n")).unwrap_or_default();
            let toml = format!(
                "[project]\nname = \"t\"\n\n[dependencies.nested]\ngit = \"{git}\"\n{rev_line}\
                 subdirectory = \"pkgs/nested\"\n"
            );
            let parsed = toml.parse::<Manifest>();
            assert!(parsed.is_err(), "should reject git={git:?} rev={rev:?}");
            let err = parsed.unwrap_err().to_string();
            assert!(err.contains("not a valid GitHub source"), "got: {err}");
        }
    }

    #[test]
    fn subdirectory_must_be_a_string() {
        let toml = r#"
[project]
name = "t"

[dependencies.nested]
git = "owner/repo"
subdirectory = 42
"#;
        let err = toml.parse::<Manifest>().unwrap_err().to_string();
        assert!(err.contains("must be a string"), "got: {err}");
    }

    #[test]
    fn programmatic_invalid_subdirectory_is_not_serialized() {
        let mut manifest = Manifest::new("t", None);
        for dep in [
            DetailedDep {
                git: Some("owner/repo".into()),
                subdirectory: Some("../escape".into()),
                ..Default::default()
            },
            DetailedDep {
                git: Some("gitlab::gitlab.com/group/repo".into()),
                subdirectory: Some("pkgs/nested".into()),
                ..Default::default()
            },
        ] {
            manifest
                .dependencies
                .insert("nested".into(), DependencySpec::Detailed(dep));
            assert!(manifest.to_toml_string().is_err());
            let temp = tempfile::TempDir::new().unwrap();
            let path = temp.path().join("uvr.toml");
            assert!(manifest.write(&path).is_err());
            assert!(!path.exists());
        }
    }

    #[test]
    fn table_header_syntax_with_valid_fields_is_accepted() {
        // `[dependencies.DESeq2]` with known DetailedDep fields must still work.
        let toml = r#"
[project]
name = "test"
r_version = "4.5"

[dependencies.DESeq2]
bioc = true

[dependencies.myPkg]
git = "user/repo"
rev = "main"
"#;
        let m: Manifest = toml.parse().expect("valid table-header syntax must parse");
        assert!(m.dependencies.get("DESeq2").unwrap().is_bioc());
        assert_eq!(
            m.dependencies.get("myPkg").unwrap().git(),
            Some("user/repo")
        );
    }
}
