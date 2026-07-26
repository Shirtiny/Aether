use super::{AppState, GatewayError, LocalMutationOutcome, LocalProviderDeleteTaskState};
use crate::admin_api::{reconcile_admin_fixed_provider_template_endpoints, AdminAppState};
use crate::handlers::shared::sync_provider_key_oauth_status_snapshot;
use aether_data_contracts::repository::{candidates, global_models, pool_scores, provider_catalog};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

impl AppState {
    /// Ensure fixed-provider endpoint templates are present after a binary
    /// upgrade. This is intentionally limited to endpoint reconciliation; the
    /// endpoint's `is_active` value remains the provider-level Search gate.
    pub async fn reconcile_fixed_provider_templates_on_startup(&self) -> Result<usize, String> {
        if !self.has_provider_catalog_data_reader() || !self.has_provider_catalog_data_writer() {
            return Ok(0);
        }

        let providers = self
            .list_provider_catalog_providers(false)
            .await
            .map_err(GatewayError::into_message)?;
        let admin_state = AdminAppState::new(self);
        let mut reconciled = 0usize;
        for provider in providers {
            if admin_state
                .fixed_provider_template(&provider.provider_type)
                .is_none()
            {
                continue;
            }
            reconcile_admin_fixed_provider_template_endpoints(&admin_state, &provider)
                .await
                .map_err(GatewayError::into_message)?;
            reconciled += 1;
        }
        if reconciled > 0 {
            self.invalidate_provider_runtime_routing_caches();
        }
        Ok(reconciled)
    }

    async fn finish_codex_ws_catalog_hot_after_authoritative_read<T>(
        &self,
        mutation: crate::codex_ws::hot_state::CodexWsHotMutation,
        authoritative_read: Result<T, aether_data::DataLayerError>,
    ) -> Result<(), GatewayError> {
        match authoritative_read {
            Ok(_) => crate::codex_ws::hot_state::finish_catalog_hot_mutation(self, mutation).await,
            Err(err) => {
                crate::codex_ws::hot_state::leave_hot_mutation_unstable(self, mutation).await;
                Err(GatewayError::Internal(err.to_string()))
            }
        }
    }

    async fn finish_codex_ws_key_hot_after_authoritative_read(
        &self,
        mutation: crate::codex_ws::hot_state::CodexWsHotMutation,
        key_id: &str,
    ) -> Result<(), GatewayError> {
        self.data.clear_provider_catalog_cache();
        match self
            .data
            .list_provider_catalog_keys_by_ids(&[key_id.to_string()])
            .await
        {
            Ok(keys) => {
                crate::codex_ws::hot_state::finish_key_hot_mutation(self, mutation, keys.first())
                    .await
            }
            Err(err) => {
                crate::codex_ws::hot_state::leave_hot_mutation_unstable(self, mutation).await;
                Err(GatewayError::Internal(err.to_string()))
            }
        }
    }

    async fn provider_is_codex_for_ws(&self, provider_id: &str) -> Result<bool, GatewayError> {
        Ok(self
            .data
            .list_provider_catalog_providers_by_ids(&[provider_id.to_string()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .first()
            .is_some_and(|provider| provider.provider_type.trim().eq_ignore_ascii_case("codex")))
    }

    async fn key_is_codex_ws_relevant(
        &self,
        key: &provider_catalog::StoredProviderCatalogKey,
        explicit_ws_mutation: bool,
    ) -> Result<bool, GatewayError> {
        if !codex_ws_key_shape_matches(key, explicit_ws_mutation) {
            return Ok(false);
        }
        self.provider_is_codex_for_ws(&key.provider_id).await
    }

    async fn endpoint_is_codex_ws_relevant(
        &self,
        endpoint: &provider_catalog::StoredProviderCatalogEndpoint,
    ) -> Result<bool, GatewayError> {
        if !endpoint
            .api_format
            .trim()
            .eq_ignore_ascii_case("openai:responses")
        {
            return Ok(false);
        }
        self.provider_is_codex_for_ws(&endpoint.provider_id).await
    }

    pub fn has_provider_catalog_data_reader(&self) -> bool {
        self.data.has_provider_catalog_reader()
    }

    pub(crate) fn has_provider_catalog_data_writer(&self) -> bool {
        self.data.has_provider_catalog_writer()
    }

    pub(crate) fn has_global_model_data_reader(&self) -> bool {
        self.data.has_global_model_reader()
    }

    pub(crate) fn has_global_model_data_writer(&self) -> bool {
        self.data.has_global_model_writer()
    }

    pub(crate) fn has_minimal_candidate_selection_reader(&self) -> bool {
        self.data.has_minimal_candidate_selection_reader()
    }

    pub(crate) fn has_management_token_reader(&self) -> bool {
        self.data.has_management_token_reader()
    }

    pub(crate) fn has_management_token_writer(&self) -> bool {
        self.data.has_management_token_writer()
    }

    pub(crate) async fn list_provider_catalog_providers(
        &self,
        active_only: bool,
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogProvider>, GatewayError> {
        self.data
            .list_provider_catalog_providers(active_only)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_endpoints_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogEndpoint>, GatewayError> {
        self.data
            .list_provider_catalog_endpoints_by_provider_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_public_global_models(
        &self,
        query: &global_models::PublicGlobalModelQuery,
    ) -> Result<global_models::StoredPublicGlobalModelPage, GatewayError> {
        self.data
            .list_public_global_models(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_management_tokens(
        &self,
        query: &aether_data::repository::management_tokens::ManagementTokenListQuery,
    ) -> Result<
        aether_data::repository::management_tokens::StoredManagementTokenListPage,
        GatewayError,
    > {
        self.data
            .list_management_tokens(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn get_management_token_with_user(
        &self,
        token_id: &str,
    ) -> Result<
        Option<aether_data::repository::management_tokens::StoredManagementTokenWithUser>,
        GatewayError,
    > {
        self.data
            .get_management_token_with_user(token_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn get_management_token_with_user_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<
        Option<aether_data::repository::management_tokens::StoredManagementTokenWithUser>,
        GatewayError,
    > {
        self.data
            .get_management_token_with_user_by_hash(token_hash)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn create_management_token(
        &self,
        record: &aether_data::repository::management_tokens::CreateManagementTokenRecord,
    ) -> Result<
        LocalMutationOutcome<aether_data::repository::management_tokens::StoredManagementToken>,
        GatewayError,
    > {
        self.data
            .create_management_token(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn update_management_token(
        &self,
        record: &aether_data::repository::management_tokens::UpdateManagementTokenRecord,
    ) -> Result<
        LocalMutationOutcome<aether_data::repository::management_tokens::StoredManagementToken>,
        GatewayError,
    > {
        self.data
            .update_management_token(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn delete_management_token(
        &self,
        token_id: &str,
    ) -> Result<bool, GatewayError> {
        self.data
            .delete_management_token(token_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn record_management_token_usage(
        &self,
        token_id: &str,
        last_used_ip: Option<&str>,
    ) -> Result<
        Option<aether_data::repository::management_tokens::StoredManagementToken>,
        GatewayError,
    > {
        self.data
            .record_management_token_usage(token_id, last_used_ip)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn set_management_token_active(
        &self,
        token_id: &str,
        is_active: bool,
    ) -> Result<
        Option<aether_data::repository::management_tokens::StoredManagementToken>,
        GatewayError,
    > {
        self.data
            .set_management_token_active(token_id, is_active)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn regenerate_management_token_secret(
        &self,
        mutation: &aether_data::repository::management_tokens::RegenerateManagementTokenSecret,
    ) -> Result<
        LocalMutationOutcome<aether_data::repository::management_tokens::StoredManagementToken>,
        GatewayError,
    > {
        self.data
            .regenerate_management_token_secret(mutation)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn get_public_global_model_by_name(
        &self,
        model_name: &str,
    ) -> Result<Option<global_models::StoredPublicGlobalModel>, GatewayError> {
        self.data
            .get_public_global_model_by_name(model_name)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_public_catalog_models(
        &self,
        query: &global_models::PublicCatalogModelListQuery,
    ) -> Result<Vec<global_models::StoredPublicCatalogModel>, GatewayError> {
        self.data
            .list_public_catalog_models(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn search_public_catalog_models(
        &self,
        query: &global_models::PublicCatalogModelSearchQuery,
    ) -> Result<Vec<global_models::StoredPublicCatalogModel>, GatewayError> {
        self.data
            .search_public_catalog_models(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_admin_provider_models(
        &self,
        query: &global_models::AdminProviderModelListQuery,
    ) -> Result<Vec<global_models::StoredAdminProviderModel>, GatewayError> {
        self.data
            .list_admin_provider_models(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_admin_global_models(
        &self,
        query: &global_models::AdminGlobalModelListQuery,
    ) -> Result<global_models::StoredAdminGlobalModelPage, GatewayError> {
        self.data
            .list_admin_global_models(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn get_admin_provider_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<Option<global_models::StoredAdminProviderModel>, GatewayError> {
        self.data
            .get_admin_provider_model(provider_id, model_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_admin_provider_available_source_models(
        &self,
        provider_id: &str,
    ) -> Result<Vec<global_models::StoredAdminProviderModel>, GatewayError> {
        self.data
            .list_admin_provider_available_source_models(provider_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn get_admin_global_model_by_id(
        &self,
        global_model_id: &str,
    ) -> Result<Option<global_models::StoredAdminGlobalModel>, GatewayError> {
        self.data
            .get_admin_global_model_by_id(global_model_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn get_admin_global_model_by_name(
        &self,
        model_name: &str,
    ) -> Result<Option<global_models::StoredAdminGlobalModel>, GatewayError> {
        self.data
            .get_admin_global_model_by_name(model_name)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_admin_provider_models_by_global_model_id(
        &self,
        global_model_id: &str,
    ) -> Result<Vec<global_models::StoredAdminProviderModel>, GatewayError> {
        self.data
            .list_admin_provider_models_by_global_model_id(global_model_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn create_admin_provider_model(
        &self,
        record: &global_models::UpsertAdminProviderModelRecord,
    ) -> Result<Option<global_models::StoredAdminProviderModel>, GatewayError> {
        let created = self
            .data
            .create_admin_provider_model(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn update_admin_provider_model(
        &self,
        record: &global_models::UpsertAdminProviderModelRecord,
    ) -> Result<Option<global_models::StoredAdminProviderModel>, GatewayError> {
        let updated = self
            .data
            .update_admin_provider_model(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_admin_provider_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<bool, GatewayError> {
        let deleted = self
            .data
            .delete_admin_provider_model(provider_id, model_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn create_admin_global_model(
        &self,
        record: &global_models::CreateAdminGlobalModelRecord,
    ) -> Result<Option<global_models::StoredAdminGlobalModel>, GatewayError> {
        let created = self
            .data
            .create_admin_global_model(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn update_admin_global_model(
        &self,
        record: &global_models::UpdateAdminGlobalModelRecord,
    ) -> Result<Option<global_models::StoredAdminGlobalModel>, GatewayError> {
        let updated = self
            .data
            .update_admin_global_model(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_admin_global_model(
        &self,
        global_model_id: &str,
    ) -> Result<bool, GatewayError> {
        let deleted = self
            .data
            .delete_admin_global_model(global_model_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn list_provider_model_stats(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<global_models::StoredProviderModelStats>, GatewayError> {
        self.data
            .list_provider_model_stats(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_active_global_model_ids_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<global_models::StoredProviderActiveGlobalModel>, GatewayError> {
        self.data
            .list_active_global_model_ids_by_provider_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_finalized_request_candidates_by_endpoint_ids_since(
        &self,
        endpoint_ids: &[String],
        since_unix_secs: u64,
        limit: usize,
    ) -> Result<Vec<candidates::StoredRequestCandidate>, GatewayError> {
        self.data
            .list_finalized_request_candidates_by_endpoint_ids_since(
                endpoint_ids,
                since_unix_secs,
                limit,
            )
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn count_finalized_request_candidate_statuses_by_endpoint_ids_since(
        &self,
        endpoint_ids: &[String],
        since_unix_secs: u64,
    ) -> Result<Vec<candidates::PublicHealthStatusCount>, GatewayError> {
        self.data
            .count_finalized_request_candidate_statuses_by_endpoint_ids_since(
                endpoint_ids,
                since_unix_secs,
            )
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn aggregate_finalized_request_candidate_timeline_by_endpoint_ids_since(
        &self,
        endpoint_ids: &[String],
        since_unix_secs: u64,
        until_unix_secs: u64,
        segments: u32,
    ) -> Result<Vec<candidates::PublicHealthTimelineBucket>, GatewayError> {
        self.data
            .aggregate_finalized_request_candidate_timeline_by_endpoint_ids_since(
                endpoint_ids,
                since_unix_secs,
                until_unix_secs,
                segments,
            )
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_keys_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        self.data
            .list_provider_catalog_keys_by_provider_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_key_summaries_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        self.data
            .list_provider_catalog_key_summaries_by_provider_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_key_maintenance_summaries_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogKeyMaintenanceSummary>, GatewayError>
    {
        self.data
            .list_provider_catalog_key_maintenance_summaries_by_provider_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_keys_by_ids(
        &self,
        key_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        self.data
            .list_provider_catalog_keys_by_ids(key_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_key_page(
        &self,
        query: &provider_catalog::ProviderCatalogKeyListQuery,
    ) -> Result<provider_catalog::StoredProviderCatalogKeyPage, GatewayError> {
        self.data
            .list_provider_catalog_key_page(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_provider_catalog_key_stats_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogKeyStats>, GatewayError> {
        self.data
            .list_provider_catalog_key_stats_by_provider_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn create_provider_catalog_key(
        &self,
        key: &provider_catalog::StoredProviderCatalogKey,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        let hot_mutation = self
            .key_is_codex_ws_relevant(key, false)
            .await?
            .then(|| crate::codex_ws::hot_state::begin_key_hot_mutation(self, &key.id));
        let hot_mutation = match hot_mutation {
            Some(mutation) => Some(mutation.await?),
            None => None,
        };
        let created = self.data.create_provider_catalog_key(key).await;
        if let Some(mutation) = hot_mutation {
            self.finish_codex_ws_key_hot_after_authoritative_read(mutation, &key.id)
                .await?;
        }
        let created = created.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn create_provider_catalog_provider(
        &self,
        provider: &provider_catalog::StoredProviderCatalogProvider,
        shift_existing_priorities_from: Option<i32>,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogProvider>, GatewayError> {
        let hot_mutation = if provider.provider_type.trim().eq_ignore_ascii_case("codex") {
            Some(crate::codex_ws::hot_state::begin_catalog_hot_mutation(self).await?)
        } else {
            None
        };
        let created = self
            .data
            .create_provider_catalog_provider(provider, shift_existing_priorities_from)
            .await;
        if let Some(mutation) = hot_mutation {
            self.data.clear_provider_catalog_cache();
            let authoritative = self
                .data
                .list_provider_catalog_providers_by_ids(&[provider.id.clone()])
                .await;
            self.finish_codex_ws_catalog_hot_after_authoritative_read(mutation, authoritative)
                .await?;
        }
        let created = created.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn update_provider_catalog_provider(
        &self,
        provider: &provider_catalog::StoredProviderCatalogProvider,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogProvider>, GatewayError> {
        let existing_relevant = self.provider_is_codex_for_ws(&provider.id).await?;
        let hot_mutation =
            if existing_relevant || provider.provider_type.trim().eq_ignore_ascii_case("codex") {
                Some(crate::codex_ws::hot_state::begin_catalog_hot_mutation(self).await?)
            } else {
                None
            };
        let updated = self.data.update_provider_catalog_provider(provider).await;
        if let Some(mutation) = hot_mutation {
            self.data.clear_provider_catalog_cache();
            let authoritative = self
                .data
                .list_provider_catalog_providers_by_ids(&[provider.id.clone()])
                .await;
            self.finish_codex_ws_catalog_hot_after_authoritative_read(mutation, authoritative)
                .await?;
        }
        let updated = updated.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_provider_catalog_provider(
        &self,
        provider_id: &str,
    ) -> Result<bool, GatewayError> {
        let hot_mutation = if self.provider_is_codex_for_ws(provider_id).await? {
            Some(crate::codex_ws::hot_state::begin_catalog_hot_mutation(self).await?)
        } else {
            None
        };
        let deleted = self
            .data
            .delete_provider_catalog_provider(provider_id)
            .await;
        if let Some(mutation) = hot_mutation {
            self.data.clear_provider_catalog_cache();
            let authoritative = self
                .data
                .list_provider_catalog_providers_by_ids(&[provider_id.to_string()])
                .await;
            self.finish_codex_ws_catalog_hot_after_authoritative_read(mutation, authoritative)
                .await?;
        }
        let deleted = deleted.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn cleanup_deleted_provider_catalog_refs(
        &self,
        provider_id: &str,
        endpoint_ids: &[String],
        key_ids: &[String],
    ) -> Result<(), GatewayError> {
        self.data
            .cleanup_deleted_provider_catalog_refs(provider_id, endpoint_ids, key_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        for key_id in key_ids {
            if let Err(err) = self
                .data
                .delete_pool_member_scores_for_member(
                    &pool_scores::PoolMemberIdentity::provider_api_key(
                        provider_id.to_string(),
                        key_id.to_string(),
                    ),
                )
                .await
            {
                warn!(
                    provider_id,
                    key_id,
                    error = ?err,
                    "gateway provider catalog cleanup: failed to delete pool member scores"
                );
            }
        }
        if !endpoint_ids.is_empty() || !key_ids.is_empty() {
            self.invalidate_provider_routing_caches();
        }
        Ok(())
    }

    pub(crate) async fn create_provider_catalog_endpoint(
        &self,
        endpoint: &provider_catalog::StoredProviderCatalogEndpoint,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogEndpoint>, GatewayError> {
        let hot_mutation = if self.endpoint_is_codex_ws_relevant(endpoint).await? {
            Some(crate::codex_ws::hot_state::begin_catalog_hot_mutation(self).await?)
        } else {
            None
        };
        let created = self.data.create_provider_catalog_endpoint(endpoint).await;
        if let Some(mutation) = hot_mutation {
            self.data.clear_provider_catalog_cache();
            let authoritative = self
                .data
                .list_provider_catalog_endpoints_by_ids(&[endpoint.id.clone()])
                .await;
            self.finish_codex_ws_catalog_hot_after_authoritative_read(mutation, authoritative)
                .await?;
        }
        let created = created.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn update_provider_catalog_endpoint(
        &self,
        endpoint: &provider_catalog::StoredProviderCatalogEndpoint,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogEndpoint>, GatewayError> {
        let existing = self
            .data
            .list_provider_catalog_endpoints_by_ids(&[endpoint.id.clone()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .into_iter()
            .next();
        let relevant = self.endpoint_is_codex_ws_relevant(endpoint).await?
            || match existing.as_ref() {
                Some(existing) => self.endpoint_is_codex_ws_relevant(existing).await?,
                None => false,
            };
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_catalog_hot_mutation(self).await?)
        } else {
            None
        };
        let updated = self.data.update_provider_catalog_endpoint(endpoint).await;
        if let Some(mutation) = hot_mutation {
            self.data.clear_provider_catalog_cache();
            let authoritative = self
                .data
                .list_provider_catalog_endpoints_by_ids(&[endpoint.id.clone()])
                .await;
            self.finish_codex_ws_catalog_hot_after_authoritative_read(mutation, authoritative)
                .await?;
        }
        let updated = updated.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_provider_catalog_endpoint(
        &self,
        endpoint_id: &str,
    ) -> Result<bool, GatewayError> {
        let existing = self
            .data
            .list_provider_catalog_endpoints_by_ids(&[endpoint_id.to_string()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .into_iter()
            .next();
        let relevant = match existing.as_ref() {
            Some(existing) => self.endpoint_is_codex_ws_relevant(existing).await?,
            None => false,
        };
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_catalog_hot_mutation(self).await?)
        } else {
            None
        };
        let deleted = self
            .data
            .delete_provider_catalog_endpoint(endpoint_id)
            .await;
        if let Some(mutation) = hot_mutation {
            self.data.clear_provider_catalog_cache();
            let authoritative = self
                .data
                .list_provider_catalog_endpoints_by_ids(&[endpoint_id.to_string()])
                .await;
            self.finish_codex_ws_catalog_hot_after_authoritative_read(mutation, authoritative)
                .await?;
        }
        let deleted = deleted.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn update_provider_catalog_key(
        &self,
        key: &provider_catalog::StoredProviderCatalogKey,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        let existing = self
            .data
            .list_provider_catalog_keys_by_ids(&[key.id.clone()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .into_iter()
            .next();
        let relevant = self.key_is_codex_ws_relevant(key, false).await?
            || match existing.as_ref() {
                Some(existing) => self.key_is_codex_ws_relevant(existing, false).await?,
                None => false,
            };
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_key_hot_mutation(self, &key.id).await?)
        } else {
            None
        };
        let updated = self.data.update_provider_catalog_key(key).await;
        if let Some(mutation) = hot_mutation {
            self.finish_codex_ws_key_hot_after_authoritative_read(mutation, &key.id)
                .await?;
        }
        let updated = updated.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn update_provider_catalog_key_codex_ws_metadata(
        &self,
        key_id: &str,
        enabled: bool,
        websocket_transport_profile: Option<&serde_json::Value>,
        updated_at_unix_secs: u64,
    ) -> Result<bool, GatewayError> {
        let existing = self
            .data
            .list_provider_catalog_keys_by_ids(&[key_id.to_string()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .into_iter()
            .next();
        let relevant = match existing.as_ref() {
            Some(existing) => self.key_is_codex_ws_relevant(existing, true).await?,
            None => false,
        };
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_key_hot_mutation(self, key_id).await?)
        } else {
            None
        };
        let updated = self
            .data
            .update_provider_catalog_key_codex_ws_metadata(
                key_id,
                enabled,
                websocket_transport_profile,
                updated_at_unix_secs,
            )
            .await;
        if let Some(mutation) = hot_mutation {
            self.finish_codex_ws_key_hot_after_authoritative_read(mutation, key_id)
                .await?;
        }
        let updated = updated.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn update_provider_catalog_key_runtime_state(
        &self,
        key: &provider_catalog::StoredProviderCatalogKey,
    ) -> Result<Option<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        // Runtime telemetry, refreshed OAuth material, and health/quota state
        // may change very frequently. Refresh selection/transport snapshots,
        // but preserve the global scheduler-affinity epoch so unrelated
        // long-lived bindings are not treated as stale catalog snapshots.
        let restrictive = !crate::codex_ws::hot_state::key_runtime_eligibility(key).0;
        let relevant = restrictive && self.key_is_codex_ws_relevant(key, true).await?;
        // Restrictive runtime writes are rare and safety-sensitive. Use the
        // same distributed transition lock as admin mutations so a failed
        // one-shot CAS can never leave an eligible generation visible after
        // the authoritative DB commit.
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_key_hot_mutation(self, &key.id).await?)
        } else {
            None
        };
        let updated = self.data.update_provider_catalog_key(key).await;
        if let Some(mutation) = hot_mutation {
            self.finish_codex_ws_key_hot_after_authoritative_read(mutation, &key.id)
                .await?;
        }
        let updated = updated.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_runtime_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn update_provider_catalog_key_upstream_metadata(
        &self,
        key_id: &str,
        upstream_metadata: Option<&serde_json::Value>,
        updated_at_unix_secs: Option<u64>,
    ) -> Result<bool, GatewayError> {
        let updated = self
            .data
            .update_provider_catalog_key_upstream_metadata(
                key_id,
                upstream_metadata,
                updated_at_unix_secs,
            )
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated {
            // Upstream metadata is refreshed by runtime discovery/quota paths.
            // It can affect candidate facts and transport materialization, but
            // it does not change the identity of an existing affinity target.
            self.invalidate_provider_runtime_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_provider_catalog_key(
        &self,
        key_id: &str,
    ) -> Result<bool, GatewayError> {
        let existing_key = self
            .data
            .list_provider_catalog_keys_by_ids(&[key_id.to_string()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .into_iter()
            .next();
        let relevant = match existing_key.as_ref() {
            Some(key) => self.key_is_codex_ws_relevant(key, false).await?,
            None => false,
        };
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_key_hot_mutation(self, key_id).await?)
        } else {
            None
        };
        let deleted = self.data.delete_provider_catalog_key(key_id).await;
        if let Some(mutation) = hot_mutation {
            self.finish_codex_ws_key_hot_after_authoritative_read(mutation, key_id)
                .await?;
        }
        let deleted = deleted.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            if let Some(key) = existing_key {
                if let Err(err) = self
                    .data
                    .delete_pool_member_scores_for_member(
                        &pool_scores::PoolMemberIdentity::provider_api_key(
                            key.provider_id.clone(),
                            key.id.clone(),
                        ),
                    )
                    .await
                {
                    warn!(
                        provider_id = %key.provider_id,
                        key_id = %key.id,
                        error = ?err,
                        "gateway provider catalog key delete: failed to delete pool member scores"
                    );
                }
            }
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn clear_provider_catalog_key_oauth_invalid_marker(
        &self,
        key_id: &str,
    ) -> Result<bool, GatewayError> {
        let Some(mut key) = self
            .data
            .list_provider_catalog_keys_by_ids(&[key_id.to_string()])
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };

        key.oauth_invalid_at_unix_secs = None;
        key.oauth_invalid_reason = None;
        key.status_snapshot =
            sync_provider_key_oauth_status_snapshot(key.status_snapshot.as_ref(), &key);
        key.updated_at_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs());

        self.update_provider_catalog_key_runtime_state(&key)
            .await
            .map(|updated| updated.is_some())
    }

    pub(crate) fn put_provider_delete_task(&self, task: LocalProviderDeleteTaskState) {
        let mut tasks = self
            .provider_delete_tasks
            .lock()
            .expect("provider delete tasks cache should lock");
        tasks.insert(task.task_id.clone(), task);
    }

    pub(crate) fn get_provider_delete_task(
        &self,
        task_id: &str,
    ) -> Option<LocalProviderDeleteTaskState> {
        let tasks = self
            .provider_delete_tasks
            .lock()
            .expect("provider delete tasks cache should lock");
        tasks.get(task_id).cloned()
    }

    pub(crate) async fn read_provider_catalog_providers_by_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogProvider>, GatewayError> {
        self.data
            .list_provider_catalog_providers_by_ids(provider_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn read_provider_catalog_endpoints_by_ids(
        &self,
        endpoint_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogEndpoint>, GatewayError> {
        self.data
            .list_provider_catalog_endpoints_by_ids(endpoint_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn read_provider_catalog_keys_by_ids(
        &self,
        key_ids: &[String],
    ) -> Result<Vec<provider_catalog::StoredProviderCatalogKey>, GatewayError> {
        self.data
            .list_provider_catalog_keys_by_ids(key_ids)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn update_provider_catalog_key_format_health(
        &self,
        key_id: &str,
        api_format: &str,
        health_by_format: &serde_json::Value,
    ) -> Result<bool, GatewayError> {
        let api_format = api_format.trim();
        if api_format.is_empty() {
            return Ok(false);
        }

        let Some(current_key) = self
            .read_provider_catalog_keys_by_ids(&[key_id.to_string()])
            .await?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };

        if current_key.health_by_format.as_ref() == Some(health_by_format) {
            return Ok(false);
        }

        self.update_provider_catalog_key_health_state(
            key_id,
            current_key.is_active,
            Some(health_by_format),
            current_key.circuit_breaker_by_format.as_ref(),
        )
        .await
    }

    pub(crate) async fn update_provider_catalog_key_health_state(
        &self,
        key_id: &str,
        is_active: bool,
        health_by_format: Option<&serde_json::Value>,
        circuit_breaker_by_format: Option<&serde_json::Value>,
    ) -> Result<bool, GatewayError> {
        let relevant = if is_active {
            false
        } else {
            let keys = self
                .data
                .list_provider_catalog_keys_by_ids(&[key_id.to_string()])
                .await
                .map_err(|err| GatewayError::Internal(err.to_string()))?;
            match keys.first() {
                Some(key) => self.key_is_codex_ws_relevant(key, false).await?,
                None => false,
            }
        };
        let hot_mutation = if relevant {
            Some(crate::codex_ws::hot_state::begin_key_hot_mutation(self, key_id).await?)
        } else {
            None
        };
        let updated = self
            .data
            .update_provider_catalog_key_health_state(
                key_id,
                is_active,
                health_by_format,
                circuit_breaker_by_format,
            )
            .await;
        if let Some(mutation) = hot_mutation {
            self.finish_codex_ws_key_hot_after_authoritative_read(mutation, key_id)
                .await?;
        }
        let updated = updated.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated {
            self.invalidate_provider_runtime_routing_caches();
        }
        Ok(updated)
    }
}

fn codex_ws_key_shape_matches(
    key: &provider_catalog::StoredProviderCatalogKey,
    explicit_ws_mutation: bool,
) -> bool {
    let oauth_for_responses = key.auth_type.trim().eq_ignore_ascii_case("oauth")
        || key
            .auth_type_by_format
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .and_then(|formats| formats.get("openai:responses"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|auth_type| auth_type.trim().eq_ignore_ascii_case("oauth"));
    if !oauth_for_responses {
        return false;
    }
    let supports_responses = key.api_formats.as_ref().is_none_or(|formats| {
        formats.as_array().is_some_and(|formats| {
            formats.iter().any(|format| {
                format
                    .as_str()
                    .is_some_and(|format| format.trim().eq_ignore_ascii_case("openai:responses"))
            })
        })
    });
    if !supports_responses {
        return false;
    }
    explicit_ws_mutation
        || key
            .capabilities
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .is_some_and(|capabilities| {
                capabilities.contains_key(aether_provider_transport::CODEX_OFFICIAL_WS_CAPABILITY)
            })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    use aether_data::repository::{
        global_models::InMemoryGlobalModelReadRepository,
        provider_catalog::InMemoryProviderCatalogReadRepository,
    };
    use aether_data::DataLayerError;
    use aether_data_contracts::repository::candidate_selection::{
        MinimalCandidateSelectionReadRepository, StoredMinimalCandidateSelectionRow,
        StoredPoolKeyCandidateRowsByKeyIdsQuery, StoredPoolKeyCandidateRowsQuery,
        StoredRequestedModelCandidateRowsQuery,
    };
    use aether_data_contracts::repository::global_models::{
        CreateAdminGlobalModelRecord, StoredAdminGlobalModel, UpdateAdminGlobalModelRecord,
        UpsertAdminProviderModelRecord,
    };
    use aether_data_contracts::repository::provider_catalog::{
        StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
    };
    use async_trait::async_trait;
    use serde_json::json;

    use crate::cache::SchedulerAffinityTarget;
    use crate::data::GatewayDataState;
    use crate::AppState;

    fn sample_provider() -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            Some("https://example.com".to_string()),
            "openai".to_string(),
        )
        .expect("provider should build")
    }

    fn sample_endpoint() -> StoredProviderCatalogEndpoint {
        StoredProviderCatalogEndpoint::new(
            "endpoint-1".to_string(),
            "provider-1".to_string(),
            "openai:chat".to_string(),
            Some("openai".to_string()),
            Some("chat".to_string()),
            true,
        )
        .expect("endpoint should build")
        .with_transport_fields(
            "https://api.example.com/v1".to_string(),
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

    fn sample_key() -> StoredProviderCatalogKey {
        StoredProviderCatalogKey::new(
            "key-1".to_string(),
            "provider-1".to_string(),
            "Key 1".to_string(),
            "api_key".to_string(),
            None,
            true,
        )
        .expect("key should build")
    }

    fn sample_codex_provider() -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            "provider-1".to_string(),
            "Codex".to_string(),
            None,
            "codex".to_string(),
        )
        .expect("Codex provider should build")
    }

    fn sample_codex_endpoint() -> StoredProviderCatalogEndpoint {
        StoredProviderCatalogEndpoint::new(
            "endpoint-1".to_string(),
            "provider-1".to_string(),
            "openai:responses".to_string(),
            Some("openai".to_string()),
            Some("responses".to_string()),
            true,
        )
        .expect("Codex endpoint should build")
    }

    fn sample_codex_ws_key() -> StoredProviderCatalogKey {
        let mut key = StoredProviderCatalogKey::new(
            "key-1".to_string(),
            "provider-1".to_string(),
            "Codex OAuth".to_string(),
            "oauth".to_string(),
            Some(json!({
                (aether_provider_transport::CODEX_OFFICIAL_WS_CAPABILITY): true
            })),
            true,
        )
        .expect("Codex key should build");
        key.api_formats = Some(json!(["openai:responses"]));
        key.fingerprint = Some(json!({
            (aether_provider_transport::CODEX_OFFICIAL_WS_PROFILE_FINGERPRINT_KEY): {
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
            }
        }));
        key
    }

    fn sample_admin_global_model() -> StoredAdminGlobalModel {
        StoredAdminGlobalModel::new(
            "global-1".to_string(),
            "gpt-5".to_string(),
            "GPT 5".to_string(),
            true,
            None,
            None,
            None,
            None,
            0,
            0,
            0,
            Some(1_711_000_000),
            Some(1_711_000_000),
        )
        .expect("global model should build")
    }

    #[tokio::test]
    async fn startup_reconcile_adds_missing_codex_search_endpoint() {
        let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![sample_codex_provider()],
            vec![sample_codex_endpoint()],
            vec![sample_codex_ws_key()],
        ));
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_provider_catalog_repository_for_tests(repository),
            );

        let reconciled = state
            .reconcile_fixed_provider_templates_on_startup()
            .await
            .expect("fixed provider templates should reconcile");
        assert_eq!(reconciled, 1);

        let mut formats = state
            .list_provider_catalog_endpoints_by_provider_ids(&["provider-1".to_string()])
            .await
            .expect("endpoints should list")
            .into_iter()
            .map(|endpoint| endpoint.api_format)
            .collect::<Vec<_>>();
        formats.sort();
        assert_eq!(
            formats,
            vec![
                "openai:image".to_string(),
                "openai:responses".to_string(),
                "openai:responses:compact".to_string(),
                "openai:search".to_string(),
            ]
        );
    }

    fn sample_provider_model_record(
        id: &str,
        global_model_id: &str,
        is_active: bool,
    ) -> UpsertAdminProviderModelRecord {
        UpsertAdminProviderModelRecord::new(
            id.to_string(),
            "provider-1".to_string(),
            global_model_id.to_string(),
            "gpt-5-upstream".to_string(),
            None,
            None,
            None,
            None,
            None,
            Some(true),
            None,
            None,
            is_active,
            true,
            None,
        )
        .expect("provider model record should build")
    }

    #[derive(Debug, Default)]
    struct ClearCountingCandidateSelectionReadRepository {
        clear_count: AtomicUsize,
    }

    impl ClearCountingCandidateSelectionReadRepository {
        fn clear_count(&self) -> usize {
            self.clear_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MinimalCandidateSelectionReadRepository for ClearCountingCandidateSelectionReadRepository {
        fn clear_local_cache(&self) {
            self.clear_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn list_for_exact_api_format(
            &self,
            _api_format: &str,
        ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
            Ok(Vec::new())
        }

        async fn list_for_exact_api_format_and_global_model(
            &self,
            _api_format: &str,
            _global_model_name: &str,
        ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
            Ok(Vec::new())
        }

        async fn list_for_exact_api_format_and_requested_model(
            &self,
            _api_format: &str,
            _requested_model_name: &str,
        ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
            Ok(Vec::new())
        }

        async fn list_for_exact_api_format_and_requested_model_page(
            &self,
            _query: &StoredRequestedModelCandidateRowsQuery,
        ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
            Ok(Vec::new())
        }

        async fn list_pool_key_rows_for_group(
            &self,
            _query: &StoredPoolKeyCandidateRowsQuery,
        ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
            Ok(Vec::new())
        }

        async fn list_pool_key_rows_for_group_key_ids(
            &self,
            _query: &StoredPoolKeyCandidateRowsByKeyIdsQuery,
        ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn admin_model_writes_invalidate_candidate_selection_cache() {
        let candidate_repository =
            Arc::new(ClearCountingCandidateSelectionReadRepository::default());
        let global_model_repository = Arc::new(
            InMemoryGlobalModelReadRepository::seed(Vec::new())
                .with_admin_global_models(vec![sample_admin_global_model()]),
        );
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_minimal_candidate_selection_reader_for_tests(
                    candidate_repository.clone(),
                )
                .with_global_model_repository_for_tests(global_model_repository),
            );

        assert_eq!(candidate_repository.clear_count(), 0);

        let provider_model = sample_provider_model_record("model-1", "global-1", true);
        state
            .create_admin_provider_model(&provider_model)
            .await
            .expect("provider model create should succeed")
            .expect("provider model should create");
        assert_eq!(candidate_repository.clear_count(), 1);

        let disabled_provider_model = sample_provider_model_record("model-1", "global-1", false);
        state
            .update_admin_provider_model(&disabled_provider_model)
            .await
            .expect("provider model update should succeed")
            .expect("provider model should update");
        assert_eq!(candidate_repository.clear_count(), 2);

        assert!(state
            .delete_admin_provider_model("provider-1", "model-1")
            .await
            .expect("provider model delete should succeed"));
        assert_eq!(candidate_repository.clear_count(), 3);

        let created_global_model = CreateAdminGlobalModelRecord::new(
            "global-2".to_string(),
            "gpt-4.1".to_string(),
            "GPT 4.1".to_string(),
            true,
            None,
            None,
            None,
            None,
        )
        .expect("global model create record should build");
        state
            .create_admin_global_model(&created_global_model)
            .await
            .expect("global model create should succeed")
            .expect("global model should create");
        assert_eq!(candidate_repository.clear_count(), 4);

        let disabled_global_model = UpdateAdminGlobalModelRecord::new(
            "global-1".to_string(),
            "GPT 5".to_string(),
            false,
            None,
            None,
            None,
            None,
        )
        .expect("global model update record should build");
        state
            .update_admin_global_model(&disabled_global_model)
            .await
            .expect("global model update should succeed")
            .expect("global model should update");
        assert_eq!(candidate_repository.clear_count(), 5);

        assert!(state
            .delete_admin_global_model("global-2")
            .await
            .expect("global model delete should succeed"));
        assert_eq!(candidate_repository.clear_count(), 6);
    }

    #[tokio::test]
    async fn provider_catalog_update_invalidates_scheduler_affinity_and_transport_snapshot_cache() {
        let provider = sample_provider();
        let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![provider.clone()],
            vec![sample_endpoint()],
            vec![sample_key()],
        ));
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                    .with_encryption_key_for_tests("test-encryption-key"),
            );

        let snapshot = state
            .read_provider_transport_snapshot("provider-1", "endpoint-1", "key-1")
            .await
            .expect("provider transport should read")
            .expect("provider transport should exist");
        assert!(!snapshot.provider.keep_priority_on_conversion);

        let cache_key = "scheduler_affinity:api-key-1:openai:chat:gpt-5";
        let ttl = Duration::from_secs(300);
        state.remember_scheduler_affinity_target(
            cache_key,
            SchedulerAffinityTarget {
                provider_id: "provider-1".to_string(),
                endpoint_id: "endpoint-1".to_string(),
                key_id: "key-1".to_string(),
            },
            ttl,
            128,
        );
        assert!(state
            .read_scheduler_affinity_target(cache_key, ttl)
            .is_some());
        let initial_epoch = state.scheduler_affinity_epoch();

        let mut updated_provider = provider;
        updated_provider.keep_priority_on_conversion = true;
        updated_provider.provider_priority = -10;
        state
            .update_provider_catalog_provider(&updated_provider)
            .await
            .expect("provider update should succeed")
            .expect("provider should update");

        assert!(state.scheduler_affinity_epoch() > initial_epoch);
        assert!(state
            .read_scheduler_affinity_target(cache_key, ttl)
            .is_none());
        let snapshot = state
            .read_provider_transport_snapshot("provider-1", "endpoint-1", "key-1")
            .await
            .expect("provider transport should read after update")
            .expect("provider transport should exist after update");
        assert!(snapshot.provider.keep_priority_on_conversion);
    }

    #[tokio::test]
    async fn non_codex_provider_update_does_not_wait_for_codex_ws_hot_state() {
        let provider = sample_provider();
        let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![provider.clone()],
            vec![sample_endpoint()],
            vec![sample_key()],
        ));
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                    .with_encryption_key_for_tests("test-encryption-key"),
            );
        let held_codex_ws_mutation = crate::codex_ws::hot_state::begin_catalog_hot_mutation(&state)
            .await
            .expect("test should hold the Codex WS mutation lock");

        let mut updated = provider;
        updated.description = Some("ordinary OpenAI update".to_string());
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            state.update_provider_catalog_provider(&updated),
        )
        .await
        .expect("non-Codex update must not contend on the Codex WS lock")
        .expect("non-Codex update should succeed");
        assert!(result.is_some());

        crate::codex_ws::hot_state::leave_hot_mutation_unstable(&state, held_codex_ws_mutation)
            .await;
    }

    #[tokio::test]
    async fn provider_catalog_health_update_keeps_scheduler_affinity_cache() {
        let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![sample_provider()],
            vec![sample_endpoint()],
            vec![sample_key()],
        ));
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                    .with_encryption_key_for_tests("test-encryption-key"),
            );

        let cache_key = "scheduler_affinity:api-key-1:openai:chat:gpt-5";
        let ttl = Duration::from_secs(300);
        let target = SchedulerAffinityTarget {
            provider_id: "provider-1".to_string(),
            endpoint_id: "endpoint-1".to_string(),
            key_id: "key-1".to_string(),
        };
        state.remember_scheduler_affinity_target(cache_key, target.clone(), ttl, 128);
        let initial_epoch = state.scheduler_affinity_epoch();

        let health_by_format = serde_json::json!({
            "openai:chat": {
                "last_success_at_unix_secs": 1,
                "consecutive_failures": 0
            }
        });
        let updated = state
            .update_provider_catalog_key_health_state("key-1", true, Some(&health_by_format), None)
            .await
            .expect("key health update should succeed");

        assert!(updated);
        assert_eq!(state.scheduler_affinity_epoch(), initial_epoch);
        assert_eq!(
            state.read_scheduler_affinity_target(cache_key, ttl),
            Some(target)
        );
    }

    #[tokio::test]
    async fn provider_key_upstream_metadata_update_keeps_scheduler_affinity_cache() {
        let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![sample_provider()],
            vec![sample_endpoint()],
            vec![sample_key()],
        ));
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                    .with_encryption_key_for_tests("test-encryption-key"),
            );

        let cache_key = "scheduler_affinity:api-key-1:openai:chat:gpt-5";
        let ttl = Duration::from_secs(300);
        let target = SchedulerAffinityTarget {
            provider_id: "provider-1".to_string(),
            endpoint_id: "endpoint-1".to_string(),
            key_id: "key-1".to_string(),
        };
        state.remember_scheduler_affinity_target(cache_key, target.clone(), ttl, 128);
        let initial_epoch = state.scheduler_affinity_epoch();

        let updated = state
            .update_provider_catalog_key_upstream_metadata(
                "key-1",
                Some(&json!({"quota": {"remaining": 99}})),
                Some(1_721_440_000),
            )
            .await
            .expect("upstream metadata update should succeed");

        assert!(updated);
        assert_eq!(state.scheduler_affinity_epoch(), initial_epoch);
        assert_eq!(
            state.read_scheduler_affinity_target(cache_key, ttl),
            Some(target)
        );
    }

    #[tokio::test]
    async fn restrictive_codex_runtime_update_fences_only_the_changed_key() {
        let key = sample_codex_ws_key();
        let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![sample_codex_provider()],
            vec![sample_codex_endpoint()],
            vec![key.clone()],
        ));
        let state = AppState::new()
            .expect("app state should build")
            .with_data_state_for_tests(
                GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                    .with_encryption_key_for_tests("test-encryption-key"),
            );
        let initial_lease =
            crate::codex_ws::hot_state::ensure_key_hot_leases(&state, std::slice::from_ref(&key))
                .await
                .expect("initial key hot state should initialize")
                .remove(&key.id)
                .expect("initial key lease should exist");
        assert!(initial_lease.eligible);
        let initial_scheduler_epoch = state.scheduler_affinity_epoch();

        let mut blocked = key;
        blocked.oauth_invalid_at_unix_secs = Some(1_721_440_000);
        blocked.oauth_invalid_reason = Some("oauth refresh rejected".to_string());
        state
            .update_provider_catalog_key_runtime_state(&blocked)
            .await
            .expect("restrictive runtime state should persist")
            .expect("key should update");

        assert_eq!(state.scheduler_affinity_epoch(), initial_scheduler_epoch);
        let blocked_lease = crate::codex_ws::hot_state::ensure_key_hot_leases(
            &state,
            std::slice::from_ref(&blocked),
        )
        .await
        .expect("blocked key hot state should read")
        .remove(&blocked.id)
        .expect("blocked key lease should exist");
        assert!(!blocked_lease.eligible);
        assert_ne!(blocked_lease.generation, initial_lease.generation);
    }
}
