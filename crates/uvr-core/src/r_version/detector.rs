use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Result, UvrError};
use crate::project::{read_global_r_version_pin, read_r_version_pin_from};
use crate::resolver::normalize_version;

/// Discovered R installation.
#[derive(Debug, Clone)]
pub struct RInstallation {
    pub binary: PathBuf,
    pub version: String,
    pub managed: bool, // true = installed by uvr
}

/// Which rule selected the R, as `uvr r find` and `uvr doctor` report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RSource {
    /// `UVR_R_VERSION`.
    EnvOverride,
    /// A `.r-version` in the directory or a parent.
    ProjectPin,
    /// The global pin that `uvr r pin --global` writes.
    GlobalPin,
    /// The newest installed R that satisfies the constraint.
    Constraint,
    /// The newest installed R.
    Newest,
}

impl std::fmt::Display for RSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RSource::EnvOverride => "UVR_R_VERSION",
            RSource::ProjectPin => "project pin",
            RSource::GlobalPin => "global pin",
            RSource::Constraint => "constraint",
            RSource::Newest => "newest",
        })
    }
}

/// Find all R installations: managed ones + system R.
pub fn find_all() -> Vec<RInstallation> {
    let mut found = Vec::new();

    let r_name = if cfg!(target_os = "windows") {
        "R.exe"
    } else {
        "R"
    };

    // Managed installations in ~/.uvr/r-versions/
    if let Some(versions_dir) = crate::env_vars::r_install_dir() {
        if let Ok(entries) = std::fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                // Skip dot-prefixed dirs: `.uvr-stage-*` extraction dirs
                // orphaned by a killed install would otherwise be listed as
                // installed versions.
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let bin = entry.path().join("bin").join(r_name);
                if bin.exists() {
                    let version = entry.file_name().to_string_lossy().to_string();
                    found.push(RInstallation {
                        binary: bin,
                        version,
                        managed: true,
                    });
                }
            }
        }
    }

    // R_HOME: if uvr is spawned from within an R session, R_HOME is set.
    // Use it to locate the running R binary even if it's not on PATH.
    if let Ok(r_home) = std::env::var("R_HOME") {
        let r_home_bin = PathBuf::from(&r_home).join("bin").join(r_name);
        if r_home_bin.exists() && !found.iter().any(|i| i.binary == r_home_bin) {
            if let Some(version) = query_r_version(&r_home_bin) {
                found.push(RInstallation {
                    binary: r_home_bin,
                    version,
                    managed: false,
                });
            }
        }
    }

    // System R on PATH
    if let Ok(r_path) = which::which(r_name) {
        if !found.iter().any(|i| i.binary == r_path) {
            if let Some(version) = query_r_version(&r_path) {
                found.push(RInstallation {
                    binary: r_path,
                    version,
                    managed: false,
                });
            }
        }
    }

    // Windows: check common install locations. Besides `ProgramFiles`, the
    // official installer offers `ProgramFiles(x86)` for 32-bit builds and
    // defaults to `%LOCALAPPDATA%\Programs\R` for per-user (no-admin)
    // installs — common on corporate/university machines (#158).
    #[cfg(target_os = "windows")]
    {
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(dir) = std::env::var(var) {
                roots.push(std::path::Path::new(&dir).join("R"));
            }
        }
        if roots.is_empty() {
            roots.push(std::path::PathBuf::from("C:\\Program Files\\R"));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            roots.push(std::path::Path::new(&local).join("Programs").join("R"));
        }
        for r_base in roots {
            if let Ok(entries) = std::fs::read_dir(&r_base) {
                for entry in entries.flatten() {
                    let bin = entry.path().join("bin").join("R.exe");
                    if bin.exists() && !found.iter().any(|i| i.binary == bin) {
                        if let Some(version) = query_r_version(&bin) {
                            found.push(RInstallation {
                                binary: bin,
                                version,
                                managed: false,
                            });
                        }
                    }
                }
            }
        }
    }

    found
}

/// Get the R binary to use.
///
/// Resolution order:
/// 0. `UVR_R_VERSION`, which has no fallback
/// 1. `.r-version` file (exact pin, walked up from cwd)
/// 2. The global pin, if it satisfies `version_constraint`
/// 3. `version_constraint` from `uvr.toml` (see [`r_version_satisfies`])
/// 4. Any managed installation, then system R
pub fn find_r_binary(version_constraint: Option<&str>) -> Result<PathBuf> {
    find_r_binary_with_source(version_constraint).map(|(binary, _)| binary)
}

/// Like [`find_r_binary`], and also says which rule selected the R.
pub fn find_r_binary_with_source(version_constraint: Option<&str>) -> Result<(PathBuf, RSource)> {
    let cwd = pin_start_dir();
    resolve_r_binary(version_constraint, Rules::all(Some(&cwd)))
}

fn pin_start_dir() -> PathBuf {
    // If the working directory cannot be resolved (deleted, or a parent is
    // unreadable), `.` still finds a pin in the directory itself, but the
    // walk cannot go up to its parents. Say so rather than miss it silently.
    std::env::current_dir().unwrap_or_else(|e| {
        tracing::warn!(
            "Cannot determine the working directory ({e}); a .r-version pin \
             in a parent directory will not be found"
        );
        PathBuf::from(".")
    })
}

/// Like [`find_r_binary`], but walks up from `dir` for the `.r-version` pin
/// instead of from the working directory. For callers acting on a project
/// they are not necessarily inside — `uvr init <name>` creates the project
/// in a subdirectory, and the IDE settings it writes must name the R that
/// `uvr run` will later pick from inside that project (#321).
pub fn find_r_binary_for_project(dir: &Path, version_constraint: Option<&str>) -> Result<PathBuf> {
    resolve_r_binary(version_constraint, Rules::all(Some(dir))).map(|(binary, _)| binary)
}

/// Like [`find_r_binary`], but ignores any `.r-version` pin.
///
/// For standalone script runs (#181). A script carrying its own dependency
/// header has to resolve the same way in every directory — that is the whole
/// promise of it. The pin is walked up from the working directory, so
/// honouring it would let whatever project the file happens to be sitting in
/// choose the interpreter, and with it the ephemeral environment's cache key:
/// the same script would get a different R, and a different set of installed
/// packages, purely because of where it was run from.
///
/// `UVR_R_VERSION` and the global pin still apply. They are machine state, not
/// directory state, so they do not break that promise (#329).
pub fn find_r_binary_ignoring_pin(version_constraint: Option<&str>) -> Result<PathBuf> {
    resolve_r_binary(version_constraint, Rules::all(None)).map(|(binary, _)| binary)
}

/// The newest working R that satisfies `constraint`, with no other rule. For a
/// caller that asks that exact question: `uvr r find <constraint>`, and the R
/// that a script header just caused uvr to download.
pub fn find_r_binary_satisfying(constraint: &str) -> Result<PathBuf> {
    let rules = Rules {
        env_override: false,
        pin_dir: None,
        global_pin: false,
    };
    resolve_r_binary(Some(constraint), rules).map(|(binary, _)| binary)
}

/// For `uvr run --r-version`: an explicit flag outranks `UVR_R_VERSION` and the
/// global pin, as a flag does its environment form in uv. `honour_pin` is false
/// in script mode, which ignores directory pins.
///
/// A directory pin outranks the flag, as it does a constraint, except when
/// `UVR_R_VERSION` is set. The variable outranks the pin, so the pin must not
/// come back only because a flag replaced the variable: the result would be an
/// R that neither of them asked for.
pub fn find_r_binary_for_flag(constraint: &str, honour_pin: bool) -> Result<PathBuf> {
    let honour_pin = honour_pin && crate::env_vars::r_version().is_none();
    let cwd = honour_pin.then(pin_start_dir);
    let rules = Rules {
        env_override: false,
        pin_dir: cwd.as_deref(),
        global_pin: false,
    };
    resolve_r_binary(Some(constraint), rules).map(|(binary, _)| binary)
}

/// Whether the installed R `version` satisfies a `[project] r_version`
/// constraint.
///
/// A bare version (`4.5.3`, `4.5`) is a pin, matched by component prefix —
/// the same meaning it has in `.r-version` and in `uvr r use`: `4.5.3`
/// selects exactly 4.5.3 and `4.5` selects any 4.5.x. Anything with an
/// operator (`>=4.3`, `^4.5`, `==4.5.3`) is a version requirement. A bare
/// version used to go straight to semver, which reads it as a caret
/// requirement, so `r_version = "4.5.3"` selected R 4.6.0 while the
/// mismatch check called the same string a pin and warned (#320).
pub fn r_version_satisfies(constraint: &str, version: &str) -> Result<bool> {
    let constraint = constraint.trim();
    if is_plausible_r_version(constraint) {
        return Ok(version_matches_prefix(constraint, version));
    }
    let req = crate::resolver::parse_version_req(constraint)?;
    let norm = normalize_version(version);
    Ok(semver::Version::parse(&norm)
        .map(|v| req.matches(&v))
        .unwrap_or(false))
}

/// The rules a resolution may use, beside the constraint and the newest R.
#[derive(Clone, Copy)]
struct Rules<'a> {
    env_override: bool,
    /// Where to start walking up for a `.r-version` pin; `None` ignores
    /// directory pins entirely.
    pin_dir: Option<&'a Path>,
    global_pin: bool,
}

impl<'a> Rules<'a> {
    fn all(pin_dir: Option<&'a Path>) -> Self {
        Rules {
            env_override: true,
            pin_dir,
            global_pin: true,
        }
    }
}

/// The newest installed R that `keep` accepts and that responds to a version
/// query, so a broken install is skipped and does not capture every command.
fn newest_working(
    installations: &[RInstallation],
    keep: impl Fn(&RInstallation) -> bool,
) -> Option<PathBuf> {
    let mut matches: Vec<&RInstallation> = installations.iter().filter(|i| keep(i)).collect();
    matches.sort_by(|a, b| version_cmp(&b.version, &a.version));
    matches
        .into_iter()
        .find(|inst| query_r_version(&inst.binary).is_some())
        .map(|inst| inst.binary.clone())
}

fn installed_versions(installations: &[RInstallation]) -> String {
    if installations.is_empty() {
        return "none".to_string();
    }
    installations
        .iter()
        .map(|i| i.version.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Tier 0: `UVR_R_VERSION` chose this, so nothing else may stand in for it.
fn select_env_override(installations: &[RInstallation], wanted: &str) -> Result<PathBuf> {
    r_version_satisfies(wanted, "0.0.0").map_err(|e| {
        UvrError::Other(format!(
            "UVR_R_VERSION=`{wanted}` is not a valid R version or constraint: {e}"
        ))
    })?;
    newest_working(installations, |inst| {
        r_version_satisfies(wanted, &inst.version).unwrap_or(false)
    })
    .ok_or_else(|| {
        UvrError::Other(format!(
            "UVR_R_VERSION=`{wanted}` matches no working R (installed: {}). Install one \
             with `uvr r install <version>`, or unset UVR_R_VERSION.",
            installed_versions(installations)
        ))
    })
}

/// Tier 2: the global pin is a preference, so anything wrong with it gives
/// `None`, and the next rule decides. A default that is not used without a
/// reason is hard to find, so say why, unless the constraint excludes the pin.
fn select_global_pin(installations: &[RInstallation], constraint: Option<&str>) -> Option<PathBuf> {
    // One command can resolve R several times.
    static WARNED: std::sync::Once = std::sync::Once::new();
    let warn = |message: String| WARNED.call_once(|| tracing::warn!("{message}"));

    let pin = read_global_r_version_pin()?;
    if !is_plausible_r_version(&pin) {
        warn(format!(
            "Ignoring the global R pin `{pin}`: it is not a valid R version. Set it again \
             with `uvr r pin --global <version>`."
        ));
        return None;
    }
    let is_pinned = |inst: &RInstallation| version_matches_prefix(&pin, &inst.version);
    if !installations.iter().any(is_pinned) {
        if !constraint.is_some_and(|c| pin_conflicts_with_constraint(&pin, c)) {
            warn(format!(
                "The global R pin {pin} is not installed, so it is not used. Run \
                 `uvr r install {pin}`, or clear it with `uvr r pin --global --unset`."
            ));
        }
        return None;
    }
    let usable = |inst: &RInstallation| {
        is_pinned(inst)
            && constraint.is_none_or(|c| r_version_satisfies(c, &inst.version).unwrap_or(false))
    };
    let selected = newest_working(installations, usable);
    if selected.is_none() && installations.iter().any(usable) {
        warn(format!(
            "The global R pin {pin} is installed but does not work (no version response), so \
             it is not used. Reinstall it, or clear it with `uvr r pin --global --unset`."
        ));
    }
    selected
}

fn resolve_r_binary(
    version_constraint: Option<&str>,
    rules: Rules<'_>,
) -> Result<(PathBuf, RSource)> {
    let installations = find_all();

    // 0. UVR_R_VERSION overrides every rule below. Its error names the variable,
    // also if there is no R at all.
    if rules.env_override {
        if let Some(wanted) = crate::env_vars::r_version() {
            return select_env_override(&installations, &wanted)
                .map(|bin| (bin, RSource::EnvOverride));
        }
    }

    if installations.is_empty() {
        return Err(UvrError::RNotFound);
    }

    // 1. Honour .r-version exact pin
    let pin = rules.pin_dir.and_then(read_r_version_pin_from);
    if let Some(pinned) = pin {
        let bin = find_exact_version(&installations, &pinned)?;
        // Validate the pinned install — a broken managed R (one whose binary
        // doesn't respond to a version query, e.g. a corrupted or partially
        // extracted install) must not be silently selected and crash
        // downstream.
        let Some(resolved) = query_r_version(&bin) else {
            return Err(UvrError::Other(format!(
                "R {pinned} is pinned in .r-version but the install at {} is broken \
                 (no version response). Reinstall: `uvr r uninstall {pinned} && uvr r install {pinned}`.",
                bin.display()
            )));
        };
        // The pin always wins over the constraint (from uvr.toml or from
        // `uvr run --r-version`), but silent drift between the two is confusing
        // (`uvr r use 4.3.0` after setting `r_version = "^4.5"`, #156) — surface
        // it without changing precedence.
        if let Some(constraint) = version_constraint {
            if pin_conflicts_with_constraint(&resolved, constraint) {
                tracing::warn!(
                    ".r-version pins R {pinned} (resolved to {resolved}), which does not \
                     satisfy the constraint `{constraint}`; the pin takes precedence. \
                     Update .r-version or the constraint to bring them back in sync."
                );
            }
        }
        return Ok((bin, RSource::ProjectPin));
    }

    // Validate once up front so an unparseable constraint is an error,
    // not "no installation matches".
    if let Some(constraint) = version_constraint {
        r_version_satisfies(constraint, "0.0.0")?;
    }

    // 2. The global pin, within any constraint
    if rules.global_pin {
        if let Some(bin) = select_global_pin(&installations, version_constraint) {
            return Ok((bin, RSource::GlobalPin));
        }
    }

    // 3. Honour the constraint from uvr.toml
    if let Some(constraint) = version_constraint {
        // Probe matches in version-descending order and skip broken installs,
        // mirroring case (4) below.
        if let Some(bin) = newest_working(&installations, |inst| {
            r_version_satisfies(constraint, &inst.version).unwrap_or(false)
        }) {
            return Ok((bin, RSource::Constraint));
        }
        return Err(UvrError::RVersionUnsatisfied {
            constraint: constraint.to_string(),
            installed: installed_versions(&installations),
        });
    }

    // 4. Prefer managed installation, fall back to first system R.
    //    Validate each candidate via `query_r_version` so a broken managed
    //    install (one whose R binary doesn't respond to a version query)
    //    isn't silently selected and doesn't capture every uvr command.
    let mut managed: Vec<&RInstallation> = installations.iter().filter(|i| i.managed).collect();
    let mut system: Vec<&RInstallation> = installations.iter().filter(|i| !i.managed).collect();
    // Probe in version-descending order so the newest working install wins.
    managed.sort_by(|a, b| version_cmp(&b.version, &a.version));
    system.sort_by(|a, b| version_cmp(&b.version, &a.version));
    for inst in managed.into_iter().chain(system) {
        if query_r_version(&inst.binary).is_some() {
            return Ok((inst.binary.clone(), RSource::Newest));
        }
    }
    Err(UvrError::RNotFound)
}

/// True when `resolved_version` (the full version of the install a
/// `.r-version` pin resolved to, e.g. `4.3.2`) fails a parseable uvr.toml
/// `constraint`. Unparseable constraints or versions return false — the
/// drift warning must never fire on input the constraint branch itself
/// couldn't act on.
///
/// Public so `uvr r pin` (no arg) can refuse to write a pin that violates
/// the manifest constraint (#137), using the exact same conflict semantics
/// as the resolution-time drift warning above.
pub fn pin_conflicts_with_constraint(resolved_version: &str, constraint: &str) -> bool {
    // Unparseable versions never satisfy a requirement, so they would read
    // as a conflict here; keep the old "only on input we can act on" rule.
    let norm = normalize_version(resolved_version);
    if semver::Version::parse(&norm).is_err() {
        return false;
    }
    matches!(r_version_satisfies(constraint, resolved_version), Ok(false))
}

/// Compare two version strings with R's semantics: `.` and `-` both separate
/// components (`1.2-7` == `1.2.7`), and missing trailing components count as
/// zero (`4.5` == `4.5.0`). A non-numeric tail (`4.5.0-rc1`, `4.5.0alpha`)
/// marks a pre-release that sorts below the plain release. The old `Vec<u32>`
/// comparison got both wrong — shorter Vecs sorted first, and non-numeric
/// parts were silently dropped, tying `4.5.0-rc1` with `4.5.0` (#157).
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    // (numeric components, has a non-numeric tail). Parsing stops at the
    // first non-numeric part; everything after it is just a pre-release
    // marker, not compared further.
    let parse = |s: &str| -> (Vec<u32>, bool) {
        let mut nums = Vec::new();
        for part in s.split(['.', '-']) {
            match part.parse() {
                Ok(n) => nums.push(n),
                Err(_) => return (nums, true),
            }
        }
        (nums, false)
    };
    let (mut va, pre_a) = parse(a);
    let (mut vb, pre_b) = parse(b);
    let len = va.len().max(vb.len());
    va.resize(len, 0);
    vb.resize(len, 0);
    // On equal numeric components the pre-release (true) sorts lower.
    va.cmp(&vb).then(pre_b.cmp(&pre_a))
}

/// True when `s` has the shape of an R version a user may type: 2–4
/// dot-separated, non-empty, all-digit components (`4.5`, `4.5.1`,
/// `4.5.1.2`). Rejects constraint syntax, garbage like `--`/`....`/`4..`,
/// and dash forms (`4.5-2` belongs to package versions, not R itself).
pub fn is_plausible_r_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    (2..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// True when every component of `prefix` numerically equals the
/// corresponding leading component of `full`: `4.5` matches `4.5.1` but not
/// `4.50.1`; `4.5.1` matches only `4.5.1[.x]`. Non-numeric input never
/// matches.
pub fn version_matches_prefix(prefix: &str, full: &str) -> bool {
    let parse = |s: &str| -> Option<Vec<u32>> { s.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(prefix), parse(full)) {
        (Some(p), Some(f)) => {
            !p.is_empty() && p.len() <= f.len() && p.iter().zip(&f).all(|(a, b)| a == b)
        }
        _ => false,
    }
}

fn find_exact_version(installations: &[RInstallation], version: &str) -> Result<PathBuf> {
    // Exact string match first — the common case for full `X.Y.Z` pins.
    if let Some(inst) = installations.iter().find(|i| i.version == version) {
        return Ok(inst.binary.clone());
    }
    // Partial pin (`4.5`): newest installed version matching by component
    // prefix. Pins used to be compared with string equality only, so a
    // `.r-version` holding `4.5` could never match the `4.5.x` install
    // directories and every command failed with "not installed" (#136).
    let mut matches: Vec<&RInstallation> = installations
        .iter()
        .filter(|i| version_matches_prefix(version, &i.version))
        .collect();
    matches.sort_by(|a, b| version_cmp(&b.version, &a.version));
    matches.first().map(|i| i.binary.clone()).ok_or_else(|| {
        UvrError::Other(format!(
            "R {version} is pinned in .r-version but not installed. Run: uvr r install {version}"
        ))
    })
}

/// Given a list of installations, return the binary for an exact version match.
/// Used by `.r-version` resolution.
/// (Exposed for testing as `find_exact_version` is private.)
#[cfg(test)]
pub fn test_find_exact(installations: &[RInstallation], version: &str) -> Result<PathBuf> {
    find_exact_version(installations, version)
}

/// Identity of an R binary for memoization: path plus the file's size and
/// mtime, so reinstalling a different R at the same path is a cache miss.
type RBinaryIdentity = (std::path::PathBuf, u64, Option<std::time::SystemTime>);

fn r_binary_identity(binary: &std::path::Path) -> RBinaryIdentity {
    let md = std::fs::metadata(binary).ok();
    (
        binary.to_path_buf(),
        md.as_ref().map(|m| m.len()).unwrap_or(0),
        md.as_ref().and_then(|m| m.modified().ok()),
    )
}

#[allow(clippy::type_complexity)]
static R_VERSION_MEMO: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<RBinaryIdentity, Option<String>>>,
> = std::sync::OnceLock::new();

/// Ask an R binary for its `major.minor` version, memoized per process.
///
/// A single `uvr sync` asks the same binary repeatedly — `find_all` probes
/// every discovered install, the pin-mismatch check re-resolves, the IDE
/// scaffolding resolves again — and each ask is a full R startup. That's
/// ~0.1s locally but 0.3–0.5s on a container's shared I/O, which is most
/// of the fixed floor a warm sync pays before installing anything (#246).
///
/// Memoizing is safe within a process: the answer is a property of the
/// binary, and the key includes size and mtime so an R replaced at the
/// same path mid-run is a miss rather than a stale hit. Failures are
/// cached too — a binary that won't start won't start twice.
pub fn query_r_version(binary: &std::path::Path) -> Option<String> {
    let key = r_binary_identity(binary);
    let memo = R_VERSION_MEMO.get_or_init(Default::default);
    if let Ok(cache) = memo.lock() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let answer = query_r_version_uncached(binary);
    if let Ok(mut cache) = memo.lock() {
        cache.insert(key, answer.clone());
    }
    answer
}

fn query_r_version_uncached(binary: &std::path::Path) -> Option<String> {
    let mut cmd = Command::new(binary);
    cmd
        // When uvr runs inside an R session (RStudio terminal, `system("uvr …")`
        // from uvr-r), the enclosing session's R_HOME/R_LIBS* leak into this
        // spawn. The queried binary knows its own home; an inherited R_HOME
        // from a *different* — possibly since-removed — install at best makes
        // R warn on stdout and at worst breaks startup (#128, #99).
        .env_remove("R_HOME")
        .env_remove("R_LIBS")
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE")
        .args([
            "--vanilla",
            "--slave",
            "-e",
            "cat(R.version$major, \".\", R.version$minor, sep='')",
        ]);
    // A busy executable would otherwise be memoized as "no R here" for the
    // rest of the run (#301).
    let output = crate::process::retry_text_file_busy(|| cmd.output()).ok()?;
    if !output.status.success() {
        return None;
    }
    // R sometimes prints startup warnings (e.g. "WARNING: ignoring environment
    // value of R_HOME" when R_HOME points at a different install) to **stdout**
    // before user code runs. The version string from our `-e` script is always
    // the last line, so pick the last non-empty line that parses as a version.
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().rev().find_map(|line| {
        let t = line.trim();
        // Strict shape check: the old digits-and-dots filter accepted noise
        // like `....` or `4..` (#159).
        if is_plausible_r_version(t) {
            Some(t.to_string())
        } else {
            None
        }
    })
}

#[allow(clippy::type_complexity)]
static R_BASE_PACKAGES_MEMO: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<RBinaryIdentity, Option<Vec<String>>>>,
> = std::sync::OnceLock::new();

/// Marks the line of the base-package query's output that holds the names,
/// so R start-up warnings on stdout are not read as package names.
const BASE_PACKAGES_SENTINEL: &str = "UVR_BASE_PACKAGES:";

/// Ask an R binary for the names of its base packages, memoized per process
/// like [`query_r_version`].
///
/// The resolver's hardcoded list goes stale when R adds a base package; the
/// resolver then sends that name to the registry and fails with
/// `PackageNotFound` (#169). Only the system library (`.Library`) is read,
/// because base packages are always there.
pub fn query_r_base_packages(binary: &std::path::Path) -> Option<Vec<String>> {
    let key = r_binary_identity(binary);
    let memo = R_BASE_PACKAGES_MEMO.get_or_init(Default::default);
    if let Ok(cache) = memo.lock() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let answer = query_r_base_packages_uncached(binary);
    if let Ok(mut cache) = memo.lock() {
        cache.insert(key, answer.clone());
    }
    answer
}

fn query_r_base_packages_uncached(binary: &std::path::Path) -> Option<Vec<String>> {
    let script = format!(
        "cat('{BASE_PACKAGES_SENTINEL}', \
         rownames(installed.packages(lib.loc = .Library, priority = 'base')), '\\n')"
    );
    let output = Command::new(binary)
        // Same isolation as `query_r_version_uncached` (#128, #99).
        .env_remove("R_HOME")
        .env_remove("R_LIBS")
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE")
        .args(["--vanilla", "--slave", "-e", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_base_packages_output(&String::from_utf8_lossy(&output.stdout))
}

/// Read the package names from the sentinel line. `None` when the line is
/// missing or holds no names, so a broken R adds nothing to the base set.
fn parse_base_packages_output(stdout: &str) -> Option<Vec<String>> {
    let line = stdout
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix(BASE_PACKAGES_SENTINEL))?;
    let names: Vec<String> = line
        .split_whitespace()
        .filter(|n| crate::package_name::is_valid(n))
        .map(str::to_string)
        .collect();
    (!names.is_empty()).then_some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_base_packages_output_skips_startup_noise() {
        let out = "WARNING: ignoring environment value of R_HOME\n\
                   UVR_BASE_PACKAGES: base compiler newbase utils \n";
        assert_eq!(
            parse_base_packages_output(out).unwrap(),
            vec!["base", "compiler", "newbase", "utils"]
        );
    }

    #[test]
    fn parse_base_packages_output_rejects_missing_or_empty() {
        assert!(parse_base_packages_output("").is_none());
        assert!(parse_base_packages_output("base utils\n").is_none());
        assert!(parse_base_packages_output("UVR_BASE_PACKAGES: \n").is_none());
    }

    fn fake_installation(version: &str, managed: bool) -> RInstallation {
        RInstallation {
            binary: PathBuf::from(format!("/fake/r-{version}/bin/R")),
            version: version.to_string(),
            managed,
        }
    }

    #[test]
    fn find_exact_version_found() {
        let installations = vec![
            fake_installation("4.3.2", true),
            fake_installation("4.4.1", true),
        ];
        let result = find_exact_version(&installations, "4.4.1").unwrap();
        assert_eq!(result, PathBuf::from("/fake/r-4.4.1/bin/R"));
    }

    #[test]
    fn find_exact_version_not_found() {
        let installations = vec![fake_installation("4.3.2", true)];
        let result = find_exact_version(&installations, "4.4.1");
        assert!(result.is_err());
    }

    #[test]
    fn find_exact_version_partial_pin_picks_newest_match() {
        // #136: a `4.5` pin must resolve against `4.5.x` installs.
        let installations = vec![
            fake_installation("4.5.1", true),
            fake_installation("4.5.3", true),
            fake_installation("4.6.0", true),
        ];
        let result = find_exact_version(&installations, "4.5").unwrap();
        assert_eq!(result, PathBuf::from("/fake/r-4.5.3/bin/R"));
    }

    #[test]
    fn find_exact_version_partial_pin_no_match() {
        let installations = vec![fake_installation("4.6.0", true)];
        assert!(find_exact_version(&installations, "4.5").is_err());
    }

    #[test]
    fn version_matches_prefix_component_semantics() {
        assert!(version_matches_prefix("4.5", "4.5.1"));
        assert!(version_matches_prefix("4.5", "4.5"));
        assert!(version_matches_prefix("4.5.1", "4.5.1"));
        assert!(version_matches_prefix("4.5.1", "4.5.1.2"));
        // Component-wise, not string-prefix:
        assert!(!version_matches_prefix("4.5", "4.50.1"));
        // A longer pin never matches a shorter install:
        assert!(!version_matches_prefix("4.5.0", "4.5"));
        assert!(!version_matches_prefix("4.5", "4.6.0"));
        // Non-numeric input never matches:
        assert!(!version_matches_prefix("--", "4.5.1"));
        assert!(!version_matches_prefix("4.5", "four.five"));
        assert!(!version_matches_prefix("", "4.5.1"));
    }

    #[test]
    fn is_plausible_r_version_shapes() {
        assert!(is_plausible_r_version("4.5"));
        assert!(is_plausible_r_version("4.5.1"));
        assert!(is_plausible_r_version("4.5.1.2"));
        // Garbage the old char-filter accepted (#171, #159):
        assert!(!is_plausible_r_version("--"));
        assert!(!is_plausible_r_version("4-5-2"));
        assert!(!is_plausible_r_version("4.5-2"));
        assert!(!is_plausible_r_version("...."));
        assert!(!is_plausible_r_version("4.."));
        assert!(!is_plausible_r_version("4"));
        assert!(!is_plausible_r_version(""));
        assert!(!is_plausible_r_version("4.5.1.2.3"));
        assert!(!is_plausible_r_version(">=4.5"));
    }

    #[test]
    fn version_cmp_r_semantics() {
        use std::cmp::Ordering::*;
        // #157: unequal lengths used to sort the shorter Vec first, and
        // non-numeric tails were dropped so pre-releases tied with releases.
        let cases = [
            // Missing components are zero.
            ("4.5", "4.5.0", Equal),
            ("4.5.0", "4.5", Equal),
            ("4.5", "4.5.1", Less),
            ("4.5.1", "4.5", Greater),
            // `-` is a component separator, as in package versions.
            ("1.2-7", "1.2.7", Equal),
            ("1.2-7", "1.2-6", Greater),
            // Numeric, not lexicographic.
            ("4.10.0", "4.9.9", Greater),
            // A non-numeric tail is a pre-release, below the plain release.
            ("4.5.0-rc1", "4.5.0", Less),
            ("4.5.0", "4.5.0-rc1", Greater),
            ("4.5.0alpha", "4.5.0", Less),
            ("4.5.1-rc1", "4.5.0", Greater),
            // Tails are markers only; two pre-releases of one base tie.
            ("4.5.0-rc1", "4.5.0-rc2", Equal),
        ];
        for (a, b, want) in cases {
            assert_eq!(version_cmp(a, b), want, "version_cmp({a:?}, {b:?})");
        }
    }

    #[test]
    fn pin_conflicts_with_constraint_detects_drift() {
        // #156: manifest `^4.5` + pin resolving to 4.3.x is drift.
        assert!(pin_conflicts_with_constraint("4.3.0", "^4.5"));
        assert!(pin_conflicts_with_constraint("4.6.1", ">=4.7"));
        // Satisfied constraints are not drift.
        assert!(!pin_conflicts_with_constraint("4.5.1", "^4.5"));
        assert!(!pin_conflicts_with_constraint("4.5.1", ">=4.4"));
        assert!(!pin_conflicts_with_constraint("4.5.1", "*"));
        // Two-component resolved versions normalize before matching.
        assert!(!pin_conflicts_with_constraint("4.5", "^4.5"));
        assert!(pin_conflicts_with_constraint("4.3", "^4.5"));
        // Unparseable constraint or version never reports drift.
        assert!(!pin_conflicts_with_constraint("4.5.1", "not-a-constraint"));
        assert!(!pin_conflicts_with_constraint("garbage", "^4.5"));
        // A bare manifest version is a pin, not a caret requirement (#320).
        assert!(pin_conflicts_with_constraint("4.6.0", "4.5.3"));
        assert!(!pin_conflicts_with_constraint("4.5.3", "4.5.3"));
    }

    #[test]
    fn r_version_satisfies_reads_bare_versions_as_pins() {
        // #320: semver read `4.5.3` as `^4.5.3`, so R 4.6.0 satisfied it.
        assert!(r_version_satisfies("4.5.3", "4.5.3").unwrap());
        assert!(!r_version_satisfies("4.5.3", "4.6.0").unwrap());
        assert!(!r_version_satisfies("4.5.3", "4.5.2").unwrap());
        // A partial pin matches its minor series, as in `.r-version`.
        assert!(r_version_satisfies("4.5", "4.5.0").unwrap());
        assert!(r_version_satisfies("4.5", "4.5.3").unwrap());
        assert!(!r_version_satisfies("4.5", "4.6.0").unwrap());
        assert!(!r_version_satisfies("4.5", "4.50.1").unwrap());
        assert!(r_version_satisfies(" 4.5.3 ", "4.5.3").unwrap());
        // Operator forms keep requirement semantics.
        assert!(r_version_satisfies("==4.5.3", "4.5.3").unwrap());
        assert!(!r_version_satisfies("==4.5.3", "4.6.0").unwrap());
        assert!(r_version_satisfies(">=4.3.0", "4.6.0").unwrap());
        assert!(r_version_satisfies("^4.5", "4.6.0").unwrap());
        assert!(r_version_satisfies("*", "4.6.0").unwrap());
        // Garbage is an error, not "unsatisfied".
        assert!(r_version_satisfies("not-a-constraint", "4.5.3").is_err());
    }

    #[test]
    fn find_all_returns_something() {
        // On a dev machine, there should be at least one R installation.
        // This is not strictly guaranteed in CI but is a reasonable smoke test.
        let installations = find_all();
        // Just verify it doesn't panic and returns a Vec
        for inst in &installations {
            assert!(!inst.version.is_empty());
            assert!(!inst.binary.as_os_str().is_empty());
        }
    }

    #[test]
    fn r_installation_struct_fields() {
        let inst = fake_installation("4.5.0", true);
        assert_eq!(inst.version, "4.5.0");
        assert!(inst.managed);
        assert!(inst.binary.to_string_lossy().contains("4.5.0"));
    }

    #[test]
    fn find_r_binary_prefers_managed() {
        // This tests the logic indirectly — if there are managed + system R,
        // managed should be preferred. Since we can't mock find_all, we test
        // the fallback logic through find_r_binary without constraint.
        // Just verify it returns Ok (not Err) when R is available.
        if let Ok(binary) = find_r_binary(None) {
            assert!(binary.to_string_lossy().contains("R"));
        }
        // If R is not installed at all, that's fine — test is informational
    }

    #[test]
    fn find_r_binary_with_constraint() {
        // If we have R, a loose constraint should match
        if let Ok(binary) = find_r_binary(Some(">=3.0.0")) {
            assert!(binary.exists());
        }
    }

    #[test]
    fn find_r_binary_impossible_constraint() {
        // A constraint for R 99.0.0 should fail (no such version exists). This
        // function reads no `UVR_R_VERSION` or pin from the developer's machine.
        let result = find_r_binary_satisfying(">=99.0.0");
        if !find_all().is_empty() {
            assert!(result.is_err());
        }
    }
}
