use super::{
    build_auth_error_response, resolve_authenticated_local_user, AppState,
    GatewayPublicRequestContext,
};
use axum::{
    body::Body,
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub(super) async fn handle_users_me_referral_get(
    state: &AppState,
    request_context: &GatewayPublicRequestContext,
    headers: &http::HeaderMap,
) -> Response<Body> {
    let auth = match resolve_authenticated_local_user(state, request_context, headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !state.has_referral_data_backend() {
        return build_auth_error_response(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "邀请返利数据暂不可用",
            false,
        );
    }
    let dashboard = match state.referral_dashboard(&auth.user.id).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            return build_auth_error_response(
                http::StatusCode::SERVICE_UNAVAILABLE,
                "邀请返利数据暂不可用",
                false,
            );
        }
        Err(err) => {
            return build_auth_error_response(
                http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("referral dashboard failed: {err:?}"),
                false,
            );
        }
    };
    let base = headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let invitation_link = if base.is_empty() {
        format!("/register?invite={}", dashboard.invite_code)
    } else {
        format!("{base}/register?invite={}", dashboard.invite_code)
    };
    Json(json!({
        "invite_code": dashboard.invite_code,
        "invitation_link": invitation_link,
        "summary": {
            "total_invites": dashboard.total_invites,
            "effective_invites": dashboard.effective_invites,
            "paid_reward_usd": dashboard.paid_reward_usd,
            "pending_reward_usd": dashboard.pending_reward_usd,
            "reversed_reward_usd": dashboard.reversed_reward_usd,
        }
    }))
    .into_response()
}
