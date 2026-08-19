use std::time::Duration;

use aether_cache::ExpiringMap;
use tokio::sync::{Mutex, MutexGuard};

const MAX_ENTRIES: usize = 512;
pub(crate) const SYSTEM_CONFIG_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct SystemConfigCache {
    entries: ExpiringMap<String, Option<serde_json::Value>>,
    reload: Mutex<()>,
}

impl Default for SystemConfigCache {
    fn default() -> Self {
        Self {
            entries: ExpiringMap::new(),
            reload: Mutex::new(()),
        }
    }
}

impl SystemConfigCache {
    pub(crate) fn get(&self, key: &str, ttl: Duration) -> Option<Option<serde_json::Value>> {
        self.entries.get_fresh(&key.to_string(), ttl)
    }

    pub(crate) fn insert(&self, key: String, value: Option<serde_json::Value>, ttl: Duration) {
        self.entries.insert(key, value, ttl, MAX_ENTRIES);
    }

    pub(crate) fn clear(&self) {
        self.entries.clear();
    }

    pub(crate) async fn lock_reload(&self) -> MutexGuard<'_, ()> {
        self.reload.lock().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use tokio::sync::Barrier;

    use super::SystemConfigCache;

    #[tokio::test]
    async fn cache_preserves_values_negative_entries_and_expiry() {
        let cache = SystemConfigCache::default();
        let ttl = Duration::from_secs(30);
        cache.insert("present".to_string(), Some(json!(true)), ttl);
        cache.insert("missing".to_string(), None, ttl);

        assert_eq!(cache.get("present", ttl), Some(Some(json!(true))));
        assert_eq!(cache.get("missing", ttl), Some(None));
        assert_eq!(cache.get("unknown", ttl), None);

        cache.insert(
            "expired".to_string(),
            Some(json!("old")),
            Duration::from_millis(1),
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(cache.get("expired", Duration::from_millis(1)), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reload_lock_collapses_concurrent_cold_reads() {
        const READERS: usize = 200;
        let cache = Arc::new(SystemConfigCache::default());
        let barrier = Arc::new(Barrier::new(READERS + 1));
        let backend_reads = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_secs(30);
        let mut tasks = Vec::with_capacity(READERS);

        for _ in 0..READERS {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let backend_reads = Arc::clone(&backend_reads);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                if cache.get("capture_policy", ttl).is_none() {
                    let _reload = cache.lock_reload().await;
                    if cache.get("capture_policy", ttl).is_none() {
                        backend_reads.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        cache.insert(
                            "capture_policy".to_string(),
                            Some(json!({"record_level": "basic"})),
                            ttl,
                        );
                    }
                }
                cache.get("capture_policy", ttl)
            }));
        }

        barrier.wait().await;
        for task in tasks {
            assert_eq!(
                task.await.expect("cache reader should complete"),
                Some(Some(json!({"record_level": "basic"})))
            );
        }
        assert_eq!(backend_reads.load(Ordering::SeqCst), 1);
    }
}
