#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use uvr_core::sysreqs::{check_system_deps, filter_missing, PackageSysReqQuery, SysReq};

fn executable(path: &std::path::Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn rpm_providers_and_scoped_overrides() {
    for case in [
        "compatible",
        "old",
        "missing",
        "no-pkg-config",
        "wrong-distro",
        "no-policy",
        "invalid-policy",
        "original-installed",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let alternative_status = if case == "missing" { 1 } else { 0 };
        let module_status = if case == "old" { 1 } else { 0 };
        let original_status = if case == "original-installed" { 0 } else { 1 };
        executable(
            &dir.path().join("rpm"),
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n\
             '-q --whatprovides virtual-devel') exit 0 ;;\n\
             '-q --whatprovides canonical-sdk') exit {alternative_status} ;;\n\
             '-q --whatprovides gdal-devel') exit {original_status} ;;\n\
             *) exit 1 ;;\nesac\n"
            ),
        );
        if case != "no-pkg-config" {
            executable(&dir.path().join("pkg-config"), &format!(
                "#!/bin/sh\n[ \"$*\" = '--exists --atleast-version=3.4 sdk' ] || exit 1\nexit {module_status}\n"
            ));
        }
        let policy = dir.path().join("overrides.toml");
        fs::write(&policy, if case == "invalid-policy" {
            "not valid TOML"
        } else {
            "[[overrides]]\ndistro = 'redhat-9'\npackage = 'gdal-devel'\ninstalled = 'canonical-sdk'\npkg_config = 'sdk'\nminimum_version = '3.4'\n"
        }).unwrap();
        // The child owns its PATH and policy, so parallel tests cannot see
        // our fake RPM database or select a host dpkg ahead of it.
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "rpm_probe_child", "--nocapture"])
            .env("PATH", dir.path())
            .env("UVR_TEST_RPM_CASE", case)
            .env_remove("UVR_SYSREQS_OVERRIDES");
        if case != "no-policy" {
            child.env("UVR_SYSREQS_OVERRIDES", &policy);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn rpm_probe_child() {
    let Ok(case) = std::env::var("UVR_TEST_RPM_CASE") else {
        return;
    };
    // An installed virtual provider satisfies an exact catalog requirement
    // without any override, but unrelated/runtime requirements remain missing.
    let requirements: Vec<_> = ["virtual-devel", "gdal", "udunits2-devel"]
        .into_iter()
        .map(|package| SysReq {
            package: package.to_string(),
        })
        .collect();
    assert_eq!(
        filter_missing(&requirements)
            .iter()
            .map(|req| req.package.as_str())
            .collect::<Vec<_>>(),
        ["gdal", "udunits2-devel"]
    );
    // Bioconductor requests take the local rules path without network access.
    let distro = if case == "wrong-distro" {
        "redhat-10"
    } else {
        "redhat-9"
    };
    let queries = [PackageSysReqQuery {
        name: "test-gis".to_string(),
        system_requirements: Some("GDAL".to_string()),
        bioc: true,
    }];
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(check_system_deps(&reqwest::Client::new(), &queries, distro));
    if case == "invalid-policy" {
        assert!(result.is_err());
        return;
    }
    let check = result.unwrap();
    let missing = check.missing.get("test-gis").unwrap();
    assert!(missing.iter().any(|req| req.package == "gdal"));
    let satisfied = case == "compatible" || case == "original-installed";
    assert_eq!(
        missing.iter().any(|req| req.package == "gdal-devel"),
        !satisfied
    );
    assert_eq!(
        check.overrides_applied.len(),
        usize::from(case == "compatible")
    );
}
