use std::collections::BTreeMap;

use serde_json::Value;

use crate::snapshot::GatewayProviderTransportSnapshot;

/// Official xAI API root, used for media traffic and for accounts that opt into
/// API mode.
pub const GROK_API_BASE_URL: &str = "https://api.x.ai/v1";
/// Grok CLI chat-proxy root. A subscription OAuth grant serves non-media chat
/// from here rather than from the official API root.
pub const GROK_CLI_CHAT_PROXY_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const GROK_DEFAULT_BASE_URL: &str = GROK_CLI_CHAT_PROXY_BASE_URL;

/// Identity headers the Grok CLI chat-proxy expects. It rejects a request that
/// carries only a bearer token.
const GROK_CLI_CLIENT_VERSION: &str = "0.2.103";
const GROK_CLI_TOKEN_AUTH_HEADER: &str = "x-xai-token-auth";
const GROK_CLI_TOKEN_AUTH_VALUE: &str = "xai-grok-cli";
const GROK_CLI_CLIENT_VERSION_HEADER: &str = "x-grok-client-version";

pub fn is_grok_provider_transport(transport: &GatewayProviderTransportSnapshot) -> bool {
    transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("grok")
}

fn normalize_grok_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

fn is_grok_api_base_url(base_url: &str) -> bool {
    normalize_grok_base_url(base_url) == normalize_grok_base_url(GROK_API_BASE_URL)
}

pub fn is_grok_cli_chat_proxy_base_url(base_url: &str) -> bool {
    normalize_grok_base_url(base_url) == normalize_grok_base_url(GROK_CLI_CHAT_PROXY_BASE_URL)
}

/// Report whether a key reaches xAI as a direct API consumer rather than as a
/// pooled Grok CLI subscription. OAuth keys default to subscription mode; an
/// explicit `using_api` in the key's auth config wins over that default.
pub fn grok_using_api(transport: &GatewayProviderTransportSnapshot) -> bool {
    let declared = transport
        .key
        .decrypted_auth_config
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|config| match config.get("using_api") {
            Some(Value::Bool(value)) => Some(*value),
            Some(Value::String(value)) => value.trim().parse::<bool>().ok(),
            _ => None,
        });
    if let Some(using_api) = declared {
        return using_api;
    }
    !transport.key.auth_type.trim().eq_ignore_ascii_case("oauth")
}

/// Resolve the base URL for non-media Grok chat. API-mode keys keep the
/// official API root. Subscription keys are served by the CLI chat-proxy, so an
/// empty or official-default base URL is rewritten to it while a deliberate
/// custom base URL is left alone.
pub fn grok_chat_base_url(base_url: &str, using_api: bool) -> String {
    let base_url = normalize_grok_base_url(base_url);
    if using_api {
        if base_url.is_empty() {
            return GROK_API_BASE_URL.to_string();
        }
        return base_url;
    }
    if !base_url.is_empty() && !is_grok_api_base_url(&base_url) {
        return base_url;
    }
    GROK_CLI_CHAT_PROXY_BASE_URL.to_string()
}

pub fn grok_base_url(base_url: &str) -> String {
    let base_url = normalize_grok_base_url(base_url);
    if base_url.is_empty() {
        GROK_DEFAULT_BASE_URL.to_string()
    } else {
        base_url
    }
}

/// Attach the Grok CLI identity headers when a subscription key is routed to
/// the chat-proxy. API-mode keys and custom base URLs are left untouched.
///
/// This keys off the endpoint's configured base URL rather than off
/// [`grok_chat_base_url`], because request planning sends to that configured
/// URL verbatim. Deciding from a rewritten URL could attach chat-proxy identity
/// to a request that is actually leaving for the official API root.
pub fn apply_grok_chat_identity_headers(
    headers: &mut BTreeMap<String, String>,
    transport: &GatewayProviderTransportSnapshot,
) {
    if !is_grok_provider_transport(transport) || grok_using_api(transport) {
        return;
    }
    if !is_grok_cli_chat_proxy_base_url(&transport.endpoint.base_url) {
        return;
    }
    headers.insert(
        GROK_CLI_TOKEN_AUTH_HEADER.to_string(),
        GROK_CLI_TOKEN_AUTH_VALUE.to_string(),
    );
    headers.insert(
        GROK_CLI_CLIENT_VERSION_HEADER.to_string(),
        GROK_CLI_CLIENT_VERSION.to_string(),
    );
    headers.insert(
        "user-agent".to_string(),
        format!("xai-grok-workspace/{GROK_CLI_CLIENT_VERSION}"),
    );
}

pub fn build_grok_upstream_url(transport: &GatewayProviderTransportSnapshot, path: &str) -> String {
    let base_url = grok_chat_base_url(&transport.endpoint.base_url, grok_using_api(transport));
    let path = path.trim();
    if path.starts_with('/') {
        format!("{base_url}{path}")
    } else {
        format!("{base_url}/{path}")
    }
}

/// Resolve the official xAI OAuth access token as an Authorization bearer
/// header. Runtime request planning normally goes through the shared OAuth
/// coordinator so expiring tokens are refreshed with singleflight semantics.
pub fn resolve_grok_bearer_auth(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<(String, String)> {
    let raw_secret = transport.key.decrypted_api_key.trim();
    let access_token = raw_secret
        .strip_prefix("Bearer ")
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| (!raw_secret.is_empty()).then(|| raw_secret.to_string()))
        .or_else(|| {
            transport
                .key
                .decrypted_auth_config
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|config| {
                    config
                        .get("access_token")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                })
        })?;
    let access_token = access_token.trim();
    if access_token.is_empty() || access_token == "__placeholder__" {
        return None;
    }
    Some((
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    ))
}

pub fn resolve_grok_model_alias(provider_type: &str, model: &str) -> String {
    if !provider_type.trim().eq_ignore_ascii_case("grok") {
        return model.to_string();
    }
    match model.trim().to_ascii_lowercase().as_str() {
        "grok" | "grok-4.5-latest" => "grok-4.5".to_string(),
        "grok-latest" | "grok-4.3-latest" => "grok-4.3".to_string(),
        "grok-build" | "grok-code-fast" | "grok-code-fast-1" | "grok-code-fast-1-0825" => {
            "grok-build-0.1".to_string()
        }
        "grok-build-latest" => "grok-4.5".to_string(),
        _ => model.to_string(),
    }
}

/// Remove rejected fields and adapt Codex namespace/custom tools to the
/// function-only Responses shape accepted by xAI. The reversible identities
/// are collected separately into the execution report context so response
/// finalization can restore Codex routing fields.
pub fn apply_grok_xai_responses_body_edits(
    body: &mut Value,
    provider_type: &str,
    provider_api_format: &str,
) {
    if !provider_type.trim().eq_ignore_ascii_case("grok")
        || !matches!(
            provider_api_format.trim().to_ascii_lowercase().as_str(),
            "openai:responses" | "openai:responses:compact"
        )
    {
        return;
    }

    let is_grok_45 = body
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| model.trim().eq_ignore_ascii_case("grok-4.5"));
    if let Some(object) = body.as_object_mut() {
        for field in [
            "prompt_cache_retention",
            "safety_identifier",
            "stream_options",
        ] {
            object.remove(field);
        }
        if is_grok_45 {
            for field in [
                "presence_penalty",
                "presencePenalty",
                "frequency_penalty",
                "frequencyPenalty",
                "stop",
            ] {
                object.remove(field);
            }
        }
    }
    aether_ai_formats::provider_compat::grok_responses::normalize_grok_responses_request_tools(
        body,
    );
    sanitize_grok_xai_responses_tool_fields(body);
    remove_grok_xai_field_outside_schemas(body, "external_web_access");
}

/// Remove only tool-declaration fields that xAI documents as request-rejected.
fn sanitize_grok_xai_responses_tool_fields(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools.iter_mut().filter_map(Value::as_object_mut) {
        match tool
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
        {
            "web_search" => {
                for field in [
                    "external_web_access",
                    "search_context_size",
                    "user_location",
                ] {
                    tool.remove(field);
                }
            }
            "file_search" => {
                for field in ["filters", "ranking_options"] {
                    tool.remove(field);
                }
            }
            "code_interpreter" => {
                tool.remove("container");
            }
            _ => {}
        }
    }
}

/// Preserve the existing compatibility cleanup for misplaced
/// `external_web_access` fields while leaving JSON Schema property names intact.
fn remove_grok_xai_field_outside_schemas(value: &mut Value, field: &str) {
    match value {
        Value::Object(object) => {
            object.remove(field);
            for (key, child) in object.iter_mut() {
                if matches!(key.as_str(), "parameters" | "schema") {
                    continue;
                }
                remove_grok_xai_field_outside_schemas(child, field);
            }
        }
        Value::Array(items) => {
            for item in items {
                remove_grok_xai_field_outside_schemas(item, field);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_grok_chat_identity_headers, apply_grok_xai_responses_body_edits,
        build_grok_upstream_url, grok_base_url, grok_chat_base_url, grok_using_api,
        resolve_grok_bearer_auth, resolve_grok_model_alias,
    };
    use crate::snapshot::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider, GatewayProviderTransportSnapshot,
    };
    use std::collections::BTreeMap;

    fn sample_transport(access_token: &str) -> GatewayProviderTransportSnapshot {
        GatewayProviderTransportSnapshot {
            provider: GatewayProviderTransportProvider {
                id: "provider-1".to_string(),
                name: "Grok".to_string(),
                provider_type: "grok".to_string(),
                website: None,
                is_active: true,
                keep_priority_on_conversion: false,
                enable_format_conversion: true,
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
                api_family: None,
                endpoint_kind: None,
                is_active: true,
                base_url: "https://api.x.ai/v1/".to_string(),
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
                name: "key".to_string(),
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
                decrypted_api_key: access_token.to_string(),
                decrypted_auth_config: Some(
                    serde_json::json!({"refresh_token": "refresh-token"}).to_string(),
                ),
            },
        }
    }

    #[test]
    fn resolves_official_xai_bearer_auth() {
        let transport = sample_transport("access-token");
        assert_eq!(
            resolve_grok_bearer_auth(&transport),
            Some((
                "authorization".to_string(),
                "Bearer access-token".to_string()
            ))
        );
    }

    #[test]
    fn defaults_to_the_cli_chat_proxy_root() {
        assert_eq!(grok_base_url(""), "https://cli-chat-proxy.grok.com/v1");
        let transport = sample_transport("access-token");
        assert_eq!(
            build_grok_upstream_url(&transport, "/responses"),
            "https://cli-chat-proxy.grok.com/v1/responses"
        );
    }

    #[test]
    fn oauth_keys_default_to_subscription_mode() {
        let mut transport = sample_transport("access-token");
        assert!(!grok_using_api(&transport));

        transport.key.auth_type = "api_key".to_string();
        assert!(grok_using_api(&transport));
    }

    #[test]
    fn an_explicit_using_api_flag_overrides_the_auth_type_default() {
        let mut transport = sample_transport("access-token");
        transport.key.decrypted_auth_config =
            Some(serde_json::json!({"using_api": true}).to_string());
        assert!(grok_using_api(&transport));

        transport.key.decrypted_auth_config =
            Some(serde_json::json!({"using_api": "false"}).to_string());
        transport.key.auth_type = "api_key".to_string();
        assert!(!grok_using_api(&transport));
    }

    #[test]
    fn subscription_chat_rewrites_the_official_api_root_to_the_chat_proxy() {
        for base_url in ["", "https://api.x.ai/v1", "https://api.x.ai/v1/"] {
            assert_eq!(
                grok_chat_base_url(base_url, false),
                "https://cli-chat-proxy.grok.com/v1",
                "subscription chat must not reach the official API root ({base_url:?})"
            );
        }
    }

    #[test]
    fn a_deliberate_custom_base_url_survives_subscription_rewriting() {
        assert_eq!(
            grok_chat_base_url("https://grok.example.com/v1", false),
            "https://grok.example.com/v1"
        );
    }

    #[test]
    fn api_mode_keeps_the_official_api_root() {
        assert_eq!(grok_chat_base_url("", true), "https://api.x.ai/v1");
        assert_eq!(
            grok_chat_base_url("https://api.x.ai/v1", true),
            "https://api.x.ai/v1"
        );
    }

    #[test]
    fn chat_proxy_requests_carry_the_grok_cli_identity() {
        let mut transport = sample_transport("access-token");
        transport.endpoint.base_url = "https://cli-chat-proxy.grok.com/v1".to_string();
        let mut headers = BTreeMap::new();
        apply_grok_chat_identity_headers(&mut headers, &transport);

        assert_eq!(
            headers.get("x-xai-token-auth").map(String::as_str),
            Some("xai-grok-cli")
        );
        assert_eq!(
            headers.get("x-grok-client-version").map(String::as_str),
            Some("0.2.103")
        );
        assert_eq!(
            headers.get("user-agent").map(String::as_str),
            Some("xai-grok-workspace/0.2.103")
        );
    }

    #[test]
    fn api_mode_and_custom_hosts_skip_the_grok_cli_identity() {
        let mut api_mode = sample_transport("access-token");
        api_mode.endpoint.base_url = "https://cli-chat-proxy.grok.com/v1".to_string();
        api_mode.key.decrypted_auth_config =
            Some(serde_json::json!({"using_api": true}).to_string());
        let mut headers = BTreeMap::new();
        apply_grok_chat_identity_headers(&mut headers, &api_mode);
        assert!(headers.is_empty());

        let mut custom_host = sample_transport("access-token");
        custom_host.endpoint.base_url = "https://grok.example.com/v1".to_string();
        let mut headers = BTreeMap::new();
        apply_grok_chat_identity_headers(&mut headers, &custom_host);
        assert!(headers.is_empty());
    }

    #[test]
    fn the_official_api_root_never_receives_chat_proxy_identity() {
        // Request planning sends to the configured base URL as-is, so an
        // endpoint still pointed at the API root must not be told it is talking
        // to the CLI chat-proxy.
        let mut transport = sample_transport("access-token");
        transport.endpoint.base_url = "https://api.x.ai/v1".to_string();
        let mut headers = BTreeMap::new();
        apply_grok_chat_identity_headers(&mut headers, &transport);
        assert!(headers.is_empty());
    }

    #[test]
    fn a_non_grok_provider_never_receives_the_grok_cli_identity() {
        let mut transport = sample_transport("access-token");
        transport.provider.provider_type = "openai".to_string();
        let mut headers = BTreeMap::new();
        apply_grok_chat_identity_headers(&mut headers, &transport);
        assert!(headers.is_empty());
    }

    #[test]
    fn resolves_official_xai_text_model_aliases() {
        assert_eq!(resolve_grok_model_alias("grok", "grok"), "grok-4.5");
        assert_eq!(resolve_grok_model_alias("grok", "grok-latest"), "grok-4.3");
        assert_eq!(
            resolve_grok_model_alias("grok", "grok-code-fast-1-0825"),
            "grok-build-0.1"
        );
        assert_eq!(
            resolve_grok_model_alias("custom", "grok-latest"),
            "grok-latest"
        );
    }

    #[test]
    fn sanitizes_fields_rejected_by_xai_responses() {
        let mut body = serde_json::json!({
            "model": "grok-4.5",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "hello",
                    "external_web_access": true
                }]
            }],
            "prompt_cache_retention": "24h",
            "safety_identifier": "user-1",
            "stream_options": {"reasoning_summary_delivery":"sequential_cutoff"},
            "presence_penalty": 0.2,
            "frequency_penalty": 0.3,
            "stop": ["done"]
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert!(body.get("prompt_cache_retention").is_none());
        assert!(body.get("safety_identifier").is_none());
        assert!(body.get("stream_options").is_none());
        assert!(body.get("presence_penalty").is_none());
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("stop").is_none());
        assert!(body["input"][0]["content"][0]
            .get("external_web_access")
            .is_none());
        assert_eq!(body["input"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn flattens_codex_namespace_and_custom_tools_for_xai_responses() {
        let mut body = serde_json::json!({
            "model": "grok-4.5",
            "input": "hello",
            "tools": [
                {"type":"function","name":"exec_command","parameters":{"type":"object"}},
                {
                    "type":"namespace",
                    "name":"multi_agent_v1",
                    "tools":[{"type":"function","name":"spawn_agent"}]
                },
                {"type":"custom","name":"unsupported_custom"},
                {"type":"tool_search"},
                {"type":"computer_use_preview"},
                {"type":"web_search"}
            ],
            "tool_choice": {"type":"namespace","name":"multi_agent_v1"}
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert_eq!(
            body["tools"],
            serde_json::json!([
                {"type":"function","name":"exec_command","parameters":{"type":"object"}},
                {"type":"function","name":"multi_agent_v1__spawn_agent","parameters":{"type":"object","properties":{}}},
                {"type":"function","name":"unsupported_custom","parameters":{"type":"object","properties":{"input":{}},"required":["input"],"additionalProperties":false}},
                {"type":"web_search"}
            ])
        );
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn simplifies_function_parameter_schemas_rejected_by_xai_responses() {
        let mut body = serde_json::json!({
            "model": "grok-4.5",
            "input": "hello",
            "tools": [{
                "type":"namespace",
                "name":"codex_app",
                "tools":[{
                    "type":"function",
                    "name":"automation_update",
                    "strict":true,
                    "parameters":{
                        "oneOf":[
                            {"$ref":"#/$defs/create"},
                            {"type":"null"}
                        ]
                    }
                }]
            }]
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert_eq!(body["tools"][0]["name"], "codex_app__automation_update");
        assert_eq!(
            body["tools"][0]["parameters"],
            serde_json::json!({
                "type":"object",
                "properties":{},
                "additionalProperties":true
            })
        );
        assert_eq!(body["tools"][0]["strict"], false);
    }

    #[test]
    fn removes_only_xai_rejected_tool_options_without_mutating_user_data() {
        let mut body = serde_json::json!({
            "model":"grok-4.5",
            "input":[{
                "role":"user",
                "content":[{
                    "type":"input_text",
                    "text":"{\"external_web_access\":true,\"filters\":{\"keep\":true}}"
                }]
            }, {
                "type":"function_call",
                "name":"lookup",
                "call_id":"call_1",
                "arguments":"{\"external_web_access\":true}"
            }],
            "tools":[
                {
                    "type":"web_search",
                    "external_web_access":true,
                    "search_context_size":"high",
                    "user_location":{"type":"approximate","country":"US"},
                    "allowed_domains":["example.com"]
                },
                {
                    "type":"file_search",
                    "vector_store_ids":["collection_1"],
                    "filters":{"type":"eq"},
                    "ranking_options":{"ranker":"auto"}
                },
                {
                    "type":"code_interpreter",
                    "container":{"type":"auto"}
                },
                {
                    "type":"function",
                    "name":"lookup",
                    "parameters":{
                        "type":"object",
                        "properties":{"external_web_access":{"type":"boolean"}}
                    }
                }
            ]
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert!(body["tools"][0].get("external_web_access").is_none());
        assert!(body["tools"][0].get("search_context_size").is_none());
        assert!(body["tools"][0].get("user_location").is_none());
        assert_eq!(body["tools"][0]["allowed_domains"][0], "example.com");
        assert!(body["tools"][1].get("filters").is_none());
        assert!(body["tools"][1].get("ranking_options").is_none());
        assert!(body["tools"][2].get("container").is_none());
        assert_eq!(
            body["tools"][3]["parameters"]["properties"]["external_web_access"]["type"],
            "boolean"
        );
        assert!(body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("external_web_access"));
        assert!(body["input"][1]["arguments"]
            .as_str()
            .unwrap()
            .contains("external_web_access"));
    }

    #[test]
    fn removes_tool_fields_when_xai_rejects_every_tool() {
        let mut body = serde_json::json!({
            "model": "grok-4.5",
            "input": "hello",
            "tools": [{"type":"namespace","name":"mcp__node_repl","tools":[]}],
            "tool_choice": "required"
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn keeps_model_specific_fields_for_non_grok_45_responses() {
        let mut body = serde_json::json!({
            "model": "grok-4.3",
            "input": "hello",
            "presence_penalty": 0.2,
            "stop": ["done"]
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert_eq!(body["presence_penalty"], 0.2);
        assert_eq!(body["stop"], serde_json::json!(["done"]));
    }
}
