use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const DEFAULT_PROVIDER_SESSION_RISK_CONTROL_BLOCK_TTL_SECONDS: u64 = 24 * 60 * 60;

const PROVIDER_SESSION_RISK_CONTROL_BLOCK_KEY_PREFIX: &str =
    "provider_session_risk_control_avoidance:v1";
const SESSION_RISK_CONTROL_BLOCK_KEY_PREFIX: &str = "session_risk_control_avoidance:v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderSessionRiskControlAvoidanceMode {
    Disabled,
    Candidate,
    Block,
}

impl ProviderSessionRiskControlAvoidanceMode {
    pub(crate) fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    pub(crate) fn blocks_session(self) -> bool {
        matches!(self, Self::Block)
    }
}

pub(crate) fn provider_session_risk_control_block_key(
    provider_id: &str,
    session_key: &str,
) -> Option<String> {
    let provider_id = provider_id.trim();
    let session_key = session_key.trim();
    if provider_id.is_empty() || session_key.is_empty() {
        return None;
    }
    Some(format!(
        "{PROVIDER_SESSION_RISK_CONTROL_BLOCK_KEY_PREFIX}:{}:{}",
        sha256_hex(provider_id.as_bytes()),
        sha256_hex(session_key.as_bytes())
    ))
}

pub(crate) fn session_risk_control_block_key(session_key: &str) -> Option<String> {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return None;
    }
    Some(format!(
        "{SESSION_RISK_CONTROL_BLOCK_KEY_PREFIX}:{}",
        sha256_hex(session_key.as_bytes())
    ))
}

pub(crate) fn client_session_key_from_metadata(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_object)
        .and_then(|object| object.get("client_session_affinity"))
        .and_then(Value::as_object)
        .and_then(|object| object.get("session_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn provider_session_risk_control_avoidance_mode(
    config: Option<&serde_json::Value>,
) -> ProviderSessionRiskControlAvoidanceMode {
    let Some(object) = config
        .and_then(|value| value.get("risk_control_session_avoidance"))
        .and_then(serde_json::Value::as_object)
    else {
        return ProviderSessionRiskControlAvoidanceMode::Candidate;
    };

    if let Some(mode) = object
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        return match mode.as_str() {
            "candidate" | "candidates" | "fallback" | "fallback_candidate" => {
                ProviderSessionRiskControlAvoidanceMode::Candidate
            }
            "block" | "blocked" | "deny" | "deny_all" => {
                ProviderSessionRiskControlAvoidanceMode::Block
            }
            _ => ProviderSessionRiskControlAvoidanceMode::Candidate,
        };
    }

    ProviderSessionRiskControlAvoidanceMode::Candidate
}

pub(crate) fn provider_session_risk_control_avoidance_ttl_seconds(
    config: Option<&serde_json::Value>,
) -> u64 {
    config
        .and_then(|value| value.get("risk_control_session_avoidance"))
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get("ttl_seconds"))
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .or_else(|| {
            config
                .and_then(|value| value.get("pool_advanced"))
                .and_then(serde_json::Value::as_object)
                .and_then(|object| object.get("sticky_session_ttl_seconds"))
                .and_then(serde_json::Value::as_u64)
                .filter(|value| *value > 0)
        })
        .unwrap_or(DEFAULT_PROVIDER_SESSION_RISK_CONTROL_BLOCK_TTL_SECONDS)
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
        provider_session_risk_control_avoidance_mode, ProviderSessionRiskControlAvoidanceMode,
    };

    #[test]
    fn risk_control_session_avoidance_defaults_to_candidate() {
        assert_eq!(
            provider_session_risk_control_avoidance_mode(None),
            ProviderSessionRiskControlAvoidanceMode::Candidate
        );
        assert_eq!(
            provider_session_risk_control_avoidance_mode(Some(&json!({
                "risk_control_session_avoidance": {}
            }))),
            ProviderSessionRiskControlAvoidanceMode::Candidate
        );
    }

    #[test]
    fn risk_control_session_avoidance_reads_select_modes() {
        assert_eq!(
            provider_session_risk_control_avoidance_mode(Some(&json!({
                "risk_control_session_avoidance": {
                    "mode": "candidate"
                }
            }))),
            ProviderSessionRiskControlAvoidanceMode::Candidate
        );
        assert_eq!(
            provider_session_risk_control_avoidance_mode(Some(&json!({
                "risk_control_session_avoidance": {
                    "mode": "block"
                }
            }))),
            ProviderSessionRiskControlAvoidanceMode::Block
        );
    }

    #[test]
    fn risk_control_session_avoidance_treats_legacy_enabled_as_candidate() {
        assert_eq!(
            provider_session_risk_control_avoidance_mode(Some(&json!({
                "risk_control_session_avoidance": {
                    "enabled": true
                }
            }))),
            ProviderSessionRiskControlAvoidanceMode::Candidate
        );
        assert_eq!(
            provider_session_risk_control_avoidance_mode(Some(&json!({
                "risk_control_session_avoidance": {
                    "enabled": false
                }
            }))),
            ProviderSessionRiskControlAvoidanceMode::Candidate
        );
    }
}
