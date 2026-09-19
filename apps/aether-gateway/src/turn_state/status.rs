use super::*;

#[derive(Default, Serialize, Deserialize)]
pub(super) struct CollectionAttempt {
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub last_success_at: Option<u64>,
    pub error: Option<String>,
}

/// Admin-only projection. Never serialize TicketCache itself into an API.
/// Looking at status must not fetch tickets, change scheduling or expose secrets.
pub(crate) async fn collection_status(
    runtime: &RuntimeState,
    provider: &StoredProviderCatalogProvider,
) -> Value {
    let config = provider
        .config
        .as_ref()
        .and_then(Value::as_object)
        .and_then(collection_config)
        .and_then(|v| parse_source_config(v).ok());
    source_status(runtime, &provider.id, config, provider.is_active).await
}

pub(crate) async fn key_collection_status(
    runtime: &RuntimeState,
    provider: &StoredProviderCatalogProvider,
    key: &aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey,
) -> Value {
    let config = key_collection_config(key).and_then(|v| parse_source_config(v).ok());
    source_status(
        runtime,
        &account_source_id(&provider.id, &key.id),
        config,
        provider.is_active && key.provider_id == provider.id,
    )
    .await
}

async fn source_status(
    runtime: &RuntimeState,
    source_id: &str,
    config: Option<SourceConfig>,
    active: bool,
) -> Value {
    let enabled = config.as_ref().is_some_and(|c| c.enabled) && active;
    let now = crate::codex_client_release::unix_now_secs();
    let mut available = true;
    let cache = match runtime.kv_get(&cache_key(source_id)).await {
        Ok(Some(raw)) => match serde_json::from_str::<TicketCache>(&raw) {
            Ok(cache) if cache.source_provider_id == source_id => Some(cache),
            _ => {
                available = false;
                None
            }
        },
        Ok(None) => None,
        Err(_) => {
            available = false;
            None
        }
    };
    let mut models = Vec::new();
    for model in config.map(|c| c.models).unwrap_or_default() {
        let attempt = cache.as_ref().and_then(|c| c.attempts.get(&model));
        let ticket = cache.as_ref().and_then(|c| c.tickets.get(&model));
        let valid = enabled && cache.as_ref().and_then(|c| c.for_model(&model)).is_some();
        let next_attempt_at = if enabled {
            match runtime
                .kv_ttl_seconds(&attempt_key(source_id, &model))
                .await
            {
                Ok(Some(ttl)) if ttl >= 0 => Some(now.saturating_add(ttl as u64)),
                Ok(_) => None,
                Err(_) => {
                    available = false;
                    None
                }
            }
        } else {
            None
        };
        let interrupted = attempt
            .is_some_and(|a| a.finished_at.is_none() && now.saturating_sub(a.started_at) >= 60);
        let result = if !enabled {
            "disabled"
        } else if interrupted {
            "failed"
        } else if attempt.is_some_and(|a| a.finished_at.is_none()) {
            "collecting"
        } else if attempt.is_some_and(|a| a.error.is_some()) {
            "failed"
        } else if attempt.is_some() {
            "success"
        } else if valid {
            "success"
        } else if next_attempt_at.is_some() {
            "unknown"
        } else {
            "pending"
        };
        let last_success_at = attempt
            .and_then(|a| a.last_success_at)
            .or_else(|| ticket.map(|t| t.fetched_at));
        models.push(json!({
            "model": model,
            "result": result,
            "last_attempt_at": attempt.map(|a| a.started_at),
            "last_success_at": last_success_at,
            "next_attempt_at": next_attempt_at,
            "error": if interrupted { Some("上次采集超时或进程中断，等待下次调度") } else { attempt.and_then(|a| a.error.as_deref()) },
            "ticket_valid": valid,
            "ticket_length": if valid { ticket.map(|t| t.value.len()) } else { None },
            "expires_at": last_success_at.map(|t| t.saturating_add(TICKET_TTL.as_secs())),
        }));
    }
    json!({"enabled": enabled, "available": available, "checked_at": now, "models": models})
}
