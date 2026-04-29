use anyhow::{Context, Result};
use indicatif::ProgressBar;

pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("Failed to build HTTP client")
}

/// Re-export from the ui module so existing call sites keep compiling.
pub fn make_spinner(msg: &str) -> ProgressBar {
    crate::ui::make_spinner(msg)
}

/// R-pin sanity check (#63, #64, #70). Best-effort: never errors, never
/// blocks the command. Surfaces three classes of mismatch:
///
/// 1. Pin set but the pinned version isn't installed → "Run uvr r install".
/// 2. Pin minor != the R uvr will resolve and use → silent-fallback warning.
/// 3. Pin set, R uvr resolves to == pin, but uvr is being **invoked from an
///    R session whose minor differs from the pin** (`R_HOME` env points at a
///    different R) → packages built for the pinned R won't load in the calling
///    session. This is the case from #70 — pin to 4.6 from inside a 4.5 R
///    session, sync silently rebuilds the library for 4.6 and the calling
///    session ends up unable to load anything.
pub fn warn_r_pin_mismatch() {
    use uvr_core::project::Project;
    use uvr_core::r_version::detector::{find_all, find_r_binary, query_r_version};

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let project = match Project::find(&cwd) {
        Ok(p) => p,
        Err(_) => return,
    };

    let r_version_str = project.manifest.project.r_version.clone();
    let pin_source: Option<(&'static str, String)> = project
        .read_r_version_pin()
        .map(|v| (".r-version", v))
        .or_else(|| {
            r_version_str
                .as_deref()
                .filter(|v| looks_like_exact(v))
                .map(|v| ("uvr.toml [project] r_version", v.to_string()))
        });
    let Some((pin_label, pinned)) = pin_source else {
        return;
    };

    let r_constraint = project.manifest.project.r_version.as_deref();
    match find_r_binary(r_constraint) {
        Ok(active_bin) => {
            let Some(active_ver) = query_r_version(&active_bin) else {
                return;
            };
            if r_minor_of(&pinned) != r_minor_of(&active_ver) {
                crate::ui::warn(format!(
                    "R version mismatch: pinned {pinned} via {pin_label}, active R is {active_ver} ({}). Run `uvr r install {pinned}` to align.",
                    active_bin.display()
                ));
                // No need to also check calling-R below — the active-vs-pin
                // warning is the louder signal here.
                return;
            }

            // (3) Calling-session R vs pin. Only meaningful when uvr is
            // invoked from inside R (R_HOME set by the parent process).
            warn_calling_r_mismatch(&pinned, pin_label, &active_ver);
        }
        Err(_) => {
            // find_r_binary errors when the pinned version isn't installed.
            // Tell the user what's wrong instead of letting the command fall
            // through to a generic "R not found" later.
            let installed = find_all()
                .into_iter()
                .map(|i| i.version)
                .collect::<Vec<_>>();
            let installed_msg = if installed.is_empty() {
                "no R installations detected".to_string()
            } else {
                format!("installed: {}", installed.join(", "))
            };
            crate::ui::warn(format!(
                "R {pinned} pinned via {pin_label} but not installed ({installed_msg}). Run `uvr r install {pinned}`."
            ));
        }
    }
}

/// Warn when the calling R session (`R_HOME`) doesn't match the pinned/active
/// R that uvr will install for. Silent if `R_HOME` isn't set (terminal call).
fn warn_calling_r_mismatch(pinned: &str, pin_label: &str, active_ver: &str) {
    let Ok(r_home) = std::env::var("R_HOME") else {
        return;
    };
    let r_name = if cfg!(windows) { "R.exe" } else { "R" };
    let calling_bin = std::path::PathBuf::from(&r_home).join("bin").join(r_name);
    let Some(calling_ver) = uvr_core::r_version::detector::query_r_version(&calling_bin) else {
        return;
    };
    if r_minor_of(&calling_ver) == r_minor_of(active_ver) {
        return;
    }
    crate::ui::warn(format!(
        "Calling R session is {calling_ver} ({r_home}) but uvr will install for R {active_ver} (pinned {pinned} via {pin_label}). \
         Packages built for {active_ver} won't load in this {calling_ver} session — restart your R IDE pointed at \
         the pinned R, or change the pin to match this session."
    ));
}

fn r_minor_of(v: &str) -> String {
    let parts: Vec<&str> = v.splitn(3, '.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        v.to_string()
    }
}

fn looks_like_exact(v: &str) -> bool {
    if v.is_empty() {
        return false;
    }
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.parse::<u32>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r_minor_of_strips_patch() {
        assert_eq!(r_minor_of("4.6.0"), "4.6");
        assert_eq!(r_minor_of("4.5"), "4.5");
        assert_eq!(r_minor_of("3.6.3"), "3.6");
    }

    #[test]
    fn looks_like_exact_accepts_concrete_versions() {
        assert!(looks_like_exact("4.6.0"));
        assert!(looks_like_exact("4.5"));
        assert!(looks_like_exact("3.6.3"));
    }

    #[test]
    fn looks_like_exact_rejects_constraints() {
        assert!(!looks_like_exact(""));
        assert!(!looks_like_exact(">=4.0.0"));
        assert!(!looks_like_exact("*"));
        assert!(!looks_like_exact("~4.5"));
    }

    #[test]
    fn looks_like_exact_rejects_malformed() {
        // Trailing dot — not a real version.
        assert!(!looks_like_exact("4.5."));
        // Bare major — not exact enough to be a pin.
        assert!(!looks_like_exact("4"));
        // All-dots / empty components.
        assert!(!looks_like_exact("..."));
        assert!(!looks_like_exact("4..5"));
        // Letters mid-string.
        assert!(!looks_like_exact("4.5a"));
    }
}
