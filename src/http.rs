use std::sync::OnceLock;
use reqwest::{Client, ClientBuilder};
use std::time::Duration;

/// Global shared HTTP client — one connection pool for the entire process.
/// Connection pooling eliminates the 150MB+ leak from creating reqwest::Client::new() per request.
pub fn client() -> &'static Client {
    static INSTANCE: OnceLock<Client> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        ClientBuilder::new()
            .pool_max_idle_per_host(4)
            .pool_idle_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .expect("failed to build HTTP client")
    })
}
