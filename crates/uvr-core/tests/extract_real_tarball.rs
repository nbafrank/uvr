//! Network-dependent integration test: download a real binary tarball from
//! cran.rpkgs.com and run `install_binary_package` against it. Reproduces
//! the user-reported failure path end-to-end so fixes can be validated
//! against the actual workload rather than synthetic fixtures.
//!
//! Run with: `cargo test --release --test extract_real_tarball -- --ignored --nocapture`

// Pick the right per-arch path so the tarball matches the running test host.
#[cfg(target_arch = "x86_64")]
const RPKGS_BASE: &str = "https://cran.rpkgs.com/amd64/alpine323/latest/src/contrib";
#[cfg(target_arch = "aarch64")]
const RPKGS_BASE: &str = "https://cran.rpkgs.com/arm64/alpine323/latest/src/contrib";

#[cfg(target_arch = "x86_64")]
fn host_ua() -> &'static str {
    "R (4.5.0 x86_64-pc-linux-musl x86_64 linux-musl)"
}
#[cfg(target_arch = "aarch64")]
fn host_ua() -> &'static str {
    "R (4.5.0 aarch64-pc-linux-musl aarch64 linux-musl)"
}

async fn fetch_tarball(name: &str, version: &str) -> Vec<u8> {
    let url = format!("{RPKGS_BASE}/{name}_{version}.tar.gz");
    eprintln!("[fetch] GET {url}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("client");
    let resp = client
        .get(&url)
        .header("User-Agent", host_ua())
        .send()
        .await
        .expect("send");
    assert!(resp.status().is_success(), "fetch {url} -> {}", resp.status());
    resp.bytes().await.expect("bytes").to_vec()
}

async fn run_extract(name: &str, version: &str) {
    let bytes = fetch_tarball(name, version).await;
    eprintln!("[fetch] got {} bytes for {name}_{version}", bytes.len());

    let tarball_file = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tarball_file.path(), &bytes).expect("write tarball");

    let library = tempfile::TempDir::new().expect("library tempdir");
    eprintln!("[extract] library={}", library.path().display());

    match uvr_core::installer::binary_install::install_binary_package(
        tarball_file.path(),
        library.path(),
        name,
        None,
    ) {
        Ok(()) => {
            let extracted = library.path().join(name);
            eprintln!("[extract] OK");
            let description = extracted.join("DESCRIPTION");
            assert!(
                description.exists(),
                "DESCRIPTION should exist at {}",
                description.display()
            );
            let content = std::fs::read_to_string(&description).expect("read DESCRIPTION");
            assert!(
                content.contains("Package:"),
                "DESCRIPTION should have Package: field"
            );
            eprintln!("[extract] DESCRIPTION first line: {}", content.lines().next().unwrap_or(""));
        }
        Err(e) => {
            panic!("install_binary_package failed: {e}");
        }
    }
}

#[tokio::test]
#[ignore]
async fn extract_rlang_from_rpkgs() {
    run_extract("rlang", "1.2.0").await;
}

#[tokio::test]
#[ignore]
async fn extract_jsonlite_from_rpkgs() {
    run_extract("jsonlite", "2.0.0").await;
}
