use anyhow::{Context, Result};
use console::style;

use uvr_core::installer::package_cache;

pub fn run_clean() -> Result<()> {
    let base = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".uvr");
    let cache_dir = base.join("cache");

    let mut count = 0u64;
    let mut bytes = 0u64;

    // Clean tarball download cache
    if cache_dir.exists() {
        for entry in std::fs::read_dir(&cache_dir)
            .with_context(|| format!("Cannot read cache dir {}", cache_dir.display()))?
            .flatten()
        {
            if let Ok(meta) = entry.metadata() {
                bytes += meta.len();
                count += 1;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }

    // Clean global package cache
    let packages_dir = package_cache::global_packages_dir();
    if packages_dir.exists() {
        let (pkg_count, pkg_bytes) = package_cache::cache_stats();
        count += pkg_count;
        bytes += pkg_bytes;
        let _ = std::fs::remove_dir_all(&packages_dir);
    }

    if count == 0 {
        println!("{} Cache is already empty", style("✓").green().bold());
    } else {
        let mb = bytes as f64 / 1_048_576.0;
        println!(
            "{} Cleared {} item(s) ({:.1} MB) from cache",
            style("✓").green().bold(),
            count,
            mb,
        );
    }
    Ok(())
}
