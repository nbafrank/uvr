use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::info;

use crate::error::{Result, UvrError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOsArm64,
    MacOsX86_64,
    LinuxX86_64,
    LinuxArm64,
    WindowsX86_64,
}

impl Platform {
    pub fn detect() -> Result<Self> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return Ok(Platform::MacOsArm64);
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return Ok(Platform::MacOsX86_64);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return Ok(Platform::LinuxX86_64);
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return Ok(Platform::LinuxArm64);
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return Ok(Platform::WindowsX86_64);
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        Err(UvrError::UnsupportedPlatform(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )))
    }

    /// Return the Rust target triple for this platform (e.g. `"aarch64-apple-darwin"`).
    pub fn rust_target_triple(&self) -> &'static str {
        match self {
            Platform::MacOsArm64 => "aarch64-apple-darwin",
            Platform::MacOsX86_64 => "x86_64-apple-darwin",
            Platform::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Platform::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Platform::WindowsX86_64 => "x86_64-pc-windows-msvc",
        }
    }

    pub fn is_windows(&self) -> bool {
        matches!(self, Platform::WindowsX86_64)
    }

    pub fn is_macos(&self) -> bool {
        matches!(self, Platform::MacOsArm64 | Platform::MacOsX86_64)
    }

    /// Return the download URL for a given R version.
    pub fn download_url(&self, version: &str) -> String {
        match self {
            Platform::MacOsArm64 => format!(
                "https://cran.r-project.org/bin/macosx/big-sur-arm64/base/R-{version}-arm64.pkg"
            ),
            Platform::MacOsX86_64 => format!(
                "https://cran.r-project.org/bin/macosx/big-sur-x86_64/base/R-{version}-x86_64.pkg"
            ),
            Platform::LinuxX86_64 => {
                let distro = detect_posit_distro_slug();
                format!("https://cdn.posit.co/r/{distro}/pkgs/r-{version}_1_amd64.deb")
            }
            Platform::LinuxArm64 => {
                let distro = detect_posit_distro_slug();
                format!("https://cdn.posit.co/r/{distro}/pkgs/r-{version}_1_arm64.deb")
            }
            Platform::WindowsX86_64 => {
                format!("https://cran.r-project.org/bin/windows/base/R-{version}-win.exe")
            }
        }
    }
}

/// Detect the Posit CDN distro slug from `/etc/os-release`.
///
/// Returns strings like `"ubuntu-2204"`, `"ubuntu-2404"`, `"debian-12"`,
/// `"centos-7"`, `"rhel-9"`, `"opensuse-154"`. Falls back to `"ubuntu-2204"`.
fn detect_posit_distro_slug() -> String {
    // Read os-release; fall back to default if anything fails
    let content = match std::fs::read_to_string("/etc/os-release") {
        Ok(c) => c,
        Err(_) => return "ubuntu-2204".to_string(),
    };

    let mut id = String::new();
    let mut version_id = String::new();
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("ID=") {
            id = val.trim_matches('"').to_lowercase();
        } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
            version_id = val.trim_matches('"').to_string();
        }
    }

    // Posit CDN uses no dots in version for Ubuntu/openSUSE, but keeps them for others
    match id.as_str() {
        "ubuntu" => {
            let ver = version_id.replace('.', "");
            format!("ubuntu-{ver}")
        }
        "debian" => format!("debian-{version_id}"),
        "centos" => format!("centos-{version_id}"),
        "rhel" | "rocky" | "almalinux" => {
            // Major version only
            let major = version_id.split('.').next().unwrap_or(&version_id);
            format!("rhel-{major}")
        }
        "opensuse-leap" | "sles" => {
            let ver = version_id.replace('.', "");
            format!("opensuse-{ver}")
        }
        _ => "ubuntu-2204".to_string(),
    }
}

/// Download and extract R to `~/.uvr/r-versions/<version>/`.
pub async fn download_and_install_r(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
) -> Result<PathBuf> {
    let install_dir = dirs::home_dir()
        .ok_or_else(|| UvrError::Other("Cannot determine home directory".into()))?
        .join(".uvr")
        .join("r-versions")
        .join(version);

    let r_binary_name = if platform.is_windows() { "R.exe" } else { "R" };
    let r_binary = install_dir.join("bin").join(r_binary_name);
    if r_binary.exists() {
        info!("R {version} already installed at {}", install_dir.display());
        return Ok(install_dir);
    }

    let url = platform.download_url(version);
    info!("Downloading R {version} from {url}");

    let bytes = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    std::fs::create_dir_all(&install_dir)?;

    match platform {
        Platform::MacOsArm64 | Platform::MacOsX86_64 => {
            install_r_macos(&bytes, version, &install_dir)?;
        }
        Platform::LinuxX86_64 | Platform::LinuxArm64 => {
            install_r_linux(&bytes, version, &install_dir)?;
        }
        Platform::WindowsX86_64 => {
            install_r_windows(&bytes, version, &install_dir)?;
        }
    }

    if !r_binary.exists() {
        return Err(UvrError::Other(format!(
            "R binary not found after installation at {}",
            r_binary.display()
        )));
    }

    info!("R {version} installed to {}", install_dir.display());
    Ok(install_dir)
}

/// macOS: `.pkg` → xar → Payload (gzip+cpio) → extract
fn install_r_macos(pkg_bytes: &[u8], version: &str, dest: &Path) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let pkg_path = tmp.path().join(format!("R-{version}.pkg"));
    std::fs::write(&pkg_path, pkg_bytes)?;

    // Step 1: pkgutil --expand R.pkg <expanded_dir>
    let expanded_dir = tmp.path().join("expanded");
    let out = Command::new("pkgutil")
        .args([
            "--expand",
            &pkg_path.to_string_lossy(),
            &expanded_dir.to_string_lossy(),
        ])
        .output()?;
    if !out.status.success() {
        return Err(UvrError::Other(format!(
            "pkgutil --expand failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    // Step 2: find ALL Payload files (the pkg is a product archive with multiple
    // component packages: R framework, texinfo, tcltk, GUI, …).
    // We try each Payload until we find one that contains a usable R binary.
    let payloads = find_all_payload_files(&expanded_dir)?;
    if payloads.is_empty() {
        return Err(UvrError::Other(
            "No Payload files found in expanded pkg".into(),
        ));
    }

    // Step 3 + 4: extract each Payload and look for bin/R
    let resources = payloads
        .iter()
        .find_map(|payload| {
            let stage_dir = tmp
                .path()
                .join(format!("stage-{}", payload.display().to_string().len()));
            std::fs::create_dir_all(&stage_dir).ok()?;
            let ok = Command::new("tar")
                .args([
                    "xf",
                    &payload.to_string_lossy(),
                    "-C",
                    &stage_dir.to_string_lossy(),
                ])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                return None;
            }
            find_dir_with_r_binary(&stage_dir, 0)
        })
        .ok_or_else(|| {
            UvrError::Other(format!(
                "Could not find bin/R in any of {} Payload(s) in the pkg",
                payloads.len()
            ))
        })?;
    info!("Found R Resources at {}", resources.display());

    // Step 5: copy Resources contents → dest.
    // -P: preserve symlinks as-is (don't follow/dereference them).
    //     fontconfig/fonts/conf.d contains symlinks pointing to system files that
    //     aren't in the extracted pkg — -P copies the symlink itself, not the target.
    std::fs::create_dir_all(dest)?;
    let src = format!("{}/.", resources.to_string_lossy());
    let out = Command::new("cp")
        .args(["-rP", &src, &dest.to_string_lossy()])
        .output()?;
    if !out.status.success() {
        return Err(UvrError::Other(format!(
            "Failed to copy R Resources to {}: {}",
            dest.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    // Step 6: patch all text files that reference the original R_HOME.
    // bin/R, etc/Makeconf, etc/ldpaths, etc/R, and others contain absolute
    // paths hardcoded at build time (e.g. /Library/Frameworks/R.framework/Resources).
    // Extract the original R_HOME from bin/R, then replace it everywhere.
    let original_r_home = extract_r_home_dir(dest)?;
    info!("Patching R_HOME: {} → {}", original_r_home, dest.display());
    patch_text_files(dest, &original_r_home, &dest.to_string_lossy())?;

    // Step 7: fix the LIBR line in etc/Makeconf.
    // After the text substitution above, LIBR still contains the framework path
    // (e.g. `-F/Library/Frameworks/R.framework/.. -framework R`) because it uses
    // the framework ROOT, not the Resources subdirectory.
    // Replace it with a plain `-L<dest>/lib -lR` that works from any location.
    patch_makeconf_libr(dest)?;

    // Step 8: fix libR.dylib's embedded install-name so packages compiled against
    // it can find the library at its new location without needing DYLD_LIBRARY_PATH.
    fix_libr_install_name(dest);

    // Step 9(a): patch ALL sibling dylibs (libRlapack, libRblas, libgfortran, …)
    // so they reference each other via managed-R paths rather than the original
    // CRAN framework paths.  Without this, symbols like `_dgebal_` (LAPACK) that
    // live in libRlapack/libRblas are invisible to R and packages like Matrix fail
    // to load with "Symbol not found: _dgebal_".
    patch_r_dylibs(dest);

    // Step 9(b): write etc/Renviron.site so that DYLD_LIBRARY_PATH is set for every
    // R process this installation spawns — including the fresh `R --slave` sessions
    // used by R CMD INSTALL for byte-compilation.  On macOS 15+ (SIP), DYLD_*
    // variables are stripped when inherited through /bin/sh, so setting the env
    // var on the parent process alone is not sufficient.
    // R expands ${R_HOME} in Renviron.site before any package code runs.
    write_renviron_site(dest)?;

    Ok(())
}

/// Read `<dest>/bin/R` and extract the value of `R_HOME_DIR=...`.
fn extract_r_home_dir(dest: &Path) -> Result<String> {
    let r_script = dest.join("bin").join("R");
    let content = std::fs::read_to_string(&r_script)?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("R_HOME_DIR=") {
            return Ok(val.trim().to_string());
        }
    }
    // Fallback: standard macOS R framework path.
    Ok("/Library/Frameworks/R.framework/Resources".to_string())
}

/// Recursively replace `old` with `new` in every text file under `dir`.
/// Symlinks are skipped (they can't be patched and may be dangling).
///
/// Binary files are detected by the presence of a null byte and skipped entirely.
/// This prevents accidentally corrupting (and invalidating the code signature of)
/// Mach-O binaries or dylibs that may embed the path string as a constant.
fn patch_text_files(dir: &Path, old: &str, new: &str) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            patch_text_files(&path, old, new)?;
        } else if ft.is_file() {
            if let Ok(bytes) = std::fs::read(&path) {
                // Skip binary files — null bytes are a reliable indicator.
                if bytes.contains(&0) {
                    continue;
                }
                if let Ok(content) = String::from_utf8(bytes) {
                    if content.contains(old) {
                        std::fs::write(&path, content.replace(old, new))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rewrite the `LIBR = …` line in `<dest>/etc/Makeconf` to use `-lR` instead
/// of the macOS `-framework R` flag, which only works from the original install path.
fn patch_makeconf_libr(dest: &Path) -> Result<()> {
    let makeconf = dest.join("etc").join("Makeconf");
    if !makeconf.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&makeconf)?;
    let dest_str = dest.to_string_lossy();
    let patched = content
        .lines()
        .map(|line| {
            let t = line.trim_start();
            if t.starts_with("LIBR =") || t.starts_with("LIBR=") {
                format!("LIBR = -L{dest_str}/lib -lR")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let patched = if content.ends_with('\n') {
        patched + "\n"
    } else {
        patched
    };
    std::fs::write(&makeconf, patched)?;
    Ok(())
}

/// Update the embedded install-name of `libR.dylib` to its actual path so that
/// packages compiled against it can find it at runtime without DYLD_LIBRARY_PATH.
///
/// IMPORTANT: `install_name_tool` invalidates the Mach-O code signature.
/// On Apple Silicon every dylib must be signed; we re-apply an ad-hoc signature
/// with `codesign --force --sign -` immediately after the rename.
fn fix_libr_install_name(dest: &Path) {
    let libr = dest.join("lib").join("libR.dylib");
    if !libr.exists() {
        return;
    }
    let new_id = libr.to_string_lossy().to_string();

    let ok = Command::new("install_name_tool")
        .args(["-id", &new_id, &new_id])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        // Re-sign with an ad-hoc signature after modifying the binary.
        // Without this, macOS (arm64) kills any process that loads the dylib.
        let _ = Command::new("codesign")
            .args(["--force", "--sign", "-", &new_id])
            .status();
    }
}

/// Patch all `.dylib` install names in `<r_home>/lib/` to use the managed-R
/// path instead of the original CRAN framework path.
///
/// The CRAN macOS `.pkg` compiles every dylib with an install name pointing
/// to `/Library/Frameworks/R.framework/…`. After extraction to
/// `~/.uvr/r-versions/<ver>/`, those absolute paths don't exist, so
/// `libRblas`/`libRlapack`/`libgfortran` are never found by the dynamic linker.
/// Packages compiled against system R (e.g. `Matrix`) expect LAPACK symbols
/// from `libR.dylib`'s load chain — if `libRlapack.dylib` is never loaded,
/// those symbols are absent and `dlopen` fails.
///
/// This function is idempotent: if all paths already point to the managed-R
/// lib dir the `install_name_tool` calls succeed silently.
pub fn patch_r_dylibs(r_home: &Path) {
    let lib_dir = r_home.join("lib");
    if !lib_dir.exists() {
        return;
    }
    let lib_str = lib_dir.to_string_lossy().to_string();

    let Ok(entries) = std::fs::read_dir(&lib_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("dylib") {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();

        // Fix the dylib's own install name if it still points to the framework.
        let old_id = Command::new("otool")
            .args(["-D", &path_str])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()
                    .and_then(|t| t.lines().nth(1).map(|l| l.trim().to_string()))
            })
            .unwrap_or_default();

        let mut needs_resign = false;
        if old_id.contains("/Library/Frameworks/R.framework/") {
            let _ = Command::new("install_name_tool")
                .args(["-id", &path_str, &path_str])
                .status();
            needs_resign = true;
        }

        // Fix all dependency paths pointing into the R framework.
        let deps = Command::new("otool")
            .args(["-L", &path_str])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        for line in deps.lines().skip(1) {
            let dep = line.split_whitespace().next().unwrap_or("");
            if !dep.contains("/Library/Frameworks/R.framework/") {
                continue;
            }
            let filename = std::path::Path::new(dep)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if filename.is_empty() {
                continue;
            }
            let new_dep = format!("{lib_str}/{filename}");
            if Command::new("install_name_tool")
                .args(["-change", dep, &new_dep, &path_str])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                needs_resign = true;
            }
        }

        if needs_resign {
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-", &path_str])
                .status();
        }
    }
}

/// Write `etc/Renviron.site` so every R process from this installation
pub fn patch_renviron_site(r_home: &Path) -> Result<()> {
    write_renviron_site(r_home)
}

/// Write `etc/Renviron.site` so every R process from this installation
/// automatically has `DYLD_LIBRARY_PATH` pointing at its own lib directory.
///
/// R reads this file at startup (before user code) and expands `${R_HOME}`.
/// This survives the `DYLD_*` stripping that macOS applies to sub-processes
/// spawned through SIP-protected shells like `/bin/sh`.
fn write_renviron_site(dest: &Path) -> Result<()> {
    let renviron = dest.join("etc").join("Renviron.site");
    // Append only if our line isn't already present (idempotent for re-installs).
    let existing = std::fs::read_to_string(&renviron).unwrap_or_default();
    if existing.contains("DYLD_LIBRARY_PATH=${R_HOME}/lib") {
        return Ok(());
    }
    let content = format!(
        "{existing}# Added by uvr: ensure libR.dylib is always findable by sub-processes.\n\
         DYLD_LIBRARY_PATH=${{R_HOME}}/lib\n\
         LD_LIBRARY_PATH=${{R_HOME}}/lib\n"
    );
    std::fs::write(&renviron, content)?;
    Ok(())
}

fn find_dir_with_r_binary(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 12 {
        return None; // guard against symlink loops
    }
    if dir.join("bin").join("R").exists() {
        return Some(dir.to_path_buf());
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Use metadata() so we follow symlinks (file_type() does not).
        if std::fs::metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            if let Some(found) = find_dir_with_r_binary(&path, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn find_all_payload_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    collect_payloads(dir, &mut found)?;
    Ok(found)
}

fn collect_payloads(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            collect_payloads(&e.path(), out)?;
        } else if e.file_name().to_string_lossy() == "Payload" {
            out.push(e.path());
        }
    }
    Ok(())
}

/// Windows: Inno Setup `.exe` → silent install to `dest` without admin rights.
///
/// The `/CURRENTUSER` flag tells Inno Setup to install for the current user only,
/// avoiding the need for admin/UAC elevation — a key differentiator over rig.
fn install_r_windows(exe_bytes: &[u8], version: &str, dest: &Path) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let exe_path = tmp.path().join(format!("R-{version}-win.exe"));
    std::fs::write(&exe_path, exe_bytes)?;

    info!("Installing R {version} silently to {}", dest.display());

    // Inno Setup log for diagnosing install failures
    let log_path = tmp.path().join("r-install.log");

    // Do NOT embed extra quotes around the /DIR= path — Rust's Command API
    // already quotes arguments when building the Windows command line.
    // Embedding quotes causes double-escaping that Inno Setup rejects (exit 1).
    let dir_arg = format!("/DIR={}", dest.to_string_lossy());
    let log_arg = format!("/LOG={}", log_path.to_string_lossy());
    let output = Command::new(&exe_path)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            &dir_arg,
            "/CURRENTUSER",
            "/NOICONS",
            "/NORESTART",
            &log_arg,
        ])
        .output()?;

    if !output.status.success() {
        let mut detail = format!(
            "R {version} silent installer failed (exit {})",
            output.status.code().unwrap_or(-1)
        );

        // Include Inno Setup log if available
        if let Ok(log) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = log.lines().collect();
            let start = lines.len().saturating_sub(20);
            let last_lines = lines[start..].join("\n");
            detail.push_str(&format!("\n\nInstaller log (last 20 lines):\n{last_lines}"));
        }

        // Include stdout/stderr if any
        if !output.stdout.is_empty() {
            if let Ok(stdout) = String::from_utf8(output.stdout) {
                detail.push_str(&format!("\n\nstdout: {stdout}"));
            }
        }
        if !output.stderr.is_empty() {
            if let Ok(stderr) = String::from_utf8(output.stderr) {
                detail.push_str(&format!("\n\nstderr: {stderr}"));
            }
        }

        return Err(UvrError::Other(detail));
    }

    Ok(())
}

/// Linux: `.deb` → `ar x` → `data.tar.gz` → extract
fn install_r_linux(deb_bytes: &[u8], version: &str, dest: &Path) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let deb_path = tmp.path().join(format!("r-{version}.deb"));
    std::fs::write(&deb_path, deb_bytes)?;

    // ar x <deb>
    let status = Command::new("ar")
        .args(["x", &deb_path.to_string_lossy()])
        .current_dir(tmp.path())
        .status()?;
    if !status.success() {
        return Err(UvrError::Other("ar x failed on .deb".into()));
    }

    // Find data.tar.*
    let data_tar = find_data_tar(tmp.path())?;

    let status = Command::new("tar")
        .args([
            "xf",
            &data_tar.to_string_lossy(),
            "-C",
            &dest.to_string_lossy(),
            "--strip-components=4", // strip ./opt/R/<version>/
        ])
        .status()?;
    if !status.success() {
        return Err(UvrError::Other(
            "tar extraction of Linux R .deb failed".into(),
        ));
    }

    // Patch hardcoded /opt/R/<version> paths to the actual install dir.
    // The Posit .deb is built with /opt/R/<version> baked into bin/R,
    // etc/Makeconf, etc/ldpaths, etc. We replace them all so R_HOME resolves
    // correctly from ~/.uvr/r-versions/<version>/.
    let original_prefix = format!("/opt/R/{version}");
    patch_text_files(dest, &original_prefix, &dest.to_string_lossy())?;

    // Write Renviron.site so LD_LIBRARY_PATH is set for every R process
    // spawned from this installation. The .so files in lib/ have their RPATH
    // pointing to /opt/R/<version>/lib; setting LD_LIBRARY_PATH at the R
    // level is the simplest way to ensure they resolve without patchelf.
    write_renviron_site(dest)?;

    Ok(())
}

fn find_data_tar(dir: &Path) -> Result<PathBuf> {
    for ext in &["data.tar.gz", "data.tar.xz", "data.tar.zst", "data.tar.bz2"] {
        let p = dir.join(ext);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(UvrError::Other("data.tar.* not found in .deb".into()))
}

/// Fetch the list of available R versions for `platform` from the CRAN CDN.
///
/// Returns versions sorted oldest-first (e.g. `["4.3.0", "4.3.1", ...]`).
pub async fn fetch_available_versions(
    client: &reqwest::Client,
    platform: Platform,
) -> Result<Vec<String>> {
    // Use the platform-specific binary index when possible; fall back to the
    // CRAN source index (which lists every released version) for Linux.
    let (url, prefix, suffix): (&str, &str, &str) = match platform {
        Platform::MacOsArm64 => (
            "https://cran.r-project.org/bin/macosx/big-sur-arm64/base/",
            "R-",
            "-arm64.pkg",
        ),
        Platform::MacOsX86_64 => (
            "https://cran.r-project.org/bin/macosx/big-sur-x86_64/base/",
            "R-",
            "-x86_64.pkg",
        ),
        Platform::LinuxX86_64 | Platform::LinuxArm64 => {
            ("https://cran.r-project.org/src/base/", "R-", ".tar.gz")
        }
        Platform::WindowsX86_64 => (
            "https://cran.r-project.org/bin/windows/base/",
            "R-",
            "-win.exe",
        ),
    };

    let html = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // Parse href="R-<version><suffix>" fragments from the directory listing HTML.
    let needle = format!("href=\"{prefix}");
    let mut versions: Vec<String> = html
        .split(needle.as_str())
        .skip(1)
        .filter_map(|chunk| {
            let end = chunk.find(suffix)?;
            let ver = &chunk[..end];
            // Sanity check: must be X.Y or X.Y.Z (digits and dots only).
            if ver.chars().all(|c| c.is_ascii_digit() || c == '.') && ver.contains('.') {
                Some(ver.to_string())
            } else {
                None
            }
        })
        .collect();

    // Sort numerically by component (not lexicographically).
    versions.sort_by(|a, b| {
        let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
        parse(a).cmp(&parse(b))
    });
    versions.dedup();
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn platform_detect_succeeds() {
        // Should always succeed on supported platforms (macOS/Linux/Windows)
        let platform = Platform::detect().unwrap();
        // Just verify it returns something valid
        assert!(matches!(
            platform,
            Platform::MacOsArm64
                | Platform::MacOsX86_64
                | Platform::LinuxX86_64
                | Platform::LinuxArm64
                | Platform::WindowsX86_64
        ));
    }

    #[test]
    fn platform_is_macos() {
        assert!(Platform::MacOsArm64.is_macos());
        assert!(Platform::MacOsX86_64.is_macos());
        assert!(!Platform::LinuxX86_64.is_macos());
        assert!(!Platform::WindowsX86_64.is_macos());
    }

    #[test]
    fn platform_is_windows() {
        assert!(Platform::WindowsX86_64.is_windows());
        assert!(!Platform::MacOsArm64.is_windows());
        assert!(!Platform::LinuxX86_64.is_windows());
    }

    #[test]
    fn download_url_macos_arm64() {
        let url = Platform::MacOsArm64.download_url("4.4.2");
        assert!(url.contains("arm64"));
        assert!(url.contains("4.4.2"));
        assert!(url.ends_with(".pkg"));
    }

    #[test]
    fn download_url_macos_x86() {
        let url = Platform::MacOsX86_64.download_url("4.3.1");
        assert!(url.contains("x86_64"));
        assert!(url.contains("4.3.1"));
        assert!(url.ends_with(".pkg"));
    }

    #[test]
    fn download_url_linux_x86() {
        let url = Platform::LinuxX86_64.download_url("4.4.2");
        assert!(url.contains("amd64"));
        assert!(url.contains("4.4.2"));
        assert!(url.ends_with(".deb"));
    }

    #[test]
    fn download_url_linux_arm64() {
        let url = Platform::LinuxArm64.download_url("4.4.2");
        assert!(url.contains("arm64"));
        assert!(url.ends_with(".deb"));
    }

    #[test]
    fn download_url_windows() {
        let url = Platform::WindowsX86_64.download_url("4.4.2");
        assert!(url.contains("windows"));
        assert!(url.contains("4.4.2"));
        assert!(url.ends_with(".exe"));
    }

    #[test]
    fn extract_r_home_from_script() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(
            bin_dir.join("R"),
            "#!/bin/sh\nR_HOME_DIR=/custom/path/to/R\nexport R_HOME\n",
        )
        .unwrap();
        let result = extract_r_home_dir(dir.path()).unwrap();
        assert_eq!(result, "/custom/path/to/R");
    }

    #[test]
    fn extract_r_home_fallback() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("R"), "#!/bin/sh\necho hello\n").unwrap();
        let result = extract_r_home_dir(dir.path()).unwrap();
        assert_eq!(result, "/Library/Frameworks/R.framework/Resources");
    }

    #[test]
    fn find_dir_with_r_binary_direct() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("R"), "").unwrap();
        let found = find_dir_with_r_binary(dir.path(), 0);
        assert_eq!(found, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn find_dir_with_r_binary_nested() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("Resources");
        let bin = nested.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("R"), "").unwrap();
        let found = find_dir_with_r_binary(dir.path(), 0);
        assert_eq!(found, Some(nested));
    }

    #[test]
    fn find_dir_with_r_binary_not_found() {
        let dir = TempDir::new().unwrap();
        let found = find_dir_with_r_binary(dir.path(), 0);
        assert!(found.is_none());
    }

    #[test]
    fn find_dir_depth_limit() {
        let dir = TempDir::new().unwrap();
        // Even if R binary exists deeply nested, depth=13 guard should kick in
        let found = find_dir_with_r_binary(dir.path(), 13);
        assert!(found.is_none());
    }

    #[test]
    fn find_data_tar_gz() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.tar.gz"), "").unwrap();
        let result = find_data_tar(dir.path()).unwrap();
        assert!(result.to_string_lossy().contains("data.tar.gz"));
    }

    #[test]
    fn find_data_tar_xz() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.tar.xz"), "").unwrap();
        let result = find_data_tar(dir.path()).unwrap();
        assert!(result.to_string_lossy().contains("data.tar.xz"));
    }

    #[test]
    fn find_data_tar_missing() {
        let dir = TempDir::new().unwrap();
        let result = find_data_tar(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn collect_payloads_finds_files() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("R-fw.pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Payload"), "data").unwrap();
        let payloads = find_all_payload_files(dir.path()).unwrap();
        assert_eq!(payloads.len(), 1);
    }

    #[test]
    fn collect_payloads_empty_dir() {
        let dir = TempDir::new().unwrap();
        let payloads = find_all_payload_files(dir.path()).unwrap();
        assert!(payloads.is_empty());
    }

    #[test]
    fn patch_makeconf_libr_rewrites() {
        let dir = TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(
            etc.join("Makeconf"),
            "CC = gcc\nLIBR = -F/Library/Frameworks/R.framework/.. -framework R\nCFLAGS = -O2\n",
        )
        .unwrap();
        patch_makeconf_libr(dir.path()).unwrap();
        let content = std::fs::read_to_string(etc.join("Makeconf")).unwrap();
        assert!(content.contains(&format!("LIBR = -L{}/lib -lR", dir.path().display())));
        assert!(content.contains("CC = gcc"));
        assert!(content.contains("CFLAGS = -O2"));
    }

    #[test]
    fn patch_makeconf_missing_file() {
        let dir = TempDir::new().unwrap();
        // Should be a no-op, not an error
        patch_makeconf_libr(dir.path()).unwrap();
    }

    #[test]
    fn write_renviron_site_creates_file() {
        let dir = TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        write_renviron_site(dir.path()).unwrap();
        let content = std::fs::read_to_string(etc.join("Renviron.site")).unwrap();
        assert!(content.contains("DYLD_LIBRARY_PATH=${R_HOME}/lib"));
        assert!(content.contains("LD_LIBRARY_PATH=${R_HOME}/lib"));
    }

    #[test]
    fn write_renviron_site_idempotent() {
        let dir = TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        write_renviron_site(dir.path()).unwrap();
        let first = std::fs::read_to_string(etc.join("Renviron.site")).unwrap();
        write_renviron_site(dir.path()).unwrap();
        let second = std::fs::read_to_string(etc.join("Renviron.site")).unwrap();
        assert_eq!(first, second, "write_renviron_site should be idempotent");
    }

    #[test]
    fn patch_text_files_replaces_in_text() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.txt"),
            "path=/old/location\nother=/old/location/sub\n",
        )
        .unwrap();
        patch_text_files(dir.path(), "/old/location", "/new/location").unwrap();
        let content = std::fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert!(content.contains("/new/location"));
        assert!(!content.contains("/old/location\n"));
    }

    #[test]
    fn patch_text_files_skips_binary() {
        let dir = TempDir::new().unwrap();
        let mut data = b"/old/location".to_vec();
        data.push(0); // null byte → binary file
        data.extend_from_slice(b"more data");
        std::fs::write(dir.path().join("binary.so"), &data).unwrap();
        patch_text_files(dir.path(), "/old/location", "/new/location").unwrap();
        let content = std::fs::read(dir.path().join("binary.so")).unwrap();
        // Should be unchanged — binary files are skipped
        assert_eq!(content, data);
    }
}
