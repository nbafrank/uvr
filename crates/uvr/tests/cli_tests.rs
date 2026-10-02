use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn uvr_cmd() -> Command {
    let mut cmd = Command::cargo_bin("uvr").unwrap();
    // The binary inherits the parent environment, so running the suite from a
    // Positron/RStudio terminal — or with UVR_UNATTENDED/UVR_NO_COMPANION
    // exported — would change these tests' results. Strip the vars that drive
    // IDE detection and headless mode. Tests that need them set them
    // explicitly afterwards; a later `.env()` overrides this.
    for var in ["POSITRON", "RSTUDIO", "UVR_UNATTENDED", "UVR_NO_COMPANION"] {
        cmd.env_remove(var);
    }
    cmd
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

// ─── path dependencies (#188) ──────────────────────────────

/// A project at `<tmp>/proj` with a pure-R package `tinypkg` checked out at
/// `<tmp>/tiny-checkout` (the directory name is not the package name).
fn project_with_sibling_package() -> TempDir {
    let dir = TempDir::new().unwrap();
    let pkg = dir.path().join("tiny-checkout");
    fs::create_dir_all(pkg.join("R")).unwrap();
    fs::write(
        pkg.join("DESCRIPTION"),
        "Package: tinypkg\nVersion: 0.1-0\nTitle: Tiny\nDescription: Tiny.\n\
         License: MIT\nEncoding: UTF-8\n",
    )
    .unwrap();
    fs::write(pkg.join("NAMESPACE"), "export(hello)\n").unwrap();
    fs::write(
        pkg.join("R").join("hello.R"),
        "hello <- function() \"hello from tinypkg\"\n",
    )
    .unwrap();
    uvr_cmd()
        .args(["init", "proj"])
        .current_dir(dir.path())
        .assert()
        .success();
    dir
}

#[test]
fn test_add_no_lock_records_a_manifest_relative_path_dependency() {
    let dir = project_with_sibling_package();
    let proj = dir.path().join("proj");
    let sub = proj.join("analysis");
    fs::create_dir(&sub).unwrap();

    // Typed from a subdirectory: recorded relative to uvr.toml, keyed by
    // the DESCRIPTION name.
    uvr_cmd()
        .args(["add", "--no-lock", "../../tiny-checkout"])
        .current_dir(&sub)
        .assert()
        .success()
        .stdout(predicate::str::contains("tinypkg"));

    let content = fs::read_to_string(proj.join("uvr.toml")).unwrap();
    let m: uvr_core::manifest::Manifest = content.parse().unwrap();
    assert_eq!(
        m.dependencies["tinypkg"].path(),
        Some("../tiny-checkout"),
        "{content}"
    );
    assert!(!proj.join("uvr.lock").exists());
}

#[test]
fn test_add_rejects_missing_and_non_package_directories() {
    let dir = project_with_sibling_package();
    let proj = dir.path().join("proj");
    fs::create_dir(dir.path().join("not-a-package")).unwrap();
    let before = fs::read_to_string(proj.join("uvr.toml")).unwrap();

    for (spec, message) in [
        ("../missing", "'../missing' does not exist"),
        ("../not-a-package", "is not an R package"),
    ] {
        uvr_cmd()
            .args(["add", "--no-lock", spec])
            .current_dir(&proj)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));
    }
    assert_eq!(fs::read_to_string(proj.join("uvr.toml")).unwrap(), before);
}

#[test]
fn test_add_does_not_treat_an_existing_directory_as_a_path_without_path_syntax() {
    let dir = init_project("no-sniff");
    // A checkout that happens to be named like a GitHub spec.
    let checkout = dir.path().join("tidyverse").join("ggplot2");
    fs::create_dir_all(&checkout).unwrap();
    fs::write(
        checkout.join("DESCRIPTION"),
        "Package: ggplot2\nVersion: 9.9.9\n",
    )
    .unwrap();

    uvr_cmd()
        .args(["add", "--no-lock", "tidyverse/ggplot2"])
        .current_dir(dir.path())
        .assert()
        .success();
    let m: uvr_core::manifest::Manifest = fs::read_to_string(dir.path().join("uvr.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(m.dependencies["ggplot2"].git(), Some("tidyverse/ggplot2"));
    assert_eq!(m.dependencies["ggplot2"].path(), None);

    // Only explicit path syntax selects the directory.
    uvr_cmd()
        .args(["add", "--no-lock", "./tidyverse/ggplot2"])
        .current_dir(dir.path())
        .assert()
        .success();
    let m: uvr_core::manifest::Manifest = fs::read_to_string(dir.path().join("uvr.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        m.dependencies["ggplot2"].path(),
        Some("./tidyverse/ggplot2")
    );
}

/// Hand-written manifest + lock for the sibling package, so the install
/// needs no index fetch.
fn write_local_lock(proj: &std::path::Path, path: &str) {
    let mut toml = fs::read_to_string(proj.join("uvr.toml")).unwrap();
    toml.push_str(&format!(
        "\n[dependencies]\ntinypkg = {{ path = \"{path}\" }}\n"
    ));
    fs::write(proj.join("uvr.toml"), toml).unwrap();
    fs::write(
        proj.join("uvr.lock"),
        format!(
            "[r]\nversion = \"*\"\n\n[[package]]\nname = \"tinypkg\"\nversion = \"0.1.0\"\n\
             source = \"local:{path}\"\nraw_version = \"0.1-0\"\n"
        ),
    )
    .unwrap();
}

#[test]
fn test_sync_installs_a_path_package_from_its_directory() {
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let dir = project_with_sibling_package();
    let proj = dir.path().join("proj");
    write_local_lock(&proj, "../tiny-checkout");

    uvr_cmd()
        .args(["sync"])
        .current_dir(&proj)
        .assert()
        .success();
    let installed = proj.join(".uvr/library/tinypkg");
    assert!(installed.join("Meta/package.rds").exists());
    let desc = fs::read_to_string(installed.join("DESCRIPTION")).unwrap();
    assert!(desc.contains("Version: 0.1-0"), "{desc}");

    // An edit without a version bump still reaches the library.
    fs::write(
        dir.path().join("tiny-checkout/R/hello.R"),
        "hello <- function() \"edited\"\n",
    )
    .unwrap();
    uvr_cmd()
        .args(["sync"])
        .current_dir(&proj)
        .assert()
        .success();
    fs::write(proj.join("hello.R"), "cat(tinypkg::hello())\n").unwrap();
    uvr_cmd()
        .args(["run", "hello.R"])
        .current_dir(&proj)
        .assert()
        .success()
        .stdout(predicate::str::contains("edited"));
}

#[test]
fn test_sync_fails_clearly_when_a_locked_path_is_missing() {
    let dir = project_with_sibling_package();
    let proj = dir.path().join("proj");
    write_local_lock(&proj, "../moved-away");

    // Fails before any download, whether or not R is installed.
    uvr_cmd()
        .args(["sync"])
        .current_dir(&proj)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("'../moved-away' does not exist")
                .or(predicate::str::contains("R not found")),
        );
}

#[test]
fn test_frozen_sync_warns_that_a_path_lock_is_not_reproducible() {
    let dir = project_with_sibling_package();
    let proj = dir.path().join("proj");
    // A missing directory makes the frozen re-resolve fail fast; the warning
    // must already be out by then.
    write_local_lock(&proj, "../moved-away");

    uvr_cmd()
        .args(["sync", "--frozen"])
        .current_dir(&proj)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "uvr.lock contains local path dependencies (tinypkg); it is not reproducible \
             across machines",
        ));
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
/// [`spawn_stub`] over `tests/fixtures/rpkgs-stub/`.
fn spawn_rpkgs_stub() -> (String, StubGuard) {
    spawn_stub(fixture("rpkgs-stub"), None)
}

#[cfg(not(target_os = "windows"))]
/// [`spawn_rpkgs_stub`] for any `fixtures_root`. With `required_auth`, a
/// request whose `Authorization` header is not exactly that value gets a
/// 401, as a private repository would answer (#185).
fn spawn_stub(
    fixtures_root: std::path::PathBuf,
    required_auth: Option<String>,
) -> (String, StubGuard) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let url = format!("http://{}", addr);

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
                    let authorized = required_auth.as_deref().is_none_or(|want| {
                        req.lines().any(|line| {
                            line.split_once(':').is_some_and(|(k, v)| {
                                k.eq_ignore_ascii_case("authorization") && v.trim() == want
                            })
                        })
                    });
                    if !authorized {
                        let _ = socket.write_all(
                            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    } else if let Ok(body) = std::fs::read(&file_path) {
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

#[cfg(not(target_os = "windows"))]
#[test]
fn frozen_sync_that_bails_writes_no_scaffolding() {
    // Regression for the ordering of the `--frozen` staleness check in
    // `sync::run_inner`: the project-plumbing writes (`.Rprofile`, ...) used
    // to run *before* the check, so a frozen sync that bailed on a stale
    // lockfile still dirtied the working tree. The write block now runs after
    // the check, so a bail must not write any scaffolding.
    let (server_url, _server) = spawn_rpkgs_stub();

    let dir = init_project("frozen-nowrite");
    let toml_path = dir.path().join("uvr.toml");
    let mut toml = fs::read_to_string(&toml_path).unwrap();
    toml.push_str(&format!(
        "\n[[sources]]\nname = \"rpkgs-stub\"\nurl = \"{}\"\n",
        server_url
    ));
    fs::write(&toml_path, toml).unwrap();

    // Lock the empty dependency set, then make the manifest stale by adding a
    // dependency the lockfile doesn't know about.
    uvr_cmd()
        .args(["lock"])
        .current_dir(dir.path())
        .assert()
        .success();
    let mut manifest = uvr_core::manifest::Manifest::from_file(&toml_path).unwrap();
    manifest.add_dep(
        "jsonlite".into(),
        uvr_core::manifest::DependencySpec::Version("*".into()),
        false,
    );
    manifest.write(&toml_path).unwrap();

    // `init` wrote `.Rprofile`; remove it so the test can tell whether a
    // failing `--frozen` sync recreates it.
    fs::remove_file(dir.path().join(".Rprofile")).unwrap();

    uvr_cmd()
        .args(["sync", "--frozen"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("out of date"));

    assert!(
        !dir.path().join(".Rprofile").exists(),
        "a --frozen sync that bailed wrote .Rprofile; the plumbing writes must run after the staleness check"
    );
}

// ─── authenticated repositories (#185) ─────────────────────

#[cfg(not(target_os = "windows"))]
/// A project whose `[[sources]]` entry `private-repo` is a private CRAN-like
/// repository holding one pure-R package, `uvrauthpkg`, served only to
/// requests with `Authorization: <required_auth>`. `url_userinfo` (e.g.
/// `"user:pass@"`) is written into the source URL. Returns the project, a
/// store for the repository and an isolated cache (with an empty CRAN index,
/// so resolution needs no network), the repository URL, and its server.
fn private_repo_project(
    required_auth: &str,
    url_userinfo: &str,
) -> (TempDir, TempDir, String, StubGuard) {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    let store = TempDir::new().unwrap();
    let contrib = store.path().join("repo").join("src").join("contrib");
    fs::create_dir_all(&contrib).unwrap();
    let packages = "Package: uvrauthpkg\nVersion: 0.1.0\nNeedsCompilation: no\n";
    fs::write(contrib.join("PACKAGES"), packages).unwrap();
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(packages.as_bytes()).unwrap();
    fs::write(contrib.join("PACKAGES.gz"), gz.finish().unwrap()).unwrap();

    let tarball = fs::File::create(contrib.join("uvrauthpkg_0.1.0.tar.gz")).unwrap();
    let mut tar = tar::Builder::new(GzEncoder::new(tarball, Compression::default()));
    for (path, body) in [
        (
            "uvrauthpkg/DESCRIPTION",
            "Package: uvrauthpkg\nVersion: 0.1.0\nTitle: Test\nDescription: Test package.\n\
             License: MIT\nAuthor: uvr\nMaintainer: uvr <uvr@example.com>\nNeedsCompilation: no\n",
        ),
        ("uvrauthpkg/NAMESPACE", "export(hello)\n"),
        ("uvrauthpkg/R/hello.R", "hello <- function() \"hi\"\n"),
    ] {
        let mut header = tar::Header::new_gnu();
        // R's own untar rejects the legacy NUL entry type.
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, path, body.as_bytes()).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();

    let cache = store.path().join("cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("cran-packages.txt"), "").unwrap();

    let (url, guard) = spawn_stub(store.path().join("repo"), Some(required_auth.to_string()));
    let dir = init_project("authproj");
    let toml_path = dir.path().join("uvr.toml");
    let mut toml = fs::read_to_string(&toml_path).unwrap();
    let source_url = url.replacen("http://", &format!("http://{url_userinfo}"), 1);
    toml.push_str(&format!(
        "\n[[sources]]\nname = \"private-repo\"\nurl = \"{source_url}\"\n"
    ));
    fs::write(&toml_path, toml).unwrap();
    (dir, store, url, guard)
}

#[cfg(not(target_os = "windows"))]
fn private_repo_cmd(dir: &TempDir, store: &TempDir, env: &[(&str, &str)]) -> Command {
    let mut cmd = uvr_cmd();
    cmd.current_dir(dir.path())
        .env("UVR_CACHE_DIR", store.path().join("cache"))
        .env("UVR_PACKAGES_DIR", store.path().join("packages"))
        .env("UVR_NO_BINARY", "1")
        // Not the user's ~/.netrc; a test may write this file.
        .env("NETRC", store.path().join("netrc"))
        .env_remove("UVR_REPOS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

#[cfg(not(target_os = "windows"))]
fn output_text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[cfg(not(target_os = "windows"))]
/// Lock `uvrauthpkg` from the private repository and, when R is available,
/// install it. `secret` must never appear in uvr's output (`-v` included)
/// nor, unless it was written into the URL, in uvr.toml or uvr.lock.
fn assert_private_repo_installs(
    required_auth: &str,
    url_userinfo: &str,
    env: &[(&str, &str)],
    secret: &str,
) {
    let (dir, store, url, _server) = private_repo_project(required_auth, url_userinfo);

    let out = private_repo_cmd(&dir, &store, env)
        .args(["add", "--no-install", "uvrauthpkg"])
        .output()
        .unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    assert!(!text.contains(secret), "{text}");
    let lock = fs::read_to_string(dir.path().join("uvr.lock")).unwrap();
    assert!(lock.contains("private-repo"), "{lock}");
    assert!(
        lock.contains("/src/contrib/uvrauthpkg_0.1.0.tar.gz"),
        "{lock}"
    );
    if url_userinfo.is_empty() {
        assert!(lock.contains(&url), "{lock}");
        assert!(!lock.contains(secret), "{lock}");
        let toml = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
        assert!(!toml.contains(secret), "{toml}");
    }

    if !have_r() {
        eprintln!("skipping the install half: no R on PATH");
        return;
    }
    let out = private_repo_cmd(&dir, &store, env)
        .args(["sync", "-v"])
        .output()
        .unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    // `-v` prints each package's download URL: it must be redacted.
    assert!(text.contains("uvrauthpkg 0.1.0"), "{text}");
    assert!(!text.contains(secret), "{text}");
    assert!(
        dir.path()
            .join(".uvr/library/uvrauthpkg/DESCRIPTION")
            .exists(),
        "uvrauthpkg must be installed: {text}"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn private_repository_with_bearer_token_resolves_and_installs() {
    assert_private_repo_installs(
        "Bearer tok-185-secret",
        "",
        &[("UVR_REPO_TOKEN_PRIVATE_REPO", "tok-185-secret")],
        "tok-185-secret",
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn private_repository_with_basic_auth_resolves_and_installs() {
    assert_private_repo_installs(
        // base64("alice:s3cret-185")
        "Basic YWxpY2U6czNjcmV0LTE4NQ==",
        "",
        &[
            ("UVR_REPO_USER_PRIVATE_REPO", "alice"),
            ("UVR_REPO_PASSWORD_PRIVATE_REPO", "s3cret-185"),
        ],
        "s3cret-185",
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn private_repository_url_credentials_are_redacted() {
    assert_private_repo_installs(
        "Basic YWxpY2U6czNjcmV0LTE4NQ==",
        "alice:s3cret-185@",
        &[],
        "s3cret-185",
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn private_repository_with_netrc_resolves_and_installs() {
    let netrc_dir = TempDir::new().unwrap();
    let netrc = netrc_dir.path().join("netrc");
    fs::write(
        &netrc,
        "# uvr test\nmachine 127.0.0.1\n  login alice\n  password n3trc-186\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&netrc, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_private_repo_installs(
        // base64("alice:n3trc-186")
        "Basic YWxpY2U6bjN0cmMtMTg2",
        "",
        &[("NETRC", netrc.to_str().unwrap())],
        "n3trc-186",
    );
}

#[cfg(unix)]
#[test]
fn private_repository_netrc_permissions_and_precedence() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, store, _url, _server) = private_repo_project("Basic YWxpY2U6bjN0cmMtMTg2", "");
    // `private_repo_cmd` points NETRC here.
    let netrc = store.path().join("netrc");
    fs::write(&netrc, "machine 127.0.0.1 login alice password n3trc-186\n").unwrap();
    let add = |env: &[(&str, &str)]| {
        let out = private_repo_cmd(&dir, &store, env)
            .args(["add", "--no-install", "uvrauthpkg"])
            .output()
            .unwrap();
        (out.status.success(), output_text(&out))
    };

    // Other users can read it: uvr warns, skips it, and carries on
    // without credentials.
    fs::set_permissions(&netrc, fs::Permissions::from_mode(0o644)).unwrap();
    let (ok, text) = add(&[]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("users other than you can access it"),
        "{text}"
    );
    assert!(text.contains("it needs credentials"), "{text}");
    assert!(
        text.contains("add a `machine 127.0.0.1` entry to"),
        "{text}"
    );
    assert!(!text.contains("n3trc-186"), "{text}");

    fs::set_permissions(&netrc, fs::Permissions::from_mode(0o600)).unwrap();
    // An env credential takes precedence over netrc.
    let (ok, text) = add(&[("UVR_REPO_TOKEN_PRIVATE_REPO", "wrong-186")]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("refused the token in UVR_REPO_TOKEN_PRIVATE_REPO"),
        "{text}"
    );
    // Without it, the netrc entry authenticates.
    let (ok, text) = add(&[]);
    assert!(ok, "{text}");
    assert!(!text.contains("users other than you"), "{text}");
    assert!(!text.contains("n3trc-186"), "{text}");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn private_repository_refusal_says_how_to_authenticate() {
    let (dir, store, _url, _server) = private_repo_project("Bearer tok-185-secret", "");
    let cases: [(&[(&str, &str)], &str); 2] = [
        (&[], "it needs credentials. Set UVR_REPO_TOKEN_PRIVATE_REPO"),
        (
            // A wrong token is refused, and never echoed back.
            &[("UVR_REPO_TOKEN_PRIVATE_REPO", "wrong-185")],
            "refused the token in UVR_REPO_TOKEN_PRIVATE_REPO",
        ),
    ];
    for (env, expected) in cases {
        let out = private_repo_cmd(&dir, &store, env)
            .args(["add", "--no-install", "uvrauthpkg"])
            .output()
            .unwrap();
        let text = output_text(&out);
        assert!(!out.status.success(), "{text}");
        assert!(text.contains("repository 'private-repo'"), "{text}");
        assert!(text.contains("401 Unauthorized"), "{text}");
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains("wrong-185"), "{text}");
    }
}

#[test]
fn test_add_source_refuses_credentials_in_the_url() {
    let dir = init_project("credsrc");
    let before = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let out = uvr_cmd()
        .args([
            "add",
            "--no-install",
            "--source",
            "https://alice:s3cret-185@ppm.corp.example:8443/cran/latest",
            "jsonlite",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("UVR_REPO_TOKEN_PPM_CORP_EXAMPLE"),
        "{stderr}"
    );
    assert!(
        stderr.contains("https://***@ppm.corp.example:8443"),
        "{stderr}"
    );
    assert!(!stderr.contains("s3cret-185"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.path().join("uvr.toml")).unwrap(),
        before
    );
}

// ─── any git host (#190) ───────────────────────────────────
//
// Gated to non-Windows like the other networked-style CLI tests; the
// unit tests in git_generic.rs run the same git calls on every platform.

#[cfg(not(target_os = "windows"))]
fn have_git() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(not(target_os = "windows"))]
fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=uvr", "-c", "user.email=uvr@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[cfg(not(target_os = "windows"))]
/// A git repository holding the pure-R package `uvrgitpkg`: tag `v0.1.0`
/// at version 0.1.0, then 0.2.0 on `main`. Returns the store, the
/// `file://` URL, and the tagged commit. The store also holds an isolated
/// cache with an empty CRAN index, so resolution needs no network.
fn git_repo_store() -> (TempDir, String, String) {
    let store = TempDir::new().unwrap();
    let repo = store.path().join("repo");
    fs::create_dir_all(repo.join("R")).unwrap();
    let description = |version: &str| {
        format!(
            "Package: uvrgitpkg\nVersion: {version}\nTitle: Test\nDescription: Test package.\n\
             License: MIT\nAuthor: uvr\nMaintainer: uvr <uvr@example.com>\nNeedsCompilation: no\n"
        )
    };
    fs::write(repo.join("DESCRIPTION"), description("0.1.0")).unwrap();
    fs::write(repo.join("NAMESPACE"), "export(hello)\n").unwrap();
    fs::write(repo.join("R/hello.R"), "hello <- function() \"hi\"\n").unwrap();
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-q", "-m", "first"]);
    git_in(&repo, &["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
    let tagged = git_in(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("DESCRIPTION"), description("0.2.0")).unwrap();
    git_in(&repo, &["commit", "-q", "-am", "second"]);

    let cache = store.path().join("cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("cran-packages.txt"), "").unwrap();
    let url = format!("file://{}", repo.display());
    (store, url, tagged)
}

#[cfg(not(target_os = "windows"))]
fn git_project_cmd(dir: &TempDir, store: &TempDir) -> Command {
    let mut cmd = uvr_cmd();
    cmd.current_dir(dir.path())
        .env("UVR_CACHE_DIR", store.path().join("cache"))
        .env("UVR_PACKAGES_DIR", store.path().join("packages"))
        .env("UVR_NO_BINARY", "1")
        .env("NETRC", store.path().join("netrc"))
        .env_remove("UVR_REPOS");
    cmd
}

#[cfg(not(target_os = "windows"))]
#[test]
fn git_dependency_locks_installs_and_resyncs() {
    if !have_git() {
        eprintln!("skipping: no git on PATH");
        return;
    }
    let (store, url, tagged) = git_repo_store();
    let dir = init_project("gitproj");
    let spec = format!("git::{url}@v0.1.0");

    let out = git_project_cmd(&dir, &store)
        .args(["add", "--no-install", &spec])
        .output()
        .unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    // The repository is named `repo`; DESCRIPTION names the package.
    assert!(text.contains("repo → uvrgitpkg"), "{text}");
    let toml = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let manifest: uvr_core::manifest::Manifest = toml.parse().unwrap();
    let dep = manifest
        .dependencies
        .get("uvrgitpkg")
        .unwrap_or_else(|| panic!("{toml}"));
    assert_eq!(dep.git(), Some(format!("git::{url}").as_str()));
    assert!(toml.contains("rev = \"v0.1.0\""), "{toml}");

    // The lock pins the tagged commit (not the tag object), has no `url`,
    // and a second lock writes the same file.
    let lock_path = dir.path().join("uvr.lock");
    let lock = fs::read_to_string(&lock_path).unwrap();
    let lockfile: uvr_core::lockfile::Lockfile = lock.parse().unwrap();
    let pkg = lockfile
        .get_package("uvrgitpkg")
        .unwrap_or_else(|| panic!("{lock}"));
    assert_eq!(pkg.version, "0.1.0");
    assert_eq!(
        pkg.source,
        uvr_core::lockfile::PackageSource::Git { url: url.clone() }
    );
    assert_eq!(
        pkg.checksum.as_deref(),
        Some(format!("git:{tagged}").as_str())
    );
    assert_eq!(pkg.url, None);
    let out = git_project_cmd(&dir, &store).arg("lock").output().unwrap();
    assert!(out.status.success(), "{}", output_text(&out));
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), lock);

    if !have_r() {
        eprintln!("skipping the install half: no R on PATH");
        return;
    }
    let installed = dir.path().join(".uvr/library/uvrgitpkg/DESCRIPTION");
    let out = git_project_cmd(&dir, &store)
        .args(["sync", "-v"])
        .output()
        .unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        fs::read_to_string(&installed)
            .unwrap()
            .contains("Version: 0.1.0"),
        "{text}"
    );

    // Re-sync without the repository: the library, then the package cache,
    // are gone, and the archive of the locked commit is still in the cache.
    fs::remove_dir_all(store.path().join("repo")).unwrap();
    fs::remove_dir_all(dir.path().join(".uvr/library/uvrgitpkg")).unwrap();
    fs::remove_dir_all(store.path().join("packages")).unwrap();
    let out = git_project_cmd(&dir, &store).arg("sync").output().unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    assert!(installed.exists(), "{text}");
    let out = git_project_cmd(&dir, &store).arg("sync").output().unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("Everything is up to date"), "{text}");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn git_dependency_without_git_fails_clearly() {
    if !have_git() {
        eprintln!("skipping: no git on PATH to build the fixture");
        return;
    }
    let (store, url, _) = git_repo_store();
    let dir = init_project("nogitproj");
    let before = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let empty_path = TempDir::new().unwrap();
    let out = git_project_cmd(&dir, &store)
        .env("PATH", empty_path.path())
        .args(["add", "--no-install", &format!("git::{url}")])
        .output()
        .unwrap();
    let text = output_text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("`git` is not on PATH"), "{text}");
    assert_eq!(
        fs::read_to_string(dir.path().join("uvr.toml")).unwrap(),
        before
    );

    // `uvr doctor` says so too, and calls it an issue for this project.
    let out = git_project_cmd(&dir, &store)
        .env("PATH", empty_path.path())
        .args(["add", "--no-lock", &format!("git::{url}")])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", output_text(&out));
    let out = git_project_cmd(&dir, &store)
        .env("PATH", empty_path.path())
        .arg("doctor")
        .output()
        .unwrap();
    let text = output_text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("needed for git:: dependencies"), "{text}");
    assert!(
        text.contains("git is not on PATH, and this project has git:: dependencies"),
        "{text}"
    );
}

#[test]
fn test_doctor_reports_git() {
    let out = uvr_cmd().arg("doctor").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    let git_found = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let row = stdout
        .lines()
        .find(|line| line.contains(" git "))
        .unwrap_or_else(|| panic!("no git row: {stdout}"));
    assert_eq!(
        row.contains("found") && !row.contains("not found"),
        git_found,
        "{row}"
    );
}

// Existing Git provider specs retain their meaning after manifest edits (#190).
#[test]
fn test_add_no_lock_keeps_every_git_spec_shape() {
    let dir = init_project("specshapes");
    uvr_cmd()
        .args([
            "add",
            "--no-lock",
            "owner/ghpkg@v1",
            "forgejo::codefloe.com/team/fjpkg@main",
            "gitlab::gitlab.com/group/sub/glpkg",
            "git::git@bitbucket.org:team/bbpkg.git@v2",
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    let manifest = uvr_core::manifest::Manifest::from_file(&dir.path().join("uvr.toml")).unwrap();
    for (name, git, rev) in [
        ("ghpkg", "owner/ghpkg", Some("v1")),
        ("fjpkg", "forgejo::codefloe.com/team/fjpkg", Some("main")),
        ("glpkg", "gitlab::gitlab.com/group/sub/glpkg", None),
        ("bbpkg", "git::git@bitbucket.org:team/bbpkg.git", Some("v2")),
    ] {
        let spec = &manifest.dependencies[name];
        assert_eq!(spec.git(), Some(git), "{name}");
        let uvr_core::manifest::DependencySpec::Detailed(dep) = spec else {
            panic!("expected detailed dependency for {name}");
        };
        assert_eq!(dep.rev.as_deref(), rev, "{name}");
    }
}

// ─── url dependencies (#189) ───────────────────────────────

/// A gzip tar with `files` (path, contents).
#[cfg(not(target_os = "windows"))]
fn gzip_tar(files: &[(&str, &str)]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut builder = tar::Builder::new(&mut enc);
        for (path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            // R's internal untar rejects the default NUL type flag.
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, contents.as_bytes()).unwrap();
        }
        builder.finish().unwrap();
    }
    enc.finish().unwrap()
}

/// A minimal installable pure-R source package called `urlpkg`.
#[cfg(not(target_os = "windows"))]
fn urlpkg_tarball(version: &str) -> Vec<u8> {
    let description = format!(
        "Package: urlpkg\nVersion: {version}\nTitle: Test\nDescription: Test package.\n\
         License: MIT\nAuthor: uvr\nMaintainer: uvr <uvr@example.org>\nNeedsCompilation: no\n"
    );
    gzip_tar(&[
        ("urlpkg/DESCRIPTION", &description),
        ("urlpkg/NAMESPACE", "export(hello)\n"),
        (
            "urlpkg/R/hello.R",
            "hello <- function() \"hello from urlpkg\"\n",
        ),
    ])
}

#[cfg(not(target_os = "windows"))]
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
}

#[test]
fn test_add_no_lock_records_url_dependency() {
    let dir = init_project("url-no-lock");
    let url = "https://example.org/dl/urlpkg_0.1.0.tar.gz";
    uvr_cmd()
        .args(["add", "--no-lock", url])
        .current_dir(dir.path())
        .assert()
        .success();

    let content = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    let m: uvr_core::manifest::Manifest = content.parse().unwrap();
    assert_eq!(m.dependencies.get("urlpkg").unwrap().url(), Some(url));
    assert!(!dir.path().join("uvr.lock").exists());
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_add_rejects_urls_that_are_not_source_tarballs() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("page.tar.gz"),
        "<!DOCTYPE html><html><body>Not found</body></html>",
    )
    .unwrap();
    fs::write(
        root.path().join("urlpkg_0.1.0.tgz"),
        gzip_tar(&[(
            "urlpkg/DESCRIPTION",
            "Package: urlpkg\nVersion: 0.1.0\nBuilt: R 4.5.0; x86_64-pc-linux-gnu; 2025-01-15; unix\n",
        )]),
    )
    .unwrap();
    let (base, _server) = spawn_stub(root.path().to_path_buf(), None);

    let dir = init_project("url-reject");
    let before = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    for (file, reason) in [
        ("page.tar.gz", "not gzip-compressed"),
        ("urlpkg_0.1.0.tgz", "pre-built binary"),
        ("missing_0.1.0.tar.gz", "404"),
    ] {
        uvr_cmd()
            .args(["add", "--no-install", &format!("{base}/{file}")])
            .current_dir(dir.path())
            .assert()
            .failure()
            .stderr(predicate::str::contains(reason));
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("uvr.toml")).unwrap(),
        before,
        "a rejected URL must not reach uvr.toml"
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_add_url_locks_url_and_checksum() {
    // Resolution also fetches the CRAN index, like
    // `lock_with_binary_capable_source_records_source_urls` above.
    let root = TempDir::new().unwrap();
    let bytes = urlpkg_tarball("0.1.0");
    // The file name is not the package name: DESCRIPTION decides.
    fs::write(root.path().join("build-artifact.tar.gz"), &bytes).unwrap();
    let (base, _server) = spawn_stub(root.path().to_path_buf(), None);
    let url = format!("{base}/build-artifact.tar.gz");

    let dir = init_project("url-lock");
    uvr_cmd()
        .args(["add", "--no-install", &url])
        .current_dir(dir.path())
        .assert()
        .success();

    let manifest: uvr_core::manifest::Manifest = fs::read_to_string(dir.path().join("uvr.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        manifest.dependencies.get("urlpkg").unwrap().url(),
        Some(url.as_str())
    );
    let lock: uvr_core::lockfile::Lockfile = fs::read_to_string(dir.path().join("uvr.lock"))
        .unwrap()
        .parse()
        .unwrap();
    let pkg = lock.get_package("urlpkg").expect("urlpkg locked");
    assert_eq!(pkg.source, uvr_core::lockfile::PackageSource::Url);
    assert_eq!(pkg.url.as_deref(), Some(url.as_str()));
    assert_eq!(pkg.checksum, Some(sha256(&bytes)));

    // An unchanged manifest keeps its pinned archive until explicitly upgraded.
    let changed = gzip_tar(&[
        ("urlpkg/DESCRIPTION", "Package: urlpkg\nVersion: 0.1.0\nTitle: Changed archive\nDescription: Test package.\nLicense: MIT\nAuthor: uvr\nMaintainer: uvr <uvr@example.org>\nNeedsCompilation: no\n"),
        ("urlpkg/NAMESPACE", "export(hello)\n"),
        ("urlpkg/R/hello.R", "hello <- function() \"updated source\"\n"),
    ]);
    fs::write(root.path().join("build-artifact.tar.gz"), &changed).unwrap();
    uvr_cmd()
        .arg("lock")
        .current_dir(dir.path())
        .assert()
        .success();
    let preserved = uvr_core::lockfile::Lockfile::from_file(&dir.path().join("uvr.lock")).unwrap();
    assert_eq!(
        preserved.get_package("urlpkg").unwrap().checksum,
        Some(sha256(&bytes))
    );
    uvr_cmd()
        .args(["lock", "--upgrade"])
        .current_dir(dir.path())
        .assert()
        .success();
    let refreshed = uvr_core::lockfile::Lockfile::from_file(&dir.path().join("uvr.lock")).unwrap();
    assert_eq!(
        refreshed.get_package("urlpkg").unwrap().checksum,
        Some(sha256(&changed))
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_sync_url_dependency_verifies_checksum_then_installs() {
    if !have_r() {
        eprintln!("skipping: no R on PATH");
        return;
    }
    let root = TempDir::new().unwrap();
    let bytes = urlpkg_tarball("0.1.0");
    fs::write(root.path().join("urlpkg_0.1.0.tar.gz"), &bytes).unwrap();
    let (base, _server) = spawn_stub(root.path().to_path_buf(), None);
    let url = format!("{base}/urlpkg_0.1.0.tar.gz");

    let home = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("uvr.toml"),
        format!(
            "[project]\nname = \"urlsync\"\n\n[dependencies]\nurlpkg = {{ url = \"{url}\" }}\n"
        ),
    )
    .unwrap();
    // `version = "*"` keeps sync from re-resolving for the active R.
    let write_lock = |checksum: &str| {
        fs::write(
            dir.path().join("uvr.lock"),
            format!(
                "[r]\nversion = \"*\"\n\n[[package]]\nname = \"urlpkg\"\nversion = \"0.1.0\"\n\
                 source = \"url\"\nurl = \"{url}\"\nchecksum = \"{checksum}\"\n"
            ),
        )
        .unwrap();
    };
    let sync = || {
        let mut cmd = uvr_cmd();
        cmd.arg("sync")
            .current_dir(dir.path())
            .env("HOME", home.path())
            .env("UVR_CACHE_DIR", home.path().join("cache"))
            .env("UVR_PACKAGES_DIR", home.path().join("packages"))
            .env("UVR_NO_BINARY", "1")
            .env_remove("UVR_REPOS")
            .env_remove("UVR_LIBRARY")
            .env_remove("R_HOME");
        cmd
    };

    // The file no longer matches the lockfile: a hard error, nothing installed.
    let stale = format!("sha256:{}", "0".repeat(64));
    write_lock(&stale);
    sync()
        .assert()
        .failure()
        .stderr(predicate::str::contains(&url))
        .stderr(predicate::str::contains(&stale))
        .stderr(predicate::str::contains(sha256(&bytes)))
        .stderr(predicate::str::contains("run `uvr lock`"));
    let installed = dir.path().join(".uvr/library/urlpkg/DESCRIPTION");
    assert!(!installed.exists());

    write_lock(&sha256(&bytes));
    sync().assert().success();
    assert!(installed.exists(), "urlpkg was not installed");
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
fn test_init_ide_rejects_unsupported_rstudio() {
    // `--ide` only accepts `positron` today. RStudio has no config in uvr, so
    // rejecting the value is better than accepting a silent no-op. Adding
    // RStudio later means adding it to `Ide` and the config writer.
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "rstudioproj", "--ide=rstudio"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
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
fn test_init_rstudio_env_still_wires_library_but_writes_no_ide_config() {
    // RStudio is not a supported `--ide` value, but its users are still served:
    // `.Rprofile` wires the library and no IDE config is written. Pin that so
    // removing the RStudio variant does not silently break RStudio terminals.
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
fn test_init_unattended_writes_only_manifest_and_library() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "unattendedproj", "--unattended"])
        .env("POSITRON", "1")
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join("uvr.toml").exists());
    assert!(dir.path().join(".uvr").join("library").exists());
    assert!(!dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".gitignore").exists());
    assert!(!dir.path().join(".uvr").join("activate").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_unattended_before_subcommand_still_beats_explicit_ide() {
    // The removed `conflicts_with = "unattended"` only fired when the global
    // flag followed the subcommand, so `uvr --unattended init --ide positron`
    // was accepted anyway. It is accepted for every spelling now; the runtime
    // gate must still make `--unattended` win and write no IDE config.
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args([
            "--unattended",
            "init",
            "--here",
            "unattendedide",
            "--ide=positron",
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join("uvr.toml").exists());
    assert!(!dir.path().join(".Rprofile").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_init_bare_conflicts_with_ide() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "bareide", "--bare", "--ide=positron"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
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
fn test_init_bare_skips_scaffolding_but_ignores_library() {
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["init", "--here", "bareproj", "--bare"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert!(dir.path().join("uvr.toml").exists());
    assert!(dir.path().join(".uvr").join("library").exists());
    assert!(!dir.path().join(".Rprofile").exists());
    assert!(dir.path().join(".gitignore").exists());
    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains("/.uvr/library/"));
    assert!(!dir.path().join(".uvr").join("activate").exists());
    assert!(!dir.path().join(".vscode").exists());
    let manifest = fs::read_to_string(dir.path().join("uvr.toml")).unwrap();
    assert!(manifest.contains("bare = true"));
}

#[test]
fn test_ide_flag_is_scoped_to_init_sync_import() {
    // `--ide` is only meaningful where IDE config is written. On `add` it
    // must be rejected at parse time, not silently accepted and ignored.
    let dir = TempDir::new().unwrap();
    uvr_cmd()
        .args(["add", "--ide=positron", "ggplot2"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}
