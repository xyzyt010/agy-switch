use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub label: Option<String>,
    pub credential: OAuthCredential,
    pub quota: Option<crate::quota::models::QuotaSnapshot>,
    pub added_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub is_rate_limited: bool,
    pub rate_limit_reset_at: Option<DateTime<Utc>>,
    pub enabled: bool,
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
