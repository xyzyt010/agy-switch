use std::path::PathBuf;

use crate::config::ensure_config_dir;
use crate::error::AgySwitchError;
use crate::store::account::Account;
use uuid::Uuid;

/// In-memory JSON-based file store for accounts.
/// All data lives in a Vec<Account> in RAM. Flushed to disk atomically.
pub struct FileStore {
    accounts: Vec<Account>,
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        FileStore {
            accounts: Vec::new(),
            path,
        }
    }

    /// Load accounts from disk
    pub async fn load(&mut self) -> Result<(), AgySwitchError> {
        if !tokio::fs::try_exists(&self.path).await.unwrap_or(false) {
            return Ok(());
        }
        let contents = tokio::fs::read_to_string(&self.path).await.map_err(AgySwitchError::Io)?;
        if contents.trim().is_empty() {
            self.accounts = Vec::new();
            return Ok(());
        }
        let accounts: Vec<Account> = serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;
        self.accounts = accounts;
        Ok(())
    }

    /// Flush accounts to disk atomically.
    /// Uses compact JSON for fast writes (called every 10s by daemon).
    /// Writes to a temp file first, then renames to prevent corruption on crash.
    pub async fn flush(&self) -> Result<(), AgySwitchError> {
        ensure_config_dir().map_err(AgySwitchError::Io)?;
        let contents = serde_json::to_string(&self.accounts).map_err(AgySwitchError::Json)?;

        // Atomic write: temp file → rename
        let tmp_path = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp_path, &contents).await.map_err(AgySwitchError::Io)?;
        tokio::fs::rename(&tmp_path, &self.path).await.map_err(AgySwitchError::Io)?;

        // Also write a backup copy (survives accidental deletion of main file)
        let backup_path = self.path.with_extension("json.bak");
        let _ = tokio::fs::write(&backup_path, &contents).await;

        Ok(())
    }

    /// Add a new account (deduplicates by email)
    pub async fn add(&mut self, account: Account) -> Result<(), AgySwitchError> {
        if self.accounts.iter().any(|a| a.email == account.email) {
            return Err(AgySwitchError::DuplicateAccount(account.email));
        }
        self.accounts.push(account);
        self.flush().await
    }

    /// Remove an account by id
    pub async fn remove(&mut self, id: Uuid) -> Result<(), AgySwitchError> {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.id != id);
        if self.accounts.len() == before {
            return Err(AgySwitchError::AccountNotFound(id.to_string()));
        }
        self.flush().await
    }

    /// Remove all accounts
    pub async fn remove_all(&mut self) -> Result<(), AgySwitchError> {
        self.accounts.clear();
        self.flush().await
    }

    /// Get account by id
    pub fn get(&self, id: Uuid) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Get account by email (case-insensitive)
    pub fn get_by_email(&self, email: &str) -> Option<&Account> {
        let email_lower = email.to_ascii_lowercase();
        self.accounts.iter().find(|a| a.email.to_ascii_lowercase() == email_lower)
    }

    /// List all accounts
    pub fn list(&self) -> &Vec<Account> {
        &self.accounts
    }

    /// List accounts sorted by quota percentage descending (100% at top), then A-Z.
    /// Rate-limited and disabled accounts go at the bottom.
    pub fn list_sorted(&self) -> Vec<Account> {
        let mut accounts: Vec<Account> = self.accounts.iter().cloned().collect();
        accounts.sort_by(|a, b| {
            let pa = sort_key(a);
            let pb = sort_key(b);
            pb.partial_cmp(&pa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.email.cmp(&b.email))
        });
        accounts
    }

    /// Mutable list access for updates
    pub fn list_mut(&mut self) -> &mut Vec<Account> {
        &mut self.accounts
    }

    /// Update an existing account (matched by id)
    pub async fn update(&mut self, account: Account) -> Result<(), AgySwitchError> {
        let index = self.accounts.iter().position(|a| a.id == account.id)
            .ok_or_else(|| AgySwitchError::AccountNotFound(account.id.to_string()))?;
        self.accounts[index] = account;
        self.flush().await
    }

    /// Number of accounts
    pub fn count(&self) -> usize {
        self.accounts.len()
    }

    /// Estimate total memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        size += std::mem::size_of::<Account>() * self.accounts.capacity();
        for account in &self.accounts {
            size += account.memory_usage();
        }
        size
    }

    /// Find next healthy (non-rate-limited, enabled) account given a current index.
    #[allow(dead_code)]
    pub fn next_available(&self, current_index: usize) -> Option<(usize, &Account)> {
        if self.accounts.is_empty() {
            return None;
        }
        let len = self.accounts.len();
        let start = current_index % len;
        for offset in 0..len {
            let idx = (start + offset) % len;
            let account = &self.accounts[idx];
            if !account.enabled {
                continue;
            }
            if account.is_rate_limited {
                if let Some(reset) = account.rate_limit_reset_at {
                    if reset > chrono::Utc::now() {
                        continue;
                    }
                }
            }
            return Some((idx, account));
        }
        None
    }

    /// Find next available account in sorted order (highest quota first, A-Z within tier),
    /// skipping the current account, disabled, rate-limited, and exhausted ones.
    pub fn next_available_by_id(&self, current_id: Uuid) -> Option<(usize, Account)> {
        let sorted = self.list_sorted();
        for account in &sorted {
            if account.id == current_id {
                continue;
            }
            if !account.enabled {
                continue;
            }
            if account.is_rate_limited {
                if let Some(reset) = account.rate_limit_reset_at {
                    if reset > chrono::Utc::now() {
                        continue;
                    }
                }
            }
            // Skip accounts with exhausted quota
            if let Some(quota) = &account.quota {
                if !quota.models.is_empty() {
                    let all_exhausted = quota.models.iter().all(|m| {
                        m.remaining_fraction.map_or(false, |f| f <= 0.0)
                    });
                    if all_exhausted {
                        continue;
                    }
                }
            }
            return Some((0, account.clone()));
        }
        None
    }
}

/// Sort key for accounts: higher = better quota.
/// - Active rate limited: -3.0
/// - Disabled: -2.0
/// - Exhausted (all 0%): -1.0
/// - Healthy: actual min remaining fraction (0.0 to 1.0)
/// - No quota data: 0.0 (treat as empty)
fn sort_key(account: &Account) -> f64 {
    if !account.enabled {
        return -2.0;
    }
    if account.is_rate_limited {
        if let Some(reset) = account.rate_limit_reset_at {
            if reset > chrono::Utc::now() {
                return -3.0;
            }
        }
    }
    match &account.quota {
        Some(quota) if quota.models.is_empty() => 0.0,
        Some(quota) => {
            let min_frac = quota
                .models
                .iter()
                .filter_map(|m| m.remaining_fraction)
                .fold(f64::INFINITY, f64::min);
            if min_frac.is_infinite() {
                0.0
            } else {
                min_frac
            }
        }
        None => 0.0,
    }
}
