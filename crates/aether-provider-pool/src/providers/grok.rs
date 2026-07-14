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

pub const XAI_DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
pub const XAI_QUOTA_PROBE_MODEL: &str = "grok-4.3";
const PLACEHOLDER_API_KEY: &str = "__placeholder__";

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
    if let Some((name, value)) = resolved_oauth_auth {
        headers.insert(name.trim().to_ascii_lowercase(), value);
    } else {
        let access_token = decrypted_api_key.unwrap_or_default().trim();
        if access_token.is_empty() || access_token == PLACEHOLDER_API_KEY {
            return Err("缺少 xAI OAuth 认证信息，请先授权/刷新 Token".to_string());
        }
        headers.insert(
            "authorization".to_string(),
            format!("Bearer {access_token}"),
        );
    }

    let base_url = base_url.trim().trim_end_matches('/');
    let base_url = if base_url.is_empty() {
        XAI_DEFAULT_BASE_URL
    } else {
        base_url
    };
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
    if matches!(code.as_str(), "requests" | "tokens") {
        window.insert("scope".to_string(), json!("account"));
        if code == "tokens" {
            window.insert("label".to_string(), json!("Token"));
            window.insert("unit".to_string(), json!("tokens"));
        } else {
            window.insert("label".to_string(), json!("请求"));
            window.insert("unit".to_string(), json!("count"));
        }
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
    merged.insert("windows".to_string(), Value::Array(windows));

    let mut snapshot = Value::Object(merged);
    recompute_grok_quota_aggregate(&mut snapshot);
    snapshot
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
}
