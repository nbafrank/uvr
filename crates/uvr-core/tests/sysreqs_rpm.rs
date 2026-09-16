#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use uvr_core::sysreqs::{filter_missing, SysReq};

#[test]
fn rpm_gdal_compatibility_preserves_runtime_requirements() {
    for (version, canonical_status, compat_status, expected) in [
        ("3.10.3", 0, 1, "gdal3.4,udunits2-devel"),
        ("3.4.0", 0, 1, "gdal3.4,udunits2-devel"),
        ("3.3.3", 0, 1, "gdal3.4-devel,gdal3.4,udunits2-devel"),
        ("unknown", 0, 1, "gdal3.4-devel,gdal3.4,udunits2-devel"),
        ("3.10.3", 1, 1, "gdal3.4-devel,gdal3.4,udunits2-devel"),
        ("", 1, 0, "gdal3.4,udunits2-devel"),
    ] {
        let bin = tempfile::tempdir().unwrap();
        let rpm = bin.path().join("rpm");
        fs::write(
            &rpm,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                 '-q gdal3.4-devel') exit {compat_status} ;;\n\
                 '-q --qf %{{VERSION}}\\n gdal-devel') printf '%s\\n' '{version}'; exit {canonical_status} ;;\n\
                 *) exit 1 ;;\nesac\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&rpm, fs::Permissions::from_mode(0o755)).unwrap();

        // Isolate PATH in a child test process so parallel tests cannot observe
        // our fake RPM database, and a host dpkg cannot take precedence.
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "rpm_probe_child", "--nocapture"])
            .env("PATH", bin.path())
            .env("UVR_TEST_RPM_EXPECTED", expected)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "version={version:?}, canonical_status={canonical_status}, compat_status={compat_status}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn rpm_probe_child() {
    let Ok(expected) = std::env::var("UVR_TEST_RPM_EXPECTED") else {
        return;
    };
    let requirements: Vec<_> = ["gdal3.4-devel", "gdal3.4", "udunits2-devel"]
        .into_iter()
        .map(|package| SysReq {
            package: package.to_string(),
        })
        .collect();
    let missing = filter_missing(&requirements)
        .into_iter()
        .map(|req| req.package.as_str())
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(missing, expected);
}
