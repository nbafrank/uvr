//! Sending HTTP requests.

use std::future::Future;
use std::time::Duration;

/// How many times [`retry_connect`] sends a request in all.
const ATTEMPTS: u32 = 3;

/// The wait before the first repeat; it doubles before each next one.
#[cfg(not(test))]
const FIRST_DELAY: Duration = Duration::from_secs(1);
#[cfg(test)]
const FIRST_DELAY: Duration = Duration::from_millis(10);

/// Run `send` again while it fails to connect (#332). Any other result,
/// an HTTP error status included, returns at once.
///
/// A connect failure means that no request reached the server, so a repeat
/// is safe. The nightly distro suite sees a CRAN connect timeout on some
/// nights, on a different distro each time, and a single repeat is usually
/// enough. A request that started and then failed is not repeated: the
/// client's total timeout applies to each attempt, so a repeat could
/// multiply a long wait.
pub(crate) async fn retry_connect<F, Fut>(mut send: F) -> reqwest::Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut delay = FIRST_DELAY;
    for attempt in 1..ATTEMPTS {
        match send().await {
            Err(e) if e.is_connect() => {
                tracing::debug!("connect failed (attempt {attempt} of {ATTEMPTS}): {e}");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            result => return result,
        }
    }
    send().await
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::retry_connect;
    use crate::auth::{test_response, test_server};
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn a_refused_connection_is_tried_three_times() {
        // A port that was just free: nothing listens there.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = reqwest::Client::new();
        let tries = AtomicU32::new(0);
        let err = retry_connect(|| {
            tries.fetch_add(1, Ordering::SeqCst);
            client.get(format!("http://127.0.0.1:{port}/")).send()
        })
        .await
        .unwrap_err();
        assert!(err.is_connect(), "{err}");
        assert_eq!(tries.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn an_http_error_status_is_not_repeated() {
        let (url, seen) = test_server(|_| test_response("503 Service Unavailable", "", b""));
        let client = reqwest::Client::new();
        let resp = retry_connect(|| client.get(&url).send()).await.unwrap();
        assert_eq!(resp.status(), 503);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
