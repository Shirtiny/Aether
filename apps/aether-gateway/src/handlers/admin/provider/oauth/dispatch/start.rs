use super::super::errors::build_internal_control_error_response;
use super::super::state::{
    admin_provider_oauth_template, build_provider_oauth_start_response,
    generate_codex_oauth_pkce_verifier, generate_codex_oauth_state, generate_provider_oauth_nonce,
    generate_provider_oauth_pkce_verifier, is_fixed_provider_type_for_provider_oauth,
    provider_oauth_pkce_s256, AdminProviderOAuthClientIdentity,
};
use crate::handlers::admin::provider::shared::paths::{
    admin_provider_oauth_start_key_id, admin_provider_oauth_start_provider_id,
};
use crate::handlers::admin::request::{
    AdminAppState, AdminProviderOAuthTemplate, AdminRequestContext,
};
use crate::provider_key_auth::provider_key_is_oauth_managed;
use crate::GatewayError;
use axum::{
    body::Body,
    http,
    response::{IntoResponse, Response},
    Json,
};

/// Generates the `state` nonce and PKCE verifier for an OAuth start. Codex
/// mirrors codex-rs (32 random bytes base64url for `state`, 64 random bytes
/// base64url for the PKCE verifier); every other provider keeps its existing
/// hex-nonce / hex-verifier shape.
fn generate_provider_oauth_start_secrets(
    template: &AdminProviderOAuthTemplate,
) -> (String, Option<String>) {
    if template.provider_type == "codex" {
        let nonce = generate_codex_oauth_state();
        let pkce_verifier = template.use_pkce.then(generate_codex_oauth_pkce_verifier);
        return (nonce, pkce_verifier);
    }
    let nonce = generate_provider_oauth_nonce();
    let pkce_verifier = template
        .use_pkce
        .then(generate_provider_oauth_pkce_verifier);
    (nonce, pkce_verifier)
}

pub(super) async fn handle_admin_provider_oauth_start_key(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
) -> Result<Response<Body>, GatewayError> {
    let Some(key_id) = admin_provider_oauth_start_key_id(request_context.path()) else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Key 不存在",
        ));
    };
    let key = state
        .read_provider_catalog_keys_by_ids(std::slice::from_ref(&key_id))
        .await?
        .into_iter()
        .next();
    let Some(key) = key else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Key 不存在",
        ));
    };
    let provider_id = key.provider_id.clone();
    let provider = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id))
        .await?
        .into_iter()
        .next();
    let Some(provider) = provider else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Provider 不存在",
        ));
    };
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    if !provider_key_is_oauth_managed(&key, provider_type.as_str()) {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Key 不是 OAuth 管理账号",
        ));
    }
    if !is_fixed_provider_type_for_provider_oauth(&provider_type) {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Provider 不是固定类型，无法使用 provider-oauth",
        ));
    }
    if provider_type == "windsurf" {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "Windsurf 请使用浏览器登录或导入凭据。",
        ));
    }
    let Some(template) = admin_provider_oauth_template(&provider_type) else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Provider 不支持 OAuth 授权",
        ));
    };

    let (nonce, pkce_verifier) = generate_provider_oauth_start_secrets(&template);
    let code_challenge = pkce_verifier.as_deref().map(provider_oauth_pkce_s256);
    // Re-authorizing an existing key keeps the client identity its pool
    // traffic already presents, so the login and the later API calls agree.
    let client_identity = AdminProviderOAuthClientIdentity::for_existing_codex_key(
        &provider_type,
        provider.config.as_ref(),
        key.fingerprint.as_ref(),
        &key.id,
    );
    if state
        .save_provider_oauth_state(
            &nonce,
            &key_id,
            &provider_id,
            &provider_type,
            pkce_verifier.as_deref(),
            client_identity.as_ref(),
        )
        .await
        .is_err()
    {
        return Ok(build_internal_control_error_response(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "provider oauth redis unavailable",
        ));
    }

    Ok(Json(build_provider_oauth_start_response(
        template,
        &nonce,
        code_challenge.as_deref(),
        client_identity.as_ref(),
    ))
    .into_response())
}

pub(super) async fn handle_admin_provider_oauth_start_provider(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
) -> Result<Response<Body>, GatewayError> {
    let Some(provider_id) = admin_provider_oauth_start_provider_id(request_context.path()) else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Provider 不存在",
        ));
    };
    let provider = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id))
        .await?
        .into_iter()
        .next();
    let Some(provider) = provider else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::NOT_FOUND,
            "Provider 不存在",
        ));
    };
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    if !is_fixed_provider_type_for_provider_oauth(&provider_type) {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Provider 不是固定类型，无法使用 provider-oauth",
        ));
    }
    if provider_type == "kiro" {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "Kiro 不支持 OAuth 授权，请使用导入授权。",
        ));
    }
    if provider_type == "windsurf" {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "Windsurf 请使用浏览器登录或导入凭据。",
        ));
    }
    let Some(template) = admin_provider_oauth_template(&provider_type) else {
        return Ok(build_internal_control_error_response(
            http::StatusCode::BAD_REQUEST,
            "该 Provider 不支持 OAuth 授权",
        ));
    };

    let (nonce, pkce_verifier) = generate_provider_oauth_start_secrets(&template);
    let code_challenge = pkce_verifier.as_deref().map(provider_oauth_pkce_s256);
    // The account is unknown until the callback, so pick the client identity
    // now (seeded by the state nonce); the callback freezes it into the key.
    let client_identity = AdminProviderOAuthClientIdentity::for_new_codex_login(
        &provider_type,
        provider.config.as_ref(),
        &nonce,
    );
    if state
        .save_provider_oauth_state(
            &nonce,
            "",
            &provider_id,
            &provider_type,
            pkce_verifier.as_deref(),
            client_identity.as_ref(),
        )
        .await
        .is_err()
    {
        return Ok(build_internal_control_error_response(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "provider oauth redis unavailable",
        ));
    }

    Ok(Json(build_provider_oauth_start_response(
        template,
        &nonce,
        code_challenge.as_deref(),
        client_identity.as_ref(),
    ))
    .into_response())
}
