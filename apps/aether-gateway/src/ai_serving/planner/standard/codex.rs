#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::codex_profile::{
    apply_codex_concrete_account_profile_to_request,
    apply_codex_concrete_account_profile_to_request_with_body_policy,
    apply_codex_concrete_account_profile_to_search_headers, codex_account_selection_key,
    materialize_codex_key_fingerprint, resolve_codex_concrete_account_profile,
    strip_codex_client_metadata_from_body, CodexConcreteAccountProfile,
    CodexProfileMaterializationOutcome, CodexProfileMaterializeInput,
    CodexProfileRequestBodyPolicy,
};

pub(crate) use crate::ai_serving::{
    apply_codex_official_ws_handshake_headers, apply_codex_openai_responses_special_body_edits,
    apply_codex_openai_responses_special_headers,
};

use crate::ai_serving::GatewayProviderTransportSnapshot;

const DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES: &[(&str, &str)] = &[
    (
        "codex-tui/0.142.0 (Mac OS 26.4.1; arm64) iTerm.app/3.6.10 (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Windows 10.0.26200; x86_64) WindowsTerminal (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Debian 13.0.0; x86_64) xterm-256color (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Ubuntu 22.4.0; x86_64) WindowsTerminal (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Ubuntu 24.4.0; x86_64) WindowsTerminal (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Ubuntu 24.4.0; x86_64) WezTerm/20240203-110809-5046fc22 (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Mac OS 26.2.0; arm64) xterm-256color (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Mac OS 15.6.1; arm64) Apple_Terminal (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Windows 10.0.26200; x86_64) WarpTerminal (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.142.0 (Mac OS 26.5.1; arm64) ghostty/1.3.1 (codex-tui; 0.142.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.141.0 (Debian 13.0.0; x86_64) xterm-256color (codex-tui; 0.141.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.141.0 (Mac OS 15.7.5; arm64) iTerm.app/3.6.6 (codex-tui; 0.141.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.141.0 (Windows 10.0.26200; x86_64) waveterm (codex-tui; 0.141.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.141.0 (Mac OS 26.2.0; arm64) vscode/1.125.0 (codex-tui; 0.141.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.134.0 (Mac OS 14.1.0; arm64) iTerm.app/3.6.9 (codex-tui; 0.134.0)",
        "codex-tui",
    ),
    (
        "Codex Desktop/0.142.0 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.616.71553)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0 (Windows 10.0.19045; x86_64) unknown (Codex Desktop; 26.616.81150)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0 (Mac OS 26.5.1; arm64) unknown (Codex Desktop; 26.616.71553)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0-alpha.6 (Mac OS 26.5.0; arm64) unknown (Codex Desktop; 26.616.51431)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.616.81150)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0 (Mac OS 26.5.0; arm64) unknown (Codex Desktop; 26.616.81150)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0 (Mac OS 14.1.0; arm64) unknown (Codex Desktop; 26.616.81150)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.142.0 (Mac OS 13.1.0; x86_64) unknown (Codex Desktop; 26.616.81150)",
        "Codex Desktop",
    ),
    (
        "codex_vscode/0.142.0 (Windows 10.0.19045; x86_64) unknown (VS Code; 26.616.81150)",
        "codex_vscode",
    ),
    (
        "codex_vscode/0.142.0-alpha.1 (Windows 10.0.22631; x86_64) unknown (Windsurf; 26.616.32156)",
        "codex_vscode",
    ),
    (
        "codex_vscode/0.142.0 (Windows 10.0.22631; x86_64) unknown (Antigravity IDE; 26.616.71553)",
        "codex_vscode",
    ),
    (
        "codex_cli_rs/0.93.0 (Windows 10.0.26200; x86_64) vscode/1.108.1",
        "codex_cli",
    ),
    (
        "codex_cli_rs/0.133.0 (Windows 10.0.26200; x64)",
        "codex_cli_rs",
    ),
    (
        "codex_cli_rs/0.125.0 (Mac OS 24.6.0; arm64)",
        "codex_cli_rs",
    ),
    (
        "codex_cli_rs/0.77.0 (Windows 10.0.26100; x86_64) WindowsTerminal",
        "codex_cli_rs",
    ),
    (
        "codex_exec/0.142.0 (Mac OS 15.7.5; arm64) iTerm.app/3.6.6 (codex_exec; 0.142.0)",
        "codex_exec",
    ),
    (
        "codex_sdk_ts/0.136.0 (Windows 10.0.19045; x86_64) unknown (codex_exec; 0.136.0)",
        "codex_sdk_ts",
    ),
];

const CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST: &[&str] = &[
    "anthropic-version",
    "x-amz-user-agent",
    "x-amzn-codewhisperer-optout",
    "x-amzn-kiro-agent-mode",
    // Codex obtains this proof just in time from a capable Desktop host. A
    // pooled request can switch the upstream account and concrete profile, so
    // it must not reuse an inbound client attestation.
    "x-oai-attestation",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexClientHeaderProfile {
    user_agent: String,
    originator: String,
}

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
        provider_request_headers.insert("user-agent".to_string(), profile.user_agent);
        provider_request_headers.insert("originator".to_string(), profile.originator);
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

    provider_request_headers.insert("user-agent".to_string(), header_profile.user_agent);
    provider_request_headers.insert("originator".to_string(), header_profile.originator);
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
    if body_policy == CodexProfileRequestBodyPolicy::StripClientMetadata {
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
    DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES
        .iter()
        .map(|(user_agent, originator)| CodexClientHeaderProfile {
            user_agent: (*user_agent).to_string(),
            originator: (*originator).to_string(),
        })
        .collect()
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
