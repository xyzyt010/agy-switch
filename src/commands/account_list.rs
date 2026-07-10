use colored::Colorize;

use crate::error::AgySwitchError;
use crate::store::file_store::FileStore;

#[allow(dead_code)]
pub async fn handle_list(store: &FileStore, verbose: bool) -> Result<(), AgySwitchError> {
    let accounts = store.list();

    if accounts.is_empty() {
        println!("[AGY-SWITCH] No accounts. Use `agy-switch account add` to add one.");
        return Ok(());
    }

    // Detect active account from official antigravity tools
    let active_email = crate::store::active_writer::detect_active_account_from_official().await;

    println!("[AGY-SWITCH] Accounts ({})\n", accounts.len());

    if verbose {
        for (i, account) in accounts.iter().enumerate() {
            let status = if !account.enabled {
                "DISABLED".dimmed().to_string()
            } else if account.is_rate_limited {
                "RATE LIMITED".red().to_string()
            } else {
                "healthy".green().to_string()
            };

            println!(
                "  {} {} [{}]",
                format!("{:>3}", i + 1).dimmed(),
                account.email.bold(),
                status,
            );

            if let Some(label) = &account.label {
                println!("       Label: {}", label);
            }
            println!("       ID: {}", account.id);
            println!();
        }
    } else {
        // Summary mode
        println!("  {:>3}  {:<30} {:<15}", "#", "Email", "Status");
        println!("  {:>3}  {:<30} {:<15}", "---", "------", "------");
        for (i, account) in accounts.iter().enumerate() {
            let status = if !account.enabled {
                "Disabled".dimmed().to_string()
            } else if account.is_rate_limited {
                "Rate Limited".red().to_string()
            } else {
                // No rate-limit detection = healthy
                "Healthy".green().to_string()
            };

            let is_active = active_email.as_deref() == Some(&account.email);
            let active = if is_active {
                "*".green().bold().to_string()
            } else {
                " ".to_string()
            };

            println!(
                "  {}{:<3}  {:<30} {}",
                active,
                i + 1,
                account.email,
                status
            );
        }
    }

    Ok(())
}


