use std::time::{SystemTime, UNIX_EPOCH};

use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey;
use serde_json::{json, Map, Value};

use crate::provider::ProviderPoolMemberInput;
use crate::providers::codex::codex_quota_soft_threshold_exceeded;
use crate::service::ProviderPoolService;

pub fn provider_pool_key_account_quota_exhausted(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
) -> bool {
    provider_pool_key_account_quota_exhausted_with_basis(key, provider_type, None)
}

pub fn provider_pool_key_account_quota_exhausted_with_basis(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
    codex_quota_basis: Option<&str>,
) -> bool {
    provider_pool_key_account_quota_exhausted_with_policy(
        key,
        provider_type,
        codex_quota_basis,
        None,
    )
}

pub fn provider_pool_key_account_quota_exhausted_with_policy(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
    codex_quota_basis: Option<&str>,
    codex_quota_soft_threshold_percent: Option<f64>,
) -> bool {
    let adapter = ProviderPoolService::with_builtin_adapters().adapter(provider_type);
    let exhausted = adapter.quota_exhausted(&ProviderPoolMemberInput {
        provider_type,
        key,
        auth_config: None,
        codex_quota_basis,
    });
    exhausted
        || (provider_type.trim().eq_ignore_ascii_case("codex")
            && codex_quota_soft_threshold_exceeded(key, codex_quota_soft_threshold_percent))
}

pub fn provider_pool_key_codex_quota_soft_threshold_exceeded(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
    codex_quota_soft_threshold_percent: Option<f64>,
) -> bool {
    provider_type.trim().eq_ignore_ascii_case("codex")
        && codex_quota_soft_threshold_exceeded(key, codex_quota_soft_threshold_percent)
}

pub fn provider_pool_member_quota_snapshot<'a>(
    key: &'a StoredProviderCatalogKey,
    provider_type: &str,
) -> Option<&'a Map<String, Value>> {
    let quota_snapshot = key
        .status_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("quota"))
        .and_then(Value::as_object)?;
    provider_pool_quota_snapshot_matches_provider(quota_snapshot, provider_type)
        .then_some(quota_snapshot)
}

pub fn provider_pool_quota_snapshot_updated_at(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
) -> Option<u64> {
    let quota_snapshot = provider_pool_member_quota_snapshot(key, provider_type)?;
    provider_pool_timestamp_unix_secs(quota_snapshot.get("updated_at"))
}

pub fn provider_pool_quota_metadata_updated_at(
    upstream_metadata: Option<&Value>,
    provider_type: &str,
) -> Option<u64> {
    let bucket = provider_pool_metadata_bucket(upstream_metadata, provider_type)?;
    provider_pool_timestamp_unix_secs(bucket.get("updated_at"))
}

pub fn provider_pool_quota_metadata_provider_type(metadata_update: &Value) -> Option<String> {
    let object = metadata_update.as_object()?;
    let service = ProviderPoolService::with_builtin_adapters();
    let known_provider_type = service
        .provider_types()
        .find(|provider_type| object.contains_key(*provider_type))
        .map(ToOwned::to_owned);
    known_provider_type.or_else(|| {
        object
            .iter()
            .find(|(_, value)| value.is_object())
            .map(|(provider_type, _)| provider_type.clone())
    })
}

pub fn provider_pool_key_scheduling_label(
    is_active: bool,
    cooldown_reason: Option<&str>,
    cooldown_ttl_seconds: Option<u64>,
) -> (String, String, String, Vec<Value>) {
    if !is_active {
        return (
            "blocked".to_string(),
            "inactive".to_string(),
            "已禁用".to_string(),
            vec![json!({
                "code": "inactive",
                "label": "已禁用",
                "blocking": true,
                "source": "manual",
                "ttl_seconds": Value::Null,
                "detail": Value::Null,
            })],
        );
    }
    if let Some(reason) = cooldown_reason {
        return (
            "degraded".to_string(),
            "cooldown".to_string(),
            "冷却中".to_string(),
            vec![json!({
                "code": "cooldown",
                "label": "冷却中",
                "blocking": true,
                "source": "pool",
                "ttl_seconds": cooldown_ttl_seconds,
                "detail": reason,
            })],
        );
    }
    (
        "available".to_string(),
        "available".to_string(),
        "可用".to_string(),
        Vec::new(),
    )
}

pub(crate) fn provider_pool_metadata_bucket<'a>(
    upstream_metadata: Option<&'a Value>,
    provider_type: &str,
) -> Option<&'a Map<String, Value>> {
    upstream_metadata
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get(&provider_type.trim().to_ascii_lowercase()))
        .and_then(Value::as_object)
}

pub(crate) fn provider_pool_json_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn provider_pool_json_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(value)) => value.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

pub(crate) fn provider_pool_timestamp_unix_secs(value: Option<&Value>) -> Option<u64> {
    let mut timestamp = provider_pool_json_f64(value)?;
    if timestamp <= 0.0 {
        return None;
    }
    if timestamp > 1_000_000_000_000.0 {
        timestamp /= 1000.0;
    }
    Some(timestamp as u64)
}

pub(crate) fn provider_pool_current_unix_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn provider_pool_reset_deadline_unix_secs(
    item: &Map<String, Value>,
    fallback_observed_at: Option<u64>,
) -> Option<u64> {
    provider_pool_timestamp_unix_secs(item.get("reset_at"))
        .or_else(|| provider_pool_timestamp_unix_secs(item.get("next_reset_at")))
        .or_else(|| {
            let reset_seconds = provider_pool_json_f64(item.get("reset_seconds"))
                .or_else(|| provider_pool_json_f64(item.get("reset_after_seconds")))?;
            if reset_seconds < 0.0 {
                return None;
            }
            let base = provider_pool_timestamp_unix_secs(item.get("observed_at"))
                .or_else(|| provider_pool_timestamp_unix_secs(item.get("updated_at")))
                .or(fallback_observed_at)?;
            Some(base.saturating_add(reset_seconds.ceil() as u64))
        })
}

pub(crate) fn provider_pool_reset_deadline_elapsed(
    item: &Map<String, Value>,
    fallback_observed_at: Option<u64>,
    now_unix_secs: u64,
) -> bool {
    provider_pool_reset_deadline_unix_secs(item, fallback_observed_at)
        .is_some_and(|reset_at| reset_at <= now_unix_secs)
}

fn provider_pool_quota_window_is_exhausted(window: &Map<String, Value>) -> bool {
    provider_pool_json_bool(window.get("is_exhausted"))
        .or_else(|| {
            provider_pool_json_f64(window.get("used_ratio")).map(|value| value >= 1.0 - 1e-6)
        })
        .unwrap_or(false)
}

fn provider_pool_quota_snapshot_matches_provider(
    quota_snapshot: &Map<String, Value>,
    provider_type: &str,
) -> bool {
    let normalized_provider_type = provider_type.trim().to_ascii_lowercase();
    match quota_snapshot
        .get("provider_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(provider_type) => provider_type.eq_ignore_ascii_case(&normalized_provider_type),
        None => {
            provider_pool_json_bool(quota_snapshot.get("exhausted")) == Some(true)
                || quota_snapshot
                    .get("code")
                    .and_then(Value::as_str)
                    .is_some_and(|code| !code.trim().eq_ignore_ascii_case("unknown"))
                || quota_snapshot
                    .get("updated_at")
                    .is_some_and(|value| !value.is_null())
                || quota_snapshot
                    .get("observed_at")
                    .is_some_and(|value| !value.is_null())
                || quota_snapshot
                    .get("usage_ratio")
                    .is_some_and(|value| !value.is_null())
                || quota_snapshot
                    .get("reset_seconds")
                    .is_some_and(|value| !value.is_null())
                || quota_snapshot
                    .get("windows")
                    .and_then(Value::as_array)
                    .is_some_and(|windows| !windows.is_empty())
                || quota_snapshot
                    .get("credits")
                    .and_then(Value::as_object)
                    .is_some_and(|credits| !credits.is_empty())
        }
    }
}

pub(crate) fn provider_pool_quota_snapshot_exhausted_decision(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
) -> Option<bool> {
    let quota_snapshot = key
        .status_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("quota"))
        .and_then(Value::as_object)?;
    if !provider_pool_quota_snapshot_matches_provider(quota_snapshot, provider_type) {
        return None;
    }
    let exhausted = provider_pool_json_bool(quota_snapshot.get("exhausted"))?;
    if exhausted {
        let now_unix_secs = provider_pool_current_unix_secs();
        let snapshot_observed_at =
            provider_pool_timestamp_unix_secs(quota_snapshot.get("observed_at"))
                .or_else(|| provider_pool_timestamp_unix_secs(quota_snapshot.get("updated_at")));

        if let Some(windows) = quota_snapshot
            .get("windows")
            .and_then(Value::as_array)
            .filter(|windows| !windows.is_empty())
        {
            let mut saw_exhausted_window = false;
            let mut saw_active_exhausted_window = false;
            let mut windows_max_ratio = None::<f64>;
            let top_level_reset_active =
                provider_pool_reset_deadline_unix_secs(quota_snapshot, snapshot_observed_at)
                    .is_some_and(|reset_at| now_unix_secs.is_none_or(|now| reset_at > now));

            for window in windows.iter().filter_map(Value::as_object) {
                if let Some(ratio) = provider_pool_json_f64(window.get("used_ratio")) {
                    windows_max_ratio =
                        Some(windows_max_ratio.map_or(ratio, |current| current.max(ratio)));
                }
                if provider_pool_quota_window_is_exhausted(window) {
                    saw_exhausted_window = true;
                    let reset_active =
                        provider_pool_reset_deadline_unix_secs(window, snapshot_observed_at)
                            .is_some_and(|reset_at| now_unix_secs.is_none_or(|now| reset_at > now));
                    // A raw exhausted flag without a recovery deadline is an
                    // observation, not a permanent scheduling block. Runtime
                    // error cooldowns provide the bounded fallback.
                    if reset_active {
                        saw_active_exhausted_window = true;
                    }
                }
            }

            if saw_exhausted_window {
                return Some(saw_active_exhausted_window || top_level_reset_active);
            }
            if top_level_reset_active {
                return Some(true);
            }
            if windows_max_ratio.is_some_and(|ratio| ratio < 1.0 - 1e-6) {
                return Some(false);
            }
        } else if now_unix_secs.is_some_and(|now| {
            provider_pool_reset_deadline_elapsed(quota_snapshot, snapshot_observed_at, now)
        }) {
            return Some(false);
        }
    }
    Some(exhausted)
}

pub(crate) fn provider_pool_quota_usage_ratio(key: &StoredProviderCatalogKey) -> Option<f64> {
    key.status_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("quota"))
        .and_then(Value::as_object)
        .and_then(|quota| provider_pool_json_f64(quota.get("usage_ratio")))
}

pub(crate) fn provider_pool_quota_reset_seconds(key: &StoredProviderCatalogKey) -> Option<f64> {
    key.status_snapshot
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("quota"))
        .and_then(Value::as_object)
        .and_then(|quota| provider_pool_json_f64(quota.get("reset_seconds")))
}

pub(crate) fn provider_pool_account_blocked(key: &StoredProviderCatalogKey) -> bool {
    key.oauth_invalid_reason.as_deref().is_some_and(|reason| {
        let normalized = reason.trim().to_ascii_lowercase();
        !normalized.is_empty()
            && [
                "banned",
                "forbidden",
                "blocked",
                "suspend",
                "deactivated",
                "disabled",
                "verification",
                "workspace",
                "受限",
                "封",
                "禁",
            ]
            .iter()
            .any(|hint| normalized.contains(hint))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_with_quota(quota: Value) -> StoredProviderCatalogKey {
        let mut key = StoredProviderCatalogKey::new(
            "key-grok".to_string(),
            "provider-grok".to_string(),
            "grok".to_string(),
            "oauth".to_string(),
            None,
            true,
        )
        .expect("key");
        key.status_snapshot = Some(json!({ "quota": quota }));
        key
    }

    #[test]
    fn exhausted_window_without_deadline_is_not_a_permanent_block() {
        let key = key_with_quota(json!({
            "provider_type": "grok",
            "exhausted": true,
            "windows": [{
                "code": "requests",
                "used_ratio": 1.0,
                "is_exhausted": true
            }]
        }));
        assert_eq!(
            provider_pool_quota_snapshot_exhausted_decision(&key, "grok"),
            Some(false)
        );
    }

    #[test]
    fn top_level_retry_deadline_blocks_even_when_windows_have_capacity() {
        let now = provider_pool_current_unix_secs().expect("clock");
        let key = key_with_quota(json!({
            "provider_type": "grok",
            "observed_at": now,
            "exhausted": true,
            "reset_at": now + 120,
            "windows": [{
                "code": "requests",
                "used_ratio": 0.5,
                "is_exhausted": false
            }]
        }));
        assert_eq!(
            provider_pool_quota_snapshot_exhausted_decision(&key, "grok"),
            Some(true)
        );
    }
}
