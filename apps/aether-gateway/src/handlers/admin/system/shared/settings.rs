use crate::handlers::admin::request::AdminAppState;
use crate::handlers::shared::{system_config_bool, system_config_string};
use crate::GatewayError;
use aether_admin::system::{
    build_admin_api_formats_payload as build_admin_api_formats_payload_pure,
    build_admin_system_check_update_payload as build_admin_system_check_update_payload_pure,
    build_admin_system_check_update_payload_with_release,
    build_admin_system_settings_payload as build_admin_system_settings_payload_pure,
    build_admin_system_settings_updated_payload,
    build_admin_system_stats_payload as build_admin_system_stats_payload_pure,
    parse_admin_system_settings_update, AdminSystemUpdateRelease,
};
use axum::body::Bytes;
use axum::http;
#[cfg(not(test))]
use serde::Deserialize;
use serde_json::json;
#[cfg(not(test))]
use std::time::Duration;

const AETHER_RELEASES_API_URL: &str =
    "https://api.github.com/repos/Shirtiny/Aether/releases?per_page=20";

pub(crate) fn current_aether_version() -> String {
    option_env!("AETHER_BUILD_VERSION")
        .filter(|version| !version.is_empty())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string()
}

pub(crate) fn build_admin_system_check_update_payload() -> serde_json::Value {
    build_admin_system_check_update_payload_pure(current_aether_version())
}

pub(crate) fn build_admin_system_check_update_payload_from_release(
    latest_release: Option<AdminSystemUpdateRelease>,
    error: Option<String>,
) -> serde_json::Value {
    build_admin_system_check_update_payload_with_release(
        current_aether_version(),
        latest_release,
        error,
    )
}

#[cfg(not(test))]
pub(crate) async fn fetch_latest_admin_system_release(
) -> (Option<AdminSystemUpdateRelease>, Option<String>) {
    match fetch_latest_admin_system_release_inner().await {
        Ok(release) => (release, None),
        Err(err) => (None, Some(err)),
    }
}

#[cfg(test)]
pub(crate) async fn fetch_latest_admin_system_release(
) -> (Option<AdminSystemUpdateRelease>, Option<String>) {
    (None, Some("测试环境未请求 GitHub Releases".to_string()))
}

#[cfg(not(test))]
async fn fetch_latest_admin_system_release_inner(
) -> Result<Option<AdminSystemUpdateRelease>, String> {
    let releases: Vec<GitHubRelease> = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|err| format!("创建更新检查客户端失败: {err}"))?
        .get(AETHER_RELEASES_API_URL)
        .header(reqwest::header::USER_AGENT, "Aether-Gateway update-check")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| format!("请求 GitHub Releases 失败: {err}"))?
        .error_for_status()
        .map_err(|err| format!("GitHub Releases 返回错误: {err}"))?
        .json()
        .await
        .map_err(|err| format!("解析 GitHub Releases 失败: {err}"))?;

    Ok(releases
        .into_iter()
        .find(|release| !release.draft && release.tag_name.starts_with('v'))
        .map(|release| AdminSystemUpdateRelease {
            version: release.tag_name,
            release_url: Some(release.html_url),
            release_notes: release.body.filter(|body| !body.trim().is_empty()),
            published_at: release.published_at,
        }))
}

#[cfg(not(test))]
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
}

pub(crate) async fn build_admin_system_stats_payload(
    state: &AdminAppState<'_>,
) -> Result<serde_json::Value, GatewayError> {
    let providers = state
        .list_provider_catalog_providers(false)
        .await
        .unwrap_or_default();
    let total_providers = providers.len() as u64;
    let active_providers = providers
        .iter()
        .filter(|provider| provider.is_active)
        .count() as u64;
    let stats = state.read_admin_system_stats().await?;

    Ok(build_admin_system_stats_payload_pure(
        stats.total_users,
        stats.active_users,
        total_providers,
        active_providers,
        stats.total_api_keys,
        stats.total_requests,
    ))
}

pub(crate) async fn build_admin_system_settings_payload(
    state: &AdminAppState<'_>,
) -> Result<serde_json::Value, GatewayError> {
    let default_provider_config = state
        .read_system_config_json_value("default_provider")
        .await?;
    let default_model_config = state.read_system_config_json_value("default_model").await?;
    let enable_usage_tracking_config = state
        .read_system_config_json_value("enable_usage_tracking")
        .await?;
    let password_policy_level_config = state
        .read_system_config_json_value("password_policy_level")
        .await?;

    let default_provider = match system_config_string(default_provider_config.as_ref()) {
        Some(value) => Some(value),
        None => state
            .list_provider_catalog_providers(false)
            .await
            .ok()
            .unwrap_or_default()
            .into_iter()
            .find(|provider| provider.is_active)
            .map(|provider| provider.name),
    };
    let default_model = system_config_string(default_model_config.as_ref());
    let enable_usage_tracking = system_config_bool(enable_usage_tracking_config.as_ref(), true);
    let password_policy_level = match system_config_string(password_policy_level_config.as_ref()) {
        Some(value) if matches!(value.as_str(), "weak" | "medium" | "strong") => value,
        _ => "weak".to_string(),
    };

    Ok(build_admin_system_settings_payload_pure(
        default_provider,
        default_model,
        enable_usage_tracking,
        password_policy_level,
    ))
}

pub(crate) async fn apply_admin_system_settings_update(
    state: &AdminAppState<'_>,
    request_body: &Bytes,
) -> Result<Result<serde_json::Value, (http::StatusCode, serde_json::Value)>, GatewayError> {
    let update = match parse_admin_system_settings_update(request_body) {
        Ok(update) => update,
        Err(err) => return Ok(Err(err)),
    };

    if let Some(default_provider) = update.default_provider {
        if let Some(default_provider) = default_provider {
            let provider_exists = state
                .list_provider_catalog_providers(false)
                .await
                .ok()
                .unwrap_or_default()
                .into_iter()
                .any(|provider| provider.is_active && provider.name == default_provider);
            if !provider_exists {
                return Ok(Err((
                    http::StatusCode::BAD_REQUEST,
                    json!({ "detail": format!("提供商 '{default_provider}' 不存在或未启用") }),
                )));
            }
            let _ = state
                .upsert_system_config_json_value(
                    "default_provider",
                    &json!(default_provider),
                    Some("系统默认提供商，当用户未设置个人提供商时使用"),
                )
                .await?;
        } else {
            let _ = state
                .upsert_system_config_json_value("default_provider", &serde_json::Value::Null, None)
                .await?;
        }
    }

    if let Some(default_model) = update.default_model {
        let config_value = default_model
            .map(|value| json!(value))
            .unwrap_or(serde_json::Value::Null);
        let _ = state
            .upsert_system_config_json_value("default_model", &config_value, None)
            .await?;
    }

    if let Some(enable_usage_tracking) = update.enable_usage_tracking {
        let _ = state
            .upsert_system_config_json_value(
                "enable_usage_tracking",
                &json!(enable_usage_tracking),
                None,
            )
            .await?;
    }

    if let Some(password_policy_level) = update.password_policy_level {
        let _ = state
            .upsert_system_config_json_value(
                "password_policy_level",
                &json!(password_policy_level),
                None,
            )
            .await?;
    }

    Ok(Ok(build_admin_system_settings_updated_payload()))
}

pub(crate) fn build_admin_api_formats_payload() -> serde_json::Value {
    build_admin_api_formats_payload_pure()
}
