use chrono::Utc;
use colored::Colorize;
use uuid::Uuid;

use crate::config::AppState;
use crate::error::AgySwitchError;
use crate::store::active_writer::write_active_account;
use crate::store::file_store::FileStore;

#[allow(dead_code)]
pub async fn handle_use(
    store: &mut FileStore,
    state: &mut AppState,
    target: &str,
) -> Result<(), AgySwitchError> {
    // Resolve target by UUID or email
    let account = if let Ok(uuid) = Uuid::parse_str(target) {
        store.get(uuid)
            .ok_or_else(|| AgySwitchError::AccountNotFound(target.to_string()))?
            .clone()
    } else {
        store.get_by_email(target)
            .ok_or_else(|| AgySwitchError::AccountNotFound(target.to_string()))?
            .clone()
    };

    if !account.enabled {
        println!("{} Account {} is disabled.", "⚠️".yellow(), account.email);
        return Ok(());
    }

    // Write active credential to Claude Code settings
    write_active_account(&account).await?;

    // Update state
    state.active_account_id = Some(account.id);

    // Persist state immediately
    crate::config::save_state(state).await?;

    // Update the account's last_used_at
    let mut updated = account.clone();
    updated.last_used_at = Some(Utc::now());
    store.update(updated).await?;

    println!(
        "{} Activated account: {}",
        "✅".green(),
        account.email.bold()
    );
    Ok(())
}
