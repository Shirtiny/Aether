use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::GatewayError;
use axum::{
    body::{Body, Bytes},
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub(super) async fn maybe_handle(
    _state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
    _request_body: Option<&Bytes>,
) -> Result<Option<Response<Body>>, GatewayError> {
    let Some(decision) = request_context.decision() else {
        return Ok(None);
    };
    if decision.route_family.as_deref() != Some("endpoints_manage")
        || decision.route_kind.as_deref() != Some("update_key_codex_ws")
        || request_context.method() != http::Method::PUT
    {
        return Ok(None);
    }

    // Old clients must not report a successful toggle that no longer has effect.
    Ok(Some(
        (
            http::StatusCode::GONE,
            Json(json!({"detail": "Codex OAuth 账号默认支持 WebSocket，账号级 WS 开关已移除"})),
        )
            .into_response(),
    ))
}
