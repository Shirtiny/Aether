use std::collections::BTreeMap;

use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogEndpoint;
use aether_pool_core::PoolSchedulingPreset;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::capability::ProviderPoolCapabilities;
use crate::provider::{
    provider_pool_endpoint_format_matches, provider_pool_matching_endpoint, ProviderPoolAdapter,
    ProviderPoolMemberInput,
};
use crate::quota::{
    provider_pool_current_unix_secs, provider_pool_metadata_bucket,
    provider_pool_quota_snapshot_exhausted_decision, provider_pool_timestamp_unix_secs,
};
use crate::quota_refresh::ProviderPoolQuotaRequestSpec;

/// Fallback probe root. Callers resolve the account's real chat base URL and
/// pass it in; a subscription grant is served by the Grok CLI chat-proxy rather
/// than by the official API root.
pub const XAI_DEFAULT_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const XAI_QUOTA_PROBE_MODEL: &str = "grok-4.3";
const PLACEHOLDER_API_KEY: &str = "__placeholder__";

/// Pseudo model name carried by the billing lookup so it is attributable in
/// execution telemetry without naming a real model.
pub const XAI_BILLING_PROBE_MODEL: &str = "grok-billing";
/// Cent ceilings xAI publishes for the paid subscription tiers. The billing
/// payload never names the plan, so it is recovered from the monthly limit.
const XAI_SUPER_GROK_LIMIT_CENTS: f64 = 15_000.0;
const XAI_SUPER_GROK_HEAVY_LIMIT_CENTS: f64 = 150_000.0;

/// The two windows xAI bills against. They are served by the same path with
/// different query parameters and must be fetched separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokBillingWindow {
    Weekly,
    Monthly,
}

impl GrokBillingWindow {
    fn path(self) -> &'static str {
        match self {
            Self::Weekly => "/billing?format=credits",
            Self::Monthly => "/billing",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GrokProviderPoolAdapter;

impl ProviderPoolAdapter for GrokProviderPoolAdapter {
    fn provider_type(&self) -> &'static str {
        "grok"
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
        if let Some(exhausted) =
            provider_pool_quota_snapshot_exhausted_decision(input.key, input.provider_type)
        {
            return exhausted;
        }
        provider_pool_metadata_bucket(input.key.upstream_metadata.as_ref(), input.provider_type)
            .is_some_and(grok_quota_bucket_exhausted)
    }

    fn quota_refresh_endpoint(
        &self,
        endpoints: &[StoredProviderCatalogEndpoint],
        include_inactive: bool,
    ) -> Option<StoredProviderCatalogEndpoint> {
        provider_pool_matching_endpoint(endpoints, include_inactive, |endpoint| {
            provider_pool_endpoint_format_matches(endpoint, "openai:responses")
        })
        .or_else(|| {
            provider_pool_matching_endpoint(endpoints, include_inactive, |endpoint| {
                provider_pool_endpoint_format_matches(endpoint, "openai:chat")
            })
        })
    }

    fn quota_refresh_missing_endpoint_message(&self) -> String {
        "找不到有效的 Grok openai:responses 端点".to_string()
    }
}

fn insert_grok_authorization(
    headers: &mut BTreeMap<String, String>,
    resolved_oauth_auth: Option<(String, String)>,
    decrypted_api_key: Option<&str>,
) -> Result<(), String> {
    if let Some((name, value)) = resolved_oauth_auth {
        headers.insert(name.trim().to_ascii_lowercase(), value);
        return Ok(());
    }
    let access_token = decrypted_api_key.unwrap_or_default().trim();
    if access_token.is_empty() || access_token == PLACEHOLDER_API_KEY {
        return Err("缺少 xAI OAuth 认证信息，请先授权/刷新 Token".to_string());
    }
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    );
    Ok(())
}

fn resolve_grok_base_url(base_url: &str) -> &str {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        XAI_DEFAULT_BASE_URL
    } else {
        base_url
    }
}

pub fn build_grok_pool_quota_request(
    key_id: &str,
    base_url: &str,
    resolved_oauth_auth: Option<(String, String)>,
    decrypted_api_key: Option<&str>,
) -> Result<ProviderPoolQuotaRequestSpec, String> {
    let mut headers = BTreeMap::from([
        ("accept".to_string(), "application/json".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ]);
    insert_grok_authorization(&mut headers, resolved_oauth_auth, decrypted_api_key)?;

    let base_url = resolve_grok_base_url(base_url);
    Ok(ProviderPoolQuotaRequestSpec {
        request_id: format!("grok-quota:{key_id}"),
        provider_name: "grok".to_string(),
        quota_kind: "grok:xai_headers".to_string(),
        method: "POST".to_string(),
        url: format!("{base_url}/responses"),
        headers,
        content_type: Some("application/json".to_string()),
        json_body: Some(json!({
            "model": XAI_QUOTA_PROBE_MODEL,
            "input": ".",
            "max_output_tokens": 1,
            "store": false,
        })),
        client_api_format: "openai:responses".to_string(),
        provider_api_format: "openai:responses".to_string(),
        model_name: Some(XAI_QUOTA_PROBE_MODEL.to_string()),
        accept_invalid_certs: false,
    })
}

/// Build the xAI billing lookup for one of the two windows xAI reports
/// separately. Unlike the `/responses` probe this is a plain GET, so refreshing
/// a paid account's quota costs no model quota at all.
pub fn build_grok_billing_request(
    key_id: &str,
    base_url: &str,
    resolved_oauth_auth: Option<(String, String)>,
    decrypted_api_key: Option<&str>,
    window: GrokBillingWindow,
) -> Result<ProviderPoolQuotaRequestSpec, String> {
    let mut headers = BTreeMap::from([("accept".to_string(), "application/json".to_string())]);
    insert_grok_authorization(&mut headers, resolved_oauth_auth, decrypted_api_key)?;

    let base_url = resolve_grok_base_url(base_url);
    let window_code = window.code();
    Ok(ProviderPoolQuotaRequestSpec {
        request_id: format!("grok-billing-{window_code}:{key_id}"),
        provider_name: "grok".to_string(),
        quota_kind: "grok:xai_billing".to_string(),
        method: "GET".to_string(),
        url: format!("{base_url}{}", window.path()),
        headers,
        content_type: None,
        json_body: None,
        client_api_format: "openai:responses".to_string(),
        provider_api_format: "openai:responses".to_string(),
        model_name: Some(XAI_BILLING_PROBE_MODEL.to_string()),
        accept_invalid_certs: false,
    })
}

/// Parse the quota headers emitted by the official xAI API into the common
/// provider quota snapshot shape consumed by the generic pool scheduler.
pub fn parse_grok_quota_headers(
    headers: &BTreeMap<String, String>,
    status_code: u16,
    observed_at_unix_secs: u64,
) -> Option<Value> {
    let requests = parse_quota_window(headers, "requests", observed_at_unix_secs);
    let tokens = parse_quota_window(headers, "tokens", observed_at_unix_secs);
    let retry_after_seconds = header_value(headers, &["retry-after"])
        .and_then(|raw| parse_retry_after(raw, observed_at_unix_secs));
    let subscription_tier =
        header_value(headers, &["xai-subscription-tier", "x-subscription-tier"])
            .map(ToOwned::to_owned);
    let entitlement_status =
        header_value(headers, &["xai-entitlement-status", "x-entitlement-status"])
            .map(ToOwned::to_owned);

    let mut windows = Vec::new();
    if let Some(window) = requests {
        windows.push(window);
    }
    if let Some(window) = tokens {
        windows.push(window);
    }
    let headers_observed = !windows.is_empty()
        || retry_after_seconds.is_some()
        || subscription_tier.is_some()
        || entitlement_status.is_some();
    // A status code by itself is not a quota observation. Persisting a
    // headerless 429 as exhausted would block the account forever because no
    // reset deadline is available; a headerless 5xx would also erase the last
    // known-good quota snapshot. Let the runtime cooldown path own those
    // transient failures instead.
    if !headers_observed {
        return None;
    }

    let mut snapshot = json!({
        "version": 2,
        "provider_type": "grok",
        "code": "ok",
        "exhausted": false,
        "usage_ratio": Value::Null,
        "reset_at": Value::Null,
        "reset_seconds": Value::Null,
        "updated_at": observed_at_unix_secs,
        "observed_at": observed_at_unix_secs,
        "status_code": status_code,
        "headers_observed": headers_observed,
        "retry_after_seconds": retry_after_seconds,
        "subscription_tier": subscription_tier,
        "plan_type": subscription_tier,
        "entitlement_status": entitlement_status,
        "windows": windows,
    });
    recompute_grok_quota_aggregate(&mut snapshot);
    Some(snapshot)
}

fn parse_quota_window(
    headers: &BTreeMap<String, String>,
    dimension: &str,
    observed_at_unix_secs: u64,
) -> Option<Value> {
    let limit = header_value(headers, &[&format!("x-ratelimit-limit-{dimension}")])
        .and_then(parse_non_negative_f64);
    let remaining = header_value(headers, &[&format!("x-ratelimit-remaining-{dimension}")])
        .and_then(parse_non_negative_f64);
    let reset_at = header_value(headers, &[&format!("x-ratelimit-reset-{dimension}")])
        .and_then(parse_reset_at);
    if limit.is_none() && remaining.is_none() && reset_at.is_none() {
        return None;
    }
    let used_ratio = limit.zip(remaining).and_then(|(limit, remaining)| {
        (limit > 0.0).then(|| ((limit - remaining).max(0.0) / limit).clamp(0.0, 1.0))
    });
    let is_exhausted = remaining.is_some_and(|remaining| remaining <= 0.0)
        || used_ratio.is_some_and(|ratio| ratio >= 1.0 - 1e-6);
    let reset_seconds = reset_at
        .filter(|reset_at| *reset_at >= observed_at_unix_secs)
        .map(|reset_at| reset_at.saturating_sub(observed_at_unix_secs));
    let remaining_ratio = limit
        .zip(remaining)
        .and_then(|(limit, remaining)| (limit > 0.0).then(|| (remaining / limit).clamp(0.0, 1.0)));
    let used_value = limit
        .zip(remaining)
        .map(|(limit, remaining)| (limit - remaining).max(0.0));
    let (label, unit) = if dimension.eq_ignore_ascii_case("tokens") {
        ("Token", "tokens")
    } else {
        ("请求", "count")
    };
    Some(json!({
        "code": dimension,
        "label": label,
        "scope": "account",
        "unit": unit,
        "limit": limit,
        "remaining": remaining,
        "limit_value": limit,
        "remaining_value": remaining,
        "used_value": used_value,
        "used_ratio": used_ratio,
        "remaining_ratio": remaining_ratio,
        "is_exhausted": is_exhausted,
        "reset_at": reset_at,
        "reset_seconds": reset_seconds,
    }))
}

fn window_code(window: &Value) -> Option<String> {
    window
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
}

fn merge_non_null_object_fields(target: &mut Map<String, Value>, incoming: &Map<String, Value>) {
    for (key, value) in incoming {
        if !value.is_null() {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn normalize_merged_grok_quota_window(window: &mut Value, observed_at: u64) {
    let Some(window) = window.as_object_mut() else {
        return;
    };
    let code = window
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Only the header-observed dimensions are normalized here: they arrive as
    // limit/remaining with everything else derived from them. Billing windows
    // are already derived, and re-deriving would overwrite their authoritative
    // exhaustion verdict with a naive remaining<=0 test.
    if !matches!(code.as_str(), "requests" | "tokens") {
        return;
    }
    let limit = window
        .get("limit")
        .or_else(|| window.get("limit_value"))
        .and_then(Value::as_f64);
    let remaining = window
        .get("remaining")
        .or_else(|| window.get("remaining_value"))
        .and_then(Value::as_f64);
    if let Some(limit) = limit {
        window.insert("limit".to_string(), json!(limit));
        window.insert("limit_value".to_string(), json!(limit));
    }
    if let Some(remaining) = remaining {
        window.insert("remaining".to_string(), json!(remaining));
        window.insert("remaining_value".to_string(), json!(remaining));
    }
    if let Some((limit, remaining)) = limit.zip(remaining) {
        let remaining_ratio = (limit > 0.0).then(|| (remaining / limit).clamp(0.0, 1.0));
        let used_ratio = remaining_ratio.map(|ratio| (1.0 - ratio).clamp(0.0, 1.0));
        window.insert(
            "used_value".to_string(),
            json!((limit - remaining).max(0.0)),
        );
        window.insert("used_ratio".to_string(), json!(used_ratio));
        window.insert("remaining_ratio".to_string(), json!(remaining_ratio));
        window.insert(
            "is_exhausted".to_string(),
            json!(remaining <= 0.0 || used_ratio.is_some_and(|ratio| ratio >= 1.0 - 1e-6)),
        );
    }
    if let Some(reset_at) = window.get("reset_at").and_then(Value::as_u64) {
        window.insert(
            "reset_seconds".to_string(),
            json!((reset_at >= observed_at).then(|| reset_at.saturating_sub(observed_at))),
        );
    }
    window.insert("scope".to_string(), json!("account"));
    if code == "tokens" {
        window.insert("label".to_string(), json!("Token"));
        window.insert("unit".to_string(), json!("tokens"));
    } else {
        window.insert("label".to_string(), json!("请求"));
        window.insert("unit".to_string(), json!("count"));
    }
}

/// Merge independently emitted xAI request/token observations without letting
/// a partial header set erase the last known state for the other dimension.
pub fn merge_grok_quota_snapshot(previous: Option<&Value>, incoming: &Value) -> Value {
    let Some(incoming_object) = incoming.as_object() else {
        return incoming.clone();
    };
    let mut merged = previous
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for (key, value) in incoming_object {
        if key == "windows" {
            continue;
        }
        // Identity metadata is sparse and should survive a response which does
        // not repeat it. Observation-local fields, including retry-after, are
        // authoritative even when null and must not leak into the next result.
        let preserve_when_null = matches!(
            key.as_str(),
            "subscription_tier" | "plan_type" | "entitlement_status"
        );
        if !value.is_null() || !preserve_when_null {
            merged.insert(key.clone(), value.clone());
        }
    }

    let mut windows = previous
        .and_then(|value| value.get("windows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(incoming_windows) = incoming_object.get("windows").and_then(Value::as_array) {
        for incoming_window in incoming_windows {
            let Some(code) = window_code(incoming_window) else {
                continue;
            };
            if let Some(existing) = windows
                .iter_mut()
                .find(|window| window_code(window).as_deref() == Some(code.as_str()))
            {
                if let (Some(existing), Some(incoming)) =
                    (existing.as_object_mut(), incoming_window.as_object())
                {
                    merge_non_null_object_fields(existing, incoming);
                } else {
                    *existing = incoming_window.clone();
                }
            } else {
                windows.push(incoming_window.clone());
            }
        }
    }
    let observed_at = incoming_object
        .get("observed_at")
        .or_else(|| incoming_object.get("updated_at"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    for window in &mut windows {
        normalize_merged_grok_quota_window(window, observed_at);
    }
    // Regenerate the billing projection from the merged billing view rather
    // than merging it as if it were an upstream observation.
    windows.retain(|window| {
        window_code(window).is_none_or(|code| !is_grok_billing_window_code(&code))
    });
    windows.extend(grok_billing_quota_windows(merged.get("billing")));
    merged.insert("windows".to_string(), Value::Array(windows));

    let mut snapshot = Value::Object(merged);
    recompute_grok_quota_aggregate(&mut snapshot);
    snapshot
}

/// Normalize one `/billing` response body. xAI serves both windows from the
/// same shape and only the populated fields distinguish them, so the caller
/// does not have to say which window it fetched.
pub fn parse_grok_billing_payload(body: &Value, observed_at_unix_secs: u64) -> Option<Value> {
    let config = body.get("config")?.as_object()?;

    let credit_usage = config.get("creditUsagePercent").and_then(Value::as_f64);
    let period = config.get("currentPeriod").and_then(Value::as_object);
    let mut period_type = resolve_grok_billing_period_type(period);
    let billing_start = billing_string(config.get("billingPeriodStart"));
    let billing_end = billing_string(config.get("billingPeriodEnd"));
    let period_start = period
        .and_then(|period| billing_string(period.get("start")))
        .or_else(|| billing_start.clone());
    let period_end = period
        .and_then(|period| billing_string(period.get("end")))
        .or_else(|| billing_end.clone());

    let products = config
        .get("productUsage")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let product = billing_string(item.get("product"))?;
                    Some(json!({
                        "product": product,
                        "usage_percent": item.get("usagePercent").and_then(Value::as_f64),
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let monthly_limit = parse_grok_cent_value(config.get("monthlyLimit"));
    let used = parse_grok_cent_value(config.get("used"));
    // Overflow budgets. A spent weekly window is only a hard stop when neither
    // of these can absorb further spend.
    let on_demand_cap = parse_grok_cent_value(config.get("onDemandCap"));
    let on_demand_used = parse_grok_cent_value(config.get("onDemandUsed"));
    let prepaid_balance = parse_grok_cent_value(config.get("prepaidBalance"));
    // Spend beyond the subscription ceiling is billed separately, so the
    // in-plan figure is what the usage percentage is measured against.
    let included_used = used.map(|used| match monthly_limit {
        Some(limit) if limit > 0.0 => used.min(limit),
        _ => used,
    });
    let used_percent = monthly_limit
        .filter(|limit| *limit > 0.0)
        .zip(included_used)
        .map(|(limit, included_used)| (included_used / limit) * 100.0);

    let has_weekly = credit_usage.is_some() || period_type == "weekly" || !products.is_empty();
    let has_monthly =
        monthly_limit.is_some() || used.is_some() || (!has_weekly && billing_end.is_some());
    if !has_weekly && !has_monthly {
        return None;
    }

    let mut billing = Map::new();
    if has_weekly {
        if period_type == "unknown" {
            period_type = "weekly".to_string();
        }
        billing.insert("usage_percent".to_string(), json!(credit_usage));
        billing.insert("period_start".to_string(), json!(period_start));
        billing.insert("period_end".to_string(), json!(period_end));
        // The window's own bounds. Unlike the rate-limit headers, billing
        // always carries a deadline, so exhaustion can never latch forever;
        // the start lets local usage be counted over the same period.
        billing.insert(
            "period_start_unix".to_string(),
            json!(period_start.as_deref().and_then(parse_reset_at)),
        );
        billing.insert(
            "period_end_unix".to_string(),
            json!(period_end.as_deref().and_then(parse_reset_at)),
        );
        billing.insert("on_demand_cap_cents".to_string(), json!(on_demand_cap));
        billing.insert("on_demand_used_cents".to_string(), json!(on_demand_used));
        billing.insert("prepaid_balance_cents".to_string(), json!(prepaid_balance));
    } else {
        // Monthly-only payloads must not populate the weekly usage field: the
        // two windows measure different things and the UI renders the weekly
        // bar purely from `period_type`.
        period_type = "monthly".to_string();
        billing.insert("period_start".to_string(), json!(billing_start));
        billing.insert("period_end".to_string(), json!(billing_end));
    }
    billing.insert("period_type".to_string(), json!(period_type));
    billing.insert("product_usage".to_string(), json!(products));
    billing.insert("monthly_limit_cents".to_string(), json!(monthly_limit));
    billing.insert("used_cents".to_string(), json!(used));
    billing.insert("included_used_cents".to_string(), json!(included_used));
    billing.insert("used_percent".to_string(), json!(used_percent));
    billing.insert("billing_period_start".to_string(), json!(billing_start));
    billing.insert("billing_period_end".to_string(), json!(billing_end));
    billing.insert(
        "billing_period_start_unix".to_string(),
        json!(billing_start.as_deref().and_then(parse_reset_at)),
    );
    billing.insert(
        "billing_period_end_unix".to_string(),
        json!(billing_end.as_deref().and_then(parse_reset_at)),
    );
    billing.insert(
        "plan".to_string(),
        json!(resolve_grok_billing_plan(monthly_limit)),
    );
    billing.insert("observed_at".to_string(), json!(observed_at_unix_secs));
    Some(Value::Object(billing))
}

/// Merge the two independently fetched billing windows. A window that failed to
/// refresh keeps its previous value rather than being erased, so one flaky
/// request never downgrades a known-good billing view to "unknown".
pub fn merge_grok_billing_snapshot(
    previous: Option<&Value>,
    weekly: Option<&Value>,
    monthly: Option<&Value>,
    weekly_status_code: u16,
    monthly_status_code: u16,
    observed_at_unix_secs: u64,
) -> Option<Value> {
    if weekly.is_none() && monthly.is_none() && previous.is_none() {
        return None;
    }
    let mut merged = previous
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if let Some(weekly) = weekly.and_then(Value::as_object) {
        for field in [
            "period_type",
            "usage_percent",
            "period_start",
            "period_end",
            "period_start_unix",
            "period_end_unix",
            "on_demand_cap_cents",
            "on_demand_used_cents",
            "prepaid_balance_cents",
            "product_usage",
        ] {
            if let Some(value) = weekly.get(field) {
                merged.insert(field.to_string(), value.clone());
            }
        }
        merged.insert(
            "weekly_updated_at".to_string(),
            json!(observed_at_unix_secs),
        );
    }
    if let Some(monthly) = monthly.and_then(Value::as_object) {
        for field in [
            "monthly_limit_cents",
            "used_cents",
            "included_used_cents",
            "used_percent",
            "billing_period_start",
            "billing_period_end",
            "billing_period_start_unix",
            "billing_period_end_unix",
            "plan",
        ] {
            if let Some(value) = monthly.get(field) {
                merged.insert(field.to_string(), value.clone());
            }
        }
        if billing_string(merged.get("period_type")).is_none() {
            merged.insert("period_type".to_string(), json!("monthly"));
        }
        merged.insert(
            "monthly_updated_at".to_string(),
            json!(observed_at_unix_secs),
        );
    }

    let mut failed_windows = Vec::new();
    if weekly.is_none() {
        failed_windows.push("weekly");
    }
    if monthly.is_none() {
        failed_windows.push("monthly");
    }
    merged.insert("partial".to_string(), json!(!failed_windows.is_empty()));
    merged.insert("failed_windows".to_string(), json!(failed_windows));
    merged.insert("weekly_status_code".to_string(), json!(weekly_status_code));
    merged.insert(
        "monthly_status_code".to_string(),
        json!(monthly_status_code),
    );
    merged.insert("observed_at".to_string(), json!(observed_at_unix_secs));
    merged.insert("source".to_string(), json!("billing_probe"));
    Some(Value::Object(merged))
}

/// Whether billing answered with something that actually describes the
/// account's allowance. Free accounts get a successful but empty response, so
/// they fall through to the rate-limit header path instead.
pub fn grok_billing_has_authoritative_quota(billing: Option<&Value>) -> bool {
    let Some(billing) = billing.and_then(Value::as_object) else {
        return false;
    };
    billing
        .get("usage_percent")
        .and_then(Value::as_f64)
        .is_some()
        || billing
            .get("used_percent")
            .and_then(Value::as_f64)
            .is_some()
        || billing
            .get("monthly_limit_cents")
            .and_then(Value::as_f64)
            .is_some_and(|limit| limit > 0.0)
        || billing_string(billing.get("plan")).is_some()
}

/// Whether at least one billing endpoint returned a successful response.
///
/// This is deliberately weaker than `grok_billing_has_authoritative_quota`:
/// some xAI plans return HTTP 200 plus period boundaries, but omit numeric
/// usage and limits. Admin surfaces must still treat that as the billing view
/// instead of presenting static request/token ceilings as subscription quota.
pub fn grok_billing_has_successful_response(billing: Option<&Value>) -> bool {
    let Some(billing) = billing.and_then(Value::as_object) else {
        return false;
    };
    ["weekly_status_code", "monthly_status_code"]
        .into_iter()
        .filter_map(|field| billing.get(field).and_then(Value::as_u64))
        .any(|status| (200..300).contains(&status))
}

/// Project the billing view into the shared quota-window shape, so exhaustion,
/// utilisation, reset deadlines and rendering all run through the generic
/// machinery instead of grok-specific branches.
///
/// These windows are a projection rather than an observation: they are
/// regenerated from `billing` on every merge, so the two representations cannot
/// drift apart.
fn grok_billing_quota_windows(billing: Option<&Value>) -> Vec<Value> {
    let Some(billing) = billing.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut windows = Vec::new();

    if let Some(usage_percent) = billing.get("usage_percent").and_then(Value::as_f64) {
        let used_ratio = (usage_percent / 100.0).clamp(0.0, 1.0);
        // Spending the weekly credits is only a hard stop when neither overflow
        // budget can absorb more. A missing budget field counts as unknown
        // rather than zero: wrongly blocking a healthy key costs a full billing
        // period, while missing an exhausted one only repeats the 402 the
        // circuit breaker already handles.
        let mut headroom = false;
        let mut unknown_headroom = false;
        for field in ["on_demand_cap_cents", "prepaid_balance_cents"] {
            match billing.get(field).and_then(Value::as_f64) {
                Some(budget) if budget > 0.0 => headroom = true,
                Some(_) => {}
                None => unknown_headroom = true,
            }
        }
        // Credits are a percentage rather than a countable balance, so this
        // window carries ratios only.
        windows.push(json!({
            "code": "billing_weekly",
            "label": "周额度",
            "scope": "account",
            "unit": "percent",
            "used_ratio": used_ratio,
            "remaining_ratio": (1.0 - used_ratio).clamp(0.0, 1.0),
            "is_exhausted": used_ratio >= 1.0 - 1e-6 && !headroom && !unknown_headroom,
            "window_start_at": billing.get("period_start_unix").and_then(Value::as_u64),
            "reset_at": billing.get("period_end_unix").and_then(Value::as_u64),
        }));
    }

    if let Some(limit_cents) = billing
        .get("monthly_limit_cents")
        .and_then(Value::as_f64)
        .filter(|limit| *limit > 0.0)
    {
        let used_cents = billing
            .get("included_used_cents")
            .or_else(|| billing.get("used_cents"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0);
        // Reported in dollars so the unit matches the value.
        let limit = limit_cents / 100.0;
        let used = (used_cents / 100.0).min(limit);
        let remaining = (limit - used).max(0.0);
        let used_ratio = (used / limit).clamp(0.0, 1.0);
        windows.push(json!({
            "code": "billing_monthly",
            "label": "月额度",
            "scope": "account",
            "unit": "usd",
            "limit_value": limit,
            "used_value": used,
            "remaining_value": remaining,
            "used_ratio": used_ratio,
            "remaining_ratio": (1.0 - used_ratio).clamp(0.0, 1.0),
            "is_exhausted": remaining <= 0.0,
            "window_start_at": billing
                .get("billing_period_start_unix")
                .and_then(Value::as_u64),
            "reset_at": billing.get("billing_period_end_unix").and_then(Value::as_u64),
        }));
    }

    windows
}

fn is_grok_billing_window_code(code: &str) -> bool {
    code.starts_with("billing_")
}

fn billing_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// xAI reports cent amounts as `{"val": N}`, a bare number, or a string
/// depending on the field and the account tier.
fn parse_grok_cent_value(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if let Some(object) = value.as_object() {
        return parse_grok_cent_value(object.get("val"));
    }
    if let Some(number) = value.as_f64() {
        return number.is_finite().then_some(number);
    }
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

fn resolve_grok_billing_plan(monthly_limit_cents: Option<f64>) -> Option<&'static str> {
    // Tolerate float noise from the JSON round-trip.
    match monthly_limit_cents.map(f64::round) {
        Some(limit) if limit == XAI_SUPER_GROK_LIMIT_CENTS => Some("SuperGrok"),
        Some(limit) if limit == XAI_SUPER_GROK_HEAVY_LIMIT_CENTS => Some("SuperGrok Heavy"),
        _ => None,
    }
}

fn resolve_grok_billing_period_type(period: Option<&Map<String, Value>>) -> String {
    let raw = period
        .and_then(|period| billing_string(period.get("type")))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if raw.contains("weekly") {
        "weekly".to_string()
    } else if raw.contains("monthly") {
        "monthly".to_string()
    } else {
        "unknown".to_string()
    }
}

fn recompute_grok_quota_aggregate(snapshot: &mut Value) {
    let Some(snapshot) = snapshot.as_object_mut() else {
        return;
    };
    let observed_at = snapshot
        .get("observed_at")
        .or_else(|| snapshot.get("updated_at"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let status_code = snapshot
        .get("status_code")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(200);
    let retry_after_seconds = snapshot.get("retry_after_seconds").and_then(Value::as_u64);
    let windows = snapshot
        .get("windows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // A paid account's rate-limit headers are a static ceiling reporting zero
    // usage however much of the subscription is gone; the projected billing
    // windows carry the real figures. Both kinds are just windows here.
    let usage_ratio = windows
        .iter()
        .filter_map(|window| window.get("used_ratio").and_then(Value::as_f64))
        .reduce(f64::max);
    let active_exhausted_resets = windows
        .iter()
        .filter(|window| {
            window
                .get("is_exhausted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|window| window.get("reset_at").and_then(Value::as_u64))
        .filter(|reset_at| *reset_at > observed_at)
        .collect::<Vec<_>>();
    let retry_reset_at = (status_code == 429)
        .then_some(retry_after_seconds)
        .flatten()
        .map(|seconds| observed_at.saturating_add(seconds));
    let blocking_reset_at = active_exhausted_resets
        .into_iter()
        .chain(retry_reset_at)
        .max();
    let exhausted = blocking_reset_at.is_some();
    let informational_reset_at = windows
        .iter()
        .filter_map(|window| window.get("reset_at").and_then(Value::as_u64))
        .filter(|reset_at| *reset_at > observed_at)
        .min();
    let reset_at = blocking_reset_at.or(informational_reset_at);
    let reset_seconds = reset_at.map(|value| value.saturating_sub(observed_at));
    let code = match status_code {
        401 => "unauthorized",
        403 => "forbidden",
        500..=599 => "upstream_overloaded",
        _ if exhausted => "exhausted",
        _ => "ok",
    };

    snapshot.insert("usage_ratio".to_string(), json!(usage_ratio));
    snapshot.insert("exhausted".to_string(), json!(exhausted));
    snapshot.insert("reset_at".to_string(), json!(reset_at));
    snapshot.insert("reset_seconds".to_string(), json!(reset_seconds));
    snapshot.insert("code".to_string(), json!(code));
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        headers.iter().find_map(|(header_name, value)| {
            header_name
                .eq_ignore_ascii_case(name)
                .then(|| value.trim())
                .filter(|value| !value.is_empty())
        })
    })
}

fn parse_non_negative_f64(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn parse_reset_at(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Ok(mut value) = raw.parse::<u64>() {
        if value > 1_000_000_000_000 {
            value /= 1000;
        }
        return Some(value);
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .and_then(|value| u64::try_from(value.timestamp()).ok())
}

fn parse_retry_after(raw: &str, now_unix_secs: u64) -> Option<u64> {
    let raw = raw.trim();
    if let Ok(value) = raw.parse::<u64>() {
        return Some(value);
    }
    DateTime::parse_from_rfc2822(raw)
        .or_else(|_| DateTime::parse_from_rfc3339(raw))
        .ok()
        .and_then(|value| u64::try_from(value.with_timezone(&Utc).timestamp()).ok())
        .map(|deadline| deadline.saturating_sub(now_unix_secs))
}

fn grok_quota_bucket_exhausted(bucket: &Map<String, Value>) -> bool {
    // New snapshots make the aggregate decision explicit. In particular, an
    // exhausted window without a reset deadline is deliberately persisted as
    // `exhausted: false` so it cannot block the key forever.
    if let Some(exhausted) = bucket.get("exhausted").and_then(Value::as_bool) {
        return exhausted;
    }

    // Keep accepting legacy metadata snapshots which only stored per-window
    // exhaustion. Ignore a legacy window after its known reset deadline.
    let now_unix_secs = provider_pool_current_unix_secs();
    let observed_at = provider_pool_timestamp_unix_secs(
        bucket
            .get("observed_at")
            .or_else(|| bucket.get("updated_at")),
    );
    bucket
        .get("windows")
        .and_then(Value::as_array)
        .is_some_and(|windows| {
            windows.iter().filter_map(Value::as_object).any(|window| {
                window.get("is_exhausted").and_then(Value::as_bool) == Some(true)
                    && grok_quota_window_reset_at(window, observed_at)
                        .is_some_and(|reset_at| now_unix_secs.is_none_or(|now| reset_at > now))
            })
        })
}

fn grok_quota_window_reset_at(
    window: &Map<String, Value>,
    fallback_observed_at: Option<u64>,
) -> Option<u64> {
    provider_pool_timestamp_unix_secs(window.get("reset_at"))
        .or_else(|| provider_pool_timestamp_unix_secs(window.get("next_reset_at")))
        .or_else(|| {
            let seconds = window
                .get("reset_seconds")
                .or_else(|| window.get("reset_after_seconds"))
                .and_then(Value::as_f64)?;
            let observed_at = provider_pool_timestamp_unix_secs(window.get("observed_at"))
                .or_else(|| provider_pool_timestamp_unix_secs(window.get("updated_at")))
                .or(fallback_observed_at)?;
            (seconds >= 0.0).then(|| observed_at.saturating_add(seconds.ceil() as u64))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_official_xai_probe_request() {
        let spec = build_grok_pool_quota_request(
            "key-1",
            "https://api.x.ai/v1/",
            Some(("authorization".to_string(), "Bearer access".to_string())),
            None,
        )
        .expect("probe request should build");

        assert_eq!(spec.url, "https://api.x.ai/v1/responses");
        assert_eq!(spec.headers["authorization"], "Bearer access");
        assert_eq!(spec.json_body.as_ref().unwrap()["model"], "grok-4.3");
        assert_eq!(spec.json_body.as_ref().unwrap()["max_output_tokens"], 1);
        assert_eq!(spec.provider_api_format, "openai:responses");
    }

    #[test]
    fn parses_xai_request_and_token_windows() {
        let headers = BTreeMap::from([
            ("x-ratelimit-limit-requests".to_string(), "100".to_string()),
            (
                "x-ratelimit-remaining-requests".to_string(),
                "25".to_string(),
            ),
            ("x-ratelimit-reset-requests".to_string(), "2000".to_string()),
            ("x-ratelimit-limit-tokens".to_string(), "1000".to_string()),
            ("x-ratelimit-remaining-tokens".to_string(), "0".to_string()),
            ("x-ratelimit-reset-tokens".to_string(), "3000".to_string()),
            ("x-subscription-tier".to_string(), "supergrok".to_string()),
            ("x-entitlement-status".to_string(), "active".to_string()),
        ]);

        let snapshot =
            parse_grok_quota_headers(&headers, 200, 1000).expect("quota headers should parse");
        assert_eq!(snapshot["usage_ratio"], 1.0);
        assert_eq!(snapshot["exhausted"], true);
        assert_eq!(snapshot["reset_seconds"], 2000);
        assert_eq!(snapshot["plan_type"], "supergrok");
        assert_eq!(snapshot["windows"].as_array().unwrap().len(), 2);
        assert_eq!(snapshot["windows"][0]["scope"], "account");
        assert_eq!(snapshot["windows"][0]["limit_value"], 100.0);
        assert_eq!(snapshot["windows"][0]["remaining_value"], 25.0);
    }

    #[test]
    fn retry_after_marks_429_exhausted_without_dimension_headers() {
        let headers = BTreeMap::from([("Retry-After".to_string(), "120".to_string())]);
        let snapshot = parse_grok_quota_headers(&headers, 429, 1000)
            .expect("retry-after should be enough for a snapshot");
        assert_eq!(snapshot["exhausted"], true);
        assert_eq!(snapshot["reset_at"], 1120);
        assert_eq!(snapshot["retry_after_seconds"], 120);
    }

    #[test]
    fn headerless_errors_do_not_replace_the_last_known_quota_state() {
        assert!(parse_grok_quota_headers(&BTreeMap::new(), 429, 1000).is_none());
        assert!(parse_grok_quota_headers(&BTreeMap::new(), 503, 1000).is_none());
    }

    #[test]
    fn exhausted_window_without_reset_is_not_persisted_as_permanent_block() {
        let headers = BTreeMap::from([
            ("x-ratelimit-limit-requests".to_string(), "100".to_string()),
            (
                "x-ratelimit-remaining-requests".to_string(),
                "0".to_string(),
            ),
        ]);
        let snapshot = parse_grok_quota_headers(&headers, 429, 1000).expect("headers should parse");
        assert_eq!(snapshot["exhausted"], false);
        assert!(snapshot["reset_at"].is_null());
        assert!(!grok_quota_bucket_exhausted(
            snapshot.as_object().expect("snapshot object")
        ));
    }

    #[test]
    fn exhausted_window_does_not_borrow_an_unrelated_window_reset() {
        let headers = BTreeMap::from([
            ("x-ratelimit-limit-requests".to_string(), "100".to_string()),
            (
                "x-ratelimit-remaining-requests".to_string(),
                "0".to_string(),
            ),
            ("x-ratelimit-limit-tokens".to_string(), "1000".to_string()),
            (
                "x-ratelimit-remaining-tokens".to_string(),
                "900".to_string(),
            ),
            ("x-ratelimit-reset-tokens".to_string(), "3000".to_string()),
        ]);
        let snapshot = parse_grok_quota_headers(&headers, 429, 1000).expect("headers should parse");
        assert_eq!(snapshot["exhausted"], false);
        assert_eq!(snapshot["reset_at"], 3000);
    }

    #[test]
    fn partial_observation_preserves_the_other_quota_dimension() {
        let previous = parse_grok_quota_headers(
            &BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "100".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "50".to_string(),
                ),
                ("x-ratelimit-reset-requests".to_string(), "2000".to_string()),
                ("x-ratelimit-limit-tokens".to_string(), "1000".to_string()),
                ("x-ratelimit-remaining-tokens".to_string(), "0".to_string()),
                ("x-ratelimit-reset-tokens".to_string(), "3000".to_string()),
            ]),
            200,
            1000,
        )
        .expect("full observation");
        let incoming = parse_grok_quota_headers(
            &BTreeMap::from([
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "40".to_string(),
                ),
                ("x-ratelimit-reset-requests".to_string(), "2100".to_string()),
            ]),
            200,
            1100,
        )
        .expect("partial observation");

        let merged = merge_grok_quota_snapshot(Some(&previous), &incoming);
        assert_eq!(merged["windows"].as_array().map(Vec::len), Some(2));
        let tokens = merged["windows"]
            .as_array()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|window| window["code"].as_str() == Some("tokens"))
            })
            .expect("tokens window should remain");
        assert_eq!(tokens["remaining_value"], 0.0);
        let requests = merged["windows"]
            .as_array()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|window| window["code"].as_str() == Some("requests"))
            })
            .expect("requests window should remain");
        assert_eq!(requests["limit_value"], 100.0);
        assert_eq!(requests["remaining_value"], 40.0);
        assert_eq!(requests["used_ratio"], 0.6);
        assert_eq!(merged["exhausted"], true);
        assert_eq!(merged["reset_at"], 3000);
    }

    #[test]
    fn legacy_window_without_reset_does_not_block_forever() {
        let snapshot = json!({
            "windows": [{
                "code": "requests",
                "is_exhausted": true,
                "remaining": 0.0
            }]
        });
        assert!(!grok_quota_bucket_exhausted(
            snapshot.as_object().expect("snapshot object")
        ));
    }

    /// Captured from cli-chat-proxy.grok.com on 2026-07-29 for a SuperGrok
    /// account whose weekly credit window was exhausted (the upstream state
    /// behind a 402 "Grok Build usage balance exhausted").
    fn weekly_billing_body() -> Value {
        json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-07-25T12:23:24.325987+00:00",
                    "end": "2026-08-01T12:23:24.325987+00:00"
                },
                "creditUsagePercent": 100.0,
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0},
                "productUsage": [{"product": "GrokBuild", "usagePercent": 100.0}],
                "isUnifiedBillingUser": true,
                "prepaidBalance": {"val": 0},
                "topUpMethod": "TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
                "billingPeriodStart": "2026-07-25T12:23:24.325987+00:00",
                "billingPeriodEnd": "2026-08-01T12:23:24.325987+00:00"
            }
        })
    }

    /// Captured alongside the weekly body above. The monthly window carries no
    /// `currentPeriod` and reports every amount in `{"val": N}` cent form.
    fn monthly_billing_body() -> Value {
        json!({
            "config": {
                "monthlyLimit": {"val": 15000},
                "used": {"val": 10406},
                "onDemandCap": {"val": 0},
                "billingPeriodStart": "2026-07-01T00:00:00+00:00",
                "billingPeriodEnd": "2026-08-01T00:00:00+00:00",
                "history": [{
                    "billingCycle": {"year": 2026, "month": 6},
                    "includedUsed": {"val": 0},
                    "onDemandUsed": {"val": 0},
                    "totalUsed": {"val": 0}
                }]
            }
        })
    }

    #[test]
    fn builds_billing_lookups_without_spending_model_quota() {
        let weekly = build_grok_billing_request(
            "key-1",
            "https://cli-chat-proxy.grok.com/v1/",
            Some(("authorization".to_string(), "Bearer token".to_string())),
            None,
            GrokBillingWindow::Weekly,
        )
        .expect("weekly billing request should build");
        assert_eq!(weekly.method, "GET");
        assert_eq!(
            weekly.url,
            "https://cli-chat-proxy.grok.com/v1/billing?format=credits"
        );
        assert!(weekly.json_body.is_none());
        assert!(weekly.content_type.is_none());
        assert_eq!(weekly.quota_kind, "grok:xai_billing");

        let monthly = build_grok_billing_request(
            "key-1",
            "",
            Some(("authorization".to_string(), "Bearer token".to_string())),
            None,
            GrokBillingWindow::Monthly,
        )
        .expect("monthly billing request should build");
        assert_eq!(monthly.url, format!("{XAI_DEFAULT_BASE_URL}/billing"));
        // Concurrent window lookups must stay individually attributable.
        assert_ne!(weekly.request_id, monthly.request_id);
    }

    #[test]
    fn parses_weekly_and_monthly_billing_payloads() {
        let weekly = parse_grok_billing_payload(&weekly_billing_body(), 1000).expect("weekly");
        // xAI spells the period as USAGE_PERIOD_TYPE_WEEKLY.
        assert_eq!(weekly["period_type"], "weekly");
        assert_eq!(weekly["usage_percent"], 100.0);
        assert_eq!(weekly["period_start"], "2026-07-25T12:23:24.325987+00:00");
        assert_eq!(weekly["period_end"], "2026-08-01T12:23:24.325987+00:00");
        assert_eq!(weekly["product_usage"][0]["product"], "GrokBuild");
        // The weekly window says nothing about the subscription ceiling.
        assert!(weekly["monthly_limit_cents"].is_null());

        let monthly = parse_grok_billing_payload(&monthly_billing_body(), 1000).expect("monthly");
        assert_eq!(monthly["monthly_limit_cents"], 15000.0);
        assert_eq!(monthly["used_cents"], 10406.0);
        assert_eq!(monthly["plan"], "SuperGrok");
        assert_eq!(monthly["used_percent"], 10406.0 / 15000.0 * 100.0);
    }

    #[test]
    fn exhausted_weekly_credits_surface_as_full_utilisation() {
        // The live shape behind a 402: the weekly credit window is spent while
        // the monthly subscription still has room, so the aggregate has to
        // follow the binding window rather than average them.
        let weekly = parse_grok_billing_payload(&weekly_billing_body(), 1000).expect("weekly");
        let monthly = parse_grok_billing_payload(&monthly_billing_body(), 1000).expect("monthly");
        let billing =
            merge_grok_billing_snapshot(None, Some(&weekly), Some(&monthly), 200, 200, 1000)
                .expect("billing");
        assert!(grok_billing_has_authoritative_quota(Some(&billing)));

        let merged = merge_grok_quota_snapshot(None, &json!({"billing": billing}));
        assert_eq!(merged["usage_ratio"], 1.0);
        assert_eq!(merged["exhausted"], true);
        assert_eq!(merged["code"], "exhausted");
        // Billing always carries a period end, so exhaustion cannot latch.
        assert_eq!(
            merged["reset_at"].as_u64().expect("reset deadline"),
            parse_reset_at("2026-08-01T12:23:24.325987+00:00").expect("period end")
        );
        let weekly_window = merged["windows"]
            .as_array()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|window| window["code"] == "billing_weekly")
            })
            .expect("weekly billing window");
        assert_eq!(weekly_window["is_exhausted"], true);
        // The monthly subscription still had room; the week is what binds.
        let monthly_window = merged["windows"]
            .as_array()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|window| window["code"] == "billing_monthly")
            })
            .expect("monthly billing window");
        assert_eq!(monthly_window["is_exhausted"], false);
        assert_eq!(monthly_window["limit_value"], 150.0);
        assert_eq!(monthly_window["used_value"], 104.06);
    }

    #[test]
    fn overflow_budget_keeps_a_spent_weekly_window_schedulable() {
        let spent = |extra: Value| {
            let mut config = weekly_billing_body();
            let target = config["config"].as_object_mut().expect("config object");
            for (key, value) in extra.as_object().expect("extra object") {
                if value.is_null() {
                    target.remove(key);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
            let billing = parse_grok_billing_payload(&config, 1000).expect("weekly");
            grok_billing_quota_windows(Some(&billing))
                .into_iter()
                .find(|window| window["code"] == "billing_weekly")
                .expect("weekly window")["is_exhausted"]
                == json!(true)
        };

        assert!(spent(json!({})), "100% with no headroom is spent");
        assert!(
            !spent(json!({"onDemandCap": {"val": 5000}})),
            "on-demand headroom absorbs further spend"
        );
        assert!(
            !spent(json!({"prepaidBalance": {"val": 2500}})),
            "prepaid balance absorbs further spend"
        );
        // Unknown headroom must not block: a wrong verdict costs a whole
        // billing period, while missing one only repeats the 402.
        assert!(
            !spent(json!({"onDemandCap": Value::Null})),
            "absent overflow field is unknown, not zero"
        );
        assert!(
            !spent(json!({"creditUsagePercent": 99.5})),
            "below 100% is not spent"
        );
    }

    #[test]
    fn monthly_only_payload_leaves_the_weekly_usage_field_empty() {
        let monthly = parse_grok_billing_payload(&monthly_billing_body(), 1000).expect("monthly");
        assert_eq!(monthly["period_type"], "monthly");
        assert!(monthly["usage_percent"].is_null());
    }

    #[test]
    fn overage_spend_is_clamped_to_the_subscription_ceiling() {
        let body = json!({"config": {"monthlyLimit": 15000, "used": 21000}});
        let monthly = parse_grok_billing_payload(&body, 1000).expect("monthly");
        assert_eq!(monthly["included_used_cents"], 15000.0);
        assert_eq!(monthly["used_cents"], 21000.0);
        assert_eq!(monthly["used_percent"], 100.0);
    }

    #[test]
    fn reads_cent_values_in_every_shape_xai_emits() {
        for raw in [json!({"val": 150000}), json!(150000), json!("150000")] {
            let body = json!({"config": {"monthlyLimit": raw, "used": 0}});
            let monthly = parse_grok_billing_payload(&body, 1000).expect("monthly");
            assert_eq!(monthly["plan"], "SuperGrok Heavy");
        }
        let unknown_tier = json!({"config": {"monthlyLimit": 999, "used": 0}});
        let monthly = parse_grok_billing_payload(&unknown_tier, 1000).expect("monthly");
        assert!(monthly["plan"].is_null());
    }

    #[test]
    fn empty_billing_config_is_not_an_observation() {
        assert!(parse_grok_billing_payload(&json!({"config": {}}), 1000).is_none());
        assert!(parse_grok_billing_payload(&json!({}), 1000).is_none());
    }

    #[test]
    fn failed_window_keeps_the_previous_billing_value() {
        let weekly = parse_grok_billing_payload(&weekly_billing_body(), 1000).expect("weekly");
        let monthly = parse_grok_billing_payload(&monthly_billing_body(), 1000).expect("monthly");
        let full = merge_grok_billing_snapshot(None, Some(&weekly), Some(&monthly), 200, 200, 1000)
            .expect("full observation");
        assert_eq!(full["partial"], false);
        assert_eq!(full["plan"], "SuperGrok");
        assert_eq!(full["usage_percent"], 100.0);
        // The monthly window owns the billing period; the weekly one must not
        // overwrite it with its own narrower dates.
        assert_eq!(full["billing_period_start"], "2026-07-01T00:00:00+00:00");

        let weekly_only =
            merge_grok_billing_snapshot(Some(&full), Some(&weekly), None, 200, 503, 1100)
                .expect("partial observation");
        assert_eq!(weekly_only["partial"], true);
        assert_eq!(weekly_only["failed_windows"], json!(["monthly"]));
        assert_eq!(weekly_only["monthly_status_code"], 503);
        // The monthly domain must survive its own lookup failing.
        assert_eq!(weekly_only["plan"], "SuperGrok");
        assert_eq!(weekly_only["monthly_limit_cents"], 15000.0);
    }

    #[test]
    fn authoritative_quota_needs_a_paid_billing_signal() {
        assert!(!grok_billing_has_authoritative_quota(None));
        assert!(!grok_billing_has_authoritative_quota(Some(&json!({}))));
        // A Free account answers successfully but says nothing about allowance.
        assert!(!grok_billing_has_authoritative_quota(Some(&json!({
            "period_type": "monthly",
            "monthly_limit_cents": Value::Null,
            "plan": Value::Null
        }))));
        assert!(grok_billing_has_authoritative_quota(Some(&json!({
            "usage_percent": 12.0
        }))));
        assert!(grok_billing_has_authoritative_quota(Some(&json!({
            "monthly_limit_cents": 15000.0
        }))));
        assert!(grok_billing_has_authoritative_quota(Some(&json!({
            "plan": "SuperGrok"
        }))));
    }

    #[test]
    fn successful_empty_billing_is_still_a_display_result() {
        let billing = json!({
            "weekly_status_code": 200,
            "monthly_status_code": 200,
            "usage_percent": Value::Null,
            "monthly_limit_cents": 0.0,
            "plan": Value::Null
        });

        assert!(grok_billing_has_successful_response(Some(&billing)));
        assert!(!grok_billing_has_authoritative_quota(Some(&billing)));
        assert!(!grok_billing_has_successful_response(Some(&json!({
            "weekly_status_code": 503,
            "monthly_status_code": 0
        }))));
    }

    #[test]
    fn billing_usage_beats_the_static_ceiling_windows() {
        // Reproduces the live shape: remaining == limit with no reset, which
        // reports zero usage however much of the subscription is gone.
        let headers = parse_grok_quota_headers(
            &BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "8300".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "8300".to_string(),
                ),
            ]),
            200,
            1000,
        )
        .expect("header observation");
        let previous = json!({
            "billing": {
                "usage_percent": 32.1,
                "monthly_limit_cents": 15000.0,
                "included_used_cents": 9600.0,
                "plan": "SuperGrok",
            }
        });

        let merged = merge_grok_quota_snapshot(Some(&previous), &headers);
        // 9600/15000 = 64% monthly beats both the 32.1% week and the ceiling.
        assert_eq!(merged["usage_ratio"], 0.64);
        assert_eq!(merged["exhausted"], false);
        let codes = merged["windows"]
            .as_array()
            .expect("windows")
            .iter()
            .filter_map(|window| window["code"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(codes, vec!["requests", "billing_weekly", "billing_monthly"]);
    }

    #[test]
    fn header_observation_does_not_erase_the_billing_bucket() {
        let billing = parse_grok_billing_payload(&monthly_billing_body(), 1000).expect("monthly");
        let previous = merge_grok_quota_snapshot(
            None,
            &json!({"provider_type": "grok", "billing": billing, "observed_at": 1000}),
        );
        let headers = parse_grok_quota_headers(
            &BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "8300".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "8300".to_string(),
                ),
            ]),
            200,
            1100,
        )
        .expect("header observation");

        let merged = merge_grok_quota_snapshot(Some(&previous), &headers);
        assert_eq!(merged["billing"]["plan"], "SuperGrok");
        assert_eq!(merged["billing"]["monthly_limit_cents"], 15000.0);
        // The header observation adds its own window and leaves the billing
        // projection standing. The weekly window is absent because a
        // monthly-only payload says nothing about credits.
        let codes = merged["windows"]
            .as_array()
            .expect("windows")
            .iter()
            .filter_map(|window| window["code"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(codes, vec!["requests", "billing_monthly"]);
    }
}
