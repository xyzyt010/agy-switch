use crate::config::{accounts_path, app_config_dir, load_state, save_state, SwitchMode};
use crate::error::AgySwitchError;
use crate::store::file_store::FileStore;
use std::time::Duration;

/// Run the daemon loop
pub async fn run_daemon() -> Result<(), AgySwitchError> {
    // Silence stdout/stderr for daemon mode — all logging goes to log file via parent
    // (on Linux, stdio is redirected to /dev/null by the double-fork)

    let stop_path = app_config_dir().join("stop.signal");
    let mut tick = 0u64;

    const QUOTA_CHECK_INTERVAL: u64 = 10;
    const FULL_REFRESH_INTERVAL: u64 = 300;

    loop {
        if stop_path.exists() {
            eprintln!("[AGY-SWITCH] Daemon stopping (stop signal received)");
            let _ = std::fs::remove_file(&stop_path);
            break;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
        tick += 1;

        // Full refresh every 5 minutes
        if tick % FULL_REFRESH_INTERVAL == 0 {
            if let Err(e) = full_refresh().await {
                eprintln!("[AGY-SWITCH] Full refresh error: {}", e);
            }
        }

        // Quota check every 10 seconds
        if tick % QUOTA_CHECK_INTERVAL == 0 {
            if let Err(e) = quota_check_and_auto_switch().await {
                eprintln!("[AGY-SWITCH] Quota check error: {}", e);
            }
        }
    }

    eprintln!("[AGY-SWITCH] Daemon stopped");
    Ok(())
}

/// Full refresh: reload store, sync from official tools, fetch all quotas
async fn full_refresh() -> Result<(), AgySwitchError> {
    let mut store = FileStore::new(accounts_path());
    store.load().await?;

    let _ = crate::store::active_writer::import_from_official_tools(&mut store).await;
    let _ = crate::store::active_writer::import_from_proxy_readonly(&mut store).await;
    let _ = crate::store::active_writer::refresh_accounts_from_official(&mut store).await;
    let _ = crate::store::active_writer::fetch_all_quotas(&mut store).await;
    let _ = store.flush().await;

    let mem_kb = store.memory_usage() / 1024;
    eprintln!(
        "[AGY-SWITCH] Full refresh done: {} accounts, {}KB memory",
        store.count(),
        mem_kb
    );

    Ok(())
}

/// Fetch fresh quota for the active account, update store, and auto-switch if exhausted.
/// This is the 10-second heartbeat that keeps the TUI updated with fresh data.
async fn quota_check_and_auto_switch() -> Result<(), AgySwitchError> {
    let mut state = load_state().await?;
    if !state.enabled {
        return Ok(());
    }

    let active_id = match state.active_account_id {
        Some(id) => id,
        None => return Ok(()),
    };

    let mut store = FileStore::new(accounts_path());
    store.load().await?;

    let active_account = match store.get(active_id) {
        Some(a) => a.clone(),
        None => return Ok(()),
    };

    // Fetch FRESH quota for the active account right now
    let fresh_quota = crate::quota::fetcher::get_model_quotas(
        &active_account.credential,
        &active_account.email,
    ).await;

    match fresh_quota {
        Ok(snapshot) => {
            // Check if quota actually changed to avoid unnecessary disk writes
            let changed = match &active_account.quota {
                Some(old) => !quotas_equal(old, &snapshot),
                None => true,
            };
            let was_rate_limited = active_account.is_rate_limited;

            // Update the account in store with fresh quota and flush only if changed
            if changed || was_rate_limited {
                if let Some(mut acc) = store.get(active_id).cloned() {
                    acc.quota = Some(snapshot.clone());
                    // Account was rate limited before but now has fresh quota → clear it
                    if was_rate_limited {
                        acc.is_rate_limited = false;
                        acc.rate_limit_reset_at = None;
                    }
                    let _ = store.update(acc).await;
                }
            }

            // Check if exhausted using FRESH data
            if state.mode == SwitchMode::Auto && is_quota_exhausted(&snapshot) {
                eprintln!(
                    "[AGY-SWITCH] Active account {} exhausted, auto-switching...",
                    active_account.email
                );
                auto_switch_next(&mut store, active_id, &mut state).await?;
            } else if changed {
                eprintln!(
                    "[AGY-SWITCH] Quota updated for {}",
                    active_account.email
                );
            }
        }
        Err(AgySwitchError::RateLimited { endpoint, reset_at }) => {
            eprintln!(
                "[AGY-SWITCH] Account {} rate limited (429 from {}), marking...",
                active_account.email, endpoint
            );
            // Mark account as rate limited
            if let Some(mut acc) = store.get(active_id).cloned() {
                if !acc.is_rate_limited {
                    acc.is_rate_limited = true;
                    // Use API-provided reset time if available; otherwise default to 10 minutes
                    // (chosen empirically — Cloud Code quota windows usually reset within minutes,
                    // and the daemon re-checks every 10s anyway).
                    acc.rate_limit_reset_at = Some(reset_at.unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::minutes(10)));
                    let _ = store.update(acc).await;

                    // Auto-switch if in auto mode
                    if state.mode == SwitchMode::Auto {
                        eprintln!(
                            "[AGY-SWITCH] Active account {} rate limited, auto-switching...",
                            active_account.email
                        );
                        auto_switch_next(&mut store, active_id, &mut state).await?;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!(
                "[AGY-SWITCH] Failed to fetch quota for {}: {}",
                active_account.email, e
            );
        }
    }

    Ok(())
}

/// Compare two quota snapshots for equality (to avoid unnecessary disk writes)
fn quotas_equal(a: &crate::quota::models::QuotaSnapshot, b: &crate::quota::models::QuotaSnapshot) -> bool {
    if a.models.len() != b.models.len() {
        return false;
    }
    for (am, bm) in a.models.iter().zip(b.models.iter()) {
        if am.model_id != bm.model_id || am.remaining_fraction != bm.remaining_fraction {
            return false;
        }
    }
    true
}

/// Check if a fresh QuotaSnapshot is exhausted (all models at 0%)
fn is_quota_exhausted(snapshot: &crate::quota::models::QuotaSnapshot) -> bool {
    if snapshot.models.is_empty() {
        return false;
    }
    snapshot.models.iter().all(|m| {
        m.remaining_fraction.map_or(false, |f| f <= 0.0)
    })
}

/// Switch to the next available account (highest quota first, A-Z within tier)
async fn auto_switch_next(
    store: &mut FileStore,
    current_id: uuid::Uuid,
    state: &mut crate::config::AppState,
) -> Result<(), AgySwitchError> {
    if let Some((_, next_account)) = store.next_available_by_id(current_id) {
        let next_id = next_account.id;
        let next_email = next_account.email.clone();

        match crate::store::active_writer::write_active_account(&next_account).await {
            Ok(fresh_cred) => {
                if let Some(stored) = store.list_mut().iter_mut().find(|a| a.id == next_id) {
                    stored.credential = fresh_cred;
                }
                let _ = store.flush().await;

                state.active_account_id = Some(next_id);
                save_state(state).await?;

                eprintln!("[AGY-SWITCH] Switched to: {}", next_email);
            }
            Err(e) => {
                eprintln!("[AGY-SWITCH] Failed to switch: {}", e);
            }
        }
    } else {
        eprintln!("[AGY-SWITCH] No available accounts to switch to");
    }
    Ok(())
}
