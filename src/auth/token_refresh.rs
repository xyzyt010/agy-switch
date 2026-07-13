use chrono::{Duration, Utc};
use serde::Deserialize;

use crate::auth::oauth_flow::TOKEN_ENDPOINT;
use crate::error::AgySwitchError;
use crate::store::account::OAuthCredential;

#[derive(Deserialize, Debug)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

/// Parse a composite refresh token like `refreshToken||projectId||managedProjectId`
/// Returns (real_refresh_token, project_id, managed_project_id)
pub fn parse_composite_token(composite: &str) -> (String, Option<String>, Option<String>) {
    let parts: Vec<&str> = composite.split("||").collect();
    let real_token = parts[0].to_string();
    let project_id = parts.get(1).map(|s| s.to_string()).filter(|s| !s.is_empty());
    let managed_project_id = parts.get(2).map(|s| s.to_string()).filter(|s| !s.is_empty());
    (real_token, project_id, managed_project_id)
}

/// Build a composite refresh token string for storage
pub fn build_composite_token(refresh_token: &str, project_id: Option<&str>, managed_project_id: Option<&str>) -> String {
    match (project_id, managed_project_id) {
        (Some(p), Some(m)) => format!("{}||{}||{}", refresh_token, p, m),
        (Some(p), None) => format!("{}||{}", refresh_token, p),
        (None, _) => refresh_token.to_string(),
    }
}

/// Refresh the access token using the stored refresh token.
/// Handles composite tokens (refreshToken||projectId||managedProjectId).
/// Returns a new credential with updated access_token, expiry, and possibly refresh_token.
pub async fn refresh_access_token(credential: &OAuthCredential, email: Option<&str>) -> Result<OAuthCredential, AgySwitchError> {
    let client = crate::http::client();

    // Extract the real refresh token (strip embedded project IDs)
    let (real_token, _, _) = parse_composite_token(&credential.refresh_token);

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", real_token.as_str()),
        ("client_id", crate::auth::oauth_flow::client_id()),
        ("client_secret", crate::auth::oauth_flow::client_secret()),
    ];

    let res = client
        .post(TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .await
        .map_err(AgySwitchError::Http)?;

    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(AgySwitchError::TokenRefreshFailed {
            email: email.unwrap_or("unknown").to_string(),
            reason: text,
        });
    }

    let data: TokenResponse = res.json().await.map_err(AgySwitchError::Http)?;

    // Google may rotate the refresh token
    let new_raw_token = data
        .refresh_token
        .unwrap_or_else(|| real_token.clone());

    // Re-compose the composite token with project IDs if we had them
    let new_refresh_token = build_composite_token(
        &new_raw_token,
        credential.project_id.as_deref(),
        credential.managed_project_id.as_deref(),
    );

    let new_credential = OAuthCredential {
        access_token: data.access_token,
        refresh_token: new_refresh_token,
        expiry: Utc::now() + Duration::seconds(data.expires_in),
        project_id: credential.project_id.clone(),
        managed_project_id: credential.managed_project_id.clone(),
    };

    Ok(new_credential)
}

/// Ensure the access token is fresh, with email for error reporting.
pub async fn ensure_fresh_with_email(credential: &OAuthCredential, email: Option<&str>) -> Result<OAuthCredential, AgySwitchError> {
    let five_minutes_from_now = Utc::now() + Duration::minutes(5);
    if credential.expiry <= five_minutes_from_now {
        refresh_access_token(credential, email).await
    } else {
        Ok(credential.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_composite_token() {
        let (token, proj, managed) = parse_composite_token("1//0gEvQ||my-project||my-managed");
        assert_eq!(token, "1//0gEvQ");
        assert_eq!(proj.as_deref(), Some("my-project"));
        assert_eq!(managed.as_deref(), Some("my-managed"));
    }

    #[test]
    fn test_parse_simple_token() {
        let (token, proj, managed) = parse_composite_token("1//0gEvQ");
        assert_eq!(token, "1//0gEvQ");
        assert!(proj.is_none());
        assert!(managed.is_none());
    }

    #[test]
    fn test_parse_two_part_token() {
        let (token, proj, managed) = parse_composite_token("1//0gEvQ||my-project");
        assert_eq!(token, "1//0gEvQ");
        assert_eq!(proj.as_deref(), Some("my-project"));
        assert!(managed.is_none());
    }

    #[test]
    fn test_build_composite_token() {
        let result = build_composite_token("1//0gEvQ", Some("proj1"), Some("managed1"));
        assert_eq!(result, "1//0gEvQ||proj1||managed1");
    }

    #[test]
    fn test_build_simple_token() {
        let result = build_composite_token("1//0gEvQ", None, None);
        assert_eq!(result, "1//0gEvQ");
    }
}
