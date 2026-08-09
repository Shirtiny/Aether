use aether_provider_transport::snapshot::{
    GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
    GatewayProviderTransportProvider, GatewayProviderTransportSnapshot,
};
use http::Request;
use serde_json::{json, Value};

use super::{
    build_cross_format_openai_responses_request_body, build_local_openai_responses_request_body,
    build_local_openai_responses_upstream_url, build_owned_local_openai_responses_request_body,
};

fn object_keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("json object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn sample_transport(base_url: &str, api_format: &str) -> GatewayProviderTransportSnapshot {
    GatewayProviderTransportSnapshot {
        provider: GatewayProviderTransportProvider {
            id: "provider-codex".to_string(),
            name: "codex".to_string(),
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
            id: "endpoint-codex".to_string(),
            provider_id: "provider-codex".to_string(),
            api_format: api_format.to_string(),
            api_family: Some("openai".to_string()),
            endpoint_kind: Some("cli".to_string()),
            is_active: true,
            base_url: base_url.to_string(),
            header_rules: None,
            body_rules: None,
            max_retries: None,
            custom_path: None,
            config: None,
            format_acceptance_config: None,
            proxy: None,
        },
        key: GatewayProviderTransportKey {
            id: "key-codex".to_string(),
            provider_id: "provider-codex".to_string(),
            name: "oauth".to_string(),
            auth_type: "oauth".to_string(),
            is_active: true,
            api_formats: Some(vec![api_format.to_string()]),
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
            decrypted_api_key: "__placeholder__".to_string(),
            decrypted_auth_config: None,
        },
    }
}

#[test]
fn builds_openai_chat_cross_format_request_body_from_openai_responses_source() {
    let body_json = json!({
        "model": "gpt-5",
        "input": "hello",
    });

    let provider_request_body = build_cross_format_openai_responses_request_body(
        &body_json,
        "gpt-5-upstream",
        "openai:responses",
        "openai:chat",
        false,
        false,
        "openai",
        None,
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("openai responses to openai chat body should build");

    assert_eq!(provider_request_body["model"], "gpt-5-upstream");
    assert_eq!(provider_request_body["messages"][0]["role"], "user");
    assert_eq!(provider_request_body["messages"][0]["content"], "hello");
}

#[test]
fn grok_chat_cross_format_request_fills_required_tool_parameters() {
    let body_json = json!({
        "model": "grok-4.5",
        "input": "hello",
        "tools": [{"type": "web_search"}],
    });
    let body_rules = json!([{
        "action": "drop",
        "path": "tools[*].function.parameters"
    }]);

    let provider_request_body = build_cross_format_openai_responses_request_body(
        &body_json,
        "grok-4.5",
        "openai:responses",
        "openai:chat",
        true,
        false,
        "grok",
        Some(&body_rules),
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("grok chat request should build");

    assert_eq!(
        provider_request_body["tools"][0]["function"]["parameters"],
        json!({"type": "object", "properties": {}})
    );
}

#[test]
fn grok_responses_request_sanitizes_codex_reasoning_history() {
    let body_json = json!({
        "model": "grok-4.5",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}],
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            },
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "thinking"}],
                "content": null,
                "encrypted_content": null,
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            }
        ],
        "client_metadata": {"client_version": "0.144.5"}
    });

    let provider_request_body = build_local_openai_responses_request_body(
        &body_json,
        "grok-4.5",
        true,
        false,
        "grok",
        "openai:responses",
        None,
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("grok responses request should build");

    assert!(provider_request_body.get("client_metadata").is_none());
    assert!(provider_request_body["input"][0]
        .get("internal_chat_message_metadata_passthrough")
        .is_none());
    assert!(provider_request_body["input"][1]
        .get("internal_chat_message_metadata_passthrough")
        .is_none());
    assert!(provider_request_body["input"][1].get("content").is_none());
    assert!(provider_request_body["input"][1]
        .get("encrypted_content")
        .is_none());
    assert_eq!(
        provider_request_body["input"][1]["summary"][0]["text"],
        "thinking"
    );
}

#[test]
fn local_openai_responses_wrapper_preserves_body_order_after_edits() {
    let body_json: Value = serde_json::from_str(
        r#"{
            "text": {"format": {"type": "text"}},
            "input": [],
            "model": "gpt-5.4",
            "store": false,
            "tools": [],
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "reasoning": {"effort": "high"},
            "tool_choice": "auto"
        }"#,
    )
    .expect("request body should parse");

    let provider_request_body = build_local_openai_responses_request_body(
        &body_json,
        "gpt-5.4",
        true,
        false,
        "codex",
        "openai:responses",
        None,
        Some("key-123"),
        &http::HeaderMap::new(),
        false,
    )
    .expect("local openai responses body should build");

    assert_eq!(
        object_keys(&provider_request_body),
        vec![
            "text",
            "input",
            "model",
            "store",
            "tools",
            "stream",
            "include",
            "reasoning",
            "tool_choice",
            "parallel_tool_calls",
            "instructions",
        ]
    );
    assert_eq!(provider_request_body["parallel_tool_calls"], json!(true));
    assert_eq!(provider_request_body["instructions"], json!(""));
    assert!(provider_request_body.get("prompt_cache_key").is_none());
}

#[test]
fn local_openai_responses_compact_wrapper_strips_store_for_same_format_requests() {
    let body_json = json!({
        "model": "gpt-5.4",
        "input": [],
        "store": true
    });

    let provider_request_body = build_local_openai_responses_request_body(
        &body_json,
        "gpt-5.4",
        false,
        false,
        "openai",
        "openai:responses:compact",
        None,
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("local openai compact body should build");

    assert!(provider_request_body.get("store").is_none());
    assert!(provider_request_body.get("stream").is_none());
}

#[test]
fn local_openai_responses_compact_wrapper_strips_include_for_codex_requests() {
    let body_json = json!({
        "model": "gpt-5.4",
        "input": [],
        "include": ["reasoning.encrypted_content"],
        "store": true,
        "stream": true
    });

    let provider_request_body = build_local_openai_responses_request_body(
        &body_json,
        "gpt-5.4",
        false,
        false,
        "codex",
        "openai:responses:compact",
        None,
        Some("key-123"),
        &http::HeaderMap::new(),
        false,
    )
    .expect("local codex compact body should build");

    assert!(provider_request_body.get("include").is_none());
    assert!(provider_request_body.get("store").is_none());
    assert!(provider_request_body.get("stream").is_none());
    assert_eq!(provider_request_body["instructions"], "");
    assert!(provider_request_body.get("prompt_cache_key").is_none());
}

#[test]
fn local_openai_responses_wrapper_applies_model_directive_before_body_rules() {
    let body_json = json!({
        "model": "gpt-5.4-max",
        "input": "hello",
        "reasoning": {"effort": "low", "summary": "auto"}
    });
    let body_rules = json!([
        {"action":"set","path":"metadata.override_seen","value":true}
    ]);

    let provider_request_body = build_local_openai_responses_request_body(
        &body_json,
        "gpt-5.4",
        false,
        false,
        "openai",
        "openai:responses",
        Some(&body_rules),
        None,
        &http::HeaderMap::new(),
        true,
    )
    .expect("local openai responses body should build");

    assert_eq!(provider_request_body["reasoning"]["effort"], "xhigh");
    assert_eq!(provider_request_body["reasoning"]["summary"], "auto");
    assert_eq!(provider_request_body["metadata"]["override_seen"], true);
}

#[test]
fn owned_codex_ws_materialization_matches_borrowed_rules_without_cloning_input() {
    let large_input = "x".repeat(1024 * 1024);
    let body_json = json!({
        "model": "gpt-5.4-max-fast",
        "input": large_input,
        "reasoning": {"effort": "low", "summary": "auto"},
        "stream": false
    });
    let input_pointer = body_json["input"].as_str().expect("input string").as_ptr();
    let body_rules = json!([
        {"action":"set","path":"metadata.header_seen","value":true,"condition":{"source":"request_headers","path":"x-mode","op":"eq","value":"fast"}},
        {"action":"set","path":"metadata.model_seen","value":true,"condition":{"source":"current","path":"model","op":"eq","value":"gpt-5.4"}}
    ]);
    let mut headers = http::HeaderMap::new();
    headers.insert("x-mode", http::HeaderValue::from_static("fast"));

    let expected = build_local_openai_responses_request_body(
        &body_json,
        "gpt-5.4",
        true,
        false,
        "codex",
        "openai:responses",
        Some(&body_rules),
        Some("key-123"),
        &headers,
        true,
    )
    .expect("borrowed materialization");
    let actual = build_owned_local_openai_responses_request_body(
        body_json,
        "gpt-5.4",
        true,
        false,
        "codex",
        "openai:responses",
        Some(&body_rules),
        Some("key-123"),
        &headers,
        true,
    )
    .expect("owned materialization");

    assert_eq!(actual, expected);
    assert_eq!(
        actual["input"].as_str().expect("owned input").as_ptr(),
        input_pointer,
        "the large client payload allocation must move into the provider body"
    );
}

#[test]
fn owned_codex_ws_materialization_rejects_original_body_rules() {
    let body_rules = json!([{
        "action": "set",
        "path": "metadata.from_original",
        "value": true,
        "condition": {"source": "original", "path": "model", "op": "exists"}
    }]);
    assert!(build_owned_local_openai_responses_request_body(
        json!({"model":"gpt-5.4","input":"hello"}),
        "gpt-5.4",
        true,
        false,
        "codex",
        "openai:responses",
        Some(&body_rules),
        Some("key-123"),
        &http::HeaderMap::new(),
        false,
    )
    .is_none());
}

#[test]
fn local_openai_responses_upstream_url_preserves_codex_base_path() {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .body(())
        .expect("request should build");
    let (parts, _) = request.into_parts();

    let upstream_url = build_local_openai_responses_upstream_url(
        &parts,
        &sample_transport("https://tiger.bookapi.cc/codex", "openai:responses"),
        false,
    )
    .expect("openai responses upstream url should build");

    assert_eq!(upstream_url, "https://tiger.bookapi.cc/codex/responses");
}

#[test]
fn strips_metadata_for_codex_openai_responses_requests() {
    let body_json = json!({
        "model": "claude-sonnet-4-5",
        "metadata": {"trace_id": "abc"},
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        }],
    });

    let provider_request_body = build_cross_format_openai_responses_request_body(
        &body_json,
        "gpt-5-upstream",
        "claude:messages",
        "openai:responses",
        true,
        false,
        "codex",
        None,
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("claude cli to codex request should build");

    assert!(provider_request_body.get("metadata").is_none());
}

#[test]
fn openai_chat_to_codex_responses_preserves_json_mode_chat_messages() {
    let body_json = json!({
        "model": "gpt-5.5",
        "messages": [
            {"role": "system", "content": "Return a JSON object."},
            {"role": "user", "content": "Why did this JSON request fail?"}
        ],
        "response_format": {"type": "json_object"}
    });

    let provider_request_body = build_cross_format_openai_responses_request_body(
        &body_json,
        "gpt-5.5-upstream",
        "openai:chat",
        "openai:responses",
        false,
        false,
        "codex",
        None,
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("openai chat to codex responses request should build");

    assert_eq!(
        provider_request_body["text"]["format"]["type"],
        "json_object"
    );
    assert_eq!(provider_request_body["input"][0]["role"], "user");
    assert_eq!(
        provider_request_body["input"][0]["content"][0]["text"],
        "Why did this JSON request fail?"
    );
    assert_eq!(
        provider_request_body["instructions"],
        "Return a JSON object."
    );
}

#[test]
fn applies_codex_defaults_unless_body_rules_handle_the_field() {
    let body_json = json!({
        "model": "claude-sonnet-4-5",
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        }],
        "metadata": {"trace_id": "abc"},
        "store": true
    });
    let body_rules = json!([
        {"action":"set","path":"store","value":true},
        {"action":"set","path":"instructions","value":"Custom instructions"},
        {"action":"set","path":"metadata","value":{"trace_id":"keep-me"}}
    ]);

    let provider_request_body = build_cross_format_openai_responses_request_body(
        &body_json,
        "gpt-5-upstream",
        "claude:messages",
        "openai:responses",
        true,
        false,
        "codex",
        Some(&body_rules),
        None,
        &http::HeaderMap::new(),
        false,
    )
    .expect("claude cli to codex request should build");

    assert_eq!(provider_request_body["store"], true);
    assert_eq!(provider_request_body["instructions"], "Custom instructions");
    assert_eq!(provider_request_body["metadata"]["trace_id"], "keep-me");
}

#[test]
fn responses_normalization_does_not_use_api_key_as_prompt_cache_identity() {
    let body_json = json!({
        "model": "claude-sonnet-4-5",
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        }],
    });

    let provider_request_body = build_cross_format_openai_responses_request_body(
        &body_json,
        "gpt-5-upstream",
        "claude:messages",
        "openai:responses",
        true,
        false,
        "codex",
        None,
        Some("key-123"),
        &http::HeaderMap::new(),
        false,
    )
    .expect("claude cli to codex request should build");

    assert!(provider_request_body.get("prompt_cache_key").is_none());
}

#[test]
fn chat_normalization_does_not_use_api_key_as_prompt_cache_identity() {
    let body_json = json!({
        "model": "gpt-5",
        "messages": [{
            "role": "user",
            "content": "hello"
        }],
    });

    let provider_request_body = super::build_cross_format_openai_chat_request_body(
        &body_json,
        "gpt-5-upstream",
        "codex",
        "openai:responses",
        false,
        false,
        None,
        Some("key-123"),
        &http::HeaderMap::new(),
        false,
    )
    .expect("openai chat to codex request should build");

    assert!(provider_request_body.get("prompt_cache_key").is_none());
}
