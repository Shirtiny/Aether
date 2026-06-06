//! Safe DNS resolver for reqwest that reuses validated addresses from DnsCache.
//!
//! This resolver ensures reqwest connects only to addresses that have been
//! previously validated by `target_filter::validate_target()`, eliminating
//! the TOCTTOU gap where DNS rebinding could redirect traffic to private IPs.

use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::target_filter::{self, DnsCache};

/// A DNS resolver that serves validated public addresses from the shared DnsCache.
///
/// When reqwest needs to resolve a hostname, this resolver returns addresses
/// from the cache (populated by `validate_target()` during request validation).
/// If the hostname is not in cache (shouldn't happen in normal flow), it
/// performs a fresh resolution with private-IP filtering.
pub struct SafeDnsResolver {
    dns_cache: Arc<DnsCache>,
}

impl SafeDnsResolver {
    pub fn new(dns_cache: Arc<DnsCache>) -> Self {
        Self { dns_cache }
    }
}

impl Resolve for SafeDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let dns_cache = Arc::clone(&self.dns_cache);
        Box::pin(async move {
            let host = name.as_str();

            // Try cache first (should be populated by validate_target).
            // reqwest resolves by hostname only (no port), so use host-only lookup.
            if let Some(addrs) = dns_cache.get_by_host(host).await {
                let socket_addrs: Vec<SocketAddr> = (*addrs).clone();
                return Ok(Box::new(socket_addrs.into_iter()) as Addrs);
            }

            // Fallback: resolve with private-IP filtering (defensive).
            // This path should rarely be hit since validate_target() runs first.
            // We don't know the real port here (reqwest Resolve only gives hostname),
            // so resolve directly without caching to avoid polluting the cache with
            // an incorrect port-based key.
            let addr_str = format!("{}:0", host);
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host(&addr_str)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .filter(|addr| !target_filter::is_private_ip(&addr.ip()))
                .collect();

            if resolved.is_empty() {
                return Err(Box::new(std::io::Error::other(format!(
                    "all resolved addresses for {} are private/reserved",
                    host
                )))
                    as Box<dyn std::error::Error + Send + Sync>);
            }

            Ok(Box::new(resolved.into_iter()) as Addrs)
        })
    }
}
