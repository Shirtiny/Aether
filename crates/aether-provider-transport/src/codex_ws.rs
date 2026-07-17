use std::collections::BTreeMap;

use aether_contracts::{
    ExecutionPlan, ExecutionTimeouts, ProxySnapshot, RequestBody, ResolvedTransportProfile,
};
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::snapshot::GatewayProviderTransportSnapshot;

pub const CODEX_OFFICIAL_WS_CAPABILITY: &str = "codex_official_ws";
pub const CODEX_OFFICIAL_WS_CONTINUATION_MODE: &str = "connection_local";
pub const CODEX_OFFICIAL_WS_PROFILE_ID: &str =
    "codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1";
pub const CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION: u64 = 3;
pub const CODEX_OFFICIAL_WS_PROFILE_FINGERPRINT_KEY: &str = "websocket_transport_profile";
pub const CODEX_OFFICIAL_WS_CODEX_COMMIT: &str = "1f0566d3f59298d1bb88820a0d35294f1eeb07ea";
pub const CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV: &str =
    "0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186";
pub const CODEX_OFFICIAL_WS_TUNGSTENITE_REV: &str = "4fffad30fe373adbdcffab9545e9e9bf4f2fc19f";
pub const CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID: &str =
    "aether-tungstenite-0.27-out-buffer-retention-v1";
pub const CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES: usize = 128 * 1024;
pub const CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES: usize = 17 * 1024 * 1024;
pub const CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES: usize = 256 * 1024;
pub const CODEX_OFFICIAL_WS_CRYPTO_PROVIDER: &str = "aws-lc-rs";
pub const CODEX_OFFICIAL_WS_HOST: &str = "chatgpt.com";
pub const CODEX_OFFICIAL_WS_BASE_PATH: &str = "/backend-api/codex";

pub struct CodexOfficialWsPlanningPlanInput {
    pub request_id: String,
    pub candidate_id: String,
    pub provider_name: String,
    pub provider_id: String,
    pub endpoint_id: String,
    pub key_id: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub client_api_format: String,
    pub provider_api_format: String,
    pub model_name: String,
    pub proxy: Option<ProxySnapshot>,
    pub transport_profile: Option<ResolvedTransportProfile>,
    pub timeouts: Option<ExecutionTimeouts>,
}

/// Builds the body-free scheduler/lifecycle plan used during official WS
/// account fanout. The selected account materializes response.create exactly
/// once immediately before its provider-bound write.
pub fn build_codex_official_ws_planning_plan(
    input: CodexOfficialWsPlanningPlanInput,
) -> ExecutionPlan {
    ExecutionPlan {
        request_id: input.request_id,
        candidate_id: Some(input.candidate_id),
        provider_name: Some(input.provider_name),
        provider_id: input.provider_id,
        endpoint_id: input.endpoint_id,
        key_id: input.key_id,
        method: "POST".to_string(),
        url: input.url,
        headers: input.headers,
        content_type: Some("application/json".to_string()),
        content_encoding: None,
        body: RequestBody {
            json_body: None,
            body_bytes_b64: None,
            body_ref: None,
        },
        stream: true,
        client_api_format: input.client_api_format,
        provider_api_format: input.provider_api_format,
        model_name: Some(input.model_name),
        proxy: input.proxy,
        transport_profile: input.transport_profile,
        timeouts: input.timeouts,
    }
}

pub const fn codex_official_ws_requires_http_redaction(
    runtime_enabled: bool,
    account_enabled: bool,
) -> bool {
    runtime_enabled && account_enabled
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexOfficialWsGlobalFlags {
    pub enabled: bool,
    pub native_codex_ws_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum CodexOfficialWsIneligibilityReason {
    GlobalDisabled = 0,
    NativeCodexWsDisabled = 1,
    ProviderTypeUnsupported = 2,
    ProviderInactive = 3,
    KeyAuthTypeUnsupported = 4,
    KeyInactive = 5,
    AccountCapabilityMissing = 6,
    AccountCapabilityDisabled = 7,
    AccountCapabilityInvalid = 8,
    WebsocketTransportProfileMissing = 9,
    WebsocketTransportProfileInvalid = 10,
    WebsocketTransportProfileSchemaUnsupported = 11,
    WebsocketTransportProfileIdUnsupported = 12,
    EndpointInactive = 13,
    OfficialEndpointInvalid = 14,
    OfficialEndpointSchemeUnsupported = 15,
    OfficialEndpointHostUnsupported = 16,
    OfficialEndpointPortUnsupported = 17,
    OfficialEndpointPathUnsupported = 18,
    WebsocketTransportProfileCodexCommitUnsupported = 19,
    WebsocketTransportProfileTokioTungsteniteRevUnsupported = 20,
    WebsocketTransportProfileTungsteniteRevUnsupported = 21,
    WebsocketTransportProfileCryptoProviderUnsupported = 22,
    WebsocketTransportProfileTungstenitePatchUnsupported = 23,
    WebsocketTransportProfileWriteBufferSizeUnsupported = 24,
    WebsocketTransportProfileMaxWriteBufferSizeUnsupported = 25,
    WebsocketTransportProfileMaxRetainedWriteBufferCapacityUnsupported = 26,
    EndpointApiFormatUnsupported = 27,
}

const ALL_INELIGIBILITY_REASONS: [CodexOfficialWsIneligibilityReason; 28] = [
    CodexOfficialWsIneligibilityReason::GlobalDisabled,
    CodexOfficialWsIneligibilityReason::NativeCodexWsDisabled,
    CodexOfficialWsIneligibilityReason::ProviderTypeUnsupported,
    CodexOfficialWsIneligibilityReason::ProviderInactive,
    CodexOfficialWsIneligibilityReason::KeyAuthTypeUnsupported,
    CodexOfficialWsIneligibilityReason::KeyInactive,
    CodexOfficialWsIneligibilityReason::AccountCapabilityMissing,
    CodexOfficialWsIneligibilityReason::AccountCapabilityDisabled,
    CodexOfficialWsIneligibilityReason::AccountCapabilityInvalid,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMissing,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileInvalid,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileSchemaUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileIdUnsupported,
    CodexOfficialWsIneligibilityReason::EndpointInactive,
    CodexOfficialWsIneligibilityReason::OfficialEndpointInvalid,
    CodexOfficialWsIneligibilityReason::OfficialEndpointSchemeUnsupported,
    CodexOfficialWsIneligibilityReason::OfficialEndpointHostUnsupported,
    CodexOfficialWsIneligibilityReason::OfficialEndpointPortUnsupported,
    CodexOfficialWsIneligibilityReason::OfficialEndpointPathUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileCodexCommitUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTokioTungsteniteRevUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTungsteniteRevUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileCryptoProviderUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTungstenitePatchUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileWriteBufferSizeUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMaxWriteBufferSizeUnsupported,
    CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMaxRetainedWriteBufferCapacityUnsupported,
    CodexOfficialWsIneligibilityReason::EndpointApiFormatUnsupported,
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexOfficialWsIneligibilityReasons(u32);

impl CodexOfficialWsIneligibilityReasons {
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub fn contains(self, reason: CodexOfficialWsIneligibilityReason) -> bool {
        self.0 & reason_bit(reason) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = CodexOfficialWsIneligibilityReason> {
        ALL_INELIGIBILITY_REASONS
            .into_iter()
            .filter(move |reason| self.contains(*reason))
    }

    fn insert(&mut self, reason: CodexOfficialWsIneligibilityReason) {
        self.0 |= reason_bit(reason);
    }
}

impl Serialize for CodexOfficialWsIneligibilityReasons {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for reason in self.iter() {
            sequence.serialize_element(&reason)?;
        }
        sequence.end()
    }
}

const fn reason_bit(reason: CodexOfficialWsIneligibilityReason) -> u32 {
    1u32 << reason as u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CodexOfficialWsResolution {
    pub configured: bool,
    /// Static account/profile/endpoint prerequisites only. Proxy resolution,
    /// model matching and mutable scheduler state are deliberately excluded.
    pub profile_effective: bool,
    pub reasons: CodexOfficialWsIneligibilityReasons,
    pub continuation_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexOfficialWsRuntimeEligibilityReason {
    ProfileNotEffective,
    ProxyRouteNotEvaluated,
    RequestModelNotEvaluated,
    QuotaRuntimeStateNotEvaluated,
    CircuitRuntimeStateNotEvaluated,
    ConcurrencyRuntimeStateNotEvaluated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexOfficialWsAdminRuntimeState {
    RequestScoped,
    ProfileBlocked,
    SoftDraining,
    HardRevoked,
}

pub const fn resolve_codex_official_ws_admin_runtime_state(
    key_active: bool,
    configured: bool,
    profile_effective: bool,
) -> CodexOfficialWsAdminRuntimeState {
    if !key_active {
        CodexOfficialWsAdminRuntimeState::HardRevoked
    } else if !configured {
        CodexOfficialWsAdminRuntimeState::SoftDraining
    } else if !profile_effective {
        CodexOfficialWsAdminRuntimeState::ProfileBlocked
    } else {
        CodexOfficialWsAdminRuntimeState::RequestScoped
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexOfficialWsRuntimeEligibilityResolution {
    /// `None` means that eligibility is request-scoped and was not evaluated.
    /// Administrative configuration mutations do not have the model, route or
    /// instantaneous scheduler state required to truthfully produce `true`.
    pub runtime_eligible: Option<bool>,
    pub reasons: Vec<CodexOfficialWsRuntimeEligibilityReason>,
}

impl CodexOfficialWsResolution {
    pub fn runtime_eligibility_without_request_context(
        &self,
    ) -> CodexOfficialWsRuntimeEligibilityResolution {
        if !self.profile_effective {
            return CodexOfficialWsRuntimeEligibilityResolution {
                runtime_eligible: Some(false),
                reasons: vec![CodexOfficialWsRuntimeEligibilityReason::ProfileNotEffective],
            };
        }

        CodexOfficialWsRuntimeEligibilityResolution {
            runtime_eligible: None,
            reasons: vec![
                CodexOfficialWsRuntimeEligibilityReason::ProxyRouteNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::RequestModelNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::QuotaRuntimeStateNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::CircuitRuntimeStateNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::ConcurrencyRuntimeStateNotEvaluated,
            ],
        }
    }
}

pub fn resolve_codex_official_ws(
    transport: &GatewayProviderTransportSnapshot,
    global: CodexOfficialWsGlobalFlags,
) -> CodexOfficialWsResolution {
    let mut reasons = CodexOfficialWsIneligibilityReasons::default();

    if !global.enabled {
        reasons.insert(CodexOfficialWsIneligibilityReason::GlobalDisabled);
    }
    if !global.native_codex_ws_enabled {
        reasons.insert(CodexOfficialWsIneligibilityReason::NativeCodexWsDisabled);
    }
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        reasons.insert(CodexOfficialWsIneligibilityReason::ProviderTypeUnsupported);
    }
    if !transport.provider.is_active {
        reasons.insert(CodexOfficialWsIneligibilityReason::ProviderInactive);
    }
    resolve_official_endpoint(transport, &mut reasons);
    if !transport.key.auth_type.trim().eq_ignore_ascii_case("oauth") {
        reasons.insert(CodexOfficialWsIneligibilityReason::KeyAuthTypeUnsupported);
    }
    if !transport.key.is_active {
        reasons.insert(CodexOfficialWsIneligibilityReason::KeyInactive);
    }

    let configured = resolve_account_capability(transport.key.capabilities.as_ref(), &mut reasons);
    let profile_id =
        resolve_websocket_transport_profile(transport.key.fingerprint.as_ref(), &mut reasons);

    let profile_effective = reasons.is_empty();
    CodexOfficialWsResolution {
        configured,
        profile_effective,
        reasons,
        continuation_mode: CODEX_OFFICIAL_WS_CONTINUATION_MODE,
        profile_id,
    }
}

fn resolve_official_endpoint(
    transport: &GatewayProviderTransportSnapshot,
    reasons: &mut CodexOfficialWsIneligibilityReasons,
) {
    if !transport.endpoint.is_active {
        reasons.insert(CodexOfficialWsIneligibilityReason::EndpointInactive);
    }
    if !transport
        .endpoint
        .api_format
        .trim()
        .eq_ignore_ascii_case("openai:responses")
    {
        reasons.insert(CodexOfficialWsIneligibilityReason::EndpointApiFormatUnsupported);
    }

    let Ok(endpoint) = url::Url::parse(transport.endpoint.base_url.trim()) else {
        reasons.insert(CodexOfficialWsIneligibilityReason::OfficialEndpointInvalid);
        return;
    };

    if endpoint.scheme() != "https" {
        reasons.insert(CodexOfficialWsIneligibilityReason::OfficialEndpointSchemeUnsupported);
    }
    if endpoint.host_str() != Some(CODEX_OFFICIAL_WS_HOST)
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        reasons.insert(CodexOfficialWsIneligibilityReason::OfficialEndpointHostUnsupported);
    }
    if endpoint.port_or_known_default() != Some(443) {
        reasons.insert(CodexOfficialWsIneligibilityReason::OfficialEndpointPortUnsupported);
    }

    let path_matches =
        endpoint.path() == CODEX_OFFICIAL_WS_BASE_PATH || endpoint.path() == "/backend-api/codex/";
    let custom_path_is_empty = transport
        .endpoint
        .custom_path
        .as_deref()
        .is_none_or(|value| value.trim().is_empty());
    if !path_matches
        || !custom_path_is_empty
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        reasons.insert(CodexOfficialWsIneligibilityReason::OfficialEndpointPathUnsupported);
    }
}

fn resolve_account_capability(
    capabilities: Option<&Value>,
    reasons: &mut CodexOfficialWsIneligibilityReasons,
) -> bool {
    let Some(capabilities) = capabilities else {
        reasons.insert(CodexOfficialWsIneligibilityReason::AccountCapabilityMissing);
        return false;
    };
    let Some(capabilities) = capabilities.as_object() else {
        reasons.insert(CodexOfficialWsIneligibilityReason::AccountCapabilityInvalid);
        return false;
    };
    let Some(capability) = capabilities.get(CODEX_OFFICIAL_WS_CAPABILITY) else {
        reasons.insert(CodexOfficialWsIneligibilityReason::AccountCapabilityMissing);
        return false;
    };
    match capability.as_bool() {
        Some(true) => true,
        Some(false) => {
            reasons.insert(CodexOfficialWsIneligibilityReason::AccountCapabilityDisabled);
            false
        }
        None => {
            reasons.insert(CodexOfficialWsIneligibilityReason::AccountCapabilityInvalid);
            false
        }
    }
}

fn resolve_websocket_transport_profile(
    fingerprint: Option<&Value>,
    reasons: &mut CodexOfficialWsIneligibilityReasons,
) -> Option<&'static str> {
    let Some(fingerprint) = fingerprint else {
        reasons.insert(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMissing);
        return None;
    };
    let Some(fingerprint) = fingerprint.as_object() else {
        reasons.insert(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileInvalid);
        return None;
    };
    let Some(profile) = fingerprint.get(CODEX_OFFICIAL_WS_PROFILE_FINGERPRINT_KEY) else {
        reasons.insert(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMissing);
        return None;
    };
    let Some(profile) = profile.as_object() else {
        reasons.insert(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileInvalid);
        return None;
    };

    let schema_matches = profile.get("schema_version").and_then(Value::as_u64)
        == Some(CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION);
    if !schema_matches {
        reasons
            .insert(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileSchemaUnsupported);
    }

    let profile_matches =
        profile.get("profile_id").and_then(Value::as_str) == Some(CODEX_OFFICIAL_WS_PROFILE_ID);
    if !profile_matches {
        reasons.insert(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileIdUnsupported);
    }

    let codex_commit_matches =
        profile.get("codex_commit").and_then(Value::as_str) == Some(CODEX_OFFICIAL_WS_CODEX_COMMIT);
    if !codex_commit_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileCodexCommitUnsupported,
        );
    }

    let tokio_tungstenite_rev_matches =
        profile.get("tokio_tungstenite_rev").and_then(Value::as_str)
            == Some(CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV);
    if !tokio_tungstenite_rev_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTokioTungsteniteRevUnsupported,
        );
    }

    let tungstenite_rev_matches = profile.get("tungstenite_rev").and_then(Value::as_str)
        == Some(CODEX_OFFICIAL_WS_TUNGSTENITE_REV);
    if !tungstenite_rev_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTungsteniteRevUnsupported,
        );
    }

    let crypto_provider_matches = profile.get("crypto_provider").and_then(Value::as_str)
        == Some(CODEX_OFFICIAL_WS_CRYPTO_PROVIDER);
    if !crypto_provider_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileCryptoProviderUnsupported,
        );
    }

    let tungstenite_patch_matches = profile.get("tungstenite_patch_id").and_then(Value::as_str)
        == Some(CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID);
    if !tungstenite_patch_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTungstenitePatchUnsupported,
        );
    }

    let write_buffer_size_matches = profile
        .get("write_buffer_size_bytes")
        .and_then(Value::as_u64)
        == Some(CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES as u64);
    if !write_buffer_size_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileWriteBufferSizeUnsupported,
        );
    }

    let max_write_buffer_size_matches = profile
        .get("max_write_buffer_size_bytes")
        .and_then(Value::as_u64)
        == Some(CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES as u64);
    if !max_write_buffer_size_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMaxWriteBufferSizeUnsupported,
        );
    }

    let max_retained_write_buffer_capacity_matches = profile
        .get("max_retained_write_buffer_capacity_bytes")
        .and_then(Value::as_u64)
        == Some(CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES as u64);
    if !max_retained_write_buffer_capacity_matches {
        reasons.insert(
            CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMaxRetainedWriteBufferCapacityUnsupported,
        );
    }

    (schema_matches
        && profile_matches
        && codex_commit_matches
        && tokio_tungstenite_rev_matches
        && tungstenite_rev_matches
        && crypto_provider_matches
        && tungstenite_patch_matches
        && write_buffer_size_matches
        && max_write_buffer_size_matches
        && max_retained_write_buffer_capacity_matches)
        .then_some(CODEX_OFFICIAL_WS_PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::hint::black_box;
    use std::time::Instant;

    use serde_json::{json, Value};

    use super::{
        build_codex_official_ws_planning_plan, codex_official_ws_requires_http_redaction,
        resolve_codex_official_ws, resolve_codex_official_ws_admin_runtime_state,
        CodexOfficialWsAdminRuntimeState, CodexOfficialWsGlobalFlags,
        CodexOfficialWsIneligibilityReason, CodexOfficialWsPlanningPlanInput,
        CodexOfficialWsRuntimeEligibilityReason, CODEX_OFFICIAL_WS_CODEX_COMMIT,
        CODEX_OFFICIAL_WS_CONTINUATION_MODE, CODEX_OFFICIAL_WS_CRYPTO_PROVIDER,
        CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
        CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES, CODEX_OFFICIAL_WS_PROFILE_ID,
        CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION, CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
        CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID, CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
        CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
    };
    use crate::snapshot::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider, GatewayProviderTransportSnapshot,
    };

    fn valid_transport() -> GatewayProviderTransportSnapshot {
        GatewayProviderTransportSnapshot {
            provider: GatewayProviderTransportProvider {
                id: "provider-1".to_string(),
                name: "Codex".to_string(),
                provider_type: "codex".to_string(),
                website: None,
                is_active: true,
                keep_priority_on_conversion: false,
                enable_format_conversion: false,
                concurrent_limit: None,
                max_retries: None,
                proxy: None,
                request_timeout_secs: None,
                stream_first_byte_timeout_secs: None,
                config: None,
            },
            endpoint: GatewayProviderTransportEndpoint {
                id: "endpoint-1".to_string(),
                provider_id: "provider-1".to_string(),
                api_format: "openai:responses".to_string(),
                api_family: Some("openai".to_string()),
                endpoint_kind: Some("responses".to_string()),
                is_active: true,
                base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                header_rules: None,
                body_rules: None,
                max_retries: None,
                custom_path: None,
                config: None,
                format_acceptance_config: None,
                proxy: None,
            },
            key: GatewayProviderTransportKey {
                id: "key-1".to_string(),
                provider_id: "provider-1".to_string(),
                name: "oauth-account".to_string(),
                auth_type: "oauth".to_string(),
                is_active: true,
                api_formats: Some(vec!["openai:responses".to_string()]),
                auth_type_by_format: None,
                allow_auth_channel_mismatch_formats: None,
                allowed_models: None,
                capabilities: Some(json!({"codex_official_ws": true})),
                rate_multipliers: None,
                global_priority_by_format: None,
                expires_at_unix_secs: None,
                proxy: None,
                fingerprint: Some(json!({
                    "websocket_transport_profile": {
                        "schema_version": CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION,
                        "profile_id": CODEX_OFFICIAL_WS_PROFILE_ID,
                        "codex_commit": CODEX_OFFICIAL_WS_CODEX_COMMIT,
                        "tokio_tungstenite_rev": CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV,
                        "tungstenite_rev": CODEX_OFFICIAL_WS_TUNGSTENITE_REV,
                        "tungstenite_patch_id": CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID,
                        "write_buffer_size_bytes": CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
                        "max_write_buffer_size_bytes": CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES,
                        "max_retained_write_buffer_capacity_bytes": CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
                        "crypto_provider": CODEX_OFFICIAL_WS_CRYPTO_PROVIDER
                    }
                })),
                upstream_metadata: None,
                decrypted_api_key: "access-token".to_string(),
                decrypted_auth_config: None,
            },
        }
    }

    fn enabled_flags() -> CodexOfficialWsGlobalFlags {
        CodexOfficialWsGlobalFlags {
            enabled: true,
            native_codex_ws_enabled: true,
        }
    }

    #[test]
    fn account_profile_contract_matches_the_connector_manifest() {
        let manifest = aether_codex_ws_connector::codex_ws_profile_manifest();

        assert_eq!(
            manifest.schema_version,
            CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION
        );
        assert_eq!(manifest.profile_id, CODEX_OFFICIAL_WS_PROFILE_ID);
        assert_eq!(
            manifest.continuation_mode,
            CODEX_OFFICIAL_WS_CONTINUATION_MODE
        );
        assert_eq!(
            manifest.source.codex_revision,
            CODEX_OFFICIAL_WS_CODEX_COMMIT
        );
        assert_eq!(
            manifest.dependencies.tokio_tungstenite_revision,
            CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV
        );
        assert_eq!(
            manifest.dependencies.tungstenite_revision,
            CODEX_OFFICIAL_WS_TUNGSTENITE_REV
        );
        assert_eq!(
            manifest.dependencies.tungstenite_patch_id,
            CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID
        );
        assert_eq!(
            manifest.websocket.write_buffer_size_bytes,
            CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES
        );
        assert_eq!(
            manifest.websocket.max_write_buffer_size_bytes,
            CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES
        );
        assert_eq!(
            manifest.websocket.max_retained_write_buffer_capacity_bytes,
            CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES
        );
        assert_eq!(
            manifest.tls.crypto_provider,
            CODEX_OFFICIAL_WS_CRYPTO_PROVIDER
        );
    }

    fn planning_plan(index: usize) -> aether_contracts::ExecutionPlan {
        build_codex_official_ws_planning_plan(CodexOfficialWsPlanningPlanInput {
            request_id: "request-1".to_string(),
            candidate_id: format!("candidate-{index}"),
            provider_name: "Codex pool".to_string(),
            provider_id: "provider-1".to_string(),
            endpoint_id: "endpoint-1".to_string(),
            key_id: format!("key-{index}"),
            url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            headers: BTreeMap::from([
                ("authorization".to_string(), "Bearer token".to_string()),
                ("user-agent".to_string(), "codex-cli".to_string()),
            ]),
            client_api_format: "openai:responses".to_string(),
            provider_api_format: "openai:responses".to_string(),
            model_name: "gpt-5.4".to_string(),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        })
    }

    fn request_body(request_body_size: usize) -> Value {
        json!({
            "type": "response.create",
            "model": "gpt-5.4",
            "input": "x".repeat(request_body_size),
        })
    }

    fn planning_artifact_bytes(request_body: &Value, candidates: usize) -> usize {
        black_box(request_body);
        let plans = (0..candidates).map(planning_plan).collect::<Vec<_>>();
        assert!(plans.iter().all(|plan| {
            plan.body.json_body.is_none()
                && plan.body.body_bytes_b64.is_none()
                && plan.body.body_ref.is_none()
        }));
        serde_json::to_vec(&plans)
            .expect("body-free plans serialize")
            .len()
    }

    fn measure_body_free_plan_builds(
        request_body: &Value,
        candidates: usize,
        iterations: usize,
    ) -> u128 {
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(request_body);
            black_box((0..candidates).map(planning_plan).collect::<Vec<_>>());
        }
        started.elapsed().as_micros()
    }

    #[test]
    fn candidate_plans_do_not_retain_response_create_body() {
        for candidates in [1, 16] {
            let small_body = request_body(64 * 1024);
            let large_body = request_body(1024 * 1024);
            let small = planning_artifact_bytes(&small_body, candidates);
            let large = planning_artifact_bytes(&large_body, candidates);
            assert_eq!(small, large);
            assert!(large < candidates * 1024);
        }
    }

    #[test]
    fn native_ws_requires_http_path_when_redaction_is_effective() {
        assert!(!codex_official_ws_requires_http_redaction(false, false));
        assert!(!codex_official_ws_requires_http_redaction(false, true));
        assert!(!codex_official_ws_requires_http_redaction(true, false));
        assert!(codex_official_ws_requires_http_redaction(true, true));
    }

    #[test]
    #[ignore = "repeatable local performance probe; run with --ignored --nocapture"]
    fn body_free_planning_perf_probe_64k_1m() {
        const ITERATIONS: usize = 20_000;
        let bodies = [request_body(64 * 1024), request_body(1024 * 1024)];
        for candidates in [1, 16] {
            for (request_body_size, request_body) in
                [(64 * 1024, &bodies[0]), (1024 * 1024, &bodies[1])]
            {
                let bytes = planning_artifact_bytes(request_body, candidates);
                let elapsed_us = (0..5)
                    .map(|_| measure_body_free_plan_builds(request_body, candidates, ITERATIONS))
                    .min()
                    .expect("at least one performance sample");
                eprintln!(
                    "codex_ws_body_free_plan body_bytes={request_body_size} candidates={candidates} iterations={ITERATIONS} best_elapsed_us={elapsed_us} artifact_bytes={bytes}",
                );
            }
        }
    }

    #[test]
    fn resolves_fixed_account_profile_without_copying_configuration() {
        let result = resolve_codex_official_ws(&valid_transport(), enabled_flags());

        assert!(result.configured);
        assert!(result.profile_effective);
        assert!(result.reasons.is_empty());
        assert_eq!(
            result.continuation_mode,
            CODEX_OFFICIAL_WS_CONTINUATION_MODE
        );
        assert_eq!(result.profile_id, Some(CODEX_OFFICIAL_WS_PROFILE_ID));
    }

    #[test]
    fn rejects_every_invalid_cartesian_configuration() {
        const DIMENSIONS: usize = 14;
        for mask in 0usize..(1usize << DIMENSIONS) {
            let mut transport = valid_transport();
            let flags = CodexOfficialWsGlobalFlags {
                enabled: mask & (1 << 0) != 0,
                native_codex_ws_enabled: mask & (1 << 1) != 0,
            };
            transport.provider.is_active = mask & (1 << 2) != 0;
            transport.endpoint.is_active = mask & (1 << 3) != 0;
            transport.key.is_active = mask & (1 << 4) != 0;
            if mask & (1 << 5) == 0 {
                transport.provider.provider_type = "openai".to_string();
            }
            if mask & (1 << 6) == 0 {
                transport.key.auth_type = "api_key".to_string();
            }
            transport.key.capabilities = Some(json!({
                "codex_official_ws": mask & (1 << 7) != 0
            }));
            transport.key.fingerprint = Some(json!({
                "websocket_transport_profile": {
                    "schema_version": if mask & (1 << 8) != 0 {
                        CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION
                    } else {
                        CODEX_OFFICIAL_WS_PROFILE_SCHEMA_VERSION + 1
                    },
                    "profile_id": if mask & (1 << 9) != 0 {
                        CODEX_OFFICIAL_WS_PROFILE_ID
                    } else {
                        "unsupported-profile"
                    },
                    "codex_commit": if mask & (1 << 10) != 0 {
                        CODEX_OFFICIAL_WS_CODEX_COMMIT
                    } else {
                        "unsupported-codex-commit"
                    },
                    "tokio_tungstenite_rev": if mask & (1 << 11) != 0 {
                        CODEX_OFFICIAL_WS_TOKIO_TUNGSTENITE_REV
                    } else {
                        "unsupported-tokio-tungstenite-rev"
                    },
                    "tungstenite_rev": if mask & (1 << 12) != 0 {
                        CODEX_OFFICIAL_WS_TUNGSTENITE_REV
                    } else {
                        "unsupported-tungstenite-rev"
                    },
                    "tungstenite_patch_id": CODEX_OFFICIAL_WS_TUNGSTENITE_PATCH_ID,
                    "write_buffer_size_bytes": CODEX_OFFICIAL_WS_WRITE_BUFFER_SIZE_BYTES,
                    "max_write_buffer_size_bytes": CODEX_OFFICIAL_WS_MAX_WRITE_BUFFER_SIZE_BYTES,
                    "max_retained_write_buffer_capacity_bytes": CODEX_OFFICIAL_WS_MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES,
                    "crypto_provider": if mask & (1 << 13) != 0 {
                        CODEX_OFFICIAL_WS_CRYPTO_PROVIDER
                    } else {
                        "unsupported-crypto-provider"
                    },
                }
            }));

            let result = resolve_codex_official_ws(&transport, flags);
            let all_dimensions_valid = mask == (1usize << DIMENSIONS) - 1;
            assert_eq!(
                result.profile_effective,
                all_dimensions_valid,
                "unexpected result for cartesian mask {mask:014b}: {:?}",
                result.reasons.iter().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn rejects_inactive_or_non_official_endpoints_with_stable_reasons() {
        let cases = [
            (
                "not a URL",
                None,
                CodexOfficialWsIneligibilityReason::OfficialEndpointInvalid,
            ),
            (
                "http://chatgpt.com/backend-api/codex",
                None,
                CodexOfficialWsIneligibilityReason::OfficialEndpointSchemeUnsupported,
            ),
            (
                "https://example.com/backend-api/codex",
                None,
                CodexOfficialWsIneligibilityReason::OfficialEndpointHostUnsupported,
            ),
            (
                "https://chatgpt.com:8443/backend-api/codex",
                None,
                CodexOfficialWsIneligibilityReason::OfficialEndpointPortUnsupported,
            ),
            (
                "https://chatgpt.com/backend-api/not-codex",
                None,
                CodexOfficialWsIneligibilityReason::OfficialEndpointPathUnsupported,
            ),
            (
                "https://chatgpt.com/backend-api/codex?redirect=1",
                None,
                CodexOfficialWsIneligibilityReason::OfficialEndpointPathUnsupported,
            ),
            (
                "https://chatgpt.com/backend-api/codex",
                Some("/custom"),
                CodexOfficialWsIneligibilityReason::OfficialEndpointPathUnsupported,
            ),
        ];

        for (base_url, custom_path, reason) in cases {
            let mut transport = valid_transport();
            transport.endpoint.base_url = base_url.to_string();
            transport.endpoint.custom_path = custom_path.map(str::to_string);
            let result = resolve_codex_official_ws(&transport, enabled_flags());
            assert!(
                !result.profile_effective,
                "endpoint unexpectedly eligible: {base_url}"
            );
            assert!(
                result.reasons.contains(reason),
                "missing {reason:?} for {base_url}: {:?}",
                result.reasons.iter().collect::<Vec<_>>()
            );
        }

        let mut transport = valid_transport();
        transport.endpoint.is_active = false;
        let result = resolve_codex_official_ws(&transport, enabled_flags());
        assert!(!result.profile_effective);
        assert!(result
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::EndpointInactive));

        let mut transport = valid_transport();
        transport.endpoint.api_format = "openai:chat-completions".to_string();
        let result = resolve_codex_official_ws(&transport, enabled_flags());
        assert!(!result.profile_effective);
        assert!(result
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::EndpointApiFormatUnsupported));
    }

    #[test]
    fn rejects_each_missing_immutable_profile_manifest_field() {
        let cases = [
            (
                "codex_commit",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileCodexCommitUnsupported,
            ),
            (
                "tokio_tungstenite_rev",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTokioTungsteniteRevUnsupported,
            ),
            (
                "tungstenite_rev",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTungsteniteRevUnsupported,
            ),
            (
                "crypto_provider",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileCryptoProviderUnsupported,
            ),
            (
                "tungstenite_patch_id",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileTungstenitePatchUnsupported,
            ),
            (
                "write_buffer_size_bytes",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileWriteBufferSizeUnsupported,
            ),
            (
                "max_write_buffer_size_bytes",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMaxWriteBufferSizeUnsupported,
            ),
            (
                "max_retained_write_buffer_capacity_bytes",
                CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMaxRetainedWriteBufferCapacityUnsupported,
            ),
        ];

        for (field, reason) in cases {
            let mut transport = valid_transport();
            transport
                .key
                .fingerprint
                .as_mut()
                .and_then(Value::as_object_mut)
                .and_then(|fingerprint| fingerprint.get_mut("websocket_transport_profile"))
                .and_then(Value::as_object_mut)
                .expect("valid profile should exist")
                .remove(field);

            let result = resolve_codex_official_ws(&transport, enabled_flags());
            assert!(
                !result.profile_effective,
                "profile unexpectedly eligible without {field}"
            );
            assert!(result.reasons.contains(reason));
            assert_eq!(result.profile_id, None);
        }
    }

    #[test]
    fn reports_missing_invalid_and_disabled_account_configuration_separately() {
        let mut transport = valid_transport();
        transport.key.capabilities = None;
        transport.key.fingerprint = None;
        let missing = resolve_codex_official_ws(&transport, enabled_flags());
        assert!(!missing.configured);
        assert!(missing
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::AccountCapabilityMissing));
        assert!(missing
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileMissing));

        transport.key.capabilities = Some(json!({"codex_official_ws": "true"}));
        transport.key.fingerprint = Some(json!({
            "websocket_transport_profile": CODEX_OFFICIAL_WS_PROFILE_ID
        }));
        let invalid = resolve_codex_official_ws(&transport, enabled_flags());
        assert!(!invalid.configured);
        assert!(invalid
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::AccountCapabilityInvalid));
        assert!(invalid
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::WebsocketTransportProfileInvalid));

        transport.key.capabilities = Some(json!({"codex_official_ws": false}));
        transport.key.fingerprint = valid_transport().key.fingerprint;
        let disabled = resolve_codex_official_ws(&transport, enabled_flags());
        assert!(!disabled.configured);
        assert!(disabled
            .reasons
            .contains(CodexOfficialWsIneligibilityReason::AccountCapabilityDisabled));
    }

    #[test]
    fn serializes_stable_reason_codes_without_exposing_invalid_values() {
        let result =
            resolve_codex_official_ws(&valid_transport(), CodexOfficialWsGlobalFlags::default());
        let payload = serde_json::to_value(result).expect("resolution should serialize");

        assert_eq!(payload["configured"], json!(true));
        assert_eq!(payload["profile_effective"], json!(false));
        assert!(payload.get("effective").is_none());
        assert_eq!(
            payload["reasons"],
            json!(["global_disabled", "native_codex_ws_disabled"])
        );
        assert_eq!(payload["profile_id"], json!(CODEX_OFFICIAL_WS_PROFILE_ID));
    }

    #[test]
    fn admin_resolution_never_claims_runtime_eligibility_without_request_context() {
        let profile = resolve_codex_official_ws(&valid_transport(), enabled_flags());
        let runtime = profile.runtime_eligibility_without_request_context();

        assert_eq!(runtime.runtime_eligible, None);
        assert_eq!(
            runtime.reasons,
            vec![
                CodexOfficialWsRuntimeEligibilityReason::ProxyRouteNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::RequestModelNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::QuotaRuntimeStateNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::CircuitRuntimeStateNotEvaluated,
                CodexOfficialWsRuntimeEligibilityReason::ConcurrencyRuntimeStateNotEvaluated,
            ]
        );

        let payload = serde_json::to_value(runtime).expect("runtime resolution should serialize");
        assert_eq!(payload["runtime_eligible"], Value::Null);
        assert_eq!(
            payload["reasons"],
            json!([
                "proxy_route_not_evaluated",
                "request_model_not_evaluated",
                "quota_runtime_state_not_evaluated",
                "circuit_runtime_state_not_evaluated",
                "concurrency_runtime_state_not_evaluated"
            ])
        );
    }

    #[test]
    fn static_profile_blocker_is_a_known_runtime_blocker() {
        let profile =
            resolve_codex_official_ws(&valid_transport(), CodexOfficialWsGlobalFlags::default());
        let runtime = profile.runtime_eligibility_without_request_context();

        assert_eq!(runtime.runtime_eligible, Some(false));
        assert_eq!(
            runtime.reasons,
            vec![CodexOfficialWsRuntimeEligibilityReason::ProfileNotEffective]
        );
    }

    #[test]
    fn admin_runtime_state_never_uses_active_for_request_scoped_eligibility() {
        let cases = [
            (
                true,
                true,
                true,
                CodexOfficialWsAdminRuntimeState::RequestScoped,
            ),
            (
                true,
                true,
                false,
                CodexOfficialWsAdminRuntimeState::ProfileBlocked,
            ),
            (
                true,
                false,
                false,
                CodexOfficialWsAdminRuntimeState::SoftDraining,
            ),
            (
                false,
                true,
                false,
                CodexOfficialWsAdminRuntimeState::HardRevoked,
            ),
        ];

        for (key_active, configured, profile_effective, expected) in cases {
            let state = resolve_codex_official_ws_admin_runtime_state(
                key_active,
                configured,
                profile_effective,
            );
            assert_eq!(state, expected);
        }
        assert_eq!(
            serde_json::to_value(CodexOfficialWsAdminRuntimeState::RequestScoped)
                .expect("admin runtime state should serialize"),
            json!("request_scoped")
        );
    }

    #[test]
    fn websocket_account_switch_does_not_change_http_transport_eligibility() {
        let mut transport = valid_transport();
        transport.key.capabilities = Some(json!({"codex_official_ws": false}));

        let ws = resolve_codex_official_ws(&transport, enabled_flags());
        assert!(!ws.profile_effective);
        assert!(
            crate::policy::supports_local_standard_transport_with_network(
                &transport,
                "openai:responses"
            )
        );
    }
}
