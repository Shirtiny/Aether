//! Isolated connector for the official Codex Responses WebSocket transport profile.
//!
//! This crate intentionally has no dependency on Aether routing or account state. Callers must
//! resolve eligibility and pass one explicit outbound route before opening a connection.

mod dialer;
mod profile;
mod tls;

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures_util::Sink;
use futures_util::Stream;
use rustls::ClientConfig;
use rustls::RootCertStore;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream as TungsteniteStream;

pub use profile::codex_websocket_config;
pub use profile::codex_ws_profile_manifest;
pub use profile::CodexWsDependencyManifest;
pub use profile::CodexWsProfileManifest;
pub use profile::CodexWsProtocolManifest;
pub use profile::CodexWsSourceManifest;
pub use profile::CodexWsTlsManifest;
pub use profile::CODEX_SOURCE_REVISION;
pub use profile::CODEX_WS_CONTINUATION_MODE;
pub use profile::CODEX_WS_PROFILE_ID;
pub use profile::CODEX_WS_PROFILE_MANIFEST_JSON;
pub use profile::CODEX_WS_PROFILE_SCHEMA_VERSION;
pub use profile::MAX_FRAME_SIZE_BYTES;
pub use profile::MAX_MESSAGE_SIZE_BYTES;
pub use profile::MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES;
pub use profile::MAX_WRITE_BUFFER_SIZE_BYTES;
pub use profile::TOKIO_TUNGSTENITE_REVISION;
pub use profile::TUNGSTENITE_PATCH_ID;
pub use profile::TUNGSTENITE_REVISION;
pub use profile::WRITE_BUFFER_SIZE_BYTES;
pub use tls::ConnectorBuildError;
pub use tungstenite::client::IntoClientRequest;
pub use tungstenite::error::ProtocolError as WebSocketProtocolError;
pub use tungstenite::error::TlsError as WebSocketTlsError;
pub use tungstenite::error::UrlError as WebSocketUrlError;
pub use tungstenite::handshake::client::Request;
pub use tungstenite::handshake::client::Response;
pub use tungstenite::Error as WebSocketError;
pub use tungstenite::Message;

#[derive(Clone, PartialEq, Eq)]
pub enum OutboundRoute {
    /// Preserve the pinned transport's HTTP(S)_PROXY, ALL_PROXY, and NO_PROXY handling.
    TransportDefault,
    /// Connect directly to the WebSocket authority.
    Direct,
    /// Tunnel through an explicit `http://`, `https://`, `socks5://`, or `socks5h://` proxy URL.
    Proxy { url: String },
}

impl OutboundRoute {
    pub fn proxy(url: impl Into<String>) -> Self {
        Self::Proxy { url: url.into() }
    }
}

impl fmt::Debug for OutboundRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransportDefault => formatter.write_str("TransportDefault"),
            Self::Direct => formatter.write_str("Direct"),
            Self::Proxy { .. } => formatter
                .debug_struct("Proxy")
                .field("url", &"<redacted>")
                .finish(),
        }
    }
}

/// Connector that caches parsed roots while creating fresh TLS state per physical connection.
#[derive(Clone)]
pub struct CodexWebSocketConnector {
    roots: Arc<RootCertStore>,
}

impl CodexWebSocketConnector {
    /// Builds a connector from the process native root store cached by this crate.
    pub fn new() -> Result<Self, ConnectorBuildError> {
        Ok(Self {
            roots: tls::cached_codex_roots()?,
        })
    }

    /// Builds the same pinned TLS profile with a caller-provided root store.
    ///
    /// This is useful for enterprise roots and hermetic tests. It does not alter the cached native
    /// root template used by [`Self::new`].
    pub fn with_root_store(roots: RootCertStore) -> Result<Self, ConnectorBuildError> {
        Ok(Self {
            roots: Arc::new(roots),
        })
    }

    /// Opens one connection with the pinned Codex profile and an explicit outbound route.
    pub async fn connect(
        &self,
        request: Request,
        route: OutboundRoute,
    ) -> Result<(WebSocketConnection, Response), WebSocketError> {
        let tls_config = self
            .fresh_tls_config()
            .map_err(|error| WebSocketError::Io(std::io::Error::other(error)))?;
        let (inner, response) =
            dialer::connect(request, codex_websocket_config(), tls_config, route).await?;
        Ok((WebSocketConnection { inner }, response))
    }

    fn fresh_tls_config(&self) -> Result<Arc<ClientConfig>, ConnectorBuildError> {
        Ok(Arc::new(tls::build_tls_config((*self.roots).clone())?))
    }
}

/// Established WebSocket independent of its direct, proxy, and TLS transport layers.
pub struct WebSocketConnection {
    inner: ConnectionInner,
}

impl Stream for WebSocketConnection {
    type Item = Result<Message, WebSocketError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match &mut self.get_mut().inner {
            ConnectionInner::TransportDefault(stream) => Pin::new(stream).poll_next(context),
            ConnectionInner::Routed(stream) => Pin::new(stream).poll_next(context),
        }
    }
}

impl Sink<Message> for WebSocketConnection {
    type Error = WebSocketError;

    fn poll_ready(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match &mut self.get_mut().inner {
            ConnectionInner::TransportDefault(stream) => Pin::new(stream).poll_ready(context),
            ConnectionInner::Routed(stream) => Pin::new(stream).poll_ready(context),
        }
    }

    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
        match &mut self.get_mut().inner {
            ConnectionInner::TransportDefault(stream) => Pin::new(stream).start_send(message),
            ConnectionInner::Routed(stream) => Pin::new(stream).start_send(message),
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match &mut self.get_mut().inner {
            ConnectionInner::TransportDefault(stream) => Pin::new(stream).poll_flush(context),
            ConnectionInner::Routed(stream) => Pin::new(stream).poll_flush(context),
        }
    }

    fn poll_close(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match &mut self.get_mut().inner {
            ConnectionInner::TransportDefault(stream) => Pin::new(stream).poll_close(context),
            ConnectionInner::Routed(stream) => Pin::new(stream).poll_close(context),
        }
    }
}

pub(crate) enum ConnectionInner {
    TransportDefault(TungsteniteStream<MaybeTlsStream<TcpStream>>),
    Routed(TungsteniteStream<MaybeTlsStream<Box<dyn AsyncIo>>>),
}

pub(crate) trait AsyncIo: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> AsyncIo for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

#[cfg(test)]
mod tests;
