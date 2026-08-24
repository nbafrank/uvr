use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn uvr_cmd() -> Command {
    Command::cargo_bin("uvr").unwrap()
}

fn init_project(name: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", name])
        .current_dir(dir.path())
        .assert()
        .success();
    dir
}

/// Path to the workspace-level test fixtures.
fn fixture(rel: &str) -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR = crates/uvr/
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..") // crates/
        .join("..") // workspace root
        .join("tests")
        .join("fixtures")
        .join(rel)
}

#[test]
fn test_init_creates_subdirectory() {
    // #56: `uvr init <name>` creates `<name>/` and initializes inside it.
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "test-project"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("test-project"));

    let subdir = dir.path().join("test-project");
    assert!(subdir.is_dir(), "subdirectory not created");
    assert!(subdir.join("uvr.toml").exists(), "uvr.toml not created");
    assert!(
        subdir.join(".uvr").join("library").exists(),
        ".uvr/library not created"
    );
    let content = fs::read_to_string(subdir.join("uvr.toml")).unwrap();
    assert!(content.contains("test-project"));
}

#[test]
fn test_init_here_uses_current_dir() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "in-place"])
        .current_dir(dir.path())
        .assert()
        .success();

    assert!(dir.path().join("uvr.toml").exists(), "uvr.toml not created");
    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(content.contains("in-place"));
}

#[test]
fn test_init_rejects_bound_unsupported_description_alias() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("DESCRIPTION"),
        "Package: parent\nImports: Alias\n\
         Remotes: Alias=url::https://example.com/pkg.tar.gz\n",
    )
    .unwrap();

    uvr_cmd()
        .args(["init", "--here"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported bound Remotes entry"));
    assert!(!dir.path().join("uvr.toml").exists());
}

#[test]
fn test_init_preserves_explicit_description_alias_in_manifest() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("DESCRIPTION"),
        "Package: parent\nImports: Alias\nRemotes: Alias=owner/repo@main\n",
    )
    .unwrap();

    uvr_cmd()
        .args(["init", "--here"])
        .current_dir(dir.path())
        .assert()
        .success();
    let manifest = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let parsed: uvr_core::manifest::Manifest = manifest.parse().unwrap();
    let dependency = parsed.dependencies.get("Alias").unwrap();
    assert_eq!(dependency.git(), Some("owner/repo"));
}

#[test]
fn test_init_with_r_version() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "my-proj", "--r-version", ">=4.3.0"])
        .current_dir(dir.path())
        .assert()
        .success();

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(content.contains(">=4.3.0"));
}

#[test]
fn test_init_fails_if_manifest_exists() {
    let dir = init_project("already-exists");
    uvr_cmd()
        .args(["init", "--here", "again"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn test_remove_nonexistent_does_not_crash() {
    let dir = init_project("test-remove");
    uvr_cmd()
        .args(["remove", "nonexistent-pkg"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn test_run_outside_project_uses_system_r() {
    // uvr run outside a project should succeed (falls back to system R)
    // and NOT print any "not inside a uvr project" error.
    let dir = TempDir::new().unwrap();
    // Run without a script → drops into interactive R, but with --no-save
    // the assertion just checks it doesn't error with a "project not found" message.
    // We can't run interactive R in CI, so just verify the error is R-level, not uvr-level.
    let output = uvr_cmd()
        .args(["run", "nonexistent_script_xyz.R"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("uvr project") && !stderr.contains("uvr.toml"),
        "unexpected uvr project error: {stderr}"
    );
}

#[test]
fn test_r_use_updates_manifest() {
    let dir = init_project("r-version-test");
    uvr_cmd()
        .args(["r", "use", ">=4.3.0"])
        .current_dir(dir.path())
        .assert()
        .success();

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(content.contains(">=4.3.0"));
}

#[test]
fn test_add_help_works() {
    uvr_cmd().args(["add", "--help"]).assert().success();
}

#[test]
fn test_upgrade_help_works() {
    uvr_cmd().args(["upgrade", "--help"]).assert().success();
}

#[test]
fn test_self_update_alias_works() {
    // Backward-compat: `uvr self-update` is a hidden alias for `uvr upgrade`.
    uvr_cmd().args(["self-update", "--help"]).assert().success();
}

#[test]
fn test_r_use_exact_writes_r_version_file() {
    let dir = init_project("pin-test");
    uvr_cmd()
        .args(["r", "use", "4.3.2"])
        .current_dir(dir.path())
        .assert()
        .success();

    let pin = dir.path().join(".r-version");
    assert!(
        pin.exists(),
        ".r-version not created by `uvr r use <exact>`"
    );
    let content = fs::read_to_string(&pin).unwrap();
    assert_eq!(content.trim(), "4.3.2");
}

#[test]
fn test_r_use_constraint_no_r_version_file() {
    let dir = init_project("constraint-test");
    uvr_cmd()
        .args(["r", "use", ">=4.3.0"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Constraint (not exact) should NOT create .r-version
    assert!(
        !dir.path().join(".r-version").exists(),
        ".r-version should not be created for a constraint"
    );
    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(content.contains(">=4.3.0"));
}

#[test]
fn test_r_pin_help_works() {
    uvr_cmd().args(["r", "pin", "--help"]).assert().success();
}

#[test]
fn test_r_install_advertises_install_dir() {
    // #89: `--install-dir` overrides UVR_R_INSTALL_DIR for one invocation.
    // The help text is the discoverable surface of that agreement.
    uvr_cmd()
        .args(["r", "install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--install-dir"))
        .stdout(predicate::str::contains("UVR_R_INSTALL_DIR"));
}

#[test]
fn test_r_install_rejects_an_empty_install_dir() {
    // An empty path would reach the downloader and die there as "Install
    // path <version> has no parent directory" — an error that never names
    // the flag. It cannot: clap's PathBuf parser refuses empty values at
    // the argument layer. This pins that guarantee so a parser change
    // cannot quietly reopen the hole.
    uvr_cmd()
        .args(["r", "install", "4.5.1", "--install-dir", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("a value is required"));
}

#[test]
fn test_sync_without_lockfile_fails() {
    let dir = init_project("no-lock-test");
    uvr_cmd()
        .args(["sync"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("uvr lock").or(predicate::str::contains("lockfile")));
}

#[test]
fn test_lockfile_round_trip() {
    let path = fixture("sample_project/uvr.lock");
    let content = fs::read_to_string(&path).unwrap();
    let lf: uvr_core::lockfile::Lockfile = content.parse().unwrap();
    assert_eq!(lf.r.version, "4.3.2");
    assert_eq!(lf.packages.len(), 6);
    assert!(lf.get_package("ggplot2").is_some());
}

#[test]
fn test_manifest_round_trip() {
    let path = fixture("sample_project/uvr.toml");
    let content = fs::read_to_string(&path).unwrap();
    let m: uvr_core::manifest::Manifest = content.parse().unwrap();
    assert_eq!(m.project.name, "sample-project");
    assert!(m.dependencies.contains_key("ggplot2"));
}

#[test]
fn test_add_no_lock_writes_github_subdirectory_dependency() {
    let dir = init_project("subdir-add");
    uvr_cmd()
        .args([
            "add",
            "--no-lock",
            "owner/repo@v1.0#subdirectory=pkgs/nested",
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let m: uvr_core::manifest::Manifest = content.parse().unwrap();
    let dep = m.dependencies.get("nested").expect("nested dependency");
    assert_eq!(dep.git(), Some("owner/repo"));
    assert_eq!(dep.subdirectory(), Some("pkgs/nested"));
    assert!(
        content.contains(r#"subdirectory = "pkgs/nested""#),
        "{content}"
    );
    assert!(content.contains(r#"rev = "v1.0""#), "{content}");
    assert!(!dir.path().join("uvr.lock").exists());
}

#[test]
fn test_add_rejects_an_unsafe_subdirectory_fragment() {
    let dir = init_project("subdir-reject");
    uvr_cmd()
        .args(["add", "--no-lock", "owner/repo#subdirectory=../escape"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("subdirectory"));

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(!content.contains("escape"), "{content}");
}

// ─── import ────────────────────────────────────────────────

#[test]
fn test_import_from_renv_lock() {
    let dir = TempDir::new().unwrap();
    let renv_lock = fixture("sample_renv.lock");
    fs::copy(&renv_lock, dir.path().join("renv.lock")).unwrap();

    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported from"))
        .stdout(predicate::str::contains("CRAN"))
        .stdout(predicate::str::contains("Bioconductor"))
        .stdout(predicate::str::contains("GitHub"));

    // uvr.toml should exist with imported deps
    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(content.contains("jsonlite"), "missing jsonlite");
    assert!(content.contains("rlang"), "missing rlang");
    assert!(content.contains("DESeq2"), "missing DESeq2");
    assert!(content.contains("testuser/myPkg"), "missing GitHub dep");
    assert!(content.contains("4.3.2"), "missing R version");

    // Library dir should exist
    assert!(dir.path().join(".uvr").join("library").exists());
}

#[test]
fn test_import_with_explicit_path() {
    let dir = TempDir::new().unwrap();
    let renv_lock = fixture("sample_renv.lock");

    uvr_cmd()
        .args(["import", renv_lock.to_str().unwrap()])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported from"));

    assert!(dir.path().join("uvr.toml").exists());
}

#[test]
fn test_import_merges_into_existing_manifest() {
    let dir = init_project("import-merge");
    let renv_lock = fixture("sample_renv.lock");
    fs::copy(&renv_lock, dir.path().join("renv.lock")).unwrap();

    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Merged from renv.lock"));
}

#[test]
fn test_import_fails_if_no_renv_lock() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("File not found"));
}

#[test]
fn test_import_preserves_github_remote_subdir() {
    use uvr_core::manifest::{DependencySpec, Manifest};

    let dir = TempDir::new().unwrap();
    let renv_lock = fixture("sample_renv_subdir.lock");
    fs::copy(&renv_lock, dir.path().join("renv.lock")).unwrap();

    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("GitHub"));

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let m: Manifest = content.parse().unwrap();

    let nested = m.dependencies.get("nested").expect("nested dependency");
    assert_eq!(nested.git(), Some("testuser/monorepo"));
    assert_eq!(nested.subdirectory(), Some("pkgs/nested"));
    let DependencySpec::Detailed(nested_detail) = nested else {
        panic!("nested should be a detailed dependency");
    };
    assert_eq!(
        nested_detail.rev.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );

    let rooted = m.dependencies.get("rooted").expect("rooted dependency");
    assert_eq!(rooted.git(), Some("testuser/rooted"));
    assert_eq!(rooted.subdirectory(), None);
}

#[test]
fn test_import_rejects_an_invalid_remote_subdir() {
    let dir = TempDir::new().unwrap();
    let renv_lock = r#"{
  "R": { "Version": "4.3.2", "Repositories": [] },
  "Packages": {
    "escape": {
      "Package": "escape",
      "Version": "0.1.0",
      "Source": "GitHub",
      "RemoteUsername": "testuser",
      "RemoteRepo": "monorepo",
      "RemoteSubdir": "../escape"
    }
  }
}"#;
    fs::write(dir.path().join("renv.lock"), renv_lock).unwrap();

    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("RemoteSubdir"));

    assert!(!dir.path().join("uvr.toml").exists());
}

#[test]
fn test_import_rejects_an_invalid_remote_subdir_without_remote_identity() {
    let dir = TempDir::new().unwrap();
    let renv_lock = r#"{
  "R": { "Version": "4.3.2", "Repositories": [] },
  "Packages": {
    "escape": {
      "Package": "escape",
      "Version": "0.1.0",
      "Source": "GitHub",
      "RemoteUsername": "testuser",
      "RemoteSubdir": "../escape"
    }
  }
}"#;
    fs::write(dir.path().join("renv.lock"), renv_lock).unwrap();

    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("RemoteSubdir"));

    assert!(!dir.path().join("uvr.toml").exists());
}

#[test]
fn test_github_subdirectory_round_trips_through_renv() {
    use uvr_core::manifest::{DependencySpec, Manifest};

    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("uvr.toml"),
        "[project]\nname = \"roundtrip\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("uvr.lock"),
        r#"[r]
version = "4.4.2"

[[package]]
name = "nested"
version = "0.1.0"
source = "github"
url = "https://api.github.com/repos/testuser/monorepo/tarball/0123456789abcdef0123456789abcdef01234567"
checksum = "git:0123456789abcdef0123456789abcdef01234567"
subdirectory = "pkgs/nested"
"#,
    )
    .unwrap();

    uvr_cmd()
        .args(["export", "--output", "renv.lock"])
        .current_dir(dir.path())
        .assert()
        .success();
    fs::remove_file(dir.path().join("uvr.toml")).unwrap();
    fs::remove_file(dir.path().join("uvr.lock")).unwrap();
    uvr_cmd()
        .args(["import", "--name", "roundtrip"])
        .current_dir(dir.path())
        .assert()
        .success();

    let manifest: Manifest = fs::read_to_string(dir.path().join("uvr.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let dep = manifest
        .dependencies
        .get("nested")
        .expect("nested dependency");
    assert_eq!(dep.git(), Some("testuser/monorepo"));
    assert_eq!(dep.subdirectory(), Some("pkgs/nested"));
    let DependencySpec::Detailed(detail) = dep else {
        panic!("nested should be a detailed dependency");
    };
    assert_eq!(
        detail.rev.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
}

// ─── export ────────────────────────────────────────────────

#[test]
fn test_export_requires_lockfile() {
    let dir = init_project("export-test");
    uvr_cmd()
        .args(["export"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("lockfile").or(predicate::str::contains("uvr.lock")));
}

#[test]
fn test_export_with_lockfile() {
    let dir = TempDir::new().unwrap();
    // Copy sample project with lockfile
    let manifest = fixture("sample_project/uvr.toml");
    let lockfile = fixture("sample_project/uvr.lock");
    fs::copy(&manifest, dir.path().join("uvr.toml")).unwrap();
    fs::copy(&lockfile, dir.path().join("uvr.lock")).unwrap();
    fs::create_dir_all(dir.path().join(".uvr").join("library")).unwrap();

    uvr_cmd()
        .args(["export"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Packages"));
}

// ─── tree ──────────────────────────────────────────────────

#[test]
fn test_tree_requires_lockfile() {
    let dir = init_project("tree-test");
    uvr_cmd()
        .args(["tree"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("lockfile").or(predicate::str::contains("uvr.lock")));
}

#[test]
fn test_tree_with_lockfile() {
    let dir = TempDir::new().unwrap();
    let manifest = fixture("sample_project/uvr.toml");
    let lockfile = fixture("sample_project/uvr.lock");
    fs::copy(&manifest, dir.path().join("uvr.toml")).unwrap();
    fs::copy(&lockfile, dir.path().join("uvr.lock")).unwrap();
    fs::create_dir_all(dir.path().join(".uvr").join("library")).unwrap();

    uvr_cmd()
        .args(["tree"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("ggplot2"));
}

#[test]
fn test_tree_with_depth() {
    let dir = TempDir::new().unwrap();
    let manifest = fixture("sample_project/uvr.toml");
    let lockfile = fixture("sample_project/uvr.lock");
    fs::copy(&manifest, dir.path().join("uvr.toml")).unwrap();
    fs::copy(&lockfile, dir.path().join("uvr.lock")).unwrap();
    fs::create_dir_all(dir.path().join(".uvr").join("library")).unwrap();

    uvr_cmd()
        .args(["tree", "--depth", "1"])
        .current_dir(dir.path())
        .assert()
        .success();
}

// ─── doctor ────────────────────────────────────────────────

#[test]
fn test_doctor_runs() {
    uvr_cmd().args(["doctor"]).assert().success();
}

// ─── completions ───────────────────────────────────────────

#[test]
fn test_completions_zsh() {
    uvr_cmd()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("compdef").or(predicate::str::contains("_uvr")));
}

#[test]
fn test_completions_bash() {
    uvr_cmd()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

// ─── update ────────────────────────────────────────────────

#[test]
fn test_update_dry_run_on_empty_project() {
    let dir = init_project("update-test");
    uvr_cmd()
        .args(["update", "--dry-run"])
        .current_dir(dir.path())
        .assert()
        .success()
        // `ui::warn` writes to stderr (warnings are diagnostic output).
        .stderr(predicate::str::contains("Dry run"));
}

// ─── cache ─────────────────────────────────────────────────

#[test]
fn test_cache_clean() {
    // Isolated dirs: this test used to run a REAL `uvr cache clean` against
    // the developer's ~/.uvr, wiping the whole package + download cache on
    // every `cargo test` run. The HOME/USERPROFILE overrides alone are not
    // enough — on Windows `dirs::home_dir()` resolves via the Known Folder
    // API, ignoring the child's env — so the explicit UVR_*_DIR overrides
    // are the load-bearing isolation.
    let home = TempDir::new().unwrap();
    let cache = home.path().join(".uvr").join("cache");
    let packages = home.path().join(".uvr").join("packages");
    let entry = packages
        .join("pkg-1.0-0123456789abcdef0123456789abcdef")
        .join("pkg");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("aabbccdd-pkg_1.0.tar.gz"), b"tar").unwrap();
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::write(entry.join("DESCRIPTION"), "Package: pkg\n").unwrap();

    uvr_cmd()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("UVR_CACHE_DIR", &cache)
        .env("UVR_PACKAGES_DIR", &packages)
        .args(["cache", "clean"])
        .assert()
        .success();

    assert!(
        std::fs::read_dir(&cache)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "seeded download cache should be emptied"
    );
    assert!(
        !entry.exists(),
        "seeded package cache entry should be removed"
    );
}

#[test]
fn test_cache_clean_filtered_no_match() {
    // Filtered clean with no matching entries reports and touches nothing.
    let home = TempDir::new().unwrap();
    let cache = home.path().join(".uvr").join("cache");
    let packages = home.path().join(".uvr").join("packages");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("aabbccdd-other_2.0.tar.gz"), b"tar").unwrap();

    uvr_cmd()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("UVR_CACHE_DIR", &cache)
        .env("UVR_PACKAGES_DIR", &packages)
        .args(["cache", "clean", "--package", "nomatch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No cache entries matched"));

    assert!(
        cache.join("aabbccdd-other_2.0.tar.gz").exists(),
        "non-matching tarball must survive a filtered clean"
    );
}

// ─── help ──────────────────────────────────────────────────

#[test]
fn test_import_help() {
    uvr_cmd()
        .args(["import", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("renv"));
}

// ─── sources / stub-server ─────────────────────────────────
//
// Gated to non-Windows. Windows' IPv4/IPv6 dual-stack loopback semantics
// race against `TcpListener::bind("127.0.0.1:0")` here, causing flaky
// failures unrelated to the logic under test. The pure parser tests in
// crates/uvr-core/src/registry/cran.rs cover the same code path
// (Built:/Path: extraction, is_binary_capable, etc.) on all platforms.

#[cfg(not(target_os = "windows"))]
/// Guard returned by [`spawn_rpkgs_stub`]. Dropping it (at the end of the test)
/// signals the server thread to stop and joins it, so the server's lifetime is
/// exactly the test's scope — no wall-clock self-destruct that can expire while
/// a slow parallel run is still connecting (which surfaced as flaky
/// "Connection refused" / hangs).
struct StubGuard {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_os = "windows"))]
impl Drop for StubGuard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(not(target_os = "windows"))]
/// Spin up a tiny HTTP server in a thread that serves files from
/// `tests/fixtures/rpkgs-stub/`. Returns the bound URL (`http://127.0.0.1:PORT`)
/// and a [`StubGuard`]; the server runs until the guard is dropped.
fn spawn_rpkgs_stub() -> (String, StubGuard) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let url = format!("http://{}", addr);

    let fixtures_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("rpkgs-stub");

    listener.set_nonblocking(true).expect("set_nonblocking");

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        // Serve until the guard signals stop. A generous wall-clock backstop
        // guards against a leak only if Drop somehow never runs (it does, even
        // on unwind) — it's NOT the primary shutdown, so it can't expire mid-test.
        let start = std::time::Instant::now();
        loop {
            if stop_thread.load(Ordering::SeqCst)
                || start.elapsed() > std::time::Duration::from_secs(120)
            {
                break;
            }
            match listener.accept() {
                Ok((mut socket, _)) => {
                    // The accepted socket can inherit the listener's non-blocking
                    // flag on macOS (observed in CI). Force it back to blocking and
                    // attach a short read timeout so we never hang on a malformed
                    // request; without this, read() returns WouldBlock immediately
                    // and we end up sending 404 + closing the connection before
                    // reqwest finishes its GET, surfacing as
                    // "received unexpected message from connection".
                    let _ = socket.set_nonblocking(false);
                    let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .trim_start_matches('/');
                    let safe_path = path
                        .split('/')
                        .filter(|s| !s.is_empty() && *s != ".." && *s != ".")
                        .collect::<Vec<_>>()
                        .join("/");
                    let file_path = fixtures_root.join(&safe_path);
                    if let Ok(body) = std::fs::read(&file_path) {
                        // `Connection: close` tells reqwest not to keep-alive
                        // against our one-shot per-connection handler.
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(header.as_bytes());
                        let _ = socket.write_all(&body);
                    } else {
                        let _ = socket.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    let _ = socket.shutdown(std::net::Shutdown::Write);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    (
        url,
        StubGuard {
            stop,
            handle: Some(handle),
        },
    )
}

#[cfg(not(target_os = "windows"))]
#[test]
fn lock_with_binary_capable_source_records_source_urls() {
    let (server_url, _server) = spawn_rpkgs_stub();

    let dir = init_project("stubproj");
    // Append a [[sources]] entry pointing at the stub server.
    let toml_path = dir.path().join("uvr.toml");
    let mut toml = fs::read_to_string(&toml_path).unwrap();
    toml.push_str(&format!(
        "\n[[sources]]\nname = \"rpkgs-stub\"\nurl = \"{}\"\n",
        server_url
    ));
    fs::write(&toml_path, toml).unwrap();

    // Add jsonlite (which the stub serves as a binary-capable entry).
    // Use --no-install so we only exercise lock-time behaviour — the stub
    // doesn't serve real tarballs, only PACKAGES.gz.
    uvr_cmd()
        .args(["add", "--no-install", "jsonlite"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Lockfile should record the source URL (binary upgrade happens at sync time).
    let lock = fs::read_to_string(dir.path().join("uvr.lock")).unwrap();
    assert!(
        lock.contains("jsonlite"),
        "lockfile should contain jsonlite: {lock}"
    );
    // The lockfile URL points at the stub's src/contrib (source URL),
    // NOT the upgraded-at-sync-time binary URL.
    assert!(
        lock.contains(&format!("{}/src/contrib/jsonlite", server_url)),
        "lockfile should record the source URL from rpkgs-stub: {lock}"
    );
}

#[test]
fn test_init_writes_activation_shims() {
    let dir = init_project("shimproj");
    for shim in ["activate", "activate.fish", "activate.ps1"] {
        let path = dir.path().join(".uvr").join(shim);
        assert!(path.exists(), "{shim} not written by init");
        let body = fs::read_to_string(&path).unwrap();
        // The staleness guarantee: shims delegate, never bake in paths.
        assert!(
            body.contains("uvr activate --emit"),
            "{shim} does not delegate to the binary"
        );
    }
    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.contains(".uvr/activate*"),
        "generated shims are not git-ignored: {gitignore}"
    );
}

#[test]
fn test_activate_emit_outside_project_fails() {
    // Must fail loudly so the shim's `&& eval` leaves the shell untouched.
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["activate", "--emit", "sh"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Not inside a uvr project"));
}

/// True when uvr can resolve an R interpreter. `activate --emit` resolves R
/// so the emitted script points at a real one, so these tests need one.
/// CI runs `cargo test` before its R-install step.
fn have_r() -> bool {
    std::process::Command::new("R")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn test_activate_emit_sh_is_valid_shell() {
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = init_project("emitproj");
    let out = uvr_cmd()
        .args(["activate", "--emit", "sh"])
        .current_dir(dir.path())
        .assert()
        .success();
    let script = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    // Parse it with a real shell so a syntax error can't ship.
    let mut sh = Command::new("sh");
    sh.arg("-n").write_stdin(script.clone()).assert().success();

    assert!(script.contains("R_LIBS_USER="));
    assert!(script.contains("deactivate()"));
    // Isolation: both Renviron gates must be blanked, or a user's
    // ~/.Renviron can re-point R_LIBS_USER out from under the project.
    assert!(script.contains("R_ENVIRON="));
    assert!(script.contains("R_ENVIRON_USER="));
}

#[test]
fn test_activate_emit_every_shell_succeeds() {
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = init_project("allshells");
    for shell in ["sh", "bash", "zsh", "fish", "powershell"] {
        uvr_cmd()
            .args(["activate", "--emit", shell])
            .current_dir(dir.path())
            .assert()
            .success()
            .stdout(predicate::str::contains("R_LIBS_USER"));
    }
}

/// True when a fish interpreter is on PATH.
fn have_fish() -> bool {
    std::process::Command::new("fish")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn test_activate_emit_fish_is_valid_fish() {
    // fish's emitter is the most structurally different of the three (list
    // PATH, `set -q`, function-based prompt) and was the only one never
    // handed to a real interpreter.
    if !have_fish() || !have_r() {
        eprintln!("skipping: need fish and R");
        return;
    }
    let dir = init_project("fishproj");
    let out = uvr_cmd()
        .args(["activate", "--emit", "fish"])
        .current_dir(dir.path())
        .assert()
        .success();
    let script = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    let f = dir.path().join("emitted.fish");
    fs::write(&f, &script).unwrap();
    Command::new("fish").arg("-n").arg(&f).assert().success();

    assert!(script.contains("set -gx R_LIBS_USER"));
    assert!(script.contains("function deactivate"));
}

#[test]
fn test_activate_write_shim_restores_deleted_shims() {
    let dir = init_project("restoreproj");
    let fish = dir.path().join(".uvr").join("activate.fish");
    fs::remove_file(&fish).unwrap();
    assert!(!fish.exists());

    uvr_cmd()
        .args(["activate", "--write-shim"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(fish.exists(), "--write-shim did not restore the fish shim");
}

/// True when a PowerShell interpreter is on PATH.
fn have_pwsh() -> bool {
    std::process::Command::new("pwsh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn test_activate_powershell_isolation_survives_a_real_shell() {
    // Runs wherever pwsh exists — notably the windows-latest CI runner.
    //
    // The load-bearing assertion is that `$env:R_LIBS_SITE` is *present and
    // empty* after the emitted script runs. Win32's SetEnvironmentVariable
    // deletes a variable assigned an empty string, and if PowerShell routes
    // through that on Windows, the three blanked isolation variables would
    // become absent and R would fall back to the system site library. Verified
    // correct on PowerShell 7.6 for Linux; this test is what would catch the
    // Windows case before a user does.
    if !have_pwsh() {
        eprintln!("skipping: pwsh not installed");
        return;
    }
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }

    let dir = init_project("psiso");
    let out = uvr_cmd()
        .args(["activate", "--emit", "powershell"])
        .current_dir(dir.path())
        .assert()
        .success();
    let script = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    let emitted = dir.path().join("emitted.ps1");
    fs::write(&emitted, &script).unwrap();

    let runner = dir.path().join("runner.ps1");
    fs::write(
        &runner,
        format!(
            r#". "{}"
Write-Output ("LIBS_USER=" + $env:R_LIBS_USER)
Write-Output ("SITE_PRESENT=" + (Test-Path Env:\R_LIBS_SITE))
Write-Output ("SITE_VALUE=[" + $env:R_LIBS_SITE + "]")
Write-Output ("ENVIRON_USER_PRESENT=" + (Test-Path Env:\R_ENVIRON_USER))
Write-Output ("PROJECT=" + $env:UVR_PROJECT)
deactivate
Write-Output ("AFTER_PROJECT_PRESENT=" + (Test-Path Env:\UVR_PROJECT))
Write-Output ("AFTER_DEACTIVATE_PRESENT=" + (Test-Path Function:\deactivate))
"#,
            emitted.display()
        ),
    )
    .unwrap();

    let res = std::process::Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&runner)
        .output()
        .expect("run pwsh");
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(
        res.status.success(),
        "pwsh failed: {}\n{}",
        String::from_utf8_lossy(&res.stderr),
        stdout
    );

    assert!(
        stdout.contains("SITE_PRESENT=True"),
        "R_LIBS_SITE was deleted rather than set empty — the system site \
         library is no longer shadowed:\n{stdout}"
    );
    assert!(stdout.contains("SITE_VALUE=[]"), "{stdout}");
    assert!(stdout.contains("ENVIRON_USER_PRESENT=True"), "{stdout}");
    assert!(stdout.contains("PROJECT=psiso"), "{stdout}");
    // deactivate must leave nothing of ours behind.
    assert!(stdout.contains("AFTER_PROJECT_PRESENT=False"), "{stdout}");
    assert!(
        stdout.contains("AFTER_DEACTIVATE_PRESENT=False"),
        "{stdout}"
    );
}

// ─── inline script headers (#181) ───────────────────────────

/// Write `source` to `script.R` in a fresh directory.
fn script_dir(source: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("script.R"), source).unwrap();
    dir
}

/// An R script that reports whether the *project* library is on the search
/// path, printing `LINKED` or `NOT_LINKED`.
///
/// The comparison happens inside R rather than by matching the path in Rust,
/// because a Rust-side `String::contains` cannot survive Windows: R prints
/// `.libPaths()` with forward slashes where `Path` renders backslashes, and
/// GitHub's Windows runners point `TEMP` at the 8.3 short form
/// (`C:\Users\RUNNER~1\…`) while R resolves and prints the long form
/// (`C:/Users/runneradmin/…`). Two different spellings of one directory.
///
/// Getting this wrong is worse than a red test: a `!contains` assertion that
/// can never match passes vacuously, so the isolation check would quietly
/// stop checking anything on the one platform where the profile suppression
/// is spelled differently.
const REPORTS_PROJECT_LIB_LINKAGE: &str = r#"
lib <- normalizePath(file.path(getwd(), ".uvr", "library"), winslash = "/", mustWork = FALSE)
paths <- normalizePath(.libPaths(), winslash = "/", mustWork = FALSE)
cat(if (lib %in% paths) "LINKED" else "NOT_LINKED", "\n")
cat(paths, sep = "\n")
"#;

#[test]
fn test_unterminated_script_header_is_a_hard_error() {
    // No closing `# ///` at all: every declared dependency would be silently
    // dropped and resurface as a missing-package failure somewhere far away.
    let dir = script_dir("# /// script\n# dependencies = [\"ggplot2\"]\n");
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Invalid script header in script.R",
        ))
        .stderr(predicate::str::contains("unterminated"));
}

#[test]
fn test_stray_code_inside_the_header_names_the_offending_line() {
    // The block *is* closed further down, so "unterminated" would be the
    // wrong diagnosis — the message must point at the line that cannot
    // belong to it.
    let dir = script_dir(
        "# /// script\n# dependencies = [\"ggplot2\"]\nlibrary(ggplot2)\n# ///\nprint(1)\n",
    );
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Invalid script header in script.R",
        ))
        .stderr(predicate::str::contains("library(ggplot2)"));
}

#[test]
fn test_a_spec_grammar_this_slice_cannot_honour_is_rejected() {
    // Passing `ggplot2>=3.4` through would reach the resolver as a literal
    // package name and fail with "Package not found: ggplot2>=3.4" plus a
    // nonsense `uvr add cran/ggplot2>=3.4@master` suggestion — naming
    // neither the cause nor the fix.
    let dir = script_dir("# /// script\n# dependencies = [\"ggplot2>=3.4\"]\n# ///\n");
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Invalid script header in script.R",
        ))
        .stderr(predicate::str::contains("not a plain package name"));
}

#[test]
fn test_malformed_toml_in_a_script_header_is_a_hard_error() {
    let dir = script_dir("# /// script\n# dependencies = [\n# ///\n");
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Invalid script header in script.R",
        ));
}

#[test]
fn test_a_second_header_block_is_a_hard_error() {
    // #181: a broken header is "never silently ignored". A stale second
    // block — the classic leftover of a bad merge — used to be discarded
    // without a word, its dependencies never provisioned.
    let dir = script_dir(
        "# /// script\n# dependencies = []\n# ///\n\
         print(1)\n\
         # /// script\n# dependencies = [\"nope\"]\n# ///\n",
    );
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Invalid script header in script.R",
        ))
        .stderr(predicate::str::contains("only one"));
}

#[test]
fn test_header_text_cannot_smuggle_ansi_into_uvr_output() {
    // A header is text you accept from a stranger. `\u001b` is legal TOML
    // that decodes to a live ESC; printed raw it can repaint or overwrite
    // uvr's own diagnostics. The r-pin warning is the interpolation site a
    // successfully-parsed header reaches, so it is the one exercised here
    // against the real binary.
    let dir = script_dir(
        "# /// script\n# r = \"\\u001b[31mINJECTED>=4.3\"\n# dependencies = []\n# ///\n",
    );
    // The warning fires before an interpreter is resolved, so stderr carries
    // it whether or not this machine has an R to continue with — the exit
    // status is deliberately not asserted.
    let output = uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\\u{1b}[31mINJECTED"),
        "escaped rendering missing: {stderr:?}"
    );
    assert!(
        !stderr.contains("\u{1b}[31mINJECTED"),
        "raw ESC reached the terminal: {stderr:?}"
    );
}

#[test]
fn test_header_errors_come_before_r_is_needed() {
    // The header is parsed before uvr looks for an interpreter, so the
    // message a user gets is about their typo — not about R being missing on
    // a machine where it is irrelevant to the failure.
    let dir = script_dir("# /// script\n# dependencies = [\"x\"]\n");
    let output = uvr_cmd()
        .args(["run", "script.R", "--r-version", "99.9.9"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid script header"),
        "expected a header error, got: {stderr}"
    );
    assert!(!stderr.contains("R not found"), "{stderr}");
}

#[test]
fn test_a_comment_divider_is_not_a_script_header() {
    // `# ///` is a plausible section divider. A script using one must not be
    // rejected as a broken header — it has no header at all, so this behaves
    // exactly as it did before inline headers existed (regression).
    let dir = script_dir("# ///\n# just a divider\nprint(1)\n");
    let output = uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("script header"), "{stderr}");
}

#[test]
fn test_headerless_script_does_not_gain_a_header_error() {
    // Regression: ordinary scripts are untouched by header detection.
    let dir = script_dir("library(stats)\nprint(1)\n");
    let output = uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("script header"), "{stderr}");
}

#[test]
#[ignore = "requires network access to CRAN/P3M and a managed R"]
fn test_headered_script_runs_standalone_in_an_empty_directory() {
    // The whole promise of #181: no project, no manifest, no setup — the
    // file carries its environment. Run with `cargo test -- --ignored`.
    let dir = script_dir(
        "# /// script\n\
         # dependencies = [\"jsonlite\"]\n\
         # ///\n\
         cat(jsonlite::toJSON(list(ok = TRUE)))\n",
    );
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"ok\""));
}

#[test]
#[ignore = "requires network access to CRAN/P3M and a managed R"]
fn test_headered_script_env_is_not_hijacked_by_uvr_library() {
    // Two features of this release collide here. `UVR_LIBRARY` (#97)
    // redirects a project's library, and it is meant to be exported
    // globally by people whose storage lives elsewhere. A script
    // environment is ephemeral and cache-owned, so it must ignore that
    // override entirely — otherwise the script's declared packages install
    // into the user's shared library while R is pointed at the (empty)
    // with-env dir, so the script fails *and* the shared library silently
    // accumulates packages nothing will ever prune.
    let dir = script_dir(
        "# /// script\n\
         # dependencies = [\"jsonlite\"]\n\
         # ///\n\
         cat(if (requireNamespace(\"jsonlite\", quietly = TRUE)) \"OK\" else \"MISSING\")\n",
    );
    let override_lib = TempDir::new().unwrap();
    // A dedicated cache dir matters: `ensure_with_env` returns early when
    // the env is already populated, so against a warm cache this test would
    // pass without ever exercising the install path the bug lives in.
    let cache = TempDir::new().unwrap();

    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .env("UVR_LIBRARY", override_lib.path())
        .env("UVR_CACHE_DIR", cache.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("OK"));

    // The override library must be untouched — not even the companion.
    let leaked: Vec<_> = fs::read_dir(override_lib.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leaked.is_empty(),
        "script env leaked into UVR_LIBRARY: {leaked:?}"
    );
}

#[test]
#[ignore = "requires network access to CRAN/P3M and a managed R"]
fn test_headered_script_ignores_the_surrounding_project() {
    // Run the same script inside a project that declares a *different*
    // package. If the project library leaked onto the search path, the
    // undeclared package would resolve and the script would wrongly succeed.
    let dir = init_project("leaky");
    fs::write(
        dir.path().join("script.R"),
        "# /// script\n\
         # dependencies = [\"jsonlite\"]\n\
         # ///\n\
         cat(if (requireNamespace(\"cli\", quietly = TRUE)) \"LEAKED\" else \"ISOLATED\")\n",
    )
    .unwrap();
    uvr_cmd()
        .args(["add", "cli"])
        .current_dir(dir.path())
        .assert()
        .success();

    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("ISOLATED"));
}

#[test]
fn test_headered_script_does_not_inherit_the_project_library() {
    // The isolation that makes a headered script portable is not delivered
    // by environment variables alone: uvr's own project `.Rprofile` runs
    // `.libPaths(unique(c(lib, .libPaths())))` at startup, which would put
    // the surrounding project's library *ahead* of the script's own
    // environment. An empty dependency list keeps this offline.
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = init_project("isolation");
    fs::write(
        dir.path().join("script.R"),
        format!("# /// script\n# dependencies = []\n# ///\n{REPORTS_PROJECT_LIB_LINKAGE}"),
    )
    .unwrap();

    let out = uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    assert!(
        stdout.contains("NOT_LINKED"),
        "the project library leaked into a headered script's search path:\n{stdout}"
    );
    assert!(
        stdout.contains("with-envs"),
        "expected the ephemeral environment on the search path:\n{stdout}"
    );
}

#[test]
fn test_headerless_script_still_gets_the_project_library() {
    // The converse regression: suppressing the startup profile is scoped to
    // script mode, so an ordinary `uvr run` inside a project still has its
    // library linked by `.Rprofile` exactly as before.
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = init_project("linked");
    fs::write(
        dir.path().join("script.R"),
        REPORTS_PROJECT_LIB_LINKAGE.trim_start(),
    )
    .unwrap();

    let out = uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    assert!(
        stdout.contains("LINKED") && !stdout.contains("NOT_LINKED"),
        "the project library is no longer linked for an ordinary run:\n{stdout}"
    );
}

#[test]
fn test_headered_script_ignores_a_surrounding_r_version_pin() {
    // `.r-version` is walked up from the working directory and outranks every
    // other signal, so without this a headered script would let whichever
    // project it happens to sit in choose its interpreter — and the R version
    // is part of the ephemeral environment's cache key, so the same file
    // would get a different set of packages per directory.
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = script_dir("# /// script\n# dependencies = []\n# ///\ncat(\"RAN\\n\")\n");
    fs::write(dir.path().join("plain.R"), "cat(\"RAN\\n\")\n").unwrap();
    // A version nobody has installed, so honouring the pin is unmistakable.
    fs::write(dir.path().join(".r-version"), "3.0.0\n").unwrap();

    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("RAN"));

    // The converse: an ordinary run still honours the pin, so the change is
    // scoped to script mode.
    uvr_cmd()
        .args(["run", "plain.R"])
        .current_dir(dir.path())
        .assert()
        .failure();
}

#[test]
fn test_an_unsupported_r_pin_in_a_header_is_reported_not_swallowed() {
    // `r` is parsed but not honoured until #183. Running against whichever R
    // happens to be around without saying so is the trap this guards.
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir =
        script_dir("# /// script\n# r = \">=4.3\"\n# dependencies = []\n# ///\ncat(\"RAN\\n\")\n");
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("RAN"))
        .stderr(predicate::str::contains("does not honour yet"));
}

// ─── IDE-mode scaffolding ─────────────────────────────────────────

#[test]
fn test_init_default_writes_rprofile_but_no_ide_config() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "plainproj"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_ide_positron_writes_vscode_settings() {
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "posproj", "--ide=positron"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".vscode").join("settings.json").exists());
    let settings = fs::read_to_string(dir.path().join(".vscode").join("settings.json")).unwrap();
    assert!(settings.contains("positron.r.interpreters.default"));
}

#[test]
fn test_init_ide_rstudio_writes_rprofile_but_no_vscode() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "rstudioproj", "--ide=rstudio"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_positron_env_writes_vscode_settings() {
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "envposproj"])
        .env("POSITRON", "1")
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".vscode").join("settings.json").exists());
}

#[test]
fn test_init_rstudio_env_writes_rprofile_but_no_vscode() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "envrstudioproj"])
        .env("RSTUDIO", "1")
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_unattended_overrides_positron_env() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "unattendedproj", "--unattended"])
        .env("POSITRON", "1")
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_no_ide_overrides_positron_env() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "noideproj", "--no-ide"])
        .env("POSITRON", "1")
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_bare_writes_only_manifest_and_library() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "bareproj", "--bare"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join("uvr.toml").exists());
    assert!(dir.path().join(".uvr").join("library").exists());
    assert!(!dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".gitignore").exists());
    assert!(!dir.path().join(".uvr").join("activate").exists());
    assert!(!dir.path().join(".vscode").exists());
    let manifest = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(manifest.contains("bare = true"));
}
