use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::error::AgySwitchError;

/// Persisted application state
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct AppState {
    pub enabled: bool,
    pub mode: SwitchMode,
    pub active_account_id: Option<Uuid>,
    pub auto_switch_index: usize,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SwitchMode {
    Auto,
    Manual,
}

impl Default for SwitchMode {
    fn default() -> Self {
        SwitchMode::Manual
    }
}

/// Get the base directory for AGY-SWITCH configuration
pub fn app_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agy-switch")
}

/// Paths for persisted files
pub fn state_path() -> PathBuf {
    app_config_dir().join("state.json")
}

pub fn accounts_path() -> PathBuf {
    app_config_dir().join("accounts.json")
}

/// Ensure the base config directory exists
pub fn ensure_config_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(app_config_dir())
}

/// Load state from disk, returning defaults if file doesn't exist or is empty
pub async fn load_state() -> Result<AppState, AgySwitchError> {
    let path = state_path();
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        let contents = tokio::fs::read_to_string(&path).await.map_err(AgySwitchError::Io)?;
        if contents.trim().is_empty() {
            return Ok(AppState::default());
        }
        serde_json::from_str(&contents).map_err(AgySwitchError::Json)
    } else {
        Ok(AppState::default())
    }
}

/// Save state to disk
pub async fn save_state(state: &AppState) -> Result<(), AgySwitchError> {
    ensure_config_dir().map_err(AgySwitchError::Io)?;
    let contents = serde_json::to_string_pretty(state).map_err(AgySwitchError::Json)?;
    tokio::fs::write(state_path(), contents).await.map_err(AgySwitchError::Io)?;
    Ok(())
}
