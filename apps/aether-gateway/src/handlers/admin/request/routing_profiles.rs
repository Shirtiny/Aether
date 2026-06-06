use aether_data_contracts::repository::routing_profiles::{
    CreateRoutingGroupBindingRecord, CreateRoutingGroupRecord, CreateRoutingGroupVersionRecord,
    RoutingGroupBindingQuery, RoutingGroupLookupKey, StoredRoutingGroup, StoredRoutingGroupBinding,
    StoredRoutingGroupVersion, UpdateRoutingGroupBindingRecord, UpdateRoutingGroupRecord,
};

use super::AdminAppState;
use crate::GatewayError;

impl<'a> AdminAppState<'a> {
    pub(crate) async fn list_routing_groups(
        &self,
    ) -> Result<Vec<StoredRoutingGroup>, GatewayError> {
        self.app.list_routing_groups().await
    }

    pub(crate) async fn find_routing_group(
        &self,
        lookup: RoutingGroupLookupKey<'_>,
    ) -> Result<Option<StoredRoutingGroup>, GatewayError> {
        self.app.find_routing_group(lookup).await
    }

    pub(crate) async fn list_routing_group_bindings(
        &self,
        query: &RoutingGroupBindingQuery,
    ) -> Result<Vec<StoredRoutingGroupBinding>, GatewayError> {
        self.app.list_routing_group_bindings(query).await
    }

    pub(crate) async fn list_routing_group_versions(
        &self,
        group_id: &str,
    ) -> Result<Vec<StoredRoutingGroupVersion>, GatewayError> {
        self.app.list_routing_group_versions(group_id).await
    }

    pub(crate) async fn create_routing_group(
        &self,
        record: CreateRoutingGroupRecord,
    ) -> Result<Option<StoredRoutingGroup>, GatewayError> {
        self.app.create_routing_group(record).await
    }

    pub(crate) async fn update_routing_group(
        &self,
        id: &str,
        patch: UpdateRoutingGroupRecord,
    ) -> Result<Option<StoredRoutingGroup>, GatewayError> {
        self.app.update_routing_group(id, patch).await
    }

    pub(crate) async fn delete_routing_group(&self, id: &str) -> Result<bool, GatewayError> {
        self.app.delete_routing_group(id).await
    }

    pub(crate) async fn create_routing_group_binding(
        &self,
        record: CreateRoutingGroupBindingRecord,
    ) -> Result<Option<StoredRoutingGroupBinding>, GatewayError> {
        self.app.create_routing_group_binding(record).await
    }

    pub(crate) async fn update_routing_group_binding(
        &self,
        id: &str,
        patch: UpdateRoutingGroupBindingRecord,
    ) -> Result<Option<StoredRoutingGroupBinding>, GatewayError> {
        self.app.update_routing_group_binding(id, patch).await
    }

    pub(crate) async fn delete_routing_group_binding(
        &self,
        id: &str,
    ) -> Result<bool, GatewayError> {
        self.app.delete_routing_group_binding(id).await
    }

    pub(crate) async fn create_routing_group_version(
        &self,
        record: CreateRoutingGroupVersionRecord,
    ) -> Result<Option<StoredRoutingGroupVersion>, GatewayError> {
        self.app.create_routing_group_version(record).await
    }
}
