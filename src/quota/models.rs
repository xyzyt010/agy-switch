use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub models: Vec<ModelQuota>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuota {
    pub model_id: String,
    pub display_name: String,
    /// 0.0 = exhausted, 1.0 = full quota (from Google fetchAvailableModels API)
    pub remaining_fraction: Option<f64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub is_exhausted: bool,
}

impl Default for QuotaSnapshot {
    fn default() -> Self {
        QuotaSnapshot {
            fetched_at: Utc::now(),
            models: Vec::new(),
        }
    }
}
