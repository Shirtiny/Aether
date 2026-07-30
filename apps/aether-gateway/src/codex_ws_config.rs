use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::sync::{Mutex, MutexGuard};
use tracing::warn;

use crate::handlers::shared::system_config_bool;
use crate::provider_transport::CodexOfficialWsGlobalFlags;
use crate::AppState;

pub(crate) const CODEX_WS_SYSTEM_CONFIG_KEY: &str = "codex_ws";
const DEFAULT_CODEX_WS_FEATURE_FLAGS: CodexWsFeatureFlags = CodexWsFeatureFlags {
    enabled: true,
    native_codex_ws_enabled: true,
    // The v2 reader is deliberately opt-in. Writers can dual-publish the v2
    // state before every gateway instance is upgraded.
    catalog_fence_v2_enabled: false,
};
const ENABLED: u64 = 1 << 0;
const NATIVE_CODEX_WS_ENABLED: u64 = 1 << 1;
const CATALOG_FENCE_V2_ENABLED: u64 = 1 << 2;
const SNAPSHOT_INITIALIZED: u64 = 1 << 3;
const GENERATION_SHIFT: u32 = 4;
const STATE_MASK: u64 = (1 << GENERATION_SHIFT) - 1;
const CONFIG_READ_ERROR_BACKOFF_MS: u64 = 250;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexWsFeatureFlags {
    pub(crate) enabled: bool,
    pub(crate) native_codex_ws_enabled: bool,
    pub(crate) catalog_fence_v2_enabled: bool,
}

impl CodexWsFeatureFlags {
    pub(crate) fn native_account_flags(self) -> CodexOfficialWsGlobalFlags {
        CodexOfficialWsGlobalFlags {
            enabled: self.enabled,
            native_codex_ws_enabled: self.native_codex_ws_enabled,
        }
    }

    const fn to_bits(self) -> u64 {
        (if self.enabled { ENABLED } else { 0 })
            | (if self.native_codex_ws_enabled {
                NATIVE_CODEX_WS_ENABLED
            } else {
                0
            })
            | (if self.catalog_fence_v2_enabled {
                CATALOG_FENCE_V2_ENABLED
            } else {
                0
            })
    }

    const fn from_bits(bits: u64) -> Self {
        Self {
            enabled: bits & ENABLED != 0,
            native_codex_ws_enabled: bits & NATIVE_CODEX_WS_ENABLED != 0,
            catalog_fence_v2_enabled: bits & CATALOG_FENCE_V2_ENABLED != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexWsFeatureFlagsLease {
    pub(crate) flags: CodexWsFeatureFlags,
    pub(crate) generation: u64,
}

impl CodexWsFeatureFlagsLease {
    pub(crate) fn native_account_flags(self) -> CodexOfficialWsGlobalFlags {
        self.flags.native_account_flags()
    }

    pub(crate) fn native_enabled(self) -> bool {
        self.flags.enabled && self.flags.native_codex_ws_enabled
    }
}

#[derive(Debug, Default)]
pub(crate) struct CodexWsFeatureFlagsSnapshot {
    // Flags and generation share one word so readers cannot bind a torn configuration.
    state: AtomicU64,
    read_error_retry_after_unix_ms: AtomicU64,
    initialization_lock: Mutex<()>,
}

impl CodexWsFeatureFlagsSnapshot {
    pub(crate) fn load(&self) -> Option<CodexWsFeatureFlagsLease> {
        let state = self.state.load(Ordering::Acquire);
        Self::lease_from_state(state)
    }

    pub(crate) fn store(&self, flags: CodexWsFeatureFlags) -> CodexWsFeatureFlagsLease {
        self.read_error_retry_after_unix_ms
            .store(0, Ordering::Release);
        self.update(flags, true)
            .expect("an initialized feature snapshot always returns a lease")
    }

    fn initialize_if_unchanged(
        &self,
        expected: u64,
        flags: CodexWsFeatureFlags,
    ) -> Option<CodexWsFeatureFlagsLease> {
        let next = Self::next_state(expected, flags, true);
        match self
            .state
            .compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Self::lease_from_state(next),
            Err(current) => Self::lease_from_state(current),
        }
    }

    pub(crate) fn clear(&self) {
        let _ = self.update(CodexWsFeatureFlags::default(), false);
    }

    pub(crate) fn is_current_native(&self, lease: CodexWsFeatureFlagsLease) -> bool {
        lease.native_enabled() && self.load() == Some(lease)
    }

    async fn initialization_guard(&self) -> MutexGuard<'_, ()> {
        self.initialization_lock.lock().await
    }

    fn update(
        &self,
        flags: CodexWsFeatureFlags,
        initialized: bool,
    ) -> Option<CodexWsFeatureFlagsLease> {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let next = Self::next_state(current, flags, initialized);
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Self::lease_from_state(next),
                Err(observed) => current = observed,
            }
        }
    }

    fn next_state(current: u64, flags: CodexWsFeatureFlags, initialized: bool) -> u64 {
        let generation = (current >> GENERATION_SHIFT).wrapping_add(1);
        (generation << GENERATION_SHIFT)
            | if initialized {
                SNAPSHOT_INITIALIZED | flags.to_bits()
            } else {
                0
            }
    }

    fn lease_from_state(state: u64) -> Option<CodexWsFeatureFlagsLease> {
        (state & SNAPSHOT_INITIALIZED != 0).then(|| CodexWsFeatureFlagsLease {
            flags: CodexWsFeatureFlags::from_bits(state & STATE_MASK),
            generation: state >> GENERATION_SHIFT,
        })
    }
}

/// Loads the process snapshot once. After initialization this path is an atomic read and cannot
/// reach the data layer. Long-lived connections retain the returned generation lease and validate
/// it with `is_current_native` at execution fences.
pub(crate) async fn read_codex_ws_feature_flags(state: &AppState) -> CodexWsFeatureFlagsLease {
    loop {
        if let Some(flags) = state.codex_ws_feature_flags.load() {
            return flags;
        }
        let now_unix_ms = crate::clock::current_unix_ms();
        if state
            .codex_ws_feature_flags
            .read_error_retry_after_unix_ms
            .load(Ordering::Acquire)
            > now_unix_ms
        {
            return CodexWsFeatureFlagsLease {
                flags: CodexWsFeatureFlags::default(),
                generation: state.codex_ws_feature_flags.state.load(Ordering::Acquire)
                    >> GENERATION_SHIFT,
            };
        }

        let _initialization_guard = state.codex_ws_feature_flags.initialization_guard().await;
        if let Some(flags) = state.codex_ws_feature_flags.load() {
            return flags;
        }
        let expected = state.codex_ws_feature_flags.state.load(Ordering::Acquire);
        if let Some(flags) = CodexWsFeatureFlagsSnapshot::lease_from_state(expected) {
            return flags;
        }
        let flags = match state
            .read_system_config_json_value(CODEX_WS_SYSTEM_CONFIG_KEY)
            .await
        {
            Ok(config) => parse_codex_ws_feature_flags(config.as_ref()),
            Err(error) => {
                warn!(error = ?error, "gateway codex ws config lookup failed");
                state
                    .codex_ws_feature_flags
                    .read_error_retry_after_unix_ms
                    .store(
                        crate::clock::current_unix_ms()
                            .saturating_add(CONFIG_READ_ERROR_BACKOFF_MS),
                        Ordering::Release,
                    );
                return CodexWsFeatureFlagsLease {
                    flags: CodexWsFeatureFlags::default(),
                    generation: expected >> GENERATION_SHIFT,
                };
            }
        };
        state
            .codex_ws_feature_flags
            .read_error_retry_after_unix_ms
            .store(0, Ordering::Release);
        if let Some(flags) = state
            .codex_ws_feature_flags
            .initialize_if_unchanged(expected, flags)
        {
            return flags;
        }
    }
}

pub(crate) fn parse_codex_ws_feature_flags(config: Option<&Value>) -> CodexWsFeatureFlags {
    let Some(config) = config else {
        return DEFAULT_CODEX_WS_FEATURE_FLAGS;
    };
    let Some(config) = config.as_object() else {
        return CodexWsFeatureFlags::default();
    };

    CodexWsFeatureFlags {
        enabled: system_config_bool(config.get("enabled"), !config.contains_key("enabled")),
        native_codex_ws_enabled: system_config_bool(
            config.get("native_codex_ws_enabled"),
            !config.contains_key("native_codex_ws_enabled"),
        ),
        catalog_fence_v2_enabled: system_config_bool(config.get("catalog_fence_v2_enabled"), false),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use serde_json::json;

    use super::{
        parse_codex_ws_feature_flags, read_codex_ws_feature_flags, CodexWsFeatureFlags,
        CodexWsFeatureFlagsSnapshot, CODEX_WS_SYSTEM_CONFIG_KEY,
    };
    use crate::data::GatewayDataState;
    use crate::AppState;

    #[test]
    fn defaults_missing_config_and_fields_on() {
        let enabled = CodexWsFeatureFlags {
            enabled: true,
            native_codex_ws_enabled: true,
            catalog_fence_v2_enabled: false,
        };

        assert_eq!(parse_codex_ws_feature_flags(None), enabled);
        assert_eq!(parse_codex_ws_feature_flags(Some(&json!({}))), enabled);
        assert_eq!(
            parse_codex_ws_feature_flags(Some(&json!({"enabled": false}))),
            CodexWsFeatureFlags {
                enabled: false,
                native_codex_ws_enabled: true,
                catalog_fence_v2_enabled: false,
            }
        );
        assert_eq!(
            parse_codex_ws_feature_flags(Some(&json!({
                "native_codex_ws_enabled": false
            }))),
            CodexWsFeatureFlags {
                enabled: true,
                native_codex_ws_enabled: false,
                catalog_fence_v2_enabled: false,
            }
        );
    }

    #[test]
    fn malformed_config_and_fields_fail_closed() {
        assert_eq!(
            parse_codex_ws_feature_flags(Some(&json!(true))),
            CodexWsFeatureFlags::default()
        );
        assert_eq!(
            parse_codex_ws_feature_flags(Some(&json!({
                "enabled": "not-a-bool",
                "native_codex_ws_enabled": null
            }))),
            CodexWsFeatureFlags::default()
        );
        assert_eq!(
            parse_codex_ws_feature_flags(Some(&json!({"enabled": []}))),
            CodexWsFeatureFlags {
                enabled: false,
                native_codex_ws_enabled: true,
                catalog_fence_v2_enabled: false,
            }
        );
    }

    #[test]
    fn parses_all_flags_from_one_config_object() {
        let flags = parse_codex_ws_feature_flags(Some(&json!({
            "enabled": true,
            "native_codex_ws_enabled": true,
            "catalog_fence_v2_enabled": true
        })));

        assert_eq!(
            flags,
            CodexWsFeatureFlags {
                enabled: true,
                native_codex_ws_enabled: true,
                catalog_fence_v2_enabled: true,
            }
        );
        assert_eq!(
            flags.native_account_flags(),
            aether_provider_transport::CodexOfficialWsGlobalFlags {
                enabled: true,
                native_codex_ws_enabled: true,
            }
        );
    }

    #[test]
    fn initialization_cannot_overwrite_a_concurrent_config_write() {
        let snapshot = CodexWsFeatureFlagsSnapshot::default();
        let fresh = CodexWsFeatureFlags {
            enabled: true,
            native_codex_ws_enabled: true,
            catalog_fence_v2_enabled: false,
        };
        let expected = snapshot.state.load(Ordering::Acquire);
        let written = snapshot.store(fresh);

        let initialized = snapshot
            .initialize_if_unchanged(expected, CodexWsFeatureFlags::default())
            .expect("the concurrent initialized snapshot should be returned");

        assert_eq!(initialized, written);
        assert_eq!(initialized.flags, fresh);
        assert_eq!(snapshot.load(), Some(written));
    }

    #[test]
    fn disabling_and_reenabling_invalidates_the_retained_generation() {
        let snapshot = CodexWsFeatureFlagsSnapshot::default();
        let enabled = CodexWsFeatureFlags {
            enabled: true,
            native_codex_ws_enabled: true,
            catalog_fence_v2_enabled: false,
        };
        let original = snapshot.store(enabled);
        assert!(snapshot.is_current_native(original));

        let disabled = snapshot.store(CodexWsFeatureFlags::default());
        assert!(!snapshot.is_current_native(original));
        assert!(!snapshot.is_current_native(disabled));

        let reenabled = snapshot.store(enabled);
        assert!(snapshot.is_current_native(reenabled));
        assert_ne!(reenabled.generation, original.generation);
        assert!(!snapshot.is_current_native(original));
    }

    #[test]
    fn clearing_an_initialized_snapshot_invalidates_its_lease() {
        let snapshot = CodexWsFeatureFlagsSnapshot::default();
        let retained = snapshot.store(CodexWsFeatureFlags {
            enabled: true,
            native_codex_ws_enabled: true,
            catalog_fence_v2_enabled: false,
        });

        snapshot.clear();

        assert_eq!(snapshot.load(), None);
        assert!(!snapshot.is_current_native(retained));
    }

    #[test]
    fn stale_initialization_cannot_cross_a_clear_generation() {
        let snapshot = CodexWsFeatureFlagsSnapshot::default();
        let stale_generation = snapshot.state.load(Ordering::Acquire);
        snapshot.clear();

        let initialized = snapshot.initialize_if_unchanged(
            stale_generation,
            CodexWsFeatureFlags {
                enabled: true,
                native_codex_ws_enabled: true,
                catalog_fence_v2_enabled: false,
            },
        );

        assert_eq!(initialized, None);
        assert_eq!(snapshot.load(), None);
    }

    #[test]
    fn concurrent_writes_publish_unique_atomic_generations() {
        let snapshot = std::sync::Arc::new(CodexWsFeatureFlagsSnapshot::default());
        let generations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));

        std::thread::scope(|scope| {
            for index in 0..8 {
                let snapshot = std::sync::Arc::clone(&snapshot);
                let generations = std::sync::Arc::clone(&generations);
                let start = std::sync::Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    let lease = snapshot.store(CodexWsFeatureFlags {
                        enabled: index % 2 == 0,
                        native_codex_ws_enabled: index % 2 == 0,
                        catalog_fence_v2_enabled: false,
                    });
                    generations
                        .lock()
                        .expect("generation result should lock")
                        .push(lease.generation);
                });
            }
        });

        let mut generations = generations
            .lock()
            .expect("generation results should lock")
            .clone();
        generations.sort_unstable();
        generations.dedup();
        assert_eq!(generations.len(), 8);
        assert!(generations.contains(
            &snapshot
                .load()
                .expect("one concurrent write should remain visible")
                .generation
        ));
    }

    #[tokio::test]
    async fn missing_app_config_initializes_the_process_snapshot_on() {
        let state = AppState::new()
            .expect("state should build")
            .with_data_state_for_tests(GatewayDataState::disabled());

        let flags = read_codex_ws_feature_flags(&state).await;

        assert!(flags.native_enabled());
        assert_eq!(state.codex_ws_feature_flags.load(), Some(flags));
    }

    #[tokio::test]
    async fn initializes_once_then_reads_only_the_process_snapshot() {
        let data = GatewayDataState::disabled().with_system_config_values_for_tests([(
            CODEX_WS_SYSTEM_CONFIG_KEY.to_string(),
            json!({
                "enabled": true,
                "native_codex_ws_enabled": true
            }),
        )]);
        let state = AppState::new()
            .expect("state should build")
            .with_data_state_for_tests(data);

        let flags = read_codex_ws_feature_flags(&state).await;

        assert_eq!(
            flags.flags,
            CodexWsFeatureFlags {
                enabled: true,
                native_codex_ws_enabled: true,
                catalog_fence_v2_enabled: false,
            }
        );

        state
            .data
            .upsert_system_config_value(
                CODEX_WS_SYSTEM_CONFIG_KEY,
                &json!({
                    "enabled": false,
                    "native_codex_ws_enabled": false
                }),
                None,
            )
            .await
            .expect("direct test data update should succeed");
        state.system_config_cache.clear();

        assert_eq!(read_codex_ws_feature_flags(&state).await, flags);
    }

    #[tokio::test]
    async fn app_config_writes_refresh_the_process_snapshot_immediately() {
        let state = AppState::new()
            .expect("state should build")
            .with_data_state_for_tests(
                GatewayDataState::disabled().with_system_config_values_for_tests([(
                    CODEX_WS_SYSTEM_CONFIG_KEY.to_string(),
                    json!({"enabled": true, "native_codex_ws_enabled": true}),
                )]),
            );
        let enabled = read_codex_ws_feature_flags(&state).await;
        assert!(enabled.native_enabled());

        state
            .upsert_system_config_json_value(
                CODEX_WS_SYSTEM_CONFIG_KEY,
                &json!({"enabled": false, "native_codex_ws_enabled": false}),
                None,
            )
            .await
            .expect("app config update should succeed");

        let disabled = read_codex_ws_feature_flags(&state).await;
        assert_eq!(disabled.flags, CodexWsFeatureFlags::default());
        assert!(disabled.generation > enabled.generation);

        assert!(state
            .delete_system_config_value(CODEX_WS_SYSTEM_CONFIG_KEY)
            .await
            .expect("app config delete should succeed"));

        let restored_default = read_codex_ws_feature_flags(&state).await;
        assert!(restored_default.native_enabled());
        assert!(restored_default.generation > disabled.generation);
    }
}
