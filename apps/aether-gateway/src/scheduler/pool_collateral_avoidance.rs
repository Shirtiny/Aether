use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const DEFAULT_PROVIDER_POOL_STICKY_COLLATERAL_BLOCK_TTL_SECONDS: u64 = 24 * 60 * 60;

const PROVIDER_POOL_STICKY_COLLATERAL_BLOCK_KEY_PREFIX: &str =
    "provider_pool_sticky_collateral_avoidance:v1";

pub(crate) fn provider_pool_sticky_collateral_block_key(
    provider_id: &str,
    sticky_session_token: &str,
) -> Option<String> {
    let provider_id = provider_id.trim();
    let sticky_session_token = sticky_session_token.trim();
    if provider_id.is_empty() || sticky_session_token.is_empty() {
        return None;
    }
    Some(format!(
        "{PROVIDER_POOL_STICKY_COLLATERAL_BLOCK_KEY_PREFIX}:{}:{}",
        sha256_hex(provider_id.as_bytes()),
        sha256_hex(sticky_session_token.as_bytes())
    ))
}

pub(crate) fn provider_pool_sticky_collateral_avoidance_enabled(config: Option<&Value>) -> bool {
    let Some(pool_advanced) = config
        .and_then(|value| value.get("pool_advanced"))
        .and_then(Value::as_object)
    else {
        return false;
    };

    [
        "sticky_collateral_avoidance_enabled",
        "pool_sticky_collateral_avoidance_enabled",
        "sticky_account_collateral_avoidance_enabled",
        "collateral_avoidance_enabled",
    ]
    .into_iter()
    .find_map(|key| pool_advanced.get(key).and_then(Value::as_bool))
    .unwrap_or(false)
}

pub(crate) fn provider_pool_sticky_collateral_avoidance_ttl_seconds(config: Option<&Value>) -> u64 {
    config
        .and_then(|value| value.get("pool_advanced"))
        .and_then(Value::as_object)
        .and_then(|object| {
            object
                .get("sticky_collateral_avoidance_ttl_seconds")
                .or_else(|| object.get("pool_sticky_collateral_avoidance_ttl_seconds"))
                .or_else(|| object.get("sticky_session_ttl_seconds"))
        })
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PROVIDER_POOL_STICKY_COLLATERAL_BLOCK_TTL_SECONDS)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to string should not fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        provider_pool_sticky_collateral_avoidance_enabled,
        provider_pool_sticky_collateral_avoidance_ttl_seconds,
        provider_pool_sticky_collateral_block_key,
        DEFAULT_PROVIDER_POOL_STICKY_COLLATERAL_BLOCK_TTL_SECONDS,
    };

    #[test]
    fn pool_sticky_collateral_block_key_ignores_blank_inputs() {
        assert!(provider_pool_sticky_collateral_block_key("", "session-1").is_none());
        assert!(provider_pool_sticky_collateral_block_key("provider-1", "  ").is_none());
        assert!(provider_pool_sticky_collateral_block_key("provider-1", "session-1").is_some());
    }

    #[test]
    fn pool_sticky_collateral_avoidance_reads_pool_advanced_switch() {
        assert!(!provider_pool_sticky_collateral_avoidance_enabled(None));
        assert!(!provider_pool_sticky_collateral_avoidance_enabled(Some(
            &json!({
                "pool_advanced": {}
            })
        )));
        assert!(provider_pool_sticky_collateral_avoidance_enabled(Some(
            &json!({
                "pool_advanced": {
                    "sticky_collateral_avoidance_enabled": true
                }
            })
        )));
        assert!(provider_pool_sticky_collateral_avoidance_enabled(Some(
            &json!({
                "pool_advanced": {
                    "pool_sticky_collateral_avoidance_enabled": true
                }
            })
        )));
    }

    #[test]
    fn pool_sticky_collateral_avoidance_ttl_uses_sticky_ttl_fallback() {
        assert_eq!(
            provider_pool_sticky_collateral_avoidance_ttl_seconds(None),
            DEFAULT_PROVIDER_POOL_STICKY_COLLATERAL_BLOCK_TTL_SECONDS
        );
        assert_eq!(
            provider_pool_sticky_collateral_avoidance_ttl_seconds(Some(&json!({
                "pool_advanced": {
                    "sticky_session_ttl_seconds": 600
                }
            }))),
            600
        );
        assert_eq!(
            provider_pool_sticky_collateral_avoidance_ttl_seconds(Some(&json!({
                "pool_advanced": {
                    "sticky_session_ttl_seconds": 600,
                    "sticky_collateral_avoidance_ttl_seconds": 1200
                }
            }))),
            1200
        );
    }
}
