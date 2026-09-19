//! Opt-in provider turn-state collection and Codex pool egress override.
//! Tickets are credentials: never log them or put them in provider config.
use std::collections::BTreeMap;
use std::time::Duration;

use aether_contracts::{ExecutionPlan, ExecutionResult, RequestBody};
use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogProvider;
use aether_runtime_state::RuntimeState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::provider_transport::GatewayProviderTransportSnapshot;
use crate::AppState;

mod status;
pub(crate) use status::collection_status;

pub(crate) const HEADER: &str = "x-codex-turn-state";
const SOURCE_CONFIG: &str = "turn_state_collection";
const LEGACY_SOURCE_CONFIG: &str = "congming_turn_state";
const OVERRIDE_CONFIG: &str = "turn_state_source_provider_id";
const CACHE_PREFIX: &str = "aether:turn_state:v1:source:";
const REFRESH_INTERVAL: Duration = Duration::from_secs(40 * 60);
// User-observed lifetime is approximately one hour, not a verified upstream SLA.
// This is a local maximum age, never a promise that upstream accepts the ticket.
const TICKET_TTL: Duration = Duration::from_secs(60 * 60);
const SCAN_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceConfig {
    enabled: bool,
    #[serde(default)]
    models: Vec<String>,
}

pub(crate) fn validate_config(config: &Map<String, Value>) -> Result<(), String> {
    if let Some(value) = config
        .get(SOURCE_CONFIG)
        .or_else(|| config.get(LEGACY_SOURCE_CONFIG))
        .filter(|value| !value.is_null())
    {
        parse_source_config(value)?;
    }

    if let Some(value) = config
        .get("pool_advanced")
        .and_then(|pool| pool.get(OVERRIDE_CONFIG))
    {
        if !value.is_null() && !value.as_str().is_some_and(valid_source_id) {
            return Err("turn_state_source_provider_id 必须是渠道 ID 字符串或 null（关闭）".into());
        }
    }
    Ok(())
}

fn parse_source_config(value: &Value) -> Result<SourceConfig, String> {
    let mut source: SourceConfig = serde_json::from_value(value.clone()).map_err(|_| {
        "Turn-State 票据采集配置须包含 enabled 布尔值和 models 字符串列表".to_string()
    })?;
    if source.models.len() > 64
        || source.models.iter().any(|model| {
            model.trim().is_empty() || model.len() > 200 || model.chars().any(char::is_control)
        })
    {
        return Err("采集模型最多 64 个，每个模型名须为非空字符串且不超过 200 字节".into());
    }
    source.models = source
        .models
        .into_iter()
        .map(|model| model.trim().to_owned())
        .collect();
    source.models.sort();
    source.models.dedup();
    if source.enabled && source.models.is_empty() {
        return Err("启用 Turn-State 票据采集时必须填写模型列表".into());
    }
    Ok(source)
}

/// Read the old collection setting until the channel is next saved. The old
/// pool boolean is deliberately NOT interpreted as a source: never guess.
pub(crate) fn collection_config(config: &Map<String, Value>) -> Option<&Value> {
    config
        .get(SOURCE_CONFIG)
        .or_else(|| config.get(LEGACY_SOURCE_CONFIG))
}

fn source_config(provider: &StoredProviderCatalogProvider) -> Option<SourceConfig> {
    let config =
        parse_source_config(collection_config(provider.config.as_ref()?.as_object()?)?).ok()?;
    (provider.is_active && config.enabled).then_some(config)
}

fn valid_source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

pub(crate) fn source_provider_id(transport: &GatewayProviderTransportSnapshot) -> Option<&str> {
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return None;
    }
    transport
        .provider
        .config
        .as_ref()?
        .get("pool_advanced")?
        .get(OVERRIDE_CONFIG)?
        .as_str()
        .filter(|value| valid_source_id(value))
}

fn cache_key(provider_id: &str) -> String {
    format!("{CACHE_PREFIX}{provider_id}")
}

fn valid_ticket(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4 * 1024
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[derive(Serialize, Deserialize)]
struct Ticket {
    value: String,
    fetched_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct TicketCache {
    source_provider_id: String,
    tickets: BTreeMap<String, Ticket>,
    #[serde(default)]
    attempts: BTreeMap<String, status::CollectionAttempt>,
}

impl TicketCache {
    /// Exact final upstream model only: never cross models or fall back to
    /// the downstream alias, even if no ticket exists for the wire model.
    pub(crate) fn for_body(&self, body: &Value) -> Option<&str> {
        let model = body.get("model")?.as_str()?;
        self.for_model(model)
    }

    fn for_model(&self, model: &str) -> Option<&str> {
        let ticket = self.tickets.get(model.trim())?;
        let age = crate::codex_client_release::unix_now_secs().checked_sub(ticket.fetched_at)?;
        (age < TICKET_TTL.as_secs() && valid_ticket(&ticket.value)).then_some(ticket.value.as_str())
    }
}

pub(crate) async fn load_tickets(
    runtime: &RuntimeState,
    source_id: Option<&str>,
) -> Option<TicketCache> {
    let source_id = source_id.filter(|value| valid_source_id(value))?;
    let raw = runtime.kv_get(&cache_key(source_id)).await.ok()??;
    let cache: TicketCache = serde_json::from_str(&raw).ok()?;
    (cache.source_provider_id == source_id).then_some(cache)
}

/// Must run AFTER runtime identity sanitation and endpoint rules. No inbound
/// header or matching turn is required for an explicitly enabled override.
pub(crate) fn apply_ticket(
    headers: &mut BTreeMap<String, String>,
    body: Option<&mut Value>,
    ticket: &str,
    websocket: bool,
) {
    if !valid_ticket(ticket) {
        return;
    }
    headers.retain(|name, _| !name.eq_ignore_ascii_case(HEADER));
    headers.insert(HEADER.into(), ticket.into());
    if let Some(object) = body.and_then(Value::as_object_mut) {
        // WS response.create carries the per-step header in client_metadata.
        // For HTTP only replace a body projection if the client supplied one.
        if websocket {
            let meta = object.entry("client_metadata").or_insert_with(|| json!({}));
            if !meta.is_object() {
                *meta = json!({});
            }
        }
        if let Some(meta) = object
            .get_mut("client_metadata")
            .and_then(Value::as_object_mut)
        {
            if websocket || meta.keys().any(|name| name.eq_ignore_ascii_case(HEADER)) {
                meta.retain(|name, _| !name.eq_ignore_ascii_case(HEADER));
                meta.insert(HEADER.into(), Value::String(ticket.into()));
            }
        }
    }
}

fn is_collection_url(raw: &str) -> bool {
    url::Url::parse(raw).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn ticket_from_response(result: &ExecutionResult) -> Result<String, String> {
    if !(200..300).contains(&result.status_code) || result.error.is_some() {
        return Err(format!("采集响应失败（HTTP {}）", result.status_code));
    }
    let values = result
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(HEADER))
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(format!(
            "来源返回 HTTP {}，但未返回 x-codex-turn-state 响应头；请确认来源支持生成票据并开启该响应头透传。普通对话成功不代表采集成功。",
            result.status_code
        ));
    }
    if values.len() != 1 || !valid_ticket(values[0]) {
        return Err(
            "来源返回的 x-codex-turn-state 无效（须为单个非空 ASCII 票据，最多 4096 字节）".into(),
        );
    }
    Ok(values[0].clone())
}

async fn build_fetch_plan(
    state: &AppState,
    transport: &GatewayProviderTransportSnapshot,
    model: &str,
) -> Result<ExecutionPlan, String> {
    // Reuse existing credential, proxy and transport-profile resolution; this
    // builds a plan only and does NOT issue a /models request.
    let mut plan = aether_model_fetch::build_models_fetch_execution_plan(state, transport)
        .await
        .map_err(|_| "来源渠道认证或传输配置无效")?;
    // The collection request is a Codex Responses conversation, not a model
    // listing. Use the shared bundled client identity unless the endpoint
    // has explicitly configured its own UA/originator.
    if plan
        .headers
        .get("user-agent")
        .is_none_or(|ua| ua == "openai-codex/1.0")
    {
        let profiles: Vec<Value> = serde_json::from_str(include_str!(
            "../../../resources/codex-client-header-profiles.json"
        ))
        .map_err(|_| "内置 Codex 客户端配置无效")?;
        if let Some(profile) = profiles.first() {
            if let (Some(ua), Some(originator)) = (
                profile["user_agent"].as_str(),
                profile["originator"].as_str(),
            ) {
                crate::codex_profile::apply_codex_client_identity_headers(
                    &mut plan.headers,
                    ua,
                    originator,
                );
            }
        }
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    for header in ["session-id", "thread-id", "x-client-request-id"] {
        plan.headers.insert(header.into(), session_id.clone());
    }
    let body = json!({
        "model": model.trim(), "stream": true, "store": false,
        "instructions": "Reply with OK only.",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "Hi"}]}]
    });
    plan.url = crate::provider_transport::build_transport_request_url(
        transport,
        crate::provider_transport::TransportRequestUrlParams {
            provider_api_format: "openai:responses",
            mapped_model: Some(model.trim()),
            upstream_is_stream: true,
            request_query: None,
            kiro_api_region: None,
        },
    )
    .ok_or_else(|| "来源渠道 Responses 端点无效".to_string())?;
    if !is_collection_url(&plan.url) {
        return Err("票据采集需要有效的 HTTP(S) Responses 端点".into());
    }
    plan.request_id = format!("turn-state-collection-{}", uuid::Uuid::new_v4());
    plan.method = "POST".into();
    plan.headers
        .retain(|name, _| !name.eq_ignore_ascii_case(HEADER));
    plan.headers
        .insert("accept".into(), "text/event-stream".into());
    plan.headers
        .insert("content-type".into(), "application/json".into());
    plan.content_type = Some("application/json".into());
    plan.model_name = Some(model.trim().into());
    plan.body = RequestBody::from_json(body);
    plan.stream = true;
    Ok(plan)
}

async fn fetch_ticket(state: &AppState, provider_id: &str, model: &str) -> Result<String, String> {
    let ids = [provider_id.to_string()];
    let endpoints = state
        .list_provider_catalog_endpoints_by_provider_ids(&ids)
        .await
        .map_err(|_| "读取来源渠道端点失败")?;
    let endpoint = endpoints
        .iter()
        .find(|endpoint| {
            endpoint.is_active
                && crate::ai_serving::normalize_api_format_alias(&endpoint.api_format)
                    == "openai:responses"
                && is_collection_url(&endpoint.base_url)
        })
        .ok_or("来源渠道没有启用的 Responses 端点")?;
    let keys = state
        .list_provider_catalog_keys_by_provider_ids(&ids)
        .await
        .map_err(|_| "读取来源渠道密钥失败")?;
    let now = crate::codex_client_release::unix_now_secs();
    let key = keys
        .iter()
        .find(|key| {
            key.is_active
                && key.expires_at_unix_secs.is_none_or(|expires| expires > now)
                && key
                    .api_formats
                    .as_ref()
                    .filter(|v| !v.is_null())
                    .is_none_or(|value| {
                        value.as_array().is_some_and(|formats| {
                            formats.iter().filter_map(Value::as_str).any(|format| {
                                crate::ai_serving::normalize_api_format_alias(format)
                                    == "openai:responses"
                            })
                        })
                    })
                && key
                    .allowed_models
                    .as_ref()
                    .filter(|v| !v.is_null())
                    .is_none_or(|value| {
                        value.as_array().is_some_and(|models| {
                            models
                                .iter()
                                .any(|allowed| allowed.as_str() == Some(model.trim()))
                        })
                    })
        })
        .ok_or("来源渠道没有可用于采集模型的启用密钥")?;
    let transport = state
        .read_provider_transport_snapshot(provider_id, &endpoint.id, &key.id)
        .await
        .map_err(|_| "读取来源渠道传输配置失败")?
        .ok_or("来源渠道传输配置不存在")?;
    let plan = build_fetch_plan(state, &transport, model).await?;
    let result = crate::execution_runtime::execute_execution_runtime_sync_plan(state, None, &plan)
        .await
        .map_err(|_| "Turn-State 票据采集请求失败")?;
    ticket_from_response(&result)
}

async fn scan(state: &AppState) -> Result<(), String> {
    scan_with_fetch(state, fetch_ticket).await
}

async fn scan_with_fetch(
    state: &AppState,
    fetch: impl AsyncFn(&AppState, &str, &str) -> Result<String, String> + Sync,
) -> Result<(), String> {
    use futures_util::{stream, StreamExt};
    let providers = state
        .list_provider_catalog_providers(false)
        .await
        .map_err(|_| "读取票据采集渠道失败")?;
    let runtime = &state.runtime_state;
    // Deleted channels must not leave usable credentials behind. Do not
    // delete unrelated runtime keys or the previous version's global cache.
    let cached_keys = runtime
        .scan_keys(&format!("{CACHE_PREFIX}*"), 100)
        .await
        .map_err(|_| "读取票据缓存目录失败")?;
    for key in &cached_keys {
        let key = runtime.strip_namespace(key);
        if !providers
            .iter()
            .any(|provider| cache_key(&provider.id) == key)
        {
            runtime
                .kv_delete(key)
                .await
                .map_err(|_| "清除已删除来源票据失败")?;
        }
    }
    let sources: Vec<_> = providers
        .into_iter()
        .filter(|provider| {
            source_config(provider).is_some()
                || cached_keys
                    .iter()
                    .any(|key| runtime.strip_namespace(key) == cache_key(&provider.id))
        })
        .collect();
    let mut pending = Vec::new();
    for provider in sources {
        let fetch = &fetch;
        pending.push(async move {
            let provider_id = &provider.id;
            let lock_key = format!("aether:turn_state:v1:scan:{provider_id}");
            let lease = runtime
                .lock_try_acquire(&lock_key, "turn_state_collection", Duration::from_secs(75))
                .await
                .map_err(|_| "获取票据采集锁失败".to_string())?;
            let Some(lease) = lease else {
                return Ok(());
            };
            let result = tokio::time::timeout(
                Duration::from_secs(60),
                scan_source(state, &provider, fetch),
            )
            .await
            .unwrap_or_else(|_| Err("票据采集请求超时".into()));
            let _ = runtime.lock_release(&lease).await;
            if let Err(reason) = &result {
                tracing::warn!(provider_id, %reason, "turn-state collection failed");
            }
            result
        });
    }
    let mut results = stream::iter(pending).buffer_unordered(4);
    let mut failed = false;
    while let Some(result) = results.next().await {
        if result.is_err() {
            failed = true;
        }
    }
    if failed {
        Err("部分渠道票据采集失败".into())
    } else {
        Ok(())
    }
}

async fn scan_source(
    state: &AppState,
    provider: &StoredProviderCatalogProvider,
    fetch: &impl AsyncFn(&AppState, &str, &str) -> Result<String, String>,
) -> Result<(), String> {
    let runtime = &state.runtime_state;
    let provider_id = &provider.id;
    let key = cache_key(provider_id);
    // Re-read under the source lock: a waiting scan must not resurrect a
    // channel that was disabled while another scan was fetching it.
    let current = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(provider_id))
        .await
        .map_err(|_| "读取票据采集开关失败")?;
    let Some(config) = current.first().and_then(source_config) else {
        runtime
            .kv_delete(&key)
            .await
            .map_err(|_| "清除停用票据失败")?;
        return Ok(());
    };
    let mut cache: TicketCache = runtime
        .kv_get(&key)
        .await
        .map_err(|_| "读取票据缓存失败")?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if cache.source_provider_id != *provider_id {
        cache = TicketCache {
            source_provider_id: provider_id.clone(),
            ..Default::default()
        };
    }
    // Removed models and expired credentials are discarded before fetching.
    let now = crate::codex_client_release::unix_now_secs();
    cache.tickets.retain(|model, ticket| {
        config.models.contains(model)
            && valid_ticket(&ticket.value)
            && now
                .checked_sub(ticket.fetched_at)
                .is_some_and(|age| age < TICKET_TTL.as_secs())
    });
    cache
        .attempts
        .retain(|model, _| config.models.contains(model));
    save_cache(runtime, &cache).await?;
    for model in &config.models {
        // A shared, expiring marker prevents duplicate paid requests on
        // replicas, errors and restarts. Only one model per scan keeps the
        // lock bounded; the others are picked up at the next 15-second tick.
        if !runtime
            .kv_set_if_absent(&attempt_key(provider_id, model), "1", REFRESH_INTERVAL)
            .await
            .map_err(|_| "读取票据采集调度状态失败")?
        {
            continue;
        }
        let started_at = crate::codex_client_release::unix_now_secs();
        let attempt = cache.attempts.entry(model.clone()).or_default();
        attempt.started_at = started_at;
        attempt.finished_at = None;
        attempt.error = None;
        save_cache(runtime, &cache).await?;
        // Leave time inside the outer source lease to persist a timeout result.
        let result =
            tokio::time::timeout(Duration::from_secs(50), fetch(state, provider_id, model))
                .await
                .unwrap_or_else(|_| Err("票据采集请求超时".into()))
                .and_then(|value| {
                    if valid_ticket(&value) {
                        Ok(value)
                    } else {
                        Err("采集响应票据无效".into())
                    }
                });
        // Do not republish after an in-flight source/model switch change.
        let current = state
            .read_provider_catalog_providers_by_ids(std::slice::from_ref(provider_id))
            .await
            .map_err(|_| "读取票据采集开关失败")?;
        if current.first().and_then(source_config).as_ref() != Some(&config) {
            runtime
                .kv_delete(&key)
                .await
                .map_err(|_| "清除停用票据失败")?;
            return Ok(());
        }
        let attempt = cache
            .attempts
            .get_mut(model)
            .expect("attempt recorded before fetching");
        attempt.finished_at = Some(crate::codex_client_release::unix_now_secs());
        match result {
            Ok(value) => {
                attempt.last_success_at = Some(started_at);
                cache.tickets.insert(
                    model.clone(),
                    Ticket {
                        value,
                        fetched_at: started_at,
                    },
                );
            }
            Err(reason) => {
                // fetch_ticket returns bounded, sanitized messages, never raw
                // upstream bodies, header values or credentials.
                attempt.error = Some(reason.clone());
                save_cache(runtime, &cache).await?;
                tracing::warn!(provider_id, model, %reason, "turn-state model collection failed");
                return Err(reason);
            }
        }
        save_cache(runtime, &cache).await?;
        tracing::info!(provider_id, model, "turn-state collection refreshed");
        break;
    }
    Ok(())
}

fn attempt_key(provider_id: &str, model: &str) -> String {
    let model_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, model.as_bytes());
    format!("aether:turn_state:v1:attempt:{provider_id}:{model_id}")
}

async fn save_cache(runtime: &RuntimeState, cache: &TicketCache) -> Result<(), String> {
    runtime
        .kv_set(
            &cache_key(&cache.source_provider_id),
            serde_json::to_string(cache).map_err(|_| "编码票据失败")?,
            Some(TICKET_TTL),
        )
        .await
        .map_err(|_| "保存票据缓存失败".into())
}

pub(crate) fn spawn_worker(state: AppState) -> Option<tokio::task::JoinHandle<()>> {
    if !state.has_provider_catalog_data_reader() {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(SCAN_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(reason) = scan(&state).await {
                tracing::warn!(%reason, "turn-state collection scan failed");
            }
        }
    }))
}

#[cfg(test)]
pub(crate) mod tests;
