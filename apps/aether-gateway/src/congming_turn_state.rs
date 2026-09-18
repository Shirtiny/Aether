//! Opt-in Congming turn-state collection and Codex pool egress override.
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

pub(crate) const HEADER: &str = "x-codex-turn-state";
const SOURCE_CONFIG: &str = "congming_turn_state";
const OVERRIDE_CONFIG: &str = "congming_turn_state_override";
const TICKET_KEY: &str = "aether:congming_turn_state:v1:ticket";
const LOCK_KEY: &str = "aether:congming_turn_state:v1:scan";
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
    if let Some(value) = config.get(SOURCE_CONFIG).filter(|value| !value.is_null()) {
        parse_source_config(value)?;
    }

    if let Some(value) = config
        .get("pool_advanced")
        .and_then(|pool| pool.get(OVERRIDE_CONFIG))
    {
        if !value.is_boolean() {
            return Err("congming_turn_state_override 必须是布尔值".into());
        }
    }
    Ok(())
}

fn parse_source_config(value: &Value) -> Result<SourceConfig, String> {
    let mut source: SourceConfig = serde_json::from_value(value.clone())
        .map_err(|_| "聪明票据采集配置须包含 enabled 布尔值和 models 字符串列表".to_string())?;
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
        return Err("启用聪明票据采集时必须填写模型列表".into());
    }
    Ok(source)
}

fn source_config(provider: &StoredProviderCatalogProvider) -> Option<SourceConfig> {
    let config = parse_source_config(provider.config.as_ref()?.get(SOURCE_CONFIG)?).ok()?;
    (provider.is_active && config.enabled).then_some(config)
}

pub(crate) fn override_enabled(transport: &GatewayProviderTransportSnapshot) -> bool {
    transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
        && transport
            .provider
            .config
            .as_ref()
            .and_then(|config| config.get("pool_advanced"))
            .and_then(|pool| pool.get(OVERRIDE_CONFIG))
            .and_then(Value::as_bool)
            == Some(true)
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

pub(crate) async fn load_tickets(runtime: &RuntimeState, enabled: bool) -> Option<TicketCache> {
    if !enabled {
        return None;
    }
    let raw = runtime.kv_get(TICKET_KEY).await.ok()??;
    serde_json::from_str(&raw).ok()
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

fn is_congming_url(raw: &str) -> bool {
    url::Url::parse(raw).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("sub2.congmingai.com")
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
    if values.len() != 1 || !valid_ticket(values[0]) {
        return Err("采集响应缺少有效的 x-codex-turn-state（非空 ASCII，最多 4096 字节）".into());
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
        .map_err(|_| "聪明渠道认证或传输配置无效")?;
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
    .ok_or_else(|| "聪明渠道 Responses 端点无效".to_string())?;
    if !is_congming_url(&plan.url) {
        return Err("票据采集只允许 https://sub2.congmingai.com 的 Responses 端点".into());
    }
    plan.request_id = format!("congming-turn-state-{}", uuid::Uuid::new_v4());
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
        .map_err(|_| "读取聪明渠道端点失败")?;
    let endpoint = endpoints
        .iter()
        .find(|endpoint| {
            endpoint.is_active
                && crate::ai_serving::normalize_api_format_alias(&endpoint.api_format)
                    == "openai:responses"
                && is_congming_url(&endpoint.base_url)
        })
        .ok_or("聪明渠道没有启用的 Responses 端点")?;
    let keys = state
        .list_provider_catalog_keys_by_provider_ids(&ids)
        .await
        .map_err(|_| "读取聪明渠道密钥失败")?;
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
        .ok_or("聪明渠道没有可用于采集模型的启用密钥")?;
    let transport = state
        .read_provider_transport_snapshot(provider_id, &endpoint.id, &key.id)
        .await
        .map_err(|_| "读取聪明渠道传输配置失败")?
        .ok_or("聪明渠道传输配置不存在")?;
    let plan = build_fetch_plan(state, &transport, model).await?;
    let result = crate::execution_runtime::execute_execution_runtime_sync_plan(state, None, &plan)
        .await
        .map_err(|_| "聪明票据采集请求失败")?;
    ticket_from_response(&result)
}

async fn selected_source(state: &AppState) -> Result<Option<(String, SourceConfig)>, String> {
    let providers = state
        .list_provider_catalog_providers(true)
        .await
        .map_err(|_| "读取票据采集开关失败")?;
    let mut sources = providers
        .iter()
        .filter_map(|provider| source_config(provider).map(|config| (provider.id.clone(), config)));
    let source = sources.next();
    if sources.next().is_some() {
        return Err("只能启用一个聪明票据采集渠道".into());
    }
    Ok(source)
}

async fn scan(state: &AppState) -> Result<(), String> {
    scan_with_fetch(state, fetch_ticket).await
}

async fn scan_with_fetch(
    state: &AppState,
    fetch: impl AsyncFn(&AppState, &str, &str) -> Result<String, String>,
) -> Result<(), String> {
    let runtime = &state.runtime_state;
    let source = selected_source(state).await;
    let (provider_id, config) = match source {
        Ok(Some(source)) => source,
        other => {
            runtime
                .kv_delete(TICKET_KEY)
                .await
                .map_err(|_| "清除停用票据失败")?;
            return other.map(|_| ());
        }
    };
    let mut cache: TicketCache = runtime
        .kv_get(TICKET_KEY)
        .await
        .map_err(|_| "读取票据缓存失败")?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if cache.source_provider_id != provider_id {
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
    save_cache(runtime, &cache).await?;
    for model in &config.models {
        // A shared, expiring marker prevents duplicate paid requests on
        // replicas, errors and restarts. Only one model per scan keeps the
        // lock bounded; the others are picked up at the next 15-second tick.
        if !runtime
            .kv_set_if_absent(&attempt_key(&provider_id, model), "1", REFRESH_INTERVAL)
            .await
            .map_err(|_| "读取票据采集调度状态失败")?
        {
            continue;
        }
        let started_at = crate::codex_client_release::unix_now_secs();
        let value = fetch(state, &provider_id, model).await?;
        if !valid_ticket(&value) {
            return Err("采集响应票据无效".into());
        }
        // Do not republish after an in-flight source/model switch change.
        if selected_source(state).await? != Some((provider_id.clone(), config.clone())) {
            runtime
                .kv_delete(TICKET_KEY)
                .await
                .map_err(|_| "清除停用票据失败")?;
            return Ok(());
        }
        cache.tickets.insert(
            model.clone(),
            Ticket {
                value,
                fetched_at: started_at,
            },
        );
        save_cache(runtime, &cache).await?;
        tracing::info!(provider_id, model, "congming turn-state refreshed");
        break;
    }
    Ok(())
}

fn attempt_key(provider_id: &str, model: &str) -> String {
    let model_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, model.as_bytes());
    format!("aether:congming_turn_state:v1:attempt:{provider_id}:{model_id}")
}

async fn save_cache(runtime: &RuntimeState, cache: &TicketCache) -> Result<(), String> {
    runtime
        .kv_set(
            TICKET_KEY,
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
            // Fail closed for collection when shared coordination is unavailable.
            let Ok(Some(lease)) = state
                .runtime_state
                .lock_try_acquire(LOCK_KEY, "congming_turn_state", Duration::from_secs(75))
                .await
            else {
                continue;
            };
            let result = tokio::time::timeout(Duration::from_secs(60), scan(&state)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => tracing::warn!(%reason, "congming turn-state collection failed"),
                Err(_) => tracing::warn!("congming turn-state collection timed out"),
            }
            let _ = state.runtime_state.lock_release(&lease).await;
        }
    }))
}

#[cfg(test)]
pub(crate) mod tests;
