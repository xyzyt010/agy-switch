use std::io::{self, Write};
use uuid::Uuid;

use crate::error::AgySwitchError;
use crate::store::file_store::FileStore;

pub async fn handle_remove(store: &mut FileStore, target: Option<String>, all: bool) -> Result<(), AgySwitchError> {
    if all {
        print!("[AGY-SWITCH] ⚠️  This will remove ALL accounts. Type 'DELETE ALL' to confirm: ");
        io::stdout().flush().map_err(AgySwitchError::Io)?;
        let mut input = String::new();
        io::stdin().read_line(&mut input).map_err(AgySwitchError::Io)?;
        if input.trim() != "DELETE ALL" {
            println!("[AGY-SWITCH] Cancelled.");
            return Ok(());
        }
        let count = store.count();
        store.remove_all().await?;
        println!("[AGY-SWITCH] ✅ Removed all {} accounts.", count);
        return Ok(());
    }

    let target = target.ok_or_else(|| {
        eprintln!("[AGY-SWITCH] Error: No account specified. Use --all to remove all.");
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
    store.remove(id).await?;
    println!("[AGY-SWITCH] ✅ Removed account: {}", account.email);

    Ok(())
}
