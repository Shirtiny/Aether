//! Network utility functions (public IP detection, region detection).
//!
//! These are standalone helpers not tied to any specific client or service.

use aether_http::{build_http_client, HttpClientConfig};
use tracing::{debug, info};

/// Auto-detect public IP by querying external services.
pub async fn detect_public_ip() -> anyhow::Result<String> {
    let endpoints = [
        "https://api.ipify.org",
        "https://ifconfig.me/ip",
        "https://icanhazip.com",
    ];

    let client = build_http_client(&HttpClientConfig {
        request_timeout_ms: Some(5_000),
        user_agent: Some("aether-tunnel/net".to_string()),
        ..HttpClientConfig::default()
    })?;

    for endpoint in &endpoints {
        match client.get(*endpoint).send().await {
            Ok(resp) if resp.status().is_success() => {
                let ip = resp.text().await?.trim().to_string();
                if !ip.is_empty() {
                    info!(ip = %ip, source = %endpoint, "detected public IP");
                    return Ok(ip);
                }
            }
            Ok(resp) => {
                debug!(endpoint = %endpoint, status = %resp.status(), "IP detection failed");
            }
            Err(e) => {
                debug!(endpoint = %endpoint, error = %e, "IP detection failed");
            }
        }
    }

    anyhow::bail!("failed to detect public IP from any source; use --public-ip")
}

/// Auto-detect geographic region from a public IP address.
///
/// Uses multiple providers with HTTPS preferred.  Falls back to ip-api.com
/// over plain HTTP (their free tier doesn't support HTTPS).
/// This is best-effort and non-sensitive -- region detection should never
/// block startup.
pub async fn detect_region(ip: &str) -> Option<String> {
    // Try HTTPS provider first
    let https_url = format!("https://ipinfo.io/{}/country", ip);

    let client = build_http_client(&HttpClientConfig {
        request_timeout_ms: Some(5_000),
        user_agent: Some("aether-tunnel/net".to_string()),
        ..HttpClientConfig::default()
    })
    .ok()?;

    // Try ipinfo.io (HTTPS, returns plain text country code)
    if let Ok(resp) = client.get(&https_url).send().await {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let code = text.trim();
                if !code.is_empty() && code.len() <= 3 {
                    info!(region = %code, ip = %ip, source = "ipinfo.io", "detected region");
                    return Some(code.to_string());
                }
            }
        }
    }

    // Fallback: ip-api.com (HTTP only on free tier, non-sensitive data)
    let http_url = format!("http://ip-api.com/json/{}?fields=countryCode", ip);
    match client.get(&http_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.ok()?;
            let code = body.get("countryCode")?.as_str()?;
            if code.is_empty() {
                return None;
            }
            info!(region = %code, ip = %ip, source = "ip-api.com", "detected region");
            Some(code.to_string())
        }
        _ => {
            debug!(ip = %ip, "region detection failed");
            None
        }
    }
}
