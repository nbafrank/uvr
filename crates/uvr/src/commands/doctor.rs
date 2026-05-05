use anyhow::Result;

use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_all, find_r_binary, query_r_version};
use uvr_core::r_version::downloader::Platform;

use crate::ui;
use crate::ui::palette;

/// Width for the label column in `check` rows, chosen so all labels align.
const LABEL_W: usize = 28;

pub fn run() -> Result<()> {
    let mut issues: Vec<String> = Vec::new();

    println!(
        "{} {}",
        palette::info(ui::glyph::info()),
        palette::bold("uvr doctor"),
    );

    ui::section("Platform");
    check_platform();

    ui::section("R installations");
    check_r_installations(&mut issues);

    ui::section("Build tools");
    check_build_tools(&mut issues);

    ui::section("Project");
    check_project(&mut issues);

    ui::section("Cache");
    check_cache();

    println!();
    if issues.is_empty() {
        ui::success("All checks passed — you're good to go.");
    } else {
        ui::warn(format!("{} issue(s) to address", issues.len()));
        for issue in &issues {
            println!("  {} {}", palette::warn(ui::glyph::bullet()), issue);
        }
    }

    Ok(())
}

fn check_platform() {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    ui::check(true, "OS / architecture", format!("{os}/{arch}"), LABEL_W);

    match Platform::detect() {
        Ok(p) => {
            // macOS and Windows always have P3M binaries. Linux has them
            // when the distro is one PPM publishes (#55) — translate the
            // slug to a PPM codename to know.
            let label = "P3M binary packages";
            if p.is_macos() || p.is_windows() {
                ui::check(true, label, palette::success("available"), LABEL_W);
            } else {
                let slug = uvr_core::r_version::downloader::detect_posit_distro_slug();
                if let Some(codename) = uvr_core::registry::p3m::ppm_linux_codename(&slug) {
                    ui::check(
                        true,
                        label,
                        format!(
                            "{} {}",
                            palette::success("available"),
                            palette::dim(format!("(Linux {codename})"))
                        ),
                        LABEL_W,
                    );
                } else {
                    ui::check(
                        true,
                        label,
                        format!(
                            "{} {}",
                            palette::warn("unavailable"),
                            palette::dim(format!("(distro {slug} not on PPM — source-only)"))
                        ),
                        LABEL_W,
                    );
                }
            }
        }
        Err(_) => {
            ui::check(
                false,
                "Platform support",
                palette::fail("unsupported"),
                LABEL_W,
            );
        }
    }
}

fn check_r_installations(issues: &mut Vec<String>) {
    let installations = find_all();
    if installations.is_empty() {
        ui::check(
            false,
            "R installation",
            palette::fail("none found"),
            LABEL_W,
        );
        issues.push("No R installation found. Install with: uvr r install <version>".into());
        return;
    }

    for inst in &installations {
        let tag = if inst.managed {
            palette::info("managed").to_string()
        } else {
            palette::dim("system").to_string()
        };
        let label = format!("R {}", inst.version);
        ui::check(
            true,
            &label,
            format!(
                "{} {} {}",
                palette::dim(inst.binary.display().to_string()),
                palette::dim(ui::glyph::bullet()),
                tag
            ),
            LABEL_W,
        );
    }

    // Check that the active R binary works
    let r_constraint = Project::find_cwd()
        .ok()
        .and_then(|p| p.manifest.project.r_version.clone());
    match find_r_binary(r_constraint.as_deref()) {
        Ok(ref binary) => {
            if let Some(v) = query_r_version(binary) {
                println!(
                    "  {} {:<LABEL_W$} {} {}",
                    palette::info(ui::glyph::arrow()),
                    "active",
                    palette::info(&v),
                    palette::dim(binary.display().to_string()),
                );
            } else {
                ui::check(
                    false,
                    "active R",
                    palette::fail(format!("no response at {}", binary.display())),
                    LABEL_W,
                );
                issues.push(format!(
                    "R at {} is not responding — it may be corrupt",
                    binary.display()
                ));
            }
        }
        Err(e) => {
            issues.push(format!("Cannot select R binary: {e}"));
        }
    }
}

fn check_build_tools(issues: &mut Vec<String>) {
    let has_cargo = which::which("cargo").is_ok();
    ui::check(
        has_cargo,
        "cargo (Rust toolchain)",
        if has_cargo {
            palette::success("found").to_string()
        } else {
            palette::fail("not found").to_string()
        },
        LABEL_W,
    );
    if !has_cargo {
        let home_cargo = dirs::home_dir()
            .map(|h| h.join(".cargo").join("bin").join("cargo"))
            .filter(|p| p.exists());
        if home_cargo.is_some() {
            ui::hint("Found at ~/.cargo/bin/cargo — add it to PATH.");
        }
    }

    if cfg!(target_os = "macos") {
        check_macos_tools(issues);
    } else if cfg!(target_os = "windows") {
        check_windows_tools(issues);
    } else if cfg!(target_os = "linux") {
        check_linux_tools(issues);
    }
}

fn check_macos_tools(issues: &mut Vec<String>) {
    let has_xcode = std::process::Command::new("xcode-select")
        .arg("-p")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    simple_check("Xcode command line tools", has_xcode, None);
    if !has_xcode {
        issues.push("Xcode CLI tools not installed. Run: xcode-select --install".into());
    }

    let has_brew = which::which("brew").is_ok();
    simple_check("Homebrew", has_brew, Some("needed for system library deps"));
}

fn check_windows_tools(issues: &mut Vec<String>) {
    let rtools_found = [
        std::env::var("RTOOLS45_HOME").ok(),
        std::env::var("RTOOLS44_HOME").ok(),
        std::env::var("RTOOLS43_HOME").ok(),
        Some("C:\\rtools45".to_string()),
        Some("C:\\rtools44".to_string()),
        Some("C:\\rtools43".to_string()),
    ]
    .into_iter()
    .flatten()
    .any(|p| std::path::Path::new(&p).exists());

    simple_check(
        "Rtools",
        rtools_found,
        Some("needed to compile R packages from source"),
    );
    if !rtools_found {
        issues.push(
            "Rtools not found. Install from: https://cran.r-project.org/bin/windows/Rtools/".into(),
        );
    }
}

fn check_linux_tools(issues: &mut Vec<String>) {
    let has_gcc = which::which("gcc").is_ok() || which::which("cc").is_ok();
    simple_check("C compiler (gcc/cc)", has_gcc, None);
    if !has_gcc {
        issues
            .push("No C compiler found. Install with: sudo apt-get install build-essential".into());
    }

    let has_make = which::which("make").is_ok();
    simple_check("make", has_make, None);
}

fn check_project(issues: &mut Vec<String>) {
    match Project::find_cwd() {
        Ok(project) => {
            ui::check(
                true,
                "Manifest",
                palette::dim(project.manifest_path().display().to_string()),
                LABEL_W,
            );

            if let Some(ref rv) = project.manifest.project.r_version {
                ui::check(true, "R constraint", palette::info(rv), LABEL_W);
            }

            match project.load_lockfile() {
                Ok(Some(lockfile)) => {
                    ui::check(
                        true,
                        "Lockfile",
                        format!(
                            "{} package(s), R {}",
                            lockfile.packages.len(),
                            palette::info(&lockfile.r.version)
                        ),
                        LABEL_W,
                    );
                }
                Ok(None) => {
                    ui::check(
                        false,
                        "Lockfile",
                        palette::warn("not found — run `uvr lock`"),
                        LABEL_W,
                    );
                    issues.push("No lockfile found. Run: uvr lock".into());
                }
                Err(e) => {
                    ui::check(
                        false,
                        "Lockfile",
                        palette::fail(format!("error: {e}")),
                        LABEL_W,
                    );
                    issues.push(format!("Lockfile is invalid: {e}"));
                }
            }

            let lib = project.library_path();
            if lib.exists() {
                let pkg_count = std::fs::read_dir(&lib)
                    .map(|entries| {
                        entries
                            .flatten()
                            .filter(|e| e.path().join("DESCRIPTION").exists())
                            .count()
                    })
                    .unwrap_or(0);
                ui::check(
                    true,
                    "Library",
                    format!("{pkg_count} package(s) installed"),
                    LABEL_W,
                );
            } else {
                ui::check(true, "Library", palette::dim("not yet created"), LABEL_W);
            }
        }
        Err(_) => {
            println!(
                "  {} {}",
                palette::dim(ui::glyph::bullet()),
                palette::dim("Not inside a uvr project")
            );
        }
    }
}

fn check_cache() {
    let cache_dir = uvr_core::config::cache_dir()
        .unwrap_or_default();
    if cache_dir.exists() {
        let (count, size) = dir_stats(&cache_dir);
        ui::check(
            true,
            "Downloads",
            format!("{count} file(s), {}", palette::format_bytes(size)),
            LABEL_W,
        );
    } else {
        ui::check(true, "Downloads", palette::dim("empty"), LABEL_W);
    }

    let (pkg_count, pkg_bytes) = uvr_core::installer::package_cache::cache_stats();
    if pkg_count > 0 {
        ui::check(
            true,
            "Packages",
            format!("{pkg_count} entries, {}", palette::format_bytes(pkg_bytes)),
            LABEL_W,
        );
    } else {
        ui::check(true, "Packages", palette::dim("empty"), LABEL_W);
    }
}

/// Simple yes/no check row using the standard column width.
fn simple_check(name: &str, ok: bool, note: Option<&str>) {
    let status: String = if ok {
        palette::success("found").to_string()
    } else {
        let n = note.unwrap_or_default();
        if n.is_empty() {
            palette::fail("not found").to_string()
        } else {
            format!(
                "{} {}",
                palette::fail("not found"),
                palette::dim(format!("({n})"))
            )
        }
    };
    ui::check(ok, name, status, LABEL_W);
}

fn dir_stats(dir: &std::path::Path) -> (usize, u64) {
    let mut count = 0usize;
    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    count += 1;
                    size += meta.len();
                }
            }
        }
    }
    (count, size)
}
