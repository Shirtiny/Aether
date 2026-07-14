use serde_json::Value;

use crate::ai_serving::transport::apply_standard_provider_request_body_rules_with_request_headers;
use crate::ai_serving::{
    apply_codex_openai_responses_special_body_edits,
    apply_openai_responses_compact_special_body_edits,
    build_cross_format_openai_responses_request_body_with_model_directives as surface_build_cross_format_openai_responses_request_body,
    build_local_openai_responses_request_body_with_model_directives as surface_build_local_openai_responses_request_body,
    GatewayProviderTransportSnapshot,
};

use super::{enforce_provider_body_stream_policy, request_requires_body_stream_field};

pub(crate) fn build_local_openai_responses_request_body(
    body_json: &Value,
    mapped_model: &str,
    require_streaming: bool,
    force_body_stream_field: bool,
    provider_type: &str,
    provider_api_format: &str,
    body_rules: Option<&Value>,
    user_api_key_id: Option<&str>,
    request_headers: &http::HeaderMap,
    enable_model_directives: bool,
) -> Option<Value> {
    let provider_request_body = surface_build_local_openai_responses_request_body(
        body_json,
        mapped_model,
        require_streaming,
        enable_model_directives,
    )?;
    let mut provider_request_body =
        apply_standard_provider_request_body_rules_with_request_headers(
            provider_request_body,
            body_rules,
            body_json,
            request_headers,
        )?;
    apply_codex_openai_responses_special_body_edits(
        &mut provider_request_body,
        provider_type,
        provider_api_format,
        body_rules,
        user_api_key_id,
    );
    crate::provider_transport::apply_grok_xai_responses_body_edits(
        &mut provider_request_body,
        provider_type,
        provider_api_format,
    );
    apply_openai_responses_compact_special_body_edits(
        &mut provider_request_body,
        provider_api_format,
    );
    enforce_provider_body_stream_policy(
        &mut provider_request_body,
        provider_api_format,
        require_streaming,
        request_requires_body_stream_field(body_json, force_body_stream_field),
    );
    Some(provider_request_body)
}

pub(crate) fn build_cross_format_openai_responses_request_body(
    body_json: &Value,
    mapped_model: &str,
    client_api_format: &str,
    provider_api_format: &str,
    upstream_is_stream: bool,
    force_body_stream_field: bool,
    provider_type: &str,
    body_rules: Option<&Value>,
    user_api_key_id: Option<&str>,
    request_headers: &http::HeaderMap,
    enable_model_directives: bool,
) -> Option<Value> {
    let provider_request_body = surface_build_cross_format_openai_responses_request_body(
        body_json,
        mapped_model,
        client_api_format,
        provider_api_format,
        upstream_is_stream,
        enable_model_directives,
    )?;
    let mut provider_request_body =
        apply_standard_provider_request_body_rules_with_request_headers(
            provider_request_body,
            body_rules,
            body_json,
            request_headers,
        )?;
    apply_codex_openai_responses_special_body_edits(
        &mut provider_request_body,
        provider_type,
        provider_api_format,
        body_rules,
        user_api_key_id,
    );
    crate::provider_transport::apply_grok_xai_responses_body_edits(
        &mut provider_request_body,
        provider_type,
        provider_api_format,
    );
    apply_openai_responses_compact_special_body_edits(
        &mut provider_request_body,
        provider_api_format,
    );
    enforce_provider_body_stream_policy(
        &mut provider_request_body,
        provider_api_format,
        upstream_is_stream,
        request_requires_body_stream_field(body_json, force_body_stream_field),
    );
    Some(provider_request_body)
}

pub(crate) fn build_local_openai_responses_upstream_url(
    parts: &http::request::Parts,
    transport: &GatewayProviderTransportSnapshot,
    compact: bool,
) -> Option<String> {
    crate::ai_serving::transport::build_local_openai_responses_upstream_url(
        transport,
        compact,
        parts.uri.query(),
    )
}

pub(crate) fn build_cross_format_openai_responses_upstream_url(
    parts: &http::request::Parts,
    transport: &GatewayProviderTransportSnapshot,
    mapped_model: &str,
    client_api_format: &str,
    provider_api_format: &str,
    upstream_is_stream: bool,
) -> Option<String> {
    crate::ai_serving::transport::build_cross_format_openai_responses_upstream_url(
        transport,
        mapped_model,
        client_api_format,
        provider_api_format,
        upstream_is_stream,
        parts.uri.query(),
    )
}
