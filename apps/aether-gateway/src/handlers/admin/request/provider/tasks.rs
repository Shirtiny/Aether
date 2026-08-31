use super::*;
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use axum::{
    body::Body,
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

impl<'a> AdminAppState<'a> {
    pub(crate) async fn clear_admin_provider_pool_cooldown(&self, provider_id: &str, key_id: &str) {
        crate::handlers::admin::provider::pool::runtime::clear_admin_provider_pool_cooldown(
            self,
            provider_id,
            key_id,
        )
        .await
    }

    pub(crate) async fn reset_admin_provider_pool_cost(&self, provider_id: &str, key_id: &str) {
        crate::handlers::admin::provider::pool::runtime::reset_admin_provider_pool_cost(
            self,
            provider_id,
            key_id,
        )
        .await
    }

    pub(crate) fn put_provider_delete_task(&self, task: crate::LocalProviderDeleteTaskState) {
        self.app.put_provider_delete_task(task)
    }

    pub(crate) async fn run_admin_provider_delete_task(
        &self,
        provider_id: &str,
        task_id: &str,
    ) -> Result<crate::LocalProviderDeleteTaskState, GatewayError> {
        crate::handlers::admin::provider::delete_task::run_admin_provider_delete_task(
            self,
            provider_id,
            task_id,
        )
        .await
    }

    pub(crate) fn get_provider_delete_task(
        &self,
        task_id: &str,
    ) -> Option<crate::LocalProviderDeleteTaskState> {
        self.app.get_provider_delete_task(task_id)
    }

    pub(crate) fn get_admin_pool_batch_delete_task_for_provider(
        &self,
        provider_id: &str,
        task_id: &str,
    ) -> Result<crate::LocalProviderDeleteTaskState, Response<Body>> {
        let Some(task) = self.get_provider_delete_task(task_id) else {
            return Err((
                http::StatusCode::NOT_FOUND,
                Json(json!({ "detail": "批量删除任务不存在" })),
            )
                .into_response());
        };
        if task.provider_id != provider_id {
            return Err((
                http::StatusCode::NOT_FOUND,
                Json(json!({ "detail": "批量删除任务不存在" })),
            )
                .into_response());
        }
        Ok(task)
    }

    pub(crate) async fn build_admin_pool_batch_import_response(
        &self,
        provider_id: &str,
        payload: aether_admin::provider::pool::AdminPoolBatchImportRequest,
    ) -> Result<Response<Body>, GatewayError> {
        use aether_admin::provider::pool as admin_provider_pool_pure;

        let Some(provider) = self
            .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id.to_string()))
            .await?
            .into_iter()
            .next()
        else {
            return Ok((
                http::StatusCode::NOT_FOUND,
                Json(json!({ "detail": format!("Provider {provider_id} 不存在") })),
            )
                .into_response());
        };

        let endpoints = self
            .list_provider_catalog_endpoints_by_provider_ids(std::slice::from_ref(&provider.id))
            .await?;
        let existing_keys = self
            .list_provider_catalog_keys_by_provider_ids(std::slice::from_ref(&provider.id))
            .await?;
        let api_formats =
            admin_provider_pool_pure::admin_pool_resolved_api_formats(&endpoints, &existing_keys);
        if api_formats.is_empty() {
            return Ok((
                http::StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "Provider 没有可用 endpoint 或现有 key，无法推断 api_formats" })),
            )
                .into_response());
        }

        let proxy =
            admin_provider_pool_pure::admin_pool_key_proxy_value(payload.proxy_node_id.as_deref());
        let mut imported = 0usize;
        let skipped = 0usize;
        let mut errors = Vec::new();
        let now_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
            .unwrap_or(0);

        for (index, item) in payload.keys.iter().enumerate() {
            let api_key = item.api_key.trim();
            if api_key.is_empty() {
                errors.push(json!({
                    "index": index,
                    "reason": "api_key is empty",
                }));
                continue;
            }

            let Some(encrypted_api_key) = self.encrypt_catalog_secret_with_fallbacks(api_key)
            else {
                errors.push(json!({
                    "index": index,
                    "reason": "gateway 未配置 provider key 加密密钥",
                }));
                continue;
            };

            let auth_type = item.auth_type.trim().to_ascii_lowercase();
            let auth_type = if auth_type.is_empty() {
                "api_key".to_string()
            } else {
                auth_type
            };
            let name = item.name.trim();
            let record = match admin_provider_pool_pure::build_admin_pool_batch_import_key_record(
                uuid::Uuid::new_v4().to_string(),
                provider.id.clone(),
                if name.is_empty() {
                    format!("imported-{index}")
                } else {
                    name.to_string()
                },
                auth_type,
                api_formats.clone(),
                encrypted_api_key,
                proxy.clone(),
                now_unix_secs,
            ) {
                Ok(value) => value,
                Err(err) => {
                    errors.push(json!({
                        "index": index,
                        "reason": err.to_string(),
                    }));
                    continue;
                }
            };

            let Some(_) = self.create_provider_catalog_key(&record).await? else {
                return Ok((
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        json!({ "detail": "Admin pool cleanup requires provider catalog writer" }),
                    ),
                )
                    .into_response());
            };
            imported += 1;
        }

        Ok(Json(
            admin_provider_pool_pure::build_admin_pool_batch_import_result_payload(
                imported, skipped, errors,
            ),
        )
        .into_response())
    }

    pub(crate) async fn build_admin_pool_cleanup_banned_keys_response(
        &self,
        provider_id: &str,
    ) -> Result<Response<Body>, GatewayError> {
        let Some(provider) = self
            .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id.to_string()))
            .await?
            .into_iter()
            .next()
        else {
            return Ok((
                http::StatusCode::NOT_FOUND,
                Json(json!({ "detail": format!("Provider {provider_id} 不存在") })),
            )
                .into_response());
        };

        let affected = self
            .cleanup_known_banned_provider_catalog_keys(&provider)
            .await?;
        if affected == 0 {
            return Ok(Json(
                aether_admin::provider::pool::build_admin_pool_cleanup_empty_payload(
                    "未发现可清理的异常账号",
                ),
            )
            .into_response());
        }

        Ok(
            Json(aether_admin::provider::pool::build_admin_pool_cleanup_result_payload(affected))
                .into_response(),
        )
    }

    pub(crate) async fn cleanup_known_banned_provider_catalog_keys(
        &self,
        provider: &StoredProviderCatalogProvider,
    ) -> Result<usize, GatewayError> {
        use aether_admin::provider::pool as admin_provider_pool_pure;

        let banned_keys = self
            .list_provider_catalog_keys_by_provider_ids(std::slice::from_ref(&provider.id))
            .await?
            .into_iter()
            .filter(admin_provider_pool_pure::admin_pool_key_is_known_banned)
            .collect::<Vec<_>>();
        if banned_keys.is_empty() {
            return Ok(0);
        }

        let deleted_key_ids = banned_keys
            .iter()
            .map(|key| key.id.clone())
            .collect::<Vec<_>>();
        for key in &banned_keys {
            self.clear_admin_provider_pool_cooldown(&provider.id, &key.id)
                .await;
            self.reset_admin_provider_pool_cost(&provider.id, &key.id)
                .await;
        }

        let mut affected = 0usize;
        for key_id in &deleted_key_ids {
            if self.delete_provider_catalog_key(key_id).await? {
                affected += 1;
            }
        }
        self.cleanup_deleted_provider_catalog_refs(&provider.id, &[], &deleted_key_ids)
            .await?;

        Ok(affected)
    }

    pub(crate) async fn cleanup_provider_catalog_key_if_current<F>(
        &self,
        provider: &StoredProviderCatalogProvider,
        key_id: &str,
        should_delete: F,
    ) -> Result<bool, GatewayError>
    where
        F: FnOnce(&StoredProviderCatalogKey) -> bool,
    {
        let key_ids = [key_id.to_string()];
        let Some(key) = self
            .read_provider_catalog_keys_by_ids(&key_ids)
            .await?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };
        if key.provider_id != provider.id || !should_delete(&key) {
            return Ok(false);
        }

        self.clear_admin_provider_pool_cooldown(&provider.id, &key.id)
            .await;
        self.reset_admin_provider_pool_cost(&provider.id, &key.id)
            .await;
        let deleted = self.delete_provider_catalog_key(&key.id).await?;
        if deleted {
            let deleted_key_ids = [key.id.clone()];
            self.cleanup_deleted_provider_catalog_refs(&provider.id, &[], &deleted_key_ids)
                .await?;
        }
        Ok(deleted)
    }

    pub(crate) async fn build_admin_pool_batch_action_response(
        &self,
        provider_id: &str,
        payload: aether_admin::provider::pool::AdminPoolBatchActionRequest,
    ) -> Result<Response<Body>, GatewayError> {
        use aether_admin::provider::pool::{
            self as admin_provider_pool_pure, AdminPoolBatchActionKind,
        };

        let Some(provider) = self
            .read_provider_catalog_providers_by_ids(std::slice::from_ref(&provider_id.to_string()))
            .await?
            .into_iter()
            .next()
        else {
            return Ok((
                http::StatusCode::NOT_FOUND,
                Json(json!({ "detail": format!("Provider {provider_id} 不存在") })),
            )
                .into_response());
        };

        let plan = match admin_provider_pool_pure::build_admin_pool_batch_action_plan(payload) {
            Ok(plan) => plan,
            Err(detail) => {
                return Ok((
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": detail })),
                )
                    .into_response());
            }
        };

        let keys = self
            .read_provider_catalog_keys_by_ids(&plan.key_ids)
            .await?
            .into_iter()
            .filter(|key| key.provider_id == provider.id)
            .collect::<Vec<_>>();

        if plan.action == AdminPoolBatchActionKind::RefreshCodexClientProfiles
            && keys.len() != plan.key_ids.len()
        {
            return Ok((
                http::StatusCode::CONFLICT,
                Json(json!({
                    "detail": "部分 Codex 账号已不存在或不属于当前 Provider，未执行批量更新"
                })),
            )
                .into_response());
        }

        if plan.action == AdminPoolBatchActionKind::Delete {
            let deleted_key_ids = keys.iter().map(|key| key.id.clone()).collect::<Vec<_>>();
            for key in &keys {
                self.clear_admin_provider_pool_cooldown(&provider.id, &key.id)
                    .await;
                self.reset_admin_provider_pool_cost(&provider.id, &key.id)
                    .await;
            }

            let mut affected = 0usize;
            for key_id in &deleted_key_ids {
                if self.delete_provider_catalog_key(key_id).await? {
                    affected = affected.saturating_add(1);
                }
            }
            self.cleanup_deleted_provider_catalog_refs(&provider.id, &[], &deleted_key_ids)
                .await?;

            return Ok(Json(
                admin_provider_pool_pure::build_admin_pool_batch_action_result_payload(
                    affected,
                    plan.action_label,
                ),
            )
            .into_response());
        }

        if plan.action == AdminPoolBatchActionKind::RefreshCodexClientProfiles {
            if !provider.provider_type.trim().eq_ignore_ascii_case("codex") {
                return Ok((
                    http::StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "detail": "Codex client profile refresh requires a Codex provider"
                    })),
                )
                    .into_response());
            }

            let Some(client_headers) = plan
                .action_payload
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .and_then(|payload| payload.get("codex_client_headers"))
                .cloned()
            else {
                return Ok((
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({
                        "detail": "refresh_codex_client_profiles requires codex_client_headers"
                    })),
                )
                    .into_response());
            };
            if let Err(detail) =
                crate::ai_serving::validate_codex_client_header_config(&client_headers)
            {
                return Ok((
                    http::StatusCode::BAD_REQUEST,
                    Json(json!({ "detail": detail })),
                )
                    .into_response());
            }

            let now_unix_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let mut provider_config = match provider.config.as_ref() {
                Some(serde_json::Value::Object(config)) => config.clone(),
                Some(_) => {
                    return Ok((
                        http::StatusCode::CONFLICT,
                        Json(json!({ "detail": "Provider config 必须是 JSON 对象" })),
                    )
                        .into_response());
                }
                None => serde_json::Map::new(),
            };
            let pool_advanced = provider_config
                .entry("pool_advanced".to_string())
                .or_insert_with(|| json!({}));
            let Some(pool_advanced) = pool_advanced.as_object_mut() else {
                return Ok((
                    http::StatusCode::CONFLICT,
                    Json(json!({ "detail": "Provider pool_advanced 必须是 JSON 对象" })),
                )
                    .into_response());
            };
            pool_advanced.insert("codex_client_headers".to_string(), client_headers.clone());
            let mut updated_provider = provider.clone();
            updated_provider.config = Some(serde_json::Value::Object(provider_config));
            updated_provider.updated_at_unix_secs = Some(now_unix_secs);

            let mut refreshed_keys = Vec::with_capacity(keys.len());
            for mut key in keys {
                let auth_config_raw = self
                    .parse_catalog_auth_config_json(&key)
                    .map(serde_json::Value::Object)
                    .map(|value| serde_json::to_string(&value))
                    .transpose()
                    .map_err(|err| GatewayError::Internal(err.to_string()))?;
                let previous_profile = key
                    .fingerprint
                    .as_ref()
                    .and_then(serde_json::Value::as_object)
                    .and_then(|root| root.get(crate::codex_profile::CODEX_CLIENT_PROFILE_KEY));
                let requires_account_identity =
                    crate::provider_key_auth::provider_key_is_oauth_managed(
                        &key,
                        &provider.provider_type,
                    ) || previous_profile
                        .and_then(|profile| profile.get("selection_key_kind"))
                        .and_then(serde_json::Value::as_str)
                        == Some("auth_account_id");
                if requires_account_identity
                    && crate::codex_profile::codex_auth_account_id(auth_config_raw.as_deref())
                        .is_none()
                {
                    return Ok((
                        http::StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({
                            "detail": format!(
                                "Key {} 缺少可解密的 Codex account identity，未执行批量更新",
                                key.id
                            )
                        })),
                    )
                        .into_response());
                }

                let Some(outcome) = crate::ai_serving::refresh_codex_pool_key_fingerprint(
                    provider.provider_type.as_str(),
                    updated_provider.config.as_ref(),
                    key.fingerprint.as_ref(),
                    auth_config_raw.as_deref(),
                    key.id.as_str(),
                    key.name.as_str(),
                    now_unix_secs,
                ) else {
                    return Ok((
                        http::StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({
                            "detail": "Codex 稳定客户端请求头已关闭或没有可用模板"
                        })),
                    )
                        .into_response());
                };

                let refreshed_profile = outcome
                    .fingerprint
                    .as_object()
                    .and_then(|root| root.get(crate::codex_profile::CODEX_CLIENT_PROFILE_KEY));
                let identity_changed = previous_profile.is_some_and(|previous| {
                    ["selection_key_kind", "selection_key_hash"]
                        .into_iter()
                        .any(|field| {
                            previous.get(field) != refreshed_profile.and_then(|p| p.get(field))
                        })
                        || previous
                            .get("install_identity")
                            .and_then(|value| value.get("installation_id"))
                            != refreshed_profile
                                .and_then(|profile| profile.get("install_identity"))
                                .and_then(|value| value.get("installation_id"))
                });
                let transport_changed = key
                    .fingerprint
                    .as_ref()
                    .and_then(|fingerprint| fingerprint.get("transport_profile"))
                    .is_some_and(|previous| {
                        outcome.fingerprint.get("transport_profile") != Some(previous)
                    });
                if identity_changed || transport_changed {
                    return Ok((
                        http::StatusCode::CONFLICT,
                        Json(json!({
                            "detail": format!(
                                "Key {} 的账号身份或传输配置会发生变化，已中止批量更新",
                                key.id
                            )
                        })),
                    )
                        .into_response());
                }

                key.fingerprint = Some(outcome.fingerprint);
                key.updated_at_unix_secs = Some(now_unix_secs);
                refreshed_keys.push(key);
            }

            let Some(affected) = self
                .update_provider_catalog_codex_client_headers_and_key_fingerprints(
                    &provider.id,
                    &client_headers,
                    now_unix_secs,
                    &refreshed_keys,
                )
                .await?
            else {
                return Ok((
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "detail": "Provider catalog writer unavailable" })),
                )
                    .into_response());
            };
            return Ok(Json(
                admin_provider_pool_pure::build_admin_pool_batch_action_result_payload(
                    affected,
                    plan.action_label,
                ),
            )
            .into_response());
        }

        if matches!(
            plan.action,
            AdminPoolBatchActionKind::EnableCodexWs | AdminPoolBatchActionKind::DisableCodexWs
        ) {
            if !provider.provider_type.trim().eq_ignore_ascii_case("codex") {
                return Ok((
                    http::StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "detail": "Codex WS batch actions require a Codex provider" })),
                )
                    .into_response());
            }
            let enabled = plan.action == AdminPoolBatchActionKind::EnableCodexWs;
            let profile = enabled.then(|| {
                json!({
                    "schema_version": aether_provider_transport::CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION,
                    "profile_id": aether_provider_transport::CODEX_OFFICIAL_WS_PROFILE_ID,
                    "codex_commit": aether_provider_transport::CODEX_OFFICIAL_WS_CODEX_COMMIT,
                    "tokio_tungstenite_rev": aether_provider_transport::CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
                    "tungstenite_rev": aether_provider_transport::CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
                    "tungstenite_patch_id": aether_provider_transport::CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID,
                    "write_buffer_size_bytes": aether_provider_transport::CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
                    "max_write_buffer_size_bytes": aether_provider_transport::CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES,
                    "max_retained_write_buffer_capacity_bytes": aether_provider_transport::CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
                    "crypto_provider": aether_provider_transport::CODEX_OFFICIAL_WS_CRYPTO_PROVIDER,
                })
            });
            let updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let mut affected = 0usize;
            for key in keys
                .iter()
                .filter(|key| key.auth_type.trim().eq_ignore_ascii_case("oauth"))
            {
                if self
                    .update_provider_catalog_key_codex_ws_metadata(
                        &key.id,
                        enabled,
                        profile.as_ref(),
                        updated_at,
                    )
                    .await?
                {
                    affected = affected.saturating_add(1);
                }
            }
            return Ok(Json(
                admin_provider_pool_pure::build_admin_pool_batch_action_result_payload(
                    affected,
                    plan.action_label,
                ),
            )
            .into_response());
        }

        let mut affected = 0usize;
        for mut key in keys {
            match plan.action {
                AdminPoolBatchActionKind::Enable => key.is_active = true,
                AdminPoolBatchActionKind::Disable => key.is_active = false,
                AdminPoolBatchActionKind::ClearProxy => key.proxy = None,
                AdminPoolBatchActionKind::SetProxy => key.proxy = plan.proxy_payload.clone(),
                AdminPoolBatchActionKind::RegenerateFingerprint => {
                    key.fingerprint =
                        Some(aether_provider_transport::claude_code::generate_random_fingerprint())
                }
                AdminPoolBatchActionKind::RefreshCodexClientProfiles => unreachable!(),
                AdminPoolBatchActionKind::EnableCodexWs
                | AdminPoolBatchActionKind::DisableCodexWs => unreachable!(),
                AdminPoolBatchActionKind::Delete => unreachable!(),
            }
            if self.update_provider_catalog_key(&key).await?.is_some() {
                affected = affected.saturating_add(1);
            }
        }

        Ok(Json(
            admin_provider_pool_pure::build_admin_pool_batch_action_result_payload(
                affected,
                plan.action_label,
            ),
        )
        .into_response())
    }
}
