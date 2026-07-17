use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode, Uri};
use axum::response::IntoResponse;
use futures_util::{Sink, Stream};

use super::protocol::{
    negotiate_route_control, MAX_CLIENT_MESSAGE_BYTES, ROUTE_CONTROL_CAPABILITIES,
    ROUTE_CONTROL_CAPABILITIES_HEADER, ROUTE_CONTROL_SELECTED_HEADER, ROUTE_CONTROL_VERSION,
};
use super::runtime::{GatewayCodexWsRuntime, PeerError, RelayFrame, RelayPeer};
use super::session::run_codex_ws_session;
use crate::control::trusted_auth_local_rejection;
use crate::headers::extract_or_generate_trace_id;
use crate::AppState;

const DOWNSTREAM_WRITE_BUFFER_SIZE_BYTES: usize = 128 * 1024;
const DOWNSTREAM_MAX_WRITE_BUFFER_SIZE_BYTES: usize = 17 * 1024 * 1024;
const DOWNSTREAM_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES: usize = 256 * 1024;

pub(crate) async fn codex_responses_websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<std::net::SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let shared_global = match super::hot_state::ensure_global_hot_lease(&state).await {
        Ok(lease) => lease,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Codex WebSocket configuration is unavailable",
            )
        }
    };
    if !shared_global.eligible {
        return error_response(
            StatusCode::UPGRADE_REQUIRED,
            "Codex WebSocket ingress is disabled",
        );
    }
    if let Err(error) = negotiate_route_control(&headers) {
        return error_response(StatusCode::PRECONDITION_FAILED, error.message());
    }

    let trace_id = extract_or_generate_trace_id(&headers);
    let auth_context_epoch = state.auth_context_invalidation_epoch();
    let request_context = match crate::control::resolve_public_request_context(
        &state,
        &http::Method::GET,
        &uri,
        &headers,
        &trace_id,
    )
    .await
    {
        Ok(context) => context,
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth lookup failed"),
    };
    let Some(decision) = request_context.control_decision else {
        return error_response(
            StatusCode::NOT_FOUND,
            "Codex WebSocket route is unavailable",
        );
    };
    if trusted_auth_local_rejection(Some(&decision), &headers).is_some() {
        return error_response(StatusCode::UNAUTHORIZED, "API key access denied");
    }
    let Some(auth_context) = decision.auth_context.as_ref() else {
        return error_response(StatusCode::UNAUTHORIZED, "API key is required");
    };
    if !auth_context.access_allowed {
        return error_response(StatusCode::FORBIDDEN, "API key access denied");
    }
    if !crate::handlers::shared::ip_rules_allow(auth_context.ip_rules.as_deref(), remote_addr.ip())
    {
        return error_response(StatusCode::FORBIDDEN, "client IP is not allowed");
    }
    if state.auth_context_invalidation_epoch() != auth_context_epoch {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth context changed during WebSocket admission",
        );
    }
    if super::hot_state::validate_global_hot_lease(&state, &shared_global)
        .await
        .is_err()
    {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Codex WebSocket configuration changed during admission",
        );
    }

    let runtime = match GatewayCodexWsRuntime::new(
        state,
        headers,
        uri,
        decision,
        trace_id,
        shared_global,
        remote_addr.ip(),
        auth_context_epoch,
    ) {
        Ok(runtime) => runtime,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Codex WebSocket runtime is unavailable",
            )
        }
    };

    let mut response = ws
        .write_buffer_size(DOWNSTREAM_WRITE_BUFFER_SIZE_BYTES)
        .max_write_buffer_size(DOWNSTREAM_MAX_WRITE_BUFFER_SIZE_BYTES)
        .max_retained_write_buffer_capacity(DOWNSTREAM_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES)
        .max_frame_size(MAX_CLIENT_MESSAGE_BYTES)
        .max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            run_codex_ws_session(Box::new(AxumPeer { socket }), &runtime).await;
        })
        .into_response();
    response.headers_mut().insert(
        ROUTE_CONTROL_SELECTED_HEADER,
        HeaderValue::from_static(ROUTE_CONTROL_VERSION),
    );
    response.headers_mut().insert(
        ROUTE_CONTROL_CAPABILITIES_HEADER,
        HeaderValue::from_static(ROUTE_CONTROL_CAPABILITIES),
    );
    response
}

fn error_response(status: StatusCode, message: &'static str) -> Response<Body> {
    let payload = serde_json::json!({
        "error": {
            "type": "codex_websocket_error",
            "message": message,
        }
    });
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("static Codex WS error response should build")
}

struct AxumPeer {
    socket: WebSocket,
}

impl Stream for AxumPeer {
    type Item = Result<RelayFrame, PeerError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.socket)
            .poll_next(context)
            .map(|message| {
                message.map(|message| {
                    message
                        .map(|message| match message {
                            Message::Text(text) => RelayFrame::Text(text.into()),
                            Message::Binary(bytes) => RelayFrame::Binary(bytes),
                            Message::Ping(bytes) => RelayFrame::Ping(bytes),
                            Message::Pong(bytes) => RelayFrame::Pong(bytes),
                            Message::Close(_) => RelayFrame::Close,
                        })
                        .map_err(|_| PeerError("downstream WebSocket receive failed".into()))
                })
            })
    }
}

impl Sink<RelayFrame> for AxumPeer {
    type Error = PeerError;

    fn poll_ready(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.socket)
            .poll_ready(context)
            .map_err(|_| PeerError("downstream WebSocket send failed".into()))
    }

    fn start_send(
        mut self: std::pin::Pin<&mut Self>,
        frame: RelayFrame,
    ) -> Result<(), Self::Error> {
        let message =
            match frame {
                RelayFrame::Text(text) => Message::Text(text.try_into().map_err(|_| {
                    PeerError("Codex WS relay text frame was not valid UTF-8".into())
                })?),
                RelayFrame::Binary(bytes) => Message::Binary(bytes),
                RelayFrame::Ping(bytes) => Message::Ping(bytes),
                RelayFrame::Pong(bytes) => Message::Pong(bytes),
                RelayFrame::Close => Message::Close(None),
            };
        std::pin::Pin::new(&mut self.socket)
            .start_send(message)
            .map_err(|_| PeerError("downstream WebSocket send failed".into()))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.socket)
            .poll_flush(context)
            .map_err(|_| PeerError("downstream WebSocket send failed".into()))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.socket)
            .poll_close(context)
            .map_err(|_| PeerError("downstream WebSocket close failed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DOWNSTREAM_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
        DOWNSTREAM_MAX_WRITE_BUFFER_SIZE_BYTES, DOWNSTREAM_WRITE_BUFFER_SIZE_BYTES,
    };
    use crate::codex_ws::protocol::MAX_CLIENT_MESSAGE_BYTES;

    #[test]
    fn downstream_write_buffer_policy_bounds_retention_and_allows_legal_frames() {
        assert_eq!(DOWNSTREAM_WRITE_BUFFER_SIZE_BYTES, 128 * 1024);
        assert_eq!(
            DOWNSTREAM_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
            256 * 1024
        );
        assert!(DOWNSTREAM_MAX_WRITE_BUFFER_SIZE_BYTES > MAX_CLIENT_MESSAGE_BYTES);
        assert!(
            DOWNSTREAM_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES
                >= DOWNSTREAM_WRITE_BUFFER_SIZE_BYTES
        );
    }
}
