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

    /// branch / tag / commit SHA
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageSource {
    pub name: String,
    pub url: String,
}

impl std::str::FromStr for Manifest {
    type Err = crate::error::UvrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        toml::from_str(s).map_err(|e| crate::error::UvrError::ManifestParse(e.to_string()))
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
            },
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            sources: Vec::new(),
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
    pub fn from_description_str(content: &str) -> Result<Self> {
        let fields = parse_dcf(content);

        let name = fields
            .get("Package")
            .map(|s| s.as_str())
            .unwrap_or("unnamed")
            .to_string();

        let r_version = fields.get("Depends").and_then(|deps| {
            parse_r_version_from_depends(deps)
        });

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

        Ok(Manifest {
            project: ProjectMeta {
                name,
                r_version,
                bioc_version: None,
                description: fields.get("Title").cloned(),
            },
            dependencies,
            dev_dependencies,
            sources: Vec::new(),
        })
    }

    pub fn from_description_file(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_description_str(&s)
    }

    pub fn to_toml_string(&self) -> Result<String> {
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
}

/// Parse a DCF (Debian Control File) string into a `BTreeMap<field, value>`.
/// Continuation lines (leading whitespace) are joined with a space.
fn parse_dcf(content: &str) -> BTreeMap<String, String> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut current_key: Option<String> = None;
    let mut current_value = String::new();

    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line
            if current_key.is_some() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if !current_value.is_empty() {
                        current_value.push(' ');
                    }
                    current_value.push_str(trimmed);
                }
            }
        } else if let Some(colon_pos) = line.find(':') {
            // Save previous field
            if let Some(key) = current_key.take() {
                fields.insert(key, current_value.trim().to_string());
                current_value.clear();
            }
            current_key = Some(line[..colon_pos].trim().to_string());
            current_value = line[colon_pos + 1..].trim().to_string();
        }
    }
    // Save last field
    if let Some(key) = current_key {
        fields.insert(key, current_value.trim().to_string());
    }
    fields
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

/// Extract R version constraint from a `Depends:` field value.
/// e.g. `"R (>= 4.0.0), methods"` → `Some(">=4.0.0")`
fn parse_r_version_from_depends(depends: &str) -> Option<String> {
    for entry in depends.split(',') {
        let entry = entry.trim();
        if entry.starts_with('R') {
            let rest = entry[1..].trim();
            if rest.is_empty() || rest.starts_with('(') {
                if let Some(paren) = entry.find('(') {
                    let inner =
                        entry[paren + 1..entry.rfind(')').unwrap_or(entry.len())].trim();
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
        assert_eq!(m.project.description.as_deref(), Some("My Analysis Project"));
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
}
