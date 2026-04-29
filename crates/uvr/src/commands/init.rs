use std::path::Path;

use anyhow::{Context, Result};

use uvr_core::manifest::Manifest;
use uvr_core::project::{
    DESCRIPTION_FILE, DOT_UVR_DIR, LIBRARY_DIR, MANIFEST_FILE, R_VERSION_FILE,
};
use uvr_core::r_version::detector::find_r_binary;

use crate::ui;
use crate::ui::palette;

pub fn run(name: Option<String>, here: bool, r_version: Option<String>) -> Result<()> {
    let starting_cwd = std::env::current_dir().context("Cannot determine current directory")?;

    // #56 — `uvr init <name>` creates a new directory `<name>/` and
    // initializes inside it; `uvr init --here [<name>]` keeps the project
    // in the current directory; `uvr init` alone uses the current directory
    // and derives the name from its basename.
    let create_subdir = name.is_some() && !here;
    let cwd = if create_subdir {
        let dir_name = name.as_deref().expect("checked above");
        let new_dir = starting_cwd.join(dir_name);
        if new_dir.exists() {
            anyhow::bail!(
                "Cannot create directory {} — it already exists. Use `uvr init --here` to initialize in the current directory.",
                new_dir.display()
            );
        }
        std::fs::create_dir_all(&new_dir)
            .with_context(|| format!("Failed to create directory {}", new_dir.display()))?;
        new_dir
    } else {
        starting_cwd
    };

    let manifest_path = cwd.join(MANIFEST_FILE);
    if manifest_path.exists() {
        anyhow::bail!(
            "{} already exists. Remove it first if you want to re-initialize.",
            MANIFEST_FILE
        );
    }

    // If DESCRIPTION exists, import name/r_version/deps from it; CLI args override.
    let description_path = cwd.join(DESCRIPTION_FILE);
    let mut manifest = if description_path.exists() {
        Manifest::from_description_file(&description_path).context("Failed to parse DESCRIPTION")?
    } else {
        Manifest::new(String::new(), None)
    };

    // Apply CLI args (explicit --name / --r-version override DESCRIPTION values)
    if let Some(n) = name {
        manifest.project.name = n;
    } else if manifest.project.name.is_empty() {
        manifest.project.name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "my-project".to_string());
    }
    if let Some(rv) = r_version {
        manifest.project.r_version = Some(rv);
    }
    let project_name = manifest.project.name.clone();

    let imported_count = manifest.dependencies.len() + manifest.dev_dependencies.len();
    manifest
        .write(&manifest_path)
        .context("Failed to write uvr.toml")?;

    // Create .uvr/library/
    let library_path = cwd.join(DOT_UVR_DIR).join(LIBRARY_DIR);
    std::fs::create_dir_all(&library_path).context("Failed to create .uvr/library/")?;

    // Write .gitignore
    write_gitignore(&cwd).context("Failed to write .gitignore")?;

    // Add uvr files to .Rbuildignore only for actual R package source trees
    // (DESCRIPTION with a `Package:` field). Non-package projects may still
    // carry a DESCRIPTION for dependency tracking.
    if is_r_package_dir(&cwd) {
        write_rbuildignore(&cwd).context("Failed to write .Rbuildignore")?;
    }

    // Write .Rprofile so RStudio sees the uvr library
    ensure_rprofile(&cwd).context("Failed to write .Rprofile")?;

    // Write .vscode/settings.json for Positron R interpreter
    ensure_positron_settings(&cwd).context("Failed to write Positron settings")?;

    // Install the uvr R companion package if R is available
    if let Ok(r_binary) = find_r_binary(manifest.project.r_version.as_deref()) {
        if let Some(r_ver) = uvr_core::r_version::detector::query_r_version(&r_binary) {
            crate::commands::sync::ensure_companion_package(&library_path, &r_ver, &r_binary);
        }
    }

    ui::success(format!(
        "Initialized project {}",
        palette::pkg(&project_name)
    ));
    if description_path.exists() && imported_count > 0 {
        ui::bullet_dim(format!(
            "Imported {} dependenc{} from DESCRIPTION",
            imported_count,
            if imported_count == 1 { "y" } else { "ies" }
        ));
    }
    println!("  {}", palette::dim(MANIFEST_FILE));
    println!(
        "  {}",
        palette::dim(format!("{DOT_UVR_DIR}/{LIBRARY_DIR}/"))
    );

    Ok(())
}

const RPROFILE_START: &str = "# >>> uvr >>>";
const RPROFILE_END: &str = "# <<< uvr <<<";
const RPROFILE_SNIPPET: &str = r#"# >>> uvr >>>
local({
  lib <- file.path(getwd(), ".uvr", "library")
  lock <- file.path(getwd(), "uvr.lock")
  count_locked <- function(path) {
    if (!file.exists(path)) return(0L)
    length(grep("^\\[\\[package\\]\\]", readLines(path, warn = FALSE)))
  }
  if (dir.exists(lib)) {
    .libPaths(lib)
    n_locked <- count_locked(lock)
    installed <- list.dirs(lib, recursive = FALSE, full.names = FALSE)
    n_installed <- length(setdiff(installed, "uvr"))
    if (n_locked > 0 && n_installed < n_locked) {
      message("uvr: ", n_locked - n_installed, " of ", n_locked,
              " package(s) not installed. Run uvr::sync() to install.")
    } else if (n_locked > 0) {
      message("uvr: library linked (", n_installed, " packages)")
    } else if (file.exists(lock)) {
      message("uvr: library active, but uvr.lock is empty. Run uvr::lock() to populate it.")
    } else {
      message("uvr: library active, but no uvr.lock found. Run uvr::lock() to create one.")
    }
  } else if (file.exists(lock)) {
    # #59: .uvr/library/ doesn't exist yet but the lockfile does — fresh
    # checkout, never synced. Tell the user.
    n_locked <- count_locked(lock)
    message("uvr: 0 of ", n_locked, " package(s) installed. Run uvr::sync() to install.")
  }
})
# <<< uvr <<<
"#;

pub fn ensure_rprofile(dir: &Path) -> std::io::Result<()> {
    let path = dir.join(".Rprofile");

    if !path.exists() {
        return std::fs::write(&path, RPROFILE_SNIPPET);
    }

    let existing = std::fs::read_to_string(&path)?;
    if let Some(updated) = refresh_uvr_block(&existing, RPROFILE_SNIPPET) {
        if updated != existing {
            std::fs::write(&path, updated)?;
        }
        return Ok(());
    }

    // No uvr block yet — append one.
    let mut content = existing;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(RPROFILE_SNIPPET);
    std::fs::write(&path, content)
}

/// If `existing` already contains a uvr-managed block (delimited by the start/end markers),
/// return a new string with that block replaced by `snippet`. Returns `None` when no block
/// is found so the caller can decide to append.
fn refresh_uvr_block(existing: &str, snippet: &str) -> Option<String> {
    let start = existing.find(RPROFILE_START)?;
    let rest = &existing[start..];
    let end_rel = rest.find(RPROFILE_END)?;
    let end = start + end_rel + RPROFILE_END.len();
    let mut out = String::with_capacity(existing.len() + snippet.len());
    out.push_str(&existing[..start]);
    out.push_str(snippet.trim_end_matches('\n'));
    out.push_str(&existing[end..]);
    Some(out)
}

/// Write `.vscode/settings.json` with Positron R interpreter path if a pinned
/// R version is managed by uvr.
///
/// Positron's `interpreters.default` setting only selects from interpreters
/// it has already discovered — and uvr's R install at
/// `~/.uvr/r-versions/<ver>/` is not in any of Positron's standard discovery
/// paths (R.framework, conda, pixi, /usr/local/bin). So we also write
/// `positron.r.customBinaries` to inject the uvr R into discovery; `default`
/// then picks it as the primary. Without `customBinaries`, `default` was
/// silently a no-op.
pub fn ensure_positron_settings(dir: &Path) -> std::io::Result<()> {
    // Determine the pinned R version from .r-version
    let r_version_path = dir.join(R_VERSION_FILE);
    let version = match std::fs::read_to_string(&r_version_path) {
        Ok(v) => {
            let v = v.trim().to_string();
            if v.is_empty() {
                return Ok(());
            }
            v
        }
        Err(_) => return Ok(()), // No .r-version, nothing to do
    };

    // Check the R binary actually exists
    let r_home = dirs::home_dir().unwrap_or_default();
    let r_binary = r_home
        .join(".uvr")
        .join("r-versions")
        .join(&version)
        .join("bin")
        .join("R");
    if !r_binary.exists() {
        return Ok(()); // Not a uvr-managed R version
    }

    let r_binary_str = r_binary.to_string_lossy().into_owned();
    let vscode_dir = dir.join(".vscode");
    std::fs::create_dir_all(&vscode_dir)?;
    let settings_path = vscode_dir.join("settings.json");

    let default_key = "positron.r.interpreters.default";
    let custom_binaries_key = "positron.r.customBinaries";

    if settings_path.exists() {
        let existing = std::fs::read_to_string(&settings_path)?;
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&existing) {
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    default_key.to_string(),
                    serde_json::Value::String(r_binary_str.clone()),
                );

                // customBinaries is an array of paths; merge our path in if
                // not already present, preserving any user-added paths.
                let entry = obj
                    .entry(custom_binaries_key.to_string())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let Some(arr) = entry.as_array_mut() {
                    let already = arr
                        .iter()
                        .any(|v| v.as_str() == Some(r_binary_str.as_str()));
                    if !already {
                        arr.push(serde_json::Value::String(r_binary_str.clone()));
                    }
                } else {
                    // Existing value isn't an array — replace with one
                    // containing just our path. Don't try to coerce; the
                    // user can always add their own entries back manually.
                    *entry = serde_json::json!([r_binary_str.clone()]);
                }

                let pretty = serde_json::to_string_pretty(&json).unwrap_or(existing);
                return std::fs::write(&settings_path, pretty + "\n");
            }
        }
        // If we can't parse existing JSON, don't clobber it
        return Ok(());
    }

    let content = serde_json::json!({
        default_key: r_binary_str,
        custom_binaries_key: [r_binary_str],
    });
    let pretty = serde_json::to_string_pretty(&content).unwrap();
    std::fs::write(&settings_path, pretty + "\n")
}

/// Returns true if `dir` looks like an R package source tree — DESCRIPTION
/// exists and contains a `Package:` field. Non-package projects may still
/// carry a DESCRIPTION (for dependency tracking) but shouldn't get a
/// `.Rbuildignore`, since they aren't built with `R CMD build`.
pub fn is_r_package_dir(dir: &Path) -> bool {
    let desc = dir.join("DESCRIPTION");
    let Ok(contents) = std::fs::read_to_string(&desc) else {
        return false;
    };
    contents
        .lines()
        .any(|line| line.starts_with("Package:") || line.starts_with("Package :"))
}

pub fn write_rbuildignore(dir: &Path) -> std::io::Result<()> {
    let path = dir.join(".Rbuildignore");
    let wanted = ["^uvr\\.toml$", "^uvr\\.lock$", "^\\.uvr$"];

    if !path.exists() {
        let body = wanted.iter().map(|s| format!("{s}\n")).collect::<String>();
        return std::fs::write(&path, body);
    }

    // Append only lines that aren't already there. Issue #65 — don't duplicate.
    let existing = std::fs::read_to_string(&path)?;
    let missing: Vec<&str> = wanted
        .iter()
        .copied()
        .filter(|w| !line_already_present(&existing, w))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut content = existing;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    for line in missing {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(&path, content)
}

fn write_gitignore(dir: &Path) -> std::io::Result<()> {
    let path = dir.join(".gitignore");
    let uvr_entry = format!("/{DOT_UVR_DIR}/{LIBRARY_DIR}/");

    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if line_already_present(&existing, &uvr_entry) {
            return Ok(());
        }
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&uvr_entry);
        content.push('\n');
        std::fs::write(&path, content)
    } else {
        std::fs::write(&path, format!("{uvr_entry}\n"))
    }
}

/// True if `entry` is already present in `existing` as a non-comment line,
/// ignoring leading slashes (so `/foo` and `foo` count as the same entry —
/// gitignore treats them differently semantically, but for dedup purposes the
/// user almost certainly meant the same thing). Issue #65.
fn line_already_present(existing: &str, entry: &str) -> bool {
    let needle = entry.trim().trim_start_matches('/');
    existing.lines().any(|l| {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            return false;
        }
        l.trim_start_matches('/') == needle
    })
}

#[cfg(test)]
mod rprofile_tests {
    use super::*;

    #[test]
    fn refresh_replaces_outdated_block() {
        let old = "# prelude\n\n# >>> uvr >>>\nlocal({ old_body })\n# <<< uvr <<<\n";
        let new = refresh_uvr_block(old, RPROFILE_SNIPPET).expect("block should be found");
        assert!(new.starts_with("# prelude\n\n# >>> uvr >>>\n"));
        assert!(new.contains("library linked"));
        assert!(!new.contains("old_body"));
    }

    #[test]
    fn refresh_preserves_surrounding_content() {
        let existing = "options(foo = 1)\n# >>> uvr >>>\nold\n# <<< uvr <<<\noptions(bar = 2)\n";
        let new = refresh_uvr_block(existing, RPROFILE_SNIPPET).unwrap();
        assert!(new.starts_with("options(foo = 1)\n"));
        assert!(new.ends_with("options(bar = 2)\n"));
    }

    #[test]
    fn refresh_returns_none_without_markers() {
        assert!(refresh_uvr_block("options(foo = 1)\n", RPROFILE_SNIPPET).is_none());
    }

    #[test]
    fn refresh_is_idempotent() {
        let existing = RPROFILE_SNIPPET.to_string();
        let new = refresh_uvr_block(&existing, RPROFILE_SNIPPET).unwrap();
        assert_eq!(new, existing);
    }

    #[test]
    fn is_r_package_dir_true_with_package_field() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("DESCRIPTION"),
            "Package: foo\nVersion: 0.1.0\n",
        )
        .unwrap();
        assert!(is_r_package_dir(tmp.path()));
    }

    #[test]
    fn is_r_package_dir_false_without_package_field() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("DESCRIPTION"),
            "Depends: R (>= 4.1)\nImports: dplyr\n",
        )
        .unwrap();
        assert!(!is_r_package_dir(tmp.path()));
    }

    #[test]
    fn is_r_package_dir_false_without_description() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_r_package_dir(tmp.path()));
    }
}
