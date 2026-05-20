use super::shared::{
    build_quota_snapshot_payload, default_provider_quota_execution_timeouts,
    execute_provider_quota_plan, extract_execution_error_message,
    oauth_refresh_auto_removed_result, persist_provider_quota_refresh_state,
    quota_key_auto_removed, quota_refresh_success_invalid_state, ProviderQuotaExecutionOutcome,
};
use crate::handlers::admin::provider::shared::payloads::{
    OAUTH_ACCOUNT_BLOCK_PREFIX, OAUTH_EXPIRED_PREFIX,
};
use crate::handlers::admin::request::{AdminAppState, AdminGatewayProviderTransportSnapshot};
use crate::GatewayError;
use aether_admin::provider::quota::parse_chatgpt_web_conversation_init_response;
use aether_contracts::ProxySnapshot;
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use aether_provider_pool::{
    build_chatgpt_web_pool_quota_request, enrich_chatgpt_web_quota_metadata,
    normalize_chatgpt_web_image_quota_limit,
};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

const PLACEHOLDER_API_KEY: &str = "__placeholder__";

fn chatgpt_web_auth_config(
    transport: &AdminGatewayProviderTransportSnapshot,
) -> Option<serde_json::Value> {
    transport
        .key
        .decrypted_auth_config
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
}

async fn resolve_chatgpt_web_quota_auth(
    state: &AdminAppState<'_>,
    transport: &AdminGatewayProviderTransportSnapshot,
) -> Result<Option<(String, String)>, GatewayError> {
    if let Some(auth) = state.resolve_local_oauth_header_auth(transport).await? {
        return Ok(Some(auth));
    }
    let decrypted_key = transport.key.decrypted_api_key.trim();
    if decrypted_key.is_empty() || decrypted_key == PLACEHOLDER_API_KEY {
        return Ok(None);
    }
    Ok(Some((
        "authorization".to_string(),
        format!("Bearer {decrypted_key}"),
    )))
}

async fn execute_chatgpt_web_quota_plan(
    state: &AdminAppState<'_>,
    transport: &AdminGatewayProviderTransportSnapshot,
    endpoint: &StoredProviderCatalogEndpoint,
    authorization: (String, String),
    proxy_override: Option<&ProxySnapshot>,
) -> Result<ProviderQuotaExecutionOutcome, GatewayError> {
    let proxy = match proxy_override {
        Some(proxy) => Some(proxy.clone()),
        None => {
            state
                .resolve_transport_proxy_snapshot_with_tunnel_affinity(transport)
                .await
        }
    };
    let timeouts = state
        .resolve_transport_execution_timeouts(transport)
        .or(Some(default_provider_quota_execution_timeouts(
            proxy.as_ref(),
        )));
    let spec =
        build_chatgpt_web_pool_quota_request(&transport.key.id, &endpoint.base_url, authorization);
    let plan = super::shared::build_provider_quota_execution_plan(
        transport,
        spec,
        proxy,
        state.resolve_transport_profile(transport),
        timeouts,
    );

    execute_provider_quota_plan(state, transport, plan, "chatgpt_web").await
}

fn chatgpt_web_quota_invalid_reason(status_code: u16, upstream_message: Option<&str>) -> String {
    let message = upstream_message.unwrap_or_default().trim();
    let detail = if message.is_empty() {
        match status_code {
            401 => "ChatGPT Web Token 无效或已过期",
            403 => "ChatGPT Web 账户访问受限",
            _ => "ChatGPT Web 请求失败",
        }
    } else {
        message
    };
    match status_code {
        401 => format!("{OAUTH_EXPIRED_PREFIX}{detail}"),
        403 => format!("{OAUTH_ACCOUNT_BLOCK_PREFIX}{detail}"),
        _ => detail.to_string(),
    }
}

pub(crate) async fn refresh_chatgpt_web_provider_quota_locally(
    state: &AdminAppState<'_>,
    provider: &StoredProviderCatalogProvider,
    endpoint: &StoredProviderCatalogEndpoint,
    keys: Vec<StoredProviderCatalogKey>,
    proxy_override: Option<ProxySnapshot>,
) -> Result<Option<serde_json::Value>, GatewayError> {
    let mut results = Vec::new();
    let mut success_count = 0usize;
    let mut failed_count = 0usize;
    let mut auto_removed_count = 0usize;

    for key in keys {
        let transport = match state
            .read_provider_transport_snapshot(&provider.id, &endpoint.id, &key.id)
            .await?
        {
            Some(transport) => transport,
            None => {
                failed_count += 1;
                results.push(json!({
                    "key_id": key.id,
                    "key_name": key.name,
                    "status": "error",
                    "message": "Provider transport snapshot unavailable",
                }));
                continue;
            }
        };

        let authorization = match resolve_chatgpt_web_quota_auth(state, &transport).await? {
            Some(auth) => auth,
            None => {
                if quota_key_auto_removed(state, &key.id).await? {
                    auto_removed_count += 1;
                    results.push(oauth_refresh_auto_removed_result(&key));
                    continue;
                }
                failed_count += 1;
                results.push(json!({
                    "key_id": key.id,
                    "key_name": key.name,
                    "status": "error",
                    "message": "缺少 ChatGPT Web OAuth 认证信息，请先导入/刷新 Token",
                }));
                continue;
            }
        };

        let result = match execute_chatgpt_web_quota_plan(
            state,
            &transport,
            endpoint,
            authorization,
            proxy_override.as_ref(),
        )
        .await?
        {
            ProviderQuotaExecutionOutcome::Response(result) => result,
            ProviderQuotaExecutionOutcome::Failure(detail) => {
                failed_count += 1;
                results.push(json!({
                    "key_id": key.id,
                    "key_name": key.name,
                    "status": "error",
                    "message": format!("conversation/init 请求执行失败: {detail}"),
                    "status_code": 502,
                }));
                continue;
            }
        };

        let now_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let mut metadata_update = None::<serde_json::Value>;
        let (mut oauth_invalid_at_unix_secs, mut oauth_invalid_reason) = (
            key.oauth_invalid_at_unix_secs,
            key.oauth_invalid_reason.clone(),
        );
        let mut status = "error".to_string();
        let mut message = None::<String>;

        if result.status_code == 200 {
            if let Some(body_json) = result
                .body
                .as_ref()
                .and_then(|body| body.json_body.as_ref())
            {
                if let Some(mut metadata) =
                    parse_chatgpt_web_conversation_init_response(body_json, now_unix_secs)
                {
                    let auth_config = chatgpt_web_auth_config(&transport);
                    enrich_chatgpt_web_quota_metadata(&mut metadata, auth_config.as_ref());
                    normalize_chatgpt_web_image_quota_limit(
                        &mut metadata,
                        key.upstream_metadata.as_ref(),
                    );
                    metadata_update = Some(json!({ "chatgpt_web": metadata }));
                    (oauth_invalid_at_unix_secs, oauth_invalid_reason) =
                        quota_refresh_success_invalid_state(&key);
                    status = "success".to_string();
                } else {
                    status = "no_metadata".to_string();
                    message = Some("响应中未包含 ChatGPT Web 生图限额信息".to_string());
                }
            } else {
                status = "no_metadata".to_string();
                message = Some("响应中未包含 ChatGPT Web 生图限额信息".to_string());
            }
        } else {
            let err_msg = extract_execution_error_message(&result);
            message = Some(match err_msg.as_deref() {
                Some(detail) if !detail.is_empty() => {
                    format!(
                        "conversation/init 返回状态码 {}: {}",
                        result.status_code, detail
                    )
                }
                _ => format!("conversation/init 返回状态码 {}", result.status_code),
            });

            if matches!(result.status_code, 401 | 403) {
                oauth_invalid_at_unix_secs = Some(now_unix_secs);
                oauth_invalid_reason = Some(chatgpt_web_quota_invalid_reason(
                    result.status_code,
                    err_msg.as_deref(),
                ));
                status = if result.status_code == 401 {
                    "auth_invalid".to_string()
                } else {
                    "forbidden".to_string()
                };
            }
        }

        if !persist_provider_quota_refresh_state(
            state,
            &key.id,
            metadata_update.as_ref(),
            oauth_invalid_at_unix_secs,
            oauth_invalid_reason,
            None,
        )
        .await?
        {
            failed_count += 1;
            results.push(json!({
                "key_id": key.id,
                "key_name": key.name,
                "status": "error",
                "message": "Key 状态写入失败",
            }));
            continue;
        }

        if status == "success" {
            success_count += 1;
        } else {
            failed_count += 1;
        }

        let mut payload = serde_json::Map::new();
        payload.insert("key_id".to_string(), json!(key.id));
        payload.insert("key_name".to_string(), json!(key.name));
        payload.insert("status".to_string(), json!(status));
        if let Some(message) = message {
            payload.insert("message".to_string(), json!(message));
        }
        if result.status_code != 200 {
            payload.insert("status_code".to_string(), json!(result.status_code));
        }
        if let Some(metadata) = metadata_update
            .as_ref()
            .and_then(|value| value.get("chatgpt_web"))
            .cloned()
        {
            payload.insert("metadata".to_string(), metadata);
        }
        if let Some(quota_snapshot) = build_quota_snapshot_payload(
            "chatgpt_web",
            key.status_snapshot.as_ref(),
            metadata_update.as_ref(),
        ) {
            payload.insert("quota_snapshot".to_string(), quota_snapshot);
        }
        results.push(serde_json::Value::Object(payload));
    }

    Ok(Some(json!({
        "success": success_count,
        "failed": failed_count,
        "total": results.len(),
        "results": results,
        "message": format!("已处理 {} 个 Key", results.len()),
        "auto_removed": auto_removed_count,
    })))
}
