use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use futures_util::SinkExt;
use futures_util::StreamExt;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::RootCertStore;
use rustls::ServerConfig;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::server::Request as ServerRequest;
use tungstenite::handshake::server::Response as ServerResponse;
use tungstenite::proxy::ProxyScheme;
use tungstenite::Message;

use super::*;
use crate::dialer::connect_happy_eyeballs;
use crate::dialer::ProxyEndpoint;

const TEST_CERTIFICATE_DER_BASE64: &str = concat!(
    "MIIDQzCCAiugAwIBAgIUNL87ZdLGL3uDWZqqiMUxV1bU3cQwDQYJKoZIhvcNAQELBQAw",
    "FDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDcxNTE4MDgxN1oXDTM2MDcxMjE4MDgx",
    "N1owFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIB",
    "CgKCAQEAiZtaJCkE7SZT/vhScmyIv+GoQe7RqVx2JxKyEt5xUWpFf+lOMf/H6QAnvj67",
    "D7FfUuTyaqCEPvbC/pts5kET9BRgcy4gbeaEyz6CdVkMW2QkGtnXPtTT7POA1X5dHIAe",
    "JAh3Sn+cb7f9PYSbxPlCxTbvxLoxChV3ORlGeYntrzebcRLlOlSYF+rQs1UfL3zAZdn9",
    "0mXWYeG5qENDCyr67nQDGqPqeh1OEHESxVKRyUETL+/xmmfnn1wd9y8mx8XDGvlbG4Dm",
    "dKiUW1Y9O3q/9/YYRolwnkiVFSTrvBaSgVJUGDz+tZsZk86fQF3SYrgWBZuihnfJmasV",
    "4nNsHoyjowIDAQABo4GMMIGJMB0GA1UdDgQWBBQgJoBF30JyWs05bhczOyaahvFirjAf",
    "BgNVHSMEGDAWgBQgJoBF30JyWs05bhczOyaahvFirjAUBgNVHREEDTALgglsb2NhbGhv",
    "c3QwDAYDVR0TAQH/BAIwADAOBgNVHQ8BAf8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUH",
    "AwEwDQYJKoZIhvcNAQELBQADggEBADW9Y/Nq/p2c1Ye+yRxypZDhDlysZTPbBcwYGm/b",
    "vV0ZZ8AX+F6PsgBXOSBqUx/Y08Nn96nYYINzM90jo18tnme+N/kZnbDvJ5v7L32Mw17S",
    "UXO88pP+lIbWWMnqmz19l+eWvXhhc5sJacHsWHBZJIKUNMJLrGrAO3Y0gxNBvWWOP+pc",
    "GR/+cthftfXOr/Sd+G8mc8jbxwPbnDtZMZR0hd7yjhtgx+LcnYWspurMZQ/QMSHN1k2S",
    "f+qh6N7UV5/Ph9ZrYcwwjHyKY9PVStm+GzcWtPHbbloOw4GLLlJ2tde7Gj3KnMxouhmk",
    "Kp7M+iyPI3p+mY9cT8TCPj+VKic6juc="
);

const TEST_PRIVATE_KEY_DER_BASE64: &str = concat!(
    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCJm1okKQTtJlP++FJy",
    "bIi/4ahB7tGpXHYnErIS3nFRakV/6U4x/8fpACe+PrsPsV9S5PJqoIQ+9sL+m2zmQRP0",
    "FGBzLiBt5oTLPoJ1WQxbZCQa2dc+1NPs84DVfl0cgB4kCHdKf5xvt/09hJvE+ULFNu/",
    "EujEKFXc5GUZ5ie2vN5txEuU6VJgX6tCzVR8vfMBl2f3SZdZh4bmoQ0MLKvrudAMao+",
    "p6HU4QcRLFUpHJQRMv7/GaZ+efXB33LybHxcMa+VsbgOZ0qJRbVj07er/39hhGiXCeS",
    "JUVJOu8FpKBUlQYPP61mxmTzp9AXdJiuBYFm6KGd8mZqxXic2wejKOjAgMBAAECggEA",
    "HqtDs2R7BxnwRZb11S/QaKe0FwHRs8P6R2oYyzDNo74iQEhw157w4MLamMGlcnFvU+vY",
    "BaDB6MCZpCJi6oydlFxIRNOGgcgLV7sWW24d3W6bx2o+2W+YzipVT//qY7RAQ3qpj66S",
    "YKnqpJ/eEdAWLBs65Cc1T9CJ8m1qMiNmGJNh+vcI5jLXeKoDldZjQIl7tFBJcOO01Nl",
    "Fxkp+7JzCPAAHThpMIL+EnpQuMMYqwlV2wXqaiMi34K6JO0E4nAKD3+bSVblQwWk8Uc",
    "cDM/OcFOzm8EHILo4vuoSO7HZPRJ9dn7ueZzz4nL0iRsDrisCLxjq72eTolgWMgNyer5",
    "eAlQKBgQDBl3+OUwJXCMQpp79OG7M7ROpVQotapc75NbJbConsdS1JVn/s3y3mkvPNSe",
    "m0zeA8RMYcDvtJjGEzV4s1JmYgdpki7XvZQTYmdboRDe/Ssp742eziG5HOausI2qH39a",
    "Ig8eyiCE8KVvCYXfMSO33aHe86+oe4uxa37xWPsF4K/wKBgQC195kbqP44etnq2VHh9O",
    "JKGCoR0pmvKAIOPw0TzJTF7551KlXiZ0PAqb0E5d4kDTmdIEwbXPmLdnqxznjODnA+X/",
    "C/Q5eXLIYx8FAZcVjV7wyVzch2JJcrvsDZgy93czyOVoiemK3wRgv9XpseT5/X1ToLA3",
    "arWUeiwtMzI4ZbXQKBgCNVvaiCqjislvFrdtWQ5MP6rjLltH3VKdP+4xEO+WG5eYybRz",
    "o6+ivNwsZDqW6g7T7S5r4UVfV0tAElB3mqCpX+T7E6W5Kp/nJCprWaL53rkGynij8y/",
    "QgKJ+Az18BkizUsMx7YGWUvvTZyX32CclQvhozjUYZ8T4c/ElZpwKCNAoGAG5lWK4/SH",
    "xbi/m+/r5nIyJwppVJf5OUYiridbydUWUEis3qcVB59dDdKZ/fFXYpz9pTzdiL/5lst+",
    "NHsGLSv6YX7qcbCszcZk3FzdKhwZOJA8menw+OA2i2wak0vYdqkkKInToaxuwOkxeUXe",
    "d1xzPaWOx1nXk3IQ7Nw/QyiUDECgYEAktWtJQgIlXyHEK/dWG0VHiNRJ9ETLzxTTjt1r",
    "8Dkcn7Hd7HqmdkXkuliE+DLxR4ZmG65gpmdAESQ4ZdAzKno+jPSy0Y+PnyxHGki2r0+",
    "gYfhfEvVoxWYPyv0V9dXes/0eIC9jpRS5ZYeR7dmpjf+J9YrtmdszFcZcS/GfcpBb7M="
);

#[tokio::test]
async fn public_connector_negotiates_permessage_deflate_and_exposes_stream_and_sink() {
    let (target_addr, extension, target_task) = start_plain_echo_websocket_server().await;
    let request = format!("ws://localhost:{}/v1/responses", target_addr.port())
        .into_client_request()
        .expect("websocket request should build");
    let connector = CodexWebSocketConnector::with_root_store(RootCertStore::empty())
        .expect("connector should build");

    let (mut websocket, _) = connector
        .connect(request, OutboundRoute::Direct)
        .await
        .expect("websocket handshake should succeed");
    let expected = Message::Text("hello".into());
    websocket
        .send(expected.clone())
        .await
        .expect("websocket should send");
    let actual = websocket
        .next()
        .await
        .expect("websocket should receive a message")
        .expect("websocket message should be valid");
    assert_eq!(actual, expected);

    target_task.await.expect("target task should finish");
    let extension = extension
        .lock()
        .expect("extension capture lock should not be poisoned")
        .clone()
        .expect("client should send an extension header");
    assert!(extension.contains("permessage-deflate"));
}

#[tokio::test]
async fn direct_route_connects_secure_websocket() {
    let (connector, acceptor) = test_tls_configs();
    let (target_addr, target_task) = start_tls_websocket_server(acceptor).await;
    let request = format!("wss://localhost:{}/v1/responses", target_addr.port())
        .into_client_request()
        .expect("websocket request should build");

    let (websocket, _) = connector
        .connect(request, OutboundRoute::Direct)
        .await
        .expect("direct websocket handshake should succeed");
    drop(websocket);

    target_task.await.expect("target task should finish");
}

#[tokio::test]
async fn http_proxy_tunnels_secure_websocket_before_handshake() {
    assert_proxy_tunnels_secure_websocket(false).await;
}

#[tokio::test]
async fn https_proxy_tunnels_secure_websocket_before_handshake() {
    assert_proxy_tunnels_secure_websocket(true).await;
}

#[test]
fn https_proxy_defaults_to_port_443_and_preserves_explicit_port() {
    let default_port = ProxyEndpoint::parse("https://proxy.example").expect("proxy should parse");
    let explicit_port =
        ProxyEndpoint::parse("https://proxy.example:8443").expect("proxy should parse");

    assert_eq!(default_port.config.scheme, ProxyScheme::Http);
    assert_eq!(default_port.config.host, "proxy.example");
    assert_eq!(default_port.config.port, 443);
    assert!(default_port.tls);
    assert_eq!(explicit_port.config.port, 8443);
    assert!(explicit_port.tls);
}

#[test]
fn explicit_proxy_rejects_non_http_schemes_and_debug_redacts_credentials() {
    assert!(ProxyEndpoint::parse("socks5://proxy.example:1080").is_err());
    let route = OutboundRoute::proxy("http://user:password@proxy.example:8080");
    let debug = format!("{route:?}");
    assert!(!debug.contains("user"));
    assert!(!debug.contains("password"));
}

#[test]
fn roots_are_cached_but_physical_connections_get_distinct_tls_configs() {
    let first_connector = CodexWebSocketConnector::new().expect("first connector should build");
    let second_connector = CodexWebSocketConnector::new().expect("second connector should build");
    let first_config = first_connector
        .fresh_tls_config()
        .expect("first connection config should build");
    let second_config = second_connector
        .fresh_tls_config()
        .expect("second connection config should build");
    let next_config = first_connector
        .fresh_tls_config()
        .expect("next connection config should build");

    assert!(Arc::ptr_eq(&first_connector.roots, &second_connector.roots));
    assert!(!Arc::ptr_eq(&first_config, &second_config));
    assert!(!Arc::ptr_eq(&first_config, &next_config));
    assert_eq!(
        codex_ws_profile_manifest().tls.session_resumption,
        "rustls-default-empty-at-build"
    );
}

#[test]
fn tls_profile_freezes_the_pinned_codex_kx_group_order() {
    let connector = CodexWebSocketConnector::with_root_store(RootCertStore::empty())
        .expect("connector should build");
    let config = connector
        .fresh_tls_config()
        .expect("TLS config should build");
    let groups = config
        .crypto_provider()
        .kx_groups
        .iter()
        .map(|group| group.name())
        .collect::<Vec<_>>();

    assert_eq!(
        groups,
        [
            rustls::NamedGroup::X25519,
            rustls::NamedGroup::secp256r1,
            rustls::NamedGroup::secp384r1,
            rustls::NamedGroup::X25519MLKEM768,
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn happy_eyeballs_does_not_wait_for_stalled_preferred_family() {
    let stalled = "[2001:db8::1]:443"
        .parse::<SocketAddr>()
        .expect("stalled address should parse");
    let reachable = "127.0.0.1:443"
        .parse::<SocketAddr>()
        .expect("reachable address should parse");

    let connected = tokio::time::timeout(
        Duration::from_secs(1),
        connect_happy_eyeballs(vec![stalled, reachable], |address| async move {
            if address == stalled {
                std::future::pending::<()>().await;
            }
            Ok(address)
        }),
    )
    .await
    .expect("alternate family should start before timeout")
    .expect("alternate family should connect");

    assert_eq!(connected, reachable);
}

async fn start_plain_echo_websocket_server(
) -> (SocketAddr, Arc<Mutex<Option<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("target listener should bind");
    let address = listener
        .local_addr()
        .expect("target listener should have an address");
    let extension = Arc::new(Mutex::new(None));
    let captured_extension = Arc::clone(&extension);
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("target should accept");
        let mut websocket = tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            move |request: &ServerRequest, response: ServerResponse| {
                let value = request
                    .headers()
                    .get("sec-websocket-extensions")
                    .and_then(|value| value.to_str().ok())
                    .map(ToOwned::to_owned);
                *captured_extension
                    .lock()
                    .expect("extension capture lock should not be poisoned") = value;
                Ok(response)
            },
            Some(codex_websocket_config()),
        )
        .await
        .expect("target websocket handshake should succeed");
        let message = websocket
            .next()
            .await
            .expect("target should receive a message")
            .expect("target websocket message should be valid");
        websocket
            .send(message)
            .await
            .expect("target should echo the message");
    });
    (address, extension, task)
}

async fn assert_proxy_tunnels_secure_websocket(proxy_tls: bool) {
    let (connector, acceptor) = test_tls_configs();
    let (target_addr, target_task) = start_tls_websocket_server(acceptor.clone()).await;

    let proxy_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener should bind");
    let proxy_addr = proxy_listener
        .local_addr()
        .expect("proxy listener should have an address");
    let connect_request = Arc::new(Mutex::new(None));
    let proxy_connect_request = Arc::clone(&connect_request);
    let proxy_task = tokio::spawn(async move {
        let (client, _) = proxy_listener.accept().await.expect("proxy should accept");
        let mut client: Box<dyn AsyncIo> = if proxy_tls {
            Box::new(
                acceptor
                    .accept(client)
                    .await
                    .expect("proxy TLS handshake should succeed"),
            )
        } else {
            Box::new(client)
        };
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            client
                .read_exact(&mut byte)
                .await
                .expect("proxy should read CONNECT request");
            request.push(byte[0]);
        }
        *proxy_connect_request
            .lock()
            .expect("CONNECT capture lock should not be poisoned") =
            Some(String::from_utf8(request).expect("CONNECT request should be UTF-8"));

        let mut target = tokio::net::TcpStream::connect(target_addr)
            .await
            .expect("proxy should connect to target");
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .expect("proxy should acknowledge CONNECT");
        let _ = tokio::io::copy_bidirectional(&mut client, &mut target).await;
    });

    let target_authority = format!("localhost:{}", target_addr.port());
    let proxy_scheme = if proxy_tls { "https" } else { "http" };
    let request = format!("wss://{target_authority}/v1/responses")
        .into_client_request()
        .expect("websocket request should build");
    let (websocket, _) = connector
        .connect(
            request,
            OutboundRoute::proxy(format!("{proxy_scheme}://localhost:{}", proxy_addr.port())),
        )
        .await
        .expect("proxied websocket handshake should succeed");
    drop(websocket);

    target_task.await.expect("target task should finish");
    proxy_task.await.expect("proxy task should finish");
    let request = connect_request
        .lock()
        .expect("CONNECT capture lock should not be poisoned")
        .clone()
        .expect("proxy should record CONNECT request");
    let expected_request_line = format!("CONNECT {target_authority} HTTP/1.1");
    assert_eq!(request.lines().next(), Some(expected_request_line.as_str()));
}

async fn start_tls_websocket_server(acceptor: TlsAcceptor) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("target listener should bind");
    let address = listener
        .local_addr()
        .expect("target listener should have an address");
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("target should accept");
        let stream = acceptor
            .accept(stream)
            .await
            .expect("target TLS handshake should succeed");
        let mut websocket =
            tokio_tungstenite::accept_async_with_config(stream, Some(codex_websocket_config()))
                .await
                .expect("target websocket handshake should succeed");
        let _ = websocket.close(None).await;
    });
    (address, task)
}

fn test_tls_configs() -> (CodexWebSocketConnector, TlsAcceptor) {
    let certificate = CertificateDer::from(
        base64::engine::general_purpose::STANDARD
            .decode(TEST_CERTIFICATE_DER_BASE64)
            .expect("embedded test certificate should decode"),
    );
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        base64::engine::general_purpose::STANDARD
            .decode(TEST_PRIVATE_KEY_DER_BASE64)
            .expect("embedded test private key should decode"),
    ));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("server TLS versions should build")
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], private_key)
        .expect("test server config should build");

    let mut roots = RootCertStore::empty();
    roots
        .add(certificate)
        .expect("test certificate should be trusted");
    let connector =
        CodexWebSocketConnector::with_root_store(roots).expect("test connector should build");

    (connector, TlsAcceptor::from(Arc::new(server_config)))
}
