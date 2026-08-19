use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey;
use aether_runtime_state::RuntimeLockLease;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::catalog_fence::CatalogMutationImpact;
use crate::codex_ws_config::{parse_codex_ws_feature_flags, CodexWsFeatureFlags};
use crate::{AppState, GatewayError};

const HOT_STATE_SCHEMA_VERSION: u8 = 1;
const CATALOG_RESOURCE_SCHEMA_VERSION: u8 = 2;
const HOT_STATE_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const HOT_TRANSITION_TTL: Duration = Duration::from_secs(120);
const MUTATION_LOCK_TTL: Duration = Duration::from_secs(60);
const MUTATION_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const MUTATION_RECOVERY_TIMEOUT: Duration = Duration::from_secs(1);
const GLOBAL_STATE_KEY: &str = "codex-ws:eligibility:v1:global";
const CATALOG_STATE_KEY: &str = "codex-ws:eligibility:v1:catalog";
const CATALOG_RESOURCE_KEY_PREFIX: &str = "codex-ws:catalog-fence:v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HotSwitch {
    schema_version: u8,
    generation: String,
    stable: bool,
    eligible: bool,
    reason: Option<String>,
    #[serde(default)]
    valid_until_unix_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexWsHotLease {
    pub(crate) generation: String,
    pub(crate) eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CatalogResourceKind {
    Provider,
    Endpoint,
}

impl CatalogResourceKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Endpoint => "endpoint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexWsCatalogResourceSeed {
    pub(crate) kind: CatalogResourceKind,
    pub(crate) id: String,
    pub(crate) eligible: bool,
    pub(crate) ineligible_reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexWsCatalogResourceLease {
    pub(crate) hard_generation: String,
    pub(crate) drain_generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexWsCatalogBindingLease {
    pub(crate) provider: CodexWsCatalogResourceLease,
    pub(crate) endpoint: CodexWsCatalogResourceLease,
}

#[derive(Debug, Default)]
pub(crate) struct CodexWsCatalogResourceLeases {
    providers: BTreeMap<String, CodexWsCatalogResourceLease>,
    endpoints: BTreeMap<String, CodexWsCatalogResourceLease>,
}

impl CodexWsCatalogResourceLeases {
    pub(crate) fn binding(
        &self,
        provider_id: &str,
        endpoint_id: &str,
    ) -> Option<CodexWsCatalogBindingLease> {
        Some(CodexWsCatalogBindingLease {
            provider: self.providers.get(provider_id)?.clone(),
            endpoint: self.endpoints.get(endpoint_id)?.clone(),
        })
    }

    fn insert(
        &mut self,
        kind: CatalogResourceKind,
        id: String,
        lease: CodexWsCatalogResourceLease,
    ) {
        match kind {
            CatalogResourceKind::Provider => self.providers.insert(id, lease),
            CatalogResourceKind::Endpoint => self.endpoints.insert(id, lease),
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexWsFenceDecision {
    Continue,
    ContinueAndDrain,
}

impl CodexWsFenceDecision {
    pub(crate) const fn should_drain(self) -> bool {
        matches!(self, Self::ContinueAndDrain)
    }

    fn combine(self, other: Self) -> Self {
        if self == Self::ContinueAndDrain || other == Self::ContinueAndDrain {
            Self::ContinueAndDrain
        } else {
            Self::Continue
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogResourceSwitch {
    schema_version: u8,
    hard_generation: String,
    drain_generation: String,
    stable: bool,
    eligible: bool,
    reason: Option<String>,
    transition_impact: Option<CatalogMutationImpact>,
}

pub(crate) struct CodexWsCatalogResourceMutation {
    legacy: CodexWsHotMutation,
    resource_kind: CatalogResourceKind,
    resource_id: String,
    resource_key: String,
    transition_raw: String,
    previous: CatalogResourceSwitch,
    impact: CatalogMutationImpact,
}

pub(crate) struct CodexWsCatalogWriteLock {
    lock: RenewableMutationLock,
}

pub(crate) struct CodexWsHotMutation {
    state_key: String,
    transition_raw: String,
    lock: RenewableMutationLock,
}

struct RenewableMutationLock {
    runtime_state: Arc<aether_runtime_state::RuntimeState>,
    lock: Option<RuntimeLockLease>,
    renew_stop: CancellationToken,
    renew_task: Option<tokio::task::JoinHandle<()>>,
    lease_lost: Arc<AtomicBool>,
    confirmed_until_unix_ms: Arc<AtomicU64>,
}

impl Drop for RenewableMutationLock {
    fn drop(&mut self) {
        self.renew_stop.cancel();
        if let Some(task) = self.renew_task.take() {
            task.abort();
        }
        let Some(lock) = self.lock.take() else {
            return;
        };
        let runtime_state = Arc::clone(&self.runtime_state);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_millis(100),
                    runtime_state.lock_release(&lock),
                )
                .await;
            });
        }
    }
}

pub(crate) async fn begin_catalog_write_lock(
    state: &AppState,
) -> Result<CodexWsCatalogWriteLock, GatewayError> {
    let lock = acquire_renewable_mutation_lock(
        state,
        "codex-ws:catalog-write:v1",
        "codex ws catalog write is busy",
    )
    .await?;
    Ok(CodexWsCatalogWriteLock { lock })
}

pub(crate) async fn confirm_catalog_write_lock(
    state: &AppState,
    guard: &CodexWsCatalogWriteLock,
) -> Result<(), GatewayError> {
    if confirm_renewable_mutation_lock(state, &guard.lock).await {
        Ok(())
    } else {
        Err(GatewayError::Internal(
            "codex ws catalog write lost its mutation lock".to_string(),
        ))
    }
}

pub(crate) async fn release_catalog_write_lock(
    state: &AppState,
    guard: CodexWsCatalogWriteLock,
) -> Result<(), GatewayError> {
    release_renewable_mutation_lock(state, guard.lock).await
}

pub(crate) async fn ensure_global_hot_lease(
    state: &AppState,
) -> Result<CodexWsHotLease, GatewayError> {
    if let Some(snapshot) = read_switch(state, GLOBAL_STATE_KEY).await? {
        return Ok(hot_lease(snapshot));
    }
    let config = state
        .data
        .find_system_config_value(crate::codex_ws_config::CODEX_WS_SYSTEM_CONFIG_KEY)
        .await
        .map_err(|error| GatewayError::Internal(error.to_string()))?;
    let flags = parse_codex_ws_feature_flags(config.as_ref());
    ensure_switch(
        state,
        GLOBAL_STATE_KEY,
        flags.enabled && flags.native_codex_ws_enabled,
        "global_codex_ws_disabled",
        None,
    )
    .await
}

pub(crate) async fn ensure_catalog_hot_lease(
    state: &AppState,
) -> Result<CodexWsHotLease, GatewayError> {
    ensure_switch(state, CATALOG_STATE_KEY, true, "catalog_unavailable", None).await
}

/// Lazily publishes resource-scoped state from the same authoritative
/// snapshots used by candidate planning.
pub(crate) async fn ensure_catalog_resource_hot_leases(
    state: &AppState,
    seeds: &[CodexWsCatalogResourceSeed],
) -> Result<CodexWsCatalogResourceLeases, GatewayError> {
    reconcile_catalog_resource_hot_leases(state, seeds, false).await
}

async fn reconcile_catalog_resource_hot_leases(
    state: &AppState,
    seeds: &[CodexWsCatalogResourceSeed],
    reconcile_existing: bool,
) -> Result<CodexWsCatalogResourceLeases, GatewayError> {
    let mut unique = BTreeMap::new();
    for seed in seeds {
        let state_key = catalog_resource_state_key(seed.kind, &seed.id)?;
        unique.entry(state_key).or_insert_with(|| seed.clone());
    }
    if unique.is_empty() {
        return Ok(CodexWsCatalogResourceLeases::default());
    }

    let state_keys = unique.keys().cloned().collect::<Vec<_>>();
    let seeds = unique.into_values().collect::<Vec<_>>();
    let initial_values = state
        .runtime_state
        .kv_get_many(&state_keys)
        .await
        .map_err(hot_state_error)?;
    if initial_values.len() != seeds.len() {
        return Err(GatewayError::Internal(
            "codex ws catalog resource batch length mismatch".to_string(),
        ));
    }

    let mut writes = Vec::new();
    for ((seed, state_key), raw) in seeds.iter().zip(&state_keys).zip(&initial_values) {
        match raw {
            Some(previous_raw) => {
                let mut snapshot = decode_catalog_resource_switch(previous_raw)?;
                if !reconcile_existing
                    || !snapshot.stable
                    || (snapshot.eligible == seed.eligible
                        && snapshot.reason
                            == (!seed.eligible).then_some(seed.ineligible_reason.to_string()))
                {
                    continue;
                }
                snapshot.hard_generation = uuid::Uuid::new_v4().to_string();
                snapshot.drain_generation = uuid::Uuid::new_v4().to_string();
                snapshot.eligible = seed.eligible;
                snapshot.reason = (!seed.eligible).then_some(seed.ineligible_reason.to_string());
                snapshot.transition_impact = None;
                writes.push(KeyHotStateWrite::CompareAndSet {
                    state_key: state_key.clone(),
                    expected: previous_raw.clone(),
                    value: encode_catalog_resource_switch(&snapshot)?,
                });
            }
            None => {
                let snapshot = initial_catalog_resource_switch(
                    seed.eligible,
                    (!seed.eligible).then_some(seed.ineligible_reason),
                );
                writes.push(KeyHotStateWrite::SetIfAbsent {
                    state_key: state_key.clone(),
                    value: encode_catalog_resource_switch(&snapshot)?,
                });
            }
        }
    }

    if !writes.is_empty() {
        let results = futures_util::future::join_all(writes.into_iter().map(|write| {
            let runtime_state = Arc::clone(&state.runtime_state);
            async move {
                match write {
                    KeyHotStateWrite::SetIfAbsent { state_key, value } => {
                        runtime_state
                            .kv_set_if_absent(&state_key, value, HOT_STATE_TTL)
                            .await
                    }
                    KeyHotStateWrite::CompareAndSet {
                        state_key,
                        expected,
                        value,
                    } => {
                        runtime_state
                            .kv_set_if_value(&state_key, &expected, value, HOT_STATE_TTL)
                            .await
                    }
                }
            }
        }))
        .await;
        for result in results {
            result.map_err(hot_state_error)?;
        }
    }

    let actual_values = state
        .runtime_state
        .kv_get_many(&state_keys)
        .await
        .map_err(hot_state_error)?;
    if actual_values.len() != seeds.len() {
        return Err(GatewayError::Internal(
            "codex ws catalog resource batch length mismatch".to_string(),
        ));
    }
    let mut leases = CodexWsCatalogResourceLeases::default();
    for (seed, raw) in seeds.into_iter().zip(actual_values) {
        let Some(raw) = raw else {
            continue;
        };
        let snapshot = decode_catalog_resource_switch(&raw)?;
        if snapshot.stable && snapshot.eligible {
            leases.insert(
                seed.kind,
                seed.id,
                CodexWsCatalogResourceLease {
                    hard_generation: snapshot.hard_generation,
                    drain_generation: snapshot.drain_generation,
                },
            );
        }
    }
    Ok(leases)
}

pub(crate) fn bind_catalog_snapshot_generation(
    state: &AppState,
    generation: &str,
) -> Result<(), GatewayError> {
    if generation.trim().is_empty() {
        return Err(GatewayError::Internal(
            "codex ws catalog generation is empty".to_string(),
        ));
    }
    let mut bound_generation = state
        .codex_ws_catalog_snapshot_generation
        .lock()
        .map_err(|_| GatewayError::Internal("codex ws catalog generation lock poisoned".into()))?;
    if bound_generation.as_deref() == Some(generation) {
        return Ok(());
    }

    state.invalidate_provider_routing_caches();
    *bound_generation = Some(generation.to_string());
    Ok(())
}

pub(crate) async fn validate_global_hot_lease(
    state: &AppState,
    lease: &CodexWsHotLease,
) -> Result<(), &'static str> {
    let raw = state
        .runtime_state
        .kv_get(GLOBAL_STATE_KEY)
        .await
        .map_err(|_| "codex_ws_global_hot_state_unavailable")?;
    validate_switch(raw.as_deref(), &lease.generation, "codex_ws_global_changed")
}

pub(crate) async fn validate_candidate_selection_hot_leases(
    state: &AppState,
    global: Option<&CodexWsHotLease>,
    catalog: &CodexWsHotLease,
) -> Result<(), &'static str> {
    let mut keys = Vec::with_capacity(2);
    if global.is_some() {
        keys.push(GLOBAL_STATE_KEY.to_string());
    }
    keys.push(CATALOG_STATE_KEY.to_string());
    let values = state
        .runtime_state
        .kv_get_many(&keys)
        .await
        .map_err(|_| "candidate_selection_hot_state_unavailable")?;
    if values.len() != keys.len() {
        return Err("candidate_selection_hot_state_unavailable");
    }
    let catalog_index = if let Some(global) = global {
        validate_switch(
            values[0].as_deref(),
            &global.generation,
            "codex_ws_global_changed_during_selection",
        )
        .map_err(|_| "codex_ws_global_changed_during_selection")?;
        1
    } else {
        0
    };
    validate_switch(
        values[catalog_index].as_deref(),
        &catalog.generation,
        "account_catalog_changed_during_selection",
    )
    .map_err(|_| "account_catalog_changed_during_selection")
}

enum KeyHotStateWrite {
    SetIfAbsent {
        state_key: String,
        value: String,
    },
    CompareAndSet {
        state_key: String,
        expected: String,
        value: String,
    },
}

/// Reconciles a cold scheduler snapshot with one shared-state read in the steady state. Writes are
/// only needed after a relevant mutation or when a key is first observed, and run concurrently.
pub(crate) async fn ensure_key_hot_leases(
    state: &AppState,
    keys: &[StoredProviderCatalogKey],
) -> Result<BTreeMap<String, CodexWsHotLease>, GatewayError> {
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let state_keys = keys
        .iter()
        .map(|key| key_state_key(&key.id))
        .collect::<Result<Vec<_>, _>>()?;
    let initial_values = state
        .runtime_state
        .kv_get_many(&state_keys)
        .await
        .map_err(hot_state_error)?;
    if initial_values.len() != keys.len() {
        return Err(GatewayError::Internal(
            "codex ws key hot-state batch length mismatch".to_string(),
        ));
    }

    let mut writes = Vec::new();
    for ((key, state_key), raw) in keys.iter().zip(&state_keys).zip(&initial_values) {
        let (eligible, reason) = key_runtime_eligibility(key);
        match raw {
            Some(previous_raw) => {
                let mut snapshot = decode_switch(previous_raw)?;
                if !snapshot.stable {
                    continue;
                }
                if !update_stable_key_meaning(&mut snapshot, eligible, reason) {
                    continue;
                }
                let reconciled_raw = encode_switch(&snapshot)?;
                writes.push(KeyHotStateWrite::CompareAndSet {
                    state_key: state_key.clone(),
                    expected: previous_raw.clone(),
                    value: reconciled_raw,
                });
            }
            None => {
                let initial = HotSwitch {
                    schema_version: HOT_STATE_SCHEMA_VERSION,
                    generation: uuid::Uuid::new_v4().to_string(),
                    stable: true,
                    eligible,
                    reason: (!eligible).then_some(reason.to_string()),
                    valid_until_unix_secs: None,
                };
                writes.push(KeyHotStateWrite::SetIfAbsent {
                    state_key: state_key.clone(),
                    value: encode_switch(&initial)?,
                });
            }
        }
    }

    if writes.is_empty() {
        return decode_key_hot_leases(keys, initial_values);
    }

    let write_results = futures_util::future::join_all(writes.into_iter().map(|write| {
        let runtime_state = Arc::clone(&state.runtime_state);
        async move {
            match write {
                KeyHotStateWrite::SetIfAbsent { state_key, value } => {
                    runtime_state
                        .kv_set_if_absent(&state_key, value, HOT_STATE_TTL)
                        .await
                }
                KeyHotStateWrite::CompareAndSet {
                    state_key,
                    expected,
                    value,
                } => {
                    runtime_state
                        .kv_set_if_value(&state_key, &expected, value, HOT_STATE_TTL)
                        .await
                }
            }
        }
    }))
    .await;
    for result in write_results {
        result.map_err(hot_state_error)?;
    }
    let actual_values = state
        .runtime_state
        .kv_get_many(&state_keys)
        .await
        .map_err(hot_state_error)?;
    decode_key_hot_leases(keys, actual_values)
}

fn decode_key_hot_leases(
    keys: &[StoredProviderCatalogKey],
    values: Vec<Option<String>>,
) -> Result<BTreeMap<String, CodexWsHotLease>, GatewayError> {
    if values.len() != keys.len() {
        return Err(GatewayError::Internal(
            "codex ws key hot-state batch length mismatch".to_string(),
        ));
    }
    keys.iter()
        .zip(values)
        .map(|(key, raw)| {
            let raw = raw.ok_or_else(|| {
                GatewayError::Internal("codex ws key hot state disappeared".to_string())
            })?;
            Ok((key.id.clone(), hot_lease(decode_switch(&raw)?)))
        })
        .collect()
}

pub(crate) async fn validate_hot_leases(
    state: &AppState,
    provider_id: &str,
    endpoint_id: &str,
    key_id: &str,
    global_generation: Option<&str>,
    key_generation: &str,
    binding: &CodexWsCatalogBindingLease,
) -> Result<CodexWsFenceDecision, &'static str> {
    let key_state_key = key_state_key(key_id).map_err(|_| "account_hot_state_invalid")?;
    let provider_state_key = catalog_resource_state_key(CatalogResourceKind::Provider, provider_id)
        .map_err(|_| "provider_hot_state_invalid")?;
    let endpoint_state_key = catalog_resource_state_key(CatalogResourceKind::Endpoint, endpoint_id)
        .map_err(|_| "endpoint_hot_state_invalid")?;
    let mut keys = Vec::with_capacity(4);
    if global_generation.is_some() {
        keys.push(GLOBAL_STATE_KEY.to_string());
    }
    keys.extend([key_state_key, provider_state_key, endpoint_state_key]);
    let values = state
        .runtime_state
        .kv_get_many(&keys)
        .await
        .map_err(|_| "account_hot_state_unavailable")?;
    if values.len() != keys.len() {
        return Err("account_hot_state_unavailable");
    }

    let key_index = if let Some(global_generation) = global_generation {
        validate_switch(
            values[0].as_deref(),
            global_generation,
            "codex_ws_global_changed",
        )?;
        1
    } else {
        0
    };
    validate_switch(
        values[key_index].as_deref(),
        key_generation,
        "bound_account_ineligible",
    )?;

    let mut decision = validate_catalog_resource_switch(
        values[key_index + 1].as_deref(),
        &binding.provider,
        "bound_provider_changed",
    )?;
    decision = decision.combine(validate_catalog_resource_switch(
        values[key_index + 2].as_deref(),
        &binding.endpoint,
        "bound_endpoint_changed",
    )?);
    Ok(decision)
}

fn validate_catalog_resource_switch(
    raw: Option<&str>,
    expected: &CodexWsCatalogResourceLease,
    changed_reason: &'static str,
) -> Result<CodexWsFenceDecision, &'static str> {
    let raw = raw.ok_or("account_hot_state_missing")?;
    let snapshot: CatalogResourceSwitch =
        serde_json::from_str(raw).map_err(|_| "account_hot_state_invalid")?;
    if snapshot.schema_version != CATALOG_RESOURCE_SCHEMA_VERSION {
        return Err("account_hot_state_invalid");
    }
    if !snapshot.eligible {
        return Err(changed_reason);
    }
    if !snapshot.stable {
        return match snapshot.transition_impact {
            Some(CatalogMutationImpact::SelectionOnly)
                if snapshot.hard_generation == expected.hard_generation =>
            {
                Ok(CodexWsFenceDecision::Continue)
            }
            Some(CatalogMutationImpact::Drain)
                if snapshot.hard_generation == expected.hard_generation =>
            {
                Ok(CodexWsFenceDecision::ContinueAndDrain)
            }
            _ => Err(changed_reason),
        };
    }
    if snapshot.hard_generation != expected.hard_generation {
        return Err(changed_reason);
    }
    if snapshot.drain_generation != expected.drain_generation {
        return Ok(CodexWsFenceDecision::ContinueAndDrain);
    }
    Ok(CodexWsFenceDecision::Continue)
}

pub(crate) async fn begin_global_hot_mutation(
    state: &AppState,
) -> Result<CodexWsHotMutation, GatewayError> {
    begin_mutation(state, GLOBAL_STATE_KEY, "global").await
}

pub(crate) async fn finish_global_hot_mutation(
    state: &AppState,
    mutation: CodexWsHotMutation,
    flags: CodexWsFeatureFlags,
) -> Result<(), GatewayError> {
    let eligible = flags.enabled && flags.native_codex_ws_enabled;
    if !confirm_mutation_lock(state, &mutation).await {
        return finish_mutation(
            state,
            mutation,
            eligible,
            (!eligible).then_some("global_codex_ws_disabled"),
            None,
        )
        .await;
    }
    finish_mutation(
        state,
        mutation,
        eligible,
        (!eligible).then_some("global_codex_ws_disabled"),
        None,
    )
    .await
}

pub(crate) async fn begin_catalog_hot_mutation(
    state: &AppState,
) -> Result<CodexWsHotMutation, GatewayError> {
    begin_mutation(state, CATALOG_STATE_KEY, "catalog").await
}

pub(crate) async fn finish_catalog_hot_mutation(
    state: &AppState,
    mutation: CodexWsHotMutation,
) -> Result<(), GatewayError> {
    finish_mutation(state, mutation, true, None, None).await
}

pub(crate) async fn begin_catalog_resource_hot_mutation(
    state: &AppState,
    seed: &CodexWsCatalogResourceSeed,
    impact: CatalogMutationImpact,
) -> Result<CodexWsCatalogResourceMutation, GatewayError> {
    let legacy = begin_catalog_hot_mutation(state).await?;
    begin_catalog_resource_hot_mutation_with_legacy(state, legacy, seed, impact).await
}

pub(crate) async fn begin_catalog_resource_hot_mutation_with_legacy(
    state: &AppState,
    legacy: CodexWsHotMutation,
    seed: &CodexWsCatalogResourceSeed,
    impact: CatalogMutationImpact,
) -> Result<CodexWsCatalogResourceMutation, GatewayError> {
    let setup_result = async {
        reconcile_catalog_resource_hot_leases(state, std::slice::from_ref(seed), true).await?;
        let resource_key = catalog_resource_state_key(seed.kind, &seed.id)?;
        let previous_raw = state
            .runtime_state
            .kv_get(&resource_key)
            .await
            .map_err(hot_state_error)?
            .ok_or_else(|| {
                GatewayError::Internal("codex ws catalog resource state disappeared".to_string())
            })?;
        let previous = decode_catalog_resource_switch(&previous_raw)?;
        if !previous.stable {
            return Err(GatewayError::Internal(
                "codex ws catalog resource mutation is busy".to_string(),
            ));
        }
        let transition = CatalogResourceSwitch {
            schema_version: CATALOG_RESOURCE_SCHEMA_VERSION,
            hard_generation: previous.hard_generation.clone(),
            drain_generation: previous.drain_generation.clone(),
            stable: false,
            eligible: previous.eligible,
            reason: Some("mutation_in_progress".to_string()),
            transition_impact: Some(impact),
        };
        let transition_raw = encode_catalog_resource_switch(&transition)?;
        if !state
            .runtime_state
            .kv_set_if_value(
                &resource_key,
                &previous_raw,
                transition_raw.clone(),
                HOT_TRANSITION_TTL,
            )
            .await
            .map_err(hot_state_error)?
        {
            return Err(GatewayError::Internal(
                "codex ws catalog resource mutation lost its initial CAS".to_string(),
            ));
        }
        Ok((resource_key, transition_raw, previous))
    }
    .await;

    match setup_result {
        Ok((resource_key, transition_raw, previous)) => Ok(CodexWsCatalogResourceMutation {
            legacy,
            resource_kind: seed.kind,
            resource_id: seed.id.clone(),
            resource_key,
            transition_raw,
            previous,
            impact,
        }),
        Err(error) => match finish_catalog_hot_mutation(state, legacy).await {
            Ok(()) => Err(error),
            Err(recovery_error) => Err(GatewayError::Internal(format!(
                "{error:?}; failed to restore legacy catalog state: {recovery_error:?}"
            ))),
        },
    }
}

pub(crate) async fn finish_catalog_resource_hot_mutation(
    state: &AppState,
    mutation: CodexWsCatalogResourceMutation,
    eligible: bool,
    reason: Option<&str>,
) -> Result<(), GatewayError> {
    let effective_impact = if mutation.previous.eligible == eligible {
        mutation.impact
    } else {
        CatalogMutationImpact::HardFence
    };
    tracing::info!(
        event_name = "codex_ws_catalog_resource_mutation",
        log_type = "ops",
        resource_kind = mutation.resource_kind.label(),
        resource_id = %mutation.resource_id,
        requested_impact = mutation.impact.label(),
        effective_impact = effective_impact.label(),
        eligible,
        "publishing Codex WebSocket catalog resource fence"
    );
    let (hard_generation, drain_generation) = match effective_impact {
        CatalogMutationImpact::SelectionOnly => (
            mutation.previous.hard_generation.clone(),
            mutation.previous.drain_generation.clone(),
        ),
        CatalogMutationImpact::Drain => (
            mutation.previous.hard_generation.clone(),
            uuid::Uuid::new_v4().to_string(),
        ),
        CatalogMutationImpact::HardFence => (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        ),
    };
    let stable = CatalogResourceSwitch {
        schema_version: CATALOG_RESOURCE_SCHEMA_VERSION,
        hard_generation,
        drain_generation,
        stable: true,
        eligible,
        reason: reason.map(str::to_string),
        transition_impact: None,
    };
    let stable_raw = encode_catalog_resource_switch(&stable)?;
    let lock_confirmed = confirm_mutation_lock(state, &mutation.legacy).await;
    let write_result = if lock_confirmed {
        state
            .runtime_state
            .kv_set_if_value(
                &mutation.resource_key,
                &mutation.transition_raw,
                stable_raw,
                HOT_STATE_TTL,
            )
            .await
            .map_err(hot_state_error)
    } else {
        Ok(false)
    };

    match write_result {
        Ok(true) => finish_catalog_hot_mutation(state, mutation.legacy).await,
        Ok(false) => {
            let recovery =
                restore_catalog_resource_restrictive(state, &mutation.resource_key).await;
            leave_hot_mutation_unstable(state, mutation.legacy).await;
            recovery?;
            Err(GatewayError::Internal(
                "codex ws catalog resource mutation lost its lease or strict CAS".to_string(),
            ))
        }
        Err(error) => {
            let recovery =
                restore_catalog_resource_restrictive(state, &mutation.resource_key).await;
            leave_hot_mutation_unstable(state, mutation.legacy).await;
            recovery?;
            Err(error)
        }
    }
}

pub(crate) async fn leave_catalog_resource_hot_mutation_unstable(
    state: &AppState,
    mutation: CodexWsCatalogResourceMutation,
) {
    leave_hot_mutation_unstable(state, mutation.legacy).await;
}

pub(crate) async fn begin_key_hot_mutation(
    state: &AppState,
    key_id: &str,
) -> Result<CodexWsHotMutation, GatewayError> {
    let state_key = key_state_key(key_id)?;
    begin_mutation(state, &state_key, &format!("key:{key_id}")).await
}

pub(crate) async fn finish_key_hot_mutation(
    state: &AppState,
    mutation: CodexWsHotMutation,
    key: Option<&StoredProviderCatalogKey>,
) -> Result<(), GatewayError> {
    let (eligible, reason) = key
        .map(key_runtime_eligibility)
        .unwrap_or((false, "account_deleted"));
    finish_mutation(
        state,
        mutation,
        eligible,
        (!eligible).then_some(reason),
        None,
    )
    .await
}

pub(crate) async fn leave_hot_mutation_unstable(state: &AppState, mutation: CodexWsHotMutation) {
    // The transition was written with a short TTL. Missing state is fail-closed for retained
    // sessions, while a later cold candidate selection can rebuild it from authoritative storage.
    let _ = release_mutation_lock(state, mutation).await;
}

fn update_stable_key_meaning(
    snapshot: &mut HotSwitch,
    eligible: bool,
    reason: &'static str,
) -> bool {
    let next_reason = (!eligible).then_some(reason.to_string());
    if snapshot.stable
        && snapshot.eligible == eligible
        && snapshot.reason == next_reason
        && snapshot.valid_until_unix_secs.is_none()
    {
        return false;
    }
    snapshot.generation = uuid::Uuid::new_v4().to_string();
    snapshot.stable = true;
    snapshot.eligible = eligible;
    snapshot.reason = next_reason;
    snapshot.valid_until_unix_secs = None;
    true
}

async fn ensure_switch(
    state: &AppState,
    state_key: &str,
    eligible: bool,
    ineligible_reason: &str,
    valid_until_unix_secs: Option<u64>,
) -> Result<CodexWsHotLease, GatewayError> {
    if let Some(snapshot) = read_switch(state, state_key).await? {
        return Ok(hot_lease(snapshot));
    }
    let initial = HotSwitch {
        schema_version: HOT_STATE_SCHEMA_VERSION,
        generation: uuid::Uuid::new_v4().to_string(),
        stable: true,
        eligible,
        reason: (!eligible).then_some(ineligible_reason.to_string()),
        valid_until_unix_secs,
    };
    let initial_raw = encode_switch(&initial)?;
    let _ = state
        .runtime_state
        .kv_set_if_absent(state_key, initial_raw, HOT_STATE_TTL)
        .await
        .map_err(hot_state_error)?;
    let raw = state
        .runtime_state
        .kv_get(state_key)
        .await
        .map_err(hot_state_error)?
        .ok_or_else(|| GatewayError::Internal("codex ws hot state disappeared".to_string()))?;
    let snapshot = decode_switch(&raw)?;
    Ok(hot_lease(snapshot))
}

async fn begin_mutation(
    state: &AppState,
    state_key: &str,
    resource: &str,
) -> Result<CodexWsHotMutation, GatewayError> {
    let lock_key = format!("codex-ws:eligibility:v1:mutation:{resource}");
    let lock =
        acquire_renewable_mutation_lock(state, &lock_key, "codex ws hot-state mutation is busy")
            .await?;
    let generation = uuid::Uuid::new_v4().to_string();
    let transition = HotSwitch {
        schema_version: HOT_STATE_SCHEMA_VERSION,
        generation: generation.clone(),
        stable: false,
        eligible: false,
        reason: Some("mutation_in_progress".to_string()),
        valid_until_unix_secs: None,
    };
    let transition_raw = encode_switch(&transition)?;
    if let Err(error) = state
        .runtime_state
        .kv_set(state_key, transition_raw.clone(), Some(HOT_TRANSITION_TTL))
        .await
    {
        let _ = release_renewable_mutation_lock(state, lock).await;
        return Err(hot_state_error(error));
    }
    Ok(CodexWsHotMutation {
        state_key: state_key.to_string(),
        transition_raw,
        lock,
    })
}

async fn acquire_renewable_mutation_lock(
    state: &AppState,
    lock_key: &str,
    busy_message: &str,
) -> Result<RenewableMutationLock, GatewayError> {
    let owner = format!("gateway:{}", uuid::Uuid::new_v4());
    let lock = state
        .runtime_state
        .lock_try_acquire(lock_key, &owner, MUTATION_LOCK_TTL)
        .await
        .map_err(hot_state_error)?
        .ok_or_else(|| GatewayError::Internal(busy_message.to_string()))?;
    let renew_stop = CancellationToken::new();
    let renew_stop_task = renew_stop.clone();
    let renew_runtime = Arc::clone(&state.runtime_state);
    let renew_lock = lock.clone();
    let lease_lost = Arc::new(AtomicBool::new(false));
    let renew_lease_lost = Arc::clone(&lease_lost);
    let confirmed_until_unix_ms = Arc::new(AtomicU64::new(
        crate::clock::current_unix_ms()
            .saturating_add(u64::try_from(MUTATION_LOCK_TTL.as_millis()).unwrap_or(u64::MAX)),
    ));
    let renew_confirmed_until = Arc::clone(&confirmed_until_unix_ms);
    let renew_task = tokio::spawn(async move {
        let interval = MUTATION_LOCK_TTL / 3;
        let mut next_attempt = tokio::time::Instant::now() + interval;
        loop {
            tokio::select! {
                _ = renew_stop_task.cancelled() => break,
                _ = tokio::time::sleep_until(next_attempt) => {
                    match renew_runtime.lock_renew(&renew_lock, MUTATION_LOCK_TTL).await {
                        Ok(true) => {
                            renew_confirmed_until.store(
                                crate::clock::current_unix_ms().saturating_add(
                                    u64::try_from(MUTATION_LOCK_TTL.as_millis())
                                        .unwrap_or(u64::MAX),
                                ),
                                Ordering::Release,
                            );
                            next_attempt = tokio::time::Instant::now() + interval;
                        }
                        Ok(false) => {
                            renew_lease_lost.store(true, Ordering::Release);
                            break;
                        }
                        Err(_) => {
                            if crate::clock::current_unix_ms()
                                >= renew_confirmed_until.load(Ordering::Acquire)
                            {
                                renew_lease_lost.store(true, Ordering::Release);
                                break;
                            }
                            next_attempt =
                                tokio::time::Instant::now() + MUTATION_LOCK_RETRY_INTERVAL;
                        }
                    }
                }
            }
        }
    });
    Ok(RenewableMutationLock {
        runtime_state: Arc::clone(&state.runtime_state),
        lock: Some(lock),
        renew_stop,
        renew_task: Some(renew_task),
        lease_lost,
        confirmed_until_unix_ms,
    })
}

async fn finish_mutation(
    state: &AppState,
    mutation: CodexWsHotMutation,
    eligible: bool,
    reason: Option<&str>,
    valid_until_unix_secs: Option<u64>,
) -> Result<(), GatewayError> {
    let snapshot = HotSwitch {
        schema_version: HOT_STATE_SCHEMA_VERSION,
        // The unstable transition and the final stable meaning are separate epochs. No retained
        // binding can observe an ABA-compatible generation across that stability change.
        generation: uuid::Uuid::new_v4().to_string(),
        stable: true,
        eligible,
        reason: reason.map(str::to_string),
        valid_until_unix_secs,
    };
    let stable_raw = encode_switch(&snapshot)?;
    let state_key = mutation.state_key.clone();
    let lock_confirmed = confirm_mutation_lock(state, &mutation).await;
    let write_result = if lock_confirmed {
        state
            .runtime_state
            .kv_set_if_value(
                &mutation.state_key,
                &mutation.transition_raw,
                stable_raw,
                HOT_STATE_TTL,
            )
            .await
            .map_err(hot_state_error)
    } else {
        Ok(false)
    };
    let release_result = release_mutation_lock(state, mutation).await;
    match write_result {
        Ok(true) => {
            release_result?;
            Ok(())
        }
        Ok(false) => {
            let recovery = restore_restrictive_after_failed_finish(state, &state_key).await;
            if let Err(error) = recovery {
                return Err(GatewayError::Internal(format!(
                    "codex ws hot-state mutation lost strict CAS; restrictive recovery failed: {error:?}"
                )));
            }
            Err(GatewayError::Internal(
                "codex ws hot-state mutation lost its lease or strict CAS".to_string(),
            ))
        }
        Err(error) => {
            let recovery = restore_restrictive_after_failed_finish(state, &state_key).await;
            if let Err(recovery_error) = recovery {
                return Err(GatewayError::Internal(format!(
                    "{error:?}; restrictive recovery failed: {recovery_error:?}"
                )));
            }
            Err(error)
        }
    }
}

async fn confirm_mutation_lock(state: &AppState, mutation: &CodexWsHotMutation) -> bool {
    confirm_renewable_mutation_lock(state, &mutation.lock).await
}

async fn confirm_renewable_mutation_lock(
    state: &AppState,
    mutation: &RenewableMutationLock,
) -> bool {
    let Some(lock) = mutation.lock.as_ref() else {
        return false;
    };
    loop {
        if mutation.lease_lost.load(Ordering::Acquire) {
            return false;
        }
        match state
            .runtime_state
            .lock_renew(lock, MUTATION_LOCK_TTL)
            .await
        {
            Ok(true) => {
                mutation.confirmed_until_unix_ms.store(
                    crate::clock::current_unix_ms().saturating_add(
                        u64::try_from(MUTATION_LOCK_TTL.as_millis()).unwrap_or(u64::MAX),
                    ),
                    Ordering::Release,
                );
                return true;
            }
            Ok(false) => {
                mutation.lease_lost.store(true, Ordering::Release);
                return false;
            }
            Err(_) => {
                let now = crate::clock::current_unix_ms();
                let confirmed_until = mutation.confirmed_until_unix_ms.load(Ordering::Acquire);
                if now >= confirmed_until {
                    mutation.lease_lost.store(true, Ordering::Release);
                    return false;
                }
                let remaining = Duration::from_millis(confirmed_until.saturating_sub(now));
                tokio::time::sleep(std::cmp::min(MUTATION_LOCK_RETRY_INTERVAL, remaining)).await;
            }
        }
    }
}

async fn restore_restrictive_after_failed_finish(
    state: &AppState,
    state_key: &str,
) -> Result<(), GatewayError> {
    tokio::time::timeout(MUTATION_RECOVERY_TIMEOUT, async {
        loop {
            let Some(observed_raw) = state
                .runtime_state
                .kv_get(state_key)
                .await
                .map_err(hot_state_error)?
            else {
                return Ok(());
            };
            let _ = decode_switch(&observed_raw)?;
            // Fence the exact observed value, including another writer's unstable transition.
            // That writer can no longer widen this state with its old expected value.
            let recovery = HotSwitch {
                schema_version: HOT_STATE_SCHEMA_VERSION,
                generation: uuid::Uuid::new_v4().to_string(),
                stable: false,
                eligible: false,
                reason: Some("lost_mutation_recovery".to_string()),
                valid_until_unix_secs: None,
            };
            let recovery_raw = encode_switch(&recovery)?;
            if state
                .runtime_state
                .kv_set_if_value(state_key, &observed_raw, recovery_raw, HOT_TRANSITION_TTL)
                .await
                .map_err(hot_state_error)?
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| {
        GatewayError::Internal(
            "timed out restoring restrictive Codex WS hot state after a lost mutation".to_string(),
        )
    })?
}

async fn restore_catalog_resource_restrictive(
    state: &AppState,
    state_key: &str,
) -> Result<(), GatewayError> {
    tokio::time::timeout(MUTATION_RECOVERY_TIMEOUT, async {
        loop {
            let Some(observed_raw) = state
                .runtime_state
                .kv_get(state_key)
                .await
                .map_err(hot_state_error)?
            else {
                return Ok(());
            };
            let _ = decode_catalog_resource_switch(&observed_raw)?;
            let recovery = CatalogResourceSwitch {
                schema_version: CATALOG_RESOURCE_SCHEMA_VERSION,
                hard_generation: uuid::Uuid::new_v4().to_string(),
                drain_generation: uuid::Uuid::new_v4().to_string(),
                stable: false,
                eligible: false,
                reason: Some("lost_mutation_recovery".to_string()),
                transition_impact: Some(CatalogMutationImpact::HardFence),
            };
            if state
                .runtime_state
                .kv_set_if_value(
                    state_key,
                    &observed_raw,
                    encode_catalog_resource_switch(&recovery)?,
                    HOT_TRANSITION_TTL,
                )
                .await
                .map_err(hot_state_error)?
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| {
        GatewayError::Internal(
            "timed out restoring restrictive Codex WS catalog resource state".to_string(),
        )
    })?
}

async fn release_mutation_lock(
    state: &AppState,
    mutation: CodexWsHotMutation,
) -> Result<(), GatewayError> {
    release_renewable_mutation_lock(state, mutation.lock).await
}

async fn release_renewable_mutation_lock(
    state: &AppState,
    mut mutation: RenewableMutationLock,
) -> Result<(), GatewayError> {
    mutation.renew_stop.cancel();
    if let Some(mut renew_task) = mutation.renew_task.take() {
        if tokio::time::timeout(Duration::from_millis(100), &mut renew_task)
            .await
            .is_err()
        {
            renew_task.abort();
        }
    }
    if let Some(lock) = mutation.lock.take() {
        state
            .runtime_state
            .lock_release(&lock)
            .await
            .map_err(hot_state_error)?;
    }
    Ok(())
}

async fn read_switch(state: &AppState, state_key: &str) -> Result<Option<HotSwitch>, GatewayError> {
    state
        .runtime_state
        .kv_get(state_key)
        .await
        .map_err(hot_state_error)?
        .as_deref()
        .map(decode_switch)
        .transpose()
}

fn hot_lease(snapshot: HotSwitch) -> CodexWsHotLease {
    let within_validity = snapshot
        .valid_until_unix_secs
        .is_none_or(|valid_until| crate::clock::current_unix_secs() < valid_until);
    CodexWsHotLease {
        generation: snapshot.generation,
        eligible: snapshot.stable && snapshot.eligible && within_validity,
    }
}

fn validate_switch(
    raw: Option<&str>,
    expected_generation: &str,
    changed_reason: &'static str,
) -> Result<(), &'static str> {
    let raw = raw.ok_or("account_hot_state_missing")?;
    let snapshot: HotSwitch = serde_json::from_str(raw).map_err(|_| "account_hot_state_invalid")?;
    if snapshot.schema_version != HOT_STATE_SCHEMA_VERSION || !snapshot.stable {
        return Err("account_hot_state_transitioning");
    }
    if snapshot.generation != expected_generation {
        return Err(changed_reason);
    }
    if !snapshot.eligible {
        return Err("bound_account_ineligible");
    }
    if snapshot
        .valid_until_unix_secs
        .is_some_and(|valid_until| crate::clock::current_unix_secs() >= valid_until)
    {
        return Err("bound_account_eligibility_expired");
    }
    Ok(())
}

pub(crate) fn key_runtime_eligibility(key: &StoredProviderCatalogKey) -> (bool, &'static str) {
    // Keep this shared fence limited to mutation-driven hard blockers. Access-token expiry is
    // refreshable during OAuth materialization, while quota/circuit/health eligibility depends on
    // scheduler policy (for example skip_exhausted_accounts) and is evaluated by the cold planner.
    if !key.is_active {
        return (false, "key_inactive");
    }
    if key.auth_type.trim().eq_ignore_ascii_case("oauth")
        && key.oauth_invalid_at_unix_secs.is_some()
    {
        return (false, "key_oauth_invalid");
    }
    (true, "eligible")
}

pub(crate) fn known_key_runtime_blocker(key: &StoredProviderCatalogKey) -> Option<&'static str> {
    let (eligible, reason) = key_runtime_eligibility(key);
    (!eligible).then_some(reason)
}

fn key_state_key(key_id: &str) -> Result<String, GatewayError> {
    let key_id = key_id.trim();
    if key_id.is_empty() || key_id.len() > 512 {
        return Err(GatewayError::Internal(
            "invalid codex ws hot-state key identity".to_string(),
        ));
    }
    Ok(format!("codex-ws:eligibility:v1:key:{key_id}"))
}

fn catalog_resource_state_key(
    kind: CatalogResourceKind,
    resource_id: &str,
) -> Result<String, GatewayError> {
    let resource_id = resource_id.trim();
    if resource_id.is_empty() || resource_id.len() > 512 {
        return Err(GatewayError::Internal(
            "invalid codex ws catalog resource identity".to_string(),
        ));
    }
    Ok(format!(
        "{CATALOG_RESOURCE_KEY_PREFIX}:{}:{resource_id}",
        kind.label()
    ))
}

fn initial_catalog_resource_switch(eligible: bool, reason: Option<&str>) -> CatalogResourceSwitch {
    CatalogResourceSwitch {
        schema_version: CATALOG_RESOURCE_SCHEMA_VERSION,
        hard_generation: uuid::Uuid::new_v4().to_string(),
        drain_generation: uuid::Uuid::new_v4().to_string(),
        stable: true,
        eligible,
        reason: reason.map(str::to_string),
        transition_impact: None,
    }
}

fn encode_catalog_resource_switch(
    snapshot: &CatalogResourceSwitch,
) -> Result<String, GatewayError> {
    serde_json::to_string(snapshot).map_err(|error| {
        GatewayError::Internal(format!(
            "codex ws catalog resource hot-state encode: {error}"
        ))
    })
}

fn decode_catalog_resource_switch(raw: &str) -> Result<CatalogResourceSwitch, GatewayError> {
    let snapshot: CatalogResourceSwitch = serde_json::from_str(raw).map_err(|error| {
        GatewayError::Internal(format!(
            "codex ws catalog resource hot-state decode: {error}"
        ))
    })?;
    if snapshot.schema_version != CATALOG_RESOURCE_SCHEMA_VERSION {
        return Err(GatewayError::Internal(
            "unsupported codex ws catalog resource hot-state schema".to_string(),
        ));
    }
    Ok(snapshot)
}

fn encode_switch(snapshot: &HotSwitch) -> Result<String, GatewayError> {
    serde_json::to_string(snapshot)
        .map_err(|error| GatewayError::Internal(format!("codex ws hot-state encode: {error}")))
}

fn decode_switch(raw: &str) -> Result<HotSwitch, GatewayError> {
    let snapshot: HotSwitch = serde_json::from_str(raw)
        .map_err(|error| GatewayError::Internal(format!("codex ws hot-state decode: {error}")))?;
    if snapshot.schema_version != HOT_STATE_SCHEMA_VERSION {
        return Err(GatewayError::Internal(
            "unsupported codex ws hot-state schema".to_string(),
        ));
    }
    Ok(snapshot)
}

fn hot_state_error(error: aether_runtime_state::DataLayerError) -> GatewayError {
    GatewayError::Internal(format!("codex ws hot-state backend: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey;
    use aether_runtime_state::{MemoryRuntimeStateConfig, RuntimeState};
    use serde_json::json;

    use super::*;

    fn eligible_key() -> StoredProviderCatalogKey {
        let mut key = StoredProviderCatalogKey::new(
            "key-1".to_string(),
            "provider-1".to_string(),
            "Codex OAuth".to_string(),
            "oauth".to_string(),
            Some(json!({"codex_official_ws": true})),
            true,
        )
        .expect("key should build");
        key.api_formats = Some(json!(["openai:responses"]));
        key.fingerprint = Some(json!({
            "websocket_transport_profile": {
                "schema_version": aether_provider_transport::CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION,
                "profile_id": aether_provider_transport::CODEX_OFFICIAL_WS_PROFILE_ID,
                "codex_commit": aether_provider_transport::CODEX_OFFICIAL_WS_CODEX_COMMIT,
                "tokio_tungstenite_rev": aether_provider_transport::CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
                "tungstenite_rev": aether_provider_transport::CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
                "tungstenite_patch_id": aether_provider_transport::CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID,
                "write_buffer_size_bytes": aether_provider_transport::CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
                "max_write_buffer_size_bytes": aether_provider_transport::CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES,
                "max_retained_write_buffer_capacity_bytes": aether_provider_transport::CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
                "crypto_provider": aether_provider_transport::CODEX_OFFICIAL_WS_CRYPTO_PROVIDER,
            }
        }));
        key
    }

    fn resource_seed(kind: CatalogResourceKind, id: &str) -> CodexWsCatalogResourceSeed {
        CodexWsCatalogResourceSeed {
            kind,
            id: id.to_string(),
            eligible: true,
            ineligible_reason: "resource_ineligible",
        }
    }

    async fn resource_lease(
        state: &AppState,
        kind: CatalogResourceKind,
        id: &str,
    ) -> CodexWsCatalogResourceLease {
        let raw = state
            .runtime_state
            .kv_get(&catalog_resource_state_key(kind, id).expect("resource state key"))
            .await
            .expect("read resource state")
            .expect("resource state should exist");
        let snapshot = decode_catalog_resource_switch(&raw).expect("decode resource state");
        assert!(snapshot.stable);
        assert!(snapshot.eligible);
        CodexWsCatalogResourceLease {
            hard_generation: snapshot.hard_generation,
            drain_generation: snapshot.drain_generation,
        }
    }

    #[test]
    fn refreshable_access_token_expiry_is_not_a_shared_hard_blocker() {
        let mut key = eligible_key();
        key.expires_at_unix_secs = Some(0);
        assert_eq!(key_runtime_eligibility(&key), (true, "eligible"));
        assert_eq!(known_key_runtime_blocker(&key), None);
    }

    #[tokio::test]
    async fn catalog_mutation_impacts_have_distinct_bound_session_decisions() {
        let runtime = Arc::new(RuntimeState::memory(MemoryRuntimeStateConfig::default()));
        let state_a = AppState::new()
            .expect("state A")
            .with_runtime_state(Arc::clone(&runtime));
        let state_b = AppState::new()
            .expect("state B")
            .with_runtime_state(runtime);
        let seed = resource_seed(CatalogResourceKind::Provider, "provider-1");
        ensure_catalog_resource_hot_leases(&state_a, std::slice::from_ref(&seed))
            .await
            .expect("initialize provider resource state");
        let original = resource_lease(&state_a, seed.kind, &seed.id).await;

        let selection_only = begin_catalog_resource_hot_mutation(
            &state_a,
            &seed,
            CatalogMutationImpact::SelectionOnly,
        )
        .await
        .expect("begin selection-only mutation");
        let selection_transition_raw = state_b
            .runtime_state
            .kv_get(&catalog_resource_state_key(seed.kind, &seed.id).expect("state key"))
            .await
            .expect("read selection transition")
            .expect("selection transition should exist");
        assert_eq!(
            validate_catalog_resource_switch(
                Some(&selection_transition_raw),
                &original,
                "bound_provider_changed"
            ),
            Ok(CodexWsFenceDecision::Continue)
        );
        finish_catalog_resource_hot_mutation(&state_a, selection_only, true, None)
            .await
            .expect("finish selection-only mutation");
        assert_eq!(
            resource_lease(&state_b, seed.kind, &seed.id).await,
            original
        );

        let drain =
            begin_catalog_resource_hot_mutation(&state_a, &seed, CatalogMutationImpact::Drain)
                .await
                .expect("begin drain mutation");
        let transition_raw = state_b
            .runtime_state
            .kv_get(&catalog_resource_state_key(seed.kind, &seed.id).expect("state key"))
            .await
            .expect("read transition")
            .expect("transition should exist");
        assert_eq!(
            validate_catalog_resource_switch(
                Some(&transition_raw),
                &original,
                "bound_provider_changed"
            ),
            Ok(CodexWsFenceDecision::ContinueAndDrain)
        );
        finish_catalog_resource_hot_mutation(&state_a, drain, true, None)
            .await
            .expect("finish drain mutation");
        let after_drain = resource_lease(&state_b, seed.kind, &seed.id).await;
        assert_eq!(after_drain.hard_generation, original.hard_generation);
        assert_ne!(after_drain.drain_generation, original.drain_generation);

        let hard =
            begin_catalog_resource_hot_mutation(&state_a, &seed, CatalogMutationImpact::HardFence)
                .await
                .expect("begin hard mutation");
        let hard_transition_raw = state_b
            .runtime_state
            .kv_get(&catalog_resource_state_key(seed.kind, &seed.id).expect("state key"))
            .await
            .expect("read hard transition")
            .expect("hard transition should exist");
        assert_eq!(
            validate_catalog_resource_switch(
                Some(&hard_transition_raw),
                &after_drain,
                "bound_provider_changed"
            ),
            Err("bound_provider_changed")
        );
        finish_catalog_resource_hot_mutation(&state_a, hard, true, None)
            .await
            .expect("finish hard mutation");
        let after_hard_raw = state_b
            .runtime_state
            .kv_get(&catalog_resource_state_key(seed.kind, &seed.id).expect("state key"))
            .await
            .expect("read hard state")
            .expect("hard state should exist");
        assert_eq!(
            validate_catalog_resource_switch(
                Some(&after_hard_raw),
                &after_drain,
                "bound_provider_changed"
            ),
            Err("bound_provider_changed")
        );
    }

    #[tokio::test]
    async fn hard_generation_prevents_aba_after_transport_meaning_returns() {
        let state = AppState::new().expect("state should build");
        let seed = resource_seed(CatalogResourceKind::Endpoint, "endpoint-1");
        ensure_catalog_resource_hot_leases(&state, std::slice::from_ref(&seed))
            .await
            .expect("initialize endpoint resource state");
        let original = resource_lease(&state, seed.kind, &seed.id).await;

        for _ in 0..2 {
            let mutation = begin_catalog_resource_hot_mutation(
                &state,
                &seed,
                CatalogMutationImpact::HardFence,
            )
            .await
            .expect("begin hard mutation");
            finish_catalog_resource_hot_mutation(&state, mutation, true, None)
                .await
                .expect("finish hard mutation");
        }

        let actual_raw = state
            .runtime_state
            .kv_get(&catalog_resource_state_key(seed.kind, &seed.id).expect("state key"))
            .await
            .expect("read endpoint state")
            .expect("endpoint state should exist");
        assert_eq!(
            validate_catalog_resource_switch(
                Some(&actual_raw),
                &original,
                "bound_endpoint_changed"
            ),
            Err("bound_endpoint_changed")
        );
    }

    #[tokio::test]
    async fn stale_candidate_seed_cannot_reenable_an_existing_resource() {
        let state = AppState::new().expect("state should build");
        let seed = resource_seed(CatalogResourceKind::Provider, "provider-1");
        ensure_catalog_resource_hot_leases(&state, std::slice::from_ref(&seed))
            .await
            .expect("initialize provider resource state");
        let disable =
            begin_catalog_resource_hot_mutation(&state, &seed, CatalogMutationImpact::HardFence)
                .await
                .expect("begin provider disable");
        finish_catalog_resource_hot_mutation(&state, disable, false, Some("provider_inactive"))
            .await
            .expect("finish provider disable");
        let state_key = catalog_resource_state_key(seed.kind, &seed.id).expect("state key");
        let disabled_raw = state
            .runtime_state
            .kv_get(&state_key)
            .await
            .expect("read disabled state")
            .expect("disabled state should exist");

        let stale = ensure_catalog_resource_hot_leases(&state, std::slice::from_ref(&seed))
            .await
            .expect("stale reader should not fail");
        assert!(stale.providers.get(&seed.id).is_none());
        let after_stale_raw = state
            .runtime_state
            .kv_get(&state_key)
            .await
            .expect("read state after stale seed")
            .expect("state should still exist");
        assert_eq!(after_stale_raw, disabled_raw);
    }

    #[tokio::test]
    async fn selection_lease_detects_catalog_change_after_planning() {
        let state = AppState::new().expect("state should build");
        let global = ensure_switch(
            &state,
            GLOBAL_STATE_KEY,
            true,
            "global_codex_ws_disabled",
            None,
        )
        .await
        .expect("initialize global state");
        let catalog = ensure_catalog_hot_lease(&state)
            .await
            .expect("initialize catalog state");
        let mutation = begin_catalog_hot_mutation(&state)
            .await
            .expect("begin catalog change");
        assert_eq!(
            validate_candidate_selection_hot_leases(&state, Some(&global), &catalog).await,
            Err("account_catalog_changed_during_selection")
        );
        finish_catalog_hot_mutation(&state, mutation)
            .await
            .expect("finish catalog change");
        assert_eq!(
            validate_candidate_selection_hot_leases(&state, Some(&global), &catalog).await,
            Err("account_catalog_changed_during_selection")
        );
    }

    #[tokio::test]
    async fn unrelated_provider_mutation_does_not_change_bound_decision() {
        let runtime = Arc::new(RuntimeState::memory(MemoryRuntimeStateConfig::default()));
        let state_a = AppState::new()
            .expect("state A")
            .with_runtime_state(Arc::clone(&runtime));
        let state_b = AppState::new()
            .expect("state B")
            .with_runtime_state(runtime);
        let global = ensure_switch(
            &state_a,
            GLOBAL_STATE_KEY,
            true,
            "global_codex_ws_disabled",
            None,
        )
        .await
        .expect("initialize global state");
        let key = eligible_key();
        let key_lease = ensure_key_hot_leases(&state_a, std::slice::from_ref(&key))
            .await
            .expect("initialize key state")
            .remove(&key.id)
            .expect("key lease");
        let seeds = [
            resource_seed(CatalogResourceKind::Provider, "provider-1"),
            resource_seed(CatalogResourceKind::Endpoint, "endpoint-1"),
            resource_seed(CatalogResourceKind::Provider, "provider-2"),
        ];
        let resources = ensure_catalog_resource_hot_leases(&state_a, &seeds)
            .await
            .expect("initialize resources");
        let binding = resources
            .binding("provider-1", "endpoint-1")
            .expect("candidate binding");

        let unrelated = begin_catalog_resource_hot_mutation(
            &state_a,
            &seeds[2],
            CatalogMutationImpact::HardFence,
        )
        .await
        .expect("begin unrelated provider mutation");
        assert_eq!(
            validate_hot_leases(
                &state_b,
                "provider-1",
                "endpoint-1",
                &key.id,
                Some(&global.generation),
                &key_lease.generation,
                &binding,
            )
            .await,
            Ok(CodexWsFenceDecision::Continue)
        );
        finish_catalog_resource_hot_mutation(&state_a, unrelated, true, None)
            .await
            .expect("finish unrelated provider mutation");
        assert_eq!(
            validate_hot_leases(
                &state_b,
                "provider-1",
                "endpoint-1",
                &key.id,
                Some(&global.generation),
                &key_lease.generation,
                &binding,
            )
            .await,
            Ok(CodexWsFenceDecision::Continue)
        );
    }

    #[test]
    fn catalog_snapshot_generation_invalidates_local_planner_caches_once_per_generation() {
        let state = AppState::new().expect("state should build");
        let initial_scheduler_epoch = state.scheduler_affinity_epoch();
        let initial_transport_epoch = state
            .provider_transport_snapshot_cache_epoch
            .load(Ordering::Acquire);

        bind_catalog_snapshot_generation(&state, "generation-1")
            .expect("first generation should bind");
        let first_scheduler_epoch = state.scheduler_affinity_epoch();
        let first_transport_epoch = state
            .provider_transport_snapshot_cache_epoch
            .load(Ordering::Acquire);
        assert!(first_scheduler_epoch > initial_scheduler_epoch);
        assert!(first_transport_epoch > initial_transport_epoch);

        bind_catalog_snapshot_generation(&state, "generation-1")
            .expect("same generation should remain bound");
        assert_eq!(state.scheduler_affinity_epoch(), first_scheduler_epoch);
        assert_eq!(
            state
                .provider_transport_snapshot_cache_epoch
                .load(Ordering::Acquire),
            first_transport_epoch
        );

        bind_catalog_snapshot_generation(&state, "generation-2")
            .expect("new generation should bind");
        assert!(state.scheduler_affinity_epoch() > first_scheduler_epoch);
        assert!(
            state
                .provider_transport_snapshot_cache_epoch
                .load(Ordering::Acquire)
                > first_transport_epoch
        );
    }

    #[tokio::test]
    async fn global_hot_state_is_shared_across_gateway_instances() {
        let runtime = Arc::new(RuntimeState::memory(MemoryRuntimeStateConfig::default()));
        let state_a = AppState::new()
            .expect("state A")
            .with_runtime_state(Arc::clone(&runtime));
        let state_b = AppState::new()
            .expect("state B")
            .with_runtime_state(runtime);
        let enabled = ensure_switch(
            &state_a,
            GLOBAL_STATE_KEY,
            true,
            "global_codex_ws_disabled",
            None,
        )
        .await
        .expect("initialize shared state");
        assert!(enabled.eligible);

        let mutation = begin_global_hot_mutation(&state_a)
            .await
            .expect("begin disable mutation");
        let transition_generation = read_switch(&state_a, GLOBAL_STATE_KEY)
            .await
            .expect("read transition")
            .expect("transition should exist")
            .generation;
        finish_global_hot_mutation(&state_a, mutation, CodexWsFeatureFlags::default())
            .await
            .expect("finish disable mutation");

        let observed = ensure_global_hot_lease(&state_b)
            .await
            .expect("instance B should read shared state without a cold config lookup");
        assert!(!observed.eligible);
        assert_ne!(observed.generation, enabled.generation);
        assert_ne!(observed.generation, transition_generation);
    }

    #[tokio::test]
    async fn catalog_write_lock_serializes_provider_and_endpoint_classification() {
        let state = AppState::new().expect("state should build");
        let first = begin_catalog_write_lock(&state)
            .await
            .expect("first catalog writer should acquire the lock");
        let second = begin_catalog_write_lock(&state).await;
        assert!(
            matches!(second, Err(GatewayError::Internal(message)) if message.contains("catalog write is busy"))
        );
        release_catalog_write_lock(&state, first)
            .await
            .expect("first catalog writer should release the lock");
        let second = begin_catalog_write_lock(&state)
            .await
            .expect("second catalog writer should acquire after release");
        release_catalog_write_lock(&state, second)
            .await
            .expect("second catalog writer should release the lock");
    }

    #[tokio::test]
    async fn cancelled_hot_mutation_releases_its_lock() {
        let state = AppState::new().expect("state should build");
        ensure_switch(
            &state,
            GLOBAL_STATE_KEY,
            true,
            "global_codex_ws_disabled",
            None,
        )
        .await
        .expect("initialize state");
        let mutation = begin_global_hot_mutation(&state)
            .await
            .expect("first mutation should begin");
        drop(mutation);

        let second = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(mutation) = begin_global_hot_mutation(&state).await {
                    break mutation;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled mutation lock should be released promptly");
        leave_hot_mutation_unstable(&state, second).await;
    }

    #[tokio::test]
    async fn stale_db_writer_after_newer_stable_forces_shared_state_fail_closed() {
        let state = AppState::new().expect("state should build");
        ensure_switch(
            &state,
            GLOBAL_STATE_KEY,
            true,
            "global_codex_ws_disabled",
            None,
        )
        .await
        .expect("initialize state");

        let stale_writer = begin_global_hot_mutation(&state)
            .await
            .expect("stale writer should begin");
        let stale_lock = stale_writer
            .lock
            .lock
            .as_ref()
            .expect("stale writer should own a lock")
            .clone();
        state
            .runtime_state
            .lock_release(&stale_lock)
            .await
            .expect("simulate stale writer lease loss");
        stale_writer.lock.lease_lost.store(true, Ordering::Release);

        let newer_writer = begin_global_hot_mutation(&state)
            .await
            .expect("newer writer should begin");
        finish_global_hot_mutation(
            &state,
            newer_writer,
            CodexWsFeatureFlags {
                enabled: true,
                native_codex_ws_enabled: true,
            },
        )
        .await
        .expect("newer writer should publish stable enabled state");

        let stale_finish =
            finish_global_hot_mutation(&state, stale_writer, CodexWsFeatureFlags::default()).await;
        assert!(
            stale_finish.is_err(),
            "lost strict CAS must never report success"
        );
        let final_snapshot = read_switch(&state, GLOBAL_STATE_KEY)
            .await
            .expect("read final state")
            .expect("final state should exist");
        assert!(
            !hot_lease(final_snapshot).eligible,
            "the final shared state must match the stale writer's disabling DB commit or fail closed"
        );
    }

    #[tokio::test]
    async fn failed_finish_fences_an_observed_unstable_writer_before_it_can_widen() {
        let state = AppState::new().expect("state should build");
        ensure_switch(
            &state,
            GLOBAL_STATE_KEY,
            true,
            "global_codex_ws_disabled",
            None,
        )
        .await
        .expect("initialize state");

        let lost_writer = begin_global_hot_mutation(&state)
            .await
            .expect("lost writer should begin");
        let lost_lock = lost_writer
            .lock
            .lock
            .as_ref()
            .expect("lost writer should own a lock")
            .clone();
        state
            .runtime_state
            .lock_release(&lost_lock)
            .await
            .expect("simulate lease loss");
        lost_writer.lock.lease_lost.store(true, Ordering::Release);

        let later_writer = begin_global_hot_mutation(&state)
            .await
            .expect("later writer should publish its transition");
        let later_transition_generation = read_switch(&state, GLOBAL_STATE_KEY)
            .await
            .expect("read later transition")
            .expect("later transition should exist")
            .generation;
        assert!(
            finish_global_hot_mutation(&state, lost_writer, CodexWsFeatureFlags::default())
                .await
                .is_err()
        );
        let recovery = read_switch(&state, GLOBAL_STATE_KEY)
            .await
            .expect("read recovery state")
            .expect("recovery state should exist");
        assert!(!recovery.stable);
        assert_ne!(recovery.generation, later_transition_generation);

        assert!(
            finish_global_hot_mutation(
                &state,
                later_writer,
                CodexWsFeatureFlags {
                    enabled: true,
                    native_codex_ws_enabled: true,
                },
            )
            .await
            .is_err(),
            "the fenced later transition must not widen to eligible"
        );
        let final_snapshot = read_switch(&state, GLOBAL_STATE_KEY)
            .await
            .expect("read final state")
            .expect("final state should exist");
        assert!(!hot_lease(final_snapshot).eligible);
    }

    #[tokio::test]
    async fn locked_key_meaning_changes_always_advance_generation() {
        let state = AppState::new().expect("state should build");
        let eligible = eligible_key();
        let initial = ensure_key_hot_leases(&state, std::slice::from_ref(&eligible))
            .await
            .expect("initialize key hot state")
            .remove(&eligible.id)
            .expect("initial lease");

        let mut blocked = eligible.clone();
        blocked.is_active = false;
        let mutation = begin_key_hot_mutation(&state, &blocked.id)
            .await
            .expect("begin restrictive mutation");
        finish_key_hot_mutation(&state, mutation, Some(&blocked))
            .await
            .expect("publish restrictive state");
        let blocked_snapshot =
            read_switch(&state, &key_state_key(&eligible.id).expect("state key"))
                .await
                .expect("read blocked state")
                .expect("blocked state");
        assert!(!blocked_snapshot.eligible);
        assert_ne!(blocked_snapshot.generation, initial.generation);

        let mutation = begin_key_hot_mutation(&state, &eligible.id)
            .await
            .expect("begin recovery mutation");
        finish_key_hot_mutation(&state, mutation, Some(&eligible))
            .await
            .expect("publish recovered state");
        let recovered = read_switch(&state, &key_state_key(&eligible.id).expect("state key"))
            .await
            .expect("read recovered state")
            .expect("recovered state");
        assert!(recovered.eligible);
        assert_ne!(recovered.generation, blocked_snapshot.generation);
    }
}
