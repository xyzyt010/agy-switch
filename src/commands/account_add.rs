use chrono::Utc;
use std::path::PathBuf;
use uuid::Uuid;

use crate::error::AgySwitchError;
use crate::store::account::{Account, OAuthCredential};
use crate::store::file_store::FileStore;

/// Structured result of a JSON import — returned instead of printing.
pub struct ImportResult {
    pub imported: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
}

impl ImportResult {
    pub fn summary(&self) -> String {
        if self.errors.is_empty() {
            format!(
                "{} imported, {} updated, {} skipped",
                self.imported, self.updated, self.skipped
            )
        } else {
            format!(
                "{} imported, {} updated, {} skipped ({} errors)",
                self.imported,
                self.updated,
                self.skipped,
                self.errors.len()
            )
        }
    }
}

/// Run the full OAuth flow and persist the account to the store.
///
/// Returns the email of the added or updated account on success.
/// This function does **not** print to stdout — callers are responsible for
/// their own output (e.g. TUI dashboard).
///
/// Side effects: opens the browser and listens on a local port for the callback.
pub async fn add_oauth_account(store: &mut FileStore) -> Result<String, AgySwitchError> {
    let port = crate::auth::oauth_flow::find_available_port()?;
    let pkce = crate::auth::pkce::generate_pkce();
    let url = crate::auth::oauth_flow::build_authorization_url(port, &pkce).await;

    // Try to open the browser; if it fails, let the caller decide what to do.
    // We still listen regardless.
    let _ = open::that(&url);

    let code = crate::auth::oauth_flow::listen_for_callback(port).await?;

    // Exchange code for tokens
    let client = crate::http::client();
    let redirect_uri = format!("http://localhost:{}/oauth-callback", port);
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", crate::auth::oauth_flow::client_id()),
        ("client_secret", crate::auth::oauth_flow::client_secret()),
        ("code_verifier", pkce.verifier.as_str()),
    ];

    let res = client
        .post(crate::auth::oauth_flow::TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .await
        .map_err(AgySwitchError::Http)?;

    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(AgySwitchError::OAuthFailed(format!(
            "Token exchange failed: {}",
            text
        )));
    }

    let data: serde_json::Value = res.json().await.map_err(AgySwitchError::Http)?;

    let access_token = data["access_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let refresh_token = data["refresh_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let expires_in = data["expires_in"].as_i64().unwrap_or(3600);

    if refresh_token.is_empty() {
        return Err(AgySwitchError::OAuthFailed(
            "No refresh token received".to_string(),
        ));
    }

    let credential = OAuthCredential {
        access_token,
        refresh_token,
        expiry: Utc::now() + chrono::Duration::seconds(expires_in),
        project_id: None,
        managed_project_id: None,
    };

    let email = get_email_from_token(&credential.access_token)
        .await
        .unwrap_or_else(|| format!("unknown-{}", Uuid::new_v4()));

    let credential_clone = credential.clone();
    let account = Account {
        id: Uuid::new_v4(),
        email: email.clone(),
        label: None,
        credential,
        quota: None,
        added_at: Utc::now(),
        last_used_at: None,
        is_rate_limited: false,
        rate_limit_reset_at: None,
        enabled: true,
    };

    match store.add(account).await {
        Ok(()) => {}
        Err(AgySwitchError::DuplicateAccount(e)) => {
            if let Some(existing) = store.get_by_email(&e) {
                let mut updated = existing.clone();
                updated.credential = credential_clone;
                store.update(updated).await?;
            }
        }
        Err(e) => return Err(e),
    }

    Ok(email)
}

async fn get_email_from_token(access_token: &str) -> Option<String> {
    let client = crate::http::client();
    let res = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    if res.status().is_success() {
        let data: serde_json::Value = res.json().await.ok()?;
        data["email"].as_str().map(|s| s.to_string())
    } else {
        None
    }
}

pub async fn handle_add_json(
    store: &mut FileStore,
    path: PathBuf,
) -> Result<ImportResult, AgySwitchError> {
    let contents = tokio::fs::read_to_string(&path)
        .await
        .map_err(AgySwitchError::Io)?;
    let data: serde_json::Value =
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;

    // Support both formats:
    //   {"accounts": [...]}   (wrapped)
    //   [...]                 (bare array)
    let accounts_arr = if let Some(arr) = data.as_array() {
        arr.clone()
    } else if let Some(arr) = data.get("accounts").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Err(AgySwitchError::OAuthFailed(
            "JSON must be an array of accounts, or an object with an 'accounts' array".to_string(),
        ));
    };

    let mut result = ImportResult {
        imported: 0,
        updated: 0,
        skipped: 0,
        errors: Vec::new(),
    };

    for item in &accounts_arr {
        let email = match item.get("email").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => {
                result.skipped += 1;
                result.errors.push("Entry without email skipped".to_string());
                continue;
            }
        };

        // Support both snake_case (refresh_token) and camelCase (refreshToken)
        // Official accounts.json may not have tokens — accept them anyway.
        let refresh_token = item
            .get("refresh_token")
            .or_else(|| item.get("refreshToken"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Support both access_token and accessToken
        let access_token = item
            .get("access_token")
            .or_else(|| item.get("accessToken"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let expiry = item
            .get("expiry")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| Utc::now());

        // Support both project_id and projectId
        let project_id = item
            .get("project_id")
            .or_else(|| item.get("projectId"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let credential = OAuthCredential {
            access_token,
            refresh_token,
            expiry,
            project_id,
            managed_project_id: None,
        };

        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Check for duplicate by email
        if let Some(existing) = store.get_by_email(&email) {
            let mut updated_account = existing.clone();
            updated_account.credential = credential;
            if let Some(l) = label {
                updated_account.label = Some(l);
            }
            if let Err(e) = store.update(updated_account).await {
                result.errors.push(format!("{} update failed: {}", email, e));
            } else {
                result.updated += 1;
            }
        } else {
            let account = Account {
                id: Uuid::new_v4(),
                email,
                label,
                credential,
                quota: None,
                added_at: Utc::now(),
                last_used_at: None,
                is_rate_limited: false,
                rate_limit_reset_at: None,
                enabled: true,
            };
            if let Err(e) = store.add(account).await {
                result.errors.push(format!("Add failed: {}", e));
            } else {
                result.imported += 1;
            }
        }
    }

    Ok(result)
}
