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
        .args(["init", name])
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
fn test_init_creates_manifest() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "test-project"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("test-project"));

    assert!(dir.path().join("uvr.toml").exists(), "uvr.toml not created");
    assert!(
        dir.path().join(".uvr").join("library").exists(),
        ".uvr/library not created"
    );

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(content.contains("test-project"));
}

#[test]
fn test_init_with_r_version() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "my-proj", "--r-version", ">=4.3.0"])
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
        .args(["init", "again"])
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
fn test_import_fails_if_manifest_exists() {
    let dir = init_project("import-conflict");
    let renv_lock = fixture("sample_renv.lock");
    fs::copy(&renv_lock, dir.path().join("renv.lock")).unwrap();

    uvr_cmd()
        .args(["import"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("uvr.toml already exists"));
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
        .stdout(predicate::str::contains("Dry run"));
}

// ─── cache ─────────────────────────────────────────────────

#[test]
fn test_cache_clean() {
    uvr_cmd().args(["cache", "clean"]).assert().success();
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
