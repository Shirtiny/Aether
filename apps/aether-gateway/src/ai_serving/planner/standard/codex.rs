#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) use crate::ai_serving::{
    apply_codex_openai_responses_special_body_edits, apply_codex_openai_responses_special_headers,
};

use crate::ai_serving::GatewayProviderTransportSnapshot;

const DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES: &[(&str, &str)] = &[
    (
        "codex-tui/0.139.0 (Windows 10.0.26200; x86_64) WindowsTerminal (codex-tui; 0.139.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.125.0 (Windows 10.0.26100; x86_64) WindowsTerminal (codex-tui; 0.125.0)",
        "codex-tui",
    ),
    (
        "Codex Desktop/0.140.0-alpha.2 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.609.30741)",
        "Codex Desktop",
    ),
    (
        "codex_vscode/0.129.0-alpha.15 (Windows 10.0.26200; x86_64) unknown (VS Code; 26.506.21252)",
        "codex_vscode",
    ),
    (
        "Codex Desktop/0.133.0 (Windows 10.0.22631; x86_64) unknown (Codex Desktop; 26.519.81530)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.136.0-alpha.2 (Windows 10.0.19044; x86_64) unknown (Codex Desktop; 26.601.21317)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.136.0-alpha.2 (Windows 10.0.22631; x86_64) unknown (Codex Desktop; 26.601.21317)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.138.0-alpha.7 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.608.12217)",
        "Codex Desktop",
    ),
    (
        "codex_vscode/0.140.0-alpha.2 (Windows 10.0.26200; x86_64) unknown (VS Code; 26.609.30741)",
        "codex_vscode",
    ),
    (
        "codex_vscode/0.140.0-alpha.2 (Windows 10.0.22631; x86_64) unknown (Antigravity IDE; 26.609.30741)",
        "codex_vscode",
    ),
    (
        "codex-tui/0.131.0 (Mac OS 15.7.5; arm64) iTerm.app/3.6.6 (codex-tui; 0.131.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.139.0 (Mac OS 15.5.0; arm64) xterm-256color (codex-tui; 0.139.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.139.0 (Mac OS 12.5.1; x86_64) iTerm.app/3.5.14 (codex-tui; 0.139.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.139.0 (Mac OS 14.1.0; arm64) iTerm.app/3.6.9 (codex-tui; 0.139.0)",
        "codex-tui",
    ),
    (
        "codex-tui/0.139.0 (Mac OS 26.5.1; arm64) ghostty/1.3.1 (codex-tui; 0.139.0)",
        "codex-tui",
    ),
    (
        "Codex Desktop/0.140.0-alpha.2 (Mac OS 15.7.7; x86_64) unknown (Codex Desktop; 26.609.41114)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.133.0-alpha.1 (Mac OS 26.1.0; arm64) unknown (Codex Desktop; 26.519.22136)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.138.0-alpha.7 (Mac OS 26.1.0; arm64) unknown (Codex Desktop; 26.608.12217)",
        "Codex Desktop",
    ),
    (
        "Codex Desktop/0.140.0-alpha.2 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.609.30741)",
        "Codex Desktop",
    ),
    (
        "codex_cli_rs/0.137.0 (Mac OS 26.4.0; arm64) unknown",
        "codex_cli_rs",
    ),
];

const CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST: &[&str] = &[
    "anthropic-version",
    "x-amz-user-agent",
    "x-amzn-codewhisperer-optout",
    "x-amzn-kiro-agent-mode",
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
    let Some(pool_advanced) = transport
        .provider
        .config
        .as_ref()
        .and_then(|config| config.get("pool_advanced"))
    else {
        return;
    };
    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return;
    }
    remove_codex_pool_upstream_leak_headers(provider_request_headers);

    let Some(profile) =
        codex_pool_client_header_profile(pool_advanced, transport.key.name.as_str())
    else {
        return;
    };

    provider_request_headers.insert("user-agent".to_string(), profile.user_agent);
    provider_request_headers.insert("originator".to_string(), profile.originator);
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
    key_id: &str,
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
    Some(profiles[stable_index_for_key(key_id, profiles.len())].clone())
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

fn stable_index_for_key(key_id: &str, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let digest = Sha256::digest(key_id.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) as usize) % len
}
