use crate::handlers::admin::provider::ops::providers::actions::{
    admin_provider_ops_query_balance_response_for_credentials,
    admin_provider_ops_saved_connector_credentials,
};
use crate::handlers::admin::provider::shared::paths::admin_provider_id_for_key_balance;
use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::GatewayError;
use aether_admin::provider::ops::{
    admin_provider_ops_config_object, normalize_architecture_id,
    resolve_admin_provider_ops_base_url,
};
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
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

const BALANCE_QUERY_SECRET_CIPHERTEXT_KEY: &str = "secret_ciphertext";
const BALANCE_QUERY_SECRET_SAVED_AT_KEY: &str = "secret_saved_at";

#[derive(Debug, Deserialize)]
struct AdminProviderKeyBalanceRequest {
    #[serde(default)]
    key_id: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    auth_type: Option<String>,
    #[serde(default)]
    api_formats: Option<Vec<String>>,
    #[serde(default)]
    architecture_id: Option<String>,
    #[serde(default)]
    custom_base_url: Option<String>,
    #[serde(default)]
    new_api_user_id: Option<String>,
    #[serde(default)]
    sub2api_credential_kind: Option<String>,
    #[serde(default)]
    custom_endpoint: Option<String>,
    #[serde(default)]
    custom_method: Option<String>,
    #[serde(default)]
    custom_currency: Option<String>,
    #[serde(default)]
    custom_quota_divisor: Option<f64>,
    #[serde(default)]
    custom_balance_path: Option<String>,
    #[serde(default)]
    custom_used_path: Option<String>,
    #[serde(default)]
    custom_granted_path: Option<String>,
    #[serde(default)]
    auto_refresh_interval_minutes: Option<u32>,
    #[serde(default)]
    save_balance_secret: bool,
    #[serde(default)]
    save_result: bool,
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
        || decision.route_kind.as_deref() != Some("query_key_balance")
        || request_context.method() != http::Method::POST
        || !request_context
            .path()
            .starts_with("/api/admin/endpoints/providers/")
        || !request_context.path().ends_with("/key-balance")
    {
        return Ok(None);
    }

    let Some(provider_id) = admin_provider_id_for_key_balance(request_context.path()) else {
        return Ok(Some(not_found_response("Provider 不存在")));
    };
    let Some(request_body) = request_body.filter(|body| !body.is_empty()) else {
        return Ok(Some(bad_request_response("请求体不能为空")));
    };
    let payload = match serde_json::from_slice::<AdminProviderKeyBalanceRequest>(request_body) {
        Ok(payload) => payload,
        Err(_) => return Ok(Some(bad_request_response("请求体必须是合法的 JSON 对象"))),
    };

    let Some(provider) = state
        .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(Some(not_found_response(format!(
            "Provider {provider_id} 不存在"
        ))));
    };
    let endpoints = state
        .list_provider_catalog_endpoints_by_provider_ids(std::slice::from_ref(&provider_id))
        .await?;

    let stored_key =
        match load_optional_stored_key(state, &provider_id, payload.key_id.as_ref()).await? {
            Ok(key) => key,
            Err(response) => return Ok(Some(response)),
        };
    let auth_type = resolved_balance_auth_type(payload.auth_type.as_deref(), stored_key.as_ref());
    if !matches!(auth_type.as_str(), "api_key" | "bearer") {
        return Ok(Some(bad_request_response(
            "余额查询仅支持 API Key 或 Bearer Token",
        )));
    }

    let selected_endpoint = select_balance_endpoint(&endpoints, payload.api_formats.as_deref());
    let provider_ops_config = admin_provider_ops_config_object(&provider).cloned();
    let empty_provider_ops_config = Map::new();
    let provider_ops_config_ref = provider_ops_config
        .as_ref()
        .unwrap_or(&empty_provider_ops_config);
    let request_base_url = trimmed_request_string(payload.custom_base_url.as_ref())
        .map(|value| value.trim_end_matches('/').to_string());
    let base_url = request_base_url
        .or_else(|| {
            resolve_admin_provider_ops_base_url(&provider, &endpoints, provider_ops_config.as_ref())
        })
        .or_else(|| selected_endpoint.map(|endpoint| endpoint.base_url.clone()))
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty());
    let Some(base_url) = base_url else {
        return Ok(Some(bad_request_response("Provider 未配置 base_url")));
    };

    let architecture_id = payload
        .architecture_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_architecture_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            resolve_key_balance_architecture_id(&provider, provider_ops_config_ref, &base_url)
        });
    let action_config_override = build_balance_action_config_override(&payload, &architecture_id);
    let action_config_override_ref = if action_config_override.is_empty() {
        None
    } else {
        Some(&action_config_override)
    };
    let secret =
        match resolve_balance_secret(state, &payload, stored_key.as_ref(), &architecture_id) {
            Ok(secret) => secret,
            Err(detail) => return Ok(Some(bad_request_response(detail))),
        };
    let connector_config = provider_ops_connector_config(provider_ops_config_ref);
    let saved_credentials = admin_provider_ops_saved_connector_credentials(state, &provider);
    let credentials = match resolve_balance_credentials(
        &architecture_id,
        &auth_type,
        &secret,
        saved_credentials,
        &payload,
    ) {
        Ok(credentials) => credentials,
        Err(detail) => {
            return Ok(Some(
                Json(action_not_configured_payload(detail)).into_response(),
            ))
        }
    };
    let mut response_payload = admin_provider_ops_query_balance_response_for_credentials(
        state,
        &provider_id,
        &provider,
        &architecture_id,
        &base_url,
        provider_ops_config_ref,
        &connector_config,
        &credentials,
        action_config_override_ref,
    )
    .await;

    if payload.save_result {
        let save_outcome = match stored_key.as_ref() {
            Some(stored_key) => {
                persist_balance_result_metadata(
                    state,
                    stored_key,
                    &architecture_id,
                    &payload,
                    &response_payload,
                )
                .await
            }
            None => Ok(false),
        };
        attach_balance_result_save_status(
            &mut response_payload,
            save_outcome,
            stored_key.as_ref().map(|key| key.id.as_str()),
        );
    }

    Ok(Some(Json(response_payload).into_response()))
}

fn provider_ops_connector_config(provider_ops_config: &Map<String, Value>) -> Map<String, Value> {
    provider_ops_config
        .get("connector")
        .and_then(Value::as_object)
        .and_then(|connector| connector.get("config"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

async fn persist_balance_result_metadata(
    state: &AdminAppState<'_>,
    stored_key: &StoredProviderCatalogKey,
    architecture_id: &str,
    payload: &AdminProviderKeyBalanceRequest,
    response_payload: &Value,
) -> Result<bool, String> {
    if response_payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        != "success"
    {
        return Ok(false);
    }
    let Some(data) = response_payload.get("data").and_then(Value::as_object) else {
        return Ok(false);
    };

    let now = current_unix_secs();
    let extra = data
        .get("extra")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let plan_name = extra
        .get("plan_name")
        .or_else(|| extra.get("planName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let saved_secret = if balance_query_secret_matches_request(stored_key, architecture_id, payload)
    {
        balance_query_secret_ciphertext(stored_key)
    } else {
        None
    };
    let new_saved_secret = encrypted_balance_query_secret(state, payload)?;
    let secret_ciphertext = new_saved_secret
        .clone()
        .or_else(|| saved_secret.map(ToOwned::to_owned));
    let has_saved_secret = secret_ciphertext.is_some();
    let secret_saved_at = if new_saved_secret.is_some() {
        Some(now)
    } else if saved_secret.is_some() {
        balance_query_secret_saved_at(stored_key)
    } else {
        None
    };
    let mut balance_query = Map::new();
    balance_query.insert("updated_at".to_string(), json!(now));
    balance_query.insert(
        "architecture_id".to_string(),
        json!(normalize_architecture_id(architecture_id)),
    );
    balance_query.insert(
        "status".to_string(),
        response_payload
            .get("status")
            .cloned()
            .unwrap_or(Value::Null),
    );
    balance_query.insert(
        "executed_at".to_string(),
        response_payload
            .get("executed_at")
            .cloned()
            .unwrap_or(Value::Null),
    );
    balance_query.insert(
        "response_time_ms".to_string(),
        response_payload
            .get("response_time_ms")
            .cloned()
            .unwrap_or(Value::Null),
    );
    balance_query.insert(
        "total_available".to_string(),
        data.get("total_available").cloned().unwrap_or(Value::Null),
    );
    balance_query.insert(
        "total_used".to_string(),
        data.get("total_used").cloned().unwrap_or(Value::Null),
    );
    balance_query.insert(
        "total_granted".to_string(),
        data.get("total_granted").cloned().unwrap_or(Value::Null),
    );
    balance_query.insert(
        "currency".to_string(),
        data.get("currency")
            .cloned()
            .unwrap_or_else(|| json!("USD")),
    );
    balance_query.insert("plan_name".to_string(), json!(plan_name));
    balance_query.insert(
        "query_config".to_string(),
        balance_query_config_metadata(payload, architecture_id, has_saved_secret),
    );
    balance_query.insert("extra".to_string(), Value::Object(extra));
    if let Some(ciphertext) = secret_ciphertext {
        balance_query.insert(
            BALANCE_QUERY_SECRET_CIPHERTEXT_KEY.to_string(),
            Value::String(ciphertext),
        );
    }
    if let Some(saved_at) = secret_saved_at {
        balance_query.insert(
            BALANCE_QUERY_SECRET_SAVED_AT_KEY.to_string(),
            json!(saved_at),
        );
    }

    let metadata_update = json!({
        "balance_query": Value::Object(balance_query),
    });
    let merged = merge_upstream_metadata(stored_key.upstream_metadata.as_ref(), &metadata_update);
    state
        .update_provider_catalog_key_upstream_metadata(&stored_key.id, Some(&merged), Some(now))
        .await
        .map_err(|err| format!("{err:?}"))
}

fn balance_query_config_metadata(
    payload: &AdminProviderKeyBalanceRequest,
    architecture_id: &str,
    has_saved_secret: bool,
) -> Value {
    let normalized_architecture_id = normalize_architecture_id(architecture_id);
    let mut config = Map::new();

    config.insert("has_saved_secret".to_string(), json!(has_saved_secret));

    if let Some(base_url) = trimmed_request_string(payload.custom_base_url.as_ref()) {
        config.insert("custom_base_url".to_string(), Value::String(base_url));
    }
    if let Some(interval_minutes) = payload
        .auto_refresh_interval_minutes
        .filter(|value| *value > 0)
        .map(|value| value.min(10_080))
    {
        config.insert(
            "auto_refresh_interval_minutes".to_string(),
            json!(interval_minutes),
        );
    }

    match normalized_architecture_id {
        "new_api" => {
            if let Some(user_id) = trimmed_request_string(payload.new_api_user_id.as_ref()) {
                config.insert("new_api_user_id".to_string(), Value::String(user_id));
            }
        }
        "sub2api" => {
            if let Some(kind) =
                normalized_sub2api_credential_kind(payload.sub2api_credential_kind.as_deref())
            {
                config.insert("sub2api_credential_kind".to_string(), Value::String(kind));
            }
        }
        "generic_api" => {
            if let Some(endpoint) = trimmed_request_string(payload.custom_endpoint.as_ref()) {
                config.insert("custom_endpoint".to_string(), Value::String(endpoint));
            }
            if let Some(method) = normalized_request_method(payload.custom_method.as_deref()) {
                config.insert("custom_method".to_string(), Value::String(method));
            }
            if let Some(currency) = trimmed_request_string(payload.custom_currency.as_ref()) {
                config.insert("custom_currency".to_string(), Value::String(currency));
            }
            if let Some(divisor) = payload
                .custom_quota_divisor
                .filter(|value| value.is_finite() && *value > 0.0)
            {
                config.insert("custom_quota_divisor".to_string(), json!(divisor));
            }
            if let Some(path) = trimmed_request_string(payload.custom_balance_path.as_ref()) {
                config.insert("custom_balance_path".to_string(), Value::String(path));
            }
            if let Some(path) = trimmed_request_string(payload.custom_used_path.as_ref()) {
                config.insert("custom_used_path".to_string(), Value::String(path));
            }
            if let Some(path) = trimmed_request_string(payload.custom_granted_path.as_ref()) {
                config.insert("custom_granted_path".to_string(), Value::String(path));
            }
        }
        _ => {}
    }

    Value::Object(config)
}

fn encrypted_balance_query_secret(
    state: &AdminAppState<'_>,
    payload: &AdminProviderKeyBalanceRequest,
) -> Result<Option<String>, String> {
    if !payload.save_balance_secret {
        return Ok(None);
    }
    let Some(secret) = payload
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    state
        .encrypt_catalog_secret_with_fallbacks(secret)
        .map(Some)
        .ok_or_else(|| "gateway 未配置余额查询凭据加密密钥".to_string())
}

fn attach_balance_result_save_status(
    response_payload: &mut Value,
    outcome: Result<bool, String>,
    key_id: Option<&str>,
) {
    let Some(object) = response_payload.as_object_mut() else {
        return;
    };
    match outcome {
        Ok(true) => {
            object.insert("saved_to_key".to_string(), Value::Bool(true));
            object.insert(
                "saved_key_id".to_string(),
                key_id
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
            object.insert("save_message".to_string(), Value::Null);
        }
        Ok(false) => {
            object.insert("saved_to_key".to_string(), Value::Bool(false));
            object.insert(
                "saved_key_id".to_string(),
                key_id
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
            object.insert(
                "save_message".to_string(),
                Value::String("查询未成功或当前没有可保存的结果，未写入 Key".to_string()),
            );
        }
        Err(message) => {
            object.insert("saved_to_key".to_string(), Value::Bool(false));
            object.insert(
                "saved_key_id".to_string(),
                key_id
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
            object.insert(
                "save_message".to_string(),
                Value::String(format!("保存余额结果失败: {message}")),
            );
        }
    }
}

fn merge_upstream_metadata(current: Option<&Value>, updates: &Value) -> Value {
    let mut merged = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(update_object) = updates.as_object() {
        for (key, value) in update_object {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn build_balance_action_config_override(
    payload: &AdminProviderKeyBalanceRequest,
    architecture_id: &str,
) -> Map<String, Value> {
    let mut config = Map::new();
    if normalize_architecture_id(architecture_id) != "generic_api" {
        return config;
    }

    if let Some(endpoint) = trimmed_request_string(payload.custom_endpoint.as_ref()) {
        config.insert("endpoint".to_string(), Value::String(endpoint));
    }
    if let Some(method) = normalized_request_method(payload.custom_method.as_deref()) {
        config.insert("method".to_string(), Value::String(method));
    }
    if let Some(currency) = trimmed_request_string(payload.custom_currency.as_ref()) {
        config.insert("currency".to_string(), Value::String(currency));
    }
    if let Some(divisor) = payload
        .custom_quota_divisor
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        config.insert("quota_divisor".to_string(), json!(divisor));
    }
    if let Some(path) = trimmed_request_string(payload.custom_balance_path.as_ref()) {
        config.insert("balance_path".to_string(), Value::String(path));
    }
    if let Some(path) = trimmed_request_string(payload.custom_used_path.as_ref()) {
        config.insert("used_path".to_string(), Value::String(path));
    }
    if let Some(path) = trimmed_request_string(payload.custom_granted_path.as_ref()) {
        config.insert("granted_path".to_string(), Value::String(path));
    }

    config
}

fn trimmed_request_string(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalized_request_method(value: Option<&str>) -> Option<String> {
    match value.map(str::trim).map(str::to_ascii_uppercase).as_deref() {
        Some("GET") => Some("GET".to_string()),
        Some("POST") => Some("POST".to_string()),
        _ => None,
    }
}

async fn load_optional_stored_key(
    state: &AdminAppState<'_>,
    provider_id: &str,
    key_id: Option<&String>,
) -> Result<Result<Option<StoredProviderCatalogKey>, Response<Body>>, GatewayError> {
    let Some(key_id) = key_id
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return Ok(Ok(None));
    };
    let Some(key) = state
        .read_provider_catalog_keys_by_ids(&[key_id.to_string()])
        .await?
        .into_iter()
        .next()
    else {
        return Ok(Err(not_found_response(format!("Key {key_id} 不存在"))));
    };
    if key.provider_id != provider_id {
        return Ok(Err(not_found_response("Key 不属于当前 Provider")));
    }
    Ok(Ok(Some(key)))
}

fn resolved_balance_auth_type(
    request_auth_type: Option<&str>,
    stored_key: Option<&StoredProviderCatalogKey>,
) -> String {
    request_auth_type
        .or_else(|| stored_key.map(|key| key.auth_type.as_str()))
        .unwrap_or("api_key")
        .trim()
        .to_ascii_lowercase()
}

fn resolve_balance_secret(
    state: &AdminAppState<'_>,
    payload: &AdminProviderKeyBalanceRequest,
    stored_key: Option<&StoredProviderCatalogKey>,
    architecture_id: &str,
) -> Result<String, String> {
    if let Some(secret) = payload
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(secret.to_string());
    }

    let Some(stored_key) = stored_key else {
        return Err("请输入 API 密钥后再查询余额".to_string());
    };
    if let Some(secret) =
        resolve_saved_balance_query_secret(state, stored_key, architecture_id, payload)?
    {
        return Ok(secret);
    }
    if balance_query_requires_saved_secret(architecture_id, payload) {
        return Err(balance_query_missing_saved_secret_message(
            architecture_id,
            payload,
        ));
    }
    let ciphertext = stored_key
        .encrypted_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "该 Key 没有可用的密钥内容".to_string())?;
    let secret = state
        .decrypt_catalog_secret_with_fallbacks(ciphertext)
        .ok_or_else(|| "无法解密 API Key，可能是加密密钥已更改".to_string())?;
    let secret = secret.trim();
    if secret.is_empty() || secret == "__placeholder__" {
        return Err("该 Key 没有可用的密钥内容".to_string());
    }
    Ok(secret.to_string())
}

fn balance_query_requires_saved_secret(
    architecture_id: &str,
    payload: &AdminProviderKeyBalanceRequest,
) -> bool {
    match normalize_architecture_id(architecture_id) {
        "new_api" => true,
        "sub2api" => matches!(
            normalized_sub2api_credential_kind(payload.sub2api_credential_kind.as_deref())
                .as_deref(),
            Some("access_token" | "refresh_token")
        ),
        _ => false,
    }
}

fn balance_query_missing_saved_secret_message(
    architecture_id: &str,
    payload: &AdminProviderKeyBalanceRequest,
) -> String {
    match normalize_architecture_id(architecture_id) {
        "new_api" => {
            "NewAPI 余额刷新需要个人安全设置里的访问令牌。请先打开“查询余额”，填写访问令牌并开启“保存余额查询凭据”。"
                .to_string()
        }
        "sub2api"
            if matches!(
                normalized_sub2api_credential_kind(payload.sub2api_credential_kind.as_deref())
                    .as_deref(),
                Some("access_token" | "refresh_token")
            ) =>
        {
            "Sub2API 使用 Access/Refresh Token 查询余额时，需要先手动查询并保存余额查询凭据。"
                .to_string()
        }
        _ => "请先保存余额查询凭据后再刷新".to_string(),
    }
}

fn resolve_saved_balance_query_secret(
    state: &AdminAppState<'_>,
    stored_key: &StoredProviderCatalogKey,
    architecture_id: &str,
    payload: &AdminProviderKeyBalanceRequest,
) -> Result<Option<String>, String> {
    let Some(ciphertext) = balance_query_secret_ciphertext(stored_key) else {
        return Ok(None);
    };
    if !balance_query_secret_matches_request(stored_key, architecture_id, payload) {
        return Ok(None);
    }
    let plaintext = state
        .decrypt_catalog_secret_with_fallbacks(ciphertext)
        .ok_or_else(|| "无法解密已保存的余额查询凭据，可能是加密密钥已更改".to_string())?;
    let secret = plaintext.trim();
    if secret.is_empty() {
        return Ok(None);
    }
    Ok(Some(secret.to_string()))
}

fn balance_query_secret_ciphertext(stored_key: &StoredProviderCatalogKey) -> Option<&str> {
    stored_key
        .upstream_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("balance_query"))
        .and_then(Value::as_object)
        .and_then(|balance| balance.get(BALANCE_QUERY_SECRET_CIPHERTEXT_KEY))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn balance_query_secret_saved_at(stored_key: &StoredProviderCatalogKey) -> Option<u64> {
    stored_key
        .upstream_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("balance_query"))
        .and_then(Value::as_object)
        .and_then(|balance| balance.get(BALANCE_QUERY_SECRET_SAVED_AT_KEY))
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(raw) => raw.trim().parse::<u64>().ok(),
            _ => None,
        })
}

fn balance_query_secret_matches_request(
    stored_key: &StoredProviderCatalogKey,
    architecture_id: &str,
    payload: &AdminProviderKeyBalanceRequest,
) -> bool {
    let Some(balance_query) = stored_key
        .upstream_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("balance_query"))
        .and_then(Value::as_object)
    else {
        return true;
    };
    let Some(stored_architecture_id) = balance_query
        .get("architecture_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return true;
    };
    let normalized_architecture_id = normalize_architecture_id(architecture_id);
    if normalize_architecture_id(stored_architecture_id) != normalized_architecture_id {
        return false;
    }
    if normalized_architecture_id != "sub2api" {
        return true;
    }

    let stored_kind = balance_query
        .get("query_config")
        .and_then(Value::as_object)
        .and_then(|config| config.get("sub2api_credential_kind"))
        .and_then(Value::as_str)
        .and_then(|value| normalized_sub2api_credential_kind(Some(value)));
    let requested_kind =
        normalized_sub2api_credential_kind(payload.sub2api_credential_kind.as_deref());
    match (stored_kind, requested_kind) {
        (Some(stored_kind), Some(requested_kind)) => stored_kind == requested_kind,
        _ => true,
    }
}

fn select_balance_endpoint<'a>(
    endpoints: &'a [StoredProviderCatalogEndpoint],
    api_formats: Option<&[String]>,
) -> Option<&'a StoredProviderCatalogEndpoint> {
    let requested_formats = api_formats
        .into_iter()
        .flatten()
        .map(|format| crate::ai_serving::normalize_api_format_alias(format))
        .collect::<BTreeSet<_>>();

    endpoints
        .iter()
        .find(|endpoint| {
            endpoint.is_active
                && !requested_formats.is_empty()
                && requested_formats.contains(&crate::ai_serving::normalize_api_format_alias(
                    &endpoint.api_format,
                ))
        })
        .or_else(|| endpoints.iter().find(|endpoint| endpoint.is_active))
        .or_else(|| endpoints.first())
}

fn resolve_key_balance_architecture_id(
    provider: &StoredProviderCatalogProvider,
    provider_ops_config: &Map<String, Value>,
    base_url: &str,
) -> String {
    if let Some(architecture_id) = provider_ops_config
        .get("architecture_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return architecture_id.to_string();
    }

    let fingerprint =
        format!("{} {} {}", provider.provider_type, provider.name, base_url).to_ascii_lowercase();
    if fingerprint.contains("sub2api") {
        "sub2api".to_string()
    } else {
        "new_api".to_string()
    }
}

fn resolve_balance_credentials(
    architecture_id: &str,
    auth_type: &str,
    secret: &str,
    saved_credentials: Map<String, Value>,
    payload: &AdminProviderKeyBalanceRequest,
) -> Result<Map<String, Value>, String> {
    match normalize_architecture_id(architecture_id) {
        "new_api" => resolve_new_api_balance_credentials(
            auth_type,
            secret,
            saved_credentials,
            payload.new_api_user_id.as_ref(),
        ),
        "sub2api" => resolve_sub2api_balance_credentials(
            auth_type,
            secret,
            saved_credentials,
            payload.sub2api_credential_kind.as_deref(),
        ),
        normalized => Ok(balance_credentials_for_architecture(
            normalized, auth_type, secret,
        )),
    }
}

fn resolve_new_api_balance_credentials(
    auth_type: &str,
    secret: &str,
    saved_credentials: Map<String, Value>,
    request_user_id: Option<&String>,
) -> Result<Map<String, Value>, String> {
    let mut credentials = balance_credentials_for_architecture("new_api", auth_type, secret);
    if let Some(user_id) = request_user_id
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .or_else(|| non_empty_string(&saved_credentials, "user_id"))
    {
        credentials.insert("user_id".to_string(), Value::String(user_id.to_string()));
    }

    Ok(credentials)
}

fn resolve_sub2api_balance_credentials(
    auth_type: &str,
    secret: &str,
    saved_credentials: Map<String, Value>,
    credential_kind: Option<&str>,
) -> Result<Map<String, Value>, String> {
    match normalized_sub2api_credential_kind(credential_kind).as_deref() {
        Some("api_key") => {
            let mut credentials = Map::new();
            credentials.insert("api_key".to_string(), Value::String(secret.to_string()));
            return Ok(credentials);
        }
        Some("access_token") => {
            let mut credentials = Map::new();
            credentials.insert(
                "_cached_access_token".to_string(),
                Value::String(secret.to_string()),
            );
            credentials.insert(
                "_cached_token_expires_at".to_string(),
                Value::from(balance_token_fallback_expires_at()),
            );
            return Ok(credentials);
        }
        Some("refresh_token") => {
            let mut credentials = Map::new();
            credentials.insert("refresh_token".to_string(), json!(secret));
            return Ok(credentials);
        }
        _ => {}
    }

    if auth_type == "bearer" || looks_like_jwt(secret) {
        let mut credentials = Map::new();
        credentials.insert(
            "_cached_access_token".to_string(),
            Value::String(secret.to_string()),
        );
        credentials.insert(
            "_cached_token_expires_at".to_string(),
            Value::from(balance_token_fallback_expires_at()),
        );
        return Ok(credentials);
    }
    if looks_like_model_api_key(secret) || auth_type == "api_key" {
        let mut credentials = Map::new();
        credentials.insert("api_key".to_string(), Value::String(secret.to_string()));
        return Ok(credentials);
    }
    if let Some(api_key) = non_empty_string(&saved_credentials, "api_key") {
        let mut credentials = saved_credentials.clone();
        credentials.insert("api_key".to_string(), Value::String(api_key.to_string()));
        return Ok(credentials);
    }
    if has_non_empty_string(&saved_credentials, "_cached_access_token")
        || has_non_empty_string(&saved_credentials, "refresh_token")
        || (has_non_empty_string(&saved_credentials, "email")
            && has_non_empty_string(&saved_credentials, "password"))
    {
        return Ok(saved_credentials);
    }

    let mut credentials = Map::new();
    credentials.insert("refresh_token".to_string(), json!(secret));
    Ok(credentials)
}

fn normalized_sub2api_credential_kind(value: Option<&str>) -> Option<String> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("api_key" | "apikey" | "api-key") => Some("api_key".to_string()),
        Some("access_token" | "access-token" | "access" | "bearer") => {
            Some("access_token".to_string())
        }
        Some("refresh_token" | "refresh-token" | "refresh") => Some("refresh_token".to_string()),
        _ => None,
    }
}

fn balance_credentials_for_architecture(
    architecture_id: &str,
    _auth_type: &str,
    secret: &str,
) -> Map<String, Value> {
    let mut credentials = Map::new();
    if normalize_architecture_id(architecture_id) == "sub2api" {
        credentials.insert("refresh_token".to_string(), json!(secret));
    } else {
        credentials.insert("api_key".to_string(), json!(secret));
    }
    credentials
}

fn non_empty_string<'a>(credentials: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    credentials
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn has_non_empty_string(credentials: &Map<String, Value>, key: &str) -> bool {
    non_empty_string(credentials, key).is_some()
}

fn looks_like_jwt(secret: &str) -> bool {
    let trimmed = secret.trim();
    let mut parts = trimmed.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty()
    )
}

fn looks_like_model_api_key(secret: &str) -> bool {
    let normalized = secret.trim().to_ascii_lowercase();
    normalized.starts_with("sk-") || normalized.starts_with("sk_")
}

fn balance_token_fallback_expires_at() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs_f64() + 600.0)
        .unwrap_or(600.0)
}

fn action_not_configured_payload(detail: impl Into<String>) -> Value {
    json!({
        "status": "not_configured",
        "action_type": "query_balance",
        "data": Value::Null,
        "message": detail.into(),
        "executed_at": chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "response_time_ms": Value::Null,
        "cache_ttl_seconds": 0,
    })
}

fn bad_request_response(detail: impl Into<String>) -> Response<Body> {
    (
        http::StatusCode::BAD_REQUEST,
        Json(json!({ "detail": detail.into() })),
    )
        .into_response()
}

fn not_found_response(detail: impl Into<String>) -> Response<Body> {
    (
        http::StatusCode::NOT_FOUND,
        Json(json!({ "detail": detail.into() })),
    )
        .into_response()
}
