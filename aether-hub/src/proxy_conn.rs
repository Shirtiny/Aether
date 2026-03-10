/// Proxy-side WebSocket connection handler
///
/// Handles the lifecycle of a single aether-proxy connection:
/// accept -> authenticate (headers) -> read loop -> cleanup
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::hub::{HubRouter, ProxyConn};
use crate::protocol;

/// Maximum single frame size: 64 MB
const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

pub async fn handle_proxy_connection(
    ws: WebSocket,
    hub: Arc<HubRouter>,
    node_id: String,
    node_name: String,
    max_streams: usize,
    ping_interval: Duration,
    idle_timeout: Duration,
) {
    let conn_id = hub.alloc_conn_id();
    let (mut ws_tx, ws_rx) = ws.split();

    // Create channel for outbound messages
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    let conn = Arc::new(ProxyConn::new(
        conn_id,
        node_id.clone(),
        node_name.clone(),
        tx,
        max_streams,
    ));

    hub.register_proxy(conn.clone());

    // Spawn writer task: drains channel -> WebSocket
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.close().await;
    });

    // Spawn ping task
    let ping_tx = conn.tx.clone();
    let ping_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(ping_interval).await;
            let ping = protocol::encode_ping();
            if ping_tx.send(Message::Binary(ping.into())).is_err() {
                break;
            }
        }
    });

    // Spawn reader task
    let reader_hub = hub.clone();
    let reader_node_id = node_id.clone();
    let reader_tx = conn.tx.clone();
    let reader = tokio::spawn(async move {
        run_proxy_reader(
            ws_rx,
            reader_hub,
            conn_id,
            reader_node_id,
            reader_tx,
            idle_timeout,
        )
        .await;
    });

    // Wait for reader to end, then cleanup.
    let _ = reader.await;
    ping_task.abort();
    hub.unregister_proxy(conn_id, &node_id);
    // conn still holds an Arc<ProxyConn> with a channel sender clone.
    // Drop it so the writer can drain and exit.
    drop(conn);
    tokio::time::sleep(Duration::from_millis(100)).await;
    writer.abort();
    let _ = writer.await;
}

async fn run_proxy_reader(
    mut ws_rx: futures_util::stream::SplitStream<WebSocket>,
    hub: Arc<HubRouter>,
    conn_id: u64,
    node_id: String,
    tx: mpsc::UnboundedSender<Message>,
    idle_timeout: Duration,
) {
    let mut oversized_count = 0u32;
    loop {
        let msg = tokio::select! {
            msg = ws_rx.next() => msg,
            _ = tokio::time::sleep(idle_timeout) => {
                warn!(conn_id = conn_id, node_id = %node_id, "proxy idle timeout");
                let _ = tx.send(Message::Binary(protocol::encode_goaway().into()));
                break;
            }
        };

        match msg {
            Some(Ok(Message::Binary(data))) => {
                let mut data = data.to_vec();
                if data.len() > MAX_FRAME_SIZE {
                    oversized_count += 1;
                    warn!(
                        conn_id = conn_id,
                        size = data.len(),
                        "oversized frame from proxy"
                    );
                    if oversized_count >= 5 {
                        warn!(conn_id = conn_id, "too many oversized frames, closing");
                        break;
                    }
                    continue;
                }
                oversized_count = 0;

                if data.len() < protocol::HEADER_SIZE {
                    debug!(conn_id = conn_id, "frame too small, skipping");
                    continue;
                }

                hub.handle_proxy_frame(conn_id, &mut data);
            }
            Some(Ok(Message::Close(_))) | None => {
                info!(conn_id = conn_id, node_id = %node_id, "proxy WebSocket closed");
                break;
            }
            Some(Err(e)) => {
                warn!(conn_id = conn_id, error = %e, "proxy WebSocket error");
                break;
            }
            _ => {} // Ignore text/ping/pong at WS level
        }
    }
}
