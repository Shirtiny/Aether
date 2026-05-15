use std::collections::BTreeMap;

use super::{
    admin_provider_pool_config, build_admin_pool_error_response,
    read_admin_provider_pool_cooldown_counts,
    ADMIN_POOL_PROVIDER_CATALOG_READER_UNAVAILABLE_DETAIL,
};
use crate::handlers::admin::provider::shared::support::admin_provider_pool_quota_probe_active_members_key;
use crate::handlers::admin::request::AdminAppState;
use crate::maintenance::PoolQuotaProbeWorkerConfig;
use crate::provider_pool_demand::{
    provider_pool_burst_pending, read_provider_pool_demand_snapshot,
};
use crate::GatewayError;
use aether_admin::provider::pool as admin_provider_pool_pure;
use axum::{
    body::Body,
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

pub(super) async fn build_admin_pool_overview_response(
    state: &AdminAppState<'_>,
) -> Result<Response<Body>, GatewayError> {
    if !state.has_provider_catalog_data_reader() {
        return Ok(build_admin_pool_error_response(
            http::StatusCode::SERVICE_UNAVAILABLE,
            ADMIN_POOL_PROVIDER_CATALOG_READER_UNAVAILABLE_DETAIL,
        ));
    }

    let providers = state.list_provider_catalog_providers(false).await?;
    let pool_enabled_providers = providers
        .into_iter()
        .filter_map(|provider| {
            admin_provider_pool_config(&provider).map(|config| (provider, config))
        })
        .collect::<Vec<_>>();
    let provider_ids = pool_enabled_providers
        .iter()
        .map(|(provider, _)| provider.id.clone())
        .collect::<Vec<_>>();
    let (key_stats_result, cooldown_counts_by_provider) = tokio::join!(
        async {
            if provider_ids.is_empty() {
                Ok(Vec::new())
            } else {
                state
                    .list_provider_catalog_key_stats_by_provider_ids(&provider_ids)
                    .await
            }
        },
        async {
            if provider_ids.is_empty() {
                std::collections::BTreeMap::new()
            } else {
                read_admin_provider_pool_cooldown_counts(state.runtime_state(), &provider_ids).await
            }
        },
    );
    let key_stats = key_stats_result?;
    let key_stats_by_provider = key_stats
        .into_iter()
        .map(|item| (item.provider_id.clone(), item))
        .collect::<BTreeMap<_, _>>();

    let probe_config = PoolQuotaProbeWorkerConfig::from_env();
    let mut runtime_metrics_by_provider = BTreeMap::new();
    for (provider, pool_config) in &pool_enabled_providers {
        let active_keys = key_stats_by_provider
            .get(&provider.id)
            .map(|item| item.active_keys as usize)
            .unwrap_or(0);
        let hot_count = if pool_config.probing_enabled {
            state
                .runtime_state()
                .set_len(&admin_provider_pool_quota_probe_active_members_key(
                    &provider.id,
                ))
                .await
                .unwrap_or(0)
        } else {
            0
        };
        let demand_snapshot = read_provider_pool_demand_snapshot(
            state.runtime_state(),
            &provider.id,
            active_keys,
            probe_config.max_keys_per_provider,
        )
        .await;
        let burst_pending = pool_config.probing_enabled
            && provider_pool_burst_pending(state.runtime_state(), &provider.id).await;
        runtime_metrics_by_provider.insert(
            provider.id.clone(),
            json!({
                "provider_hot_count": hot_count,
                "provider_desired_hot": if pool_config.probing_enabled {
                    demand_snapshot.desired_hot
                } else {
                    0
                },
                "provider_in_flight": demand_snapshot.in_flight,
                "provider_ema_in_flight": demand_snapshot.ema_in_flight,
                "provider_burst_pending": burst_pending,
            }),
        );
    }

    let providers = pool_enabled_providers
        .into_iter()
        .map(|(provider, _)| provider)
        .collect::<Vec<_>>();

    let mut payload = admin_provider_pool_pure::build_admin_pool_overview_payload(
        &providers,
        &key_stats_by_provider,
        &cooldown_counts_by_provider,
    );
    if let Some(items) = payload.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            let Some(provider_id) = item.get("provider_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(metrics) = runtime_metrics_by_provider.get(provider_id) else {
                continue;
            };
            let Some(item_object) = item.as_object_mut() else {
                continue;
            };
            if let Some(metrics_object) = metrics.as_object() {
                for (key, value) in metrics_object {
                    item_object.insert(key.clone(), value.clone());
                }
            }
        }
    }

    Ok(Json(payload).into_response())
}
