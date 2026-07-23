use std::path::PathBuf;
use std::time::SystemTime;

use crate::config::ensure_config_dir;
use crate::error::AgySwitchError;
use crate::store::account::Account;
use uuid::Uuid;

/// In-memory JSON-based file store for accounts.
/// All data lives in a Vec<Account> in RAM. Flushed to disk atomically.
pub struct FileStore {
    accounts: Vec<Account>,
    path: PathBuf,
    /// Track mtime to skip re-reads when the file hasn't changed.
    last_loaded_mtime: Option<SystemTime>,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        FileStore {
            accounts: Vec::new(),
            path,
            last_loaded_mtime: None,
        }
    }

    /// Load accounts from disk. Skips re-read if file mtime hasn't changed since last load.
    pub async fn load(&mut self) -> Result<(), AgySwitchError> {
        if !tokio::fs::try_exists(&self.path).await.unwrap_or(false) {
            return Ok(());
        }
        // Skip re-read if file hasn't been modified since last load
        if let Ok(metadata) = tokio::fs::metadata(&self.path).await {
            if let Ok(mtime) = metadata.modified() {
                if self.last_loaded_mtime == Some(mtime) {
                    return Ok(());
                }
                self.last_loaded_mtime = Some(mtime);
            }
        }
        let raw = tokio::fs::read(&self.path).await.map_err(AgySwitchError::Io)?;
        // Reject files with null bytes (corrupted) — treat as empty
        if raw.windows(1).any(|w| w[0] == 0) {
            let _ = tokio::fs::rename(&self.path, &self.path.with_extension("json.corrupted")).await;
            self.accounts = Vec::new();
            return Ok(());
        }
        let contents = String::from_utf8(raw).map_err(|e| {
            AgySwitchError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        if contents.trim().is_empty() {
            self.accounts = Vec::new();
            return Ok(());
        }
        // Support both formats: {"accounts": [...]} (wrapped) and [...] (bare array)
        let parsed: serde_json::Value = serde_json::from_str(&contents).map_err(AgySwitchError::Json)?;
        let accounts_arr = parsed
            .as_array()
            .cloned()
            .or_else(|| parsed.get("accounts").and_then(|v| v.as_array()).cloned())
            .ok_or_else(|| AgySwitchError::Json(serde_json::from_str::<serde_json::Value>("null").unwrap_err()))?;
        let mut accounts = Vec::with_capacity(accounts_arr.len());
        for v in accounts_arr {
            accounts.push(serde_json::from_value(v).map_err(AgySwitchError::Json)?);
        }
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

    /// List accounts sorted by:
    ///   1. 100% first, descending percentage (101% → 1%)
    ///   2. Exhausted/rate-limited accounts sorted by soonest reset time FIRST
    ///      (least time until refresh → most time until refresh)
    ///   3. Disabled accounts
    ///   Within each tier, ties are broken A-Z by email.
    pub fn list_sorted(&self) -> Vec<Account> {
        let mut accounts: Vec<Account> = self.accounts.iter().cloned().collect();
        accounts.sort_by(|a, b| sort_comparator(a, b));
        accounts
    }

    /// Mutable list access for updates
    pub fn list_mut(&mut self) -> &mut Vec<Account> {
        &mut self.accounts
    }

    /// Update an existing account (matched by id) and flush to disk.
    pub async fn update(&mut self, account: Account) -> Result<(), AgySwitchError> {
        let index = self.accounts.iter().position(|a| a.id == account.id)
            .ok_or_else(|| AgySwitchError::AccountNotFound(account.id.to_string()))?;
        self.accounts[index] = account;
        self.flush().await
    }

    /// Update an existing account in memory only (no disk flush).
    /// Call `flush()` once after a batch of updates to write everything at once.
    pub fn update_no_flush(&mut self, account: Account) {
        if let Some(index) = self.accounts.iter().position(|a| a.id == account.id) {
            self.accounts[index] = account;
        }
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
        let now = chrono::Utc::now();
        let sorted = self.list_sorted();
        for account in &sorted {
            if account.id == current_id {
                continue;
            }
            if !account.enabled {
                continue;
            }
            // Skip HTTP-429 rate-limited accounts whose reset is still in the future.
            if account.is_rate_limited
                && account
                    .rate_limit_reset_at
                    .map(|t| t > now)
                    .unwrap_or(false)
            {
                continue;
            }
            // Skip accounts whose quota shows any exhausted model with a future reset
            // (these are effectively rate limited per Cloud Code semantics).
            if let Some(quota) = &account.quota {
                if quota.models.iter().any(|m| {
                    m.is_exhausted
                        && m.reset_at.map(|t| t > now).unwrap_or(false)
                }) {
                    continue;
                }
                // Also skip accounts where every model is exhausted (even without a future reset),
                // since there's no immediate capacity available.
                if !quota.models.is_empty()
                    && quota.models.iter().all(|m| {
                        m.remaining_fraction.map(|f| f <= 0.0).unwrap_or(false)
                    })
                {
                    continue;
                }
            }
            return Some((0, account.clone()));
        }
        None
    }
}

/// Sort comparator implementing the requested account ordering:
///   1. Disabled accounts last.
///   2. 100% quota first, descending percentage.
///   3. Exhausted / rate-limited accounts sorted by least-to-most reset time
///      (soonest refresh first). Accounts without a reset time go at the very
///      bottom of this tier.
///   4. Within each tier, A-Z by email.
fn sort_comparator(a: &Account, b: &Account) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let now = chrono::Utc::now();

    // Comparison result says whether `a` should come BEFORE `b`
    // (i.e. this returns Less if a sorts higher in the list).

    // --- Disabled: always last
    if a.enabled != b.enabled {
        return if a.enabled { Less } else { Greater };
    }
    if !a.enabled {
        return a.email.cmp(&b.email);
    }

    // Compute the min remaining fraction across models for each account
    let min_frac = |acc: &Account| -> Option<f64> {
        acc.quota.as_ref().and_then(|q| {
            q.models
                .iter()
                .filter_map(|m| m.remaining_fraction)
                .fold(None, |acc: Option<f64>, v: f64| {
                    Some(acc.map(|c: f64| c.min(v)).unwrap_or(v))
                })
        })
    };

    // Earliest future reset_at among exhausted models (in seconds from now; None if N/A)
    let soonest_reset_secs = |acc: &Account| -> Option<i64> {
        let q = acc.quota.as_ref()?;
        let mut soonest: Option<i64> = None;
        for m in &q.models {
            if !m.is_exhausted {
                continue;
            }
            if let Some(reset) = m.reset_at {
                let secs = (reset - now).num_seconds();
                if secs > 0 {
                    soonest = Some(match soonest {
                        Some(s) if s < secs => s,
                        _ => secs,
                    });
                }
            }
        }
        // Also consider the explicit HTTP 429 flag's reset_at
        if acc.is_rate_limited {
            if let Some(reset) = acc.rate_limit_reset_at {
                let secs = (reset - now).num_seconds();
                if secs > 0 {
                    soonest = Some(match soonest {
                        Some(s) if s < secs => s,
                        _ => secs,
                    });
                }
            }
        }
        soonest
    };

    let af = min_frac(a);
    let bf = min_frac(b);

    let a_min_zero = af.map(|f| f <= 0.0).unwrap_or(false);
    let b_min_zero = bf.map(|f| f <= 0.0).unwrap_or(false);

    // --- Account is "effectively rate limited" if it has a future reset time OR
    //     (its 429 flag is set with a future reset_at). Without a future reset,
    //     exhausted accounts sort below healthy but above accounts with no
    //     "real" reset signature.
    let a_reset_secs = soonest_reset_secs(a);
    let b_reset_secs = soonest_reset_secs(b);
    let a_rl = a_reset_secs.is_some();
    let b_rl = b_reset_secs.is_some();

    // --- Tiering rule (high → low in the list):
    //   Healthy (frac > 0)         : sorted by fraction DESC
    //   Unknown quota / no models   : below healthy but above exhausted-without-reset
    //   Exhausted with future reset : soonest reset FIRST (least M → most M)
    //   Exhausted without future reset (reset unknown) : below that
    if a_min_zero && b_min_zero {
        if a_rl && b_rl {
            // Both rate-limited: least reset time first
            return a_reset_secs
                .cmp(&b_reset_secs)
                .then_with(|| a.email.cmp(&b.email));
        }
        if a_rl {
            return Less; // a has reset info, b doesn't → a first
        }
        if b_rl {
            return Greater;
        }
        return a.email.cmp(&b.email); // neither has reset info
    }

    if a_min_zero {
        // a is exhausted but b has quota → b healthy comes first
        return Greater;
    }
    if b_min_zero {
        return Less;
    }

    // Both nonzero quota (or unknown): sort by frac DESC, unknown (None) below known.
    match (af, bf) {
        (Some(av), Some(bv)) => bv
            .partial_cmp(&av)
            .unwrap_or(Equal)
            .then_with(|| a.email.cmp(&b.email)),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => a.email.cmp(&b.email),
    }
}

/// Legacy sort key — retained for next_available()'s filter-based logic.
/// Higher = better quota. Only used for that purpose; list_sorted() uses
/// sort_comparator directly.
#[allow(dead_code)]
fn sort_key(account: &Account) -> f64 {
    if !account.enabled {
        return -2.0;
    }
    let now = chrono::Utc::now();
    if account.is_rate_limited
        && account
            .rate_limit_reset_at
            .map(|t| t > now)
            .unwrap_or(false)
    {
        return -3.0;
    }
    match &account.quota {
        Some(quota) if quota.models.is_empty() => 0.0,
        Some(quota) => {
            let (mut min_frac, mut has_future_reset) = (f64::INFINITY, false);
            for m in &quota.models {
                if let Some(f) = m.remaining_fraction {
                    min_frac = min_frac.min(f);
                }
                if m.is_exhausted
                    && m.reset_at
                        .map(|t| t > now)
                        .unwrap_or(false)
                {
                    has_future_reset = true;
                }
            }
            if has_future_reset {
                -3.0
            } else if min_frac.is_infinite() {
                0.0
            } else {
                min_frac
            }
        }
        None => 0.0,
    }
}
