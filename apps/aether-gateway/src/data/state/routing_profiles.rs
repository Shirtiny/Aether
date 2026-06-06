use aether_data_contracts::repository::routing_profiles::{
    CreateRoutingGroupBindingRecord, CreateRoutingGroupRecord, CreateRoutingGroupVersionRecord,
    RoutingGroupBindingQuery, RoutingGroupLookupKey, RoutingGroupReadRepository,
    StoredRoutingGroup, StoredRoutingGroupBinding, StoredRoutingGroupVersion,
    UpdateRoutingGroupBindingRecord, UpdateRoutingGroupRecord,
};
use std::sync::Arc;

use super::{DataLayerError, GatewayDataState};

impl GatewayDataState {
    pub(crate) fn routing_group_read_repository(
        &self,
    ) -> Option<Arc<dyn RoutingGroupReadRepository>> {
        self.routing_group_reader.clone()
    }

    pub(crate) async fn list_routing_groups(
        &self,
    ) -> Result<Vec<StoredRoutingGroup>, DataLayerError> {
        match &self.routing_group_reader {
            Some(repository) => repository.list_routing_groups().await,
            None => Ok(Vec::new()),
        }
    }

    pub(crate) async fn find_routing_group(
        &self,
        lookup: RoutingGroupLookupKey<'_>,
    ) -> Result<Option<StoredRoutingGroup>, DataLayerError> {
        match &self.routing_group_reader {
            Some(repository) => repository.find_routing_group(lookup).await,
            None => Ok(None),
        }
    }

    pub(crate) async fn list_routing_group_bindings(
        &self,
        query: &RoutingGroupBindingQuery,
    ) -> Result<Vec<StoredRoutingGroupBinding>, DataLayerError> {
        match &self.routing_group_reader {
            Some(repository) => repository.list_routing_group_bindings(query).await,
            None => Ok(Vec::new()),
        }
    }

    pub(crate) async fn list_routing_group_versions(
        &self,
        group_id: &str,
    ) -> Result<Vec<StoredRoutingGroupVersion>, DataLayerError> {
        match &self.routing_group_reader {
            Some(repository) => repository.list_routing_group_versions(group_id).await,
            None => Ok(Vec::new()),
        }
    }

    pub(crate) async fn create_routing_group(
        &self,
        record: CreateRoutingGroupRecord,
    ) -> Result<Option<StoredRoutingGroup>, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository.create_routing_group(record).await.map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn update_routing_group(
        &self,
        id: &str,
        patch: UpdateRoutingGroupRecord,
    ) -> Result<Option<StoredRoutingGroup>, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository.update_routing_group(id, patch).await,
            None => Ok(None),
        }
    }

    pub(crate) async fn delete_routing_group(&self, id: &str) -> Result<bool, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository.delete_routing_group(id).await,
            None => Ok(false),
        }
    }

    pub(crate) async fn create_routing_group_binding(
        &self,
        record: CreateRoutingGroupBindingRecord,
    ) -> Result<Option<StoredRoutingGroupBinding>, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository
                .create_routing_group_binding(record)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn update_routing_group_binding(
        &self,
        id: &str,
        patch: UpdateRoutingGroupBindingRecord,
    ) -> Result<Option<StoredRoutingGroupBinding>, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository.update_routing_group_binding(id, patch).await,
            None => Ok(None),
        }
    }

    pub(crate) async fn delete_routing_group_binding(
        &self,
        id: &str,
    ) -> Result<bool, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository.delete_routing_group_binding(id).await,
            None => Ok(false),
        }
    }

    pub(crate) async fn create_routing_group_version(
        &self,
        record: CreateRoutingGroupVersionRecord,
    ) -> Result<Option<StoredRoutingGroupVersion>, DataLayerError> {
        match &self.routing_group_writer {
            Some(repository) => repository
                .create_routing_group_version(record)
                .await
                .map(Some),
            None => Ok(None),
        }
    }
}
