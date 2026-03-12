/// Worker-side WebSocket connection handler
///
/// Handles the lifecycle of a single Gunicorn worker connection:
/// accept -> read loop (route frames via Hub) -> cleanup
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::hub::{ConnConfig, HubRouter, SendStatus, WorkerConn};
use crate::protocol;

pub async fn handle_worker_connection(ws: WebSocket, hub: Arc<HubRouter>, cfg: ConnConfig) {
    let conn_id = hub.alloc_conn_id();
    let (mut ws_tx, ws_rx) = ws.split();

    let (tx, mut rx) = mpsc::channel::<Message>(cfg.outbound_queue_capacity);
    let (close_tx, mut close_rx) = watch::channel(false);

    let conn = Arc::new(WorkerConn::new(conn_id, tx, close_tx));
    hub.register_worker(conn.clone());

    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(msg) => {
                        if ws_tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                changed = close_rx.changed() => {
                    if changed.is_err() || *close_rx.borrow() {
                        break;
                    }
                }
            }
        }
        let _ = ws_tx.close().await;
    });

    let liveness_clock = Instant::now();
    let last_seen_ms = Arc::new(AtomicU64::new(0));

    let reader_hub = hub.clone();
    let reader_conn = conn.clone();
    let reader_last_seen_ms = last_seen_ms.clone();
    let liveness_conn = conn.clone();
    let mut reader = tokio::spawn(async move {
        run_worker_reader(
            ws_rx,
            reader_hub,
            conn_id,
            reader_conn,
            reader_last_seen_ms,
            liveness_clock,
        )
        .await;
    });

    let liveness_last_seen_ms = last_seen_ms.clone();
    let mut liveness = tokio::spawn(async move {
        run_worker_liveness(
            conn_id,
            liveness_conn,
            cfg.ping_interval,
            cfg.idle_timeout,
            liveness_last_seen_ms,
            liveness_clock,
        )
        .await;
    });

    let reader_finished = tokio::select! {
        res = &mut reader => {
            if let Err(err) = res {
                warn!(worker_id = conn_id, error = %err, "worker reader task failed");
            }
            true
        }
        res = &mut liveness => {
            if let Err(err) = res {
                warn!(worker_id = conn_id, error = %err, "worker liveness task failed");
            }
            false
        }
    };

    conn.request_close();
    if !reader_finished {
        reader.abort();
        let _ = reader.await;
    }
    if reader_finished {
        liveness.abort();
        let _ = liveness.await;
    }

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
    last_seen_ms: Arc<AtomicU64>,
    liveness_clock: Instant,
) {
    loop {
        match ws_rx.next().await {
            Some(Ok(Message::Binary(data))) => {
                last_seen_ms.store(elapsed_millis(liveness_clock), Ordering::Relaxed);

                let mut data = data.to_vec();
                if data.len() < protocol::HEADER_SIZE {
                    debug!(worker_id = conn_id, "frame too small, skipping");
                    continue;
                }

                let header = match protocol::FrameHeader::parse(&data) {
                    Some(h) => h,
                    None => continue,
                };

                if header.msg_type == protocol::HEARTBEAT_ACK {
                    hub.handle_worker_heartbeat_ack(&mut data);
                    continue;
                }

                if let Some(err_msg) = hub.handle_worker_frame(conn_id, &mut data) {
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
            _ => {}
        }
    }
}

async fn run_worker_liveness(
    conn_id: u64,
    conn: Arc<WorkerConn>,
    ping_interval: Duration,
    idle_timeout: Duration,
    last_seen_ms: Arc<AtomicU64>,
    liveness_clock: Instant,
) {
    let ping_interval_ms = ping_interval.as_millis().max(1) as u64;
    let idle_timeout_ms = idle_timeout.as_millis() as u64;

    loop {
        tokio::time::sleep(ping_interval).await;

        let ping = protocol::encode_ping();
        if !matches!(conn.send(Message::Binary(ping.into())), SendStatus::Queued) {
            break;
        }

        if idle_timeout.is_zero() {
            continue;
        }

        let now_ms = elapsed_millis(liveness_clock);
        let last_seen = last_seen_ms.load(Ordering::Relaxed);
        let silent_for_ms = now_ms.saturating_sub(last_seen);
        if silent_for_ms < idle_timeout_ms {
            continue;
        }

        let missed_heartbeats = (silent_for_ms / ping_interval_ms).max(1);
        warn!(
            worker_id = conn_id,
            idle_timeout_secs = idle_timeout.as_secs(),
            silent_for_ms = silent_for_ms,
            missed_heartbeats = missed_heartbeats,
            "worker heartbeat timeout"
        );
        let _ = conn.send(Message::Binary(protocol::encode_goaway().into()));
        conn.request_close();
        break;
    }
}

fn elapsed_millis(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
}
