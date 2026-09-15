use std::io::Write;
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

    /// Return the download URL for the portable R build of `version`.
    ///
    /// Every platform pulls a relocatable build from the rstudio/r-builds CDN
    /// (`cdn.posit.co/r`). These extract-and-run archives need no binary
    /// patching — R locates its own `R_HOME` at runtime (the `bin/R` text
    /// wrapper does get one cosmetic edit for static readers, #271). Note the macOS
    /// binaries carry only an ad-hoc code signature (not notarized; verified
    /// with `codesign -dv`), so download integrity rests on TLS to the CDN —
    /// the index publishes no checksums to verify against.
    pub fn download_url(&self, version: &str) -> String {
        match self {
            Platform::MacOsArm64 => {
                format!("{PORTABLE_CDN}/macos/R-{version}-macos-arm64.tar.gz")
            }
            Platform::MacOsX86_64 => {
                format!("{PORTABLE_CDN}/macos/R-{version}-macos.tar.gz")
            }
            Platform::LinuxX86_64 | Platform::LinuxArm64 => {
                let id = linux_portable_id();
                let arch = if matches!(self, Platform::LinuxArm64) {
                    "-arm64"
                } else {
                    ""
                };
                format!("{PORTABLE_CDN}/{id}/R-{version}-{id}{arch}.tar.gz")
            }
            Platform::WindowsX86_64 => {
                format!("{PORTABLE_CDN}/windows/R-{version}-windows.zip")
            }
        }
    }
}

/// Root of the rstudio/r-builds portable R CDN.
const PORTABLE_CDN: &str = "https://cdn.posit.co/r";

/// Unified version index for the portable builds.
const VERSIONS_JSON_URL: &str = "https://cdn.posit.co/r/versions.json";

/// Rolling channels the CDN publishes alongside numbered releases, on every
/// platform and architecture.
///
/// They are not versions. `devel` and `next` are rebuilt continuously, so what
/// a project pins today is not what it resolves to tomorrow — which is why
/// they are accepted but always labelled, never silently treated as a release.
pub const ROLLING_CHANNELS: &[&str] = &["devel", "next"];

/// True when `s` names a rolling channel rather than a release.
pub fn is_rolling_channel(s: &str) -> bool {
    ROLLING_CHANNELS.contains(&s)
}

/// Whether the portable CDN publishes `version` for `platform`.
///
/// Not a floor: Windows is not contiguous. The CDN has exactly one build below
/// 4.1.0 — R 3.6.3 — and nothing else until 4.1.0, so a `>=` bound either
/// excludes a build that exists (3.6.3) or advertises thirty that do not
/// (3.0.0–3.6.2, 4.0.0–4.0.5). Verified by HEAD-probing every version in
/// `versions.json` on both platforms:
///
/// ```text
/// 3.0.0 … 3.6.2   windows=403  macos=403
/// 3.6.3           windows=200  macos=403
/// 4.0.0 … 4.0.5   windows=403  macos=403
/// 4.1.0 …         windows=200  macos=200
/// ```
///
/// Linux is published continuously from 3.0.0 on both architectures.
fn portable_publishes(platform: Platform, version: (u32, u32, u32)) -> bool {
    if platform.is_macos() {
        version >= (4, 1, 0)
    } else if platform.is_windows() {
        version == (3, 6, 3) || version >= (4, 1, 0)
    } else {
        true
    }
}

/// Human-readable summary of what `platform` has published, for the error a
/// user sees when they ask for something else.
fn portable_availability(platform: Platform) -> Option<&'static str> {
    if platform.is_macos() {
        Some("R 4.1.0 or newer")
    } else if platform.is_windows() {
        Some("R 3.6.3, or R 4.1.0 or newer")
    } else {
        None
    }
}

/// manylinux_2_34 portable builds require glibc >= 2.34.
const MANYLINUX_GLIBC_MIN: (u32, u32) = (2, 34);

/// Portable build platform identifier for the running Linux libc:
/// `musllinux_1_2` on musl (Alpine), `manylinux_2_34` on glibc.
fn linux_portable_id() -> &'static str {
    if linux_is_musl() {
        "musllinux_1_2"
    } else {
        "manylinux_2_34"
    }
}

/// True when the host runs musl libc.
///
/// Four signals, gathered here and weighed in [`musl_from_signals`]: Alpine's
/// two self-identifications (`/etc/alpine-release`, `ID=alpine`), what `ldd
/// --version` reports, and which dynamic loaders exist under `/lib` and
/// `/lib64`.
fn linux_is_musl() -> bool {
    musl_from_signals(
        alpine_marker_present(),
        ldd_version_output().as_deref(),
        loader_present("ld-linux-"),
        loader_present("ld-musl-"),
    )
}

/// Decide the host libc from independent signals, strongest first.
///
/// The signal that used to decide this alone — "a musl loader file exists" —
/// says only that musl is *installed*, not that the host runs it. Any glibc
/// machine with the `musl` package (a Rust `x86_64-unknown-linux-musl` target
/// pulls it in, so this is common on developer boxes) claimed to be musl and
/// was handed musllinux R builds, which then fail to start:
///
/// ```text
/// Error relocating /lib/libz.so.1: __snprintf_chk: symbol not found
/// ```
///
/// `ldd --version` is asked first because that is what `install.sh` asks when
/// it chooses which uvr binary to hand the same host — two detectors that
/// disagree about one machine is the shape of #175 and #209, and worth not
/// repeating. The loader scan survives as a fallback for hosts without `ldd`,
/// but glibc now wins a tie: a box with both loaders runs glibc.
fn musl_from_signals(
    alpine: bool,
    ldd: Option<&str>,
    glibc_loader: bool,
    musl_loader: bool,
) -> bool {
    if alpine {
        return true;
    }
    if let Some(out) = ldd {
        let out = out.to_ascii_lowercase();
        if out.contains("musl") {
            return true;
        }
        if out.contains("gnu libc") || out.contains("glibc") {
            return false;
        }
    }
    if glibc_loader {
        return false;
    }
    musl_loader
}

/// Alpine identifies itself twice; either is conclusive and neither costs a
/// subprocess.
fn alpine_marker_present() -> bool {
    if std::path::Path::new("/etc/alpine-release").exists() {
        return true;
    }
    std::fs::read_to_string("/etc/os-release")
        .map(|content| {
            content.lines().any(|line| {
                line.strip_prefix("ID=")
                    .is_some_and(|v| v.trim_matches('"').eq_ignore_ascii_case("alpine"))
            })
        })
        .unwrap_or(false)
}

/// `ldd --version`, merging stderr: musl's ldd writes its banner there and
/// exits non-zero, so both the status and the stream have to be ignored.
fn ldd_version_output() -> Option<String> {
    let out = std::process::Command::new("ldd")
        .arg("--version")
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (!text.trim().is_empty()).then_some(text)
}

/// Whether any dynamic loader whose name starts with `prefix` is installed.
fn loader_present(prefix: &str) -> bool {
    ["/lib", "/lib64"].iter().any(|dir| {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with(prefix))
            })
            .unwrap_or(false)
    })
}

/// Parse the host glibc version from `getconf GNU_LIBC_VERSION` (e.g. "glibc 2.39").
/// Returns `None` when `getconf` is absent or unparseable (treated as "unknown",
/// so the floor check is skipped rather than failing a possibly-fine host).
fn detect_glibc_version() -> Option<(u32, u32)> {
    let out = Command::new("getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let ver = s.split_whitespace().last()?;
    let mut it = ver.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min))
}

/// Ensure the host libc is new enough for the portable Linux builds. No-op on
/// musl and on non-Linux hosts. Returns a clear error on glibc < 2.34, the
/// floor for the manylinux_2_34 builds (excludes Ubuntu 20.04, RHEL 8, Debian 11).
fn ensure_linux_libc_supported() -> Result<()> {
    if !cfg!(target_os = "linux") || linux_is_musl() {
        return Ok(());
    }
    if let Some((maj, min)) = detect_glibc_version() {
        if (maj, min) < MANYLINUX_GLIBC_MIN {
            let (rmaj, rmin) = MANYLINUX_GLIBC_MIN;
            return Err(UvrError::UnsupportedPlatform(format!(
                "glibc {maj}.{min} is too old for portable R builds (need >= {rmaj}.{rmin}). \
                 Distros below this floor — Ubuntu 20.04, RHEL 8, Debian 11 — are not supported \
                 by uvr's R installer. Use your system package manager's R, or build R from source."
            )));
        }
    }
    Ok(())
}

/// Lowest glibc the P3M `manylinux_2_28` package repo supports.
const PPM_MANYLINUX_GLIBC_MIN: (u32, u32) = (2, 28);

/// The portable P3M package repo usable on this host, if any.
///
/// Posit publishes a `manylinux_2_28` CRAN repo (preview) whose binaries
/// vendor their own shared libraries — `libxml2-35ea8990.so.2.9.7` and the
/// like, the same trick Python wheels use. Unlike the per-distro repos, they
/// carry no dependency on the host's library versions, so they work on
/// distros Posit doesn't publish for: Arch, Fedora, NixOS, Gentoo.
///
/// That makes this the right fallback for an unrecognized distro — better
/// than compiling from source, and correct where a per-distro binary is not
/// (#175). Verified on Arch: the manylinux `xml2` loads and parses, while the
/// jammy build fails on a missing `libxml2.so.2`.
///
/// `None` on musl, on glibc older than 2.28, and on non-Linux hosts (where
/// the per-platform repos already apply).
pub fn ppm_manylinux_repo() -> Option<&'static str> {
    if !cfg!(target_os = "linux") || linux_is_musl() {
        return None;
    }
    // An unreadable glibc version is not evidence of a new enough one.
    let (maj, min) = detect_glibc_version()?;
    ((maj, min) >= PPM_MANYLINUX_GLIBC_MIN).then_some("manylinux_2_28")
}

/// Parse an `X.Y.Z` R version into a comparable tuple. Extra components and
/// non-numeric tails are ignored. Returns `None` if the first component isn't numeric.
fn parse_r_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut it = version.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min, patch))
}

/// Process-wide override for the Posit CDN distro slug. Set by
/// `uvr r install --distribution <slug>` before invoking the downloader,
/// for users on Linux distros uvr can't autodetect (e.g. PopOS, Manjaro,
/// other Ubuntu/Arch derivatives — see #54).
static DISTRO_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the Posit CDN distro slug for the rest of this process. **Write-once**:
/// subsequent calls are silently ignored (`OnceLock::set` returns `Err`). This
/// matches the CLI's one-shot model — `uvr r install --distribution X` runs
/// once per process. Library consumers that need per-call overrides must run
/// each in a separate process.
///
/// Slug examples: `"ubuntu-2204"`, `"debian-12"`, `"rhel-9"`.
pub fn set_posit_distro_override(slug: String) {
    let _ = DISTRO_OVERRIDE.set(slug);
}

/// True when the user pinned the distro with `--distribution`. The live
/// platform catalog keys off `/etc/os-release`, which is precisely what an
/// override says not to trust, so the catalog path stands down when this is
/// set (#54).
pub fn distro_override_is_set() -> bool {
    DISTRO_OVERRIDE.get().is_some()
}

/// Detect the Posit CDN distro slug from `/etc/os-release`, or use the
/// override set by [`set_posit_distro_override`] if any.
///
/// Returns strings like `"ubuntu-2204"`, `"ubuntu-2404"`, `"debian-12"`,
/// `"centos-7"`, `"rhel-9"`, `"opensuse-154"`.
///
/// A distro Posit doesn't publish for returns its own identity (`"arch"`,
/// `"cachyos"`, `"nixos-25.05"`), which no PPM codename maps to — so P3M is
/// skipped and sync compiles from source. See
/// [`detect_posit_distro_slug_from_os_release`] for why that matters.
pub fn detect_posit_distro_slug() -> String {
    if let Some(override_slug) = DISTRO_OVERRIDE.get() {
        return override_slug.clone();
    }
    let content = std::fs::read_to_string("/etc/os-release").ok();
    detect_posit_distro_slug_from_os_release(content.as_deref())
}

/// Testable helper: parse os-release content (or fall back) into a Posit
/// CDN distro slug. Module-private; the inline test module calls it
/// directly. Production callers go through [`detect_posit_distro_slug`].
/// # Why an unknown distro must not guess
///
/// This slug's only remaining production consumer is P3M **binary package**
/// selection (`sync`) and the `doctor` display; R itself is installed from
/// portable manylinux/musllinux builds chosen by libc and architecture, so
/// `--distribution` is deprecated and ignored.
///
/// That changed what a wrong guess costs. It used to pick an R tarball, where
/// being approximately right still gave you a working R. Now it picks package
/// binaries that link the *host distro's* shared libraries by SONAME. Claiming
/// `ubuntu-2204` on Arch installed jammy binaries wanting `libxml2.so.2`
/// against a system that ships `libxml2.so.16` — the install "succeeded" in
/// seconds and every affected package failed at `library()` (#175).
///
/// There is no distro-neutral binary to fall back to either: every P3M flavour
/// that serves a binary (jammy, noble, rhel9) links `libxml2.so.2`. rstudio's
/// portable-R docs say the same thing — those builds cover the interpreter,
/// and "users can compile R packages from source on target systems".
///
/// So an unrecognized distro returns its own identity, which
/// [`crate::registry::p3m::ppm_linux_codename`] maps to `None`, P3M is skipped
/// and sync compiles from source against the libraries actually installed.
/// Slower, and correct. This mirrors what the `alpine` arm already does
/// deliberately.
pub(crate) fn detect_posit_distro_slug_from_os_release(content: Option<&str>) -> String {
    // No os-release at all: a scratch container, or not Linux. Nothing to
    // identify, so claim nothing rather than a distro we merely hope for.
    let Some(content) = content else {
        return "unknown".to_string();
    };

    let crate::os_release::OsRelease { id, version_id } =
        crate::os_release::OsRelease::parse(content);

    // Posit CDN uses no dots in version for Ubuntu/openSUSE, but keeps them for others
    match id.as_str() {
        "ubuntu" => {
            let ver = version_id.replace('.', "");
            format!("ubuntu-{ver}")
        }
        "debian" => format!("debian-{version_id}"),
        "centos" => format!("centos-{version_id}"),
        // Oracle Linux reports ID="ol" and is a straight RHEL rebuild, so it
        // takes RHEL's binaries. Without this it fell through to the
        // catch-all as `ol-8.10`, which maps to no PPM codename — Oracle
        // users compiled everything (#209 follow-up). Kept in step with the
        // sysreqs side of the same alias in `sysreqs::normalize_distro`.
        "rhel" | "rocky" | "almalinux" | "ol" => {
            let major = version_id.split('.').next().unwrap_or(&version_id);
            format!("rhel-{major}")
        }
        "opensuse-leap" | "sles" => {
            let ver = version_id.replace('.', "");
            format!("opensuse-{ver}")
        }
        "alpine" => {
            // Truncate `3.23.4` → `3.23` to match the #30 sysreqs normalization
            // and to make `ppm_linux_codename` return None (P3M is then skipped
            // cleanly; sync falls through to source compile).
            let minor = version_id.split('.').take(2).collect::<Vec<_>>().join(".");
            format!("alpine-{minor}")
        }
        // Anything Posit doesn't publish for — Arch, CachyOS, NixOS, Gentoo,
        // Fedora, Void — reports itself. No PPM codename matches, so the
        // caller falls through to a source build.
        _ if id.is_empty() => "unknown".to_string(),
        _ if version_id.is_empty() => id,
        _ => format!("{id}-{version_id}"),
    }
}

/// Parsed host platform triple, modeled after Rust target triples and
/// R's `R.Version()$platform` reporting.
///
/// Used to construct user-agent strings and to match `Built:` fields in
/// CRAN-like binary repositories (cran.rpkgs.com, P3M, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTriple {
    /// CPU architecture: `"x86_64"` | `"aarch64"`.
    pub arch: String,
    /// Vendor: `"pc"` on Linux/Windows, `"apple"` on macOS.
    pub vendor: String,
    /// OS: `"linux"` | `"darwin"` | `"windows"`.
    pub os: String,
    /// ABI / libc: `"gnu"` | `"musl"` | `"darwin"` | `"msvc"`.
    pub abi: String,
}

/// Build a `HostTriple` from optional `/etc/os-release` content and the
/// detected `Platform`. Module-private; the inline test module calls it
/// directly. Production callers go through [`host_triple()`].
fn host_triple_from_os_release(content: Option<&str>, platform: Platform) -> HostTriple {
    let mut id = String::new();
    if let Some(c) = content {
        for line in c.lines() {
            if let Some(val) = line.strip_prefix("ID=") {
                id = val.trim_matches('"').to_lowercase();
                break;
            }
        }
    }

    let arch = match platform {
        Platform::LinuxX86_64 | Platform::MacOsX86_64 | Platform::WindowsX86_64 => "x86_64",
        Platform::LinuxArm64 | Platform::MacOsArm64 => "aarch64",
    };

    let (vendor, os, default_abi) = match platform {
        Platform::LinuxX86_64 | Platform::LinuxArm64 => ("pc", "linux", "gnu"),
        Platform::MacOsArm64 | Platform::MacOsX86_64 => ("apple", "darwin", "darwin"),
        Platform::WindowsX86_64 => ("pc", "windows", "msvc"),
    };

    let abi = match (platform, id.as_str()) {
        (Platform::LinuxX86_64 | Platform::LinuxArm64, "alpine") => "musl",
        _ => default_abi,
    };

    HostTriple {
        arch: arch.to_string(),
        vendor: vendor.to_string(),
        os: os.to_string(),
        abi: abi.to_string(),
    }
}

/// Detect the host triple by reading `/etc/os-release` and combining with
/// `Platform::detect()`. Used at sync time to construct the UA and match
/// `Built:` fields.
pub fn host_triple() -> HostTriple {
    let content = std::fs::read_to_string("/etc/os-release").ok();
    let platform = Platform::detect().unwrap_or(Platform::LinuxX86_64);
    host_triple_from_os_release(content.as_deref(), platform)
}

/// Host platform info plus pretty distro label and R version, suitable for
/// constructing user-agent strings.
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub triple: HostTriple,
    /// Pretty distro label as it appears in the UA, e.g. `"Alpine Linux 3.23.4"`.
    /// Defaults to `"unknown"` when `/etc/os-release` is missing or sparse.
    pub distro_label: String,
    /// R minor or patch version, e.g. `"4.5.0"`. Caller-supplied; not detected.
    pub r_version: String,
}

/// Build a `HostInfo` from optional `/etc/os-release` content. Module-private;
/// the inline test module calls it directly. Production callers go through
/// [`host_info()`].
fn host_info_from_os_release(
    content: Option<&str>,
    platform: Platform,
    r_version: &str,
) -> HostInfo {
    let triple = host_triple_from_os_release(content, platform);

    let mut name = String::new();
    let mut version_id = String::new();
    if let Some(c) = content {
        for line in c.lines() {
            if let Some(val) = line.strip_prefix("NAME=") {
                name = val.trim_matches('"').to_string();
            } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
                version_id = val.trim_matches('"').to_string();
            }
        }
    }

    let distro_label = if name.is_empty() {
        "unknown".to_string()
    } else if version_id.is_empty() {
        name
    } else {
        format!("{name} {version_id}")
    };

    HostInfo {
        triple,
        distro_label,
        r_version: r_version.to_string(),
    }
}

/// Detect the host info. `r_version` should be the R version in use for the
/// project (caller-supplied because uvr knows the project R version).
pub fn host_info(r_version: &str) -> HostInfo {
    let content = std::fs::read_to_string("/etc/os-release").ok();
    let platform = Platform::detect().unwrap_or(Platform::LinuxX86_64);
    host_info_from_os_release(content.as_deref(), platform, r_version)
}

/// Normalize an R version for use in a User-Agent string (#124).
///
/// Real R always reports a full three-part version (`4.5.1`); uvr callers
/// often hold only the minor series (`4.5`). Pad a bare `X.Y` to `X.Y.0` so
/// every UA uvr sends uses one canonical form — the PPM index fetch
/// (`registry/p3m.rs`) and the tarball download (`host_info` →
/// [`user_agent`]) must agree, because the download cache key folds the UA
/// in (#122): divergent forms would mean spurious re-downloads if the two
/// paths ever fed the same URL.
pub fn normalize_ua_r_version(v: &str) -> String {
    if v.split('.').count() == 2 {
        format!("{v}.0")
    } else {
        v.to_string()
    }
}

/// Construct a User-Agent string matching what real R sends via
/// `getOption("HTTPUserAgent")`:
///
/// ```text
/// R (<ver> <triple> <arch> <os>-<abi>)
/// ```
///
/// Examples:
/// - Alpine: `R (4.5.0 x86_64-pc-linux-musl x86_64 linux-musl)`
/// - Ubuntu: `R (4.5.0 x86_64-pc-linux-gnu x86_64 linux-gnu)`
///
/// PPM's UA gating requires this exact `R (` prefix; see the test in
/// `registry/p3m.rs`. cran.rpkgs.com uses the platform triple substring
/// (`linux-musl` vs `linux-gnu`) to route requests to the right binary.
pub fn user_agent(info: &HostInfo) -> String {
    let HostTriple {
        arch,
        vendor,
        os,
        abi,
    } = &info.triple;
    format!(
        "R ({} {}-{}-{}-{} {} {}-{})",
        normalize_ua_r_version(&info.r_version),
        arch,
        vendor,
        os,
        abi,
        arch,
        os,
        abi
    )
}

/// Download and extract R to `<r-versions>/<version>/`.
///
/// `version` may be partial (`4.5`): it is resolved to the newest published
/// matching version before install, so the install directory is always a
/// full `X.Y.Z` that pin resolution and `r list --all` can match (#170).
///
/// The r-versions directory is `install_root` when given (`--install-dir`,
/// which wins over the env var per uv convention, #89), else
/// `UVR_R_INSTALL_DIR` / `~/.uvr/r-versions`.
pub async fn download_and_install_r(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
    install_root: Option<&Path>,
) -> Result<PathBuf> {
    let version = &resolve_install_version(client, version, platform).await?;
    let install_dir = match install_root {
        Some(root) => root.to_path_buf(),
        None => crate::env_vars::r_install_dir()
            .ok_or_else(|| UvrError::Other("Cannot determine r-versions directory".into()))?,
    }
    .join(version);

    let r_binary_name = if platform.is_windows() { "R.exe" } else { "R" };
    let r_binary = install_dir.join("bin").join(r_binary_name);
    if r_binary.exists() {
        // Validate the existing install actually works before short-
        // circuiting. The previous existence-only check let half-patched
        // installs (e.g. mvuorre's #99 on macOS 26.x) sit forever
        // because `uvr r install` skipped the reinstall, and downstream
        // checks treated a version probe returning nothing as "not
        // installed" and looped the user back here. Now: if `R
        // --version` succeeds we trust it; if it fails we nuke the dir
        // and reinstall fresh.
        if crate::r_version::detector::query_r_version(&r_binary).is_some() {
            info!("R {version} already installed at {}", install_dir.display());
            return Ok(install_dir);
        }
        info!(
            "R {version} install at {} is broken (no version response); reinstalling",
            install_dir.display()
        );
        std::fs::remove_dir_all(&install_dir).map_err(|e| {
            UvrError::Other(format!(
                "Failed to remove broken install at {}: {e}",
                install_dir.display()
            ))
        })?;
    }

    // Preflight: portable manylinux builds need glibc >= 2.34.
    ensure_linux_libc_supported()?;

    // Preflight: macOS and Windows publish only part of the version range —
    // fail with a clear message rather than a bare 403 from the CDN. Rolling
    // channels have no version to check and are published everywhere.
    if !is_rolling_channel(version) {
        if let (Some(available), Some(v)) =
            (portable_availability(platform), parse_r_version(version))
        {
            if !portable_publishes(platform, v) {
                return Err(UvrError::Other(format!(
                    "R {version} is not published for your platform. Portable builds here \
                     cover {available}."
                )));
            }
        }
    }

    let url = platform.download_url(version);
    info!("Downloading R {version} from {url}");

    let response = client.get(&url).send().await?;
    if response.status().is_client_error() {
        return Err(version_not_found_error(client, version, platform, response.status()).await);
    }
    let mut response = response.error_for_status()?;

    // Stream the archive to a temp file instead of buffering it (#134). The
    // macOS tarball is ~200 MB and `.bytes()` held every byte in RAM, which
    // OOMs 2 GB CI runners and memory-capped containers — the package
    // download path (installer/download.rs) has streamed for exactly this
    // reason. The temp file is a sibling of the install dir, not `/tmp`:
    // `/tmp` is a tmpfs on many hosts, which would put the payload straight
    // back into memory, and it is often smaller than the archive.
    let staging_root = install_dir.parent().ok_or_else(|| {
        UvrError::Other(format!(
            "Install path {} has no parent directory",
            install_dir.display()
        ))
    })?;
    std::fs::create_dir_all(staging_root).map_err(|e| {
        UvrError::Other(format!("Failed to create {}: {e}", staging_root.display()))
    })?;
    let mut archive = tempfile::Builder::new()
        .prefix(".uvr-r-dl-")
        .tempfile_in(staging_root)
        .map_err(|e| UvrError::Other(format!("Failed to create temp file for R archive: {e}")))?;
    while let Some(chunk) = response.chunk().await? {
        archive.write_all(&chunk)?;
    }
    archive.flush()?;

    // install_r_portable stages next to install_dir and moves the extracted
    // tree into place with one atomic rename — install_dir must not pre-exist.
    install_r_portable(archive.path(), &install_dir, platform)?;
    // Extraction is done with it — release the ~200 MB now rather than at the
    // end of the function, which still has a version probe to run.
    drop(archive);

    if !r_binary.exists() {
        // Don't leave a tree without bin/R behind: it would trip the
        // exists-but-broken reinstall path on every subsequent run.
        let _ = std::fs::remove_dir_all(&install_dir);
        return Err(UvrError::Other(format!(
            "R binary not found after installation at {}",
            r_binary.display()
        )));
    }

    // The portable macOS builds ship libomp.dylib but link it from nothing,
    // so binary packages built with -fopenmp can't resolve their symbols
    // (#116 fallout). Load it from R's own startup instead.
    match crate::r_version::openmp::ensure_openmp_shim(&install_dir) {
        Ok(true) => info!("Enabled the bundled OpenMP runtime for R {version}"),
        Ok(false) => {}
        Err(e) => tracing::warn!(
            "Could not configure the OpenMP runtime for R {version}: {e}. \
             Binary packages built with OpenMP (Rtsne, mgcv, ...) may fail to load."
        ),
    }

    // Run it. Until now this function proved only that `bin/R` *exists*, so an
    // R that cannot start — wrong libc, truncated download, missing system
    // library — was reported as a successful install and only surfaced later
    // as a confusing failure somewhere else. The already-installed path above
    // has always probed the binary through `query_r_version`; a fresh install
    // had no such check, which is exactly the case where a bad libc guess
    // lands.
    //
    // Not on Windows. There `bin/R.exe` is a front-end that re-spawns
    // `bin\<arch>\R.exe` through msvcrt's `system()`, and the probe comes back
    // empty for a freshly unzipped install even though the R it launches is
    // fine: CI watched this delete a working R 4.1.0 and call it a failed
    // install. Whatever the probe loses in that hand-off, gating on it here
    // trades a real bug for a hypothetical one — the failure this check exists
    // for is a libc mismatch, and Windows has no libc question and one build
    // per release. (`query_r_version` being unusable against a *managed*
    // Windows R is a pre-existing bug in its own right — the exists-path above
    // hits it too — but not one to fix blind from a Linux box.)
    if !platform.is_windows() {
        verify_r_runs(&r_binary).inspect_err(|_| {
            // Leave nothing behind that the exists-only short-circuit would
            // treat as installed on the next run.
            let _ = std::fs::remove_dir_all(&install_dir);
        })?;
    }

    info!("R {version} installed to {}", install_dir.display());
    Ok(install_dir)
}

/// Resolve the user-requested version to a full `X.Y.Z`.
///
/// Full versions pass through untouched (no network). A partial `4.5`
/// resolves to the newest published `4.5.x` for the platform; anything that
/// isn't version-shaped (`--`, `4..`, `latest`) errors immediately instead
/// of becoming a CDN 404 or a never-matching install directory (#170, #171).
async fn resolve_install_version(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
) -> Result<String> {
    if is_real_r_version(version) {
        return Ok(version.to_string());
    }
    // A channel is not resolved to anything: `devel` installs as `devel` and
    // stays that name on disk, so it is never mistaken for a pinned release.
    if is_rolling_channel(version) {
        return Ok(version.to_string());
    }
    if !crate::r_version::detector::is_plausible_r_version(version) {
        return Err(UvrError::Other(format!(
            "`{version}` is not a valid R version. Expected `X.Y.Z` (e.g. 4.5.1), a \
             partial `X.Y` (e.g. 4.5, which installs the newest 4.5.x), or a rolling \
             channel (`devel`, `next`)."
        )));
    }
    let available = fetch_available_versions(client, platform).await?;
    let resolved = pick_newest_matching(&available.stable, version).ok_or_else(|| {
        UvrError::Other(format!(
            "No published R version matches {version} for your platform. \
             See `uvr r list --all` for available versions."
        ))
    })?;
    info!("R {version} resolved to {resolved}");
    Ok(resolved)
}

/// Newest entry of `available` (sorted oldest-first) whose leading components
/// match `prefix`.
fn pick_newest_matching(available: &[String], prefix: &str) -> Option<String> {
    available
        .iter()
        .rev()
        .find(|v| crate::r_version::detector::version_matches_prefix(prefix, v))
        .cloned()
}

/// Build a helpful error when the portable CDN returns 4xx for a requested R
/// version. Best-effort: queries `versions.json` so users see "latest available
/// is 4.5.3" instead of a bare "404 Not Found".
async fn version_not_found_error(
    client: &reqwest::Client,
    version: &str,
    platform: Platform,
    status: reqwest::StatusCode,
) -> UvrError {
    let available_hint = match fetch_available_versions(client, platform).await {
        Ok(versions) if !versions.stable.is_empty() => {
            let latest = versions.stable.last().unwrap();
            format!(
                "\nLatest available for your platform: {latest}.\n\
                 Try `uvr r install {latest}`, or `uvr r list --all` to see every published version."
            )
        }
        _ => "\nCheck available versions with `uvr r list --all`. If R was just released, the \
              portable build for your platform may not be published yet — try again later."
            .to_string(),
    };
    UvrError::Other(format!(
        "R {version} is not published for your platform (HTTP {status}).{available_hint}"
    ))
}

/// True when `s` looks like a real R version string (`X.Y.Z` with
/// all-digit components, optionally with a fourth `.W` for build
/// numbers). Rejects directory-listing artefacts like `..` (parent-dir
/// link) and `.` that pass a naive digits-and-dots check. Used by both
/// the directory-listing scraper in `fetch_available_versions` (uvr-r
/// #9) and `scan_versions_from_listing` to keep the version surface
/// clean.
///
/// Three-component minimum: CRAN's R 4+ has never been published as
/// `X.Y` only — every release is `X.Y.Z`. Tightening the lower bound
/// catches false-accepts that the prior 2-component allowance let
/// through.
fn is_real_r_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 3 || parts.len() > 4 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Confirm a freshly installed R actually runs.
///
/// Starting R and asking it for its own version — [`query_r_version`] runs
/// `R --vanilla --slave -e "cat(R.version...)"` — is the cheapest end-to-end
/// proof that the build matches the host: it exercises the dynamic loader, the
/// bundled libraries and R's own startup. The common failure it catches is a
/// libc mismatch, so the message names that first: a musllinux build on glibc
/// dies in the loader with something like `symbol not found`, which means
/// nothing on its own.
///
/// Callers run this on Unix only — see the call site in
/// [`download_and_install_r`] for why Windows is left out.
///
/// [`query_r_version`]: crate::r_version::detector::query_r_version
fn verify_r_runs(r_binary: &Path) -> Result<()> {
    if crate::r_version::detector::query_r_version(r_binary).is_some() {
        return Ok(());
    }
    Err(UvrError::Other(format!(
        "The R build installed at {} does not run on this host — starting it and \
         asking for its version produced nothing. This usually means the build \
         does not match the host's libc or architecture. Report it with the \
         output of `uvr doctor`.",
        r_binary.display()
    )))
}

/// Locate the directory containing `bin/R` (or `bin/R.exe` on Windows) within an
/// extracted portable archive. Portable tarballs may nest R under a top-level
/// `R-<version>/` directory, so we recurse to find the real `R_HOME` root.
fn find_dir_with_r_binary(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 12 {
        return None; // guard against symlink loops
    }
    if has_r_binary(dir) {
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

/// What the portable CDN publishes for a platform.
#[derive(Debug, Clone, Default)]
pub struct AvailableVersions {
    /// Numbered releases this platform has, oldest first.
    pub stable: Vec<String>,
    /// Rolling channels the index carries — normally `devel` and `next`, on
    /// every platform, but empty if the index ever stops listing them. Never
    /// ordered against `stable`: they are labels, not points on the version
    /// line.
    pub rolling: Vec<String>,
}

/// Fetch the list of available R versions from the portable build index
/// (`cdn.posit.co/r/versions.json`).
///
/// Releases come back sorted oldest-first (e.g. `["4.3.0", "4.3.1", ...]`) and
/// filtered to what this platform publishes; the rolling channels come back
/// separately so callers can label them instead of ranking them among
/// releases — `next` is not "newer than 4.6.1", it is a different thing.
pub async fn fetch_available_versions(
    client: &reqwest::Client,
    platform: Platform,
) -> Result<AvailableVersions> {
    let body = client
        .get(VERSIONS_JSON_URL)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| UvrError::Other(format!("Failed to parse versions.json: {e}")))?;
    let arr = json
        .get("r_versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| UvrError::Other("versions.json missing `r_versions` array".into()))?;

    let mut stable: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str())
        // Drop rolling channels and any non-X.Y.Z label; they are reported
        // separately so they can be labelled rather than sorted among releases.
        .filter(|s| is_real_r_version(s))
        .filter(|s| parse_r_version(s).is_some_and(|v| portable_publishes(platform, v)))
        .map(|s| s.to_string())
        .collect();

    stable.sort_by_key(|a| parse_r_version(a));
    stable.dedup();

    // Only advertise channels the index actually carries, in a stable order.
    let listed: std::collections::HashSet<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    let rolling: Vec<String> = ROLLING_CHANNELS
        .iter()
        .filter(|c| listed.contains(*c))
        .map(|c| c.to_string())
        .collect();

    Ok(AvailableVersions { stable, rolling })
}

/// Install a portable R build by extracting the archive at `archive` into `dest`.
///
/// The rstudio/r-builds portable archives are relocatable: R resolves its own
/// `R_HOME` at runtime and bundles its dependency libraries. So there is no
/// install-name rewriting — we extract the archive and move the `R_HOME`
/// directory into `dest` with a single rename. The one text edit we do make
/// is to the `bin/R` wrapper's first `R_HOME_DIR` assignment (#271) — see
/// `patch_wrapper_r_home_dir`.
///
/// Extraction stages in a dot-prefixed sibling of `dest`, not the OS temp
/// dir: `/tmp` is commonly a different filesystem than `~/.uvr`, where a
/// cross-device rename fails (EXDEV) and a per-directory copy fallback could
/// be interrupted, leaving `dest` half-populated yet passing the `bin/R`
/// existence checks. Same-directory staging makes the final rename atomic:
/// `dest` either doesn't exist or is complete. The staging dir is removed on
/// every error path (dot-prefixed so version listing skips it if the process
/// dies uncleanly).
///
/// That rename is also the arbitration point between concurrent installs of
/// the same version (#135) — see the race handling below.
fn install_r_portable(archive: &Path, dest: &Path, platform: Platform) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        UvrError::Other(format!(
            "Install path {} has no parent directory",
            dest.display()
        ))
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|e| UvrError::Other(format!("Failed to create {}: {e}", parent.display())))?;
    let tmp = tempfile::Builder::new()
        .prefix(".uvr-stage-")
        .tempdir_in(parent)
        .map_err(|e| UvrError::Other(format!("Failed to create staging dir for R: {e}")))?;
    let stage = tmp.path();

    if platform.is_windows() {
        extract_zip_to(archive, stage)?;
    } else {
        extract_tar_gz_to(archive, stage)?;
    }

    // Portable archives may nest R under a top-level `R-<version>/` dir.
    let r_home = find_dir_with_r_binary(stage, 0).ok_or_else(|| {
        UvrError::Other(
            "Extracted R archive did not contain a bin/R — the download may be corrupt".into(),
        )
    })?;

    // Patch on the staged tree, before the rename, so `dest` appears
    // complete-and-correct atomically (and the race loser's unpatched copy
    // is simply discarded with its staging dir).
    if !platform.is_windows() {
        patch_wrapper_r_home_dir(&r_home, dest);
    }

    if let Err(e) = std::fs::rename(&r_home, dest) {
        // Another uvr process may have installed the same version while this
        // one was downloading (#135): the existence check at the top of
        // `download_and_install_r` is a check-then-act that parallel CI matrix
        // jobs or `make -j` invocations can both pass. Renaming a directory
        // onto a populated one fails rather than clobbering it, so a rename
        // failure with a usable R now at `dest` means we lost the race —
        // their tree came from the same URL as ours, so adopt it and let the
        // staging dir be removed on drop. Any other failure is real.
        if has_r_binary(dest) {
            info!(
                "Another process installed R at {} first; using it",
                dest.display()
            );
            return Ok(());
        }
        return Err(UvrError::Other(format!(
            "Failed to move extracted R into place ({} -> {}): {e}",
            r_home.display(),
            dest.display()
        )));
    }
    Ok(())
}

/// Point the first `R_HOME_DIR` assignment in the `bin/R` wrapper at the
/// installation's real location (#271).
///
/// The portable builds make the wrapper relocatable by *appending* an
/// override that recomputes `R_HOME_DIR` from the script's own path at
/// runtime, leaving the build machine's path (e.g.
/// `/Library/Frameworks/R.framework/Versions/4.4-arm64/Resources`) on the
/// first assignment. R never reads that stale line, but tools that read the
/// wrapper as text do: Positron resolves the interpreter from the first
/// `R_HOME_DIR=` it sees and then either fails to start or silently launches
/// a different R. Rewriting the first assignment is inert for shell
/// execution (the appended override recomputes the same value) and corrects
/// what static readers see.
///
/// Best-effort by design: a wrapper we don't recognize (no such line, or not
/// UTF-8) is left alone rather than failing the install — that's the
/// pre-#271 status quo, and R itself still runs.
fn patch_wrapper_r_home_dir(r_home: &Path, dest: &Path) {
    let wrapper = r_home.join("bin").join("R");
    let Ok(content) = std::fs::read_to_string(&wrapper) else {
        return;
    };
    let dest = dest.display().to_string();
    // Quote iff needed: the assignment is a live shell statement, so an
    // unquoted path with whitespace would break execution, but the upstream
    // wrapper (and the common case) is unquoted and some static readers may
    // not strip quotes.
    let value = if dest.chars().any(char::is_whitespace) {
        format!("\"{dest}\"")
    } else {
        dest
    };
    let mut patched = String::with_capacity(content.len());
    let mut done = false;
    for line in content.split_inclusive('\n') {
        if !done && line.trim_start().starts_with("R_HOME_DIR=") {
            let indent = &line[..line.len() - line.trim_start().len()];
            let newline = if line.ends_with('\n') { "\n" } else { "" };
            patched.push_str(&format!("{indent}R_HOME_DIR={value}{newline}"));
            done = true;
        } else {
            patched.push_str(line);
        }
    }
    if done {
        // In-place truncating write preserves the wrapper's exec bit.
        let _ = std::fs::write(&wrapper, patched);
    }
}

/// True when `dir` holds an R installation root (`bin/R`, or `bin/R.exe` on
/// Windows). Not a health check — [`crate::r_version::detector::query_r_version`]
/// is what decides whether an install actually runs.
fn has_r_binary(dir: &Path) -> bool {
    let bin = dir.join("bin");
    bin.join("R").exists() || bin.join("R.exe").exists()
}

/// Extract the `.tar.gz` at `archive` into `dest`, preserving symlinks and
/// unix permissions. Reads from the file rather than a `&[u8]` so the ~200 MB
/// payload never has to be resident (#134).
fn extract_tar_gz_to(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(|e| {
        UvrError::Other(format!(
            "Failed to open downloaded R archive {}: {e}",
            archive.display()
        ))
    })?;
    let dec = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut ar = tar::Archive::new(dec);
    ar.set_preserve_permissions(true);
    ar.set_overwrite(true);
    ar.unpack(dest)
        .map_err(|e| UvrError::Other(format!("Failed to extract R tarball: {e}")))?;
    Ok(())
}

/// Extract the `.zip` at `archive` into `dest` (Windows portable builds).
/// `ZipArchive` needs `Read + Seek`, which the file provides directly — no
/// in-memory `Cursor` over the whole payload (#134).
///
/// Validates every entry against path traversal (zip-slip) before writing,
/// mirroring the package-install path in `installer::binary_install`: entries
/// with absolute paths or `..` components are rejected, and the resolved
/// destination must stay under `dest`. The CDN serves no checksum sidecar
/// for these archives, so a tampered response must not be able to write
/// outside the staging directory (#146).
///
/// No permission handling: this path only runs for Windows portable builds
/// (see `install_r_portable`), where unix exec bits don't exist.
fn extract_zip_to(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(|e| {
        UvrError::Other(format!(
            "Failed to open downloaded R archive {}: {e}",
            archive.display()
        ))
    })?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| UvrError::Other(format!("Failed to open R zip: {e}")))?;

    let canonical_dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| UvrError::Other(format!("Failed to read R zip entry: {e}")))?;

        // Guard against path traversal: reject entries with `..` components
        // or absolute paths that would escape the destination directory.
        let path = PathBuf::from(entry.name());
        if path.is_absolute()
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(UvrError::Other(format!(
                "Path traversal detected in R zip: {}",
                entry.name()
            )));
        }

        let outpath = canonical_dest.join(&path);
        if !outpath.starts_with(&canonical_dest) {
            return Err(UvrError::Other(format!(
                "Path traversal detected in R zip: {}",
                entry.name()
            )));
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&outpath).map_err(|e| {
                UvrError::Other(format!("Failed to create directory extracting R zip: {e}"))
            })?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    UvrError::Other(format!(
                        "Failed to create parent directory extracting R zip: {e}"
                    ))
                })?;
            }
            let mut outfile = std::fs::File::create(&outpath).map_err(|e| {
                UvrError::Other(format!("Failed to create file extracting R zip: {e}"))
            })?;
            std::io::copy(&mut entry, &mut outfile)
                .map_err(|e| UvrError::Other(format!("Failed to write R zip entry: {e}")))?;
        }
    }
    Ok(())
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
        assert_eq!(
            url,
            "https://cdn.posit.co/r/macos/R-4.4.2-macos-arm64.tar.gz"
        );
    }

    #[test]
    fn download_url_macos_x86() {
        let url = Platform::MacOsX86_64.download_url("4.3.1");
        assert_eq!(url, "https://cdn.posit.co/r/macos/R-4.3.1-macos.tar.gz");
    }

    #[test]
    fn download_url_linux_x86_is_portable() {
        let url = Platform::LinuxX86_64.download_url("4.4.2");
        // The libc infix (manylinux_2_34 vs musllinux_1_2) is host-dependent,
        // so assert the portable shape rather than the exact infix.
        assert!(url.starts_with("https://cdn.posit.co/r/"));
        assert!(url.contains("/R-4.4.2-"));
        assert!(url.ends_with(".tar.gz"));
        assert!(!url.contains("-arm64"));
    }

    #[test]
    fn download_url_linux_arm64_is_portable() {
        let url = Platform::LinuxArm64.download_url("4.4.2");
        assert!(url.contains("/R-4.4.2-"));
        assert!(url.ends_with("-arm64.tar.gz"));
    }

    #[test]
    fn download_url_windows() {
        let url = Platform::WindowsX86_64.download_url("4.4.2");
        assert_eq!(url, "https://cdn.posit.co/r/windows/R-4.4.2-windows.zip");
    }

    #[test]
    fn is_real_r_version_accepts_versions() {
        assert!(is_real_r_version("4.5.3"));
        assert!(is_real_r_version("3.6.3"));
        assert!(is_real_r_version("4.5.3.0")); // 4 components, rare but valid
    }

    #[test]
    fn is_real_r_version_rejects_noise() {
        assert!(!is_real_r_version(".."));
        assert!(!is_real_r_version("."));
        assert!(!is_real_r_version(""));
        assert!(!is_real_r_version("4."));
        assert!(!is_real_r_version(".4.5"));
        assert!(!is_real_r_version("4..5"));
        assert!(!is_real_r_version("v4.5.3"));
        assert!(!is_real_r_version("4.5.3-rc"));
        assert!(!is_real_r_version("4.6"));
        // Rolling channels in versions.json must be filtered out.
        assert!(!is_real_r_version("next"));
        assert!(!is_real_r_version("devel"));
    }

    #[test]
    fn pick_newest_matching_resolves_partials() {
        let available: Vec<String> = ["4.4.3", "4.5.0", "4.5.1", "4.5.3", "4.6.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            pick_newest_matching(&available, "4.5").as_deref(),
            Some("4.5.3")
        );
        assert_eq!(
            pick_newest_matching(&available, "4.5.1").as_deref(),
            Some("4.5.1")
        );
        assert_eq!(pick_newest_matching(&available, "4.7"), None);
        // Single-component prefixes never reach this fn — resolve_install_version
        // rejects them via is_plausible_r_version — but the match itself works.
        assert_eq!(
            pick_newest_matching(&available, "4").as_deref(),
            Some("4.6.0")
        );
    }

    #[test]
    fn parse_r_version_orders_correctly() {
        assert_eq!(parse_r_version("4.4.2"), Some((4, 4, 2)));
        assert_eq!(parse_r_version("4.4"), Some((4, 4, 0)));
        assert!(parse_r_version("4.1.0") > parse_r_version("4.0.5"));
        assert!(parse_r_version("next").is_none());
    }

    #[test]
    fn macos_publishes_from_4_1_0() {
        for p in [Platform::MacOsArm64, Platform::MacOsX86_64] {
            assert!(!portable_publishes(p, (4, 0, 5)));
            assert!(!portable_publishes(p, (3, 6, 3)));
            assert!(portable_publishes(p, (4, 1, 0)));
            assert!(portable_publishes(p, (4, 6, 1)));
        }
    }

    #[test]
    fn windows_publishes_3_6_3_and_then_nothing_until_4_1_0() {
        // The reason this is a predicate and not a floor. A `>= 4.1.0` bound
        // rejects 3.6.3, which exists; a `>= 3.6.3` bound advertises 4.0.0
        // through 4.0.5, which do not. Both were verified by HEAD-probing
        // every version in versions.json.
        let p = Platform::WindowsX86_64;
        assert!(portable_publishes(p, (3, 6, 3)));
        assert!(!portable_publishes(p, (3, 6, 2)));
        assert!(!portable_publishes(p, (4, 0, 5)));
        assert!(portable_publishes(p, (4, 1, 0)));
    }

    #[test]
    fn linux_publishes_the_whole_range() {
        for p in [Platform::LinuxX86_64, Platform::LinuxArm64] {
            assert!(portable_publishes(p, (3, 0, 0)));
            assert!(portable_publishes(p, (4, 0, 5)));
            assert!(portable_publishes(p, (4, 6, 1)));
            assert!(portable_availability(p).is_none());
        }
    }

    #[test]
    fn rolling_channels_are_named_not_versioned() {
        assert!(is_rolling_channel("devel"));
        assert!(is_rolling_channel("next"));
        assert!(!is_rolling_channel("4.6.1"));
        assert!(!is_rolling_channel("nightly"));
        // A channel is not a version, so it must never be mistaken for one —
        // otherwise `4.5` could resolve to it, or it could sort as "latest".
        for ch in ROLLING_CHANNELS {
            assert!(!is_real_r_version(ch));
            assert!(parse_r_version(ch).is_none());
        }
    }

    #[test]
    fn channels_are_published_on_every_platform() {
        // Unlike releases, the CDN carries devel/next for macOS and Windows
        // too (HEAD-probed). The preflight must not reject them for platforms
        // whose numbered range starts later.
        for p in [
            Platform::MacOsArm64,
            Platform::WindowsX86_64,
            Platform::LinuxX86_64,
        ] {
            for ch in ROLLING_CHANNELS {
                assert!(
                    parse_r_version(ch).is_none(),
                    "the preflight only runs on parseable versions, so {ch} passes on {p:?}"
                );
            }
        }
    }

    #[test]
    fn a_musl_loader_on_a_glibc_host_is_not_a_musl_host() {
        // The regression. Installing Rust's x86_64-unknown-linux-musl target
        // pulls in /lib/ld-musl-x86_64.so.1 on an ordinary glibc box; the old
        // check saw that file and handed the host musllinux R builds, which
        // fail in the loader on first run.
        assert!(!musl_from_signals(
            false,
            Some("ldd (GNU libc) 2.44"),
            true,
            true
        ));
        // Even with no ldd to ask, glibc wins the tie.
        assert!(!musl_from_signals(false, None, true, true));
    }

    #[test]
    fn a_real_musl_host_is_still_musl() {
        // Alpine, by either marker.
        assert!(musl_from_signals(true, None, false, false));
        // A musl distro that is not Alpine: ldd says so.
        assert!(musl_from_signals(
            false,
            Some("musl libc (x86_64)\nVersion 1.2.5"),
            false,
            true
        ));
        // No ldd at all, only a musl loader.
        assert!(musl_from_signals(false, None, false, true));
    }

    #[test]
    fn an_ordinary_glibc_host_is_not_musl() {
        assert!(!musl_from_signals(
            false,
            Some("ldd (GNU libc) 2.39"),
            true,
            false
        ));
        // Nothing conclusive anywhere: assume glibc, which is what every
        // mainstream distro is. Guessing musl would send them a build that
        // cannot start; guessing glibc at worst sends a build that is checked
        // by verify_r_runs.
        assert!(!musl_from_signals(false, None, false, false));
    }

    #[cfg(unix)]
    #[test]
    fn verify_r_runs_rejects_an_r_that_cannot_start() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();

        // A build that runs and reports a version.
        let good = dir.path().join("R-good");
        // query_r_version asks R to print `major.minor` and reads the last
        // version-shaped line, so that is what the stand-in prints.
        std::fs::write(&good, "#!/bin/sh\necho 4.5.1\n").unwrap();
        std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(verify_r_runs(&good).is_ok());

        // A build that dies in the loader, as a musllinux R does on glibc.
        let bad = dir.path().join("R-bad");
        std::fs::write(
            &bad,
            "#!/bin/sh\necho 'Error relocating /lib/libz.so.1: symbol not found' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = verify_r_runs(&bad).unwrap_err().to_string();
        assert!(err.contains("does not run on this host"), "got {err}");
        assert!(
            err.contains("libc"),
            "the message must name the likely cause: {err}"
        );
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

    /// Write a gzipped tar of `(path, contents, mode)` entries to a temp file
    /// and return it. `install_r_portable` now reads the archive from disk
    /// (#134), so tests hand it a path rather than a byte slice.
    fn write_tar_gz(entries: &[(&str, &[u8], u32)]) -> tempfile::NamedTempFile {
        let mut tar_buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            for (path, contents, mode) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(*mode);
                header.set_cksum();
                builder.append_data(&mut header, path, *contents).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&tar_buf).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn install_r_portable_flattens_nested_tarball() {
        // Build a .tar.gz whose R_HOME is nested under `R-4.4.2/` (as the
        // portable archives are), then verify install_r_portable flattens it
        // so `<dest>/bin/R` exists.
        let archive = write_tar_gz(&[
            ("R-4.4.2/bin/R", b"#!/bin/sh\necho R\n", 0o755),
            ("R-4.4.2/lib/libR.so", b"libR", 0o644),
        ]);

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        install_r_portable(archive.path(), &dest, Platform::LinuxX86_64).unwrap();
        assert!(dest.join("bin").join("R").exists());
        assert!(dest.join("lib").join("libR.so").exists());
        // The sibling staging dir must be gone after a successful install.
        let leftovers: Vec<_> = std::fs::read_dir(dest_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".uvr-stage-"))
            .collect();
        assert!(leftovers.is_empty(), "staging dir leaked: {leftovers:?}");
    }

    #[test]
    fn install_r_portable_patches_stale_r_home_dir_in_wrapper() {
        // #271: the portable wrapper carries the build machine's R_HOME_DIR
        // on its first assignment and only overrides it later at runtime.
        // Static readers (Positron) trust the first assignment, so the
        // install must rewrite it to the real location — and leave the
        // runtime override untouched.
        let wrapper = b"#!/bin/sh\n\
# Shell wrapper for R executable.\n\
\n\
R_HOME_DIR=/Library/Frameworks/R.framework/Versions/4.4-arm64/Resources\n\
# Override R_HOME_DIR for relocatable installation\n\
R_HOME_DIR=\"$(cd \"$(dirname \"$0\")/..\" && pwd)\"\n";
        let archive = write_tar_gz(&[("R-4.4.1/bin/R", wrapper, 0o755)]);

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.1");
        install_r_portable(archive.path(), &dest, Platform::MacOsArm64).unwrap();

        let patched = std::fs::read_to_string(dest.join("bin").join("R")).unwrap();
        let lines: Vec<&str> = patched.lines().collect();
        assert_eq!(
            lines[3],
            format!("R_HOME_DIR={}", dest.display()),
            "first assignment must point at the install"
        );
        assert_eq!(
            lines[5], "R_HOME_DIR=\"$(cd \"$(dirname \"$0\")/..\" && pwd)\"",
            "runtime override must be untouched"
        );
        assert!(
            !patched.contains("/Library/Frameworks"),
            "build-time path must be gone"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("bin").join("R"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "wrapper must stay executable");
        }
    }

    #[test]
    fn install_r_portable_leaves_unrecognized_wrapper_alone() {
        // A wrapper with no R_HOME_DIR line (our other tests' fake scripts,
        // or a future upstream layout change) must install unmodified.
        let archive = write_tar_gz(&[("R-4.4.2/bin/R", b"#!/bin/sh\necho R\n", 0o755)]);

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        install_r_portable(archive.path(), &dest, Platform::LinuxX86_64).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("bin").join("R")).unwrap(),
            "#!/bin/sh\necho R\n"
        );
    }

    #[test]
    fn install_r_portable_cleans_stage_on_bad_archive() {
        // A tarball with no bin/R must error AND leave neither dest nor a
        // staging dir behind.
        let archive = write_tar_gz(&[("R-4.4.2/README", b"not R", 0o644)]);

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        assert!(install_r_portable(archive.path(), &dest, Platform::LinuxX86_64).is_err());
        assert!(!dest.exists(), "dest must not exist after a failed install");
        let leftovers: Vec<_> = std::fs::read_dir(dest_dir.path())
            .unwrap()
            .flatten()
            .collect();
        assert!(leftovers.is_empty(), "staging dir leaked: {leftovers:?}");
    }

    #[test]
    fn install_r_portable_yields_to_a_concurrent_install() {
        // #135: two `uvr r install <same-ver>` processes can both pass the
        // "not installed" check and both extract. The rename arbitrates —
        // the loser must adopt the winner's tree (not clobber it, not error,
        // not leak its staging dir), which is what a pre-existing populated
        // dest simulates here.
        let archive = write_tar_gz(&[("R-4.4.2/bin/R", b"#!/bin/sh\necho ours\n", 0o755)]);

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        std::fs::create_dir_all(dest.join("bin")).unwrap();
        std::fs::write(dest.join("bin").join("R"), b"#!/bin/sh\necho theirs\n").unwrap();

        install_r_portable(archive.path(), &dest, Platform::LinuxX86_64)
            .expect("losing the rename race must not be an error");
        // The winner's install is untouched.
        assert_eq!(
            std::fs::read_to_string(dest.join("bin").join("R")).unwrap(),
            "#!/bin/sh\necho theirs\n"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dest_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".uvr-stage-"))
            .collect();
        assert!(leftovers.is_empty(), "staging dir leaked: {leftovers:?}");
    }

    #[test]
    fn install_r_portable_still_errors_when_dest_is_unusable() {
        // The race arm must not swallow genuine rename failures: a dest that
        // exists but holds no R is a broken state, not another process's win.
        let archive = write_tar_gz(&[("R-4.4.2/bin/R", b"#!/bin/sh\necho R\n", 0o755)]);

        let dest_dir = TempDir::new().unwrap();
        let dest = dest_dir.path().join("4.4.2");
        std::fs::create_dir_all(dest.join("junk")).unwrap();
        std::fs::write(dest.join("junk").join("file"), b"x").unwrap();

        assert!(install_r_portable(archive.path(), &dest, Platform::LinuxX86_64).is_err());
    }

    #[test]
    fn host_triple_alpine_x86_64() {
        let os_release = r#"NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.23.4
"#;
        let triple = host_triple_from_os_release(Some(os_release), Platform::LinuxX86_64);
        assert_eq!(triple.arch, "x86_64");
        assert_eq!(triple.vendor, "pc");
        assert_eq!(triple.os, "linux");
        assert_eq!(triple.abi, "musl");
    }

    #[test]
    fn host_triple_ubuntu_x86_64() {
        let os_release = r#"NAME="Ubuntu"
ID=ubuntu
VERSION_ID="22.04"
"#;
        let triple = host_triple_from_os_release(Some(os_release), Platform::LinuxX86_64);
        assert_eq!(triple.abi, "gnu");
    }

    #[test]
    fn host_triple_alpine_aarch64() {
        let os_release = r#"ID=alpine
VERSION_ID=3.23
"#;
        let triple = host_triple_from_os_release(Some(os_release), Platform::LinuxArm64);
        assert_eq!(triple.arch, "aarch64");
        assert_eq!(triple.abi, "musl");
    }

    #[test]
    fn host_triple_no_os_release_falls_back_to_gnu() {
        let triple = host_triple_from_os_release(None, Platform::LinuxX86_64);
        assert_eq!(triple.abi, "gnu");
    }

    #[test]
    fn host_triple_macos() {
        let triple = host_triple_from_os_release(None, Platform::MacOsArm64);
        assert_eq!(triple.vendor, "apple");
        assert_eq!(triple.os, "darwin");
        assert_eq!(triple.abi, "darwin");
    }

    #[test]
    fn host_triple_windows() {
        let triple = host_triple_from_os_release(None, Platform::WindowsX86_64);
        assert_eq!(triple.arch, "x86_64");
        assert_eq!(triple.vendor, "pc");
        assert_eq!(triple.os, "windows");
        assert_eq!(triple.abi, "msvc");
    }

    #[test]
    fn user_agent_alpine_matches_real_r() {
        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "musl".into(),
            },
            distro_label: "Alpine Linux 3.23.4".into(),
            r_version: "4.5.0".into(),
        };
        assert_eq!(
            user_agent(&info),
            "R (4.5.0 x86_64-pc-linux-musl x86_64 linux-musl)"
        );
    }

    #[test]
    fn user_agent_ubuntu_matches_real_r() {
        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "gnu".into(),
            },
            distro_label: "Ubuntu 22.04".into(),
            r_version: "4.5.0".into(),
        };
        assert_eq!(
            user_agent(&info),
            "R (4.5.0 x86_64-pc-linux-gnu x86_64 linux-gnu)"
        );
    }

    #[test]
    fn user_agent_normalizes_minor_only_r_version() {
        // #124: sync.rs feeds host_info() the minor series ("4.5") while
        // p3m.rs builds its index UA as "{r_minor}.0". Both must emit the
        // same canonical three-part form, or the download cache key (which
        // folds the UA in, #122) diverges between paths.
        assert_eq!(normalize_ua_r_version("4.5"), "4.5.0");
        assert_eq!(normalize_ua_r_version("4.5.1"), "4.5.1");

        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "gnu".into(),
            },
            distro_label: "Ubuntu 22.04".into(),
            r_version: "4.5".into(), // minor-only, as passed by sync.rs
        };
        // Must exactly match the p3m.rs index-fetch UA for the same R.
        assert_eq!(
            user_agent(&info),
            "R (4.5.0 x86_64-pc-linux-gnu x86_64 linux-gnu)"
        );
    }

    #[test]
    fn user_agent_satisfies_ppm_gating() {
        // PPM's UA gating in registry/p3m.rs sniffs for the literal "R (" prefix.
        // Regression guard: any future change to user_agent() that drops this
        // prefix will silently break P3M binary downloads on Ubuntu/Debian.
        let info = HostInfo {
            triple: HostTriple {
                arch: "x86_64".into(),
                vendor: "pc".into(),
                os: "linux".into(),
                abi: "gnu".into(),
            },
            distro_label: "Ubuntu 22.04".into(),
            r_version: "4.5.0".into(),
        };
        let ua = user_agent(&info);
        assert!(
            ua.starts_with("R ("),
            "PPM gating requires 'R (' prefix; got: {ua}"
        );
        assert!(ua.contains("linux-gnu"));
    }

    #[test]
    fn host_info_uses_pretty_distro_label() {
        let os_release = r#"NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.23.4
"#;
        let info = host_info_from_os_release(Some(os_release), Platform::LinuxX86_64, "4.5.0");
        assert_eq!(info.distro_label, "Alpine Linux 3.23.4");
        assert_eq!(info.r_version, "4.5.0");
    }

    #[test]
    fn host_info_unknown_distro_label() {
        let info = host_info_from_os_release(None, Platform::LinuxX86_64, "4.5.0");
        assert_eq!(info.distro_label, "unknown");
    }

    #[test]
    fn detect_posit_distro_slug_alpine_full_version() {
        // Alpine 3.23.4 reports VERSION_ID="3.23.4"; we truncate to 3.23
        // (matching the existing #30 sysreqs normalization).
        let slug = detect_posit_distro_slug_from_os_release(Some("ID=alpine\nVERSION_ID=3.23.4\n"));
        assert_eq!(slug, "alpine-3.23");
    }

    #[test]
    fn detect_posit_distro_slug_alpine_minor_only() {
        let slug = detect_posit_distro_slug_from_os_release(Some("ID=alpine\nVERSION_ID=3.21\n"));
        assert_eq!(slug, "alpine-3.21");
    }

    #[test]
    fn detect_posit_distro_slug_unknown_distro_reports_itself() {
        // #175: claiming ubuntu-2204 here installed jammy binaries on Arch
        // that could not load. An unknown distro now identifies itself, and
        // no PPM codename matches it, so sync compiles from source.
        for (os_release, expected) in [
            ("ID=arch\n", "arch"),
            ("ID=cachyos\nID_LIKE=arch\n", "cachyos"),
            ("ID=gentoo\n", "gentoo"),
            ("ID=nixos\nVERSION_ID=\"25.05\"\n", "nixos-25.05"),
            ("ID=fedora\nVERSION_ID=42\n", "fedora-42"),
        ] {
            let slug = detect_posit_distro_slug_from_os_release(Some(os_release));
            assert_eq!(slug, expected, "slug for {os_release:?}");
            assert_eq!(
                crate::registry::p3m::ppm_linux_codename(&slug),
                None,
                "{slug} must not resolve to a PPM codename, or binaries get installed"
            );
        }
    }

    #[test]
    fn detect_posit_distro_slug_without_os_release_claims_nothing() {
        let slug = detect_posit_distro_slug_from_os_release(None);
        assert_eq!(slug, "unknown");
        assert_eq!(crate::registry::p3m::ppm_linux_codename(&slug), None);
    }

    #[test]
    fn detect_posit_distro_slug_still_recognizes_supported_distros() {
        // Regression: the distros Posit *does* publish for must keep
        // resolving to a codename, or this fix would disable binaries for
        // everyone.
        for (os_release, slug, codename) in [
            ("ID=ubuntu\nVERSION_ID=\"22.04\"\n", "ubuntu-2204", "jammy"),
            ("ID=ubuntu\nVERSION_ID=\"24.04\"\n", "ubuntu-2404", "noble"),
            ("ID=debian\nVERSION_ID=\"12\"\n", "debian-12", "bookworm"),
            ("ID=rocky\nVERSION_ID=\"9.3\"\n", "rhel-9", "rhel9"),
        ] {
            let got = detect_posit_distro_slug_from_os_release(Some(os_release));
            assert_eq!(got, slug);
            assert_eq!(
                crate::registry::p3m::ppm_linux_codename(&got),
                Some(codename)
            );
        }
    }

    #[test]
    fn detect_posit_distro_slug_alpine_skips_p3m() {
        // Integration check: alpine slug must not resolve to a PPM codename,
        // so P3MBinaryIndex returns empty for alpine and sync falls through
        // to source compile.
        assert!(crate::registry::p3m::ppm_linux_codename("alpine-3.23").is_none());
        assert!(crate::registry::p3m::ppm_linux_codename("alpine-3.21").is_none());
    }

    /// Build a zip on disk whose entries are `(name, contents)` pairs and
    /// return its tempfile (extract_zip_to reads from a path, #134).
    fn build_zip(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let file = tempfile::NamedTempFile::new().unwrap();
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(file.path()).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, contents) in entries {
                zip.start_file(*name, options).unwrap();
                zip.write_all(contents).unwrap();
            }
            zip.finish().unwrap();
        }
        file
    }

    #[test]
    fn extract_zip_to_extracts_normal_entries() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let archive = build_zip(&[
            ("R-4.4.1/bin/R.exe", b"fake binary"),
            ("R-4.4.1/library/base/DESCRIPTION", b"Package: base\n"),
        ]);
        extract_zip_to(archive.path(), &dest).unwrap();

        assert!(dest.join("R-4.4.1").join("bin").join("R.exe").exists());
        assert!(dest
            .join("R-4.4.1")
            .join("library")
            .join("base")
            .join("DESCRIPTION")
            .exists());
    }

    #[test]
    fn extract_zip_to_rejects_parent_dir_traversal() {
        // #146: a tampered CDN response with a `../evil` entry must not be
        // able to write outside the staging directory.
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let archive = build_zip(&[("../evil.txt", b"escaped")]);
        let err = extract_zip_to(archive.path(), &dest).unwrap_err();
        assert!(err.to_string().contains("Path traversal"), "{err}");
        assert!(!dir.path().join("evil.txt").exists());
        assert!(!dest.join("evil.txt").exists());
    }

    #[test]
    fn extract_zip_to_rejects_absolute_paths() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let archive = build_zip(&[("/tmp/evil.txt", b"escaped")]);
        let err = extract_zip_to(archive.path(), &dest).unwrap_err();
        assert!(err.to_string().contains("Path traversal"), "{err}");
    }
}
