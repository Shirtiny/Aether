use super::keys::{pool_cooldown_index_key, pool_cooldown_key};
use crate::handlers::admin::request::AdminAppState;
use aether_runtime_state::RuntimeState;

/// Clear only the transient cooldown created by the matching upstream status.
/// The compare-and-delete avoids erasing a newer auth/account cooldown when a
/// quota probe finishes after another request has already failed.
pub(crate) async fn clear_admin_provider_pool_cooldown_if_reason_runtime(
    runtime: &RuntimeState,
    provider_id: &str,
    key_id: &str,
    expected_reason: &str,
) -> bool {
    let key = pool_cooldown_key(provider_id, key_id);
    let Ok(deleted) = runtime.kv_delete_if_value(&key, expected_reason).await else {
        return false;
    };
    if deleted {
        let _ = runtime
            .set_remove(&pool_cooldown_index_key(provider_id), key_id)
            .await;
    }
    deleted
}

pub(crate) async fn clear_admin_provider_pool_cooldown(
    state: &AdminAppState<'_>,
    provider_id: &str,
    key_id: &str,
) {
    let _ = state
        .runtime_state()
        .kv_delete(&pool_cooldown_key(provider_id, key_id))
        .await;
    let _ = state
        .runtime_state()
        .set_remove(&pool_cooldown_index_key(provider_id), key_id)
        .await;
}

pub(crate) async fn reset_admin_provider_pool_cost(
    state: &AdminAppState<'_>,
    provider_id: &str,
    key_id: &str,
) {
    let _ = state
        .runtime_state()
        .score_remove_by_score(&format!("ap:{provider_id}:cost:{key_id}"), f64::INFINITY)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_runtime_state::MemoryRuntimeStateConfig;
    use std::time::Duration;

    #[tokio::test]
    async fn conditional_cooldown_clear_removes_only_matching_rate_limit_state() {
        let runtime = RuntimeState::memory(MemoryRuntimeStateConfig::default());
        let cooldown_key = pool_cooldown_key("provider-1", "key-1");
        let cooldown_index = pool_cooldown_index_key("provider-1");
        runtime
            .kv_set(
                &cooldown_key,
                "rate_limited_429",
                Some(Duration::from_secs(60)),
            )
            .await
            .expect("cooldown should be stored");
        runtime
            .set_add(&cooldown_index, "key-1")
            .await
            .expect("cooldown index should be updated");

        assert!(
            clear_admin_provider_pool_cooldown_if_reason_runtime(
                &runtime,
                "provider-1",
                "key-1",
                "rate_limited_429",
            )
            .await
        );
        assert!(runtime
            .kv_get(&cooldown_key)
            .await
            .expect("cooldown should be readable")
            .is_none());
        assert!(!runtime
            .set_members(&cooldown_index)
            .await
            .expect("cooldown index should be readable")
            .iter()
            .any(|key| key == "key-1"));
    }

    #[tokio::test]
    async fn conditional_cooldown_clear_keeps_newer_non_rate_limit_state() {
        let runtime = RuntimeState::memory(MemoryRuntimeStateConfig::default());
        let cooldown_key = pool_cooldown_key("provider-1", "key-1");
        runtime
            .kv_set(
                &cooldown_key,
                "forbidden_403",
                Some(Duration::from_secs(60)),
            )
            .await
            .expect("cooldown should be stored");

        assert!(
            !clear_admin_provider_pool_cooldown_if_reason_runtime(
                &runtime,
                "provider-1",
                "key-1",
                "rate_limited_429",
            )
            .await
        );
        assert_eq!(
            runtime
                .kv_get(&cooldown_key)
                .await
                .expect("cooldown should be readable"),
            Some("forbidden_403".to_string())
        );
    }
}
