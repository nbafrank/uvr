use std::path::PathBuf;
use std::process::Command;

use crate::error::{Result, UvrError};
use crate::project::read_r_version_pin_from;
use crate::resolver::normalize_version;

/// Discovered R installation.
#[derive(Debug, Clone)]
pub struct RInstallation {
    pub binary: PathBuf,
    pub version: String,
    pub managed: bool, // true = installed by uvr
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
/// 1. `.r-version` file (exact pin, walked up from cwd)
/// 2. `version_constraint` from `uvr.toml` (semver requirement)
/// 3. Any managed installation, then system R
pub fn find_r_binary(version_constraint: Option<&str>) -> Result<PathBuf> {
    resolve_r_binary(version_constraint, true)
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
pub fn find_r_binary_ignoring_pin(version_constraint: Option<&str>) -> Result<PathBuf> {
    resolve_r_binary(version_constraint, false)
}

fn resolve_r_binary(version_constraint: Option<&str>, honor_pin: bool) -> Result<PathBuf> {
    let installations = find_all();

    if installations.is_empty() {
        return Err(UvrError::RNotFound);
    }

    // 1. Honour .r-version exact pin
    let pin = honor_pin
        .then(|| {
            // If the working directory cannot be resolved (deleted, or a
            // parent is unreadable), `.` still finds a pin in the directory
            // itself, but the walk cannot go up to its parents. The old empty
            // path behaved the same, without telling the user.
            let cwd = std::env::current_dir().unwrap_or_else(|e| {
                tracing::warn!(
                    "Cannot determine the working directory ({e}); a .r-version pin \
                     in a parent directory will not be found"
                );
                PathBuf::from(".")
            });
            read_r_version_pin_from(&cwd)
        })
        .flatten();
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
        // The pin always wins over the uvr.toml constraint, but silent drift
        // between the two is confusing (`uvr r use 4.3.0` after setting
        // `r_version = "^4.5"`, #156) — surface it without changing precedence.
        if let Some(constraint) = version_constraint {
            if pin_conflicts_with_constraint(&resolved, constraint) {
                tracing::warn!(
                    ".r-version pins R {pinned} (resolved to {resolved}), which does not \
                     satisfy the uvr.toml constraint `{constraint}`; the pin takes \
                     precedence. Update .r-version or uvr.toml to bring them back in sync."
                );
            }
        }
        return Ok(bin);
    }

    // 2. Honour semver constraint from uvr.toml
    if let Some(constraint) = version_constraint {
        let req = crate::resolver::parse_version_req(constraint)?;
        // Probe matches in version-descending order and skip broken installs,
        // mirroring case (3) below.
        let mut matches: Vec<&RInstallation> = installations
            .iter()
            .filter(|inst| {
                let norm = normalize_version(&inst.version);
                semver::Version::parse(&norm)
                    .map(|v| req.matches(&v))
                    .unwrap_or(false)
            })
            .collect();
        matches.sort_by(|a, b| version_cmp(&b.version, &a.version));
        for inst in matches {
            if query_r_version(&inst.binary).is_some() {
                return Ok(inst.binary.clone());
            }
        }
        let installed = installations
            .iter()
            .map(|i| i.version.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let installed = if installed.is_empty() {
            "none".to_string()
        } else {
            installed
        };
        return Err(UvrError::RVersionUnsatisfied {
            constraint: constraint.to_string(),
            installed,
        });
    }

    // 3. Prefer managed installation, fall back to first system R.
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
            return Ok(inst.binary.clone());
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
    let Ok(req) = crate::resolver::parse_version_req(constraint) else {
        return false;
    };
    let norm = normalize_version(resolved_version);
    match semver::Version::parse(&norm) {
        Ok(v) => !req.matches(&v),
        Err(_) => false,
    }
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
    let output = Command::new(binary)
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
        ])
        .output()
        .ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // A constraint for R 99.0.0 should fail (no such version exists)
        let result = find_r_binary(Some(">=99.0.0"));
        if !find_all().is_empty() {
            assert!(result.is_err());
        }
    }
}
