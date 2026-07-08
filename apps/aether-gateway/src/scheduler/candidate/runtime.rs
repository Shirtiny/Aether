use std::collections::{BTreeMap, BTreeSet};

use aether_admin::provider::{
    pool as admin_provider_pool_pure, status as admin_provider_status_pure,
};
use aether_data_contracts::repository::candidates::StoredRequestCandidate;
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use aether_scheduler_core::{
    auth_api_key_concurrency_limit_reached, build_provider_concurrent_limit_map,
    candidate_is_selectable_with_runtime_state, candidate_runtime_skip_reason_with_state,
    CandidateRuntimeSelectabilityInput,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data::auth::GatewayAuthApiKeySnapshot;
use crate::scheduler::pool_collateral_avoidance::provider_pool_sticky_collateral_avoidance_enabled;
use crate::scheduler::session_risk_control::{
    provider_session_risk_control_avoidance_mode, ProviderSessionRiskControlAvoidanceMode,
};
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
    session_risk_control_blocked: bool,
    provider_session_risk_control_blocks: BTreeMap<String, bool>,
    provider_pool_sticky_collateral_blocks: BTreeMap<String, bool>,
    provider_quota_blocks_requests: BTreeMap<String, bool>,
    key_account_quota_exhausted: BTreeMap<String, bool>,
    key_oauth_invalid: BTreeMap<String, bool>,
    provider_key_rpm_reset_ats: BTreeMap<String, Option<u64>>,
}

pub(super) async fn read_candidate_runtime_selection_snapshot(
    state: &(impl SchedulerRuntimeState + ?Sized),
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    client_session_affinity: Option<&ClientSessionAffinity>,
    pool_sticky_session_token: Option<&str>,
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
                    state.codex_quota_soft_threshold_percent,
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
    let provider_session_risk_control =
        read_provider_session_risk_control_block_map(state, &providers, client_session_affinity)
            .await?;
    let provider_pool_sticky_collateral_blocks = read_provider_pool_sticky_collateral_block_map(
        state,
        &providers,
        client_session_affinity,
        pool_sticky_session_token,
    )
    .await?;
    let provider_key_rpm_reset_ats =
        read_provider_key_rpm_reset_at_map(state, candidates, now_unix_secs);

    Ok(CandidateRuntimeSelectionSnapshot {
        recent_candidates,
        provider_concurrent_limits,
        provider_key_rpm_states,
        pool_provider_ids,
        session_risk_control_blocked: provider_session_risk_control.session_blocked,
        provider_session_risk_control_blocks: provider_session_risk_control.provider_blocks,
        provider_pool_sticky_collateral_blocks,
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
    if snapshot.session_risk_control_blocked {
        return false;
    }
    if snapshot
        .provider_session_risk_control_blocks
        .get(candidate.provider_id.as_str())
        .copied()
        .unwrap_or(false)
    {
        return false;
    }
    if snapshot
        .provider_pool_sticky_collateral_blocks
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
    if snapshot.session_risk_control_blocked {
        return Some("session_risk_control_blocked");
    }
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
    if snapshot
        .provider_pool_sticky_collateral_blocks
        .get(candidate.provider_id.as_str())
        .copied()
        .unwrap_or(false)
    {
        return Some("pool_sticky_collateral_avoidance");
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
    codex_quota_soft_threshold_percent: Option<f64>,
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
            let codex_quota_soft_threshold_percent = pool_advanced
                .and_then(serde_json::Value::as_object)
                .and_then(parse_runtime_cost_soft_threshold_percent)
                .filter(|_| provider.provider_type.trim().eq_ignore_ascii_case("codex"));
            (
                provider.id,
                ProviderPoolState {
                    pool_enabled: pool_advanced.is_some(),
                    skip_exhausted_accounts,
                    codex_quota_exhaustion_basis,
                    codex_quota_soft_threshold_percent,
                },
            )
        })
        .collect()
}

#[derive(Debug, Default)]
struct ProviderSessionRiskControlSnapshot {
    session_blocked: bool,
    provider_blocks: BTreeMap<String, bool>,
}

async fn read_provider_session_risk_control_block_map(
    state: &(impl SchedulerRuntimeState + ?Sized),
    providers: &[StoredProviderCatalogProvider],
    client_session_affinity: Option<&ClientSessionAffinity>,
) -> Result<ProviderSessionRiskControlSnapshot, GatewayError> {
    let session_key = client_session_affinity
        .and_then(|affinity| affinity.session_key.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(session_key) = session_key else {
        return Ok(ProviderSessionRiskControlSnapshot::default());
    };

    let enabled_modes = providers
        .iter()
        .filter_map(|provider| {
            let provider_id = provider.id.trim();
            if provider_id.is_empty() {
                return None;
            }
            let mode = provider_session_risk_control_avoidance_mode(provider.config.as_ref());
            mode.is_enabled().then(|| (provider_id.to_string(), mode))
        })
        .collect::<BTreeMap<_, _>>();
    if enabled_modes.is_empty() {
        return Ok(ProviderSessionRiskControlSnapshot::default());
    }

    if enabled_modes.values().any(|mode| mode.blocks_session())
        && state
            .session_has_runtime_risk_control_block(session_key)
            .await?
    {
        return Ok(ProviderSessionRiskControlSnapshot {
            session_blocked: true,
            provider_blocks: BTreeMap::new(),
        });
    }

    let mut snapshot = ProviderSessionRiskControlSnapshot {
        session_blocked: false,
        provider_blocks: enabled_modes
            .keys()
            .map(|provider_id| (provider_id.clone(), false))
            .collect(),
    };
    let mut history_provider_ids = Vec::with_capacity(enabled_modes.len());
    for provider_id in enabled_modes.keys() {
        if state
            .provider_session_has_runtime_risk_control_block(provider_id, session_key)
            .await?
        {
            mark_provider_session_risk_control_blocked(&mut snapshot, &enabled_modes, provider_id);
            if snapshot.session_blocked {
                return Ok(snapshot);
            }
        } else {
            history_provider_ids.push(provider_id.clone());
        }
    }
    if history_provider_ids.is_empty() {
        return Ok(snapshot);
    }

    let usage_provider_ids = state
        .list_provider_ids_with_risk_control_usage_for_session(
            &history_provider_ids,
            session_key,
            None,
        )
        .await?;
    for provider_id in usage_provider_ids {
        mark_provider_session_risk_control_blocked(&mut snapshot, &enabled_modes, &provider_id);
        if snapshot.session_blocked {
            return Ok(snapshot);
        }
    }

    let remaining_provider_ids = history_provider_ids
        .into_iter()
        .filter(|provider_id| {
            !snapshot
                .provider_blocks
                .get(provider_id.as_str())
                .copied()
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if remaining_provider_ids.is_empty() {
        return Ok(snapshot);
    }

    let request_candidate_provider_ids = state
        .read_risk_control_request_candidate_provider_ids_by_client_session_key(
            &remaining_provider_ids,
            session_key,
        )
        .await?;
    for provider_id in request_candidate_provider_ids {
        mark_provider_session_risk_control_blocked(&mut snapshot, &enabled_modes, &provider_id);
        if snapshot.session_blocked {
            break;
        }
    }

    Ok(snapshot)
}

fn mark_provider_session_risk_control_blocked(
    snapshot: &mut ProviderSessionRiskControlSnapshot,
    enabled_modes: &BTreeMap<String, ProviderSessionRiskControlAvoidanceMode>,
    provider_id: &str,
) {
    let Some(mode) = enabled_modes.get(provider_id) else {
        return;
    };
    snapshot
        .provider_blocks
        .insert(provider_id.to_string(), true);
    if mode.blocks_session() {
        snapshot.session_blocked = true;
    }
}

async fn read_provider_pool_sticky_collateral_block_map(
    state: &(impl SchedulerRuntimeState + ?Sized),
    providers: &[StoredProviderCatalogProvider],
    client_session_affinity: Option<&ClientSessionAffinity>,
    pool_sticky_session_token: Option<&str>,
) -> Result<BTreeMap<String, bool>, GatewayError> {
    let sticky_session_token = pool_sticky_session_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            client_session_affinity
                .and_then(|affinity| affinity.session_key.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    let Some(sticky_session_token) = sticky_session_token else {
        return Ok(BTreeMap::new());
    };
    let mut provider_blocks = BTreeMap::new();
    for provider in providers {
        if !provider_pool_sticky_collateral_avoidance_enabled(provider.config.as_ref()) {
            continue;
        }
        let blocked = state
            .provider_session_has_runtime_pool_sticky_collateral_block_if_enabled(
                provider.id.as_str(),
                sticky_session_token,
            )
            .await?;
        provider_blocks.insert(provider.id.clone(), blocked);
    }
    Ok(provider_blocks)
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

fn parse_runtime_cost_soft_threshold_percent(
    pool_advanced: &serde_json::Map<String, serde_json::Value>,
) -> Option<f64> {
    pool_advanced
        .get("cost_soft_threshold_percent")
        .and_then(|value| {
            value.as_f64().or_else(|| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .and_then(|value| value.parse::<f64>().ok())
            })
        })
        .filter(|value| value.is_finite() && *value > 0.0 && *value <= 100.0)
}

fn read_key_account_quota_exhaustion_map(
    candidates: &[SchedulerMinimalCandidateSelectionCandidate],
    provider_key_rpm_states: &BTreeMap<String, StoredProviderCatalogKey>,
    provider_skip_exhausted_accounts: &BTreeMap<String, (bool, String, Option<f64>)>,
) -> BTreeMap<String, bool> {
    candidates
        .iter()
        .map(|candidate| {
            let exhausted = provider_skip_exhausted_accounts
                .get(candidate.provider_id.as_str())
                .map(|(skip, _, _)| *skip)
                .unwrap_or(false)
                && provider_key_rpm_states
                    .get(candidate.key_id.as_str())
                    .is_some_and(|key| {
                        admin_provider_pool_pure::admin_pool_key_account_quota_exhausted_with_policy(
                            key,
                            candidate.provider_type.as_str(),
                            provider_skip_exhausted_accounts
                                .get(candidate.provider_id.as_str())
                                .map(|(_, basis, _)| basis.as_str()),
                            provider_skip_exhausted_accounts
                                .get(candidate.provider_id.as_str())
                                .and_then(|(_, _, threshold)| *threshold),
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
