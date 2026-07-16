use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::AgySwitchError;
use crate::quota::models::{ModelQuota, QuotaSnapshot};
use crate::store::account::OAuthCredential;

/// Cloud Code API endpoints (fallback order: daily → prod)
const ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];

/// Known Antigravity models (id, display_name)
pub const KNOWN_MODELS: &[(&str, &str)] = &[
    ("gemini-3.1-pro-low", "Gemini 3.1 Pro (Low)"),
    ("gemini-3.1-pro-high", "Gemini 3.1 Pro (High)"),
    ("gemini-pro-agent", "Gemini 3.1 Pro (Agent)"),
    ("gemini-3.1-flash-image", "Gemini 3.1 Flash Image"),
    ("gemini-2.5-flash", "Gemini 3.1 Flash Lite"),
    ("gemini-3.5-flash-extra-low", "Gemini 3.5 Flash (Low)"),
    ("gemini-2.5-flash-lite", "Gemini 3.1 Flash Lite"),
    ("gemini-3.5-flash-low", "Gemini 3.5 Flash (Medium)"),
    ("gemini-2.5-flash-thinking", "Gemini 3.1 Flash Lite"),
    ("gemini-3.1-flash-lite", "Gemini 3.1 Flash Lite"),
    ("gemini-3-flash-agent", "Gemini 3.5 Flash (High)"),
    ("gemini-3-flash", "Gemini 3 Flash"),
    ("gemini-2.5-pro", "Gemini 2.5 Pro"),
    ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    ("claude-opus-4-6-thinking", "Claude Opus 4.6 (Thinking)"),
    ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)"),
];

/// API response structures
#[derive(Deserialize, Debug)]
struct FetchModelsResponse {
    models: Option<std::collections::HashMap<String, ModelData>>,
}

#[derive(Deserialize, Debug)]
struct ModelData {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "quotaInfo")]
    quota_info: Option<QuotaInfo>,
}

#[derive(Deserialize, Debug)]
struct QuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

/// Fetch available models with quota info from the Google Cloud Code API.
/// This is the SAME endpoint the Antigravity Claude Proxy uses.
/// POST /v1internal:fetchAvailableModels with OAuth token + project ID.
async fn fetch_available_models(
    access_token: &str,
    project_id: Option<&str>,
) -> Result<FetchModelsResponse, AgySwitchError> {
    let client = crate::http::client();
    let body = if let Some(pid) = project_id {
        format!(r#"{{"project":"{}"}}"#, pid)
    } else {
        "{}".to_string()
    };

    for endpoint in ENDPOINTS {
        let url = format!("{}/v1internal:fetchAvailableModels", endpoint);
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("User-Agent", "antigravity/1.0 windows/amd64")
            .header("X-Client-Name", "antigravity")
            .header("x-goog-api-client", "gl-node/18.18.2 fire/0.8.6 grpc/1.10.x")
            .body(body.clone())
            .send()
            .await
            .map_err(AgySwitchError::Http)?;

        if resp.status().is_success() {
            return resp.json().await.map_err(AgySwitchError::Http);
        }

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(AgySwitchError::RateLimited(endpoint.to_string()));
        }

        let text = resp.text().await.unwrap_or_default();
        let _ = (endpoint, status, &text); // suppressed — was leaking into TUI
        return Err(AgySwitchError::OAuthFailed(format!("fetchAvailableModels {}: {}", status, &text[..text.len().min(200)])));
    }

    Err(AgySwitchError::OAuthFailed(
        "fetchAvailableModels failed on all endpoints".to_string(),
    ))
}

/// Retrieve actual per-model quota using the retrieveUserQuota endpoint.
/// This endpoint returns REAL quota data unlike fetchAvailableModels which always returns 1.0.
async fn retrieve_user_quota(
    access_token: &str,
    project_id: Option<&str>,
) -> Result<serde_json::Value, AgySwitchError> {
    let client = crate::http::client();
    let body = if let Some(pid) = project_id {
        format!(r#"{{"project":"{}"}}"#, pid)
    } else {
        "{}".to_string()
    };

    for endpoint in ENDPOINTS {
        let url = format!("{}/v1internal:retrieveUserQuota", endpoint);
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("User-Agent", "antigravity/1.0 windows/amd64")
            .body(body.clone())
            .send()
            .await
            .map_err(AgySwitchError::Http)?;

        if resp.status().is_success() {
            let text = resp.text().await.map_err(AgySwitchError::Http)?;
            let data: serde_json::Value = serde_json::from_str(&text).map_err(AgySwitchError::Json)?;
            return Ok(data);
        }

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(AgySwitchError::RateLimited(endpoint.to_string()));
        }

        let text = resp.text().await.unwrap_or_default();
        let _ = (endpoint, status, &text); // suppressed
        return Err(AgySwitchError::OAuthFailed(format!("retrieveUserQuota {}: {}", status, &text[..text.len().min(200)])));
    }

    Err(AgySwitchError::OAuthFailed(
        "retrieveUserQuota failed on all endpoints".to_string(),
    ))
}

/// Load code assist data (plan info, credits, project ID)
async fn load_code_assist(
    access_token: &str,
) -> Result<serde_json::Value, AgySwitchError> {
    let client = crate::http::client();
    let body = r#"{"metadata":{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}}"#;

    for endpoint in ENDPOINTS {
        let url = format!("{}/v1internal:loadCodeAssist", endpoint);
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("User-Agent", "antigravity/1.0 windows/amd64")
            .body(body)
            .send()
            .await
            .map_err(AgySwitchError::Http)?;

        if resp.status().is_success() {
            let text = resp.text().await.map_err(AgySwitchError::Http)?;
            let data: serde_json::Value = serde_json::from_str(&text).map_err(AgySwitchError::Json)?;
            return Ok(data);
        }

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(AgySwitchError::RateLimited(endpoint.to_string()));
        }

        let text = resp.text().await.unwrap_or_default();
        let _ = (endpoint, status, &text); // suppressed
        return Err(AgySwitchError::OAuthFailed(format!("loadCodeAssist {}: {}", status, &text[..text.len().min(200)])));
    }

    Err(AgySwitchError::OAuthFailed(
        "loadCodeAssist failed on all endpoints".to_string(),
    ))
}

/// Fetch model quotas for an account. Returns a QuotaSnapshot with real data
/// from the Google API (remainingFraction + resetTime per model).
/// Tries retrieveUserQuota first (actual usage), falls back to fetchAvailableModels.
pub async fn get_model_quotas(
    credential: &OAuthCredential,
    email: &str,
) -> Result<QuotaSnapshot, AgySwitchError> {
    // Ensure token is fresh
    let fresh = crate::auth::token_refresh::ensure_fresh_with_email(credential, Some(email)).await?;

    // Try managed_project_id first, fall back to project_id
    let mut project = credential.managed_project_id.as_deref()
        .or(credential.project_id.as_deref())
        .map(|s| s.to_string());

    // If no project_id, try loadCodeAssist to get it
    if project.is_none() {
        if let Ok(data) = load_code_assist(&fresh.access_token).await {
            if let Some(pid) = data.get("cloudaicompanionProject").and_then(|p| p.as_str()) {
                project = Some(pid.to_string());
            }
        }
    }

    // Try retrieveUserQuota first (returns actual usage data)
    match retrieve_user_quota(&fresh.access_token, project.as_deref()).await {
        Ok(quota_data) => {
            // Parse the retrieveUserQuota response
            // Format: { "buckets": [ { "modelId": "...", "remainingFraction": 0.8, "resetTime": "...", "tokenType": "WTUS" } ] }
            // WTUS = Weekly Total Usage Units (real quota), REQUESTS = always 1.0 (fake)
            if let Some(buckets) = quota_data.get("buckets").and_then(|b| b.as_array()) {
                let mut models = Vec::new();
                // First pass: collect WTUS entries (real data), fallback to REQUESTS
                let mut wtus_models: std::collections::HashMap<String, (Option<f64>, Option<DateTime<Utc>>)> = std::collections::HashMap::new();
                let mut request_models: std::collections::HashMap<String, (Option<f64>, Option<DateTime<Utc>>)> = std::collections::HashMap::new();

                for bucket in buckets {
                    let model_id = bucket.get("modelId")
                        .or_else(|| bucket.get("model"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    if model_id.is_empty() { continue; }
                    let token_type = bucket.get("tokenType").and_then(|t| t.as_str()).unwrap_or("");
                    let remaining_fraction = bucket.get("remainingFraction").and_then(|r| r.as_f64());
                    let reset_at = bucket.get("resetTime")
                        .and_then(|r| r.as_str())
                        .and_then(|rt| rt.parse::<DateTime<Utc>>().ok());
                    let entry = (remaining_fraction, reset_at);
                    if token_type == "WTUS" {
                        wtus_models.insert(model_id, entry);
                    } else {
                        request_models.insert(model_id, entry);
                    }
                }

                // Merge: prefer WTUS data over REQUESTS data
                let all_ids: Vec<String> = wtus_models.keys().chain(request_models.keys()).cloned().collect();
                let mut seen = std::collections::HashSet::new();
                for model_id in all_ids {
                    if !seen.insert(model_id.clone()) { continue; }
                    let lower = model_id.to_lowercase();
                    if !lower.contains("claude") && !lower.contains("gemini") { continue; }

                    let (remaining_fraction, reset_at) = wtus_models.get(&model_id)
                        .or_else(|| request_models.get(&model_id))
                        .cloned()
                        .unwrap_or((None, None));

                    let effective_fraction = remaining_fraction.or_else(|| {
                        if reset_at.is_some() { Some(0.0) } else { None }
                    });
                    let is_exhausted = effective_fraction.map_or(false, |f| f <= 0.0);
                    let display_name = KNOWN_MODELS
                        .iter()
                        .find(|(id, _)| *id == model_id)
                        .map(|(_, name)| name.to_string())
                        .unwrap_or_else(|| model_id.clone());
                    models.push(ModelQuota {
                        model_id,
                        display_name,
                        remaining_fraction: effective_fraction,
                        reset_at,
                        is_exhausted,
                    });
                }
                models.sort_by(|a, b| {
                    let a_frac = a.remaining_fraction.unwrap_or(1.0);
                    let b_frac = b.remaining_fraction.unwrap_or(1.0);
                    a_frac.partial_cmp(&b_frac).unwrap_or(std::cmp::Ordering::Equal)
                });
                if !models.is_empty() {
                    return Ok(QuotaSnapshot { fetched_at: Utc::now(), models });
                }
            }
            // If buckets parsing failed, fall through to fetchAvailableModels
        }
        Err(_) => {
            // Suppressed: previously eprintln'd here, leaked into TUI
        }
    }

    // Fallback: fetchAvailableModels (returns 1.0 for everything)
    let data = fetch_available_models(&fresh.access_token, project.as_deref()).await?;

    let models_map = match data.models {
        Some(m) => m,
        None => return Ok(QuotaSnapshot::default()),
    };

    let mut models = Vec::new();

    for (model_id, model_data) in &models_map {
        // Only include Claude and Gemini models
        let lower = model_id.to_lowercase();
        if !lower.contains("claude") && !lower.contains("gemini") {
            continue;
        }

        let remaining_fraction = model_data
            .quota_info
            .as_ref()
            .and_then(|qi| qi.remaining_fraction);

        let reset_at = model_data
            .quota_info
            .as_ref()
            .and_then(|qi| qi.reset_time.as_deref())
            .and_then(|rt| rt.parse::<DateTime<Utc>>().ok());

        // remainingFraction missing but resetTime present → quota exhausted (0%)
        let effective_fraction = remaining_fraction.or_else(|| {
            if reset_at.is_some() {
                Some(0.0)
            } else {
                None
            }
        });

        let is_exhausted = effective_fraction.map_or(false, |f| f <= 0.0);

        // Use display name from API, or fall back to known models list
        let display_name = model_data
            .display_name
            .clone()
            .or_else(|| {
                KNOWN_MODELS
                    .iter()
                    .find(|(id, _)| *id == model_id)
                    .map(|(_, name)| name.to_string())
            })
            .unwrap_or_else(|| model_id.clone());

        models.push(ModelQuota {
            model_id: model_id.clone(),
            display_name,
            remaining_fraction: effective_fraction,
            reset_at,
            is_exhausted,
        });
    }

    // Sort by remaining_fraction ascending (exhausted first)
    models.sort_by(|a, b| {
        let a_frac = a.remaining_fraction.unwrap_or(1.0);
        let b_frac = b.remaining_fraction.unwrap_or(1.0);
        a_frac.partial_cmp(&b_frac).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(QuotaSnapshot {
        fetched_at: Utc::now(),
        models,
    })
}
