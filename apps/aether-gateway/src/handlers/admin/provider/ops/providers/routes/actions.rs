use super::super::actions::{
    admin_provider_ops_is_valid_action_type, admin_provider_ops_local_action_response,
};
use super::super::balance_cache::{
    admin_provider_ops_pending_balance_response, read_admin_provider_ops_balance_cache,
    spawn_admin_provider_ops_balance_refresh, store_admin_provider_ops_balance_cache,
    AdminProviderOpsBalanceCacheLookup,
};
use super::super::config::admin_provider_ops_config_object;
use super::super::support::AdminProviderOpsExecuteActionRequest;
use crate::handlers::admin::request::AdminAppState;
use crate::GatewayError;
use axum::{
    body::{Body, Bytes},
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub(super) async fn handle_admin_provider_ops_action(
    state: &AdminAppState<'_>,
    provider_id: &str,
    route_kind: &str,
    action_route: Option<&(String, String)>,
    query_string: Option<&str>,
    request_body: Option<&Bytes>,
) -> Result<Option<Response<Body>>, GatewayError> {
    let action_type = if route_kind == "provider_checkin" {
        "checkin".to_string()
    } else if matches!(
        route_kind,
        "get_provider_balance" | "refresh_provider_balance"
    ) {
        "query_balance".to_string()
    } else {
        let Some((_, action_type)) = action_route else {
            return Ok(None);
        };
        if !admin_provider_ops_is_valid_action_type(action_type) {
            return Ok(Some(
                (
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": format!("无效的操作类型: {action_type}") })),
                )
                    .into_response(),
            ));
        }
        action_type.clone()
    };

    let request_config = if route_kind == "execute_provider_action" {
        match request_body {
            Some(body) if !body.is_empty() => {
                let raw_value = match serde_json::from_slice::<serde_json::Value>(body) {
                    Ok(raw_value) => raw_value,
                    Err(_) => {
                        return Ok(Some(bad_request_detail_response(
                            "请求体必须是合法的 JSON 对象",
                        )));
                    }
                };
                let payload =
                    match serde_json::from_value::<AdminProviderOpsExecuteActionRequest>(raw_value)
                    {
                        Ok(payload) => payload,
                        Err(_) => {
                            return Ok(Some(bad_request_detail_response(
                                "请求体必须是合法的 JSON 对象",
                            )));
                        }
                    };
                payload.config
            }
            _ => None,
        }
    } else {
        None
    };

    let provider_ids = [provider_id.to_string()];
    let providers = state
        .read_provider_catalog_providers_by_ids(&provider_ids)
        .await?;
    let provider = providers.first();
    let endpoints = if provider.is_some() {
        state
            .list_provider_catalog_endpoints_by_provider_ids(&provider_ids)
            .await?
    } else {
        Vec::new()
    };
    let payload = if action_type == "query_balance"
        && route_kind == "get_provider_balance"
        && provider.is_some_and(|provider| admin_provider_ops_config_object(provider).is_some())
    {
        match read_admin_provider_ops_balance_cache(state, provider_id).await {
            AdminProviderOpsBalanceCacheLookup::Hit(cached) => {
                if query_param_bool(query_string, "refresh", true) {
                    spawn_admin_provider_ops_balance_refresh(state, provider_id).await;
                }
                cached
            }
            AdminProviderOpsBalanceCacheLookup::Miss => {
                if query_param_bool(query_string, "refresh", true)
                    && !state.runtime_state().is_memory()
                {
                    spawn_admin_provider_ops_balance_refresh(state, provider_id).await;
                    admin_provider_ops_pending_balance_response("余额数据加载中，请稍后刷新")
                } else {
                    let payload = admin_provider_ops_local_action_response(
                        state,
                        provider_id,
                        provider,
                        &endpoints,
                        &action_type,
                        request_config.as_ref(),
                    )
                    .await;
                    store_admin_provider_ops_balance_cache(state, provider_id, &payload).await;
                    payload
                }
            }
            AdminProviderOpsBalanceCacheLookup::Unavailable => {
                let payload = admin_provider_ops_local_action_response(
                    state,
                    provider_id,
                    provider,
                    &endpoints,
                    &action_type,
                    request_config.as_ref(),
                )
                .await;
                store_admin_provider_ops_balance_cache(state, provider_id, &payload).await;
                payload
            }
        }
    } else {
        let payload = admin_provider_ops_local_action_response(
            state,
            provider_id,
            provider,
            &endpoints,
            &action_type,
            request_config.as_ref(),
        )
        .await;
        if action_type == "query_balance" && route_kind == "refresh_provider_balance" {
            store_admin_provider_ops_balance_cache(state, provider_id, &payload).await;
        }
        payload
    };

    Ok(Some(Json(payload).into_response()))
}

fn query_param_bool(query: Option<&str>, key: &str, default: bool) -> bool {
    let Some(query) = query else {
        return default;
    };
    for (entry_key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if entry_key == key {
            let normalized = value.trim().to_ascii_lowercase();
            return matches!(normalized.as_str(), "1" | "true" | "yes" | "on");
        }
    }
    default
}

fn bad_request_detail_response(detail: &str) -> Response<Body> {
    (
        http::StatusCode::BAD_REQUEST,
        Json(json!({ "detail": detail })),
    )
        .into_response()
}
