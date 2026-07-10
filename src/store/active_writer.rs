use serde_json::Value;
use std::path::PathBuf;

use crate::error::AgySwitchError;
use crate::store::account::Account;

/// Write the active account to the locations that the Antigravity CLI and app read.
/// This is what the official `agy` CLI reads to determine which account is active.
///
/// Writes to (all best-effort, no single failure aborts the others):
/// 1. ~/.antigravity_tools/accounts/<uuid>.json + accounts.json index
/// 2. ~/.antigravity/state.vscdb
/// 3. ~/.claude/settings.json
/// 4. ~/.gemini/google_accounts.json + oauth_creds.json
/// 5. Windows Credential Manager (gemini:antigravity)
///
/// Returns the fresh credential on success so the caller can update its store.
pub async fn write_active_account(account: &Account) -> Result<crate::store::account::OAuthCredential, AgySwitchError> {
    // Step 1: Refresh the access token to ensure it's fresh
    let fresh_credential = crate::auth::token_refresh::ensure_fresh_with_email(&account.credential, Some(&account.email)).await
        .map_err(|e| {
            AgySwitchError::OAuthFailed(format!("Token refresh failed for {}: {}", account.email, e))
        })?;

    // Step 2: Write to ~/.antigravity_tools/ (the official `agy` CLI reads this)
    let _ = write_to_agy_tools(account, &fresh_credential).await;

    // Step 3: Write to Antigravity state.vscdb (the official agy CLI reads this)
    let _ = write_to_state_vscdb(account, &fresh_credential).await;

    // Step 4: Also write to ~/.claude/settings.json (backup for Claude CLI)
    let _ = write_claude_settings(account, &fresh_credential).await;

    // Step 5: Write to ~/.gemini/google_accounts.json + oauth_creds.json
    let _ = write_to_gemini_google_accounts(account, &fresh_credential).await;

    // Step 6: Write to Windows Credential Manager (THE actual agy CLI auth source)
    let _ = write_to_windows_credential_manager(account, &fresh_credential).await;

    Ok(fresh_credential)
}

/// Write to ~/.antigravity_tools/ — this is what the official `agy` CLI reads.
/// Updates both the individual account file (with fresh tokens) and the index.
/// If the account is not already in the official list, it will be added.
async fn write_to_agy_tools(
    account: &Account,
    credential: &crate::store::account::OAuthCredential,
) -> Result<(), AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let tools_dir = home.join(".antigravity_tools");
    let accounts_json_path = tools_dir.join("accounts.json");

    if !tokio::fs::try_exists(&accounts_json_path)
        .await
        .unwrap_or(false)
    {
        return Err(AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Official antigravity tools not found at: {}",
                tools_dir.display()
            ),
        )));
    }

    // Read the existing accounts.json index
    let contents = tokio::fs::read_to_string(&accounts_json_path)
        .await
        .map_err(AgySwitchError::Io)?;
    let mut index: Value =
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;

    // Ensure accounts array exists
    if index.get("accounts").and_then(|v| v.as_array()).is_none() {
        index["accounts"] = Value::Array(Vec::new());
    }

    let accounts_arr = index["accounts"].as_array_mut().unwrap();

    // Find the matching official account UUID by email, or create a new entry
    let official_id = accounts_arr
        .iter()
        .find(|a| {
            a.get("email")
                .and_then(|v| v.as_str())
                .map(|e| e.eq_ignore_ascii_case(&account.email))
                .unwrap_or(false)
        })
        .and_then(|a| a.get("id").and_then(|v| v.as_str()));

    let official_id = match official_id {
        Some(id) => id.to_string(),
        None => {
            // Account not in official list — add it
            let new_id = uuid::Uuid::new_v4().to_string();
            let new_entry = serde_json::json!({
                "email": account.email,
                "id": new_id,
                "handle": account.email.split('@').next().unwrap_or(&account.email),
                "avatar_url": null,
            });
            accounts_arr.push(new_entry);
            new_id
        }
    };

    // Step A: Update or create the individual account file with fresh token
    let accounts_dir = tools_dir.join("accounts");
    let _ = tokio::fs::create_dir_all(&accounts_dir).await;
    let individual_path = accounts_dir.join(format!("{}.json", official_id));
    if tokio::fs::try_exists(&individual_path)
        .await
        .unwrap_or(false)
    {
        // Update existing file
        let individual_contents = tokio::fs::read_to_string(&individual_path)
            .await
            .map_err(AgySwitchError::Io)?;
        let mut individual: Value =
            serde_json::from_str(&individual_contents).map_err(AgySwitchError::Json)?;

        // Update the token block with fresh credentials
        if let Some(token_obj) = individual.get_mut("token") {
            if let Some(token_map) = token_obj.as_object_mut() {
                token_map.insert("access_token".to_string(), Value::String(credential.access_token.clone()));
                let (real_token, _, _) = crate::auth::token_refresh::parse_composite_token(&credential.refresh_token);
                token_map.insert("refresh_token".to_string(), Value::String(real_token));
                let expires_in = (credential.expiry - chrono::Utc::now()).num_seconds().max(0) as u64;
                token_map.insert("expires_in".to_string(), Value::Number(expires_in.into()));
                token_map.insert(
                    "expiry_timestamp".to_string(),
                    Value::Number(credential.expiry.timestamp().into()),
                );
                if let Some(proj) = &credential.project_id {
                    token_map.insert("project_id".to_string(), Value::String(proj.clone()));
                }
            }
        }

        let json = serde_json::to_string_pretty(&individual).map_err(AgySwitchError::Json)?;
        tokio::fs::write(&individual_path, json)
            .await
            .map_err(AgySwitchError::Io)?;
    } else {
        // Create new individual account file with default 100% quota for all known models
        let (real_token, _, _) = crate::auth::token_refresh::parse_composite_token(&credential.refresh_token);
        let expires_in = (credential.expiry - chrono::Utc::now()).num_seconds().max(0) as u64;
        let now_rfc3339 = chrono::Utc::now().to_rfc3339();

        // Build default quota models (100% remaining for each)
        let default_models: Vec<Value> = crate::quota::fetcher::KNOWN_MODELS
            .iter()
            .map(|(model_id, display_name)| {
                serde_json::json!({
                    "name": model_id,
                    "display_name": display_name,
                    "percentage": 100,
                    "reset_time": now_rfc3339,
                })
            })
            .collect();

        let individual = serde_json::json!({
            "token": {
                "access_token": credential.access_token,
                "token_type": "Bearer",
                "refresh_token": real_token,
                "expires_in": expires_in,
                "expiry_timestamp": credential.expiry.timestamp(),
                "project_id": credential.project_id,
                "scope": "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/userinfo.email openid",
            },
            "email": account.email,
            "name": account.email.split('@').next().unwrap_or(&account.email),
            "quota": {
                "models": default_models,
            },
        });
        let json = serde_json::to_string_pretty(&individual).map_err(AgySwitchError::Json)?;
        tokio::fs::write(&individual_path, json)
            .await
            .map_err(AgySwitchError::Io)?;
    }

    // Step B: Update the index to set current_account_id
    index["current_account_id"] = Value::String(official_id.to_string());
    let index_json = serde_json::to_string_pretty(&index).map_err(AgySwitchError::Json)?;
    tokio::fs::write(&accounts_json_path, index_json)
        .await
        .map_err(AgySwitchError::Io)?;

    Ok(())
}

/// Write to Antigravity state.vscdb using system sqlite3.exe
async fn write_to_state_vscdb(
    account: &Account,
    credential: &crate::store::account::OAuthCredential,
) -> Result<(), AgySwitchError> {
    let db_path = antigravity_state_db()?;

    let auth_status = serde_json::json!({
        "name": "antigravity",
        "apiKey": credential.access_token,
        "email": account.email,
    });

    let auth_json = serde_json::to_string(&auth_status).map_err(AgySwitchError::Json)?;

    let db_path_clone = db_path.clone();
    let auth_json_clone = auth_json.clone();
    let _ = tokio::task::spawn_blocking(move || {
        write_to_state_db(&db_path_clone, &auth_json_clone)
    })
    .await
    .map_err(|e| AgySwitchError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    Ok(())
}

/// Also update ~/.claude/settings.json for Claude CLI compatibility
async fn write_claude_settings(
    _account: &Account,
    credential: &crate::store::account::OAuthCredential,
) -> Result<(), AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let settings_path = home.join(".claude").join("settings.json");

    if let Some(parent) = settings_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(AgySwitchError::Io)?;
    }

    let mut settings = read_or_create_settings(&settings_path).await?;

    if settings.get("env").is_none() {
        settings["env"] = Value::Object(serde_json::Map::new());
    }
    let env = settings["env"].as_object_mut().unwrap();
    env.insert(
        "ANTIGRAVITY_OAUTH_TOKEN".to_string(),
        Value::String(credential.access_token.clone()),
    );
    env.insert(
        "ANTIGRAVITY_REFRESH_TOKEN".to_string(),
        Value::String(credential.refresh_token.clone()),
    );

    let contents = serde_json::to_string_pretty(&settings).map_err(AgySwitchError::Json)?;
    tokio::fs::write(&settings_path, contents)
        .await
        .map_err(AgySwitchError::Io)?;

    Ok(())
}

/// Get the path to Antigravity's state.vscdb
fn antigravity_state_db() -> Result<PathBuf, AgySwitchError> {
    let app_data = dirs::data_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| {
            AgySwitchError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "AppData directory not found",
            ))
        })?;
    let db_path = app_data
        .join("Antigravity")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");

    if !db_path.exists() {
        return Err(AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Antigravity database not found at: {}", db_path.display()),
        )));
    }

    Ok(db_path)
}

/// Write auth status to the SQLite database using Python's sqlite3 module
/// (avoids shell quoting issues with sqlite3.exe that strip JSON double quotes)
fn write_to_state_db(db_path: &PathBuf, auth_json: &str) -> Result<(), AgySwitchError> {
    // Write a temp Python script to do the update
    let script = format!(
        r#"import sqlite3
db = sqlite3.connect(r'{}')
cur = db.cursor()
cur.execute("UPDATE ItemTable SET value=? WHERE key='antigravityAuthStatus'", (r'{}',))
if cur.rowcount == 0:
    cur.execute("INSERT INTO ItemTable(key,value) VALUES('antigravityAuthStatus', ?)", (r'{}',))
db.commit()
db.close()
"#,
        db_path.display().to_string().replace('\\', "\\\\"),
        auth_json.replace('\\', "\\\\").replace('\'', "\\'"),
        auth_json.replace('\\', "\\\\").replace('\'', "\\'")
    );

    let script_path = std::env::temp_dir().join("agy_switch_vscdb.py");
    std::fs::write(&script_path, &script).map_err(|e| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to write temp script: {}", e),
        ))
    })?;

    let output = std::process::Command::new("python")
        .arg(&script_path)
        .output()
        .map_err(|e| {
            AgySwitchError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to run python: {}", e),
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            return Err(AgySwitchError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Python sqlite3 failed: {}", stderr),
            )));
        }
    }

    Ok(())
}

async fn read_or_create_settings(path: &PathBuf) -> Result<Value, AgySwitchError> {
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let contents = tokio::fs::read_to_string(path).await.map_err(AgySwitchError::Io)?;
        if contents.trim().is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)
    } else {
        Ok(Value::Object(serde_json::Map::new()))
    }
}

/// Write to ~/.gemini/google_accounts.json and ~/.gemini/oauth_creds.json
/// This is what the `agy` CLI actually reads to determine which account to authenticate with.
async fn write_to_gemini_google_accounts(
    account: &Account,
    credential: &crate::store::account::OAuthCredential,
) -> Result<(), AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let gemini_dir = home.join(".gemini");

    // Ensure ~/.gemini/ exists
    tokio::fs::create_dir_all(&gemini_dir).await.map_err(AgySwitchError::Io)?;

    // Step 1: Write google_accounts.json — sets the active email
    let ga_path = gemini_dir.join("google_accounts.json");
    let (real_token, _, _) = crate::auth::token_refresh::parse_composite_token(&credential.refresh_token);

    // Read the official account file to get the id_token and other fields
    let tools_dir = home.join(".antigravity_tools");
    let accounts_json_path = tools_dir.join("accounts.json");
    let mut id_token = String::new();
    let mut token_type = "Bearer".to_string();
    let mut scope = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/userinfo.email openid".to_string();

    if let Ok(index_contents) = tokio::fs::read_to_string(&accounts_json_path).await {
        if let Ok(index) = serde_json::from_str::<Value>(&index_contents) {
            if let Some(accounts_arr) = index.get("accounts").and_then(|v| v.as_array()) {
                if let Some(official) = accounts_arr.iter().find(|a| {
                    a.get("email")
                        .and_then(|v| v.as_str())
                        .map(|e| e.eq_ignore_ascii_case(&account.email))
                        .unwrap_or(false)
                }) {
                    let official_id = official.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let individual_path = tools_dir.join("accounts").join(format!("{}.json", official_id));
                    if let Ok(acc_contents) = tokio::fs::read_to_string(&individual_path).await {
                        if let Ok(acc_data) = serde_json::from_str::<Value>(&acc_contents) {
                            if let Some(tok) = acc_data.get("token") {
                                id_token = tok.get("id_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                if let Some(tt) = tok.get("token_type").and_then(|v| v.as_str()) {
                                    token_type = tt.to_string();
                                }
                                if let Some(sc) = tok.get("scope").and_then(|v| v.as_str()) {
                                    scope = sc.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let ga_contents = serde_json::json!({
        "active": account.email,
        "old": [],
    });

    let ga_json = serde_json::to_string_pretty(&ga_contents).map_err(AgySwitchError::Json)?;
    tokio::fs::write(&ga_path, ga_json).await.map_err(AgySwitchError::Io)?;

    // Step 2: Write oauth_creds.json — the actual OAuth tokens agy uses
    let oauth_path = gemini_dir.join("oauth_creds.json");
    let expiry_ms = credential.expiry.timestamp_millis();

    let oauth_contents = serde_json::json!({
        "access_token": credential.access_token,
        "scope": scope,
        "token_type": token_type,
        "id_token": id_token,
        "expiry_date": expiry_ms,
        "refresh_token": real_token,
    });

    let oauth_json = serde_json::to_string_pretty(&oauth_contents).map_err(AgySwitchError::Json)?;
    tokio::fs::write(&oauth_path, oauth_json).await.map_err(AgySwitchError::Io)?;

    Ok(())
}

/// Write to Windows Credential Manager target "gemini:antigravity"
/// This is what the `agy` CLI actually reads via ChainedAuth/keyring.
#[cfg(target_os = "windows")]
async fn write_to_windows_credential_manager(
    _account: &Account,
    credential: &crate::store::account::OAuthCredential,
) -> Result<(), AgySwitchError> {
    let (real_token, _, _) = crate::auth::token_refresh::parse_composite_token(&credential.refresh_token);

    // Build the JSON blob that agy expects
    let blob_data = serde_json::json!({
        "token": {
            "access_token": credential.access_token,
            "token_type": "Bearer",
            "refresh_token": real_token,
            "expiry": credential.expiry.to_rfc3339(),
        },
        "auth_method": "consumer",
    });

    let blob_json = serde_json::to_string(&blob_data).map_err(AgySwitchError::Json)?;

    // Write using Python ctypes (reliable way to call CredWrite on Windows)
    // Write blob_json to a temp file, then have Python read it
    let blob_file = std::env::temp_dir().join("agy_switch_cred_data.json");
    std::fs::write(&blob_file, &blob_json).map_err(|e| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to write blob data: {}", e),
        ))
    })?;

    let script = r#"import ctypes
import ctypes.wintypes as wintypes
import json
import os

class CREDENTIAL(ctypes.Structure):
    _fields_ = [
        ('Flags', wintypes.DWORD),
        ('Type', wintypes.DWORD),
        ('TargetName', wintypes.LPCWSTR),
        ('Comment', wintypes.LPCWSTR),
        ('LastWritten', wintypes.FILETIME),
        ('CredentialBlobSize', wintypes.DWORD),
        ('CredentialBlob', ctypes.POINTER(wintypes.BYTE)),
        ('Persist', wintypes.DWORD),
        ('AttributeCount', wintypes.DWORD),
        ('Attributes', ctypes.POINTER(ctypes.c_void_p)),
        ('TargetAlias', wintypes.LPCWSTR),
        ('UserName', wintypes.LPCWSTR),
    ]

advapi32 = ctypes.windll.advapi32
cred_write = advapi32.CredWriteW
cred_write.argtypes = [ctypes.POINTER(CREDENTIAL), wintypes.DWORD]
cred_write.restype = wintypes.BOOL

cred_delete = advapi32.CredDeleteW
cred_delete.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD]
cred_delete.restype = wintypes.BOOL

# Delete existing credential first
cred_delete('gemini:antigravity', 1, 0)

# Read blob from file
blob_path = os.path.join(os.environ['TEMP'], 'agy_switch_cred_data.json')
with open(blob_path, 'r') as f:
    blob_json = f.read()

blob_bytes = blob_json.encode('utf-8')
blob_len = len(blob_bytes)
blob_array = (wintypes.BYTE * blob_len)(*blob_bytes)

# Create credential
cred = CREDENTIAL()
cred.Flags = 0
cred.Type = 1  # CRED_TYPE_GENERIC
cred.TargetName = 'gemini:antigravity'
cred.Comment = None
cred.CredentialBlobSize = blob_len
cred.CredentialBlob = ctypes.cast(blob_array, ctypes.POINTER(wintypes.BYTE))
cred.Persist = 2  # CRED_PERSIST_LOCAL_MACHINE
cred.AttributeCount = 0
cred.Attributes = None
cred.TargetAlias = None
cred.UserName = 'antigravity'

if cred_write(ctypes.byref(cred), 0):
    print('OK')
else:
    err = ctypes.GetLastError()
    print(f'FAIL: error {err}')
"#;

    let script_path = std::env::temp_dir().join("agy_switch_cred.py");
    std::fs::write(&script_path, &script).map_err(|e| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to write temp script: {}", e),
        ))
    })?;

    let output = std::process::Command::new("python")
        .arg(&script_path)
        .output()
        .map_err(|e| {
            AgySwitchError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to run python: {}", e),
            ))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() && stdout.trim() == "OK" {
        Ok(())
    } else {
        let msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Err(AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("CredWrite failed: {}", msg),
        )))
    }
}

#[cfg(not(target_os = "windows"))]
async fn write_to_windows_credential_manager(
    _account: &Account,
    _credential: &crate::store::account::OAuthCredential,
) -> Result<(), AgySwitchError> {
    Ok(())
}

/// Detect the currently active account from official ~/.antigravity_tools/accounts.json.
/// Returns the email of the active account, if one is set.
#[allow(dead_code)]
pub async fn detect_active_account_from_official() -> Option<String> {
    let home = dirs::home_dir()?;
    let tools_dir = home.join(".antigravity_tools");
    let accounts_json_path = tools_dir.join("accounts.json");

    let contents = tokio::fs::read_to_string(&accounts_json_path)
        .await
        .ok()?;
    let index: Value = serde_json::from_str(&contents).ok()?;

    let current_id = index.get("current_account_id")?.as_str()?;

    let accounts_arr = index.get("accounts")?.as_array()?;

    let matching = accounts_arr.iter().find(|a| {
        a.get("id")
            .and_then(|v| v.as_str())
            .map(|id| id == current_id)
            .unwrap_or(false)
    })?;

    matching.get("email").and_then(|v| v.as_str()).map(String::from)
}

/// Refresh quota and token data from the official ~/.antigravity_tools/ for existing accounts.
/// This reads the official account files and updates our store with the latest quota data,
/// without adding new accounts. Returns the number of accounts updated.
pub async fn refresh_accounts_from_official(
    store: &mut crate::store::file_store::FileStore,
) -> Result<u32, AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let tools_dir = home.join(".antigravity_tools");
    let accounts_json_path = tools_dir.join("accounts.json");

    if !tokio::fs::try_exists(&accounts_json_path)
        .await
        .unwrap_or(false)
    {
        return Ok(0);
    }

    let contents = tokio::fs::read_to_string(&accounts_json_path)
        .await
        .map_err(AgySwitchError::Io)?;
    let index: Value =
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;

    let accounts_dir = tools_dir.join("accounts");
    let mut updated = 0u32;

    let accounts_arr = index
        .get("accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut email_to_official_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for official_account in &accounts_arr {
        if let Some(email) = official_account.get("email").and_then(|v| v.as_str()) {
            let id = official_account.get("id").and_then(|v| v.as_str()).unwrap_or("");
            email_to_official_id.insert(email.to_string(), id.to_string());
        }
    }

    // Refresh TOKENS from official files, then fetch QUOTA from Google API.
    let all_accounts: Vec<_> = store.list().iter().cloned().collect();
    for account in &all_accounts {
        let official_id = match email_to_official_id.get(&account.email) {
            Some(id) => id,
            None => continue,
        };

        let account_file = accounts_dir.join(format!("{}.json", official_id));
        if !tokio::fs::try_exists(&account_file)
            .await
            .unwrap_or(false)
        {
            continue;
        }

        let Ok(account_contents) = tokio::fs::read_to_string(&account_file).await else {
            continue;
        };
        let Ok(account_data) = serde_json::from_str::<Value>(&account_contents) else {
            continue;
        };

        let mut updated_account = account.clone();
        let mut changed = false;

        if let Some(tok) = account_data.get("token") {
            if let Some(at) = tok.get("access_token").and_then(|v| v.as_str()) {
                if !at.is_empty() && updated_account.credential.access_token != at {
                    updated_account.credential.access_token = at.to_string();
                    changed = true;
                }
            }

            if let Some(rt) = tok.get("refresh_token").and_then(|v| v.as_str()) {
                if !rt.is_empty() {
                    let composite = crate::auth::token_refresh::build_composite_token(
                        rt,
                        updated_account.credential.project_id.as_deref(),
                        updated_account.credential.managed_project_id.as_deref(),
                    );
                    if updated_account.credential.refresh_token != composite {
                        updated_account.credential.refresh_token = composite;
                        changed = true;
                    }
                }
            }

            if let Some(pid) = tok.get("project_id").and_then(|v| v.as_str()) {
                if updated_account.credential.project_id.as_deref() != Some(pid) {
                    updated_account.credential.project_id = Some(pid.to_string());
                    changed = true;
                }
            }

            if let Some(exp) = tok.get("expiry_timestamp").and_then(|v| v.as_i64()) {
                if let Some(dt) = chrono::DateTime::from_timestamp(exp, 0) {
                    if updated_account.credential.expiry != dt {
                        updated_account.credential.expiry = dt;
                        changed = true;
                    }
                }
            }
        }

        if changed {
            let _ = store.update(updated_account).await;
            updated += 1;
        }
    }

    Ok(updated)
}

/// Fetch real quota from Google API for all accounts in the store.
/// Uses concurrent requests (10 at a time) for speed.
pub async fn fetch_all_quotas(
    store: &mut crate::store::file_store::FileStore,
) -> Result<u32, AgySwitchError> {
    let all_accounts: Vec<_> = store.list().iter().cloned().collect();
    let total = all_accounts.len() as u32;

    // Scale concurrency: min(account_count, 20) to avoid overwhelming the API
    let concurrency = total.min(20) as usize;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for account in &all_accounts {
        let cred = account.credential.clone();
        let email = account.email.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let id = account.id;

        handles.push(tokio::spawn(async move {
            let result = crate::quota::fetcher::get_model_quotas(&cred, &email).await;
            drop(permit); // release semaphore slot
            (id, result)
        }));
    }

    let mut updated = 0u32;
    let mut rate_limited = 0u32;
    for handle in handles {
        match handle.await {
            Ok((id, Ok(snapshot))) => {
                if !snapshot.models.is_empty() {
                    if let Some(account) = store.list().iter().find(|a| a.id == id).cloned() {
                        let mut updated_account = account;
                        updated_account.quota = Some(snapshot);
                        // Clear rate limit flag if we got fresh quota
                        if updated_account.is_rate_limited {
                            updated_account.is_rate_limited = false;
                            updated_account.rate_limit_reset_at = None;
                        }
                        let _ = store.update(updated_account).await;
                        updated += 1;
                    }
                }
            }
            Ok((id, Err(AgySwitchError::RateLimited(_)))) => {
                // Mark account as rate limited
                if let Some(account) = store.list().iter().find(|a| a.id == id).cloned() {
                    if !account.is_rate_limited {
                        let mut updated_account = account;
                        updated_account.is_rate_limited = true;
                        updated_account.rate_limit_reset_at =
                            Some(chrono::Utc::now() + chrono::Duration::hours(1));
                        let _ = store.update(updated_account).await;
                        rate_limited += 1;
                    }
                }
            }
            _ => {}
        }
    }

    eprintln!(
        "[AGY-SWITCH] Fetched quota for {}/{} accounts ({} rate limited)",
        updated, total, rate_limited
    );
    Ok(updated)
}

/// Import accounts from the official ~/.antigravity_tools/ into our store.
/// Reads the official accounts.json and individual account files, extracts
/// tokens and quota data, and adds them to our FileStore.
/// Matches by email to avoid duplicates.
pub async fn import_from_official_tools(
    store: &mut crate::store::file_store::FileStore,
) -> Result<u32, AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let tools_dir = home.join(".antigravity_tools");
    let accounts_json_path = tools_dir.join("accounts.json");

    if !tokio::fs::try_exists(&accounts_json_path)
        .await
        .unwrap_or(false)
    {
        return Ok(0);
    }

    let contents = tokio::fs::read_to_string(&accounts_json_path)
        .await
        .map_err(AgySwitchError::Io)?;
    let index: Value =
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;

    let accounts_arr = index
        .get("accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let accounts_dir = tools_dir.join("accounts");
    let mut imported = 0u32;

    for official_account in &accounts_arr {
        let official_email = match official_account.get("email").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => continue,
        };

        let official_id = official_account
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Skip if already in our store (update existing instead)
        if let Some(existing) = store.get_by_email(&official_email) {
            // Update tokens from official file (never quota — official files are unreliable)
            let account_file = accounts_dir.join(format!("{}.json", official_id));
            if tokio::fs::try_exists(&account_file)
                .await
                .unwrap_or(false)
            {
                if let Ok(account_contents) = tokio::fs::read_to_string(&account_file).await {
                    if let Ok(account_data) = serde_json::from_str::<Value>(&account_contents) {
                        let mut updated = existing.clone();
                        let mut changed = false;

                        // Update token fields from official file (never quota — official files are unreliable)
                        if let Some(tok) = account_data.get("token") {
                            // Update project_id from official file
                            if let Some(pid) = tok.get("project_id").and_then(|v| v.as_str()) {
                                if updated.credential.project_id.as_deref() != Some(pid) {
                                    updated.credential.project_id = Some(pid.to_string());
                                    changed = true;
                                }
                            }

                            // Update access_token from official file (may have been refreshed by antigravity)
                            if let Some(at) = tok.get("access_token").and_then(|v| v.as_str()) {
                                if !at.is_empty() && updated.credential.access_token != at {
                                    updated.credential.access_token = at.to_string();
                                    changed = true;
                                }
                            }

                            // Update refresh_token from official file (may have been rotated)
                            if let Some(rt) = tok.get("refresh_token").and_then(|v| v.as_str()) {
                                if !rt.is_empty() {
                                    // Preserve composite structure if we have project IDs
                                    let composite = crate::auth::token_refresh::build_composite_token(
                                        rt,
                                        updated.credential.project_id.as_deref(),
                                        updated.credential.managed_project_id.as_deref(),
                                    );
                                    if updated.credential.refresh_token != composite {
                                        updated.credential.refresh_token = composite;
                                        changed = true;
                                    }
                                }
                            }

                            // Update expiry from official file
                            if let Some(exp) = tok.get("expiry_timestamp").and_then(|v| v.as_i64()) {
                                if let Some(dt) = chrono::DateTime::from_timestamp(exp, 0) {
                                    if updated.credential.expiry != dt {
                                        updated.credential.expiry = dt;
                                        changed = true;
                                    }
                                }
                            }
                        }

                        if changed {
                            let _ = store.update(updated).await;
                        }
                    }
                }
            }
            continue;
        }

        // Read the individual account file
        let account_file = accounts_dir.join(format!("{}.json", official_id));
        if !tokio::fs::try_exists(&account_file)
            .await
            .unwrap_or(false)
        {
            continue;
        }

        let account_contents = match tokio::fs::read_to_string(&account_file).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        let account_data: Value = match serde_json::from_str(&account_contents) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract token info
        let token = match account_data.get("token") {
            Some(t) => t,
            None => continue,
        };

        let access_token = token
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let refresh_token = token
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if refresh_token.is_empty() {
            continue;
        }

        let expiry_timestamp = token
            .get("expiry_timestamp")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        let expiry = chrono::DateTime::from_timestamp(expiry_timestamp, 0)
            .unwrap_or_else(|| chrono::Utc::now());

        let project_id = token
            .get("project_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let label = account_data
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Quota is not read from official files — they are unreliable.
        // New accounts start with no quota data (shown as "Unknown" until log detection fires).
        let credential = crate::store::account::OAuthCredential {
            access_token,
            refresh_token,
            expiry,
            project_id: project_id.clone(),
            managed_project_id: None,
        };

        let account = Account {
            id: uuid::Uuid::new_v4(),
            email: official_email,
            label,
            credential,
            quota: None,
            added_at: chrono::Utc::now(),
            last_used_at: None,
            is_rate_limited: false,
            rate_limit_reset_at: None,
            enabled: true,
        };

        if let Err(e) = store.add(account).await {
            match e {
                AgySwitchError::DuplicateAccount(_) => {}
                _ => eprintln!("[AGY-SWITCH] Import error: {}", e),
            }
        } else {
            imported += 1;
        }
    }

    Ok(imported)
}

/// Import accounts from ~/.config/antigravity-proxy/accounts.json (read-only).
/// This file is managed by the antigravity-claude-proxy app. We only READ from it,
/// never write. This allows our app to show all accounts the user has, even if they
/// were added through the proxy app.
pub async fn import_from_proxy_readonly(
    store: &mut crate::store::file_store::FileStore,
) -> Result<u32, AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let proxy_path = home.join(".config").join("antigravity-proxy").join("accounts.json");

    if !tokio::fs::try_exists(&proxy_path).await.unwrap_or(false) {
        return Ok(0);
    }

    let contents = tokio::fs::read_to_string(&proxy_path).await.map_err(AgySwitchError::Io)?;
    let proxy: Value = serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;

    let accounts_arr = match proxy.get("accounts").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(0),
    };

    let mut imported = 0u32;

    for item in accounts_arr {
        let email = match item.get("email").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => continue,
        };

        // Skip if already in our store
        if store.get_by_email(&email).is_some() {
            continue;
        }

        let refresh_token = match item.get("refreshToken").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => continue,
        };

        if refresh_token.is_empty() {
            continue;
        }

        // Check if marked invalid in the proxy
        let is_invalid = item.get("isInvalid").and_then(|v| v.as_bool()).unwrap_or(false);
        if is_invalid {
            continue;
        }

        let credential = crate::store::account::OAuthCredential {
            access_token: String::new(), // Will be refreshed on first use
            refresh_token,
            expiry: chrono::Utc::now(), // Will be refreshed on first use
            project_id: None,
            managed_project_id: None,
        };

        let account = Account {
            id: uuid::Uuid::new_v4(),
            email,
            label: None,
            credential,
            quota: None,
            added_at: chrono::Utc::now(),
            last_used_at: None,
            is_rate_limited: false,
            rate_limit_reset_at: None,
            enabled: true,
        };

        if let Err(e) = store.add(account).await {
            match e {
                AgySwitchError::DuplicateAccount(_) => {}
                _ => eprintln!("[AGY-SWITCH] Proxy import error: {}", e),
            }
        } else {
            imported += 1;
        }
    }

    Ok(imported)
}

