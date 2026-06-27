use super::super::errors::build_internal_control_error_response;
use super::super::runtime::resolve_provider_oauth_runtime_endpoints;
use super::super::state::is_fixed_provider_type_for_provider_oauth;
use crate::handlers::admin::provider::shared::paths::admin_provider_oauth_codex_reset_credit_key_id;
use crate::handlers::admin::request::{
    AdminAppState, AdminGatewayProviderTransportSnapshot, AdminRequestContext,
};
use crate::provider_key_auth::provider_key_is_oauth_managed;
use crate::GatewayError;
use aether_contracts::{ExecutionPlan, ExecutionTimeouts, RequestBody};
use aether_provider_pool::{build_codex_pool_reset_credit_request, ProviderPoolQuotaRequestSpec};
use axum::{
    body::Body,
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

const CODEX_RESET_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const CODEX_RESET_PROXY_TIMEOUT_MS: u64 = 60_000;

pub(super) async fn handle_admin_provider_oauth_codex_reset_credit(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
) -> Result<Response<Body>, GatewayError> {
    let Some(key_id) = admin_provider_oauth_codex_reset_credit_key_id(request_context.path())
    else {
        return Ok(control_error_response(
            http::StatusCode::NOT_FOUND,
            "Key 不存在",
        ));
    };
    let Some(key) = state
        .read_provider_catalog_keys_by_ids(std::slice::from_ref(&key_id))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(control_error_response(
            http::StatusCode::NOT_FOUND,
            "Key 不存在",
        ));
    };

    let provider_id = key.provider_id.clone();
    let Some(provider) = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(control_error_response(
            http::StatusCode::NOT_FOUND,
            "Provider 不存在",
        ));
    };
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    if provider_type != "codex" {
        return Ok(control_error_response(
            http::StatusCode::BAD_REQUEST,
            "仅 Codex OAuth 账号支持主动重置额度",
        ));
    }
    if !provider_key_is_oauth_managed(&key, provider_type.as_str()) {
        return Ok(control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Key 不是 Codex OAuth 管理账号",
        ));
    }
    if !is_fixed_provider_type_for_provider_oauth(&provider_type) {
        return Ok(control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Provider 不是固定类型，无法使用 provider-oauth",
        ));
    }

    let endpoint_resolution =
        resolve_provider_oauth_runtime_endpoints(state, &provider, &provider_type).await?;
    let Some(endpoint) = endpoint_resolution.runtime_endpoint else {
        return Ok(control_error_response(
            http::StatusCode::BAD_REQUEST,
            "找不到有效端点，无法主动重置额度",
        ));
    };
    let Some(transport) = state
        .read_provider_transport_snapshot_uncached(&provider_id, &endpoint.id, &key_id)
        .await?
    else {
        return Ok(control_error_response(
            http::StatusCode::BAD_REQUEST,
            "Provider transport snapshot unavailable",
        ));
    };
    let Some(resolved_oauth_auth) = state.resolve_local_oauth_header_auth(&transport).await? else {
        return Ok(control_error_response(
            http::StatusCode::BAD_REQUEST,
            "缺少 Codex OAuth 认证信息，请先重新授权/刷新 Token",
        ));
    };

    let auth_config = transport
        .key
        .decrypted_auth_config
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let spec = build_codex_pool_reset_credit_request(
        &transport.key.id,
        Uuid::new_v4().to_string(),
        resolved_oauth_auth,
        auth_config.as_ref(),
    );
    let plan = build_codex_reset_credit_execution_plan(state, &transport, spec).await;
    let result = match state.execute_execution_runtime_sync_plan(None, &plan).await {
        Ok(result) => result,
        Err(err) => {
            let detail = err.into_message();
            warn!(
                key_id = %transport.key.id,
                endpoint_id = %transport.endpoint.id,
                url = %plan.url,
                error = %detail,
                "codex reset credit execution runtime request failed"
            );
            return Ok(control_error_response(
                http::StatusCode::BAD_GATEWAY,
                format!("Codex 主动重置请求执行失败：{detail}"),
            ));
        }
    };

    if (200..300).contains(&result.status_code) {
        return Ok(Json(json!({
            "provider_type": provider_type,
            "status": "success",
            "message": "Codex 主动重置额度已提交",
            "status_code": result.status_code,
        }))
        .into_response());
    }

    Ok(control_error_response(
        http::StatusCode::BAD_GATEWAY,
        format!(
            "Codex 主动重置失败：HTTP {}{}",
            result.status_code,
            execution_result_body_excerpt(result.body.as_ref())
                .map(|excerpt| format!(" - {excerpt}"))
                .unwrap_or_default()
        ),
    ))
}

async fn build_codex_reset_credit_execution_plan(
    state: &AdminAppState<'_>,
    transport: &AdminGatewayProviderTransportSnapshot,
    spec: ProviderPoolQuotaRequestSpec,
) -> ExecutionPlan {
    let proxy = state
        .resolve_transport_proxy_snapshot_with_tunnel_affinity(transport)
        .await;
    let timeout_ms = if proxy.is_some() {
        CODEX_RESET_PROXY_TIMEOUT_MS
    } else {
        CODEX_RESET_DEFAULT_TIMEOUT_MS
    };
    let mut timeouts = state
        .resolve_transport_execution_timeouts(transport)
        .unwrap_or_default();
    timeouts.connect_ms = timeouts.connect_ms.or(Some(timeout_ms));
    timeouts.read_ms = timeouts.read_ms.or(Some(timeout_ms));
    timeouts.write_ms = timeouts.write_ms.or(Some(timeout_ms));
    timeouts.pool_ms = timeouts.pool_ms.or(Some(timeout_ms));
    timeouts.total_ms = timeouts.total_ms.or(Some(timeout_ms));

    let ProviderPoolQuotaRequestSpec {
        request_id,
        provider_name,
        quota_kind: _,
        method,
        url,
        mut headers,
        content_type,
        json_body,
        client_api_format,
        provider_api_format,
        model_name,
        accept_invalid_certs: _,
    } = spec;
    crate::ai_serving::apply_codex_pool_stable_client_headers(&mut headers, transport);

    ExecutionPlan {
        request_id,
        candidate_id: None,
        provider_name: Some(provider_name),
        provider_id: transport.provider.id.clone(),
        endpoint_id: transport.endpoint.id.clone(),
        key_id: transport.key.id.clone(),
        method,
        url,
        headers,
        content_type,
        content_encoding: None,
        body: json_body
            .map(RequestBody::from_json)
            .unwrap_or(RequestBody {
                json_body: None,
                body_bytes_b64: None,
                body_ref: None,
            }),
        stream: false,
        client_api_format,
        provider_api_format,
        model_name,
        proxy,
        transport_profile: state.resolve_transport_profile(transport),
        timeouts: Some(timeouts),
    }
}

fn control_error_response(status: http::StatusCode, message: impl Into<String>) -> Response<Body> {
    build_internal_control_error_response(status, message)
}

fn execution_result_body_excerpt(body: Option<&aether_contracts::ResponseBody>) -> Option<String> {
    let raw = body.and_then(|body| {
        body.json_body
            .as_ref()
            .map(Value::to_string)
            .or_else(|| body.body_bytes_b64.clone())
    })?;
    let excerpt: String = raw.chars().take(240).collect();
    if excerpt.trim().is_empty() {
        None
    } else {
        Some(excerpt)
    }
}
