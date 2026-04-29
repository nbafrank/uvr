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

/// Phase-1 R-pin sanity check (#63, #64). Best-effort: never errors, never
/// blocks the command. Compares the project's pinned R minor (.r-version
/// preferred; falls back to an exact `[project] r_version` in uvr.toml) to
/// whatever R uvr will actually dispatch to. On mismatch — including the
/// "pin set, pinned version not installed" case — prints a loud WARN so
/// users notice silent fallbacks.
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
            }
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

fn r_minor_of(v: &str) -> String {
    let parts: Vec<&str> = v.splitn(3, '.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        v.to_string()
    }
}

fn looks_like_exact(v: &str) -> bool {
    !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.contains('.')
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
}
