use std::time::Duration;

use super::super::provider_transport;

pub(crate) const AUTH_API_KEY_LAST_USED_TTL: Duration = Duration::from_secs(60);
pub(crate) const AUTH_API_KEY_LAST_USED_MAX_ENTRIES: usize = 10_000;
pub(crate) const PROVIDER_TRANSPORT_SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(1);
pub(crate) const PROVIDER_TRANSPORT_SNAPSHOT_CACHE_MAX_ENTRIES: usize = 1_024;

#[derive(Debug, Clone)]
pub(crate) struct CachedProviderTransportSnapshot {
    pub(crate) epoch: u64,
    pub(crate) loaded_at: std::time::Instant,
    pub(crate) snapshot: provider_transport::GatewayProviderTransportSnapshot,
}
