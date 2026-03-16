use anyhow::{Context, Result};
use console::style;

pub fn run_clean() -> Result<()> {
    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".uvr")
        .join("cache");

    if !cache_dir.exists() {
        println!("{} Cache is already empty", style("✓").green().bold());
        return Ok(());
    }

    let mut count = 0u64;
    let mut bytes = 0u64;

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

    let mb = bytes as f64 / 1_048_576.0;
    println!(
        "{} Cleared {} file(s) ({:.1} MB) from cache",
        style("✓").green().bold(),
        count,
        mb,
    );
    Ok(())
}
