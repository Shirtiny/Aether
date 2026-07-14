use serde_json::Value;

use crate::snapshot::GatewayProviderTransportSnapshot;

pub const GROK_DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

pub fn is_grok_provider_transport(transport: &GatewayProviderTransportSnapshot) -> bool {
    transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("grok")
}

pub fn grok_base_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        GROK_DEFAULT_BASE_URL.to_string()
    } else {
        base_url.to_string()
    }
}

pub fn build_grok_upstream_url(transport: &GatewayProviderTransportSnapshot, path: &str) -> String {
    let base_url = grok_base_url(&transport.endpoint.base_url);
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

/// Remove OpenAI Responses fields that the official xAI subscription
/// endpoint rejects. This intentionally stays smaller than the deferred
/// provider-specific tool filtering and multimedia compatibility work.
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
        for field in ["prompt_cache_retention", "safety_identifier"] {
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
    remove_grok_xai_field_recursively(body, "external_web_access");
}

fn remove_grok_xai_field_recursively(value: &mut Value, field: &str) {
    match value {
        Value::Object(object) => {
            object.remove(field);
            for child in object.values_mut() {
                remove_grok_xai_field_recursively(child, field);
            }
        }
        Value::Array(items) => {
            for item in items {
                remove_grok_xai_field_recursively(item, field);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_grok_xai_responses_body_edits, build_grok_upstream_url, grok_base_url,
        resolve_grok_bearer_auth, resolve_grok_model_alias,
    };
    use crate::snapshot::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider, GatewayProviderTransportSnapshot,
    };

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
    fn defaults_to_official_xai_api_root() {
        assert_eq!(grok_base_url(""), "https://api.x.ai/v1");
        let transport = sample_transport("access-token");
        assert_eq!(
            build_grok_upstream_url(&transport, "/responses"),
            "https://api.x.ai/v1/responses"
        );
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
            "presence_penalty": 0.2,
            "frequency_penalty": 0.3,
            "stop": ["done"]
        });

        apply_grok_xai_responses_body_edits(&mut body, "grok", "openai:responses");

        assert!(body.get("prompt_cache_retention").is_none());
        assert!(body.get("safety_identifier").is_none());
        assert!(body.get("presence_penalty").is_none());
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("stop").is_none());
        assert!(body["input"][0]["content"][0]
            .get("external_web_access")
            .is_none());
        assert_eq!(body["input"][0]["content"][0]["text"], "hello");
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
