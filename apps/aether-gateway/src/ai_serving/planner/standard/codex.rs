#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{LazyLock, OnceLock};
use std::time::SystemTime;

use aether_contracts::{
    EXECUTION_COOKIE_JAR_CHATGPT_CLOUDFLARE, EXECUTION_HEADER_ORDER_CODEX_CLI,
    EXECUTION_REQUEST_BODY_ENCODING_HEADER, EXECUTION_REQUEST_BODY_ENCODING_ZSTD,
    EXECUTION_REQUEST_COOKIE_JAR_HEADER, EXECUTION_REQUEST_HEADER_ORDER_HEADER,
};
use aether_runtime_state::RuntimeState;
use http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::codex_client_release::{
    observe_codex_client_release, resolve_codex_client_user_agent, unix_now_secs,
    ClientReleaseStore,
};
use crate::codex_environment_context::{
    apply_codex_environment_context, environment_context_rewrite_enabled,
    log_environment_context_report, process_environment_timezone, rewrite_switch_enabled,
    EnvironmentContextRewriteInput,
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
use crate::execution_runtime::chatgpt_cloudflare_cookies::is_allowed_chatgpt_host;

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

/// Client-release follow for one selected pool account, for the surfaces that
/// only carry headers (a Responses WebSocket handshake, whose step bodies get
/// their client identity from the frozen headers the runtime composes).
/// Returns the effective user-agent when it moved off the frozen one.
pub(crate) async fn apply_codex_pool_client_release_headers(
    runtime: &RuntimeState,
    transport: &GatewayProviderTransportSnapshot,
    provider_request_headers: &mut BTreeMap<String, String>,
    original_headers: &HeaderMap,
) -> Option<String> {
    let now_unix_secs = unix_now_secs();
    apply_codex_pool_client_release(
        runtime,
        transport,
        provider_request_headers,
        original_headers,
        now_unix_secs,
    )
    .await
}

/// Client-release follow for one selected pool account: records the stable
/// Codex version the inbound client is running, then moves this account's
/// frozen `user-agent` and `version` header up to the newest build that has
/// been out for at least the account's per-account lag. Best-effort: any
/// registry failure leaves the frozen user-agent in place.
///
/// The originator is never touched: it comes from the profile, not from the
/// build, and the profile pass wrote it moments earlier.
async fn apply_codex_pool_client_release(
    runtime: &RuntimeState,
    transport: &GatewayProviderTransportSnapshot,
    provider_request_headers: &mut BTreeMap<String, String>,
    original_headers: &HeaderMap,
    now_unix_secs: u64,
) -> Option<String> {
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return None;
    }
    let store = ClientReleaseStore::new(runtime, transport.provider.id.as_str());
    observe_codex_client_release(
        &store,
        original_headers
            .get(http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok()),
        now_unix_secs,
    )
    .await;
    let Some((_, frozen_user_agent)) = header_entry(provider_request_headers, "user-agent") else {
        return None;
    };
    let frozen_user_agent = frozen_user_agent.to_string();
    let selection_key = codex_pool_client_profile_selection_key(transport);
    let selection_fp = crate::codex_runtime_identity::codex_selection_fingerprint(&selection_key);
    let effective_user_agent =
        resolve_codex_client_user_agent(&store, &frozen_user_agent, &selection_fp, now_unix_secs)
            .await;
    if effective_user_agent == frozen_user_agent {
        return None;
    }
    let originator = header_entry(provider_request_headers, "originator")
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    apply_codex_client_identity_headers(
        provider_request_headers,
        &effective_user_agent,
        &originator,
    );
    log_codex_client_release_follow(&frozen_user_agent, &effective_user_agent);
    Some(effective_user_agent)
}

fn log_codex_client_release_follow(frozen_user_agent: &str, effective_user_agent: &str) {
    tracing::debug!(
        target: "aether_gateway::codex_client_release",
        frozen_user_agent,
        effective_user_agent,
        "codex pool client release follow"
    );
}

/// Case-insensitive lookup into the outbound header map, matching the
/// profile module's own convention.
fn header_entry<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<(&'a String, &'a String)> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
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
    let now = SystemTime::now();
    let now_unix_secs = unix_now_secs();
    // Client-release follow: the frozen user-agent names one build forever, so
    // the pool drifts behind every codex-rs release. Observe what real clients
    // are running (inbound, before any Aether rewrite) and let this account's
    // frozen user-agent move up to a build that has been out long enough for
    // it. Runs whether or not the runtime-identity switch is on: it only ever
    // rewrites the version tokens of the user-agent plus the `version` header,
    // and both already come from the pool profile.
    let _ = apply_codex_pool_client_release(
        runtime,
        transport,
        provider_request_headers,
        original_headers,
        now_unix_secs,
    )
    .await;
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
    // The profile pass put `x-codex-installation-id` on the headers so the
    // identity pass above could read it into the turn-metadata blob and
    // `client_metadata`; the official client only ever sends the header itself
    // on `/responses/compact`, so it comes off the other surfaces here.
    align_codex_installation_id_header_with_surface(
        transport,
        provider_request_headers,
        surface,
        codex_transport_fidelity_enabled(),
    );

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
    // Transport-level fidelity (header order, body encoding, cookie jar) is
    // decided last: it depends only on where the request goes, never on the
    // identity switch, and the transport strips the controls before egress.
    apply_codex_transport_fidelity_controls(
        transport,
        provider_request_headers,
        surface,
        codex_transport_fidelity_enabled(),
    );
    outbound
}

/// Process-level kill switch for [`apply_codex_transport_fidelity_controls`]
/// (`AETHER_CODEX_TRANSPORT_FIDELITY=off|0|false|disabled|no`).
fn codex_transport_fidelity_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        rewrite_switch_enabled(
            std::env::var("AETHER_CODEX_TRANSPORT_FIDELITY")
                .ok()
                .as_deref(),
        )
    })
}

/// Asks the execution transport to put a Codex pool request on the wire the
/// way codex-rs does when it talks to the ChatGPT backend.
///
/// A direct-login codex-rs 0.154.0 capture shows, on every
/// `/backend-api/codex/*` request: a fixed header order (`version`,
/// `x-codex-beta-features`, …, `session-id`, `thread-id`, then `accept`,
/// `content-encoding`, `content-type`, `authorization`, `chatgpt-account-id`,
/// `originator`, `user-agent`, `cookie`, `host`), a zstd-compressed
/// `/responses` body (`content-encoding: zstd`, `http-client/src/request.rs`,
/// `EnableRequestCompression` stable since 0.120.0) and Cloudflare's
/// `_cfuvid` / `__cf_bm` / `__cflb` cookies replayed from a process-wide jar
/// (`http-client/src/chatgpt_cloudflare_cookies.rs`, ≥ 0.143.0). Aether used
/// to send alphabetical headers, identity bodies and no cookie at all, which
/// is what no real client looks like.
///
/// The controls are `x-aether-execution-*` headers consumed and stripped by
/// the execution transport; nothing here reaches the upstream as a header.
/// Only Codex providers whose endpoint targets a ChatGPT host over `https`
/// qualify; relays and mirrors keep the plain behaviour. `/responses/compact`
/// and the header-only surfaces are not compressed because codex-rs only
/// compresses the `/responses` stream body.
pub(crate) fn apply_codex_transport_fidelity_controls(
    transport: &GatewayProviderTransportSnapshot,
    provider_request_headers: &mut BTreeMap<String, String>,
    surface: CodexRuntimeIdentitySurface,
    enabled: bool,
) {
    if !codex_wire_shape_alignment_applies(transport, enabled) {
        return;
    }
    provider_request_headers.insert(
        EXECUTION_REQUEST_HEADER_ORDER_HEADER.to_string(),
        EXECUTION_HEADER_ORDER_CODEX_CLI.to_string(),
    );
    provider_request_headers.insert(
        EXECUTION_REQUEST_COOKIE_JAR_HEADER.to_string(),
        EXECUTION_COOKIE_JAR_CHATGPT_CLOUDFLARE.to_string(),
    );
    if surface == CodexRuntimeIdentitySurface::HttpResponses {
        provider_request_headers.insert(
            EXECUTION_REQUEST_BODY_ENCODING_HEADER.to_string(),
            EXECUTION_REQUEST_BODY_ENCODING_ZSTD.to_string(),
        );
    }
}

/// Drops the `x-codex-installation-id` request header everywhere but on
/// `/responses/compact`, the only request codex-rs puts it on
/// (`core/src/client.rs`: it leads the compact extra headers, while
/// `build_responses_options` and `compatibility_headers()` never add it; the
/// 0.154.0 capture shows no such header on `/responses`, `/alpha/search`,
/// `/models` or the analytics beacon). The installation id still travels
/// where the official client carries it — inside `x-codex-turn-metadata` and
/// the body `client_metadata` — which the profile and identity passes filled
/// before this runs. Same scope and kill switch as
/// [`apply_codex_transport_fidelity_controls`].
pub(crate) fn align_codex_installation_id_header_with_surface(
    transport: &GatewayProviderTransportSnapshot,
    provider_request_headers: &mut BTreeMap<String, String>,
    surface: CodexRuntimeIdentitySurface,
    enabled: bool,
) {
    if surface == CodexRuntimeIdentitySurface::HttpCompact
        || !codex_wire_shape_alignment_applies(transport, enabled)
    {
        return;
    }
    provider_request_headers.retain(|name, _| {
        !name
            .trim()
            .eq_ignore_ascii_case(X_CODEX_INSTALLATION_ID_HEADER)
    });
}

const X_CODEX_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";

/// The wire-shape alignment covers a Codex provider whose endpoint targets a
/// ChatGPT host over `https`, unless the kill switch is thrown.
fn codex_wire_shape_alignment_applies(
    transport: &GatewayProviderTransportSnapshot,
    enabled: bool,
) -> bool {
    enabled
        && transport
            .provider
            .provider_type
            .trim()
            .eq_ignore_ascii_case("codex")
        && is_chatgpt_backend_base_url(transport.endpoint.base_url.as_str())
}

fn is_chatgpt_backend_base_url(base_url: &str) -> bool {
    Url::parse(base_url.trim())
        .ok()
        .filter(|url| url.scheme() == "https")
        .and_then(|url| url.host_str().map(is_allowed_chatgpt_host))
        .unwrap_or(false)
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
