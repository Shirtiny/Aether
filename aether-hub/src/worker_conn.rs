/// Worker-side WebSocket connection handler
///
/// Handles the lifecycle of a single Gunicorn worker connection:
/// accept -> read loop (route frames via Hub) -> cleanup
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::hub::{HubRouter, WorkerConn};
use crate::protocol;

pub async fn handle_worker_connection(
    ws: WebSocket,
    hub: Arc<HubRouter>,
    ping_interval: Duration,
    idle_timeout: Duration,
) {
    let conn_id = hub.alloc_conn_id();
    let (mut ws_tx, ws_rx) = ws.split();

    // Create channel for outbound messages
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    let conn = Arc::new(WorkerConn::new(conn_id, tx));
    hub.register_worker(conn.clone());

    // Spawn writer task
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
    let reader_tx = conn.tx.clone();
    let reader = tokio::spawn(async move {
        run_worker_reader(
            ws_rx,
            reader_hub,
            conn_id,
            conn.clone(),
            reader_tx,
            idle_timeout,
        )
        .await;
    });

    // Wait for reader to end, then cleanup.
    let _ = reader.await;
    ping_task.abort();
    // Unregister first so all channel senders are dropped (reader_tx dropped
    // when reader completes, ping_tx dropped by abort, conn.tx dropped when
    // the last Arc<WorkerConn> is removed from hub). This lets the writer
    // drain buffered messages (e.g. GOAWAY) before we force-abort it.
    hub.unregister_worker(conn_id);
    tokio::time::sleep(Duration::from_millis(100)).await;
    writer.abort();
    let _ = writer.await;
}

async fn run_worker_reader(
    mut ws_rx: futures_util::stream::SplitStream<WebSocket>,
    hub: Arc<HubRouter>,
    conn_id: u64,
    conn: Arc<WorkerConn>,
    tx: mpsc::UnboundedSender<Message>,
    idle_timeout: Duration,
) {
    loop {
        let msg = tokio::select! {
            msg = ws_rx.next() => msg,
            _ = tokio::time::sleep(idle_timeout) => {
                warn!(worker_id = conn_id, "worker idle timeout");
                let _ = tx.send(Message::Binary(protocol::encode_goaway().into()));
                break;
            }
        };

        match msg {
            Some(Ok(Message::Binary(data))) => {
                let mut data = data.to_vec();
                if data.len() < protocol::HEADER_SIZE {
                    debug!(worker_id = conn_id, "frame too small, skipping");
                    continue;
                }

                let header = match protocol::FrameHeader::parse(&data) {
                    Some(h) => h,
                    None => continue,
                };

                // HEARTBEAT_ACK from worker -> route back to proxy
                if header.msg_type == protocol::HEARTBEAT_ACK {
                    hub.handle_worker_heartbeat_ack(&mut data);
                    continue;
                }

                // Regular frames: route via hub
                if let Some(err_msg) = hub.handle_worker_frame(conn_id, &mut data) {
                    // Send STREAM_ERROR back to worker
                    let err_frame = protocol::encode_stream_error(header.stream_id, &err_msg);
                    let _ = conn.send(Message::Binary(err_frame.into()));
                }
            }
            Some(Ok(Message::Close(_))) | None => {
                info!(worker_id = conn_id, "worker WebSocket closed");
                break;
            }
            Some(Err(e)) => {
                warn!(worker_id = conn_id, error = %e, "worker WebSocket error");
                break;
            }
            _ => {} // Ignore text/ping/pong at WS level
        }
    }
}
