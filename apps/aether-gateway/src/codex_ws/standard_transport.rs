//! Standard OpenAI-compatible Responses WebSocket transport.
//!
//! The existing route-v1 session consumes a poll-based `RelayPeer`, while
//! `wreq` exposes async `send`/`recv` methods. A small bounded bridge owns the
//! physical socket and preserves backpressure without creating another
//! Responses session implementation.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{Sink, Stream};
use http::header::{
    ACCEPT, ACCEPT_ENCODING, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HOST,
    PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use http::{HeaderMap, HeaderName, StatusCode, Version};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::PollSender;
use url::Url;
use wreq::ws::message::Message as WreqWsMessage;

use super::protocol::MAX_PUBLIC_CLIENT_PAYLOAD_BYTES;
use super::runtime::{PeerError, RelayFrame};
use crate::execution_runtime::transport::{
    build_browser_wreq_client, build_request_headers, ExecutionTransportControls,
};

const BRIDGE_CHANNEL_CAPACITY: usize = 8;
const MAX_HANDSHAKE_ERROR_BODY_BYTES: usize = 8 * 1024;

pub(crate) struct StandardWebSocketConnection {
    pub(crate) peer: StandardPeer,
    pub(crate) response_headers: BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) enum StandardWebSocketConnectError {
    Rejected {
        status_code: u16,
        response_headers: BTreeMap<String, String>,
        error_body: Option<String>,
    },
    Transport(PeerError),
}

pub(crate) async fn connect_standard_websocket(
    plan: &aether_contracts::ExecutionPlan,
) -> Result<StandardWebSocketConnection, StandardWebSocketConnectError> {
    let upstream_url =
        websocket_upstream_url(&plan.url).map_err(StandardWebSocketConnectError::Transport)?;
    let headers = websocket_handshake_headers(&plan.headers)
        .map_err(StandardWebSocketConnectError::Transport)?;
    let client = build_websocket_client(plan).map_err(StandardWebSocketConnectError::Transport)?;
    let mut response = client
        .websocket(upstream_url.as_str())
        .headers(headers)
        .max_frame_size(MAX_PUBLIC_CLIENT_PAYLOAD_BYTES)
        .max_message_size(MAX_PUBLIC_CLIENT_PAYLOAD_BYTES)
        .send()
        .await
        .map_err(|error| {
            StandardWebSocketConnectError::Transport(PeerError(format!(
                "standard Responses WebSocket handshake failed ({})",
                wreq_error_kind(&error)
            )))
        })?;
    if !is_websocket_upgrade_response(response.status(), response.version()) {
        let status_code = response.status().as_u16();
        let response_headers = compact_response_headers(response.headers());
        let error_body = read_handshake_error_body(&mut response).await;
        return Err(StandardWebSocketConnectError::Rejected {
            status_code,
            response_headers,
            error_body,
        });
    }
    let response_headers = compact_response_headers(response.headers());
    let socket = response.into_websocket().await.map_err(|error| {
        StandardWebSocketConnectError::Transport(PeerError(format!(
            "standard Responses WebSocket upgrade failed ({})",
            wreq_error_kind(&error)
        )))
    })?;
    Ok(StandardWebSocketConnection {
        peer: StandardPeer::spawn(socket),
        response_headers,
    })
}

fn wreq_error_kind(error: &wreq::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connection_reset() {
        "connection_reset"
    } else if error.is_connect() {
        "connect"
    } else if error.is_builder() {
        "builder"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_request() {
        "request"
    } else {
        "transport"
    }
}

fn is_websocket_upgrade_response(status: StatusCode, version: Version) -> bool {
    status == StatusCode::SWITCHING_PROTOCOLS
        || (version == Version::HTTP_2 && status == StatusCode::OK)
}

async fn read_handshake_error_body(response: &mut wreq::ws::WebSocketResponse) -> Option<String> {
    let mut body = Vec::new();
    while body.len() < MAX_HANDSHAKE_ERROR_BODY_BYTES {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) | Err(_) => break,
        };
        let remaining = MAX_HANDSHAKE_ERROR_BODY_BYTES - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    (!body.is_empty()).then(|| String::from_utf8_lossy(&body).into_owned())
}

pub(crate) fn websocket_upstream_url(raw: &str) -> Result<Url, PeerError> {
    let mut url = Url::parse(raw)
        .map_err(|_| PeerError("standard Responses WebSocket URL is invalid".into()))?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(PeerError(
            "standard Responses WebSocket URL is invalid".into(),
        ));
    }
    let websocket_scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        "wss" | "ws" => return Ok(url),
        _ => {
            return Err(PeerError(
                "standard Responses WebSocket URL is invalid".into(),
            ))
        }
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| PeerError("standard Responses WebSocket URL is invalid".into()))?;
    Ok(url)
}

pub(crate) fn websocket_handshake_headers(
    provider_headers: &BTreeMap<String, String>,
) -> Result<HeaderMap, PeerError> {
    let connection_scoped_names = provider_headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(CONNECTION.as_str()))
        .flat_map(|(_, value)| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    let mut headers = build_request_headers(provider_headers, None, false)
        .map_err(|_| PeerError("standard Responses WebSocket headers are invalid".into()))?;
    for name in connection_scoped_names {
        headers.remove(name);
    }
    for header in [
        ACCEPT,
        ACCEPT_ENCODING,
        CONNECTION,
        CONTENT_ENCODING,
        CONTENT_LENGTH,
        CONTENT_TYPE,
        HOST,
        PROXY_AUTHORIZATION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(header);
    }
    for header in ["keep-alive", "proxy-connection"] {
        headers.remove(header);
    }
    let websocket_managed_names = headers
        .keys()
        .filter(|name| name.as_str().starts_with("sec-websocket-"))
        .cloned()
        .collect::<Vec<_>>();
    for name in websocket_managed_names {
        headers.remove(name);
    }
    Ok(headers)
}

fn build_websocket_client(
    plan: &aether_contracts::ExecutionPlan,
) -> Result<wreq::Client, PeerError> {
    let timeouts = websocket_transport_timeouts(plan);
    if let Some(profile) = plan.transport_profile.as_ref() {
        return build_browser_wreq_client(
            timeouts.as_ref(),
            plan.proxy.as_ref(),
            profile,
            ExecutionTransportControls::default(),
            false,
        )
        .map_err(|_| PeerError("standard Responses WebSocket client build failed".into()));
    }

    let mut builder = wreq::Client::builder();
    if let Some(connect_ms) = timeouts.as_ref().and_then(|timeouts| timeouts.connect_ms) {
        builder = builder.connect_timeout(Duration::from_millis(connect_ms));
    }
    if let Some(proxy) = plan
        .proxy
        .as_ref()
        .filter(|proxy| proxy.enabled != Some(false))
    {
        if let Some(proxy_url) = proxy
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            let proxy = wreq::Proxy::all(proxy_url)
                .map_err(|_| PeerError("standard Responses WebSocket proxy is invalid".into()))?;
            builder = builder.proxy(proxy);
        } else if proxy.node_id.is_some() || proxy.mode.as_deref() == Some("tunnel") {
            return Err(PeerError(
                "tunnel proxy does not support standard Responses WebSocket".into(),
            ));
        }
    }
    builder
        .build()
        .map_err(|_| PeerError("standard Responses WebSocket client build failed".into()))
}

fn websocket_transport_timeouts(
    plan: &aether_contracts::ExecutionPlan,
) -> Option<aether_contracts::ExecutionTimeouts> {
    let mut timeouts = plan.timeouts.clone()?;
    timeouts.read_ms = None;
    timeouts.first_byte_ms = None;
    timeouts.total_ms = None;
    Some(timeouts)
}

fn compact_response_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            if matches!(
                name.as_str(),
                "api-key"
                    | "authorization"
                    | "cookie"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "set-cookie"
                    | "x-api-key"
                    | "x-goog-api-key"
            ) {
                return None;
            }
            value.to_str().ok().and_then(|value| {
                let value = value.trim();
                (!value.is_empty() && value.len() <= 256)
                    .then(|| (name.as_str().to_string(), value.to_string()))
            })
        })
        .take(32)
        .collect()
}

struct OutboundCommand {
    message: WreqWsMessage,
    close: bool,
    completion: oneshot::Sender<Result<(), PeerError>>,
}

pub(crate) struct StandardPeer {
    inbound: tokio::sync::mpsc::Receiver<Result<RelayFrame, PeerError>>,
    outbound: PollSender<OutboundCommand>,
    pending_write: Option<oneshot::Receiver<Result<(), PeerError>>>,
    close_sent: bool,
    cancellation: CancellationToken,
}

impl StandardPeer {
    fn spawn(mut socket: wreq::ws::WebSocket) -> Self {
        let (outbound_tx, mut outbound_rx) =
            tokio::sync::mpsc::channel::<OutboundCommand>(BRIDGE_CHANNEL_CAPACITY);
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(BRIDGE_CHANNEL_CAPACITY);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = task_cancellation.cancelled() => break,
                    outbound = outbound_rx.recv() => {
                        let Some(command) = outbound else {
                            let _ = tokio::select! {
                                _ = task_cancellation.cancelled() => Ok(()),
                                result = socket.send(WreqWsMessage::Close(None)) => result,
                            };
                            break;
                        };
                        let result = tokio::select! {
                            _ = task_cancellation.cancelled() => Err(PeerError(
                                "standard Responses WebSocket bridge closed".into(),
                            )),
                            result = socket.send(command.message) => result.map_err(|_| PeerError(
                                "standard Responses WebSocket send failed".into(),
                            )),
                        };
                        let _ = command.completion.send(result.clone());
                        if let Err(error) = result {
                            let _ = inbound_tx.try_send(Err(error));
                            break;
                        }
                        if command.close {
                            break;
                        }
                    }
                    inbound = socket.recv() => {
                        match inbound {
                            Some(Ok(message)) => {
                                if inbound_tx.send(Ok(wreq_to_relay(message))).await.is_err() {
                                    break;
                                }
                            }
                            Some(Err(_)) => {
                                let _ = inbound_tx
                                    .send(Err(PeerError(
                                        "standard Responses WebSocket receive failed".into(),
                                    )))
                                    .await;
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }
        });
        Self {
            inbound: inbound_rx,
            outbound: PollSender::new(outbound_tx),
            pending_write: None,
            close_sent: false,
            cancellation,
        }
    }

    fn poll_pending_write(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), PeerError>> {
        let Some(pending) = self.pending_write.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match Pin::new(pending).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending_write = None;
                Poll::Ready(result.unwrap_or_else(|_| {
                    Err(PeerError(
                        "standard Responses WebSocket bridge closed".into(),
                    ))
                }))
            }
        }
    }

    fn start_command(&mut self, frame: RelayFrame) -> Result<(), PeerError> {
        if self.pending_write.is_some() || self.close_sent {
            return Err(PeerError(
                "standard Responses WebSocket bridge is not ready".into(),
            ));
        }
        let close = matches!(frame, RelayFrame::Close);
        let message = relay_to_wreq(frame)?;
        let (completion, pending_write) = oneshot::channel();
        self.outbound
            .send_item(OutboundCommand {
                message,
                close,
                completion,
            })
            .map_err(|_| PeerError("standard Responses WebSocket bridge closed".into()))?;
        self.pending_write = Some(pending_write);
        self.close_sent = close;
        Ok(())
    }
}

impl Drop for StandardPeer {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

fn relay_to_wreq(frame: RelayFrame) -> Result<WreqWsMessage, PeerError> {
    Ok(match frame {
        RelayFrame::Text(text) => WreqWsMessage::Text(
            text.try_into()
                .map_err(|_| PeerError("standard Responses WebSocket text is not UTF-8".into()))?,
        ),
        RelayFrame::Binary(bytes) => WreqWsMessage::Binary(bytes),
        RelayFrame::Ping(bytes) => WreqWsMessage::Ping(bytes),
        RelayFrame::Pong(bytes) => WreqWsMessage::Pong(bytes),
        RelayFrame::Close => WreqWsMessage::Close(None),
    })
}

fn wreq_to_relay(message: WreqWsMessage) -> RelayFrame {
    match message {
        WreqWsMessage::Text(text) => RelayFrame::Text(text.to_string().into()),
        WreqWsMessage::Binary(bytes) => RelayFrame::Binary(bytes),
        WreqWsMessage::Ping(bytes) => RelayFrame::Ping(bytes),
        WreqWsMessage::Pong(bytes) => RelayFrame::Pong(bytes),
        WreqWsMessage::Close(_) => RelayFrame::Close,
    }
}

impl Stream for StandardPeer {
    type Item = Result<RelayFrame, PeerError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inbound.poll_recv(cx)
    }
}

impl Sink<RelayFrame> for StandardPeer {
    type Error = PeerError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.poll_pending_write(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        if self.close_sent {
            return Poll::Ready(Err(PeerError(
                "standard Responses WebSocket bridge is closed".into(),
            )));
        }
        self.outbound
            .poll_reserve(cx)
            .map_err(|_| PeerError("standard Responses WebSocket bridge closed".into()))
    }

    fn start_send(mut self: Pin<&mut Self>, frame: RelayFrame) -> Result<(), Self::Error> {
        self.start_command(frame)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_pending_write(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.poll_pending_write(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        if !self.close_sent {
            match self.outbound.poll_reserve(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(PeerError(
                        "standard Responses WebSocket bridge closed".into(),
                    )))
                }
                Poll::Ready(Ok(())) => {
                    if let Err(error) = self.start_command(RelayFrame::Close) {
                        return Poll::Ready(Err(error));
                    }
                    match self.poll_pending_write(cx) {
                        Poll::Ready(Ok(())) => {}
                        other => return other,
                    }
                }
            }
        }
        self.outbound.close();
        self.cancellation.cancel();
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::extract::State;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use http::header::{RETRY_AFTER, SET_COOKIE};
    use http::{HeaderMap, HeaderValue, StatusCode};

    use super::{
        connect_standard_websocket, websocket_handshake_headers, websocket_upstream_url,
        OutboundCommand, StandardPeer, StandardWebSocketConnectError,
    };
    use crate::codex_ws::runtime::{PeerError, RelayFrame};

    #[test]
    fn maps_http_url_structurally_and_rejects_embedded_credentials() {
        let url =
            websocket_upstream_url("https://example.test/v1/responses?x=1").expect("valid URL");
        assert_eq!(url.as_str(), "wss://example.test/v1/responses?x=1");
        assert!(websocket_upstream_url("https://token@example.test/v1/responses").is_err());
    }

    #[test]
    fn handshake_keeps_provider_auth_and_strips_transport_headers() {
        let source = BTreeMap::from([
            ("authorization".to_string(), "Bearer provider".to_string()),
            ("x-api-key".to_string(), "provider-key".to_string()),
            (
                "connection".to_string(),
                "keep-alive, x-provider-hop".to_string(),
            ),
            ("x-provider-hop".to_string(), "drop".to_string()),
            ("sec-websocket-key".to_string(), "downstream".to_string()),
            ("proxy-authorization".to_string(), "drop".to_string()),
        ]);
        let headers = websocket_handshake_headers(&source).expect("valid provider headers");
        assert_eq!(headers["authorization"], "Bearer provider");
        assert_eq!(headers["x-api-key"], "provider-key");
        for name in [
            "connection",
            "x-provider-hop",
            "sec-websocket-key",
            "proxy-authorization",
        ] {
            assert!(headers.get(name).is_none(), "{name} must be stripped");
        }
    }

    fn test_bridge_peer() -> (StandardPeer, tokio::sync::mpsc::Receiver<OutboundCommand>) {
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        let (_inbound_tx, inbound) = tokio::sync::mpsc::channel::<Result<RelayFrame, PeerError>>(1);
        (
            StandardPeer {
                inbound,
                outbound: tokio_util::sync::PollSender::new(outbound_tx),
                pending_write: None,
                close_sent: false,
                cancellation: tokio_util::sync::CancellationToken::new(),
            },
            outbound_rx,
        )
    }

    #[tokio::test]
    async fn sink_flush_waits_for_socket_write_completion() {
        let (mut peer, mut outbound) = test_bridge_peer();
        let mut send = tokio::spawn(async move {
            peer.send(RelayFrame::Text(Bytes::from_static(b"request")))
                .await
        });
        let command = tokio::time::timeout(Duration::from_secs(1), outbound.recv())
            .await
            .expect("the bridge should receive the write")
            .expect("the bridge channel should remain open");

        assert!(!command.close);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut send)
            .await
            .is_err());

        command
            .completion
            .send(Ok(()))
            .expect("the sink should still await completion");
        assert_eq!(send.await.expect("send task should join"), Ok(()));
    }

    fn test_plan(url: String) -> aether_contracts::ExecutionPlan {
        aether_contracts::ExecutionPlan {
            request_id: "request-1".into(),
            candidate_id: Some("candidate-1".into()),
            provider_name: Some("standard".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "GET".into(),
            url,
            headers: BTreeMap::from([
                ("authorization".into(), "Bearer provider".into()),
                ("x-api-key".into(), "provider-key".into()),
            ]),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(serde_json::json!({})),
            stream: true,
            client_api_format: "openai:responses".into(),
            provider_api_format: "openai:responses".into(),
            model_name: Some("gpt-test".into()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        }
    }

    async fn start_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener should bind");
        let address = listener
            .local_addr()
            .expect("loopback listener should have an address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("test server should run");
        });
        (format!("http://{address}/v1/responses"), server)
    }

    async fn echo_upgrade(
        ws: WebSocketUpgrade,
        State(observed): State<tokio::sync::mpsc::UnboundedSender<(String, String)>>,
        headers: HeaderMap,
    ) -> Response {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let mut response = ws
            .on_upgrade(move |mut socket| async move {
                let Some(Ok(Message::Text(text))) = socket.next().await else {
                    return;
                };
                let _ = observed.send((authorization, text.to_string()));
                let _ = socket
                    .send(Message::Text(
                        r#"{"type":"response.completed","response":{"id":"resp-1"}}"#.into(),
                    ))
                    .await;
            })
            .into_response();
        response.headers_mut().insert(
            "x-request-id",
            HeaderValue::from_static("upstream-request-1"),
        );
        response
    }

    #[tokio::test]
    async fn loopback_websocket_preserves_provider_auth_and_relays_frames() {
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
        let router = Router::new()
            .route("/v1/responses", get(echo_upgrade))
            .with_state(observed_tx);
        let (url, server) = start_server(router).await;
        let connection = connect_standard_websocket(&test_plan(url))
            .await
            .expect("the loopback WebSocket should connect");
        assert_eq!(
            connection
                .response_headers
                .get("x-request-id")
                .map(String::as_str),
            Some("upstream-request-1")
        );
        let mut peer = connection.peer;

        peer.send(RelayFrame::Text(Bytes::from_static(b"request-frame")))
            .await
            .expect("the frame should be written to the socket");
        let frame = tokio::time::timeout(Duration::from_secs(1), peer.next())
            .await
            .expect("the provider frame should arrive")
            .expect("the provider stream should remain open")
            .expect("the provider frame should be valid");
        assert_eq!(
            frame,
            RelayFrame::Text(Bytes::from_static(
                br#"{"type":"response.completed","response":{"id":"resp-1"}}"#,
            ))
        );
        assert_eq!(
            observed_rx.recv().await,
            Some(("Bearer provider".into(), "request-frame".into()))
        );

        drop(peer);
        server.abort();
    }

    async fn reject_upgrade() -> Response {
        let mut response =
            (StatusCode::TOO_MANY_REQUESTS, r#"{"error":"limited"}"#).into_response();
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static("7"));
        response.headers_mut().insert(
            SET_COOKIE,
            HeaderValue::from_static("provider-session=secret"),
        );
        response
            .headers_mut()
            .insert("x-api-key", HeaderValue::from_static("provider-secret"));
        response
    }

    #[tokio::test]
    async fn rejected_handshake_preserves_status_body_and_safe_headers() {
        let router = Router::new().route("/v1/responses", get(reject_upgrade));
        let (url, server) = start_server(router).await;

        let error = match connect_standard_websocket(&test_plan(url)).await {
            Ok(_) => panic!("the rejected upgrade should fail"),
            Err(error) => error,
        };
        let StandardWebSocketConnectError::Rejected {
            status_code,
            response_headers,
            error_body,
        } = error
        else {
            panic!("the HTTP rejection should remain structured");
        };
        assert_eq!(status_code, StatusCode::TOO_MANY_REQUESTS.as_u16());
        assert_eq!(
            response_headers.get("retry-after").map(String::as_str),
            Some("7")
        );
        assert_eq!(response_headers.get("set-cookie"), None);
        assert_eq!(response_headers.get("x-api-key"), None);
        assert_eq!(error_body.as_deref(), Some(r#"{"error":"limited"}"#));

        server.abort();
    }
}
