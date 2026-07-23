use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OAuthCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expiry: DateTime<Utc>,
    pub project_id: Option<String>,
    pub managed_project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub email: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub credential: OAuthCredential,
    pub quota: Option<crate::quota::models::QuotaSnapshot>,
    #[serde(default)]
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub is_rate_limited: bool,
    pub rate_limit_reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub enabled: bool,
}

/// Official accounts.json entry format — different field names/types from Account.
/// Used only for deserialization from the official `{"accounts":[...]}` file.
#[derive(Deserialize)]
pub struct OfficialAccountEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
}

impl Account {
    /// Create an Account from an official accounts.json entry (with only email/id/name).
    pub fn from_official(entry: &OfficialAccountEntry) -> Option<Self> {
        let email = entry.email.as_deref()?.to_string();
        if email.is_empty() {
            return None;
        }
        let id = entry.id.as_deref()
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);
        Some(Account {
            id,
            email,
            label: entry.name.clone(),
            credential: OAuthCredential::default(),
            quota: None,
            added_at: Utc::now(),
            last_used_at: None,
            is_rate_limited: false,
            rate_limit_reset_at: None,
            enabled: !entry.disabled.unwrap_or(false),
        })
    }
}

impl Account {
    /// Estimate memory usage in bytes for this account
    pub fn memory_usage(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        size += self.email.len();
        size += self.credential.access_token.len();
        size += self.credential.refresh_token.len();
        if let Some(ref label) = self.label {
            size += label.len();
        }
        if let Some(ref pid) = self.credential.project_id {
            size += pid.len();
        }
        if let Some(ref mpid) = self.credential.managed_project_id {
            size += mpid.len();
        }
        if let Some(ref quota) = self.quota {
            for model in &quota.models {
                size += model.model_id.len();
                size += model.display_name.len();
            }
            size += std::mem::size_of::<crate::quota::models::ModelQuota>() * quota.models.capacity();
            size += std::mem::size_of::<crate::quota::models::QuotaSnapshot>();
        }
        size
    }
}
