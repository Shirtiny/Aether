use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogEndpoint, StoredProviderCatalogProvider,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Describes how an authoritative catalog mutation affects an already-bound
/// official Codex WebSocket connection.
///
/// Keep the ordering from least to most restrictive. Unknown configuration
/// fields deliberately remain in the hard projection and therefore fail
/// closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogMutationImpact {
    SelectionOnly,
    Drain,
    HardFence,
}

impl CatalogMutationImpact {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::SelectionOnly => "selection_only",
            Self::Drain => "drain",
            Self::HardFence => "hard_fence",
        }
    }
}

/// Provider `pool_advanced` fields that only affect future selection or pool
/// policy. They may drain a binding after the current terminal, but cannot
/// alter the bytes or route of the already-frozen provider execution.
///
/// This is intentionally an allow-list. New/unknown fields stay in the hard
/// projection until their semantics are reviewed.
const PROVIDER_DRAIN_POOL_ADVANCED_KEYS: &[&str] = &[
    "account_self_check_concurrency",
    "account_self_check_enabled",
    "account_self_check_interval_minutes",
    "active_probe_target_count",
    "active_probe_target_percent",
    "anonymous_avoidance_enabled",
    "avoid_anonymous",
    "avoid_anonymous_requests",
    "avoid_anonymous_requests_enabled",
    "codex_quota_exhaustion_basis",
    "codex_quota_weekly_basis",
    "collateral_avoidance_enabled",
    "cost_limit_per_key_tokens",
    "cost_soft_threshold_percent",
    "cost_window_seconds",
    "global_priority",
    "health_policy_enabled",
    "latency_sample_limit",
    "latency_window_seconds",
    "load_threshold_percent",
    "lru_enabled",
    "overload_cooldown_seconds",
    "pool_score_rules",
    "pool_score_weights",
    "pool_sticky_collateral_avoidance_enabled",
    "probe_concurrency",
    "probing_active_target_count",
    "probing_active_target_percent",
    "probing_enabled",
    "probing_target_count",
    "probing_target_percent",
    "proactive_refresh_seconds",
    "rate_limit_cooldown_seconds",
    "recent_refresh_seconds",
    "schedule_mode",
    "scheduling_mode",
    "scheduling_presets",
    "score_fallback_scan_limit",
    "score_rules",
    "score_top_n",
    "score_weights",
    "scoring_weights",
    "self_check_concurrency",
    "self_check_enabled",
    "self_check_interval_minutes",
    "skip_exhausted_accounts",
    "sticky_account_collateral_avoidance_enabled",
    "sticky_collateral_avoidance_enabled",
    "sticky_session_ttl_seconds",
    "stream_timeout_cooldown_seconds",
    "stream_timeout_threshold",
    "stream_timeout_window_seconds",
    "unschedulable_rules",
];

#[derive(Debug, PartialEq)]
struct ProviderHardProjection<'a> {
    provider_type: &'a str,
    is_active: bool,
    keep_priority_on_conversion: bool,
    enable_format_conversion: bool,
    concurrent_limit: Option<i32>,
    max_retries: Option<i32>,
    proxy: &'a Option<Value>,
    request_timeout_secs: Option<f64>,
    stream_first_byte_timeout_secs: Option<f64>,
    stream_idle_timeout_secs: Option<f64>,
    config: Option<Value>,
}

#[derive(Debug, PartialEq)]
struct ProviderDrainProjection {
    provider_priority: i32,
    billing_type: Option<String>,
    monthly_quota_usd: Option<f64>,
    monthly_used_usd: Option<f64>,
    quota_reset_day: Option<u64>,
    quota_last_reset_at_unix_secs: Option<u64>,
    quota_expires_at_unix_secs: Option<u64>,
    pool_advanced: Option<Value>,
}

#[derive(Debug, PartialEq)]
struct ProviderSelectionProjection<'a> {
    name: &'a str,
    description: &'a Option<String>,
    website: &'a Option<String>,
    created_at_unix_ms: Option<u64>,
    updated_at_unix_secs: Option<u64>,
}

fn split_provider_config(config: Option<&Value>) -> (Option<Value>, Option<Value>) {
    let Some(Value::Object(config)) = config else {
        return (config.cloned(), None);
    };
    let mut hard = config.clone();
    let mut drain = Map::new();

    if let Some(Value::Object(pool_advanced)) = hard.get_mut("pool_advanced") {
        let mut drain_pool = Map::new();
        for key in PROVIDER_DRAIN_POOL_ADVANCED_KEYS {
            if let Some(value) = pool_advanced.remove(*key) {
                drain_pool.insert((*key).to_string(), value);
            }
        }
        if pool_advanced.is_empty() {
            hard.remove("pool_advanced");
        }
        if !drain_pool.is_empty() {
            drain.insert("pool_advanced".to_string(), Value::Object(drain_pool));
        }
    }

    (
        (!hard.is_empty()).then_some(Value::Object(hard)),
        (!drain.is_empty()).then_some(Value::Object(drain)),
    )
}

fn provider_hard_projection(
    provider: &StoredProviderCatalogProvider,
) -> ProviderHardProjection<'_> {
    let (config, _) = split_provider_config(provider.config.as_ref());
    ProviderHardProjection {
        provider_type: provider.provider_type.trim(),
        is_active: provider.is_active,
        keep_priority_on_conversion: provider.keep_priority_on_conversion,
        enable_format_conversion: provider.enable_format_conversion,
        concurrent_limit: provider.concurrent_limit,
        max_retries: provider.max_retries,
        proxy: &provider.proxy,
        request_timeout_secs: provider.request_timeout_secs,
        stream_first_byte_timeout_secs: provider.stream_first_byte_timeout_secs,
        stream_idle_timeout_secs: provider.stream_idle_timeout_secs,
        config,
    }
}

fn provider_drain_projection(provider: &StoredProviderCatalogProvider) -> ProviderDrainProjection {
    let (_, pool_advanced) = split_provider_config(provider.config.as_ref());
    ProviderDrainProjection {
        provider_priority: provider.provider_priority,
        billing_type: provider.billing_type.clone(),
        monthly_quota_usd: provider.monthly_quota_usd,
        monthly_used_usd: provider.monthly_used_usd,
        quota_reset_day: provider.quota_reset_day,
        quota_last_reset_at_unix_secs: provider.quota_last_reset_at_unix_secs,
        quota_expires_at_unix_secs: provider.quota_expires_at_unix_secs,
        pool_advanced,
    }
}

fn provider_selection_projection(
    provider: &StoredProviderCatalogProvider,
) -> ProviderSelectionProjection<'_> {
    ProviderSelectionProjection {
        name: provider.name.trim(),
        description: &provider.description,
        website: &provider.website,
        created_at_unix_ms: provider.created_at_unix_ms,
        updated_at_unix_secs: provider.updated_at_unix_secs,
    }
}

pub(crate) fn classify_provider_update(
    before: &StoredProviderCatalogProvider,
    after: &StoredProviderCatalogProvider,
) -> Option<CatalogMutationImpact> {
    if provider_hard_projection(before) != provider_hard_projection(after) {
        return Some(CatalogMutationImpact::HardFence);
    }
    if provider_drain_projection(before) != provider_drain_projection(after) {
        return Some(CatalogMutationImpact::Drain);
    }
    if provider_selection_projection(before) != provider_selection_projection(after) {
        return Some(CatalogMutationImpact::SelectionOnly);
    }
    // A future StoredProviderCatalogProvider field that is not explicitly
    // projected above must fail closed until its bound-session semantics are
    // reviewed.
    if before != after {
        return Some(CatalogMutationImpact::HardFence);
    }
    None
}

#[derive(Debug, PartialEq)]
struct EndpointHardProjection<'a> {
    provider_id: &'a str,
    api_format: &'a str,
    api_family: &'a Option<String>,
    endpoint_kind: &'a Option<String>,
    is_active: bool,
    base_url: &'a str,
    header_rules: &'a Option<Value>,
    body_rules: &'a Option<Value>,
    max_retries: Option<i32>,
    custom_path: &'a Option<String>,
    config: &'a Option<Value>,
    format_acceptance_config: &'a Option<Value>,
    proxy: &'a Option<Value>,
}

#[derive(Debug, PartialEq)]
struct EndpointSelectionProjection {
    created_at_unix_ms: Option<u64>,
    updated_at_unix_secs: Option<u64>,
}

fn endpoint_hard_projection(
    endpoint: &StoredProviderCatalogEndpoint,
) -> EndpointHardProjection<'_> {
    EndpointHardProjection {
        provider_id: endpoint.provider_id.trim(),
        api_format: endpoint.api_format.trim(),
        api_family: &endpoint.api_family,
        endpoint_kind: &endpoint.endpoint_kind,
        is_active: endpoint.is_active,
        base_url: endpoint.base_url.trim(),
        header_rules: &endpoint.header_rules,
        body_rules: &endpoint.body_rules,
        max_retries: endpoint.max_retries,
        custom_path: &endpoint.custom_path,
        config: &endpoint.config,
        format_acceptance_config: &endpoint.format_acceptance_config,
        proxy: &endpoint.proxy,
    }
}

pub(crate) fn classify_endpoint_update(
    before: &StoredProviderCatalogEndpoint,
    after: &StoredProviderCatalogEndpoint,
) -> Option<CatalogMutationImpact> {
    if endpoint_hard_projection(before) != endpoint_hard_projection(after) {
        return Some(CatalogMutationImpact::HardFence);
    }
    if before.health_score != after.health_score {
        return Some(CatalogMutationImpact::Drain);
    }
    let before_selection = EndpointSelectionProjection {
        created_at_unix_ms: before.created_at_unix_ms,
        updated_at_unix_secs: before.updated_at_unix_secs,
    };
    let after_selection = EndpointSelectionProjection {
        created_at_unix_ms: after.created_at_unix_ms,
        updated_at_unix_secs: after.updated_at_unix_secs,
    };
    if before_selection != after_selection {
        return Some(CatalogMutationImpact::SelectionOnly);
    }
    if before != after {
        return Some(CatalogMutationImpact::HardFence);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(config: Value) -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            "provider-1".to_string(),
            "Codex".to_string(),
            None,
            "codex".to_string(),
        )
        .expect("provider")
        .with_transport_fields(
            true,
            false,
            false,
            Some(8),
            Some(2),
            None,
            Some(600.0),
            Some(20.0),
            Some(config),
        )
    }

    fn endpoint() -> StoredProviderCatalogEndpoint {
        StoredProviderCatalogEndpoint::new(
            "endpoint-1".to_string(),
            "provider-1".to_string(),
            "openai:responses".to_string(),
            Some("openai".to_string()),
            Some("responses".to_string()),
            true,
        )
        .expect("endpoint")
        .with_transport_fields(
            "https://chatgpt.com/backend-api/codex".to_string(),
            None,
            None,
            Some(2),
            None,
            None,
            None,
            None,
        )
        .expect("transport fields")
    }

    #[test]
    fn scheduling_preset_change_drains_instead_of_hard_fencing() {
        let before = provider(json!({
            "pool_advanced": {
                "scheduling_presets": [
                    {"preset": "lru", "enabled": true},
                    {"preset": "no_weight", "enabled": false}
                ],
                "codex_client_headers": {"enabled": true}
            },
            "risk_control_session_avoidance": {"mode": "block"}
        }));
        let after = provider(json!({
            "pool_advanced": {
                "scheduling_presets": [
                    {"preset": "lru", "enabled": false},
                    {"preset": "no_weight", "enabled": true}
                ],
                "codex_client_headers": {"enabled": true}
            },
            "risk_control_session_avoidance": {"mode": "block"}
        }));

        assert_eq!(
            classify_provider_update(&before, &after),
            Some(CatalogMutationImpact::Drain)
        );
    }

    #[test]
    fn unknown_or_execution_provider_config_change_is_hard() {
        let before = provider(json!({"pool_advanced": {"scheduling_presets": []}}));
        let mut after = before.clone();
        after.config = Some(json!({
            "pool_advanced": {
                "scheduling_presets": [],
                "future_execution_switch": true
            }
        }));

        assert_eq!(
            classify_provider_update(&before, &after),
            Some(CatalogMutationImpact::HardFence)
        );
    }

    #[test]
    fn client_header_profile_change_is_hard() {
        let before = provider(json!({
            "pool_advanced": {"codex_client_headers": {"enabled": true}}
        }));
        let after = provider(json!({
            "pool_advanced": {"codex_client_headers": {"enabled": false}}
        }));

        assert_eq!(
            classify_provider_update(&before, &after),
            Some(CatalogMutationImpact::HardFence)
        );
    }

    #[test]
    fn display_and_timestamp_change_does_not_touch_bound_sessions() {
        let before = provider(json!({"pool_advanced": {"lru_enabled": true}}));
        let mut after = before.clone();
        after.name = "Renamed".to_string();
        after.description = Some("description".to_string());
        after.updated_at_unix_secs = Some(123);

        assert_eq!(
            classify_provider_update(&before, &after),
            Some(CatalogMutationImpact::SelectionOnly)
        );
    }

    #[test]
    fn endpoint_transport_change_is_hard_and_health_change_drains() {
        let before = endpoint();
        let mut hard = before.clone();
        hard.base_url = "https://example.com/backend-api/codex".to_string();
        assert_eq!(
            classify_endpoint_update(&before, &hard),
            Some(CatalogMutationImpact::HardFence)
        );

        let mut drain = before.clone();
        drain.health_score = 0.5;
        assert_eq!(
            classify_endpoint_update(&before, &drain),
            Some(CatalogMutationImpact::Drain)
        );

        let mut selection_only = before.clone();
        selection_only.updated_at_unix_secs = Some(123);
        assert_eq!(
            classify_endpoint_update(&before, &selection_only),
            Some(CatalogMutationImpact::SelectionOnly)
        );
    }
}
