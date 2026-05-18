use aether_data_contracts::repository::routing_profiles::{
    CreateRoutingGroupBindingRecord, CreateRoutingGroupRecord, CreateRoutingGroupVersionRecord,
    RoutingGroupBindingQuery, RoutingGroupLookupKey, RoutingGroupReadRepository,
    StoredRoutingGroup, StoredRoutingGroupBinding, StoredRoutingGroupVersion,
    UpdateRoutingGroupBindingRecord, UpdateRoutingGroupRecord,
};
use std::sync::Arc;

use super::{AppState, GatewayError};

impl AppState {
    pub(crate) fn has_routing_group_data_reader(&self) -> bool {
        self.data.has_routing_group_reader()
    }

    pub(crate) fn has_routing_group_data_writer(&self) -> bool {
        self.data.has_routing_group_writer()
    }

    pub(crate) fn routing_group_read_repository(
        &self,
    ) -> Option<Arc<dyn RoutingGroupReadRepository>> {
        self.data.routing_group_read_repository()
    }

    pub(crate) async fn list_routing_groups(
        &self,
    ) -> Result<Vec<StoredRoutingGroup>, GatewayError> {
        self.data
            .list_routing_groups()
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn find_routing_group(
        &self,
        lookup: RoutingGroupLookupKey<'_>,
    ) -> Result<Option<StoredRoutingGroup>, GatewayError> {
        self.data
            .find_routing_group(lookup)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_routing_group_bindings(
        &self,
        query: &RoutingGroupBindingQuery,
    ) -> Result<Vec<StoredRoutingGroupBinding>, GatewayError> {
        self.data
            .list_routing_group_bindings(query)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn list_routing_group_versions(
        &self,
        group_id: &str,
    ) -> Result<Vec<StoredRoutingGroupVersion>, GatewayError> {
        self.data
            .list_routing_group_versions(group_id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }

    pub(crate) async fn create_routing_group(
        &self,
        record: CreateRoutingGroupRecord,
    ) -> Result<Option<StoredRoutingGroup>, GatewayError> {
        let created = self
            .data
            .create_routing_group(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn update_routing_group(
        &self,
        id: &str,
        patch: UpdateRoutingGroupRecord,
    ) -> Result<Option<StoredRoutingGroup>, GatewayError> {
        let updated = self
            .data
            .update_routing_group(id, patch)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_routing_group(&self, id: &str) -> Result<bool, GatewayError> {
        let deleted = self
            .data
            .delete_routing_group(id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn create_routing_group_binding(
        &self,
        record: CreateRoutingGroupBindingRecord,
    ) -> Result<Option<StoredRoutingGroupBinding>, GatewayError> {
        let created = self
            .data
            .create_routing_group_binding(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if created.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(created)
    }

    pub(crate) async fn update_routing_group_binding(
        &self,
        id: &str,
        patch: UpdateRoutingGroupBindingRecord,
    ) -> Result<Option<StoredRoutingGroupBinding>, GatewayError> {
        let updated = self
            .data
            .update_routing_group_binding(id, patch)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if updated.is_some() {
            self.invalidate_provider_routing_caches();
        }
        Ok(updated)
    }

    pub(crate) async fn delete_routing_group_binding(
        &self,
        id: &str,
    ) -> Result<bool, GatewayError> {
        let deleted = self
            .data
            .delete_routing_group_binding(id)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))?;
        if deleted {
            self.invalidate_provider_routing_caches();
        }
        Ok(deleted)
    }

    pub(crate) async fn create_routing_group_version(
        &self,
        record: CreateRoutingGroupVersionRecord,
    ) -> Result<Option<StoredRoutingGroupVersion>, GatewayError> {
        self.data
            .create_routing_group_version(record)
            .await
            .map_err(|err| GatewayError::Internal(err.to_string()))
    }
}
