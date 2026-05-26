//! Network-gated test: resolve a real public Forgejo repo end-to-end.
//!
//! Skipped by default. Run with:
//!     cargo test -p uvr-core --test forgejo_live -- --ignored

use uvr_core::registry::forgejo::resolve_forgejo_package;

#[tokio::test]
#[ignore = "requires network access to codeberg.org"]
async fn resolve_public_forgejo_repo() {
    let client = reqwest::Client::builder()
        .user_agent("uvr-test")
        .build()
        .expect("build client");

    // codeberg.org is a public Forgejo instance. The owner/repo below
    // are placeholders that should be replaced with a real public
    // Forgejo-hosted R package before merging this task. The test is
    // exercising the API surface (commit lookup, DESCRIPTION fetch,
    // archive URL construction), not this specific repo.
    let info = resolve_forgejo_package(
        &client,
        "codeberg.org",
        "Codeberg", // owner — placeholder; replace with a real org/user hosting an R pkg
        "Documentation", // repo  — placeholder; replace likewise
        "main",
    )
    .await
    .expect("resolve");

    assert!(!info.name.is_empty(), "package name from DESCRIPTION");
    assert!(
        info.url.starts_with("https://codeberg.org/api/v1/repos/"),
        "archive URL pinned to /api/v1/: {}",
        info.url
    );
    assert!(info.url.ends_with(".tar.gz"), "archive URL ends in .tar.gz");
}
