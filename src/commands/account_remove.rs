use uuid::Uuid;

use crate::error::AgySwitchError;
use crate::store::file_store::FileStore;

pub async fn handle_remove(store: &mut FileStore, target: Option<String>, all: bool) -> Result<(), AgySwitchError> {
    if all {
        // Remove from official files too
        for account in store.list().iter() {
            let _ = remove_from_official_files(&account.email).await;
        }
        store.remove_all().await?;
        return Ok(());
    }

    let target = target.ok_or_else(|| {
        AgySwitchError::AccountNotFound("no target".to_string())
    })?;

    // Try as UUID first, then as email
    let id = if let Ok(uuid) = Uuid::parse_str(&target) {
        uuid
    } else {
        match store.get_by_email(&target) {
            Some(account) => account.id,
            None => {
                return Err(AgySwitchError::AccountNotFound(target));
            }
        }
    };

    let account = store.get(id)
        .ok_or_else(|| AgySwitchError::AccountNotFound(target.clone()))?
        .clone();

    // Remove from official files first
    let _ = remove_from_official_files(&account.email).await;

    store.remove(id).await?;

    Ok(())
}

/// Remove an account from the official ~/.antigravity_tools/ accounts.json and delete its individual file.
/// Best-effort — errors are logged but don't fail the removal.
async fn remove_from_official_files(email: &str) -> Result<(), AgySwitchError> {
    let home = dirs::home_dir().ok_or_else(|| {
        AgySwitchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Home directory not found",
        ))
    })?;

    let tools_dir = home.join(".antigravity_tools");
    let accounts_json_path = tools_dir.join("accounts.json");

    if !tokio::fs::try_exists(&accounts_json_path).await.unwrap_or(false) {
        return Ok(());
    }

    let contents = tokio::fs::read_to_string(&accounts_json_path)
        .await
        .map_err(AgySwitchError::Io)?;
    let mut index: serde_json::Value =
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;

    let accounts_arr = match index.get_mut("accounts").and_then(|v| v.as_array_mut()) {
        Some(arr) => arr,
        None => return Ok(()),
    };

    // Find the matching account
    let mut found_id: Option<String> = None;
    let mut found_idx: Option<usize> = None;
    for (i, entry) in accounts_arr.iter().enumerate() {
        if let Some(entry_email) = entry.get("email").and_then(|v| v.as_str()) {
            if entry_email.eq_ignore_ascii_case(email) {
                found_id = entry.get("id").and_then(|v| v.as_str()).map(String::from);
                found_idx = Some(i);
                break;
            }
        }
    }

    if let Some(idx) = found_idx {
        accounts_arr.remove(idx);

        // Write updated index back
        let index_json = serde_json::to_string_pretty(&index).map_err(AgySwitchError::Json)?;
        let _ = tokio::fs::write(&accounts_json_path, index_json).await;
    }

    // Delete the individual account file
    if let Some(id_str) = found_id {
        let individual_path = tools_dir.join("accounts").join(format!("{}.json", id_str));
        let _ = tokio::fs::remove_file(&individual_path).await;
    }

    Ok(())
}
