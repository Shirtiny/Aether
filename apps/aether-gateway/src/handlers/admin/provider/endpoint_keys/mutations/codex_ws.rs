use crate::codex_ws_config::read_codex_ws_feature_flags;
use crate::handlers::admin::provider::shared::paths::admin_codex_ws_key_id;
use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::provider_transport::{
    resolve_codex_official_ws, GatewayProviderTransportSnapshot, CODEX_OFFICIAL_WS_CODEX_COMMIT,
    CODEX_OFFICIAL_WS_CRYPTO_PROVIDER, CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
    CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES, CODEX_OFFICIAL_WS_PROFILE_ID,
    CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION, CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
    CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID, CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
    CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
};
use crate::GatewayError;
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use axum::{
    body::{Body, Bytes},
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexWsUpdatePayload {
    enabled: bool,
    #[serde(default)]
    profile_id: Option<String>,
}

pub(super) async fn maybe_handle(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
    request_body: Option<&Bytes>,
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

    let Some(key_id) = admin_codex_ws_key_id(request_context.path()) else {
        return Ok(Some(not_found_response("Key 不存在")));
    };
    let Some(request_body) = request_body else {
        return Ok(Some(bad_request_response("请求体不能为空")));
    };
    let payload = match serde_json::from_slice::<CodexWsUpdatePayload>(request_body) {
        Ok(payload) => payload,
        Err(_) => {
            return Ok(Some(bad_request_response(
                "请求体必须只包含 enabled 布尔值和可选 profile_id",
            )))
        }
    };
    if payload
        .profile_id
        .as_deref()
        .is_some_and(|profile_id| profile_id != CODEX_OFFICIAL_WS_PROFILE_ID)
    {
        return Ok(Some(bad_request_response(
            "不支持的 Codex WebSocket profile_id",
        )));
    }
    if !state.as_ref().has_provider_catalog_data_reader() {
        return Ok(None);
    }
    if !state.as_ref().has_provider_catalog_data_writer() {
        return Ok(Some(service_unavailable_response(
            "Provider catalog writer 不可用",
        )));
    }

    let Some(existing_key) = state
        .read_provider_catalog_keys_by_ids(std::slice::from_ref(&key_id))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(Some(not_found_response(format!("Key {key_id} 不存在"))));
    };
    let Some(provider) = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(&existing_key.provider_id))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(Some(not_found_response(format!(
            "Provider {} 不存在",
            existing_key.provider_id
        ))));
    };
    if !provider.provider_type.trim().eq_ignore_ascii_case("codex") {
        return Ok(Some(unprocessable_response(
            "仅 Codex provider 的账号可以配置官方 WebSocket",
        )));
    }
    if !existing_key.auth_type.trim().eq_ignore_ascii_case("oauth") {
        return Ok(Some(unprocessable_response(
            "仅 OAuth Codex 账号可以配置官方 WebSocket",
        )));
    }

    let profile = payload.enabled.then(build_pinned_profile_manifest);
    let updated_at_unix_secs = current_unix_secs();
    let updated = state
        .update_provider_catalog_key_codex_ws_metadata(
            &key_id,
            payload.enabled,
            profile.as_ref(),
            updated_at_unix_secs,
        )
        .await?;
    if !updated {
        return Ok(Some(not_found_response(format!("Key {key_id} 不存在"))));
    }

    let key = state
        .read_provider_catalog_keys_by_ids(std::slice::from_ref(&key_id))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| GatewayError::Internal(format!("更新后的 Key {key_id} 无法读取")))?;
    let endpoints = state
        .list_provider_catalog_endpoints_by_provider_ids(std::slice::from_ref(&provider.id))
        .await?;
    let flags = read_codex_ws_feature_flags(state.app()).await;

    Ok(Some(
        Json(build_status_response(
            &provider,
            &key,
            &endpoints,
            flags.native_account_flags(),
        ))
        .into_response(),
    ))
}

fn build_pinned_profile_manifest() -> Value {
    json!({
        "schema_version": CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION,
        "profile_id": CODEX_OFFICIAL_WS_PROFILE_ID,
        "codex_commit": CODEX_OFFICIAL_WS_CODEX_COMMIT,
        "tokio_tungstenite_rev": CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
        "tungstenite_rev": CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
        "tungstenite_patch_id": CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID,
        "write_buffer_size_bytes": CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
        "max_write_buffer_size_bytes": CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES,
        "max_retained_write_buffer_capacity_bytes": CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
        "crypto_provider": CODEX_OFFICIAL_WS_CRYPTO_PROVIDER,
    })
}

fn build_status_response(
    provider: &StoredProviderCatalogProvider,
    key: &StoredProviderCatalogKey,
    endpoints: &[StoredProviderCatalogEndpoint],
    flags: crate::provider_transport::CodexOfficialWsGlobalFlags,
) -> Value {
    let mut resolutions = endpoints
        .iter()
        .map(|endpoint| {
            resolve_codex_official_ws(&transport_snapshot(provider, endpoint, key), flags)
        })
        .collect::<Vec<_>>();
    resolutions.sort_by_key(|resolution| (!resolution.profile_effective, resolution.reasons.len()));
    let best = resolutions.first();
    let capability_enabled = key
        .capabilities
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|capabilities| capabilities.get("codex_official_ws"))
        .and_then(Value::as_bool)
        == Some(true);
    let profile_effective = resolutions
        .iter()
        .any(|resolution| resolution.profile_effective);
    let profile_reasons = best
        .map(|resolution| json!(resolution.reasons))
        .unwrap_or_else(|| json!(["official_endpoint_missing"]));
    let runtime_eligibility = best
        .filter(|resolution| resolution.profile_effective == profile_effective)
        .map(|resolution| resolution.runtime_eligibility_without_request_context());
    let (runtime_eligible, runtime_reasons) =
        if let Some(reason) = crate::codex_ws::hot_state::known_key_runtime_blocker(key) {
            (Some(false), json!([reason]))
        } else {
            runtime_eligibility
                .map(|resolution| (resolution.runtime_eligible, json!(resolution.reasons)))
                .unwrap_or_else(|| (Some(false), json!(["profile_not_effective"])))
        };
    let profile_id = key
        .fingerprint
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|fingerprint| fingerprint.get("websocket_transport_profile"))
        .and_then(Value::as_object)
        .and_then(|profile| profile.get("profile_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let runtime_state = crate::provider_transport::resolve_codex_official_ws_admin_runtime_state(
        key.is_active,
        capability_enabled,
        profile_effective,
    );

    json!({
        "key_id": key.id,
        "configured": capability_enabled,
        "profile_effective": profile_effective,
        "runtime_eligible": runtime_eligible,
        "profile_id": profile_id,
        "runtime_state": runtime_state,
        "profile_reasons": profile_reasons,
        "runtime_reasons": runtime_reasons,
    })
}

fn transport_snapshot(
    provider: &StoredProviderCatalogProvider,
    endpoint: &StoredProviderCatalogEndpoint,
    key: &StoredProviderCatalogKey,
) -> GatewayProviderTransportSnapshot {
    use crate::provider_transport::snapshot::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider,
    };

    GatewayProviderTransportSnapshot {
        provider: GatewayProviderTransportProvider {
            id: provider.id.clone(),
            name: provider.name.clone(),
            provider_type: provider.provider_type.clone(),
            website: provider.website.clone(),
            is_active: provider.is_active,
            keep_priority_on_conversion: provider.keep_priority_on_conversion,
            enable_format_conversion: provider.enable_format_conversion,
            concurrent_limit: provider.concurrent_limit,
            max_retries: provider.max_retries,
            proxy: provider.proxy.clone(),
            request_timeout_secs: provider.request_timeout_secs,
            stream_first_byte_timeout_secs: provider.stream_first_byte_timeout_secs,
            config: provider.config.clone(),
        },
        endpoint: GatewayProviderTransportEndpoint {
            id: endpoint.id.clone(),
            provider_id: endpoint.provider_id.clone(),
            api_format: endpoint.api_format.clone(),
            api_family: endpoint.api_family.clone(),
            endpoint_kind: endpoint.endpoint_kind.clone(),
            is_active: endpoint.is_active,
            base_url: endpoint.base_url.clone(),
            header_rules: endpoint.header_rules.clone(),
            body_rules: endpoint.body_rules.clone(),
            max_retries: endpoint.max_retries,
            custom_path: endpoint.custom_path.clone(),
            config: endpoint.config.clone(),
            format_acceptance_config: endpoint.format_acceptance_config.clone(),
            proxy: endpoint.proxy.clone(),
        },
        key: GatewayProviderTransportKey {
            id: key.id.clone(),
            provider_id: key.provider_id.clone(),
            name: key.name.clone(),
            auth_type: key.auth_type.clone(),
            is_active: key.is_active,
            api_formats: string_array(key.api_formats.as_ref()),
            auth_type_by_format: key.auth_type_by_format.clone(),
            allow_auth_channel_mismatch_formats: key.allow_auth_channel_mismatch_formats.clone(),
            allowed_models: string_array(key.allowed_models.as_ref()),
            capabilities: key.capabilities.clone(),
            rate_multipliers: key.rate_multipliers.clone(),
            global_priority_by_format: key.global_priority_by_format.clone(),
            expires_at_unix_secs: key.expires_at_unix_secs,
            proxy: key.proxy.clone(),
            fingerprint: key.fingerprint.clone(),
            upstream_metadata: key.upstream_metadata.clone(),
            decrypted_api_key: String::new(),
            decrypted_auth_config: None,
        },
    }
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value.and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect()
    })
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn bad_request_response(detail: impl Into<String>) -> Response<Body> {
    error_response(http::StatusCode::BAD_REQUEST, detail)
}

fn unprocessable_response(detail: impl Into<String>) -> Response<Body> {
    error_response(http::StatusCode::UNPROCESSABLE_ENTITY, detail)
}

fn not_found_response(detail: impl Into<String>) -> Response<Body> {
    error_response(http::StatusCode::NOT_FOUND, detail)
}

fn service_unavailable_response(detail: impl Into<String>) -> Response<Body> {
    error_response(http::StatusCode::SERVICE_UNAVAILABLE, detail)
}

fn error_response(status: http::StatusCode, detail: impl Into<String>) -> Response<Body> {
    (status, Json(json!({ "detail": detail.into() }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::{build_pinned_profile_manifest, build_status_response};
    use crate::provider_transport::{
        CodexOfficialWsGlobalFlags, CODEX_OFFICIAL_WS_CODEX_COMMIT,
        CODEX_OFFICIAL_WS_CRYPTO_PROVIDER,
        CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
        CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES, CODEX_OFFICIAL_WS_PROFILE_ID,
        CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION, CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
        CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID, CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
        CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
    };
    use aether_data_contracts::repository::provider_catalog::{
        StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
    };
    use serde_json::{json, Value};

    fn enabled_flags() -> CodexOfficialWsGlobalFlags {
        CodexOfficialWsGlobalFlags {
            enabled: true,
            native_codex_ws_enabled: true,
        }
    }

    fn provider() -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            "provider-1".to_string(),
            "Codex".to_string(),
            None,
            "codex".to_string(),
        )
        .expect("provider should build")
    }

    fn endpoint() -> StoredProviderCatalogEndpoint {
        StoredProviderCatalogEndpoint::new(
            "endpoint-1".to_string(),
            "provider-1".to_string(),
            "openai:responses".to_string(),
            Some("openai".to_string()),
            Some("responses".to_string()),
            true,
        )
        .expect("endpoint should build")
        .with_transport_fields(
            "https://chatgpt.com/backend-api/codex".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("endpoint transport should build")
    }

    fn key() -> StoredProviderCatalogKey {
        StoredProviderCatalogKey::new(
            "key-1".to_string(),
            "provider-1".to_string(),
            "Codex OAuth".to_string(),
            "oauth".to_string(),
            Some(json!({"codex_official_ws": true})),
            true,
        )
        .expect("key should build")
        .with_transport_fields(
            Some(json!(["openai:responses"])),
            Some("encrypted-access-token".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(json!({
                "websocket_transport_profile": build_pinned_profile_manifest()
            })),
        )
        .expect("key transport should build")
    }

    #[test]
    fn pinned_manifest_contains_every_runtime_identity_field() {
        let manifest = build_pinned_profile_manifest();
        assert_eq!(
            manifest["schema_version"],
            CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION
        );
        assert_eq!(manifest["profile_id"], CODEX_OFFICIAL_WS_PROFILE_ID);
        assert_eq!(manifest["codex_commit"], CODEX_OFFICIAL_WS_CODEX_COMMIT);
        assert_eq!(
            manifest["tokio_tungstenite_rev"],
            CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV
        );
        assert_eq!(
            manifest["tungstenite_rev"],
            CODEX_OFFICIAL_WS_TUNGSTENITE_REV
        );
        assert_eq!(
            manifest["tungstenite_patch_id"],
            CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID
        );
        assert_eq!(
            manifest["write_buffer_size_bytes"],
            CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES
        );
        assert_eq!(
            manifest["max_write_buffer_size_bytes"],
            CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES
        );
        assert_eq!(
            manifest["max_retained_write_buffer_capacity_bytes"],
            CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES
        );
        assert_eq!(
            manifest["crypto_provider"],
            CODEX_OFFICIAL_WS_CRYPTO_PROVIDER
        );
    }

    #[test]
    fn admin_status_splits_profile_from_request_scoped_runtime_eligibility() {
        let payload = build_status_response(&provider(), &key(), &[endpoint()], enabled_flags());

        assert_eq!(payload["configured"], true);
        assert_eq!(payload["profile_effective"], true);
        assert_eq!(payload["runtime_eligible"], Value::Null);
        assert_eq!(payload["runtime_state"], "request_scoped");
        assert_eq!(payload["profile_reasons"], json!([]));
        assert_eq!(
            payload["runtime_reasons"],
            json!([
                "proxy_route_not_evaluated",
                "request_model_not_evaluated",
                "quota_runtime_state_not_evaluated",
                "circuit_runtime_state_not_evaluated",
                "concurrency_runtime_state_not_evaluated"
            ])
        );
        assert!(payload.get("effective").is_none());
    }

    #[test]
    fn admin_status_reports_known_profile_block_without_runtime_false_positive() {
        let mut invalid_endpoint = endpoint();
        invalid_endpoint.base_url = "https://example.com/backend-api/codex".to_string();

        let payload =
            build_status_response(&provider(), &key(), &[invalid_endpoint], enabled_flags());

        assert_eq!(payload["profile_effective"], false);
        assert_eq!(payload["runtime_eligible"], false);
        assert_eq!(payload["runtime_state"], "profile_blocked");
        assert_eq!(
            payload["profile_reasons"],
            json!(["official_endpoint_host_unsupported"])
        );
        assert_eq!(payload["runtime_reasons"], json!(["profile_not_effective"]));
    }

    #[test]
    fn admin_status_marks_disabled_account_soft_draining_not_runtime_active() {
        let mut disabled_key = key();
        disabled_key.capabilities = Some(json!({"codex_official_ws": false}));

        let payload =
            build_status_response(&provider(), &disabled_key, &[endpoint()], enabled_flags());

        assert_eq!(payload["configured"], false);
        assert_eq!(payload["runtime_eligible"], false);
        assert_eq!(payload["runtime_state"], "soft_draining");
    }
}
