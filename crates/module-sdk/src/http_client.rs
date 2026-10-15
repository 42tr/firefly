use std::{sync::OnceLock, time::Duration};

use reqwest::Client;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

/// Get a reference to the shared HTTP client.
///
/// `reqwest::Client` internally manages a connection pool, so reusing the same
/// client instance is more efficient and avoids potential memory leaks from
/// creating new clients repeatedly.
///
/// # Example
/// ```
/// use firefly_module_sdk::http_client::client;
///
/// async fn fetch_data() -> Result<String, reqwest::Error> {
///     client().get("https://example.com").send().await?.text().await
/// }
/// ```
pub fn client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("Failed to create HTTP client")
    })
}
