//! Shared application state passed to all subsystems.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_runtime::{AdmissionPermit, ConcurrencyError, ConcurrencyGate, ConcurrencySnapshot};
use aether_runtime_state::{RuntimeSemaphore, RuntimeSemaphoreError, RuntimeSemaphoreSnapshot};

use crate::config::Config;
use crate::hardware::RuntimeResourceMonitor;
use crate::registration::client::AetherClient;
use crate::runtime::SharedDynamicConfig;
use crate::target_filter::DnsCache;
use crate::upstream_client::UpstreamClientPool;

/// Central application state shared across all servers/tunnels.
pub struct AppState {
    pub config: Arc<Config>,
    /// DNS cache for upstream target resolution (shared).
    pub dns_cache: Arc<DnsCache>,
    /// Profile-keyed upstream client pool used by tunnel requests.
    pub upstream_client_pool: UpstreamClientPool,
    /// Shared TLS config for tunnel WebSocket connections (avoids re-parsing root CAs on each reconnect).
    pub tunnel_tls_config: Arc<rustls::ClientConfig>,
    /// Runtime CPU/memory monitor sampled by heartbeat payloads.
    pub resource_monitor: Arc<RuntimeResourceMonitor>,
    /// Optional per-process stream admission gate.
    pub stream_gate: Option<Arc<ConcurrencyGate>>,
    /// Optional cross-instance stream admission gate.
    pub distributed_stream_gate: Option<Arc<RuntimeSemaphore>>,
}

/// Per-server state: one instance per Aether server connection.
pub struct ServerContext {
    /// Human-readable label for logging (e.g. "server-0").
    pub server_label: String,
    /// Aether server URL for this connection.
    pub aether_url: String,
    /// Management token for this server.
    pub management_token: String,
    /// Resolved node name at registration time (per-server override or global fallback).
    /// After startup, the active node_name is read from `dynamic` (may be updated remotely).
    #[allow(dead_code)]
    pub node_name: String,
    /// Node ID assigned by this Aether server.
    pub node_id: Arc<RwLock<String>>,
    /// API client for this server.
    pub aether_client: Arc<AetherClient>,
    /// Dynamic config from this server's heartbeat ACKs.
    pub dynamic: SharedDynamicConfig,
    /// Per-server active connection count.
    pub active_connections: Arc<AtomicU64>,
    /// Per-server request/latency metrics.
    pub metrics: Arc<ProxyMetrics>,
    /// Per-server tunnel stability/traffic metrics.
    pub tunnel_metrics: Arc<TunnelMetrics>,
}

/// Aggregate metrics for reporting to Aether.
pub struct ProxyMetrics {
    pub total_requests: AtomicU64,
    /// Cumulative connection-establishment latency in nanoseconds
    /// (DNS + TCP/TLS + TTFB, excludes response body streaming).
    pub total_latency_ns: AtomicU64,
    pub failed_requests: AtomicU64,
    pub dns_failures: AtomicU64,
    pub stream_errors: AtomicU64,
}

impl ProxyMetrics {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            dns_failures: AtomicU64::new(0),
            stream_errors: AtomicU64::new(0),
        }
    }

    /// Record a completed request with its connection-establishment latency
    /// (DNS + TCP/TLS + TTFB, excludes response body streaming).
    pub fn record_request(&self, connect_elapsed: Duration) {
        let nanos = u64::try_from(connect_elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.total_requests.fetch_add(1, Ordering::Release);
        self.total_latency_ns.fetch_add(nanos, Ordering::Release);
    }
}

const RECENT_TUNNEL_ERROR_CAPACITY: usize = 64;
const TUNNEL_ERROR_CATEGORY_MAX_CHARS: usize = 48;
const TUNNEL_ERROR_MESSAGE_MAX_CHARS: usize = 320;

#[derive(Debug, Clone, serde::Serialize)]
pub struct TunnelErrorEvent {
    pub timestamp_unix_secs: u64,
    pub timestamp_unix_ms: u64,
    pub category: String,
    pub message: String,
    pub severity: String,
    pub component: String,
    pub summary: String,
    pub operator_action: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TunnelMetricsSnapshot {
    pub connect_attempts: u64,
    pub connect_successes: u64,
    pub connect_errors: u64,
    pub disconnects: u64,
    pub last_connected_at_unix_secs: u64,
    pub last_disconnected_at_unix_secs: u64,
    pub last_connected_duration_ms: u64,
    pub connected_duration_total_ms: u64,
    pub heartbeat_sent: u64,
    pub heartbeat_ack: u64,
    pub heartbeat_rtt_last_ms: u64,
    pub heartbeat_rtt_total_ms: u64,
    pub ws_in_frames: u64,
    pub ws_in_bytes: u64,
    pub ws_out_frames: u64,
    pub ws_out_bytes: u64,
    pub error_events_total: u64,
}

impl TunnelMetricsSnapshot {
    pub fn heartbeat_rtt_avg_ms(self) -> Option<f64> {
        if self.heartbeat_ack == 0 {
            None
        } else {
            Some(self.heartbeat_rtt_total_ms as f64 / self.heartbeat_ack as f64)
        }
    }
}

pub struct TunnelMetrics {
    connect_attempts: AtomicU64,
    connect_successes: AtomicU64,
    connect_errors: AtomicU64,
    disconnects: AtomicU64,
    last_connected_at_unix_secs: AtomicU64,
    last_disconnected_at_unix_secs: AtomicU64,
    last_connected_duration_ms: AtomicU64,
    connected_duration_total_ms: AtomicU64,
    heartbeat_sent: AtomicU64,
    heartbeat_ack: AtomicU64,
    heartbeat_rtt_last_ms: AtomicU64,
    heartbeat_rtt_total_ms: AtomicU64,
    ws_in_frames: AtomicU64,
    ws_in_bytes: AtomicU64,
    ws_out_frames: AtomicU64,
    ws_out_bytes: AtomicU64,
    error_events_total: AtomicU64,
    recent_errors: Mutex<VecDeque<TunnelErrorEvent>>,
}

impl TunnelMetrics {
    pub fn new() -> Self {
        Self {
            connect_attempts: AtomicU64::new(0),
            connect_successes: AtomicU64::new(0),
            connect_errors: AtomicU64::new(0),
            disconnects: AtomicU64::new(0),
            last_connected_at_unix_secs: AtomicU64::new(0),
            last_disconnected_at_unix_secs: AtomicU64::new(0),
            last_connected_duration_ms: AtomicU64::new(0),
            connected_duration_total_ms: AtomicU64::new(0),
            heartbeat_sent: AtomicU64::new(0),
            heartbeat_ack: AtomicU64::new(0),
            heartbeat_rtt_last_ms: AtomicU64::new(0),
            heartbeat_rtt_total_ms: AtomicU64::new(0),
            ws_in_frames: AtomicU64::new(0),
            ws_in_bytes: AtomicU64::new(0),
            ws_out_frames: AtomicU64::new(0),
            ws_out_bytes: AtomicU64::new(0),
            error_events_total: AtomicU64::new(0),
            recent_errors: Mutex::new(VecDeque::with_capacity(RECENT_TUNNEL_ERROR_CAPACITY)),
        }
    }

    pub fn record_connect_attempt(&self) {
        self.connect_attempts.fetch_add(1, Ordering::Release);
    }

    pub fn record_connect_success(&self) {
        self.connect_successes.fetch_add(1, Ordering::Release);
        self.last_connected_at_unix_secs
            .store(now_unix_secs(), Ordering::Release);
    }

    pub fn record_connect_error(&self) {
        self.connect_errors.fetch_add(1, Ordering::Release);
    }

    pub fn record_disconnect(&self, connected_for: Duration) {
        let duration_ms = duration_to_millis_u64(connected_for);
        self.disconnects.fetch_add(1, Ordering::Release);
        self.last_disconnected_at_unix_secs
            .store(now_unix_secs(), Ordering::Release);
        self.last_connected_duration_ms
            .store(duration_ms, Ordering::Release);
        self.connected_duration_total_ms
            .fetch_add(duration_ms, Ordering::Release);
    }

    pub fn record_heartbeat_sent(&self) {
        self.heartbeat_sent.fetch_add(1, Ordering::Release);
    }

    pub fn record_heartbeat_ack(&self, rtt: Duration) {
        let rtt_ms = duration_to_millis_u64(rtt);
        self.heartbeat_ack.fetch_add(1, Ordering::Release);
        self.heartbeat_rtt_last_ms.store(rtt_ms, Ordering::Release);
        self.heartbeat_rtt_total_ms
            .fetch_add(rtt_ms, Ordering::Release);
    }

    pub fn record_ws_incoming_frame(&self, payload_len: usize) {
        self.ws_in_frames.fetch_add(1, Ordering::Release);
        self.ws_in_bytes.fetch_add(
            u64::try_from(payload_len).unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }

    pub fn record_ws_outgoing_frame(&self, payload_len: usize) {
        self.ws_out_frames.fetch_add(1, Ordering::Release);
        self.ws_out_bytes.fetch_add(
            u64::try_from(payload_len).unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }

    pub fn record_error(&self, category: &str, message: &str) {
        self.error_events_total.fetch_add(1, Ordering::Release);
        let category = normalize_error_field(category, TUNNEL_ERROR_CATEGORY_MAX_CHARS, "unknown");
        let message = normalize_error_field(message, TUNNEL_ERROR_MESSAGE_MAX_CHARS, "n/a");
        let diagnostic = classify_tunnel_error(category.as_str(), message.as_str());

        let timestamp_unix_ms = now_unix_ms();
        let event = TunnelErrorEvent {
            timestamp_unix_secs: timestamp_unix_ms / 1_000,
            timestamp_unix_ms,
            category,
            message,
            severity: diagnostic.severity.to_string(),
            component: diagnostic.component.to_string(),
            summary: diagnostic.summary.to_string(),
            operator_action: diagnostic.operator_action.to_string(),
        };

        let mut recent_errors = match self.recent_errors.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if recent_errors.len() >= RECENT_TUNNEL_ERROR_CAPACITY {
            recent_errors.pop_front();
        }
        recent_errors.push_back(event);
    }

    pub fn recent_errors(&self, limit: usize) -> Vec<TunnelErrorEvent> {
        if limit == 0 {
            return Vec::new();
        }
        let recent_errors = match self.recent_errors.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let start = recent_errors.len().saturating_sub(limit);
        recent_errors.iter().skip(start).cloned().collect()
    }

    pub fn snapshot(&self) -> TunnelMetricsSnapshot {
        TunnelMetricsSnapshot {
            connect_attempts: self.connect_attempts.load(Ordering::Acquire),
            connect_successes: self.connect_successes.load(Ordering::Acquire),
            connect_errors: self.connect_errors.load(Ordering::Acquire),
            disconnects: self.disconnects.load(Ordering::Acquire),
            last_connected_at_unix_secs: self.last_connected_at_unix_secs.load(Ordering::Acquire),
            last_disconnected_at_unix_secs: self
                .last_disconnected_at_unix_secs
                .load(Ordering::Acquire),
            last_connected_duration_ms: self.last_connected_duration_ms.load(Ordering::Acquire),
            connected_duration_total_ms: self.connected_duration_total_ms.load(Ordering::Acquire),
            heartbeat_sent: self.heartbeat_sent.load(Ordering::Acquire),
            heartbeat_ack: self.heartbeat_ack.load(Ordering::Acquire),
            heartbeat_rtt_last_ms: self.heartbeat_rtt_last_ms.load(Ordering::Acquire),
            heartbeat_rtt_total_ms: self.heartbeat_rtt_total_ms.load(Ordering::Acquire),
            ws_in_frames: self.ws_in_frames.load(Ordering::Acquire),
            ws_in_bytes: self.ws_in_bytes.load(Ordering::Acquire),
            ws_out_frames: self.ws_out_frames.load(Ordering::Acquire),
            ws_out_bytes: self.ws_out_bytes.load(Ordering::Acquire),
            error_events_total: self.error_events_total.load(Ordering::Acquire),
        }
    }
}

fn now_unix_secs() -> u64 {
    now_unix_ms() / 1_000
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn duration_to_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn normalize_error_field(value: &str, max_chars: usize, fallback: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return fallback.to_string();
    }
    normalized.chars().take(max_chars).collect()
}

struct TunnelErrorDiagnostic {
    severity: &'static str,
    component: &'static str,
    summary: &'static str,
    operator_action: &'static str,
}

fn classify_tunnel_error(category: &str, _message: &str) -> TunnelErrorDiagnostic {
    match category {
        "stale_timeout" => TunnelErrorDiagnostic {
            severity: "warning",
            component: "tunnel_read",
            summary: "No inbound tunnel frames before stale timeout",
            operator_action:
                "Check gateway or reverse-proxy idle timeouts, packet loss, and WebSocket ping/pong reachability. Increase AETHER_PROXY_TUNNEL_STALE_TIMEOUT_MS if the network is high-latency.",
        },
        "ws_write_error" => TunnelErrorDiagnostic {
            severity: "error",
            component: "tunnel_write",
            summary: "WebSocket write failed because the peer closed or reset the connection",
            operator_action:
                "Check gateway restarts, load balancer resets, NAT/firewall connection tracking, and whether the proxy is reconnecting successfully.",
        },
        "ws_ping_error" => TunnelErrorDiagnostic {
            severity: "error",
            component: "tunnel_write",
            summary: "WebSocket keepalive ping could not be sent",
            operator_action:
                "Check whether the peer closed the socket or an intermediary is dropping idle WebSocket connections.",
        },
        "ws_read_error" => TunnelErrorDiagnostic {
            severity: "error",
            component: "tunnel_read",
            summary: "WebSocket read failed",
            operator_action:
                "Check gateway logs and network stability around the same timestamp; compare with reconnect and heartbeat ACK counters.",
        },
        "tunnel_connect_error" => TunnelErrorDiagnostic {
            severity: "critical",
            component: "tunnel_connect",
            summary: "Tunnel connection attempt failed",
            operator_action:
                "Check Aether URL reachability, DNS, TLS, management token validity, and any configured AETHER_PROXY_AETHER_PROXY_URL.",
        },
        "frame_decode_error" => TunnelErrorDiagnostic {
            severity: "error",
            component: "tunnel_protocol",
            summary: "Received tunnel frame could not be decoded",
            operator_action:
                "Check proxy and gateway version compatibility and whether traffic is being modified by an intermediary.",
        },
        "stream_dispatch_timeout" => TunnelErrorDiagnostic {
            severity: "warning",
            component: "stream_dispatch",
            summary: "Request body frame could not be delivered to its stream handler in time",
            operator_action:
                "Check proxy CPU, memory, stream concurrency saturation, and slow upstream provider requests.",
        },
        "heartbeat_ack_empty" | "heartbeat_ack_parse" => TunnelErrorDiagnostic {
            severity: "warning",
            component: "heartbeat",
            summary: "Heartbeat ACK from gateway was missing or invalid",
            operator_action:
                "Check gateway heartbeat handler logs and proxy/gateway version compatibility.",
        },
        "writer_task_panic" | "writer_task_cancelled" => TunnelErrorDiagnostic {
            severity: "error",
            component: "tunnel_writer",
            summary: "Tunnel writer task exited unexpectedly",
            operator_action:
                "Check proxy logs for the preceding write or ping error and confirm the tunnel reconnect loop is active.",
        },
        "dispatcher_error" => TunnelErrorDiagnostic {
            severity: "error",
            component: "tunnel_dispatcher",
            summary: "Tunnel dispatcher exited with an error",
            operator_action:
                "Check the proxied request stream and gateway tunnel logs around the same timestamp.",
        },
        _ => TunnelErrorDiagnostic {
            severity: "info",
            component: "tunnel",
            summary: "Tunnel reported an unclassified error",
            operator_action:
                "Inspect the raw message and compare it with proxy, gateway, and network logs at the same time.",
        },
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProxyAdmissionError {
    #[error("proxy stream admission saturated at {limit} for gate {gate}")]
    Saturated { gate: &'static str, limit: usize },
    #[error("proxy stream admission unavailable for gate {gate}: {message}")]
    Unavailable {
        gate: &'static str,
        limit: usize,
        message: String,
    },
}

impl AppState {
    pub fn with_stream_concurrency_gate(mut self, gate: Arc<ConcurrencyGate>) -> Self {
        self.stream_gate = Some(gate);
        self
    }

    pub fn with_distributed_stream_concurrency_gate(mut self, gate: Arc<RuntimeSemaphore>) -> Self {
        self.distributed_stream_gate = Some(gate);
        self
    }

    pub fn stream_concurrency_snapshot(&self) -> Option<ConcurrencySnapshot> {
        self.stream_gate.as_ref().map(|gate| gate.snapshot())
    }

    pub async fn distributed_stream_concurrency_snapshot(
        &self,
    ) -> Result<Option<RuntimeSemaphoreSnapshot>, RuntimeSemaphoreError> {
        match &self.distributed_stream_gate {
            Some(gate) => gate.snapshot().await.map(Some),
            None => Ok(None),
        }
    }

    pub async fn try_acquire_stream_permit(
        &self,
    ) -> Result<Option<AdmissionPermit>, ProxyAdmissionError> {
        let local = match &self.stream_gate {
            Some(gate) => Some(gate.try_acquire().map_err(|err| {
                match err {
                    ConcurrencyError::Saturated { gate, limit } => {
                        ProxyAdmissionError::Saturated { gate, limit }
                    }
                    ConcurrencyError::Closed { gate } => ProxyAdmissionError::Unavailable {
                        gate,
                        limit: self
                            .stream_gate
                            .as_ref()
                            .map(|inner| inner.snapshot().limit)
                            .unwrap_or(0),
                        message: "local stream gate is closed".to_string(),
                    },
                }
            })?),
            None => None,
        };

        let distributed = match &self.distributed_stream_gate {
            Some(gate) => Some(gate.try_acquire().await.map_err(|err| {
                match err {
                    RuntimeSemaphoreError::Saturated { gate, limit } => {
                        ProxyAdmissionError::Saturated { gate, limit }
                    }
                    RuntimeSemaphoreError::Unavailable {
                        gate,
                        limit,
                        message,
                    } => ProxyAdmissionError::Unavailable {
                        gate,
                        limit,
                        message,
                    },
                    RuntimeSemaphoreError::InvalidConfiguration(message) => {
                        ProxyAdmissionError::Unavailable {
                            gate: "proxy_streams_distributed",
                            limit: self
                                .distributed_stream_gate
                                .as_ref()
                                .map(|inner| inner.limit())
                                .unwrap_or(0),
                            message,
                        }
                    }
                }
            })?),
            None => None,
        };

        Ok(AdmissionPermit::from_parts(local, distributed))
    }
}
