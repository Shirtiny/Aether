use std::time::Duration;

use aether_cache::ExpiringMap;
use aether_data::repository::proxy_nodes::StoredProxyNode;

// One entry per node; the table holds a handful of rows in practice.
const MAX_ENTRIES: usize = 128;

/// Deduplicates proxy node lookups during candidate resolution.
///
/// Transport resolution reads a node per candidate, so a single request repeats
/// the same few lookups tens of times. Node rows do change — heartbeats and
/// tunnel connect/disconnect rewrite them — so entries live only long enough to
/// collapse the repeats within one request's selection phase.
#[derive(Debug, Default)]
pub(crate) struct ProxyNodeCache {
    entries: ExpiringMap<String, Option<StoredProxyNode>>,
}

impl ProxyNodeCache {
    pub(crate) fn get(&self, node_id: &str, ttl: Duration) -> Option<Option<StoredProxyNode>> {
        self.entries.get_fresh(&node_id.to_string(), ttl)
    }

    pub(crate) fn insert(&self, node_id: String, node: Option<StoredProxyNode>, ttl: Duration) {
        self.entries.insert(node_id, node, ttl, MAX_ENTRIES);
    }

    pub(crate) fn clear(&self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

    use super::*;

    fn sample_node(id: &str) -> StoredProxyNode {
        StoredProxyNode::new(
            id.to_string(),
            id.to_string(),
            "127.0.0.1".to_string(),
            0,
            false,
            "online".to_string(),
            15,
            1,
            0,
            0,
            0,
            0,
            false,
            false,
            1,
        )
        .expect("sample node should build")
    }

    #[test]
    fn serves_repeat_lookups_within_the_window_and_reloads_after_it() {
        let cache = ProxyNodeCache::default();
        let ttl = Duration::from_millis(80);

        assert!(cache.get("node-1", ttl).is_none());
        cache.insert("node-1".to_string(), Some(sample_node("node-1")), ttl);

        let hit = cache.get("node-1", ttl).expect("entry should be cached");
        assert_eq!(hit.expect("node should be present").id, "node-1");

        sleep(Duration::from_millis(120));
        assert!(
            cache.get("node-1", ttl).is_none(),
            "entry must expire so a mutated node is picked up without invalidation"
        );
    }

    #[test]
    fn caches_missing_nodes_so_lookups_for_unknown_ids_are_not_repeated() {
        let cache = ProxyNodeCache::default();
        let ttl = Duration::from_secs(5);

        cache.insert("gone".to_string(), None, ttl);

        let hit = cache.get("gone", ttl).expect("absence should be cached");
        assert!(hit.is_none());
    }

    #[test]
    fn clear_drops_every_entry() {
        let cache = ProxyNodeCache::default();
        let ttl = Duration::from_secs(5);
        cache.insert("node-1".to_string(), Some(sample_node("node-1")), ttl);

        cache.clear();

        assert!(cache.get("node-1", ttl).is_none());
    }
}
