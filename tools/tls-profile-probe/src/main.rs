use std::time::Duration;

async fn run_tunnel_native_tls_probe(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().ok_or("missing url host")?.to_string();
    let port = parsed.port_or_known_default().ok_or("missing url port")?;
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
    let connector = native_tls::TlsConnector::builder().build()?;
    let result = tokio_native_tls::TlsConnector::from(connector)
        .connect(host.as_str(), tcp)
        .await;
    eprintln!("probe result: {result:?}");
    Ok(())
}

fn build_aether_current_rustls_client() -> Result<reqwest::Client, reqwest::Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = rustls::ClientConfig::builder_with_protocol_versions(&[
        &rustls::version::TLS13,
        &rustls::version::TLS12,
    ])
    .with_root_certificates(root_store)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .use_preconfigured_tls(config)
        .build()
}

fn build_aether_codex_default_tls_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().timeout(Duration::from_secs(5)).build()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("missing mode")?;
    let url = args.next().ok_or("missing url")?;

    let client = match mode.as_str() {
        "aether-current-rustls" => build_aether_current_rustls_client()?,
        "aether-codex-default-tls" => build_aether_codex_default_tls_client()?,
        "aether-tunnel-native-tls" => {
            run_tunnel_native_tls_probe(&url).await?;
            return Ok(());
        }
        other => {
            return Err(format!(
                "unknown mode {other:?}; expected aether-current-rustls, aether-codex-default-tls, or aether-tunnel-native-tls"
            )
            .into());
        }
    };

    let result = client
        .post(url)
        .header("content-type", "application/json")
        .json(&serde_json::json!({"model": "gpt-5", "input": "probe"}))
        .send()
        .await;
    eprintln!("probe result: {result:?}");
    Ok(())
}
