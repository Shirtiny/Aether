use std::collections::BTreeMap;

use super::{
    apply_codex_openai_responses_special_body_edits, apply_codex_openai_responses_special_headers,
    apply_codex_pool_stable_client_headers,
};
use crate::ai_serving::{
    transport::snapshot::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider,
    },
    GatewayProviderTransportSnapshot,
};
use http::{HeaderMap, HeaderValue};
use serde_json::{json, Value};

fn sample_transport(
    provider_type: &str,
    provider_config: Option<Value>,
) -> GatewayProviderTransportSnapshot {
    GatewayProviderTransportSnapshot {
        provider: GatewayProviderTransportProvider {
            id: "provider-codex".to_string(),
            name: "Codex".to_string(),
            provider_type: provider_type.to_string(),
            website: None,
            is_active: true,
            keep_priority_on_conversion: false,
            enable_format_conversion: true,
            concurrent_limit: None,
            max_retries: None,
            proxy: None,
            request_timeout_secs: None,
            stream_first_byte_timeout_secs: None,
            config: provider_config,
        },
        endpoint: GatewayProviderTransportEndpoint {
            id: "endpoint-codex".to_string(),
            provider_id: "provider-codex".to_string(),
            api_format: "openai:responses".to_string(),
            api_family: None,
            endpoint_kind: None,
            is_active: true,
            base_url: "https://example.test/v1".to_string(),
            header_rules: None,
            body_rules: None,
            max_retries: None,
            custom_path: None,
            config: None,
            format_acceptance_config: None,
            proxy: None,
        },
        key: GatewayProviderTransportKey {
            id: "key-codex-1".to_string(),
            provider_id: "provider-codex".to_string(),
            name: "account".to_string(),
            auth_type: "oauth".to_string(),
            is_active: true,
            api_formats: None,
            auth_type_by_format: None,
            allow_auth_channel_mismatch_formats: None,
            allowed_models: None,
            capabilities: None,
            rate_multipliers: None,
            global_priority_by_format: None,
            expires_at_unix_secs: None,
            proxy: None,
            fingerprint: None,
            upstream_metadata: None,
            decrypted_api_key: "token".to_string(),
            decrypted_auth_config: Some(r#"{"account_id":"acc-123"}"#.to_string()),
        },
    }
}

fn codex_header_profile_value(user_agent: &str, originator: &str) -> Value {
    json!({
        "user_agent": user_agent,
        "originator": originator,
    })
}

#[test]
fn applies_codex_defaults_when_body_rules_do_not_handle_fields() {
    let mut body = json!({
        "model": "gpt-5",
        "max_output_tokens": 128,
        "temperature": 0.3,
        "top_p": 0.9,
        "metadata": {"client": "desktop"},
        "store": true
    });

    apply_codex_openai_responses_special_body_edits(
        &mut body,
        "codex",
        "openai:responses",
        None,
        None,
    );

    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("metadata").is_none());
    assert_eq!(body["store"], false);
    assert_eq!(body["instructions"], "");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["parallel_tool_calls"], true);
    assert!(body.get("reasoning").is_none());
}

#[test]
fn strips_store_for_compact_even_when_body_rules_handle_it() {
    let body_rules = json!([
        {"action":"set","path":"store","value":true},
        {"action":"set","path":"instructions","value":"Keep custom"},
        {"action":"set","path":"metadata","value":{"client":"desktop","mode":"custom"}},
        {"action":"set","path":"top_p","value":0.5}
    ]);
    let mut body = json!({
        "model": "gpt-5",
        "max_output_tokens": 128,
        "metadata": {"client": "desktop", "mode": "custom"},
        "store": true,
        "instructions": "Keep custom",
        "top_p": 0.5
    });

    apply_codex_openai_responses_special_body_edits(
        &mut body,
        "codex",
        "openai:responses:compact",
        Some(&body_rules),
        None,
    );

    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("store").is_none());
    assert_eq!(body["instructions"], "Keep custom");
    assert_eq!(body["metadata"]["mode"], "custom");
    assert_eq!(body["top_p"], 0.5);
}

#[test]
fn injects_stable_prompt_cache_key_for_codex_requests() {
    let mut body = json!({
        "model": "gpt-5",
        "input": "hello",
    });

    apply_codex_openai_responses_special_body_edits(
        &mut body,
        "codex",
        "openai:responses",
        None,
        Some("key-123"),
    );

    assert_eq!(
        body["prompt_cache_key"],
        "53363264-dbb0-5f9d-b9c7-3e92c45c5bdf"
    );
}

#[test]
fn keeps_existing_prompt_cache_key_for_codex_requests() {
    let mut body = json!({
        "model": "gpt-5",
        "input": "hello",
        "prompt_cache_key": "existing-key",
    });

    apply_codex_openai_responses_special_body_edits(
        &mut body,
        "codex",
        "openai:responses",
        None,
        Some("key-123"),
    );

    assert_eq!(body["prompt_cache_key"], "existing-key");
}

#[test]
fn injects_chatgpt_account_id_and_session_headers_for_codex_requests() {
    let mut headers = BTreeMap::new();
    let body = json!({
        "model": "gpt-5",
        "prompt_cache_key": "172c39e6-c0a0-5a70-8b63-e0f8e0d185a3",
    });

    apply_codex_openai_responses_special_headers(
        &mut headers,
        &body,
        &HeaderMap::new(),
        "codex",
        "openai:responses",
        Some("trace-codex-123"),
        Some(r#"{"account_id":"acc-123"}"#),
    );

    assert_eq!(
        headers.get("chatgpt-account-id"),
        Some(&"acc-123".to_string())
    );
    assert_eq!(
        headers.get("x-client-request-id"),
        Some(&"trace-codex-123".to_string())
    );
    assert_eq!(
        headers.get("user-agent"),
        Some(
            &"codex-tui/0.122.0 (Mac OS 15.2.0; arm64) vscode/2.6.11 (codex-tui; 0.122.0)"
                .to_string()
        )
    );
    assert_eq!(headers.get("originator"), Some(&"codex-tui".to_string()));
    assert_eq!(
        headers.get("session_id"),
        Some(&"ab5ecce4f0d110fe".to_string())
    );
    assert_eq!(
        headers.get("conversation_id"),
        Some(&"ab5ecce4f0d110fe".to_string())
    );
}

#[test]
fn respects_existing_codex_request_and_session_headers() {
    let mut headers = BTreeMap::new();
    headers.insert(
        "x-client-request-id".to_string(),
        "kept-by-rule-request".to_string(),
    );
    headers.insert("session_id".to_string(), "kept-by-rule".to_string());
    let body = json!({
        "model": "gpt-5",
        "prompt_cache_key": "172c39e6-c0a0-5a70-8b63-e0f8e0d185a3",
    });
    let mut original_headers = HeaderMap::new();
    original_headers.insert(
        "x-client-request-id",
        HeaderValue::from_static("user-specified-request"),
    );
    original_headers.insert(
        "session_id",
        HeaderValue::from_static("user-specified-session"),
    );
    original_headers.insert(
        "conversation_id",
        HeaderValue::from_static("user-specified-conversation"),
    );
    original_headers.insert(
        "user-agent",
        HeaderValue::from_static("user-specified-agent"),
    );
    original_headers.insert(
        "originator",
        HeaderValue::from_static("user-specified-originator"),
    );

    apply_codex_openai_responses_special_headers(
        &mut headers,
        &body,
        &original_headers,
        "codex",
        "openai:responses",
        Some("trace-codex-123"),
        Some(r#"{"account_id":"acc-123"}"#),
    );

    assert_eq!(
        headers.get("x-client-request-id"),
        Some(&"kept-by-rule-request".to_string())
    );
    assert!(!headers.contains_key("user-agent"));
    assert!(!headers.contains_key("originator"));
    assert_eq!(headers.get("session_id"), Some(&"kept-by-rule".to_string()));
    assert!(!headers.contains_key("conversation_id"));
}

#[test]
fn skips_conversation_id_for_compact_codex_requests() {
    let mut headers = BTreeMap::new();
    let body = json!({
        "model": "gpt-5",
        "prompt_cache_key": "172c39e6-c0a0-5a70-8b63-e0f8e0d185a3",
    });

    apply_codex_openai_responses_special_headers(
        &mut headers,
        &body,
        &HeaderMap::new(),
        "codex",
        "openai:responses:compact",
        Some("trace-codex-compact-123"),
        Some(r#"{"account_id":"acc-123"}"#),
    );

    assert_eq!(
        headers.get("chatgpt-account-id"),
        Some(&"acc-123".to_string())
    );
    assert_eq!(
        headers.get("x-client-request-id"),
        Some(&"trace-codex-compact-123".to_string())
    );
    assert_eq!(
        headers.get("user-agent"),
        Some(
            &"codex-tui/0.122.0 (Mac OS 15.2.0; arm64) vscode/2.6.11 (codex-tui; 0.122.0)"
                .to_string()
        )
    );
    assert_eq!(headers.get("originator"), Some(&"codex-tui".to_string()));
    assert_eq!(
        headers.get("session_id"),
        Some(&"ab5ecce4f0d110fe".to_string())
    );
    assert!(!headers.contains_key("conversation_id"));
}

#[test]
fn codex_pool_stable_client_headers_override_client_identity_headers() {
    let transport = sample_transport("codex", Some(json!({"pool_advanced": {}})));
    let mut headers = BTreeMap::from([
        ("user-agent".to_string(), "Go-http-client/2.0".to_string()),
        ("originator".to_string(), "Codex Desktop".to_string()),
        ("x-client-request-id".to_string(), "trace-123".to_string()),
    ]);

    apply_codex_pool_stable_client_headers(&mut headers, &transport);
    let first_user_agent = headers.get("user-agent").cloned();
    let first_originator = headers.get("originator").cloned();

    headers.insert(
        "user-agent".to_string(),
        "claude-cli/2.1.19 (external, sdk-cli)".to_string(),
    );
    headers.insert("originator".to_string(), "codex_vscode".to_string());
    apply_codex_pool_stable_client_headers(&mut headers, &transport);

    assert_eq!(headers.get("user-agent").cloned(), first_user_agent);
    assert_eq!(headers.get("originator").cloned(), first_originator);
    assert_eq!(
        headers.get("x-client-request-id"),
        Some(&"trace-123".to_string())
    );
}

#[test]
fn codex_pool_stable_client_headers_hash_by_account_name() {
    let mut first_transport = sample_transport("codex", Some(json!({"pool_advanced": {}})));
    let mut second_transport = sample_transport("codex", Some(json!({"pool_advanced": {}})));
    first_transport.key.id = "key-codex-a".to_string();
    second_transport.key.id = "key-codex-b".to_string();
    first_transport.key.name = "shared-account".to_string();
    second_transport.key.name = "shared-account".to_string();
    let mut first_headers = BTreeMap::new();
    let mut second_headers = BTreeMap::new();

    apply_codex_pool_stable_client_headers(&mut first_headers, &first_transport);
    apply_codex_pool_stable_client_headers(&mut second_headers, &second_transport);

    assert_eq!(
        first_headers.get("user-agent"),
        second_headers.get("user-agent")
    );
    assert_eq!(
        first_headers.get("originator"),
        second_headers.get("originator")
    );
}

#[test]
fn codex_pool_stable_client_headers_keep_existing_choice_when_profiles_are_appended() {
    let base_profile_values = vec![
        codex_header_profile_value("codex-tui/0.142.0 stable-a", "codex-tui"),
        codex_header_profile_value("Codex Desktop/0.142.0 stable-b", "Codex Desktop"),
    ];
    let base_profiles = vec![
        super::CodexClientHeaderProfile {
            user_agent: "codex-tui/0.142.0 stable-a".to_string(),
            originator: "codex-tui".to_string(),
        },
        super::CodexClientHeaderProfile {
            user_agent: "Codex Desktop/0.142.0 stable-b".to_string(),
            originator: "Codex Desktop".to_string(),
        },
    ];
    let selected = &base_profiles[super::stable_index_for_key("account", &base_profiles)];
    let selected_score = super::stable_profile_score("account", selected);
    let appended_profile = (0..256)
        .map(|index| super::CodexClientHeaderProfile {
            user_agent: format!("codex_cli_rs/0.133.0 appended-{index}"),
            originator: "codex_cli_rs".to_string(),
        })
        .find(|profile| super::stable_profile_score("account", profile) < selected_score)
        .expect("find appended profile with lower stable score");
    let mut appended_profile_values = base_profile_values.clone();
    appended_profile_values.push(codex_header_profile_value(
        appended_profile.user_agent.as_str(),
        appended_profile.originator.as_str(),
    ));

    let transport = sample_transport(
        "codex",
        Some(json!({
            "pool_advanced": {
                "codex_client_headers": {
                    "profiles": base_profile_values
                }
            }
        })),
    );
    let appended_transport = sample_transport(
        "codex",
        Some(json!({
            "pool_advanced": {
                "codex_client_headers": {
                    "profiles": appended_profile_values
                }
            }
        })),
    );
    let mut base_headers = BTreeMap::new();
    let mut appended_headers = BTreeMap::new();

    apply_codex_pool_stable_client_headers(&mut base_headers, &transport);
    apply_codex_pool_stable_client_headers(&mut appended_headers, &appended_transport);

    assert_eq!(
        base_headers.get("user-agent"),
        appended_headers.get("user-agent")
    );
    assert_eq!(
        base_headers.get("originator"),
        appended_headers.get("originator")
    );
}

#[test]
fn codex_pool_stable_client_headers_remove_third_party_upstream_leak_headers() {
    let transport = sample_transport("codex", Some(json!({"pool_advanced": {}})));
    let mut headers = BTreeMap::from([
        ("Anthropic-Version".to_string(), "2023-06-01".to_string()),
        (
            "x-amz-user-agent".to_string(),
            "aws-sdk-js/1.0.27 KiroIDE-0.6.18".to_string(),
        ),
        (
            "x-amzn-codewhisperer-optout".to_string(),
            "true".to_string(),
        ),
        ("x-amzn-kiro-agent-mode".to_string(), "vibe".to_string()),
        ("x-client-request-id".to_string(), "trace-123".to_string()),
    ]);

    apply_codex_pool_stable_client_headers(&mut headers, &transport);

    assert!(!headers.contains_key("anthropic-version"));
    assert!(!headers.contains_key("Anthropic-Version"));
    assert!(!headers.contains_key("x-amz-user-agent"));
    assert!(!headers.contains_key("x-amzn-codewhisperer-optout"));
    assert!(!headers.contains_key("x-amzn-kiro-agent-mode"));
    assert_eq!(
        headers.get("x-client-request-id"),
        Some(&"trace-123".to_string())
    );
}

#[test]
fn codex_pool_stable_client_headers_use_custom_profiles() {
    let transport = sample_transport(
        "codex",
        Some(json!({
            "pool_advanced": {
                "codex_client_headers": {
                    "profiles": [
                        {"user_agent": "ua-a", "originator": "origin-a"}
                    ]
                }
            }
        })),
    );
    let mut headers = BTreeMap::new();

    apply_codex_pool_stable_client_headers(&mut headers, &transport);

    assert_eq!(headers.get("user-agent"), Some(&"ua-a".to_string()));
    assert_eq!(headers.get("originator"), Some(&"origin-a".to_string()));
}

#[test]
fn codex_pool_stable_client_headers_can_be_disabled() {
    let transport = sample_transport(
        "codex",
        Some(json!({
            "pool_advanced": {
                "codex_client_headers": {
                    "enabled": false,
                    "profiles": [
                        {"user_agent": "ua-a", "originator": "origin-a"}
                    ]
                }
            }
        })),
    );
    let mut headers = BTreeMap::from([
        ("user-agent".to_string(), "Go-http-client/2.0".to_string()),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
    ]);

    apply_codex_pool_stable_client_headers(&mut headers, &transport);

    assert_eq!(
        headers.get("user-agent"),
        Some(&"Go-http-client/2.0".to_string())
    );
    assert!(!headers.contains_key("originator"));
    assert!(!headers.contains_key("anthropic-version"));
}

#[test]
fn codex_pool_stable_client_headers_ignore_non_codex_or_non_pool() {
    let mut non_codex = sample_transport("openai", Some(json!({"pool_advanced": {}})));
    let mut headers = BTreeMap::from([
        ("user-agent".to_string(), "client".to_string()),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
    ]);
    apply_codex_pool_stable_client_headers(&mut headers, &non_codex);
    assert_eq!(headers.get("user-agent"), Some(&"client".to_string()));
    assert_eq!(
        headers.get("anthropic-version"),
        Some(&"2023-06-01".to_string())
    );

    non_codex.provider.provider_type = "codex".to_string();
    non_codex.provider.config = Some(json!({}));
    apply_codex_pool_stable_client_headers(&mut headers, &non_codex);
    assert_eq!(headers.get("user-agent"), Some(&"client".to_string()));
    assert_eq!(
        headers.get("anthropic-version"),
        Some(&"2023-06-01".to_string())
    );
}
