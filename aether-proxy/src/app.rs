//! Application lifecycle: initialization, task orchestration, and shutdown.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::signal;
use tokio::sync::{watch, Mutex};
use tracing::{error, info, warn};

use crate::config::{Config, ServerEntry};
use crate::net;
use crate::registration::client::AetherClient;
use crate::runtime::{self, DynamicConfig};
use crate::state::{AppState, ProxyMetrics, ServerContext};
use crate::upstream_client;
use crate::{hardware, target_filter, tunnel};

/// Run the full application lifecycle after config has been parsed.
pub async fn run(mut config: Config, servers: Vec<ServerEntry>) -> anyhow::Result<()> {
    config.validate()?;
    init_tracing(&config);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        node_name = %config.node_name,
        server_count = servers.len(),
        "aether-proxy starting (tunnel mode)"
    );

    // Resolve public IP (best-effort for region info)
    let public_ip = match &config.public_ip {
        Some(ip) => ip.clone(),
        None => net::detect_public_ip()
            .await
            .unwrap_or_else(|_| "0.0.0.0".to_string()),
    };

    // Auto-detect region if not configured
    if config.node_region.is_none() {
        if let Some(region) = net::detect_region(&public_ip).await {
            config.node_region = Some(region);
        }
    }

    // Collect hardware info (once at startup, sent during registration)
    let hw_info = hardware::collect();

    // Auto-detect tunnel_max_streams from hardware if not explicitly set
    if config.tunnel_max_streams.is_none() {
        let auto = (hw_info.estimated_max_concurrency / 10).clamp(64, 1024) as u32;
        config.tunnel_max_streams = Some(auto);
        info!(
            tunnel_max_streams = auto,
            "auto-detected tunnel_max_streams from hardware"
        );
    }

    info!(
        max_concurrency = hw_info.estimated_max_concurrency,
        "hardware info collected"
    );

    let dns_cache = Arc::new(target_filter::DnsCache::new(
        Duration::from_secs(config.dns_cache_ttl_secs),
        config.dns_cache_capacity,
    ));

    // Build Hyper client for tunnel upstream requests (shared).
    // DNS still flows through validated addresses from DnsCache, while the
    // custom connector exposes per-request connect/TLS timing when available.
    let upstream_client = upstream_client::build_upstream_client(&config, Arc::clone(&dns_cache));

    // Register with each Aether server and build per-server contexts.
    // Wrapped in Arc<Mutex> so retry_failed_registrations can append later.
    let server_contexts: Arc<Mutex<Vec<Arc<ServerContext>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut failed_entries: Vec<(String, ServerEntry)> = Vec::new();
    for (i, entry) in servers.iter().enumerate() {
        let label = if servers.len() == 1 {
            "server".to_string()
        } else {
            format!("server-{}", i)
        };
        let node_name = entry
            .node_name
            .clone()
            .unwrap_or_else(|| config.node_name.clone());
        let client = Arc::new(AetherClient::new(
            &config,
            &entry.aether_url,
            &entry.management_token,
        ));
        match client
            .register(&config, &node_name, &public_ip, Some(&hw_info))
            .await
        {
            Ok(node_id) => {
                info!(server = %label, node_id = %node_id, url = %entry.aether_url, node_name = %node_name, "registered");
                // Initialize dynamic config with per-server node_name (not global),
                // so that the heartbeat and reconnect use the correct name.
                let mut dynamic = DynamicConfig::from_config(&config);
                dynamic.node_name = node_name.clone();
                server_contexts.lock().await.push(Arc::new(ServerContext {
                    server_label: label,
                    aether_url: entry.aether_url.clone(),
                    management_token: entry.management_token.clone(),
                    node_name,
                    node_id: Arc::new(RwLock::new(node_id)),
                    aether_client: client,
                    dynamic: Arc::new(ArcSwap::from_pointee(dynamic)),
                    active_connections: Arc::new(AtomicU64::new(0)),
                    metrics: Arc::new(ProxyMetrics::new()),
                }));
            }
            Err(e) => {
                warn!(
                    server = %label,
                    url = %entry.aether_url,
                    error = %e,
                    "registration failed, will retry in background"
                );
                failed_entries.push((label, entry.clone()));
            }
        }
    }

    {
        let ctx_count = server_contexts.lock().await.len();
        if ctx_count == 0 && failed_entries.is_empty() {
            anyhow::bail!("no servers configured");
        }
        if ctx_count == 0 {
            anyhow::bail!(
                "no servers registered successfully (all {} failed)",
                failed_entries.len()
            );
        }
    }

    // Build shared application state
    let tunnel_tls_config = Arc::new(crate::tunnel::client::build_tls_config());
    let state = Arc::new(AppState {
        config: Arc::new(config),
        dns_cache,
        upstream_client,
        tunnel_tls_config,
    });

    // Shutdown signal channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    info!(
        active_servers = server_contexts.lock().await.len(),
        "running in tunnel mode"
    );

    // Spawn tunnel connections per server (pool_size connections each)
    let pool_size = state.config.tunnel_connections.max(1) as usize;
    let mut tunnel_handles = Vec::new();
    for server in server_contexts.lock().await.iter() {
        for conn_idx in 0..pool_size {
            let s = Arc::clone(&state);
            let srv = Arc::clone(server);
            let rx = shutdown_rx.clone();
            tunnel_handles.push(tokio::spawn(async move {
                tunnel::run(&s, &srv, conn_idx, rx).await;
            }));
        }
    }

    // Spawn background retry for failed server registrations
    if !failed_entries.is_empty() {
        let retry_state = Arc::clone(&state);
        let retry_contexts = Arc::clone(&server_contexts);
        let retry_public_ip = public_ip.clone();
        let retry_hw_info = hw_info.clone();
        let retry_shutdown = shutdown_rx.clone();
        let retry_pool_size = pool_size;
        tokio::spawn(async move {
            retry_failed_registrations(
                retry_state,
                retry_contexts,
                failed_entries,
                retry_public_ip,
                retry_hw_info,
                retry_pool_size,
                retry_shutdown,
            )
            .await;
        });
    }

    // Wait for shutdown signal
    wait_for_shutdown().await;
    info!("shutdown signal received, cleaning up...");
    let _ = shutdown_tx.send(true);

    // Graceful unregister from all servers (including retry-registered ones)
    for server in server_contexts.lock().await.iter() {
        let node_id = server.node_id.read().unwrap().clone();
        if let Err(e) = server.aether_client.unregister(&node_id).await {
            error!(
                server = %server.server_label,
                error = %e,
                "unregister failed during shutdown"
            );
        }
    }

    // Wait for all tunnel tasks
    for h in tunnel_handles {
        let _ = h.await;
    }

    info!("aether-proxy stopped");
    Ok(())
}

/// Retry interval for failed server registrations (5 minutes).
const REGISTRATION_RETRY_INTERVAL: Duration = Duration::from_secs(300);
/// Max registration retry attempts before giving up.
const REGISTRATION_RETRY_MAX: u32 = 12;

/// Background task that retries registration for servers that failed at startup.
async fn retry_failed_registrations(
    state: Arc<AppState>,
    server_contexts: Arc<Mutex<Vec<Arc<ServerContext>>>>,
    failed: Vec<(String, ServerEntry)>,
    public_ip: String,
    hw_info: crate::hardware::HardwareInfo,
    pool_size: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    for (label, entry) in &failed {
        let node_name = entry
            .node_name
            .clone()
            .unwrap_or_else(|| state.config.node_name.clone());
        let client = Arc::new(AetherClient::new(
            &state.config,
            &entry.aether_url,
            &entry.management_token,
        ));

        let mut attempt = 0u32;
        let node_id = loop {
            attempt += 1;

            tokio::select! {
                _ = tokio::time::sleep(REGISTRATION_RETRY_INTERVAL) => {}
                _ = shutdown.changed() => {
                    info!(server = %label, "shutdown during registration retry");
                    return;
                }
            }

            match client
                .register(&state.config, &node_name, &public_ip, Some(&hw_info))
                .await
            {
                Ok(id) => {
                    info!(server = %label, node_id = %id, attempt, "registration retry succeeded");
                    break id;
                }
                Err(e) => {
                    warn!(
                        server = %label,
                        attempt,
                        max = REGISTRATION_RETRY_MAX,
                        error = %e,
                        "registration retry failed"
                    );
                    if attempt >= REGISTRATION_RETRY_MAX {
                        error!(server = %label, "giving up registration after {} attempts", attempt);
                        return;
                    }
                }
            }
        };

        // Build server context and spawn tunnels
        let mut dynamic = DynamicConfig::from_config(&state.config);
        dynamic.node_name = node_name.clone();
        let server = Arc::new(ServerContext {
            server_label: label.clone(),
            aether_url: entry.aether_url.clone(),
            management_token: entry.management_token.clone(),
            node_name,
            node_id: Arc::new(RwLock::new(node_id)),
            aether_client: client,
            dynamic: Arc::new(ArcSwap::from_pointee(dynamic)),
            active_connections: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(ProxyMetrics::new()),
        });

        // Add to shared list so shutdown can unregister this server
        server_contexts.lock().await.push(Arc::clone(&server));

        for conn_idx in 0..pool_size {
            let s = Arc::clone(&state);
            let srv = Arc::clone(&server);
            let rx = shutdown.clone();
            tokio::spawn(async move {
                tunnel::run(&s, &srv, conn_idx, rx).await;
            });
        }
    }
}

fn init_tracing(config: &Config) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{reload, EnvFilter};

    let filter = EnvFilter::try_new(&config.log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    let (filter_layer, reload_handle) = reload::Layer::new(filter);

    runtime::set_log_reloader(Box::new(move |level: &str| {
        if let Ok(new_filter) = EnvFilter::try_new(level) {
            let _ = reload_handle.modify(|f| *f = new_filter);
        }
    }));

    if config.log_json {
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }
}

async fn wait_for_shutdown() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
