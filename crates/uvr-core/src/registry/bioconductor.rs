use std::collections::HashMap;
use std::io::Read;

use flate2::read::GzDecoder;
use tracing::info;

use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::registry::PackageInfo;
use crate::registry::cran::{parse_dcf_block, CranPackageEntry};
use crate::resolver::PackageRegistry;

fn bioc_release_for_r(r_major: u64, r_minor: u64) -> &'static str {
    match (r_major, r_minor) {
        (4, 4) => "3.20",
        (4, 3) => "3.18",
        (4, 2) => "3.16",
        (4, 1) => "3.14",
        (4, 0) => "3.12",
        _ => "3.20",
    }
}

pub struct BiocRegistry {
    packages: HashMap<String, CranPackageEntry>,
    bioc_release: String,
}

impl BiocRegistry {
    pub async fn fetch(client: &reqwest::Client, r_version: &str) -> Result<Self> {
        let parts: Vec<&str> = r_version.split('.').collect();
        let major: u64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(4);
        let minor: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
        let bioc_release = bioc_release_for_r(major, minor).to_string();

        let url = format!(
            "https://bioconductor.org/packages/{bioc_release}/bioc/src/contrib/PACKAGES.gz"
        );
        info!("Downloading Bioconductor {bioc_release} PACKAGES.gz...");
        let bytes = client.get(&url).send().await?.bytes().await?;
        let mut gz = GzDecoder::new(bytes.as_ref());
        let mut text = String::new();
        gz.read_to_string(&mut text)?;

        let mut packages = HashMap::new();
        for block in text.split("\n\n") {
            let block = block.trim();
            if block.is_empty() {
                continue;
            }
            if let Some(entry) = parse_dcf_block(block) {
                packages.insert(entry.name.clone(), entry);
            }
        }

        info!("Bioconductor {bioc_release}: {} packages", packages.len());
        Ok(BiocRegistry { packages, bioc_release })
    }
}

impl PackageRegistry for BiocRegistry {
    fn resolve_package(&self, name: &str, constraint: Option<&str>) -> Result<PackageInfo> {
        let entry = self
            .packages
            .get(name)
            .ok_or_else(|| UvrError::PackageNotFound(name.to_string()))?;

        // Validate constraint if provided
        if let Some(c) = constraint {
            if c != "*" && !c.is_empty() {
                let req = crate::resolver::parse_version_req(c)?;
                if !req.matches(&entry.version) {
                    return Err(UvrError::NoMatchingVersion {
                        package: name.to_string(),
                        constraint: c.to_string(),
                    });
                }
            }
        }

        let url = format!(
            "https://bioconductor.org/packages/{}/bioc/src/contrib/{}_{}.tar.gz",
            self.bioc_release, entry.name, entry.raw_version
        );

        Ok(PackageInfo {
            name: entry.name.clone(),
            version: entry.version.clone(),
            source: PackageSource::Bioconductor,
            checksum: if entry.md5sum.is_empty() {
                None
            } else {
                Some(format!("md5:{}", entry.md5sum))
            },
            requires: entry.requires_as_deps(),
            url,
        })
    }
}
