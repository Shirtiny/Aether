#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use std::time::SystemTime;

use aether_runtime_state::RuntimeState;
use http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::codex_environment_context::{
    apply_codex_environment_context, environment_context_rewrite_enabled,
    log_environment_context_report, process_environment_timezone, EnvironmentContextRewriteInput,
};
use crate::codex_profile::{
    apply_codex_client_identity_headers, apply_codex_concrete_account_profile_to_request,
    apply_codex_concrete_account_profile_to_request_with_body_policy,
    apply_codex_concrete_account_profile_to_search_headers, codex_account_selection_key,
    materialize_codex_key_fingerprint, resolve_codex_concrete_account_profile,
    strip_codex_client_metadata_from_body, CodexConcreteAccountProfile,
    CodexProfileMaterializationOutcome, CodexProfileMaterializeInput,
    CodexProfileRequestBodyPolicy,
};
use crate::codex_runtime_identity::{
    apply_outbound_codex_runtime_identity, codex_runtime_identity_rewrite_enabled,
    resolve_outbound_codex_runtime_identity, unix_millis, uuid_v7_unix_millis,
    CodexRuntimeIdentityResolution, CodexRuntimeIdentityScope, CodexRuntimeIdentityStore,
    CodexRuntimeIdentitySurface, InboundCodexRuntimeIdentity, OutboundCodexRuntimeIdentity,
};

pub(crate) use crate::ai_serving::{
    apply_codex_official_ws_handshake_headers, apply_codex_openai_responses_special_body_edits,
    apply_codex_openai_responses_special_headers,
};

use crate::ai_serving::GatewayProviderTransportSnapshot;

const DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES_JSON: &str =
    include_str!("../../../../../../resources/codex-client-header-profiles.json");

const CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST: &[&str] = &[
    "anthropic-version",
    "x-amz-user-agent",
    "x-amzn-codewhisperer-optout",
    "x-amzn-kiro-agent-mode",
    // Codex obtains this proof just in time from a capable Desktop host. A
    // pooled request can switch the upstream account and concrete profile, so
    // it must not reuse an inbound client attestation.
    "x-oai-attestation",
    // Aether's own request trace id (minted or taken from the downstream
    // relay). No codex-rs client sends it, and the same value follows one
    // inbound request across accounts on retry.
    "x-trace-id",
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct CodexClientHeaderProfile {
    user_agent: String,
    originator: String,
}

static DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES: LazyLock<Vec<CodexClientHeaderProfile>> =
    LazyLock::new(|| {
        serde_json::from_str(DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES_JSON)
            .expect("built-in Codex client header profiles must be valid JSON")
    });

pub(crate) fn apply_codex_pool_stable_client_headers(
    provider_request_headers: &mut BTreeMap<String, String>,
    transport: &GatewayProviderTransportSnapshot,
) {
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return;
    }
    remove_codex_pool_upstream_leak_headers(provider_request_headers);

    if let Some(profile) = resolve_codex_pool_concrete_account_profile(transport) {
        apply_codex_client_identity_headers(
            provider_request_headers,
            &profile.user_agent,
            &profile.originator,
        );
        return;
    }

    let default_pool_advanced = Value::Object(Default::default());
    let pool_advanced = transport
        .provider
        .config
        .as_ref()
        .and_then(|config| config.get("pool_advanced"))
        .unwrap_or(&default_pool_advanced);
    let selection_key = codex_pool_client_profile_selection_key(transport);
    let Some(header_profile) = codex_pool_client_header_profile(pool_advanced, &selection_key)
    else {
        return;
    };

    apply_codex_client_identity_headers(
        provider_request_headers,
        &header_profile.user_agent,
        &header_profile.originator,
    );
}

pub(crate) fn apply_codex_pool_search_account_profile(
    provider_request_headers: &mut BTreeMap<String, String>,
    transport: &GatewayProviderTransportSnapshot,
) {
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return;
    }
    remove_codex_pool_upstream_leak_headers(provider_request_headers);

    let Some(profile) = resolve_codex_pool_concrete_account_profile(transport) else {
        apply_codex_pool_stable_client_headers(provider_request_headers, transport);
        return;
    };
    apply_codex_concrete_account_profile_to_search_headers(provider_request_headers, &profile);
}

pub(crate) fn apply_codex_pool_concrete_account_profile(
    provider_request_headers: &mut BTreeMap<String, String>,
    provider_request_body: &mut Value,
    transport: &GatewayProviderTransportSnapshot,
) {
    apply_codex_pool_concrete_account_profile_with_body_policy(
        provider_request_headers,
        provider_request_body,
        transport,
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata,
    );
}

pub(crate) fn apply_codex_pool_concrete_account_profile_for_api_format(
    provider_request_headers: &mut BTreeMap<String, String>,
    provider_request_body: &mut Value,
    transport: &GatewayProviderTransportSnapshot,
    provider_api_format: &str,
) {
    apply_codex_pool_concrete_account_profile_with_body_policy(
        provider_request_headers,
        provider_request_body,
        transport,
        codex_profile_request_body_policy(provider_api_format),
    );
}

fn apply_codex_pool_concrete_account_profile_with_body_policy(
    provider_request_headers: &mut BTreeMap<String, String>,
    provider_request_body: &mut Value,
    transport: &GatewayProviderTransportSnapshot,
    body_policy: CodexProfileRequestBodyPolicy,
) {
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return;
    }
    remove_codex_pool_upstream_leak_headers(provider_request_headers);
    if body_policy == CodexProfileRequestBodyPolicy::StripClientMetadata
        && !crate::provider_transport::body_rules_handle_path(
            transport.endpoint.body_rules.as_ref(),
            "client_metadata",
        )
    {
        strip_codex_client_metadata_from_body(provider_request_body);
    }

    let Some(profile) = resolve_codex_pool_concrete_account_profile(transport) else {
        return;
    };
    apply_codex_concrete_account_profile_to_request_with_body_policy(
        provider_request_headers,
        provider_request_body,
        &profile,
        body_policy,
    );
}

fn codex_profile_request_body_policy(provider_api_format: &str) -> CodexProfileRequestBodyPolicy {
    if provider_api_format
        .trim()
        .eq_ignore_ascii_case("openai:responses:compact")
    {
        CodexProfileRequestBodyPolicy::StripClientMetadata
    } else {
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata
    }
}

pub(crate) fn materialize_codex_pool_key_fingerprint(
    provider_type: &str,
    provider_config: Option<&Value>,
    key_fingerprint: Option<&Value>,
    auth_config_raw: Option<&str>,
    key_id: &str,
    key_name: &str,
    now_unix_secs: u64,
) -> Option<CodexProfileMaterializationOutcome> {
    if !provider_type.trim().eq_ignore_ascii_case("codex") {
        return None;
    }
    let default_pool_advanced = Value::Object(Default::default());
    let pool_advanced = provider_config
        .and_then(|config| config.get("pool_advanced"))
        .unwrap_or(&default_pool_advanced);
    let selection_key = codex_account_selection_key(auth_config_raw, key_name, key_id);
    let header_profile = codex_pool_client_header_profile(pool_advanced, &selection_key)?;
    materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
        provider_type,
        fingerprint: key_fingerprint,
        auth_config_raw,
        key_id,
        key_name,
        user_agent: header_profile.user_agent.as_str(),
        originator: header_profile.originator.as_str(),
        now_unix_secs,
    })
}

pub(crate) fn refresh_codex_pool_key_fingerprint(
    provider_type: &str,
    provider_config: Option<&Value>,
    key_fingerprint: Option<&Value>,
    auth_config_raw: Option<&str>,
    key_id: &str,
    key_name: &str,
    now_unix_secs: u64,
) -> Option<CodexProfileMaterializationOutcome> {
    let refreshable_fingerprint = strip_codex_profile_client_headers(key_fingerprint);
    materialize_codex_pool_key_fingerprint(
        provider_type,
        provider_config,
        refreshable_fingerprint.as_ref(),
        auth_config_raw,
        key_id,
        key_name,
        now_unix_secs,
    )
}

/// Freezes an explicit `User-Agent`/`originator` pair into the key fingerprint,
/// replacing any previously persisted client headers while keeping the
/// installation identity. Used after an OAuth login/import so pool traffic keeps
/// the exact client identity the account authenticated with.
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_codex_pool_key_fingerprint_with_client_headers(
    provider_type: &str,
    key_fingerprint: Option<&Value>,
    auth_config_raw: Option<&str>,
    key_id: &str,
    key_name: &str,
    user_agent: &str,
    originator: &str,
    now_unix_secs: u64,
) -> Option<CodexProfileMaterializationOutcome> {
    let stripped_fingerprint = strip_codex_profile_client_headers(key_fingerprint);
    materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
        provider_type,
        fingerprint: stripped_fingerprint.as_ref(),
        auth_config_raw,
        key_id,
        key_name,
        user_agent,
        originator,
        now_unix_secs,
    })
}

/// Selects the `(user_agent, originator)` pool profile for `selection_key`
/// from the provider's `pool_advanced.codex_client_headers` (or the built-in
/// defaults). Returns `None` when client header profiles are disabled or the
/// provider is not codex. This is the same choice `materialize_codex_pool_key_fingerprint`
/// makes, exposed so the OAuth login can advertise the identity up front.
pub(crate) fn select_codex_pool_client_header_profile(
    provider_type: &str,
    provider_config: Option<&Value>,
    selection_key: &str,
) -> Option<(String, String)> {
    if !provider_type.trim().eq_ignore_ascii_case("codex") {
        return None;
    }
    let default_pool_advanced = Value::Object(Default::default());
    let pool_advanced = provider_config
        .and_then(|config| config.get("pool_advanced"))
        .unwrap_or(&default_pool_advanced);
    codex_pool_client_header_profile(pool_advanced, selection_key)
        .map(|profile| (profile.user_agent, profile.originator))
}

fn strip_codex_profile_client_headers(key_fingerprint: Option<&Value>) -> Option<Value> {
    let mut stripped = key_fingerprint.cloned();
    if let Some(profile) = stripped
        .as_mut()
        .and_then(Value::as_object_mut)
        .and_then(|root| root.get_mut(crate::codex_profile::CODEX_CLIENT_PROFILE_KEY))
        .and_then(Value::as_object_mut)
    {
        profile.remove("client_headers");
        profile.remove("user_agent");
        profile.remove("user-agent");
        profile.remove("originator");
        profile.remove("frozen_at_unix_secs");
    }
    stripped
}

pub(crate) fn validate_codex_client_header_config(value: &Value) -> Result<(), String> {
    let config = value
        .as_object()
        .ok_or_else(|| "codex_client_headers 必须是 JSON 对象".to_string())?;
    if let Some(enabled) = config.get("enabled") {
        let enabled = enabled
            .as_bool()
            .ok_or_else(|| "codex_client_headers.enabled 必须是布尔值".to_string())?;
        if !enabled {
            return Err("Codex 稳定客户端请求头已关闭，无法更新账号 UA".to_string());
        }
    }
    let Some(profiles) = config.get("profiles") else {
        return Ok(());
    };
    if profiles.is_null() {
        return Ok(());
    }
    let profiles = profiles
        .as_array()
        .ok_or_else(|| "codex_client_headers.profiles 必须是数组".to_string())?;
    let mut seen = BTreeSet::new();
    for (index, profile) in profiles.iter().enumerate() {
        let profile = profile
            .as_object()
            .ok_or_else(|| format!("第 {} 组 Codex 请求头必须是对象", index + 1))?;
        let user_agent = profile
            .get("user_agent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("第 {} 组 User-Agent 不能为空", index + 1))?;
        let originator = profile
            .get("originator")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("第 {} 组 Originator 不能为空", index + 1))?;
        if !seen.insert((user_agent, originator)) {
            return Err(format!("第 {} 组 Codex 请求头与已有配置重复", index + 1));
        }
    }
    Ok(())
}

fn codex_pool_client_profile_selection_key(transport: &GatewayProviderTransportSnapshot) -> String {
    codex_account_selection_key(
        transport.key.decrypted_auth_config.as_deref(),
        transport.key.name.as_str(),
        transport.key.id.as_str(),
    )
}

pub(crate) fn resolve_codex_pool_concrete_account_profile(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<CodexConcreteAccountProfile> {
    let default_pool_advanced = Value::Object(Default::default());
    let pool_advanced = transport
        .provider
        .config
        .as_ref()
        .and_then(|config| config.get("pool_advanced"))
        .unwrap_or(&default_pool_advanced);
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return None;
    }

    let selection_key = codex_pool_client_profile_selection_key(transport);
    let header_profile = codex_pool_client_header_profile(pool_advanced, &selection_key)?;
    resolve_codex_concrete_account_profile(
        transport.key.fingerprint.as_ref(),
        transport.key.decrypted_auth_config.as_deref(),
        transport.key.id.as_str(),
        transport.key.name.as_str(),
        header_profile.user_agent.as_str(),
        header_profile.originator.as_str(),
    )
}

/// Outbound runtime identity synthesis scope for the selected Codex pool
/// account. `None` when the provider is not Codex or the
/// `pool_advanced.codex_runtime_identity` switch is off/invalid.
///
/// The scope is keyed by the same account selection key as the client header
/// profile, so one upstream account always owns one synthetic tree.
pub(crate) fn resolve_codex_pool_runtime_identity_scope(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<CodexRuntimeIdentityScope> {
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return None;
    }
    let pool_advanced = transport
        .provider
        .config
        .as_ref()
        .and_then(|config| config.get("pool_advanced"));
    let config =
        codex_runtime_identity_rewrite_enabled(pool_advanced, transport.provider.id.as_str())?;
    let selection_key = codex_pool_client_profile_selection_key(transport);
    Some(CodexRuntimeIdentityScope::new(
        transport.provider.id.as_str(),
        &selection_key,
        config,
    ))
}

/// HTTP-side runtime identity pass. Runs after key selection, the shared
/// special-header pass and the concrete account profile, so it only rewrites
/// projections that still equal the inbound official identity.
///
/// `original_body` / `original_headers` are the client's request as accepted
/// (before Aether fillers), which decides what the client really sent.
/// Returns the outbound identity when a rewrite happened.
pub(crate) async fn apply_codex_pool_runtime_identity(
    runtime: &RuntimeState,
    transport: &GatewayProviderTransportSnapshot,
    provider_request_headers: &mut BTreeMap<String, String>,
    mut provider_request_body: Option<&mut Value>,
    original_headers: &HeaderMap,
    original_body: Option<&Value>,
    surface: CodexRuntimeIdentitySurface,
) -> Option<OutboundCodexRuntimeIdentity> {
    let scope = resolve_codex_pool_runtime_identity_scope(transport);
    let mut inbound =
        InboundCodexRuntimeIdentity::from_request(original_body, Some(original_headers));
    if scope.is_some() && surface == CodexRuntimeIdentitySurface::HttpResponses {
        // A `/responses` egress without any official identity (a relay that
        // strips codex headers in front of a real client, or a chat/family
        // request converted to Responses) gets a content-derived root, so the
        // account never shows a Codex user-agent without a thread. Prompts are
        // read from the client's body when it is a Responses body, otherwise
        // from the converted wire body. Compact and header-only surfaces stay
        // passthrough.
        let content = match original_body {
            Some(body) if body.get("input").is_some() => Some(body),
            _ => provider_request_body.as_deref(),
        };
        inbound.synthesize_missing_root(content, original_headers);
    }
    let now = SystemTime::now();
    let outbound = if let Some(scope) = scope.as_ref() {
        let store = CodexRuntimeIdentityStore::new(runtime);
        match resolve_outbound_codex_runtime_identity(&store, scope, &inbound, None, now).await {
            CodexRuntimeIdentityResolution::Rewrite(outbound) => {
                apply_outbound_codex_runtime_identity(
                    provider_request_headers,
                    provider_request_body.as_deref_mut(),
                    &inbound,
                    &outbound,
                    surface,
                    None,
                );
                Some(outbound)
            }
            CodexRuntimeIdentityResolution::Passthrough => None,
        }
    } else {
        None
    };
    // The model-visible host clock (`<timezone>` / `<current_date>`) is
    // normalized after the identity pass so the seed is the outbound thread.
    // Runs on passthrough too: a Redis outage must not leak the client's zone.
    if scope.is_some() {
        if let Some(body) = provider_request_body.as_deref_mut() {
            let (thread_id, turn_id) = match &outbound {
                Some(outbound) => (outbound.thread_id.as_str(), outbound.turn_id.as_deref()),
                None => (
                    inbound.thread_id.as_deref().unwrap_or_default(),
                    inbound.turn_id.as_deref(),
                ),
            };
            apply_codex_pool_environment_context(body, surface, thread_id, turn_id, now);
        }
    }

    // Endpoint request-header rules are an explicit operator override.  Apply
    // them after Codex identity sanitation so a configured header is not
    // silently removed by the pool's inbound-metadata filters.  Authentication
    // and content type remain protected by the same safeguards as the initial
    // rule pass.
    let protected = ["authorization", "api-key", "x-api-key", "content-type"];
    let _ = crate::provider_transport::apply_local_header_rules_with_request_headers(
        provider_request_headers,
        transport.endpoint.header_rules.as_ref(),
        &protected,
        provider_request_body.as_deref().unwrap_or(&Value::Null),
        original_body,
        Some(original_headers),
    );
    outbound
}

/// Rewrites `<environment_context>` host-clock values for the HTTP surfaces
/// that carry `input[]`. Header-only surfaces have no body; chat/family
/// conversions have no environment blocks and come out untouched.
pub(crate) fn apply_codex_pool_environment_context(
    body: &mut Value,
    surface: CodexRuntimeIdentitySurface,
    outbound_thread_id: &str,
    outbound_turn_id: Option<&str>,
    now: SystemTime,
) {
    if !environment_context_rewrite_enabled() {
        return;
    }
    let surface_name = match surface {
        CodexRuntimeIdentitySurface::HttpResponses => "http_responses",
        CodexRuntimeIdentitySurface::HttpCompact => "http_compact",
        _ => return,
    };
    let input = EnvironmentContextRewriteInput {
        tz: process_environment_timezone().tz,
        now_unix_ms: unix_millis(now),
        turn_started_at_unix_ms: outbound_turn_id.and_then(uuid_v7_unix_millis),
        turn_id: outbound_turn_id,
        outbound_thread_id,
        allow_tail_append: surface == CodexRuntimeIdentitySurface::HttpResponses,
        prior_state: None,
    };
    let (report, _) = apply_codex_environment_context(body, &input);
    log_environment_context_report(surface_name, outbound_thread_id, &report);
}

fn remove_codex_pool_upstream_leak_headers(
    provider_request_headers: &mut BTreeMap<String, String>,
) {
    let headers_to_remove = provider_request_headers
        .keys()
        .filter(|candidate| {
            CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST
                .iter()
                .any(|blocked| candidate.eq_ignore_ascii_case(blocked))
        })
        .cloned()
        .collect::<Vec<_>>();
    for header in headers_to_remove {
        provider_request_headers.remove(&header);
    }
}

fn codex_pool_client_header_profile(
    pool_advanced: &Value,
    selection_key: &str,
) -> Option<CodexClientHeaderProfile> {
    let header_config = pool_advanced.get("codex_client_headers");
    if header_config
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return None;
    }

    let profiles = header_config
        .and_then(|value| value.get("profiles"))
        .and_then(parse_codex_client_header_profiles)
        .unwrap_or_else(default_codex_client_header_profiles);
    if profiles.is_empty() {
        return None;
    }
    Some(profiles[stable_index_for_key(selection_key, &profiles)].clone())
}

fn parse_codex_client_header_profiles(value: &Value) -> Option<Vec<CodexClientHeaderProfile>> {
    let profiles = value.as_array()?;
    let parsed = profiles
        .iter()
        .filter_map(|profile| {
            let object = profile.as_object()?;
            let user_agent = object
                .get("user_agent")
                .or_else(|| object.get("user-agent"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let originator = object
                .get("originator")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(CodexClientHeaderProfile {
                user_agent: user_agent.to_string(),
                originator: originator.to_string(),
            })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn default_codex_client_header_profiles() -> Vec<CodexClientHeaderProfile> {
    DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES.clone()
}

fn stable_index_for_key(selection_key: &str, profiles: &[CodexClientHeaderProfile]) -> usize {
    profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| (index, stable_profile_score(selection_key, profile)))
        .max_by(|(_, left), (_, right)| left.cmp(right))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn stable_profile_score(selection_key: &str, profile: &CodexClientHeaderProfile) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(selection_key.as_bytes());
    hasher.update([0]);
    hasher.update(profile.user_agent.as_bytes());
    hasher.update([0]);
    hasher.update(profile.originator.as_bytes());

    let digest = hasher.finalize();
    let mut score = [0_u8; 32];
    score.copy_from_slice(&digest);
    score
}
