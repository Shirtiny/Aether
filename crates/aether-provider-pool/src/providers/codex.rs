use std::collections::BTreeMap;

use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogEndpoint;
use aether_pool_core::PoolSchedulingPreset;
use serde_json::{Map, Value};

use crate::capability::ProviderPoolCapabilities;
use crate::provider::{
    provider_pool_endpoint_format_matches, provider_pool_matching_endpoint, ProviderPoolAdapter,
    ProviderPoolMemberInput,
};
use crate::quota::{
    provider_pool_current_unix_secs, provider_pool_json_bool, provider_pool_json_f64,
    provider_pool_metadata_bucket, provider_pool_quota_snapshot_exhausted_decision,
    provider_pool_reset_deadline_elapsed, provider_pool_timestamp_unix_secs,
};
use crate::quota_refresh::ProviderPoolQuotaRequestSpec;

pub const CODEX_WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub const CODEX_RESET_CREDIT_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume";
const CODEX_RESET_CREDIT_USER_AGENT: &str =
    "Codex Desktop/0.140.0-alpha.2 (Mac OS 26.5.0; arm64) unknown (Codex Desktop; 26.609.41114)";
const PLACEHOLDER_API_KEY: &str = "__placeholder__";

#[derive(Debug, Clone, Default)]
pub struct CodexProviderPoolAdapter;

impl ProviderPoolAdapter for CodexProviderPoolAdapter {
    fn provider_type(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> ProviderPoolCapabilities {
        ProviderPoolCapabilities {
            plan_tier: true,
            quota_reset: true,
            quota_refresh: true,
        }
    }

    fn default_scheduling_presets(&self) -> Vec<PoolSchedulingPreset> {
        vec![PoolSchedulingPreset {
            preset: "recent_refresh".to_string(),
            enabled: true,
            mode: None,
        }]
    }

    fn quota_exhausted(&self, input: &ProviderPoolMemberInput<'_>) -> bool {
        if let Some(exhausted) = codex_quota_exhausted_from_status_snapshot(
            input.key,
            input.provider_type,
            input.codex_quota_basis,
        ) {
            return exhausted;
        }
        provider_pool_metadata_bucket(input.key.upstream_metadata.as_ref(), input.provider_type)
            .is_some_and(|bucket| {
                quota_exhausted_from_bucket_with_basis(bucket, input.codex_quota_basis)
            })
    }

    fn quota_refresh_endpoint(
        &self,
        endpoints: &[StoredProviderCatalogEndpoint],
        include_inactive: bool,
    ) -> Option<StoredProviderCatalogEndpoint> {
        provider_pool_matching_endpoint(endpoints, include_inactive, |endpoint| {
            provider_pool_endpoint_format_matches(endpoint, "openai:responses")
        })
    }

    fn quota_refresh_missing_endpoint_message(&self) -> String {
        "找不到有效的 openai:responses 端点".to_string()
    }
}

pub fn build_codex_pool_quota_request(
    key_id: &str,
    resolved_oauth_auth: Option<(String, String)>,
    decrypted_api_key: Option<&str>,
    auth_config: Option<&Value>,
) -> Result<ProviderPoolQuotaRequestSpec, String> {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "application/json".to_string());

    if let Some((name, value)) = resolved_oauth_auth {
        headers.insert(name.to_ascii_lowercase(), value);
    } else {
        let decrypted_key = decrypted_api_key.unwrap_or_default().trim();
        if decrypted_key.is_empty() || decrypted_key == PLACEHOLDER_API_KEY {
            return Err("缺少 OAuth 认证信息，请先授权/刷新 Token".to_string());
        }
        headers.insert(
            "authorization".to_string(),
            format!("Bearer {decrypted_key}"),
        );
    }

    let oauth_plan_type = auth_config
        .and_then(|value| value.get("plan_type"))
        .and_then(Value::as_str)
        .and_then(|value| crate::plan::normalize_provider_plan_tier(value, "codex"));
    let oauth_account_id = auth_config
        .and_then(|value| value.get("account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if oauth_account_id.is_some() && oauth_plan_type.as_deref() != Some("free") {
        headers.insert(
            "chatgpt-account-id".to_string(),
            oauth_account_id.unwrap_or_default().to_string(),
        );
    }

    Ok(ProviderPoolQuotaRequestSpec {
        request_id: format!("codex-quota:{key_id}"),
        provider_name: "codex".to_string(),
        quota_kind: "codex".to_string(),
        method: "GET".to_string(),
        url: CODEX_WHAM_USAGE_URL.to_string(),
        headers,
        content_type: None,
        json_body: None,
        client_api_format: "openai:responses".to_string(),
        provider_api_format: "openai:responses".to_string(),
        model_name: Some("codex-wham-usage".to_string()),
        accept_invalid_certs: false,
    })
}

pub fn build_codex_pool_reset_credit_request(
    key_id: &str,
    redeem_request_id: String,
    resolved_oauth_auth: (String, String),
    auth_config: Option<&Value>,
) -> ProviderPoolQuotaRequestSpec {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "application/json".to_string());
    headers.insert(
        "user-agent".to_string(),
        CODEX_RESET_CREDIT_USER_AGENT.to_string(),
    );
    headers.insert(
        resolved_oauth_auth.0.to_ascii_lowercase(),
        resolved_oauth_auth.1,
    );

    let oauth_account_id = auth_config
        .and_then(|value| value.get("account_id"))
        .or_else(|| auth_config.and_then(|value| value.get("chatgpt_account_id")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(account_id) = oauth_account_id {
        headers.insert("chatgpt-account-id".to_string(), account_id.to_string());
    }

    ProviderPoolQuotaRequestSpec {
        request_id: format!("codex-reset-credit:{key_id}"),
        provider_name: "codex".to_string(),
        quota_kind: "codex_reset_credit".to_string(),
        method: "POST".to_string(),
        url: CODEX_RESET_CREDIT_URL.to_string(),
        headers,
        content_type: Some("application/json".to_string()),
        json_body: Some(serde_json::json!({
            "redeem_request_id": redeem_request_id,
        })),
        client_api_format: "openai:responses".to_string(),
        provider_api_format: "openai:responses".to_string(),
        model_name: Some("codex-reset-credit".to_string()),
        accept_invalid_certs: false,
    }
}

fn codex_window_reset_elapsed(bucket: &Map<String, Value>, prefix: &str) -> bool {
    let Some(now_unix_secs) = provider_pool_current_unix_secs() else {
        return false;
    };
    let mut window = Map::new();
    for (target, source) in [
        ("reset_at", format!("{prefix}_reset_at")),
        ("next_reset_at", format!("{prefix}_next_reset_at")),
        ("reset_seconds", format!("{prefix}_reset_seconds")),
        (
            "reset_after_seconds",
            format!("{prefix}_reset_after_seconds"),
        ),
    ] {
        if let Some(value) = bucket.get(source.as_str()) {
            window.insert(target.to_string(), value.clone());
        }
    }
    provider_pool_reset_deadline_elapsed(
        &window,
        provider_pool_timestamp_unix_secs(bucket.get("updated_at")),
        now_unix_secs,
    )
}

fn codex_window_used_percent_exhausted(bucket: &Map<String, Value>, prefix: &str) -> bool {
    let used_percent_key = format!("{prefix}_used_percent");
    provider_pool_json_f64(bucket.get(used_percent_key.as_str()))
        .is_some_and(|value| value >= 100.0 && !codex_window_reset_elapsed(bucket, prefix))
}

fn normalized_codex_quota_basis(value: Option<&str>) -> &'static str {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("5h" | "five_hour" | "five_hours" | "5_hour" | "5_hours") => "5h",
        _ => "weekly",
    }
}

fn codex_quota_exhausted_from_status_snapshot(
    key: &aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey,
    provider_type: &str,
    basis: Option<&str>,
) -> Option<bool> {
    if normalized_codex_quota_basis(basis) != "5h" {
        return provider_pool_quota_snapshot_exhausted_decision(key, provider_type);
    }

    let quota_snapshot = key
        .status_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("quota"))
        .and_then(Value::as_object)?;
    let window = quota_snapshot
        .get("windows")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_object)
        .find(|window| {
            window
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(|code| code.trim().eq_ignore_ascii_case("5h"))
        })?;

    Some(provider_pool_quota_window_active_exhausted(
        window,
        quota_snapshot,
    ))
}

fn provider_pool_quota_window_active_exhausted(
    window: &Map<String, Value>,
    quota_snapshot: &Map<String, Value>,
) -> bool {
    let exhausted = provider_pool_json_bool(window.get("is_exhausted"))
        .or_else(|| {
            provider_pool_json_f64(window.get("used_ratio")).map(|value| value >= 1.0 - 1e-6)
        })
        .unwrap_or(false);
    if !exhausted {
        return false;
    }
    let Some(now) = provider_pool_current_unix_secs() else {
        return true;
    };
    let snapshot_observed_at = provider_pool_timestamp_unix_secs(quota_snapshot.get("observed_at"))
        .or_else(|| provider_pool_timestamp_unix_secs(quota_snapshot.get("updated_at")));
    !provider_pool_reset_deadline_elapsed(window, snapshot_observed_at, now)
}

fn quota_exhausted_from_bucket_with_basis(
    bucket: &Map<String, Value>,
    basis: Option<&str>,
) -> bool {
    if provider_pool_json_bool(bucket.get("credits_unlimited")) == Some(true) {
        return false;
    }
    if normalized_codex_quota_basis(basis) == "5h" {
        return codex_window_used_percent_exhausted(bucket, "secondary");
    }
    let has_window_data = provider_pool_json_f64(bucket.get("primary_used_percent")).is_some()
        || provider_pool_json_f64(bucket.get("secondary_used_percent")).is_some();
    if !has_window_data && provider_pool_json_bool(bucket.get("has_credits")) == Some(false) {
        return true;
    }
    codex_window_used_percent_exhausted(bucket, "primary")
        || codex_window_used_percent_exhausted(bucket, "secondary")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_quota_uses_weekly_basis_by_default() {
        let bucket = json!({
            "primary_used_percent": 100.0,
            "secondary_used_percent": 10.0
        })
        .as_object()
        .cloned()
        .expect("bucket should be object");

        assert!(quota_exhausted_from_bucket_with_basis(
            &bucket,
            Some("weekly")
        ));
        assert!(quota_exhausted_from_bucket_with_basis(&bucket, None));
    }

    #[test]
    fn codex_quota_can_follow_five_hour_basis() {
        let bucket = json!({
            "primary_used_percent": 100.0,
            "secondary_used_percent": 10.0
        })
        .as_object()
        .cloned()
        .expect("bucket should be object");

        assert!(!quota_exhausted_from_bucket_with_basis(&bucket, Some("5h")));
        assert!(!quota_exhausted_from_bucket_with_basis(
            &bucket,
            Some("five_hour")
        ));
    }
}
