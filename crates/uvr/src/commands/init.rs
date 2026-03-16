use std::path::Path;

use anyhow::{Context, Result};
use console::style;

use uvr_core::manifest::Manifest;
use uvr_core::project::{DOT_UVR_DIR, LIBRARY_DIR, MANIFEST_FILE};

pub fn run(name: Option<String>, r_version: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;

    // Use provided name or directory name
    let project_name = name.unwrap_or_else(|| {
        cwd.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "my-project".to_string())
    });

    let manifest_path = cwd.join(MANIFEST_FILE);
    if manifest_path.exists() {
        anyhow::bail!(
            "{} already exists. Remove it first if you want to re-initialize.",
            MANIFEST_FILE
        );
    }

    let manifest = Manifest::new(project_name.clone(), r_version);
    manifest
        .write(&manifest_path)
        .context("Failed to write uvr.toml")?;

    // Create .uvr/library/
    let library_path = cwd.join(DOT_UVR_DIR).join(LIBRARY_DIR);
    std::fs::create_dir_all(&library_path).context("Failed to create .uvr/library/")?;

    // Write .gitignore
    write_gitignore(&cwd).context("Failed to write .gitignore")?;

    println!(
        "{} Initialized project {}",
        style("✓").green().bold(),
        style(&project_name).cyan()
    );
    println!("  {}", style(MANIFEST_FILE).dim());
    println!(
        "  {}/{}/",
        style(DOT_UVR_DIR).dim(),
        style(LIBRARY_DIR).dim()
    );

    Ok(())
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
