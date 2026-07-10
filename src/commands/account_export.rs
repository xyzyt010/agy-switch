use std::path::PathBuf;

use colored::Colorize;
use crate::error::AgySwitchError;
use crate::store::file_store::FileStore;

#[allow(dead_code)]
pub async fn handle_export(store: &FileStore, out: Option<PathBuf>) -> Result<(), AgySwitchError> {
    let accounts = store.list();

    if accounts.is_empty() {
        println!("[AGY-SWITCH] No accounts to export.");
        return Ok(());
    }

    // Build export JSON (import-compatible format)
    let export_accounts: Vec<serde_json::Value> = accounts
        .iter()
        .map(|account| {
            let mut obj = serde_json::json!({
                "email": account.email,
                "access_token": account.credential.access_token,
                "refresh_token": account.credential.refresh_token,
                "expiry": account.credential.expiry.to_rfc3339(),
            });
            if let Some(label) = &account.label {
                obj["label"] = serde_json::json!(label);
            }
            if let Some(project_id) = &account.credential.project_id {
                obj["project_id"] = serde_json::json!(project_id);
            }
            obj
        })
        .collect();

    let export = serde_json::json!({
        "version": 1,
        "accounts": export_accounts,
    });

    let json = serde_json::to_string_pretty(&export).map_err(AgySwitchError::Json)?;

    match out {
        Some(path) => {
            tokio::fs::write(&path, &json).await.map_err(AgySwitchError::Io)?;
            eprintln!(
                "{} Exported {} accounts to {}",
                "⚠️".yellow(),
                accounts.len(),
                path.display()
            );
        }
        None => {
            println!("{}", json);
        }
    }

    eprintln!(
        "{} WARNING: This file contains OAuth credentials (refresh tokens).\n   Treat it like a password file. Do not commit to git or share publicly.",
        "⚠️".yellow()
    );

    Ok(())
}
