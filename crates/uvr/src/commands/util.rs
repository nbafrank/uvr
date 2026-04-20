use anyhow::{Context, Result};
use indicatif::ProgressBar;

pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("uvr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("Failed to build HTTP client")
}

/// Re-export from the ui module so existing call sites keep compiling.
pub fn make_spinner(msg: &str) -> ProgressBar {
    crate::ui::make_spinner(msg)
}
