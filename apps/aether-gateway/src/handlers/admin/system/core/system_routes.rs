use super::ADMIN_AWS_REGIONS;
use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::handlers::admin::shared::attach_admin_audit_response;
use crate::handlers::admin::shared::build_proxy_error_response;
use crate::handlers::admin::system::shared::configs::{
    apply_admin_system_config_update, build_admin_system_config_detail_payload,
    build_admin_system_configs_payload, delete_admin_system_config,
};
use crate::handlers::admin::system::shared::paths::{
    admin_system_config_key_from_path, admin_system_email_template_preview_type_from_path,
    admin_system_email_template_reset_type_from_path, admin_system_email_template_type_from_path,
    is_admin_system_configs_root, is_admin_system_email_templates_root,
};
use crate::handlers::admin::system::shared::settings::{
    apply_admin_system_settings_update, build_admin_api_formats_payload,
    build_admin_system_check_update_payload_from_release, build_admin_system_settings_payload,
    build_admin_system_stats_payload, current_aether_version, fetch_latest_admin_system_release,
};
use crate::handlers::admin::system::shared::smtp::build_admin_smtp_test_payload;
use crate::GatewayError;
use axum::{
    body::{Body, Bytes},
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::time::Instant;

pub(super) async fn maybe_build_local_admin_core_system_response(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
    request_body: Option<&Bytes>,
) -> Result<Option<Response<Body>>, GatewayError> {
    let Some(decision) = request_context.decision() else {
        return Ok(None);
    };
    let request_method = request_context.method();
    let request_path = request_context.path();
    if decision.route_family.as_deref() != Some("system_manage") {
        return Ok(None);
    }

    if decision.route_kind.as_deref() == Some("version")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/version"
    {
        return Ok(Some(
            Json(json!({ "version": current_aether_version() })).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("check_update")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/check-update"
    {
        let (latest_release, error) = fetch_latest_admin_system_release().await;
        return Ok(Some(
            Json(build_admin_system_check_update_payload_from_release(
                latest_release,
                error,
            ))
            .into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("aws_regions")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/aws-regions"
    {
        return Ok(Some(
            Json(json!({ "regions": ADMIN_AWS_REGIONS })).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("stats")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/stats"
    {
        return Ok(Some(
            Json(build_admin_system_stats_payload(state).await?).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("settings_get")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/settings"
    {
        return Ok(Some(
            Json(build_admin_system_settings_payload(state).await?).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("config_export")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/config/export"
    {
        return Ok(Some(attach_admin_audit_response(
            Json(state.build_admin_system_config_export_payload().await?).into_response(),
            "admin_system_config_exported",
            "export_system_config",
            "system_config_export",
            "global",
        )));
    }

    if decision.route_kind.as_deref() == Some("config_import")
        && request_method == http::Method::POST
        && request_path == "/api/admin/system/config/import"
    {
        let Some(request_body) = request_body else {
            return Ok(Some(
                (
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": "请求数据验证失败" })),
                )
                    .into_response(),
            ));
        };
        return Ok(Some(
            match state.import_admin_system_config(request_body).await? {
                Ok(payload) => attach_admin_audit_response(
                    Json(payload).into_response(),
                    "admin_system_config_imported",
                    "import_system_config",
                    "system_config_import",
                    "global",
                ),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("users_export")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/users/export"
    {
        return Ok(Some(attach_admin_audit_response(
            Json(state.build_admin_system_users_export_payload().await?).into_response(),
            "admin_system_users_exported",
            "export_system_users",
            "user_export",
            "all_users",
        )));
    }

    if decision.route_kind.as_deref() == Some("users_import")
        && request_method == http::Method::POST
        && request_path == "/api/admin/system/users/import"
    {
        let Some(request_body) = request_body else {
            return Ok(Some(
                (
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": "请求数据验证失败" })),
                )
                    .into_response(),
            ));
        };
        return Ok(Some(
            match state
                .import_admin_system_users(
                    request_body,
                    decision
                        .admin_principal
                        .as_ref()
                        .map(|principal| principal.user_id.as_str()),
                )
                .await?
            {
                Ok(payload) => attach_admin_audit_response(
                    Json(payload).into_response(),
                    "admin_system_users_imported",
                    "import_system_users",
                    "system_users_import",
                    "global",
                ),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("data_export")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/data/export"
    {
        return Ok(Some(attach_admin_audit_response(
            Json(state.build_admin_system_data_export_payload().await?).into_response(),
            "admin_system_data_exported",
            "export_system_data",
            "system_data_export",
            "global",
        )));
    }

    if decision.route_kind.as_deref() == Some("data_import")
        && request_method == http::Method::POST
        && request_path == "/api/admin/system/data/import"
    {
        let Some(request_body) = request_body else {
            return Ok(Some(
                (
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": "请求数据验证失败" })),
                )
                    .into_response(),
            ));
        };
        return Ok(Some(
            match state
                .import_admin_system_data(
                    request_body,
                    decision
                        .admin_principal
                        .as_ref()
                        .map(|principal| principal.user_id.as_str()),
                )
                .await?
            {
                Ok(payload) => attach_admin_audit_response(
                    Json(payload).into_response(),
                    "admin_system_data_imported",
                    "import_system_data",
                    "system_data_import",
                    "global",
                ),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("smtp_test")
        && request_method == http::Method::POST
        && request_path == "/api/admin/system/smtp/test"
    {
        return Ok(Some(
            Json(build_admin_smtp_test_payload(state, request_body).await?).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("cleanup") && request_method == http::Method::POST {
        return Ok(Some(attach_admin_audit_response(
            Json(build_admin_system_cleanup_payload(state).await?).into_response(),
            "admin_system_cleanup_completed",
            "cleanup_system_data",
            "system_cleanup",
            "global",
        )));
    }

    if decision.route_kind.as_deref() == Some("cleanup_runs")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/cleanup/runs"
    {
        let records = crate::maintenance::list_admin_cleanup_run_records(&state.app().data)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        return Ok(Some(Json(json!({ "items": records })).into_response()));
    }

    if decision.route_kind.as_deref() == Some("cleanup_usage_manual")
        && request_method == http::Method::POST
        && request_path == "/api/admin/system/cleanup/usage/manual"
    {
        return Ok(Some(
            build_manual_usage_cleanup_response(state, request_body).await?,
        ));
    }

    if decision.route_kind.as_deref() == Some("cleanup_usage_preview")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/cleanup/usage/preview"
    {
        return Ok(Some(
            build_manual_usage_cleanup_preview_response(state, request_context).await?,
        ));
    }

    if let Some((task_kind, action, object_type, object_id)) =
        admin_system_purge_task_for_route_kind(decision.route_kind.as_deref())
    {
        if request_method != http::Method::POST {
            return Ok(None);
        }
        let task = crate::maintenance::start_admin_system_purge_task(state.cloned_app(), task_kind)
            .await?;
        return Ok(Some(attach_admin_audit_response(
            Json(json!({
                "message": task.message.clone(),
                "task": task,
            }))
            .into_response(),
            "admin_system_purge_task_started",
            action,
            object_type,
            object_id,
        )));
    }

    if decision.route_kind.as_deref() == Some("settings_set")
        && request_method == http::Method::PUT
        && request_path == "/api/admin/system/settings"
    {
        let Some(request_body) = request_body else {
            return Ok(Some(
                (
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": "请求数据验证失败" })),
                )
                    .into_response(),
            ));
        };
        return Ok(Some(
            match apply_admin_system_settings_update(state, request_body).await? {
                Ok(payload) => attach_admin_audit_response(
                    Json(payload).into_response(),
                    "admin_system_settings_updated",
                    "update_system_settings",
                    "system_settings",
                    "global",
                ),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("configs_list")
        && request_method == http::Method::GET
        && is_admin_system_configs_root(request_path)
    {
        let entries = state.list_system_config_entries().await?;
        return Ok(Some(
            Json(build_admin_system_configs_payload(&entries)).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("config_get") && request_method == http::Method::GET {
        let Some(config_key) = admin_system_config_key_from_path(request_path) else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "配置项不存在",
                None,
            )));
        };
        return Ok(Some(
            match build_admin_system_config_detail_payload(state, &config_key).await? {
                Ok(payload) => Json(payload).into_response(),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("config_set") && request_method == http::Method::PUT {
        let Some(config_key) = admin_system_config_key_from_path(request_path) else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "配置项不存在",
                None,
            )));
        };
        let Some(request_body) = request_body else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求数据验证失败",
                None,
            )));
        };
        return Ok(Some(
            match apply_admin_system_config_update(state, &config_key, request_body).await? {
                Ok(payload) => attach_admin_audit_response(
                    Json(payload).into_response(),
                    "admin_system_config_updated",
                    "update_system_config",
                    "system_config",
                    &config_key,
                ),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("config_delete")
        && request_method == http::Method::DELETE
    {
        let Some(config_key) = admin_system_config_key_from_path(request_path) else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "配置项不存在",
                None,
            )));
        };
        return Ok(Some(
            match delete_admin_system_config(state, &config_key).await? {
                Ok(payload) => attach_admin_audit_response(
                    Json(payload).into_response(),
                    "admin_system_config_deleted",
                    "delete_system_config",
                    "system_config",
                    &config_key,
                ),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("api_formats")
        && request_method == http::Method::GET
        && request_path == "/api/admin/system/api-formats"
    {
        return Ok(Some(
            Json(build_admin_api_formats_payload()).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("email_templates_list")
        && request_method == http::Method::GET
        && is_admin_system_email_templates_root(request_path)
    {
        return Ok(Some(
            Json(state.build_admin_email_templates_payload().await?).into_response(),
        ));
    }

    if decision.route_kind.as_deref() == Some("email_template_get")
        && request_method == http::Method::GET
    {
        let Some(template_type) = admin_system_email_template_type_from_path(request_path) else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "模板类型不存在",
                None,
            )));
        };
        return Ok(Some(
            match state
                .build_admin_email_template_payload(&template_type)
                .await?
            {
                Ok(payload) => Json(payload).into_response(),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("email_template_set")
        && request_method == http::Method::PUT
    {
        let Some(template_type) = admin_system_email_template_type_from_path(request_path) else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "模板类型不存在",
                None,
            )));
        };
        let Some(request_body) = request_body else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求数据验证失败",
                None,
            )));
        };
        return Ok(Some(
            match state
                .apply_admin_email_template_update(&template_type, request_body)
                .await?
            {
                Ok(payload) => Json(payload).into_response(),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("email_template_preview")
        && request_method == http::Method::POST
    {
        let Some(template_type) = admin_system_email_template_preview_type_from_path(request_path)
        else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "模板类型不存在",
                None,
            )));
        };
        return Ok(Some(
            match state
                .preview_admin_email_template(&template_type, request_body)
                .await?
            {
                Ok(payload) => Json(payload).into_response(),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    if decision.route_kind.as_deref() == Some("email_template_reset")
        && request_method == http::Method::POST
    {
        let Some(template_type) = admin_system_email_template_reset_type_from_path(request_path)
        else {
            return Ok(Some(build_proxy_error_response(
                http::StatusCode::NOT_FOUND,
                "not_found",
                "模板类型不存在",
                None,
            )));
        };
        return Ok(Some(
            match state.reset_admin_email_template(&template_type).await? {
                Ok(payload) => Json(payload).into_response(),
                Err((status, payload)) => (status, Json(payload)).into_response(),
            },
        ));
    }

    Ok(None)
}

fn admin_system_purge_task_for_route_kind(
    route_kind: Option<&str>,
) -> Option<(
    crate::maintenance::AdminCleanupTaskKind,
    &'static str,
    &'static str,
    &'static str,
)> {
    match route_kind {
        Some("purge_config") => Some((
            crate::maintenance::AdminCleanupTaskKind::Config,
            "purge_system_config_async",
            "system_config",
            "global",
        )),
        Some("purge_users") => Some((
            crate::maintenance::AdminCleanupTaskKind::Users,
            "purge_non_admin_users_async",
            "users",
            "non_admin",
        )),
        Some("purge_usage") => Some((
            crate::maintenance::AdminCleanupTaskKind::Usage,
            "purge_usage_records_async",
            "usage",
            "all",
        )),
        Some("purge_audit_logs") => Some((
            crate::maintenance::AdminCleanupTaskKind::AuditLogs,
            "purge_audit_logs_async",
            "audit_logs",
            "all",
        )),
        Some("purge_request_bodies") | Some("purge_request_bodies_task") => Some((
            crate::maintenance::AdminCleanupTaskKind::RequestBodies,
            "purge_request_bodies_async",
            "request_bodies",
            "all",
        )),
        Some("purge_stats") => Some((
            crate::maintenance::AdminCleanupTaskKind::Stats,
            "purge_stats_async",
            "stats",
            "all",
        )),
        _ => None,
    }
}

async fn build_admin_system_cleanup_payload(
    state: &AdminAppState<'_>,
) -> Result<serde_json::Value, GatewayError> {
    let started_at_unix_secs = chrono::Utc::now().timestamp().max(0) as u64;
    let started_at = Instant::now();
    let summary = state.run_admin_system_cleanup_once().await?;
    let cleaned = json!({
        "audit_logs": summary.audit_logs_deleted,
        "request_candidates": summary.request_candidates_deleted,
        "proxy_node_metrics_1m": summary.proxy_node_metrics.deleted_1m_rows,
        "proxy_node_metrics_1h": summary.proxy_node_metrics.deleted_1h_rows,
        "pending_failed": summary.pending_failed,
        "pending_recovered": summary.pending_recovered,
        "usage_body_externalized": summary.usage.body_externalized,
        "usage_legacy_body_refs_migrated": summary.usage.legacy_body_refs_migrated,
        "usage_body_cleaned": summary.usage.body_cleaned,
        "usage_header_cleaned": summary.usage.header_cleaned,
        "usage_keys_cleaned": summary.usage.keys_cleaned,
        "usage_records_deleted": summary.usage.records_deleted,
    });
    let total = summary
        .audit_logs_deleted
        .saturating_add(summary.request_candidates_deleted)
        .saturating_add(summary.proxy_node_metrics.deleted_1m_rows)
        .saturating_add(summary.proxy_node_metrics.deleted_1h_rows)
        .saturating_add(summary.pending_failed)
        .saturating_add(summary.pending_recovered)
        .saturating_add(summary.usage.body_externalized)
        .saturating_add(summary.usage.legacy_body_refs_migrated)
        .saturating_add(summary.usage.body_cleaned)
        .saturating_add(summary.usage.header_cleaned)
        .saturating_add(summary.usage.keys_cleaned)
        .saturating_add(summary.usage.records_deleted);

    crate::maintenance::record_completed_cleanup_run(
        &state.app().data,
        "system_cleanup",
        "manual",
        started_at_unix_secs,
        started_at,
        cleaned.clone(),
        format!("系统清理已执行，影响 {total} 项"),
    )
    .await;

    Ok(json!({
        "message": format!("系统清理已执行，影响 {} 项", total),
        "cleaned": cleaned,
    }))
}

async fn build_manual_usage_cleanup_response(
    state: &AdminAppState<'_>,
    request_body: Option<&Bytes>,
) -> Result<Response<Body>, GatewayError> {
    let older_than_days = match parse_manual_usage_cleanup_request(request_body) {
        Ok(value) => value,
        Err(response) => return Ok(response),
    };

    match crate::maintenance::run_manual_usage_cleanup_once(
        &state.app().data,
        older_than_days,
        None,
    )
    .await
    {
        Ok(summary) => {
            let total = summary
                .body_externalized
                .saturating_add(summary.legacy_body_refs_migrated)
                .saturating_add(summary.body_cleaned)
                .saturating_add(summary.header_cleaned)
                .saturating_add(summary.keys_cleaned)
                .saturating_add(summary.records_deleted);
            let message = match older_than_days {
                Some(days) => {
                    format!("请求记录手动清理完成，清理 {days} 天前的记录，影响 {total} 项")
                }
                None => format!("请求记录手动清理完成（按当前策略），影响 {total} 项"),
            };
            let payload = json!({
                "message": message,
                "requested_older_than_days": older_than_days,
                "summary": {
                    "body_externalized": summary.body_externalized,
                    "legacy_body_refs_migrated": summary.legacy_body_refs_migrated,
                    "body_cleaned": summary.body_cleaned,
                    "header_cleaned": summary.header_cleaned,
                    "keys_cleaned": summary.keys_cleaned,
                    "records_deleted": summary.records_deleted,
                },
                "total_affected": total,
            });
            Ok(attach_admin_audit_response(
                Json(payload).into_response(),
                "admin_system_usage_cleanup_completed",
                "manual_usage_cleanup",
                "usage_cleanup",
                "global",
            ))
        }
        Err(crate::maintenance::ManualUsageCleanupError::AlreadyRunning) => Ok((
            http::StatusCode::CONFLICT,
            Json(json!({
                "detail": "usage_cleanup_already_running",
                "message": "已有一次清理正在进行中，请稍后再试",
            })),
        )
            .into_response()),
        Err(crate::maintenance::ManualUsageCleanupError::DataLayer(err)) => {
            Err(GatewayError::Internal(err.to_string()))
        }
    }
}

async fn build_manual_usage_cleanup_preview_response(
    state: &AdminAppState<'_>,
    request_context: &AdminRequestContext<'_>,
) -> Result<Response<Body>, GatewayError> {
    let older_than_days = match parse_older_than_days_query(request_context.query_string()) {
        Ok(value) => value,
        Err(response) => return Ok(response),
    };
    let preview =
        crate::maintenance::preview_manual_usage_cleanup(&state.app().data, older_than_days)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
    Ok(Json(json!({
        "requested_older_than_days": preview.requested_older_than_days,
        "effective_cutoffs": {
            "detail": preview.detail_cutoff,
            "compressed": preview.compressed_cutoff,
            "header": preview.header_cutoff,
            "log": preview.log_cutoff,
        },
        "counts": {
            "detail": preview.detail_count,
            "compressed": preview.compressed_count,
            "header": preview.header_count,
            "log": preview.log_count,
        },
    }))
    .into_response())
}

fn parse_manual_usage_cleanup_request(
    request_body: Option<&Bytes>,
) -> Result<Option<u32>, Response<Body>> {
    let Some(body) = request_body else {
        return Ok(None);
    };
    if body.is_empty() {
        return Ok(None);
    }
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => {
            return Err((
                http::StatusCode::BAD_REQUEST,
                Json(json!({ "detail": format!("请求体无效 JSON: {err}") })),
            )
                .into_response())
        }
    };
    let Some(object) = parsed.as_object() else {
        return Ok(None);
    };
    match object.get("older_than_days") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|days| *days >= 1)
            .map(Some)
            .ok_or_else(|| {
                (
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({
                        "detail": "older_than_days 必须为正整数",
                    })),
                )
                    .into_response()
            }),
    }
}

fn parse_older_than_days_query(query_string: Option<&str>) -> Result<Option<u32>, Response<Body>> {
    let Some(query) = query_string.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let value = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| {
            if key == "older_than_days" && !value.is_empty() {
                Some(value)
            } else {
                None
            }
        });
    let Some(raw) = value else {
        return Ok(None);
    };
    raw.parse::<u32>()
        .ok()
        .filter(|days| *days >= 1)
        .map(Some)
        .ok_or_else(|| {
            (
                http::StatusCode::BAD_REQUEST,
                Json(json!({
                    "detail": "older_than_days 必须为正整数",
                })),
            )
                .into_response()
        })
}
