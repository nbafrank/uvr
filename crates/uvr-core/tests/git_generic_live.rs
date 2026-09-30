//! Network-gated test: resolve a real R package from a non-GitHub host
//! with `git` (#190).
//!
//! Skipped by default. Run with:
//!     cargo test -p uvr-core --test git_generic_live -- --ignored

use uvr_core::registry::git_generic::{fetch_commit_sha, resolve_git_package_at_commit_bound};

#[tokio::test]
#[ignore = "requires network access to codeberg.org and a git program"]
async fn resolve_public_codeberg_package() {
    let url = "https://codeberg.org/Bisaloo/authoritative.git";
    let commit = fetch_commit_sha(url, "v0.2.0").await.expect("resolve tag");
    assert_eq!(commit, "ce42ce668ee33b38144ab44d72013bce8f7b7fc7");

    let cache = tempfile::tempdir().unwrap();
    let (info, _, _) = resolve_git_package_at_commit_bound(cache.path(), url, &commit, true)
        .await
        .expect("fetch and read DESCRIPTION");
    assert_eq!(info.name, "authoritative");
    assert_eq!(info.version.to_string(), "0.2.0");
    assert_eq!(info.checksum, Some(format!("git:{commit}")));
    assert!(info.requires.iter().any(|d| d.name == "stringi"));
}
