/// HubRouter -- central frame routing engine
///
/// Manages proxy connections (node_id -> [ProxyConn]) and worker connections (conn_id -> WorkerConn).
/// Routes frames between workers and proxies with stream_id remapping.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::Message;
use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStatus {
    Queued,
    Closed,
    Congested,
}

// ---------------------------------------------------------------------------
// Connection configuration (shared by proxy and worker handlers)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ConnConfig {
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    pub outbound_queue_capacity: usize,
}

// ---------------------------------------------------------------------------
// Bounded outbound channel with congestion-aware close
// ---------------------------------------------------------------------------

pub struct BoundedOutbound {
    tx: mpsc::Sender<Message>,
    close_tx: watch::Sender<bool>,
    closing: AtomicBool,
}

impl BoundedOutbound {
    pub fn new(tx: mpsc::Sender<Message>, close_tx: watch::Sender<bool>) -> Self {
        Self {
            tx,
            close_tx,
            closing: AtomicBool::new(false),
        }
    }

    pub fn send(&self, msg: Message) -> SendStatus {
        if self.is_closing() {
            return SendStatus::Closed;
        }

        match self.tx.try_send(msg) {
            Ok(()) => SendStatus::Queued,
            Err(TrySendError::Closed(_)) => {
                self.mark_closing();
                SendStatus::Closed
            }
            Err(TrySendError::Full(_)) => {
                self.mark_closing();
                SendStatus::Congested
            }
        }
    }

    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// Mark as closing. Returns `true` if this call was the first to flip the flag.
    pub fn mark_closing(&self) -> bool {
        if self.closing.swap(true, Ordering::AcqRel) {
            return false;
        }
        let _ = self.close_tx.send(true);
        true
    }
}

// ---------------------------------------------------------------------------
// Proxy connection
// ---------------------------------------------------------------------------

pub struct ProxyConn {
    pub id: u64,
    pub node_id: String,
    pub node_name: String,
    pub outbound: BoundedOutbound,
    next_stream_id: AtomicU32,
    pub stream_count: AtomicUsize,
    pub max_streams: usize,
}

impl ProxyConn {
    pub fn new(
        id: u64,
        node_id: String,
        node_name: String,
        tx: mpsc::Sender<Message>,
        close_tx: watch::Sender<bool>,
        max_streams: usize,
    ) -> Self {
        Self {
            id,
            node_id,
            node_name,
            outbound: BoundedOutbound::new(tx, close_tx),
            next_stream_id: AtomicU32::new(2), // even IDs, start at 2
            stream_count: AtomicUsize::new(0),
            max_streams,
        }
    }

    /// Allocate a proxy-side stream_id (even numbers)
    pub fn alloc_stream_id(&self) -> Option<u32> {
        // Reserve one stream slot first (CAS to honor max_streams under contention).
        let mut current = self.stream_count.load(Ordering::Relaxed);
        loop {
            if current >= self.max_streams || !self.is_available() {
                return None;
            }
            match self.stream_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }

        let sid = loop {
            let current_sid = self.next_stream_id.load(Ordering::Relaxed);
            let next_sid = if current_sid >= 0xFFFF_FFFE {
                2
            } else {
                current_sid + 2
            };
            if self
                .next_stream_id
                .compare_exchange_weak(current_sid, next_sid, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                break current_sid;
            }
        };

        Some(sid)
    }

    pub fn release_stream(&self) {
        let mut current = self.stream_count.load(Ordering::Relaxed);
        while current > 0 {
            match self.stream_count.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    pub fn is_available(&self) -> bool {
        !self.outbound.is_closing()
    }

    pub fn request_close(&self) {
        self.outbound.mark_closing();
    }

    pub fn send(&self, msg: Message) -> SendStatus {
        let was_closing = self.outbound.is_closing();
        let status = self.outbound.send(msg);
        if status == SendStatus::Congested && !was_closing {
            warn!(
                conn_id = self.id,
                node_id = %self.node_id,
                node_name = %self.node_name,
                queued_streams = self.stream_count.load(Ordering::Relaxed),
                "proxy outbound queue full, closing congested connection"
            );
        }
        status
    }
}

// ---------------------------------------------------------------------------
// Worker connection
// ---------------------------------------------------------------------------

pub struct WorkerConn {
    pub id: u64,
    pub outbound: BoundedOutbound,
}

impl WorkerConn {
    pub fn new(id: u64, tx: mpsc::Sender<Message>, close_tx: watch::Sender<bool>) -> Self {
        Self {
            id,
            outbound: BoundedOutbound::new(tx, close_tx),
        }
    }

    pub fn is_available(&self) -> bool {
        !self.outbound.is_closing()
    }

    pub fn request_close(&self) {
        self.outbound.mark_closing();
    }

    pub fn send(&self, msg: Message) -> SendStatus {
        let was_closing = self.outbound.is_closing();
        let status = self.outbound.send(msg);
        if status == SendStatus::Congested && !was_closing {
            warn!(
                worker_id = self.id,
                "worker outbound queue full, closing congested connection"
            );
        }
        status
    }
}

// ---------------------------------------------------------------------------
// Stream mapping entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ProxySide {
    proxy_conn_id: u64,
    proxy_stream_id: u32,
}

#[derive(Debug, Clone, Copy)]
struct WorkerSide {
    worker_conn_id: u64,
    worker_stream_id: u32,
}

// ---------------------------------------------------------------------------
// HubRouter
// ---------------------------------------------------------------------------

pub struct HubRouter {
    /// node_id -> list of proxy connections
    proxy_conns: RwLock<std::collections::HashMap<String, Vec<Arc<ProxyConn>>>>,
    /// proxy_conn_id -> Arc<ProxyConn> (for reverse lookup)
    proxy_conns_by_id: DashMap<u64, Arc<ProxyConn>>,
    /// worker_conn_id -> Arc<WorkerConn>
    worker_conns: DashMap<u64, Arc<WorkerConn>>,
    /// (worker_conn_id, worker_stream_id) -> ProxySide
    worker_to_proxy: DashMap<(u64, u32), ProxySide>,
    /// (proxy_conn_id, proxy_stream_id) -> WorkerSide
    proxy_to_worker: DashMap<(u64, u32), WorkerSide>,
    /// Connection ID generator
    next_conn_id: AtomicU64,
    /// Round-robin counter for heartbeat forwarding
    heartbeat_rr: AtomicU64,
    /// Heartbeat tag -> proxy_conn_id mapping (u32 tag fits in stream_id field)
    heartbeat_tags: DashMap<u32, u64>,
    /// Next heartbeat tag (wrapping u32)
    next_heartbeat_tag: AtomicU32,
}

impl HubRouter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            proxy_conns: RwLock::new(std::collections::HashMap::new()),
            proxy_conns_by_id: DashMap::new(),
            worker_conns: DashMap::new(),
            worker_to_proxy: DashMap::new(),
            proxy_to_worker: DashMap::new(),
            next_conn_id: AtomicU64::new(1),
            heartbeat_rr: AtomicU64::new(0),
            heartbeat_tags: DashMap::new(),
            next_heartbeat_tag: AtomicU32::new(1),
        })
    }

    pub fn alloc_conn_id(&self) -> u64 {
        self.next_conn_id.fetch_add(1, Ordering::Relaxed)
    }

    // -----------------------------------------------------------------------
    // Proxy connection management
    // -----------------------------------------------------------------------

    pub fn register_proxy(&self, conn: Arc<ProxyConn>) {
        let node_id = conn.node_id.clone();
        let node_name = conn.node_name.clone();
        let conn_id = conn.id;
        self.proxy_conns_by_id.insert(conn_id, conn.clone());

        let mut map = self.proxy_conns.write();
        map.entry(node_id.clone()).or_default().push(conn);
        let pool_size = map.get(&node_id).map(|v| v.len()).unwrap_or(0);

        info!(
            node_id = %node_id,
            node_name = %node_name,
            conn_id = conn_id,
            pool_size = pool_size,
            "proxy connected"
        );

        drop(map);
        self.broadcast_node_status(&node_id);
    }

    pub fn unregister_proxy(&self, conn_id: u64, node_id: &str) {
        self.proxy_conns_by_id.remove(&conn_id);

        let mut map = self.proxy_conns.write();
        if let Some(conns) = map.get_mut(node_id) {
            conns.retain(|c| c.id != conn_id);
            if conns.is_empty() {
                map.remove(node_id);
            }
        }
        let pool_size = map.get(node_id).map(|v| v.len()).unwrap_or(0);

        info!(
            node_id = %node_id,
            conn_id = conn_id,
            remaining = pool_size,
            "proxy disconnected"
        );

        drop(map);

        // Cancel all in-flight streams on this proxy connection
        self.cancel_streams_for_proxy(conn_id);

        self.broadcast_node_status(node_id);
    }

    /// Get least-loaded proxy connection for a node
    fn get_proxy_conn(&self, node_id: &str) -> Option<Arc<ProxyConn>> {
        let map = self.proxy_conns.read();
        let conns = map.get(node_id)?;
        conns
            .iter()
            .filter(|c| c.is_available())
            .min_by_key(|c| c.stream_count.load(Ordering::Relaxed))
            .cloned()
    }

    /// Get pool size for a node
    fn proxy_conn_count(&self, node_id: &str) -> usize {
        let map = self.proxy_conns.read();
        map.get(node_id).map(|v| v.len()).unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Worker connection management
    // -----------------------------------------------------------------------

    pub fn register_worker(&self, conn: Arc<WorkerConn>) {
        info!(worker_id = conn.id, "worker connected");
        self.worker_conns.insert(conn.id, conn.clone());
        self.sync_node_status_to_worker(&conn);
    }

    pub fn unregister_worker(&self, conn_id: u64) {
        self.worker_conns.remove(&conn_id);
        info!(worker_id = conn_id, "worker disconnected");

        // Clean up all stream mappings for this worker
        let to_remove: Vec<(u64, u32)> = self
            .worker_to_proxy
            .iter()
            .filter(|e| e.key().0 == conn_id)
            .map(|e| *e.key())
            .collect();

        for key in &to_remove {
            if let Some((_, proxy_side)) = self.worker_to_proxy.remove(key) {
                self.proxy_to_worker
                    .remove(&(proxy_side.proxy_conn_id, proxy_side.proxy_stream_id));
                // Release stream count on proxy side
                if let Some(pc) = self.proxy_conns_by_id.get(&proxy_side.proxy_conn_id) {
                    pc.release_stream();
                }
            }
        }

        if !to_remove.is_empty() {
            debug!(
                worker_id = conn_id,
                streams_cleaned = to_remove.len(),
                "cleaned up worker streams"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Frame routing: Worker -> Proxy
    // -----------------------------------------------------------------------

    /// Handle a frame from a worker. Returns error message if routing fails.
    pub fn handle_worker_frame(&self, worker_conn_id: u64, data: &mut [u8]) -> Option<String> {
        let header = match protocol::FrameHeader::parse(data) {
            Some(h) => h,
            None => return Some("invalid frame".to_string()),
        };
        let expected_len = protocol::HEADER_SIZE + header.payload_len as usize;
        if data.len() < expected_len {
            return Some("incomplete frame payload".to_string());
        }

        match header.msg_type {
            protocol::REQUEST_HEADERS => {
                self.route_request_headers(worker_conn_id, header.stream_id, data)
            }
            protocol::REQUEST_BODY => {
                if header.flags & protocol::FLAG_END_STREAM != 0 {
                    debug!(
                        worker_conn_id = worker_conn_id,
                        stream_id = header.stream_id,
                        "worker sent REQUEST_BODY with END_STREAM"
                    );
                }
                self.route_worker_to_proxy(worker_conn_id, header.stream_id, data, false);
                None
            }
            protocol::STREAM_END | protocol::STREAM_ERROR => {
                self.route_worker_to_proxy(worker_conn_id, header.stream_id, data, true);
                None
            }
            protocol::GOAWAY => {
                warn!(
                    worker_conn_id = worker_conn_id,
                    "received GOAWAY from worker connection"
                );
                None
            }
            protocol::PING => {
                let payload = protocol::frame_payload(data).to_vec();
                let pong = protocol::encode_pong(&payload);
                if let Some(wc) = self.worker_conns.get(&worker_conn_id) {
                    let _ = wc.send(Message::Binary(pong.into()));
                }
                None
            }
            protocol::PONG => None, // Worker responded to our ping, nothing to do
            _ => {
                debug!(
                    msg_type = header.msg_type,
                    "unexpected frame type from worker"
                );
                None
            }
        }
    }

    /// Route REQUEST_HEADERS: extract node_id, allocate proxy stream, create mapping
    fn route_request_headers(
        &self,
        worker_conn_id: u64,
        worker_stream_id: u32,
        data: &mut [u8],
    ) -> Option<String> {
        // Parse payload to extract node_id, and pre-build frame with node_id stripped.
        // stream_id is set to 0 first; we'll rewrite to proxy_stream_id after allocation.
        let extracted = match protocol::rebuild_request_headers_without_node_id(data, 0) {
            Ok(v) => v,
            Err(e) => return Some(e),
        };
        let node_id = extracted.node_id;

        // Find a proxy connection for this node
        let proxy_conn = match self.get_proxy_conn(&node_id) {
            Some(c) => c,
            None => {
                return Some(format!("no proxy connection for node {}", node_id));
            }
        };

        // Allocate proxy-side stream_id
        let proxy_stream_id = match proxy_conn.alloc_stream_id() {
            Some(sid) => sid,
            None => {
                return Some(format!("stream limit reached for node {}", node_id));
            }
        };

        let mut rebuilt_frame = extracted.rebuilt_frame;
        protocol::rewrite_stream_id(&mut rebuilt_frame, proxy_stream_id);

        // Record bidirectional mapping
        self.worker_to_proxy.insert(
            (worker_conn_id, worker_stream_id),
            ProxySide {
                proxy_conn_id: proxy_conn.id,
                proxy_stream_id,
            },
        );
        self.proxy_to_worker.insert(
            (proxy_conn.id, proxy_stream_id),
            WorkerSide {
                worker_conn_id,
                worker_stream_id,
            },
        );

        match proxy_conn.send(Message::Binary(rebuilt_frame.into())) {
            SendStatus::Queued => {}
            SendStatus::Closed | SendStatus::Congested => {
                // Send failed, clean up mapping
                self.worker_to_proxy
                    .remove(&(worker_conn_id, worker_stream_id));
                self.proxy_to_worker
                    .remove(&(proxy_conn.id, proxy_stream_id));
                proxy_conn.release_stream();
                return Some("proxy connection congested".to_string());
            }
        }

        None
    }

    /// Route non-header frames from worker to proxy (REQUEST_BODY etc.)
    fn route_worker_to_proxy(
        &self,
        worker_conn_id: u64,
        worker_stream_id: u32,
        data: &mut [u8],
        terminal: bool,
    ) {
        let proxy_side = if terminal {
            match self
                .worker_to_proxy
                .remove(&(worker_conn_id, worker_stream_id))
            {
                Some((_, ps)) => {
                    self.proxy_to_worker
                        .remove(&(ps.proxy_conn_id, ps.proxy_stream_id));
                    if let Some(pc) = self.proxy_conns_by_id.get(&ps.proxy_conn_id) {
                        pc.release_stream();
                    }
                    ps
                }
                None => return, // Silently discard -- mapping already removed (race condition)
            }
        } else {
            match self
                .worker_to_proxy
                .get(&(worker_conn_id, worker_stream_id))
            {
                Some(entry) => *entry.value(),
                None => return, // Silently discard -- mapping already removed (race condition)
            }
        };

        // Rewrite stream_id
        protocol::rewrite_stream_id(data, proxy_side.proxy_stream_id);

        if let Some(pc) = self.proxy_conns_by_id.get(&proxy_side.proxy_conn_id) {
            let _ = pc.send(Message::Binary(data.to_vec().into()));
        }
    }

    // -----------------------------------------------------------------------
    // Frame routing: Proxy -> Worker
    // -----------------------------------------------------------------------

    /// Handle a frame from a proxy connection
    pub fn handle_proxy_frame(&self, proxy_conn_id: u64, data: &mut [u8]) {
        let header = match protocol::FrameHeader::parse(data) {
            Some(h) => h,
            None => return,
        };
        let expected_len = protocol::HEADER_SIZE + header.payload_len as usize;
        if data.len() < expected_len {
            return;
        }

        match header.msg_type {
            protocol::RESPONSE_HEADERS | protocol::RESPONSE_BODY => {
                self.route_proxy_to_worker(proxy_conn_id, header.stream_id, data, false);
            }
            _ if header.is_stream_terminal() => {
                self.route_proxy_to_worker(proxy_conn_id, header.stream_id, data, true);
            }
            protocol::HEARTBEAT_DATA => {
                self.forward_heartbeat_to_worker(proxy_conn_id, data);
            }
            protocol::PONG => {} // Proxy responded to our ping
            protocol::GOAWAY => {
                warn!(
                    proxy_conn_id = proxy_conn_id,
                    "received GOAWAY from proxy connection"
                );
            }
            protocol::PING => {
                // Proxy sent a ping, reply with pong
                let payload = if data.len() > protocol::HEADER_SIZE {
                    &data[protocol::HEADER_SIZE..]
                } else {
                    &[]
                };
                let pong = protocol::encode_pong(payload);
                if let Some(pc) = self.proxy_conns_by_id.get(&proxy_conn_id) {
                    let _ = pc.send(Message::Binary(pong.into()));
                }
            }
            _ => {
                debug!(
                    msg_type = header.msg_type,
                    proxy_conn_id = proxy_conn_id,
                    "unexpected frame type from proxy"
                );
            }
        }
    }

    /// Route response frames from proxy to worker
    fn route_proxy_to_worker(
        &self,
        proxy_conn_id: u64,
        proxy_stream_id: u32,
        data: &mut [u8],
        terminal: bool,
    ) {
        let worker_side = if terminal {
            // Remove mapping on terminal frames
            match self
                .proxy_to_worker
                .remove(&(proxy_conn_id, proxy_stream_id))
            {
                Some((_, ws)) => {
                    self.worker_to_proxy
                        .remove(&(ws.worker_conn_id, ws.worker_stream_id));
                    // Release stream count
                    if let Some(pc) = self.proxy_conns_by_id.get(&proxy_conn_id) {
                        pc.release_stream();
                    }
                    ws
                }
                None => return, // Silently discard
            }
        } else {
            match self.proxy_to_worker.get(&(proxy_conn_id, proxy_stream_id)) {
                Some(entry) => *entry.value(),
                None => return, // Silently discard
            }
        };

        // Rewrite stream_id to worker-side
        protocol::rewrite_stream_id(data, worker_side.worker_stream_id);

        if let Some(wc) = self.worker_conns.get(&worker_side.worker_conn_id) {
            let _ = wc.send(Message::Binary(data.to_vec().into()));
        }
    }

    /// Forward HEARTBEAT_DATA to a worker (round-robin)
    fn forward_heartbeat_to_worker(&self, proxy_conn_id: u64, data: &[u8]) {
        // Pick a worker via round-robin
        let workers: Vec<Arc<WorkerConn>> = self
            .worker_conns
            .iter()
            .filter_map(|e| {
                let worker = e.value().clone();
                worker.is_available().then_some(worker)
            })
            .collect();
        if workers.is_empty() {
            debug!("no workers to forward heartbeat to");
            return;
        }
        let idx = self.heartbeat_rr.fetch_add(1, Ordering::Relaxed) as usize % workers.len();
        let worker = &workers[idx];

        // Use a u32 tag in the stream_id field to identify the proxy connection.
        // The tag maps to the full u64 proxy_conn_id via heartbeat_tags DashMap,
        // avoiding truncation of u64 conn_id to u32.
        // Skip 0 (reserved for control frames) via CAS loop.
        let tag = loop {
            let t = self.next_heartbeat_tag.fetch_add(1, Ordering::Relaxed);
            if t != 0 {
                break t;
            }
        };
        self.heartbeat_tags.insert(tag, proxy_conn_id);

        let mut forwarded = data.to_vec();
        protocol::rewrite_stream_id(&mut forwarded, tag);

        let _ = worker.send(Message::Binary(forwarded.into()));
    }

    /// Handle HEARTBEAT_ACK from worker -- route back to the proxy
    pub fn handle_worker_heartbeat_ack(&self, data: &mut [u8]) {
        let header = match protocol::FrameHeader::parse(data) {
            Some(h) => h,
            None => return,
        };

        // Recover the original proxy_conn_id from the tag stored in stream_id
        let tag = header.stream_id;
        let proxy_conn_id = match self.heartbeat_tags.remove(&tag) {
            Some((_, id)) => id,
            None => return,
        };

        // Reset stream_id to 0 before forwarding to proxy
        protocol::rewrite_stream_id(data, 0);

        if let Some(pc) = self.proxy_conns_by_id.get(&proxy_conn_id) {
            let _ = pc.send(Message::Binary(data.to_vec().into()));
        }
    }

    // -----------------------------------------------------------------------
    // Stream cleanup
    // -----------------------------------------------------------------------

    /// Cancel all in-flight streams for a disconnected proxy connection
    fn cancel_streams_for_proxy(&self, proxy_conn_id: u64) {
        let to_remove: Vec<((u64, u32), WorkerSide)> = self
            .proxy_to_worker
            .iter()
            .filter(|e| e.key().0 == proxy_conn_id)
            .map(|e| (*e.key(), *e.value()))
            .collect();

        for ((p_conn_id, p_sid), worker_side) in &to_remove {
            self.proxy_to_worker.remove(&(*p_conn_id, *p_sid));
            self.worker_to_proxy
                .remove(&(worker_side.worker_conn_id, worker_side.worker_stream_id));

            // Send STREAM_ERROR to worker
            let err_frame =
                protocol::encode_stream_error(worker_side.worker_stream_id, "proxy disconnected");
            if let Some(wc) = self.worker_conns.get(&worker_side.worker_conn_id) {
                let _ = wc.send(Message::Binary(err_frame.into()));
            }
        }

        if !to_remove.is_empty() {
            warn!(
                proxy_conn_id = proxy_conn_id,
                streams_cancelled = to_remove.len(),
                "cancelled in-flight streams due to proxy disconnect"
            );
        }
    }

    // -----------------------------------------------------------------------
    // NODE_STATUS broadcast
    // -----------------------------------------------------------------------

    fn broadcast_node_status(&self, node_id: &str) {
        let conn_count = self.proxy_conn_count(node_id);
        let connected = conn_count > 0;
        let frame = protocol::encode_node_status(node_id, connected, conn_count);
        let msg = Message::Binary(frame.into());

        let mut sent = 0usize;
        for entry in self.worker_conns.iter() {
            if matches!(entry.value().send(msg.clone()), SendStatus::Queued) {
                sent += 1;
            }
        }

        debug!(
            node_id = %node_id,
            connected = connected,
            conn_count = conn_count,
            workers_notified = sent,
            "broadcast NODE_STATUS"
        );
    }

    /// When a worker connects, sync all current node statuses so worker state
    /// is consistent even if proxies connected before this worker came online.
    fn sync_node_status_to_worker(&self, worker: &Arc<WorkerConn>) {
        let snapshot: Vec<(String, usize)> = {
            let map = self.proxy_conns.read();
            map.iter()
                .map(|(node_id, conns)| (node_id.clone(), conns.len()))
                .collect()
        };

        for (node_id, conn_count) in &snapshot {
            let frame = protocol::encode_node_status(node_id, *conn_count > 0, *conn_count);
            let _ = worker.send(Message::Binary(frame.into()));
        }

        debug!(
            worker_id = worker.id,
            nodes_synced = snapshot.len(),
            "synced NODE_STATUS snapshot to worker"
        );
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    pub fn stats(&self) -> HubStats {
        let proxy_conns = self.proxy_conns.read();
        let total_proxy = proxy_conns.values().map(|v| v.len()).sum();
        let nodes = proxy_conns.len();
        drop(proxy_conns);

        HubStats {
            proxy_connections: total_proxy,
            worker_connections: self.worker_conns.len(),
            nodes,
            active_streams: self.worker_to_proxy.len(),
        }
    }
}

#[derive(serde::Serialize)]
pub struct HubStats {
    pub proxy_connections: usize,
    pub worker_connections: usize,
    pub nodes: usize,
    pub active_streams: usize,
}
