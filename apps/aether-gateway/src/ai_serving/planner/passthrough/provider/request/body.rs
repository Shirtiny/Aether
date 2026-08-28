use serde_json::Value;

use super::super::LocalSameFormatProviderSpec;
use crate::ai_serving::transport::{
    build_same_format_provider_request_body as build_same_format_provider_request_body_impl,
    build_same_format_provider_request_body_with_gemini_schema as build_same_format_provider_request_body_with_gemini_schema_impl,
    SameFormatProviderFamily, SameFormatProviderRequestBodyInput,
};

pub(crate) fn build_same_format_provider_request_body(
    body_json: &Value,
    provider_type: &str,
    provider_api_format: &str,
    mapped_model: &str,
    spec: LocalSameFormatProviderSpec,
    body_rules: Option<&Value>,
    request_headers: Option<&http::HeaderMap>,
    upstream_is_stream: bool,
    force_body_stream_field: bool,
    kiro_auth: Option<&crate::ai_serving::transport::kiro::KiroRequestAuth>,
    is_claude_code: bool,
    enable_model_directives: bool,
) -> Option<Value> {
    build_same_format_provider_request_body_impl(same_format_provider_request_body_input(
        body_json,
        provider_type,
        provider_api_format,
        mapped_model,
        spec,
        body_rules,
        request_headers,
        upstream_is_stream,
        force_body_stream_field,
        kiro_auth,
        is_claude_code,
        enable_model_directives,
    ))
}

pub(crate) fn build_same_format_provider_request_body_with_gemini_schema(
    body_json: &Value,
    provider_type: &str,
    provider_api_format: &str,
    mapped_model: &str,
    spec: LocalSameFormatProviderSpec,
    body_rules: Option<&Value>,
    request_headers: Option<&http::HeaderMap>,
    upstream_is_stream: bool,
    force_body_stream_field: bool,
    kiro_auth: Option<&crate::ai_serving::transport::kiro::KiroRequestAuth>,
    is_claude_code: bool,
    enable_model_directives: bool,
) -> Option<Value> {
    build_same_format_provider_request_body_with_gemini_schema_impl(
        same_format_provider_request_body_input(
            body_json,
            provider_type,
            provider_api_format,
            mapped_model,
            spec,
            body_rules,
            request_headers,
            upstream_is_stream,
            force_body_stream_field,
            kiro_auth,
            is_claude_code,
            enable_model_directives,
        ),
    )
}

fn same_format_provider_request_body_input<'a>(
    body_json: &'a Value,
    provider_type: &'a str,
    provider_api_format: &'a str,
    mapped_model: &'a str,
    spec: LocalSameFormatProviderSpec,
    body_rules: Option<&'a Value>,
    request_headers: Option<&'a http::HeaderMap>,
    upstream_is_stream: bool,
    force_body_stream_field: bool,
    kiro_auth: Option<&'a crate::ai_serving::transport::kiro::KiroRequestAuth>,
    is_claude_code: bool,
    enable_model_directives: bool,
) -> SameFormatProviderRequestBodyInput<'a> {
    SameFormatProviderRequestBodyInput {
        body_json,
        mapped_model,
        provider_type,
        client_api_format: spec.api_format,
        provider_api_format,
        source_model: body_json.get("model").and_then(Value::as_str),
        family: same_format_provider_family(spec.family),
        body_rules,
        request_headers,
        upstream_is_stream,
        force_body_stream_field,
        kiro_auth_config: kiro_auth.map(|auth| &auth.auth_config),
        is_claude_code,
        enable_model_directives,
    }
}

fn same_format_provider_family(
    family: super::super::LocalSameFormatProviderFamily,
) -> SameFormatProviderFamily {
    match family {
        super::super::LocalSameFormatProviderFamily::Standard => SameFormatProviderFamily::Standard,
        super::super::LocalSameFormatProviderFamily::Gemini => SameFormatProviderFamily::Gemini,
    }
}
