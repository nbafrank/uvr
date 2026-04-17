use anyhow::Result;
use console::style;

use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_all, find_r_binary, query_r_version};
use uvr_core::r_version::downloader::Platform;

pub fn run() -> Result<()> {
    let mut issues: Vec<String> = Vec::new();

    println!("{}\n", style("uvr doctor").bold().underlined());

    // ── Platform ──
    check_platform();

    // ── R installations ──
    check_r_installations(&mut issues);

    // ── Build tools ──
    check_build_tools(&mut issues);

    // ── Project status ──
    check_project(&mut issues);

    // ── Cache ──
    check_cache();

    // ── Summary ──
    println!();
    if issues.is_empty() {
        println!("{} No issues found", style("✓").green().bold());
    } else {
        println!(
            "{} Found {} issue(s):\n",
            style("!").yellow().bold(),
            issues.len()
        );
        for issue in &issues {
            println!("  {} {issue}", style("•").yellow());
        }
    }

    Ok(())
}

fn check_platform() {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    println!("  {} Platform: {os}/{arch}", style("•").dim());

    match Platform::detect() {
        Ok(p) => {
            let has_binaries = p.is_macos() || p.is_windows();
            if has_binaries {
                println!(
                    "  {} P3M binary packages: {}",
                    style("•").dim(),
                    style("available").green()
                );
            } else {
                println!(
                    "  {} P3M binary packages: {} (source-only)",
                    style("•").dim(),
                    style("unavailable").yellow()
                );
            }
        }
        Err(_) => {
            println!(
                "  {} Platform: {}",
                style("•").dim(),
                style("unsupported").red()
            );
        }
    }
    println!();
}

fn check_r_installations(issues: &mut Vec<String>) {
    println!("{}", style("R installations").bold());

    let installations = find_all();
    if installations.is_empty() {
        println!(
            "  {} {}",
            style("✗").red(),
            style("No R installations found").red()
        );
        issues.push("No R installation found. Install with: uvr r install <version>".into());
        println!();
        return;
    }

    for inst in &installations {
        let tag = if inst.managed {
            style("managed").cyan().to_string()
        } else {
            style("system").dim().to_string()
        };
        println!(
            "  {} R {} at {} ({})",
            style("✓").green(),
            style(&inst.version).cyan(),
            style(inst.binary.display()).dim(),
            tag,
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
                    "  {} Active R: {} ({})",
                    style("→").blue(),
                    style(&v).cyan().bold(),
                    binary.display()
                );
            } else {
                println!(
                    "  {} R binary found but could not query version: {}",
                    style("✗").red(),
                    binary.display()
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
    println!();
}

fn check_build_tools(issues: &mut Vec<String>) {
    println!("{}", style("Build tools").bold());

    // cargo (for building from source / installing uvr)
    let has_cargo = which::which("cargo").is_ok();
    print_check("cargo (Rust toolchain)", has_cargo, None);
    if !has_cargo {
        // Check ~/.cargo/bin/cargo as fallback
        let home_cargo = dirs::home_dir()
            .map(|h| h.join(".cargo").join("bin").join("cargo"))
            .filter(|p| p.exists());
        if home_cargo.is_some() {
            println!(
                "    {} Found at ~/.cargo/bin/cargo (not on PATH)",
                style("→").blue()
            );
        }
    }

    // Platform-specific build tools
    if cfg!(target_os = "macos") {
        check_macos_tools(issues);
    } else if cfg!(target_os = "windows") {
        check_windows_tools(issues);
    } else if cfg!(target_os = "linux") {
        check_linux_tools(issues);
    }

    println!();
}

fn check_macos_tools(issues: &mut Vec<String>) {
    // Xcode command line tools
    let has_xcode = std::process::Command::new("xcode-select")
        .arg("-p")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    print_check("Xcode command line tools", has_xcode, None);
    if !has_xcode {
        issues.push("Xcode CLI tools not installed. Run: xcode-select --install".into());
    }

    // Homebrew
    let has_brew = which::which("brew").is_ok();
    print_check("Homebrew", has_brew, Some("needed for system library deps"));
}

fn check_windows_tools(issues: &mut Vec<String>) {
    // Rtools
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

    print_check(
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
    // gcc/make for source compilation
    let has_gcc = which::which("gcc").is_ok() || which::which("cc").is_ok();
    print_check("C compiler (gcc/cc)", has_gcc, None);
    if !has_gcc {
        issues
            .push("No C compiler found. Install with: sudo apt-get install build-essential".into());
    }

    let has_make = which::which("make").is_ok();
    print_check("make", has_make, None);
}

fn check_project(issues: &mut Vec<String>) {
    println!("{}", style("Project").bold());

    match Project::find_cwd() {
        Ok(project) => {
            println!(
                "  {} Manifest: {}",
                style("✓").green(),
                style(project.manifest_path().display()).dim()
            );

            // R version constraint
            if let Some(ref rv) = project.manifest.project.r_version {
                println!("  {} R constraint: {}", style("•").dim(), style(rv).cyan());
            }

            // Lockfile
            match project.load_lockfile() {
                Ok(Some(lockfile)) => {
                    println!(
                        "  {} Lockfile: {} package(s), R {}",
                        style("✓").green(),
                        lockfile.packages.len(),
                        style(&lockfile.r.version).cyan()
                    );
                }
                Ok(None) => {
                    println!(
                        "  {} Lockfile: {}",
                        style("!").yellow(),
                        style("not found — run `uvr lock`").yellow()
                    );
                    issues.push("No lockfile found. Run: uvr lock".into());
                }
                Err(e) => {
                    println!(
                        "  {} Lockfile: {}",
                        style("✗").red(),
                        style(format!("error: {e}")).red()
                    );
                    issues.push(format!("Lockfile is invalid: {e}"));
                }
            }

            // Library directory
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
                println!(
                    "  {} Library: {} package(s) installed",
                    style("•").dim(),
                    pkg_count
                );
            } else {
                println!(
                    "  {} Library: {}",
                    style("•").dim(),
                    style("not yet created").dim()
                );
            }
        }
        Err(_) => {
            println!(
                "  {} {}",
                style("•").dim(),
                style("Not inside a uvr project").dim()
            );
        }
    }
    println!();
}

fn check_cache() {
    println!("{}", style("Cache").bold());
    let cache_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".uvr")
        .join("cache");
    if cache_dir.exists() {
        let (count, size) = dir_stats(&cache_dir);
        println!(
            "  {} Downloads: {} file(s), {}",
            style("•").dim(),
            count,
            human_size(size)
        );
    } else {
        println!(
            "  {} Downloads: {}",
            style("•").dim(),
            style("empty").dim()
        );
    }

    let (pkg_count, pkg_bytes) =
        uvr_core::installer::package_cache::cache_stats();
    if pkg_count > 0 {
        println!(
            "  {} Packages: {} entries, {}",
            style("•").dim(),
            pkg_count,
            human_size(pkg_bytes)
        );
    } else {
        println!(
            "  {} Packages: {}",
            style("•").dim(),
            style("empty").dim()
        );
    }
}

fn print_check(name: &str, ok: bool, note: Option<&str>) {
    let marker = if ok {
        style("✓").green()
    } else {
        style("✗").red()
    };
    let status = if ok {
        style("found").green().to_string()
    } else {
        let mut s = style("not found").red().to_string();
        if let Some(n) = note {
            s = format!("{s} ({n})");
        }
        s
    };
    println!("  {marker} {name}: {status}");
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

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
