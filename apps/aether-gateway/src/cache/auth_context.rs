use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aether_cache::ExpiringMap;

use crate::control::GatewayControlAuthContext;

#[derive(Debug, Default)]
pub(crate) struct AuthContextCache {
    entries: ExpiringMap<String, GatewayControlAuthContext>,
    invalidation_epoch: AtomicU64,
}

impl AuthContextCache {
    pub(crate) fn get_fresh(
        &self,
        cache_key: &str,
        ttl: Duration,
    ) -> Option<GatewayControlAuthContext> {
        self.entries.get_fresh(&cache_key.to_string(), ttl)
    }

    pub(crate) fn insert(
        &self,
        cache_key: String,
        auth_context: GatewayControlAuthContext,
        ttl: Duration,
        max_entries: usize,
    ) {
        self.entries
            .insert(cache_key, auth_context, ttl, max_entries);
    }

    pub(crate) fn clear(&self) {
        self.entries.clear();
        self.invalidation_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn invalidation_epoch(&self) -> u64 {
        self.invalidation_epoch.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::AuthContextCache;

    #[test]
    fn clearing_auth_snapshot_advances_invalidation_epoch() {
        let cache = AuthContextCache::default();
        let before = cache.invalidation_epoch();

        cache.clear();

        assert_eq!(cache.invalidation_epoch(), before + 1);
    }
}
