//! Network-gated test: fetch a real CRAN release history from crandb (#193).
//!
//! Skipped by default. Run with:
//!     cargo test -p uvr-core --test cran_history_live -- --ignored

use semver::Version;
use uvr_core::registry::cran_history::{fetch, CRANDB_URL};

#[tokio::test]
#[ignore = "requires network access to crandb.r-pkg.org"]
async fn fetch_glue_release_history() {
    let client = reqwest::Client::builder()
        .user_agent("uvr-test")
        .build()
        .expect("build client");
    let cache = tempfile::tempdir().expect("tempdir");

    let current = Version::parse("1.8.0").unwrap();
    let entries = fetch(&client, CRANDB_URL, cache.path(), "glue", &current)
        .await
        .expect("fetch");

    let old = entries
        .iter()
        .find(|e| e.raw_version == "1.6.0")
        .expect("glue 1.6.0 in the history");
    // The MD5 of the tarball CRAN keeps under Archive/glue/.
    assert_eq!(old.md5sum, "3f6c5e93eb3ece64c93d038e618b3e08");
    assert!(entries.iter().any(|e| e.raw_version == "1.0.0"));
    assert!(cache.path().join("glue.json").exists(), "history is cached");
}
