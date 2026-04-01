use anyhow::{Context, Result};
use console::style;
use sha2::{Digest, Sha256};

pub async fn run() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!(
        "{} Checking for updates (current: v{current})...",
        style("→").blue().bold()
    );

    let client = crate::commands::lock::build_client()?;
    let release = fetch_latest_release(&client).await?;

    let latest = release.tag_name.trim_start_matches('v');
    if !is_newer(latest, current) {
        println!(
            "{} Already up to date (v{current})",
            style("✓").green().bold()
        );
        return Ok(());
    }

    println!("  {} v{current} → v{latest}", style("→").blue(),);

    let target = detect_target();
    let ext = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };
    let asset_name = format!("uvr-{target}.{ext}");

    let asset_url = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .map(|a| a.browser_download_url.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No release asset found for {asset_name}. \
                 Download manually from: https://github.com/nbafrank/uvr/releases/latest"
            )
        })?;

    // Fetch the checksum file if available
    let expected_checksum =
        if let Some(checksums_asset) = release.assets.iter().find(|a| a.name == "sha256sums.txt") {
            let checksums_text = client
                .get(&checksums_asset.browser_download_url)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            parse_checksum(&checksums_text, &asset_name)
        } else {
            None
        };

    println!("  Downloading {asset_name}...");
    let bytes = client
        .get(&asset_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    // Verify checksum
    if let Some(expected) = &expected_checksum {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex::encode(hasher.finalize());
        if actual != *expected {
            anyhow::bail!(
                "Checksum mismatch for {asset_name}!\n  Expected: {expected}\n  Got:      {actual}"
            );
        }
        println!("  {} SHA256 checksum verified", style("✓").green());
    }

    let current_exe =
        std::env::current_exe().context("Cannot determine current executable path")?;
    let bin_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine binary directory"))?;

    let bin_name = if cfg!(target_os = "windows") {
        "uvr.exe"
    } else {
        "uvr"
    };

    let new_binary = extract_binary(&bytes, bin_name, ext)?;

    // Replace the current binary atomically:
    // 1. Write new binary to a temp file next to the current one
    // 2. Rename old → old.bak, new → current
    let tmp_path = bin_dir.join(format!("{bin_name}.new"));
    std::fs::write(&tmp_path, &new_binary).context("Failed to write new binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let backup_path = bin_dir.join(format!("{bin_name}.bak"));
    // Remove old backup if exists
    let _ = std::fs::remove_file(&backup_path);

    // On Windows, a running binary can't be deleted but CAN be renamed.
    // Move current → backup, then move new → current.
    std::fs::rename(&current_exe, &backup_path).context("Failed to back up current binary")?;
    if let Err(e) = std::fs::rename(&tmp_path, &current_exe) {
        // Restore from backup on failure
        let _ = std::fs::rename(&backup_path, &current_exe);
        return Err(e).context("Failed to replace binary");
    }

    // On Unix, clean up backup immediately. On Windows, leave it — the old
    // binary is still locked by this running process and will be cleaned up
    // on the next self-update invocation (see "Remove old backup" above).
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::fs::remove_file(&backup_path);
    }

    println!("{} Updated to v{latest}", style("✓").green().bold());
    Ok(())
}

fn detect_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

fn is_newer(latest: &str, current: &str) -> bool {
    let latest = semver::Version::parse(latest).ok();
    let current = semver::Version::parse(current).ok();
    match (latest, current) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn extract_binary(archive_bytes: &[u8], bin_name: &str, ext: &str) -> Result<Vec<u8>> {
    if ext == "zip" {
        extract_from_zip(archive_bytes, bin_name)
    } else {
        extract_from_tar_gz(archive_bytes, bin_name)
    }
}

fn extract_from_tar_gz(bytes: &[u8], bin_name: &str) -> Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let gz = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().and_then(|n| n.to_str()) == Some(bin_name) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    anyhow::bail!("Binary '{bin_name}' not found in archive")
}

fn extract_from_zip(bytes: &[u8], bin_name: &str) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read};

    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).context("Failed to read zip archive")?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.mangled_name();
        if name.file_name().and_then(|n| n.to_str()) == Some(bin_name) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    anyhow::bail!("Binary '{bin_name}' not found in zip archive")
}

async fn fetch_latest_release(client: &reqwest::Client) -> Result<GitHubRelease> {
    let url = "https://api.github.com/repos/nbafrank/uvr/releases/latest";
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()
        .context("Failed to fetch latest release from GitHub")?;
    resp.json().await.context("Failed to parse release JSON")
}

fn parse_checksum(checksums_text: &str, asset_name: &str) -> Option<String> {
    for line in checksums_text.lines() {
        // Format: "<hash>  <filename>" or "<hash> <filename>"
        let mut parts = line.split_whitespace();
        if let (Some(hash), Some(name)) = (parts.next(), parts.next()) {
            if name == asset_name {
                return Some(hash.to_lowercase());
            }
        }
    }
    None
}

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        // Pre-release: 0.2.0-rc.1 is NOT newer than 0.2.0 (semver rules)
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
        // But 0.2.0 IS newer than 0.2.0-rc.1
        assert!(is_newer("0.2.0", "0.2.0-rc.1"));
    }

    #[test]
    fn test_parse_checksum() {
        let text = "abc123  uvr-x86_64-unknown-linux-gnu.tar.gz\ndef456  uvr-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(
            parse_checksum(text, "uvr-aarch64-apple-darwin.tar.gz"),
            Some("def456".to_string())
        );
        assert_eq!(parse_checksum(text, "uvr-nonexistent.tar.gz"), None);
    }
}
