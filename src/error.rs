use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum AgySwitchError {
    #[error("Account not found: {0}")]
    AccountNotFound(String),

    #[error("OAuth flow failed: {0}")]
    OAuthFailed(String),

    #[error("Token refresh failed for {email}: {reason}")]
    TokenRefreshFailed { email: String, reason: String },

    #[error("Rate limited (HTTP 429) for {endpoint}")]
    RateLimited {
        endpoint: String,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    },

    #[error("File I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Daemon not running")]
    DaemonNotRunning,

    #[error("Daemon already running")]
    DaemonAlreadyRunning,

    #[error("Port binding failed after trying range 51121-51126")]
    PortBindingFailed,

    #[error("Account already exists: {0}. Use update to replace credential.")]
    DuplicateAccount(String),
}
