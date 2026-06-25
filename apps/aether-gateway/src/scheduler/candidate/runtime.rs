use std::collections::{BTreeMap, BTreeSet};

use aether_admin::provider::{
    pool as admin_provider_pool_pure, status as admin_provider_status_pure,
};
use aether_data_contracts::repository::candidates::{
    RequestCandidateStatus, StoredRequestCandidate,
};
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use aether_scheduler_core::{
    auth_api_key_concurrency_limit_reached, build_provider_concurrent_limit_map,
    candidate_is_selectable_with_runtime_state, candidate_runtime_skip_reason_with_state,
    CandidateRuntimeSelectabilityInput,
};
use aether_usage_runtime::{usage_json_text_matches_risk_control, usage_text_matches_risk_control};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data::auth::GatewayAuthApiKeySnapshot;
use crate::GatewayError;

use super::{
    ClientSessionAffinity, SchedulerMinimalCandidateSelectionCandidate, SchedulerRuntimeState,
};

pub(super) use aether_scheduler_core::should_skip_provider_quota;

pub(super) struct CandidateRuntimeSelectionSnapshot {
    pub(super) recent_candidates: Vec<StoredRequestCandidate>,
    pub(super) provider_concurrent_limits: BTreeMap<String, usize>,
    pub(super) provider_key_rpm_states: BTreeMap<String, StoredProviderCatalogKey>,
    pub(super) pool_provider_ids: BTreeSet<String>,
    provider_session_risk_control_blocks: BTreeMap<String, bool>,
    provider_quota_blocks_requests: BTreeMap<String, bool>,
    key_account_quota_exhausted: BTreeMap<String, bool>,
    key_oauth_invalid: BTreeMap<String, bool>,
    provider_key_rpm_reset_ats: BTreeMap<String, Option<u64>>,
}

pub(super) async fn read_candidate_runtime_selection_snapshot(
    state: &(impl SchedulerRuntimeState + ?Sized),
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    client_session_affinity: Option<&ClientSessionAffinity>,
    now_unix_secs: u64,
) -> Result<CandidateRuntimeSelectionSnapshot, GatewayError> {
    let recent_candidates = state.read_recent_request_candidates(128).await?;
    let provider_concurrent_limits = read_provider_concurrent_limits(state, candidates).await?;
    let provider_ids = candidate_provider_ids(candidates);
    let providers = if provider_ids.is_empty() {
        Vec::new()
    } else {
        state
            .read_provider_catalog_providers_by_ids(&provider_ids)
            .await?
    };
    let provider_pool_state = read_provider_pool_state_map_from_providers(&providers);
    let provider_skip_exhausted_accounts = provider_pool_state
        .iter()
        .map(|(provider_id, state)| {
            (
                provider_id.clone(),
                (
                    state.skip_exhausted_accounts,
                    state.codex_quota_exhaustion_basis.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let pool_provider_ids = provider_pool_state
        .iter()
        .filter_map(|(provider_id, state)| state.pool_enabled.then_some(provider_id.clone()))
        .collect::<BTreeSet<_>>();
    let provider_key_rpm_states = read_provider_key_rpm_states(state, candidates).await?;
    let key_account_quota_exhausted = read_key_account_quota_exhaustion_map(
        candidates,
        &provider_key_rpm_states,
        &provider_skip_exhausted_accounts,
    );
    let key_oauth_invalid =
        read_key_oauth_invalid_map(candidates, &provider_key_rpm_states, now_unix_secs);
    let provider_quota_blocks_requests =
        read_provider_quota_block_map(state, candidates, now_unix_secs).await?;
    let provider_session_risk_control_blocks =
        read_provider_session_risk_control_block_map(state, &providers, client_session_affinity)
            .await?;
    let provider_key_rpm_reset_ats =
        read_provider_key_rpm_reset_at_map(state, candidates, now_unix_secs);

    Ok(CandidateRuntimeSelectionSnapshot {
        recent_candidates,
        provider_concurrent_limits,
        provider_key_rpm_states,
        pool_provider_ids,
        provider_session_risk_control_blocks,
        provider_quota_blocks_requests,
        key_account_quota_exhausted,
        key_oauth_invalid,
        provider_key_rpm_reset_ats,
    })
}

pub(super) fn auth_snapshot_concurrency_limit_reached(
    auth_snapshot: Option<&GatewayAuthApiKeySnapshot>,
    snapshot: &CandidateRuntimeSelectionSnapshot,
    now_unix_secs: u64,
) -> bool {
    auth_snapshot
        .and_then(|snapshot| {
            usize::try_from(snapshot.api_key_concurrent_limit?)
                .ok()
                .and_then(|limit| {
                    if limit == 0 {
                        return None;
                    }
                    Some((snapshot.api_key_id.as_str(), limit))
                })
        })
        .is_some_and(|(api_key_id, limit)| {
            auth_api_key_concurrency_limit_reached(
                &snapshot.recent_candidates,
                now_unix_secs,
                api_key_id,
                limit,
            )
        })
}

pub(super) fn is_candidate_selectable(
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
    snapshot: &CandidateRuntimeSelectionSnapshot,
    now_unix_secs: u64,
) -> bool {
    if snapshot
        .provider_session_risk_control_blocks
        .get(candidate.provider_id.as_str())
        .copied()
        .unwrap_or(false)
    {
        return false;
    }
    let pool_group = snapshot
        .pool_provider_ids
        .contains(candidate.provider_id.as_str());
    candidate_is_selectable_with_runtime_state(CandidateRuntimeSelectabilityInput {
        candidate,
        recent_candidates: &snapshot.recent_candidates,
        provider_concurrent_limits: &snapshot.provider_concurrent_limits,
        provider_key_rpm_states: &snapshot.provider_key_rpm_states,
        now_unix_secs,
        provider_quota_blocks_requests: snapshot
            .provider_quota_blocks_requests
            .get(candidate.provider_id.as_str())
            .copied()
            .unwrap_or(false),
        account_quota_exhausted: !pool_group
            && snapshot
                .key_account_quota_exhausted
                .get(candidate.key_id.as_str())
                .copied()
                .unwrap_or(false),
        oauth_invalid: !pool_group
            && snapshot
                .key_oauth_invalid
                .get(candidate.key_id.as_str())
                .copied()
                .unwrap_or(false),
        enforce_key_circuit_breaker: !pool_group,
        rpm_reset_at: (!pool_group)
            .then(|| {
                snapshot
                    .provider_key_rpm_reset_ats
                    .get(candidate.key_id.as_str())
                    .copied()
                    .flatten()
            })
            .flatten(),
    })
}

pub(super) fn current_candidate_runtime_skip_reason(
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
    snapshot: &CandidateRuntimeSelectionSnapshot,
    now_unix_secs: u64,
) -> Option<&'static str> {
    let pool_group = snapshot
        .pool_provider_ids
        .contains(candidate.provider_id.as_str());
    let provider_quota_blocks_requests = snapshot
        .provider_quota_blocks_requests
        .get(candidate.provider_id.as_str())
        .copied()
        .unwrap_or(false);
    if snapshot
        .provider_session_risk_control_blocks
        .get(candidate.provider_id.as_str())
        .copied()
        .unwrap_or(false)
    {
        return Some("provider_session_risk_control_avoidance");
    }
    let rpm_reset_at = (!pool_group)
        .then(|| {
            snapshot
                .provider_key_rpm_reset_ats
                .get(candidate.key_id.as_str())
                .copied()
                .flatten()
        })
        .flatten();

    candidate_runtime_skip_reason_with_state(CandidateRuntimeSelectabilityInput {
        candidate,
        recent_candidates: &snapshot.recent_candidates,
        provider_concurrent_limits: &snapshot.provider_concurrent_limits,
        provider_key_rpm_states: &snapshot.provider_key_rpm_states,
        now_unix_secs,
        provider_quota_blocks_requests,
        account_quota_exhausted: !pool_group
            && snapshot
                .key_account_quota_exhausted
                .get(candidate.key_id.as_str())
                .copied()
                .unwrap_or(false),
        oauth_invalid: !pool_group
            && snapshot
                .key_oauth_invalid
                .get(candidate.key_id.as_str())
                .copied()
                .unwrap_or(false),
        enforce_key_circuit_breaker: !pool_group,
        rpm_reset_at,
    })
}

fn candidate_provider_ids(
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.provider_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
}

pub(super) async fn read_provider_concurrent_limits(
    state: &(impl SchedulerRuntimeState + ?Sized),
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
) -> Result<BTreeMap<String, usize>, GatewayError> {
    let provider_ids = candidate_provider_ids(candidates);
    if provider_ids.is_empty() {
        return Ok(BTreeMap::new());
    }

    let providers = state
        .read_provider_catalog_providers_by_ids(&provider_ids)
        .await?;
    Ok(build_provider_concurrent_limit_map(providers))
}

pub(super) async fn read_provider_key_rpm_states(
    state: &(impl SchedulerRuntimeState + ?Sized),
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
) -> Result<BTreeMap<String, StoredProviderCatalogKey>, GatewayError> {
    let key_ids = candidates
        .iter()
        .map(|candidate| candidate.key_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if key_ids.is_empty() {
        return Ok(BTreeMap::new());
    }

    let keys = state.read_provider_catalog_keys_by_ids(&key_ids).await?;
    Ok(keys
        .into_iter()
        .map(|key| (key.id.clone(), key))
        .collect::<BTreeMap<_, _>>())
}

async fn read_provider_quota_block_map(
    state: &(impl SchedulerRuntimeState + ?Sized),
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    now_unix_secs: u64,
) -> Result<BTreeMap<String, bool>, GatewayError> {
    let provider_ids = candidate_provider_ids(candidates);
    let mut quota_blocks = BTreeMap::new();

    for provider_id in provider_ids {
        let blocks_requests = state
            .read_provider_quota_snapshot(&provider_id)
            .await?
            .as_ref()
            .is_some_and(|quota| should_skip_provider_quota(quota, now_unix_secs));
        quota_blocks.insert(provider_id, blocks_requests);
    }

    Ok(quota_blocks)
}

#[derive(Debug, Clone)]
struct ProviderPoolState {
    pool_enabled: bool,
    skip_exhausted_accounts: bool,
    codex_quota_exhaustion_basis: String,
}

fn read_provider_pool_state_map_from_providers(
    providers: &[StoredProviderCatalogProvider],
) -> BTreeMap<String, ProviderPoolState> {
    providers
        .iter()
        .cloned()
        .into_iter()
        .map(|provider| {
            let pool_advanced = provider
                .config
                .as_ref()
                .and_then(|value| value.get("pool_advanced"));
            let skip_exhausted_accounts = pool_advanced
                .and_then(serde_json::Value::as_object)
                .and_then(|value| value.get("skip_exhausted_accounts"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let codex_quota_exhaustion_basis = pool_advanced
                .and_then(serde_json::Value::as_object)
                .map(parse_runtime_codex_quota_exhaustion_basis)
                .unwrap_or_else(|| "weekly".to_string());
            (
                provider.id,
                ProviderPoolState {
                    pool_enabled: pool_advanced.is_some(),
                    skip_exhausted_accounts,
                    codex_quota_exhaustion_basis,
                },
            )
        })
        .collect()
}

async fn read_provider_session_risk_control_block_map(
    state: &(impl SchedulerRuntimeState + ?Sized),
    providers: &[StoredProviderCatalogProvider],
    client_session_affinity: Option<&ClientSessionAffinity>,
) -> Result<BTreeMap<String, bool>, GatewayError> {
    let session_key = client_session_affinity
        .and_then(|affinity| affinity.session_key.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(session_key) = session_key else {
        return Ok(BTreeMap::new());
    };
    let mut blocks = BTreeMap::new();
    for provider in providers {
        if !provider_session_risk_control_avoidance_enabled(provider.config.as_ref()) {
            continue;
        }
        let blocked =
            provider_session_has_risk_control_history(state, provider.id.as_str(), session_key)
                .await?;
        blocks.insert(provider.id.clone(), blocked);
    }
    Ok(blocks)
}

async fn provider_session_has_risk_control_history(
    state: &(impl SchedulerRuntimeState + ?Sized),
    provider_id: &str,
    session_key: &str,
) -> Result<bool, GatewayError> {
    if state
        .provider_session_has_risk_control_usage(provider_id, session_key, None)
        .await?
    {
        return Ok(true);
    }

    let candidates = state
        .read_request_candidates_by_provider_id_and_client_session_key(provider_id, session_key)
        .await?;
    Ok(candidates
        .iter()
        .any(request_candidate_matches_risk_control))
}

fn request_candidate_matches_risk_control(candidate: &StoredRequestCandidate) -> bool {
    if !matches!(
        candidate.status,
        RequestCandidateStatus::Failed | RequestCandidateStatus::Cancelled
    ) {
        return false;
    }
    usage_text_matches_risk_control(candidate.error_message.as_deref())
        || request_candidate_json_field_matches_risk_control(
            candidate,
            &["upstream_response", "body"],
        )
        || request_candidate_json_field_matches_risk_control(candidate, &["error_flow"])
}

fn request_candidate_json_field_matches_risk_control(
    candidate: &StoredRequestCandidate,
    path: &[&str],
) -> bool {
    let Some(value) = candidate.extra_data.as_ref() else {
        return false;
    };
    let mut current = value;
    for field in path {
        let Some(next) = current.get(*field) else {
            return false;
        };
        current = next;
    }
    usage_json_text_matches_risk_control(current)
}

pub(crate) fn provider_session_risk_control_avoidance_enabled(
    config: Option<&serde_json::Value>,
) -> bool {
    config
        .and_then(|value| value.get("risk_control_session_avoidance"))
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn parse_runtime_codex_quota_exhaustion_basis(
    pool_advanced: &serde_json::Map<String, serde_json::Value>,
) -> String {
    if let Some(weekly_basis) = pool_advanced
        .get("codex_quota_weekly_basis")
        .and_then(serde_json::Value::as_bool)
    {
        return if weekly_basis {
            "weekly".to_string()
        } else {
            "five_hour".to_string()
        };
    }
    match pool_advanced
        .get("codex_quota_exhaustion_basis")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("5h" | "five_hour" | "five_hours" | "5_hour" | "5_hours") => "five_hour".to_string(),
        _ => "weekly".to_string(),
    }
}

fn read_key_account_quota_exhaustion_map(
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    provider_key_rpm_states: &BTreeMap<String, StoredProviderCatalogKey>,
    provider_skip_exhausted_accounts: &BTreeMap<String, (bool, String)>,
) -> BTreeMap<String, bool> {
    candidates
        .iter()
        .map(|candidate| {
            let exhausted = provider_skip_exhausted_accounts
                .get(candidate.provider_id.as_str())
                .map(|(skip, _)| *skip)
                .unwrap_or(false)
                && provider_key_rpm_states
                    .get(candidate.key_id.as_str())
                    .is_some_and(|key| {
                        admin_provider_pool_pure::admin_pool_key_account_quota_exhausted_with_basis(
                            key,
                            candidate.provider_type.as_str(),
                            provider_skip_exhausted_accounts
                                .get(candidate.provider_id.as_str())
                                .map(|(_, basis)| basis.as_str()),
                        )
                    });
            (candidate.key_id.clone(), exhausted)
        })
        .collect()
}

fn read_key_oauth_invalid_map(
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    provider_key_rpm_states: &BTreeMap<String, StoredProviderCatalogKey>,
    now_unix_secs: u64,
) -> BTreeMap<String, bool> {
    candidates
        .iter()
        .map(|candidate| {
            let oauth_invalid = provider_key_rpm_states
                .get(candidate.key_id.as_str())
                .is_some_and(|key| {
                    key_requires_oauth_reauth(key, candidate.provider_type.as_str(), now_unix_secs)
                });
            (candidate.key_id.clone(), oauth_invalid)
        })
        .collect()
}

fn key_requires_oauth_reauth(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
    now_unix_secs: u64,
) -> bool {
    if !key.auth_type.trim().eq_ignore_ascii_case("oauth") {
        return false;
    }

    let invalid_reason = key
        .oauth_invalid_reason
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    if !invalid_reason.is_empty() {
        return oauth_invalid_reason_blocks_scheduling(
            key,
            provider_type,
            invalid_reason,
            now_unix_secs,
        );
    }

    false
}

fn oauth_invalid_reason_blocks_scheduling(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
    invalid_reason: &str,
    now_unix_secs: u64,
) -> bool {
    let trimmed_reason = invalid_reason.trim();
    if oauth_invalid_reason_has_tag(trimmed_reason, "[OAUTH_EXPIRED]") {
        return true;
    }

    let account_state = admin_provider_status_pure::resolve_pool_account_state(
        Some(provider_type),
        key.upstream_metadata.as_ref(),
        Some(trimmed_reason),
    );
    if account_state.blocked
        && !account_state.recoverable
        && account_state
            .code
            .as_deref()
            .is_some_and(oauth_account_state_code_is_hard_block)
    {
        return true;
    }

    if oauth_invalid_reason_has_tag(trimmed_reason, "[REFRESH_FAILED]") {
        return oauth_access_token_expired(key, now_unix_secs);
    }

    false
}

fn oauth_invalid_reason_has_tag(reason: &str, tag: &str) -> bool {
    reason
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with(tag))
}

fn oauth_access_token_expired(key: &StoredProviderCatalogKey, now_unix_secs: u64) -> bool {
    let now_unix_secs = if now_unix_secs == 0 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    } else {
        now_unix_secs
    };
    key.expires_at_unix_secs
        .is_none_or(|expires_at| expires_at == 0 || expires_at <= now_unix_secs)
}

fn oauth_account_state_code_is_hard_block(code: &str) -> bool {
    matches!(
        code.trim().to_ascii_lowercase().as_str(),
        "account_banned"
            | "account_suspended"
            | "account_disabled"
            | "workspace_deactivated"
            | "account_forbidden"
            | "account_blocked"
            | "account_verification"
    )
}

fn read_provider_key_rpm_reset_at_map(
    state: &(impl SchedulerRuntimeState + ?Sized),
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    now_unix_secs: u64,
) -> BTreeMap<String, Option<u64>> {
    candidates
        .iter()
        .map(|candidate| {
            (
                candidate.key_id.clone(),
                state.provider_key_rpm_reset_at(candidate.key_id.as_str(), now_unix_secs),
            )
        })
        .collect::<BTreeMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use super::parse_runtime_codex_quota_exhaustion_basis;
    use serde_json::json;

    #[test]
    fn runtime_codex_quota_weekly_basis_overrides_legacy_basis_string() {
        let weekly = json!({
            "codex_quota_weekly_basis": true,
            "codex_quota_exhaustion_basis": "5h"
        });
        let weekly = weekly.as_object().expect("weekly config should be object");
        assert_eq!(parse_runtime_codex_quota_exhaustion_basis(weekly), "weekly");

        let five_hour = json!({
            "codex_quota_weekly_basis": false,
            "codex_quota_exhaustion_basis": "weekly"
        });
        let five_hour = five_hour
            .as_object()
            .expect("five-hour config should be object");
        assert_eq!(
            parse_runtime_codex_quota_exhaustion_basis(five_hour),
            "five_hour"
        );
    }
}
