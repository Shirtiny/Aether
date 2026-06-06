//! Runtime-mutable configuration that can be updated remotely via heartbeat.
//!
//! Fields in [`DynamicConfig`] are initially populated from the static
//! [`Config`](crate::config::Config) and may be overridden by the Aether
//! management backend through the heartbeat response.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use tracing::info;

use crate::config::Config;

/// Configuration that can be changed at runtime without restart.
#[derive(Debug, Clone)]
pub struct DynamicConfig {
    pub node_name: String,
    pub allowed_ports: Arc<HashSet<u16>>,
    pub log_level: String,
    pub heartbeat_interval: u64,
    /// Monotonically increasing version from the backend.
    /// `0` means no remote config has ever been applied.
    pub config_version: u64,
}

impl DynamicConfig {
    /// Initialize from static config (startup defaults).
    pub fn from_config(config: &Config) -> Self {
        Self {
            node_name: config.node_name.clone(),
            allowed_ports: Arc::new(config.allowed_ports.iter().copied().collect()),
            log_level: config.log_level.clone(),
            heartbeat_interval: config.heartbeat_interval,
            config_version: 0,
        }
    }
}

/// Shared dynamic config handle (lock-free reads via ArcSwap).
pub type SharedDynamicConfig = Arc<ArcSwap<DynamicConfig>>;

// -- Log-level hot-reload -----

/// Global log-level reloader function, set during tracing init.
type LogReloader = Box<dyn Fn(&str) + Send + Sync>;

static LOG_RELOADER: OnceLock<LogReloader> = OnceLock::new();

/// Register the log-level reload function (called once from `init_tracing`).
pub fn set_log_reloader(f: LogReloader) {
    let _ = LOG_RELOADER.set(f);
}

/// Apply a remote config update to the dynamic config.
///
/// Uses copy-on-write: loads the current snapshot, clones it, applies changes,
/// and stores the new Arc. Reads are always lock-free.
///
/// Returns `true` if the config was actually changed.
pub fn apply_remote_config(
    dynamic: &SharedDynamicConfig,
    remote: &crate::registration::client::RemoteConfig,
    version: u64,
) -> bool {
    let current = dynamic.load();

    if version <= current.config_version {
        return false;
    }

    let mut new_cfg = (**current).clone();
    let mut changed = Vec::new();

    if let Some(ref name) = remote.node_name {
        if *name != new_cfg.node_name {
            changed.push(format!("node_name -> {}", name));
            new_cfg.node_name = name.clone();
        }
    }

    if let Some(ref ports) = remote.allowed_ports {
        let new_set: HashSet<u16> = ports.iter().copied().collect();
        if new_set != *new_cfg.allowed_ports {
            changed.push(format!("allowed_ports -> {:?}", ports));
            new_cfg.allowed_ports = Arc::new(new_set);
        }
    }

    if let Some(interval) = remote.heartbeat_interval {
        if interval != new_cfg.heartbeat_interval {
            changed.push(format!("heartbeat_interval -> {}s", interval));
            new_cfg.heartbeat_interval = interval;
        }
    }

    if let Some(ref level) = remote.log_level {
        if *level != new_cfg.log_level {
            changed.push(format!("log_level -> {}", level));
            new_cfg.log_level = level.clone();
            // Hot-reload tracing filter
            if let Some(reloader) = LOG_RELOADER.get() {
                reloader(level);
            }
        }
    }

    let has_changes = !changed.is_empty();

    if has_changes {
        new_cfg.config_version = version;
        info!(
            version,
            changes = %changed.join(", "),
            "remote config applied"
        );
        dynamic.store(Arc::new(new_cfg));
    }

    has_changes
}
