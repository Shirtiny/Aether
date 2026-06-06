//! Frame dispatcher: reads incoming WebSocket frames and routes them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::state::{AppState, ServerContext};

use super::heartbeat::HeartbeatHandle;
use super::protocol::{decompress_if_gzip, Frame, MsgType, RequestMeta};
use super::stream_handler;
use super::stream_handler::StreamSendWindow;
use super::writer::FrameSender;
use aether_contracts::tunnel_security::SecureFrameCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamDispatchStatus {
    Delivered,
    Closed,
    TimedOut,
}

#[derive(Clone)]
struct StreamDispatchTarget {
    body_tx: mpsc::Sender<Frame>,
    response_window: Arc<StreamSendWindow>,
}

/// Run the dispatcher loop, reading from the WebSocket stream.
#[allow(dead_code)]
pub async fn run<S>(
    state: Arc<AppState>,
    server: Arc<ServerContext>,
    ws_stream: S,
    frame_tx: FrameSender,
    heartbeat: HeartbeatHandle,
    drain: watch::Receiver<bool>,
) -> Result<(), anyhow::Error>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin
        + Send
        + 'static,
{
    run_with_security(state, server, ws_stream, frame_tx, heartbeat, drain, None).await
}

pub async fn run_with_security<S>(
    state: Arc<AppState>,
    server: Arc<ServerContext>,
    mut ws_stream: S,
    frame_tx: FrameSender,
    heartbeat: HeartbeatHandle,
    mut drain: watch::Receiver<bool>,
    security: Option<Arc<SecureFrameCodec>>,
) -> Result<(), anyhow::Error>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin
        + Send
        + 'static,
{
    // Active streams: stream_id -> body sender + response flow-control window.
    let mut streams: HashMap<u32, StreamDispatchTarget> = HashMap::new();
    // Track spawned stream handlers so we can wait for them on shutdown
    let mut handler_handles: Vec<JoinHandle<()>> = Vec::new();
    let (handler_finished_tx, mut handler_finished_rx) = mpsc::unbounded_channel::<u32>();
    let max_streams = state.config.tunnel_max_streams.unwrap_or(128) as usize;
    let mut frames_since_cleanup: u32 = 0;
    let stale_timeout = state
        .config
        .tunnel_stale_timeout()
        .expect("validated config should resolve tunnel stale timeout");

    // Track last time we received any data to detect stale connections
    let mut last_data_at = tokio::time::Instant::now();
    let mut draining = *drain.borrow();

    let read_err = loop {
        if draining && streams.is_empty() {
            info!("tunnel drained after in-flight streams completed");
            break None;
        }

        let msg_result = tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(r) => r,
                    None => break None,
                }
            }
            changed = drain.changed() => {
                if changed.is_err() {
                    continue;
                }
                if *drain.borrow() {
                    info!("tunnel drain requested, waiting for in-flight streams");
                    draining = true;
                }
                continue;
            }
            finished = handler_finished_rx.recv() => {
                if let Some(stream_id) = finished {
                    streams.remove(&stream_id);
                    if draining && streams.is_empty() {
                        info!("tunnel drained after stream handler completion");
                        break None;
                    }
                }
                continue;
            }
            _ = tokio::time::sleep_until(last_data_at + stale_timeout) => {
                warn!(
                    stale_ms = stale_timeout.as_millis(),
                    "tunnel connection stale, no data received"
                );
                server.tunnel_metrics.record_error(
                    "stale_timeout",
                    &format!("no tunnel frame received for {}ms", stale_timeout.as_millis()),
                );
                break None;
            }
        };

        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                error!(error = %e, "WebSocket read error");
                server
                    .tunnel_metrics
                    .record_error("ws_read_error", &e.to_string());
                break Some(e);
            }
        };

        // Any successfully received message proves the connection is alive
        last_data_at = tokio::time::Instant::now();

        let data = match msg {
            Message::Binary(data) => {
                server.tunnel_metrics.record_ws_incoming_frame(data.len());
                Bytes::from(data)
            }
            Message::Ping(_) => continue,
            Message::Pong(_) => continue,
            Message::Close(_) => {
                debug!("received WebSocket close");
                break None;
            }
            _ => continue,
        };

        let frame = match Frame::decode(data) {
            Ok(f) => f,
            Err(e) => {
                warn!(error = %e, "failed to decode frame");
                server
                    .tunnel_metrics
                    .record_error("frame_decode_error", &e.to_string());
                continue;
            }
        };
        let frame = match security.as_deref() {
            Some(codec) => match codec.decrypt_frame(frame) {
                Ok(frame) => frame,
                Err(e) => {
                    warn!(error = %e, "failed to decrypt secure tunnel frame");
                    server
                        .tunnel_metrics
                        .record_error("secure_frame_decrypt_error", &e.to_string());
                    break None;
                }
            },
            None => frame,
        };

        match frame.msg_type {
            MsgType::RequestHeaders => {
                if draining {
                    if frame_tx
                        .try_send(Frame::new(
                            frame.stream_id,
                            MsgType::StreamError,
                            0,
                            Bytes::from("tunnel draining"),
                        ))
                        .is_err()
                    {
                        warn!(
                            stream_id = frame.stream_id,
                            "writer channel full, StreamError dropped during drain"
                        );
                    }
                    continue;
                }

                // Decompress if the frame is gzip-compressed, then parse metadata
                let payload = match decompress_if_gzip(&frame) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(stream_id = frame.stream_id, error = %e, "frame decompress failed");
                        continue;
                    }
                };
                let meta: RequestMeta = match serde_json::from_slice(&payload) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(stream_id = frame.stream_id, error = %e, "invalid request metadata");
                        // Use try_send to avoid blocking the read loop
                        if frame_tx
                            .try_send(Frame::new(
                                frame.stream_id,
                                MsgType::StreamError,
                                0,
                                Bytes::from(format!("invalid request metadata: {e}")),
                            ))
                            .is_err()
                        {
                            warn!(
                                stream_id = frame.stream_id,
                                "writer channel full, StreamError dropped"
                            );
                        }
                        continue;
                    }
                };

                if streams.len() >= max_streams {
                    warn!(
                        stream_id = frame.stream_id,
                        "max concurrent streams reached"
                    );
                    if frame_tx
                        .try_send(Frame::new(
                            frame.stream_id,
                            MsgType::StreamError,
                            0,
                            Bytes::from("max concurrent streams reached"),
                        ))
                        .is_err()
                    {
                        warn!(
                            stream_id = frame.stream_id,
                            "writer channel full, StreamError dropped"
                        );
                    }
                    continue;
                }

                // Create body channel and spawn handler
                let (body_tx, body_rx) = mpsc::channel::<Frame>(64);
                let response_window = Arc::new(StreamSendWindow::new(
                    state.config.tunnel_stream_initial_window_bytes,
                ));
                streams.insert(
                    frame.stream_id,
                    StreamDispatchTarget {
                        body_tx,
                        response_window: Arc::clone(&response_window),
                    },
                );
                let request_headers_end_stream = frame.is_end_stream();

                let state_clone = Arc::clone(&state);
                let server_clone = Arc::clone(&server);
                let tx_clone = frame_tx.clone();
                let finished_tx = handler_finished_tx.clone();
                let sid = frame.stream_id;
                let handle = tokio::spawn(async move {
                    stream_handler::handle_stream(
                        state_clone,
                        server_clone,
                        sid,
                        meta,
                        body_rx,
                        tx_clone,
                        response_window,
                    )
                    .await;
                    let _ = finished_tx.send(sid);
                });
                handler_handles.push(handle);

                if request_headers_end_stream {
                    if let Some(target) = streams.get(&sid) {
                        let _ = target.body_tx.try_send(Frame::new(
                            sid,
                            MsgType::StreamEnd,
                            0,
                            Bytes::new(),
                        ));
                    }
                }

                debug!(stream_id = frame.stream_id, "new stream started");
            }

            MsgType::RequestBody => {
                if let Some(target) = streams.get(&frame.stream_id).cloned() {
                    let is_end = frame.is_end_stream();
                    let sid = frame.stream_id;
                    let dispatch = dispatch_stream_frame(&target.body_tx, frame).await;
                    if dispatch != StreamDispatchStatus::Delivered {
                        streams.remove(&sid);
                        if dispatch == StreamDispatchStatus::TimedOut {
                            server.tunnel_metrics.record_error(
                                "stream_dispatch_timeout",
                                &format!("request body dispatch timed out for stream {}", sid),
                            );
                            try_send_stream_error(
                                &frame_tx,
                                sid,
                                "tunnel request body dispatch stalled",
                            );
                        }
                        if is_end && draining && streams.is_empty() {
                            info!("tunnel drained after request body completion");
                            break None;
                        }
                    }
                }
            }

            MsgType::StreamEnd | MsgType::StreamError | MsgType::ResetStream => {
                // Client-side cancellation or end
                if let Some(target) = streams.remove(&frame.stream_id) {
                    let _ = dispatch_stream_frame(&target.body_tx, frame).await;
                    if draining && streams.is_empty() {
                        info!("tunnel drained after stream termination");
                        break None;
                    }
                }
            }

            MsgType::Ping => {
                // Use try_send to avoid blocking the read loop when writer is congested
                if frame_tx
                    .try_send(Frame::control(MsgType::Pong, frame.payload))
                    .is_err()
                {
                    warn!("writer channel full, Pong dropped");
                }
            }

            MsgType::HeartbeatAck => {
                heartbeat.on_ack(frame.payload).await;
            }

            MsgType::GoAway => {
                info!("received GOAWAY");
                break None;
            }

            MsgType::WindowUpdate => {
                if let Ok(payload) = serde_json::from_slice::<
                    aether_contracts::tunnel::WindowUpdatePayload,
                >(&frame.payload)
                {
                    if let Some(target) = streams.get(&frame.stream_id) {
                        target.response_window.add_credit(payload.delta_bytes);
                    }
                }
                debug!(
                    msg_type = ?frame.msg_type,
                    stream_id = frame.stream_id,
                    "received tunnel protocol v3 WINDOW_UPDATE frame"
                );
            }

            MsgType::Hello | MsgType::Settings | MsgType::LoadReport => {
                debug!(
                    msg_type = ?frame.msg_type,
                    stream_id = frame.stream_id,
                    "received tunnel protocol v3 control frame"
                );
            }

            MsgType::ConnectionClose => {
                info!("received CONNECTION_CLOSE");
                break None;
            }

            _ => {
                debug!(msg_type = ?frame.msg_type, "ignoring unexpected frame type");
            }
        }

        // Periodically clean up finished handles to avoid unbounded growth.
        // Trigger every 64 frames OR when the count exceeds max_streams.
        frames_since_cleanup += 1;
        if frames_since_cleanup >= 64 || handler_handles.len() > max_streams {
            handler_handles.retain(|h| !h.is_finished());
            frames_since_cleanup = 0;
            if draining && streams.is_empty() {
                info!("tunnel drained after cleanup");
                break None;
            }
        }
    };

    // Drop body senders so stream handlers waiting on body_rx will unblock
    streams.clear();

    // Wait for active stream handlers to finish so their frame_tx clones
    // are dropped before the writer closes the sink.
    drain_handlers(handler_handles).await;

    match read_err {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

async fn dispatch_stream_frame(tx: &mpsc::Sender<Frame>, frame: Frame) -> StreamDispatchStatus {
    let stream_id = frame.stream_id;
    match tokio::time::timeout(stream_frame_dispatch_timeout(), tx.send(frame)).await {
        Ok(Ok(())) => StreamDispatchStatus::Delivered,
        Ok(Err(_)) => {
            warn!(
                stream_id,
                "stream handler channel closed while dispatching tunnel frame"
            );
            StreamDispatchStatus::Closed
        }
        Err(_) => {
            warn!(
                stream_id,
                timeout_ms = stream_frame_dispatch_timeout().as_millis(),
                "stream handler channel blocked while dispatching tunnel frame"
            );
            StreamDispatchStatus::TimedOut
        }
    }
}

/// Bound how long a single stream handler is allowed to block the shared
/// WebSocket read loop while receiving request-body frames.
fn stream_frame_dispatch_timeout() -> Duration {
    #[cfg(test)]
    {
        Duration::from_millis(25)
    }

    #[cfg(not(test))]
    {
        Duration::from_millis(500)
    }
}

fn try_send_stream_error(frame_tx: &FrameSender, stream_id: u32, message: &'static str) {
    if frame_tx
        .try_send(Frame::new(
            stream_id,
            MsgType::StreamError,
            0,
            Bytes::from(message),
        ))
        .is_err()
    {
        warn!(
            stream_id,
            "writer channel full, StreamError dropped while aborting stalled stream"
        );
    }
}

#[cfg(test)]
fn prune_closed_stream_senders(streams: &mut HashMap<u32, StreamDispatchTarget>) -> usize {
    let before = streams.len();
    streams.retain(|_, target| !target.body_tx.is_closed());
    before.saturating_sub(streams.len())
}

/// Wait for all active stream handlers to finish (with a timeout).
async fn drain_handlers(handles: Vec<JoinHandle<()>>) {
    if handles.is_empty() {
        return;
    }
    let count = handles.len();
    debug!(count, "waiting for active stream handlers to finish");
    let _ = tokio::time::timeout(Duration::from_secs(30), async {
        for h in handles {
            let _ = h.await;
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_runtime::bounded_queue;

    #[tokio::test]
    async fn dispatch_stream_frame_times_out_when_handler_stops_draining() {
        let (tx, mut rx) = mpsc::channel::<Frame>(1);
        tx.send(Frame::new(
            7,
            MsgType::RequestBody,
            0,
            Bytes::from_static(b"first"),
        ))
        .await
        .expect("first frame should enqueue");

        let stalled_send = tokio::spawn({
            let tx = tx.clone();
            async move {
                dispatch_stream_frame(
                    &tx,
                    Frame::new(7, MsgType::RequestBody, 0, Bytes::from_static(b"second")),
                )
                .await
            }
        });

        assert_eq!(
            stalled_send.await.expect("dispatch task should join"),
            StreamDispatchStatus::TimedOut
        );

        let retained = rx
            .recv()
            .await
            .expect("queued frame should still be present");
        assert_eq!(retained.payload, Bytes::from_static(b"first"));
    }

    #[tokio::test]
    async fn try_send_stream_error_emits_stream_error_frame() {
        let (high_tx, mut high_rx) = bounded_queue::<Frame>(4);
        let (normal_tx, _normal_rx) = bounded_queue::<Frame>(4);
        let frame_tx = FrameSender::from_test_queues(high_tx, normal_tx);
        try_send_stream_error(&frame_tx, 9, "tunnel request body dispatch stalled");

        let frame = high_rx
            .recv()
            .await
            .expect("stream error frame should enqueue");
        assert_eq!(frame.stream_id, 9);
        assert_eq!(frame.msg_type, MsgType::StreamError);
        assert_eq!(
            frame.payload,
            Bytes::from_static(b"tunnel request body dispatch stalled")
        );
    }

    #[test]
    fn prune_closed_stream_senders_drops_streams_with_closed_receivers() {
        let (closed_tx, closed_rx) = mpsc::channel::<Frame>(1);
        let (open_tx, _open_rx) = mpsc::channel::<Frame>(1);
        drop(closed_rx);
        let mut streams = HashMap::from([
            (
                7,
                StreamDispatchTarget {
                    body_tx: closed_tx,
                    response_window: Arc::new(StreamSendWindow::new(1024)),
                },
            ),
            (
                9,
                StreamDispatchTarget {
                    body_tx: open_tx,
                    response_window: Arc::new(StreamSendWindow::new(1024)),
                },
            ),
        ]);

        let removed = prune_closed_stream_senders(&mut streams);

        assert_eq!(removed, 1);
        assert!(!streams.contains_key(&7));
        assert!(streams.contains_key(&9));
    }
}
