use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::auth::pkce::PkcePair;
use crate::error::AgySwitchError;

// OAuth credentials: set via environment variables, or use built-in defaults
pub fn client_id() -> &'static str {
    // We use a const fn trick: just return the default
    // Users can override via AGY_CLIENT_ID env var at the callsite
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"
}

pub fn client_secret() -> &'static str {
    "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"
}

pub fn client_id_env() -> String {
    std::env::var("AGY_CLIENT_ID").unwrap_or_else(|_| client_id().into())
}

pub fn client_secret_env() -> String {
    std::env::var("AGY_CLIENT_SECRET").unwrap_or_else(|_| client_secret().into())
}
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

pub async fn build_authorization_url(port: u16, pkce: &PkcePair) -> String {
    let redirect_uri = format!("http://localhost:{}/oauth-callback", port);
    let params = [
        ("client_id", client_id()),
        ("redirect_uri", redirect_uri.as_str()),
        ("response_type", "code"),
        ("scope", SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ];

    let mut url = String::from("https://accounts.google.com/o/oauth2/v2/auth?");
    let parts: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect();
    url.push_str(&parts.join("&"));
    url
}

pub async fn listen_for_callback(port: u16) -> Result<String, AgySwitchError> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .map_err(|_| AgySwitchError::PortBindingFailed)?;

    let (mut stream, _) = listener.accept().await.map_err(AgySwitchError::Io)?;

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.map_err(AgySwitchError::Io)?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Extract code from request line, e.g., GET /oauth-callback?code=... HTTP/1.1
    let code = extract_code_from_request(&request)?;

    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h1>Authorization complete</h1><p>You may close this window.</p></body></html>";
    stream.write_all(response.as_bytes()).await.map_err(AgySwitchError::Io)?;
    stream.flush().await.map_err(AgySwitchError::Io)?;

    Ok(code)
}

fn extract_code_from_request(request: &str) -> Result<String, AgySwitchError> {
    let line = request.lines().next().ok_or_else(|| AgySwitchError::OAuthFailed("Empty request".to_string()))?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(AgySwitchError::OAuthFailed("Invalid HTTP request".to_string()));
    }
    let path = parts[1];
    let query = path.split('?').nth(1).ok_or_else(|| AgySwitchError::OAuthFailed("No query in callback URL".to_string()))?;
    let params: HashMap<String, String> = query
        .split('&')
        .filter_map(|s| {
            let mut kv = s.splitn(2, '=');
            let k = kv.next()?.to_string();
            let v = kv.next().unwrap_or("").to_string();
            Some((k, v))
        })
        .collect();

    params
        .get("code")
        .cloned()
        .ok_or_else(|| AgySwitchError::OAuthFailed("No code in callback".to_string()))
}

pub fn find_available_port() -> Result<u16, AgySwitchError> {
    for port in 51121..=51126 {
        if std::net::TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(AgySwitchError::PortBindingFailed)
}
