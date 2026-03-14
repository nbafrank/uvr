use std::fs;
use assert_cmd::Command;
use predicates::prelude::*;
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
    assert!(dir.path().join(".uvr").join("library").exists(), ".uvr/library not created");

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
fn test_run_without_project_fails() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["run", "script.R"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("uvr project").or(predicate::str::contains("uvr.toml")));
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
    uvr_cmd()
        .args(["add", "--help"])
        .assert()
        .success();
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
    assert!(pin.exists(), ".r-version not created by `uvr r use <exact>`");
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
    uvr_cmd()
        .args(["r", "pin", "--help"])
        .assert()
        .success();
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
    let lf = uvr_core::lockfile::Lockfile::from_str(&content).unwrap();
    assert_eq!(lf.r.version, "4.3.2");
    assert_eq!(lf.packages.len(), 6);
    assert!(lf.get_package("ggplot2").is_some());
}

#[test]
fn test_manifest_round_trip() {
    let path = fixture("sample_project/uvr.toml");
    let content = fs::read_to_string(&path).unwrap();
    let m = uvr_core::manifest::Manifest::from_str(&content).unwrap();
    assert_eq!(m.project.name, "sample-project");
    assert!(m.dependencies.contains_key("ggplot2"));
}
