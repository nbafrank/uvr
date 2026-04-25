use std::path::Path;

use anyhow::{Context, Result};

use uvr_core::manifest::Manifest;
use uvr_core::project::{
    DESCRIPTION_FILE, DOT_UVR_DIR, LIBRARY_DIR, MANIFEST_FILE, R_VERSION_FILE,
};
use uvr_core::r_version::detector::find_r_binary;

use crate::ui;
use crate::ui::palette;

pub fn run(name: Option<String>, r_version: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;

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
  if (dir.exists(lib)) {
    .libPaths(lib)
    lock <- file.path(getwd(), "uvr.lock")
    if (file.exists(lock)) {
      lock_lines <- readLines(lock, warn = FALSE)
      n_locked <- length(grep("^\\[\\[package\\]\\]", lock_lines))
      installed <- list.dirs(lib, recursive = FALSE, full.names = FALSE)
      n_installed <- length(setdiff(installed, "uvr"))
      if (n_locked > 0 && n_installed < n_locked) {
        message("uvr: ", n_locked - n_installed, " of ", n_locked,
                " package(s) not installed. Run uvr::sync() to install.")
      } else if (n_locked > 0) {
        message("uvr: library linked (", n_installed, " packages)")
      } else {
        message("uvr: library active, but uvr.lock is empty. Run uvr::lock() to populate it.")
      }
    } else {
      message("uvr: library active, but no uvr.lock found. Run uvr::lock() to create one.")
    }
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

    let r_binary_str = r_binary.to_string_lossy();
    let vscode_dir = dir.join(".vscode");
    std::fs::create_dir_all(&vscode_dir)?;
    let settings_path = vscode_dir.join("settings.json");

    let key = "positron.r.interpreters.default";

    if settings_path.exists() {
        let existing = std::fs::read_to_string(&settings_path)?;
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&existing) {
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    key.to_string(),
                    serde_json::Value::String(r_binary_str.into_owned()),
                );
                let pretty = serde_json::to_string_pretty(&json).unwrap_or(existing);
                return std::fs::write(&settings_path, pretty + "\n");
            }
        }
        // If we can't parse existing JSON, don't clobber it
        return Ok(());
    }

    let content = serde_json::json!({ key: r_binary_str });
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
    let entries = "^uvr\\.toml$\n^uvr\\.lock$\n^\\.uvr$\n";

    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if existing.contains("uvr\\.toml") {
            return Ok(());
        }
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(entries);
        std::fs::write(&path, content)
    } else {
        std::fs::write(&path, entries)
    }
}

fn write_gitignore(dir: &Path) -> std::io::Result<()> {
    let path = dir.join(".gitignore");
    let uvr_entry = format!("/{DOT_UVR_DIR}/{LIBRARY_DIR}/\n");

    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if existing.contains(&uvr_entry.trim_end().to_string()) {
            return Ok(());
        }
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&uvr_entry);
        std::fs::write(&path, content)
    } else {
        std::fs::write(&path, uvr_entry)
    }
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
