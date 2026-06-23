use crate::async_task::CancelVideoTaskError;
use crate::constants::EXECUTION_PATH_LOCAL_AI_PUBLIC;
use crate::control::GatewayControlDecision;
use crate::control::GatewayPublicRequestContext;
use crate::image_capabilities::{
    openai_image_gateway_max_generation_count, openai_image_gateway_max_generation_count_for_model,
};
use crate::local_probe_intercept::{
    local_probe_intercept_answer, local_probe_intercept_enabled, LocalProbeInterceptKind,
};
use crate::{AppState, GatewayError};
use aether_ai_formats::UPSTREAM_IS_STREAM_KEY;
use aether_data_contracts::repository::video_tasks::{
    StoredVideoTask, VideoTaskQueryFilter, VideoTaskStatus,
};
use aether_usage_runtime::{
    attach_cafecode_identity_metadata, UsageEvent, UsageEventData, UsageEventType,
};
use axum::body::{Body, Bytes};
use axum::http::{self, HeaderMap, Response};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use std::time::Instant;
use tracing::warn;

const CLAUDE_COUNT_TOKENS_INVALID_PAYLOAD_DETAIL: &str = "Invalid token count payload";
const CLAUDE_COUNT_TOKENS_MISSING_BODY_DETAIL: &str = "请求体不能为空";
const GEMINI_VIDEO_TASK_NOT_FOUND_DETAIL: &str = "Video task not found";
const AI_PUBLIC_METHOD_NOT_ALLOWED_DETAIL: &str = "Method not allowed";
const AI_PUBLIC_UNAUTHORIZED_DETAIL: &str = "Unauthorized";
const OPENAI_IMAGE_PROMPT_DETAIL: &str = "图片生成/编辑请求缺少 prompt";
const OPENAI_IMAGE_EDIT_INPUT_DETAIL: &str = "图片编辑请求至少需要 1 张输入图片";
const OPENAI_IMAGE_PARTIAL_IMAGES_DETAIL: &str =
    "partial_images 仅支持 0-3，且必须配合 stream=true";
const OPENAI_IMAGE_STYLE_DETAIL: &str = "当前 Codex 图片反代暂不支持 style 参数";
const OPENAI_IMAGE_RESPONSE_FORMAT_DETAIL: &str = "response_format 仅支持 url 或 b64_json";
const OPENAI_IMAGE_OUTPUT_FORMAT_DETAIL: &str = "output_format 仅支持 png、jpeg 或 webp";
const OPENAI_IMAGE_QUALITY_DETAIL: &str = "quality 仅支持 low、medium、high、standard 或 hd";
const OPENAI_IMAGE_BACKGROUND_DETAIL: &str = "background 仅支持 auto、opaque 或 transparent";
const OPENAI_IMAGE_MODERATION_DETAIL: &str = "moderation 仅支持 auto 或 low";
const OPENAI_IMAGE_INPUT_FIDELITY_DETAIL: &str = "input_fidelity 仅支持 low 或 high";
const OPENAI_IMAGE_OUTPUT_COMPRESSION_DETAIL: &str = "output_compression 必须是 0-100 的整数";
const OPENAI_IMAGE_INVALID_JSON_DETAIL: &str = "图片接口 JSON 请求体无效";
const OPENAI_IMAGE_INVALID_MULTIPART_DETAIL: &str = "图片接口 multipart/form-data 请求体无效";
const OPENAI_EMBEDDING_CONTENT_TYPE_DETAIL: &str =
    "Embedding request content-type must be application/json";
const OPENAI_EMBEDDING_INVALID_JSON_DETAIL: &str = "Embedding request JSON body is invalid";
const OPENAI_EMBEDDING_MODEL_REQUIRED_DETAIL: &str = "Embedding request model is required";
const OPENAI_EMBEDDING_INPUT_REQUIRED_DETAIL: &str = "Embedding request input is required";
const OPENAI_EMBEDDING_CHAT_PAYLOAD_DETAIL: &str =
    "Embedding request must use input, not chat messages";
const OPENAI_EMBEDDING_STREAM_UNSUPPORTED_DETAIL: &str =
    "Embedding requests do not support streaming";
const OPENAI_RERANK_CONTENT_TYPE_DETAIL: &str =
    "Rerank request content-type must be application/json";
const OPENAI_RERANK_INVALID_JSON_DETAIL: &str = "Rerank request JSON body is invalid";
const OPENAI_RERANK_MODEL_REQUIRED_DETAIL: &str = "Rerank request model is required";
const OPENAI_RERANK_QUERY_REQUIRED_DETAIL: &str = "Rerank request query is required";
const OPENAI_RERANK_DOCUMENTS_REQUIRED_DETAIL: &str = "Rerank request documents are required";
const OPENAI_RERANK_TOP_N_DETAIL: &str = "Rerank request top_n must be a positive integer";
const OPENAI_RERANK_CHAT_PAYLOAD_DETAIL: &str =
    "Rerank request must use query/documents, not chat messages";
const OPENAI_RERANK_STREAM_UNSUPPORTED_DETAIL: &str = "Rerank requests do not support streaming";
const LOCAL_PROBE_RESPONSE_HEADER: &str = "x-aether-local-probe";
const ANTIGRAVITY_USER_SETTINGS_MISSING_BODY_DETAIL: &str =
    "Antigravity setUserSettings request body is required";
const ANTIGRAVITY_USER_SETTINGS_INVALID_JSON_DETAIL: &str =
    "Antigravity setUserSettings request JSON body is invalid";
const ANTIGRAVITY_USER_SETTINGS_INVALID_DETAIL: &str =
    "Antigravity setUserSettings request must include object userSettings";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenAiImageOperation {
    Generate,
    Edit,
}

impl OpenAiImageOperation {
    fn from_path(path: &str) -> Option<Self> {
        match path {
            "/v1/images/generations" => Some(Self::Generate),
            "/v1/images/edits" => Some(Self::Edit),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
struct OpenAiImageValidationInput {
    model: Option<String>,
    prompt: Option<String>,
    image_count: usize,
    n: Option<u64>,
    stream: bool,
    partial_images: Option<u64>,
    response_format: Option<String>,
    output_format: Option<String>,
    quality: Option<String>,
    background: Option<String>,
    moderation: Option<String>,
    input_fidelity: Option<String>,
    output_compression: Option<u64>,
    style_present: bool,
}

pub(crate) fn ai_public_local_requires_buffered_body(
    request_context: &GatewayPublicRequestContext,
) -> bool {
    request_context
        .control_decision
        .as_ref()
        .is_some_and(|decision| {
            decision.route_class.as_deref() == Some("ai_public")
                && request_context.request_method == http::Method::POST
                && ((decision.route_family.as_deref() == Some("claude")
                    && decision.route_kind.as_deref() == Some("count_tokens"))
                    || (decision.route_family.as_deref() == Some("openai")
                        && matches!(
                            decision.route_kind.as_deref(),
                            Some("chat") | Some("responses") | Some("responses:compact")
                        )
                        && matches!(
                            request_context.request_path.as_str(),
                            "/v1/chat/completions" | "/v1/responses" | "/v1/responses/compact"
                        ))
                    || (decision.route_family.as_deref() == Some("openai")
                        && decision.route_kind.as_deref() == Some("embedding")
                        && request_context.request_path == "/v1/embeddings")
                    || (decision.route_family.as_deref() == Some("openai")
                        && decision.route_kind.as_deref() == Some("rerank")
                        && request_context.request_path == "/v1/rerank")
                    || (decision.route_family.as_deref() == Some("antigravity")
                        && decision.route_kind.as_deref() != Some("stream_generate_content")))
        })
}

pub(crate) async fn maybe_build_local_ai_public_response(
    state: &AppState,
    request_context: &GatewayPublicRequestContext,
    request_headers: Option<&HeaderMap>,
    request_body: Option<&Bytes>,
    started_at: Option<&Instant>,
) -> Option<Response<Body>> {
    if let Some(response) = maybe_build_local_ai_public_route_guard_response(request_context) {
        return Some(response);
    }

    let decision = request_context.control_decision.as_ref()?;
    if decision.route_class.as_deref() != Some("ai_public") {
        return None;
    }

    if let Some(response) =
        maybe_build_local_openai_request_validation_response(request_context, request_body)
    {
        return Some(response);
    }

    if let Some(response) = maybe_build_local_openai_probe_response(
        state,
        request_context,
        request_headers,
        request_body,
        started_at,
    )
    .await
    {
        return Some(response);
    }

    if let Some(response) =
        maybe_build_local_claude_count_tokens_response(request_context, request_body)
    {
        return Some(response);
    }

    if let Some(response) =
        maybe_build_local_antigravity_v1internal_response(request_context, request_body)
    {
        return Some(response);
    }

    maybe_build_local_gemini_video_operations_response(state, request_context, decision).await
}

fn maybe_build_local_openai_request_validation_response(
    request_context: &GatewayPublicRequestContext,
    request_body: Option<&Bytes>,
) -> Option<Response<Body>> {
    let decision = request_context.control_decision.as_ref()?;
    if decision.route_family.as_deref() != Some("openai")
        || request_context.request_method != http::Method::POST
    {
        return None;
    }

    if decision.route_kind.as_deref() == Some("chat")
        && request_context.request_path == "/v1/chat/completions"
    {
        return None;
    }

    if decision.route_kind.as_deref() == Some("embedding")
        && request_context.request_path == "/v1/embeddings"
    {
        let Some(request_body) = request_body else {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                OPENAI_EMBEDDING_INVALID_JSON_DETAIL,
            ));
        };
        if let Err(detail) = validate_openai_embedding_request(
            request_context.request_content_type.as_deref(),
            request_body,
        ) {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                detail,
            ));
        }
        return None;
    }

    if decision.route_kind.as_deref() == Some("rerank")
        && request_context.request_path == "/v1/rerank"
    {
        let Some(request_body) = request_body else {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                OPENAI_RERANK_INVALID_JSON_DETAIL,
            ));
        };
        if let Err(detail) = validate_openai_rerank_request(
            request_context.request_content_type.as_deref(),
            request_body,
        ) {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                detail,
            ));
        }
        return None;
    }

    let request_body = request_body?;

    if decision.route_kind.as_deref() != Some("image")
        || !matches!(
            request_context.request_path.as_str(),
            "/v1/images/generations" | "/v1/images/edits"
        )
    {
        return None;
    }

    let Some(operation) = OpenAiImageOperation::from_path(&request_context.request_path) else {
        return None;
    };
    let validation = match parse_openai_image_validation_input(
        operation,
        request_context.request_content_type.as_deref(),
        request_body,
    ) {
        Ok(validation) => validation,
        Err(detail) => {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                detail,
            ));
        }
    };

    match operation {
        OpenAiImageOperation::Generate | OpenAiImageOperation::Edit
            if validation.prompt.is_none() =>
        {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                OPENAI_IMAGE_PROMPT_DETAIL,
            ));
        }
        OpenAiImageOperation::Edit if validation.image_count == 0 => {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                OPENAI_IMAGE_EDIT_INPUT_DETAIL,
            ));
        }
        _ => {}
    }

    if let Some(detail) = validate_openai_image_n(&validation) {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            detail,
        ));
    }

    if validation.partial_images.is_some_and(|value| value > 3)
        || (validation.partial_images.is_some() && !validation.stream)
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_PARTIAL_IMAGES_DETAIL,
        ));
    }

    if validation.style_present {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_STYLE_DETAIL,
        ));
    }

    if validation
        .response_format
        .as_deref()
        .is_some_and(|value| !matches!(value, "url" | "b64_json"))
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_RESPONSE_FORMAT_DETAIL,
        ));
    }

    if validation
        .output_format
        .as_deref()
        .is_some_and(|value| !matches!(value, "png" | "jpeg" | "jpg" | "webp"))
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_OUTPUT_FORMAT_DETAIL,
        ));
    }

    if validation
        .quality
        .as_deref()
        .is_some_and(|value| !matches!(value, "low" | "medium" | "high" | "standard" | "hd"))
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_QUALITY_DETAIL,
        ));
    }

    if validation
        .background
        .as_deref()
        .is_some_and(|value| !matches!(value, "auto" | "opaque" | "transparent"))
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_BACKGROUND_DETAIL,
        ));
    }

    if validation
        .moderation
        .as_deref()
        .is_some_and(|value| !matches!(value, "auto" | "low"))
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_MODERATION_DETAIL,
        ));
    }

    if validation
        .input_fidelity
        .as_deref()
        .is_some_and(|value| !matches!(value, "low" | "high"))
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_INPUT_FIDELITY_DETAIL,
        ));
    }

    if validation
        .output_compression
        .is_some_and(|value| value > 100)
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            OPENAI_IMAGE_OUTPUT_COMPRESSION_DETAIL,
        ));
    }

    None
}

fn openai_image_n_detail(max_generation_count: u64) -> String {
    if max_generation_count >= openai_image_gateway_max_generation_count() {
        format!("当前图片反代仅支持 n=1..{max_generation_count}")
    } else {
        format!("当前图片模型仅支持 n=1..{max_generation_count}")
    }
}

fn validate_openai_image_n(validation: &OpenAiImageValidationInput) -> Option<String> {
    let max_generation_count =
        openai_image_gateway_max_generation_count_for_model(validation.model.as_deref());
    validation
        .n
        .is_some_and(|value| value == 0 || value > max_generation_count)
        .then(|| openai_image_n_detail(max_generation_count))
}

fn validate_openai_embedding_request(
    content_type: Option<&str>,
    request_body: &Bytes,
) -> Result<(), &'static str> {
    if !content_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("application/json")
    {
        return Err(OPENAI_EMBEDDING_CONTENT_TYPE_DETAIL);
    }
    if request_body.is_empty() {
        return Err(OPENAI_EMBEDDING_INVALID_JSON_DETAIL);
    }
    let payload = serde_json::from_slice::<Value>(request_body)
        .map_err(|_| OPENAI_EMBEDDING_INVALID_JSON_DETAIL)?;
    let object = payload
        .as_object()
        .ok_or(OPENAI_EMBEDDING_INVALID_JSON_DETAIL)?;
    if object.contains_key("messages") {
        return Err(OPENAI_EMBEDDING_CHAT_PAYLOAD_DETAIL);
    }
    if object
        .get("stream")
        .and_then(value_as_bool)
        .unwrap_or(false)
    {
        return Err(OPENAI_EMBEDDING_STREAM_UNSUPPORTED_DETAIL);
    }
    if object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return Err(OPENAI_EMBEDDING_MODEL_REQUIRED_DETAIL);
    }
    let Some(input) = object.get("input") else {
        return Err(OPENAI_EMBEDDING_INPUT_REQUIRED_DETAIL);
    };
    if !embedding_input_is_non_empty(input) {
        return Err(OPENAI_EMBEDDING_INPUT_REQUIRED_DETAIL);
    }
    Ok(())
}

fn validate_openai_rerank_request(
    content_type: Option<&str>,
    request_body: &Bytes,
) -> Result<(), &'static str> {
    if !content_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("application/json")
    {
        return Err(OPENAI_RERANK_CONTENT_TYPE_DETAIL);
    }
    if request_body.is_empty() {
        return Err(OPENAI_RERANK_INVALID_JSON_DETAIL);
    }
    let payload = serde_json::from_slice::<Value>(request_body)
        .map_err(|_| OPENAI_RERANK_INVALID_JSON_DETAIL)?;
    let object = payload
        .as_object()
        .ok_or(OPENAI_RERANK_INVALID_JSON_DETAIL)?;
    if object.contains_key("messages") {
        return Err(OPENAI_RERANK_CHAT_PAYLOAD_DETAIL);
    }
    if object
        .get("stream")
        .and_then(value_as_bool)
        .unwrap_or(false)
    {
        return Err(OPENAI_RERANK_STREAM_UNSUPPORTED_DETAIL);
    }
    if object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return Err(OPENAI_RERANK_MODEL_REQUIRED_DETAIL);
    }
    if object
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return Err(OPENAI_RERANK_QUERY_REQUIRED_DETAIL);
    }
    let Some(documents) = object.get("documents").and_then(Value::as_array) else {
        return Err(OPENAI_RERANK_DOCUMENTS_REQUIRED_DETAIL);
    };
    if documents.is_empty() || documents.iter().any(rerank_document_is_empty) {
        return Err(OPENAI_RERANK_DOCUMENTS_REQUIRED_DETAIL);
    }
    if object
        .get("top_n")
        .or_else(|| object.get("topN"))
        .is_some_and(|value| !positive_json_integer(value))
    {
        return Err(OPENAI_RERANK_TOP_N_DETAIL);
    }
    Ok(())
}

fn rerank_document_is_empty(value: &Value) -> bool {
    match value {
        Value::String(text) => text.trim().is_empty(),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.trim().is_empty()),
        Value::Null => true,
        _ => false,
    }
}

fn positive_json_integer(value: &Value) -> bool {
    value.as_u64().is_some_and(|number| number > 0)
        || value.as_i64().is_some_and(|number| number > 0)
        || value
            .as_str()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .is_some_and(|number| number > 0)
}

fn embedding_input_is_non_empty(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) if !items.is_empty() => embedding_array_input_is_non_empty(items),
        _ => false,
    }
}

fn embedding_array_input_is_non_empty(items: &[Value]) -> bool {
    items
        .iter()
        .all(|item| item.as_str().is_some_and(|text| !text.trim().is_empty()))
        || embedding_token_array_is_non_empty(items)
        || items.iter().all(|item| {
            item.as_array()
                .is_some_and(|items| embedding_token_array_is_non_empty(items))
        })
        || items.iter().all(embedding_multimodal_content_is_non_empty)
}

fn embedding_token_array_is_non_empty(items: &[Value]) -> bool {
    !items.is_empty() && items.iter().all(|item| item.as_u64().is_some())
}

fn embedding_multimodal_content_is_non_empty(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let valid_text = object
        .get("text")
        .map(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()));
    let valid_image = object
        .get("image")
        .map(|value| value.as_str().is_some_and(|image| !image.trim().is_empty()));
    let valid_video = object
        .get("video")
        .map(|value| value.as_str().is_some_and(|video| !video.trim().is_empty()));
    let valid_multi_images = object.get("multi_images").map(|value| {
        value.as_array().is_some_and(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|image| !image.trim().is_empty()))
        })
    });

    [valid_text, valid_image, valid_video, valid_multi_images]
        .into_iter()
        .flatten()
        .all(|valid| valid)
        && [valid_text, valid_image, valid_video, valid_multi_images]
            .into_iter()
            .flatten()
            .any(|valid| valid)
}

fn image_request_count(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<u64>().ok())
        })
}

fn parse_openai_image_validation_input(
    operation: OpenAiImageOperation,
    content_type: Option<&str>,
    request_body: &Bytes,
) -> Result<OpenAiImageValidationInput, &'static str> {
    if request_body.is_empty() {
        return Err(match operation {
            OpenAiImageOperation::Generate | OpenAiImageOperation::Edit => {
                OPENAI_IMAGE_PROMPT_DETAIL
            }
        });
    }

    let content_type = content_type.unwrap_or_default();
    if content_type
        .to_ascii_lowercase()
        .contains("multipart/form-data")
    {
        parse_openai_image_validation_input_from_multipart(request_body, content_type)
    } else {
        parse_openai_image_validation_input_from_json(request_body)
    }
}

fn parse_openai_image_validation_input_from_json(
    request_body: &Bytes,
) -> Result<OpenAiImageValidationInput, &'static str> {
    let payload = serde_json::from_slice::<Value>(request_body)
        .map_err(|_| OPENAI_IMAGE_INVALID_JSON_DETAIL)?;
    let object = payload
        .as_object()
        .ok_or(OPENAI_IMAGE_INVALID_JSON_DETAIL)?;

    Ok(OpenAiImageValidationInput {
        model: normalize_openai_image_model_for_operation(
            object.get("model").and_then(Value::as_str),
        ),
        prompt: object
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        image_count: count_json_images(object),
        n: object.get("n").and_then(image_request_count),
        stream: object
            .get("stream")
            .and_then(value_as_bool)
            .unwrap_or(false),
        partial_images: object.get("partial_images").and_then(image_request_count),
        response_format: object
            .get("response_format")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        output_format: object
            .get("output_format")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        quality: object
            .get("quality")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        background: object
            .get("background")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        moderation: object
            .get("moderation")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        input_fidelity: object
            .get("input_fidelity")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        output_compression: object
            .get("output_compression")
            .and_then(image_request_count),
        style_present: object
            .get("style")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| !value.is_empty()),
    })
}

fn parse_openai_image_validation_input_from_multipart(
    request_body: &Bytes,
    content_type: &str,
) -> Result<OpenAiImageValidationInput, &'static str> {
    let boundary = multipart_boundary(content_type).ok_or(OPENAI_IMAGE_INVALID_MULTIPART_DETAIL)?;
    let fields = parse_multipart_fields(request_body, &boundary);
    if fields.is_empty() {
        return Err(OPENAI_IMAGE_INVALID_MULTIPART_DETAIL);
    }

    let model = fields
        .iter()
        .find(|field| field.name.trim() == "model")
        .map(|field| String::from_utf8_lossy(&field.data).trim().to_string());

    Ok(OpenAiImageValidationInput {
        model: normalize_openai_image_model_for_operation(model.as_deref()),
        prompt: multipart_text_field(&fields, "prompt"),
        image_count: fields
            .iter()
            .filter(|field| {
                matches!(
                    field.name.trim(),
                    "image" | "image[]" | "images" | "images[]"
                )
            })
            .count(),
        n: multipart_text_field(&fields, "n").and_then(|value| value.trim().parse::<u64>().ok()),
        stream: multipart_text_field(&fields, "stream")
            .and_then(|value| parse_bool_string(&value))
            .unwrap_or(false),
        partial_images: multipart_text_field(&fields, "partial_images")
            .and_then(|value| value.trim().parse::<u64>().ok()),
        response_format: multipart_text_field(&fields, "response_format")
            .map(|value| value.to_ascii_lowercase()),
        output_format: multipart_text_field(&fields, "output_format")
            .map(|value| value.to_ascii_lowercase()),
        quality: multipart_text_field(&fields, "quality").map(|value| value.to_ascii_lowercase()),
        background: multipart_text_field(&fields, "background")
            .map(|value| value.to_ascii_lowercase()),
        moderation: multipart_text_field(&fields, "moderation")
            .map(|value| value.to_ascii_lowercase()),
        input_fidelity: multipart_text_field(&fields, "input_fidelity")
            .map(|value| value.to_ascii_lowercase()),
        output_compression: multipart_text_field(&fields, "output_compression")
            .and_then(|value| value.trim().parse::<u64>().ok()),
        style_present: multipart_text_field(&fields, "style").is_some(),
    })
}

fn normalize_openai_image_model_for_operation(model: Option<&str>) -> Option<String> {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn count_json_images(object: &serde_json::Map<String, Value>) -> usize {
    let mut count = 0usize;
    if let Some(value) = object.get("image") {
        count += json_image_count(value);
    }
    if let Some(values) = object.get("images").and_then(Value::as_array) {
        count += values.iter().map(json_image_count).sum::<usize>();
    }
    count
}

fn json_image_count(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(json_image_count).sum(),
        Value::String(text) => (!text.trim().is_empty()) as usize,
        Value::Object(_) => 1,
        _ => 0,
    }
}

fn value_as_bool(value: &Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.as_str().and_then(parse_bool_string))
}

fn parse_bool_string(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

#[derive(Debug)]
struct MultipartField {
    name: String,
    data: Vec<u8>,
}

fn multipart_text_field(fields: &[MultipartField], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.trim() == name)
        .map(|field| String::from_utf8_lossy(&field.data).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_multipart_fields(body: &[u8], boundary: &str) -> Vec<MultipartField> {
    let delimiter = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut cursor = 0usize;

    while let Some(index) = find_subslice(&body[cursor..], &delimiter) {
        let start = cursor + index + delimiter.len();
        if body.get(start..start + 2) == Some(b"--") {
            break;
        }
        let mut part = &body[start..];
        if part.starts_with(b"\r\n") {
            part = &part[2..];
        }
        let Some(next) = find_subslice(part, &delimiter) else {
            break;
        };
        let raw = &part[..next];
        let raw = raw.strip_suffix(b"\r\n").unwrap_or(raw);
        if let Some(field) = parse_multipart_field(raw) {
            parts.push(field);
        }
        cursor = start + next;
    }

    parts
}

fn multipart_boundary(content_type: &str) -> Option<String> {
    content_type.split(';').find_map(|segment| {
        let (key, value) = segment.trim().split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("boundary") {
            return None;
        }
        let boundary = value.trim().trim_matches('"').trim();
        (!boundary.is_empty()).then(|| boundary.to_string())
    })
}

fn parse_multipart_field(raw: &[u8]) -> Option<MultipartField> {
    let header_end = find_subslice(raw, b"\r\n\r\n")?;
    let headers = &raw[..header_end];
    let data = raw.get(header_end + 4..)?.to_vec();
    let header_text = String::from_utf8_lossy(headers);

    let mut name = None;
    for line in header_text.lines() {
        let trimmed = line.trim();
        if trimmed
            .to_ascii_lowercase()
            .starts_with("content-disposition:")
        {
            name = extract_quoted_header_value(trimmed, "name");
        }
    }

    Some(MultipartField { name: name?, data })
}

fn extract_quoted_header_value(header: &str, key: &str) -> Option<String> {
    let pattern = format!("{key}=\"");
    let start = header.find(&pattern)? + pattern.len();
    let rest = &header[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn maybe_build_local_ai_public_route_guard_response(
    request_context: &GatewayPublicRequestContext,
) -> Option<Response<Body>> {
    if request_context.request_path == "/upload/v1beta/files"
        && request_context.request_method != http::Method::POST
    {
        return Some(build_ai_public_error_response(
            http::StatusCode::METHOD_NOT_ALLOWED,
            AI_PUBLIC_METHOD_NOT_ALLOWED_DETAIL,
        ));
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalProbeKind {
    Arithmetic,
    Ping,
    Health,
}

impl LocalProbeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Arithmetic => "arithmetic",
            Self::Ping => "ping",
            Self::Health => "health",
        }
    }
}

impl From<LocalProbeInterceptKind> for LocalProbeKind {
    fn from(kind: LocalProbeInterceptKind) -> Self {
        match kind {
            LocalProbeInterceptKind::Ping => Self::Ping,
            LocalProbeInterceptKind::Health => Self::Health,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LocalProbeAnswer {
    text: String,
    kind: LocalProbeKind,
}

async fn maybe_build_local_openai_probe_response(
    state: &AppState,
    request_context: &GatewayPublicRequestContext,
    request_headers: Option<&HeaderMap>,
    request_body: Option<&Bytes>,
    started_at: Option<&Instant>,
) -> Option<Response<Body>> {
    let decision = request_context.control_decision.as_ref()?;
    if decision.route_family.as_deref() != Some("openai")
        || request_context.request_method != http::Method::POST
    {
        return None;
    }
    if !local_probe_intercept_enabled(state).await.ok()? {
        return None;
    }

    let request_body = request_body?;
    let payload = serde_json::from_slice::<Value>(request_body).ok()?;
    let probe = match (
        decision.route_kind.as_deref(),
        request_context.request_path.as_str(),
    ) {
        (
            Some("responses") | Some("responses:compact"),
            "/v1/responses" | "/v1/responses/compact",
        ) => OpenAiLocalProbeRequest::Responses(
            openai_responses_local_probe_answer(state, &payload).await?,
        ),
        (Some("chat"), "/v1/chat/completions") => {
            OpenAiLocalProbeRequest::Chat(openai_chat_local_probe_answer(state, &payload).await?)
        }
        _ => return None,
    };
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("local-probe");
    let stream = payload
        .get("stream")
        .and_then(value_as_bool)
        .unwrap_or(false);
    let answer = probe.answer();
    record_local_openai_probe_usage(
        state,
        request_context,
        request_headers,
        Some(request_body),
        started_at,
        model,
        &answer,
        stream,
    )
    .await;
    Some(match probe {
        OpenAiLocalProbeRequest::Responses(answer) => build_openai_responses_local_probe_response(
            request_context,
            model,
            &answer.text,
            answer.kind,
            stream,
        ),
        OpenAiLocalProbeRequest::Chat(answer) => build_openai_chat_local_probe_response(
            request_context,
            model,
            &answer.text,
            answer.kind,
            stream,
        ),
    })
}

async fn openai_responses_local_probe_answer(
    state: &AppState,
    payload: &Value,
) -> Option<LocalProbeAnswer> {
    let text = extract_openai_responses_last_user_text(payload)?;
    local_probe_answer_from_text(state, &text)
        .await
        .ok()
        .flatten()
}

async fn openai_chat_local_probe_answer(
    state: &AppState,
    payload: &Value,
) -> Option<LocalProbeAnswer> {
    let text = extract_openai_chat_last_user_text(payload)?;
    local_probe_answer_from_text(state, &text)
        .await
        .ok()
        .flatten()
}

#[derive(Debug)]
enum OpenAiLocalProbeRequest {
    Responses(LocalProbeAnswer),
    Chat(LocalProbeAnswer),
}

impl OpenAiLocalProbeRequest {
    fn answer(&self) -> LocalProbeAnswer {
        match self {
            Self::Responses(answer) | Self::Chat(answer) => LocalProbeAnswer {
                text: answer.text.clone(),
                kind: answer.kind,
            },
        }
    }
}

fn extract_openai_responses_last_user_text(payload: &Value) -> Option<String> {
    let input = payload.get("input")?;
    match input {
        Value::String(text) => non_empty_trimmed(text),
        Value::Array(items) => items
            .iter()
            .rev()
            .find_map(openai_responses_input_item_user_text),
        _ => None,
    }
}

fn openai_responses_input_item_user_text(item: &Value) -> Option<String> {
    match item {
        Value::String(text) => non_empty_trimmed(text),
        Value::Object(object) => {
            if object
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| !role.eq_ignore_ascii_case("user"))
            {
                return None;
            }
            openai_responses_content_text(object.get("content"))
                .or_else(|| openai_responses_content_text(object.get("text")))
        }
        _ => None,
    }
}

fn openai_responses_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(text) => non_empty_trimmed(text),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(openai_responses_content_part_text)
                .collect::<Vec<_>>()
                .join(" ");
            non_empty_trimmed(&text)
        }
        Value::Object(_) => openai_responses_content_part_text(value),
        _ => None,
    }
}

fn openai_responses_content_part_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty_trimmed(text),
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("content"))
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed),
        _ => None,
    }
}

fn extract_openai_chat_last_user_text(payload: &Value) -> Option<String> {
    let messages = payload.get("messages")?.as_array()?;
    messages
        .iter()
        .rev()
        .find_map(openai_chat_message_user_text)
}

fn openai_chat_message_user_text(message: &Value) -> Option<String> {
    let object = message.as_object()?;
    if object
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| !role.eq_ignore_ascii_case("user"))
    {
        return None;
    }
    openai_chat_content_text(object.get("content"))
}

fn openai_chat_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(text) => non_empty_trimmed(text),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(openai_chat_content_part_text)
                .collect::<Vec<_>>()
                .join(" ");
            non_empty_trimmed(&text)
        }
        Value::Object(_) => openai_chat_content_part_text(value),
        _ => None,
    }
}

fn openai_chat_content_part_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty_trimmed(text),
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("content"))
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed),
        _ => None,
    }
}

async fn local_probe_answer_from_text(
    state: &AppState,
    text: &str,
) -> Result<Option<LocalProbeAnswer>, GatewayError> {
    if !local_probe_intercept_enabled(state).await? {
        return Ok(None);
    }
    if let Some(text) = arithmetic_probe_answer(text) {
        return Ok(Some(LocalProbeAnswer {
            text,
            kind: LocalProbeKind::Arithmetic,
        }));
    }
    Ok(local_probe_intercept_answer(state, text)
        .await?
        .map(|answer| LocalProbeAnswer {
            text: answer.text,
            kind: answer.kind.into(),
        }))
}

fn arithmetic_probe_answer(text: &str) -> Option<String> {
    let normalized = normalize_probe_text(text);
    let lower = normalized.to_ascii_lowercase();
    if !lower.starts_with("calculate and respond with only the number")
        || !lower.contains("nothing else")
    {
        return None;
    }
    let q_count = lower.match_indices("q:").count();
    if q_count < 2 {
        return None;
    }
    let question_start = lower.rfind("q:")? + 2;
    let answer_marker = lower[question_start..].rfind("a:")? + question_start;
    if !normalized[answer_marker + 2..].trim().is_empty() {
        return None;
    }
    let expression = normalized[question_start..answer_marker].trim();
    evaluate_simple_integer_expression(expression).map(|value| value.to_string())
}

fn evaluate_simple_integer_expression(expression: &str) -> Option<i64> {
    let expression = expression
        .trim()
        .trim_end_matches('?')
        .trim()
        .trim_end_matches('=')
        .trim();
    let (op_index, operator) = expression.char_indices().find_map(|(index, ch)| {
        matches!(ch, '+' | '-' | '*' | 'x' | 'X' | '×' | '/' | '÷').then_some((index, ch))
    })?;
    let left = expression[..op_index].trim().parse::<i64>().ok()?;
    let right_start = op_index + operator.len_utf8();
    let right = expression[right_start..].trim().parse::<i64>().ok()?;
    match operator {
        '+' => left.checked_add(right),
        '-' => left.checked_sub(right),
        '*' | 'x' | 'X' | '×' => left.checked_mul(right),
        '/' | '÷' if right != 0 && left % right == 0 => Some(left / right),
        _ => None,
    }
}

fn normalize_probe_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty_trimmed(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn build_openai_responses_local_probe_response(
    request_context: &GatewayPublicRequestContext,
    model: &str,
    text: &str,
    kind: LocalProbeKind,
    stream: bool,
) -> Response<Body> {
    let response_id = local_probe_response_id(&request_context.trace_id);
    let created_at = chrono::Utc::now().timestamp().max(0);
    let response = openai_responses_local_probe_payload(&response_id, model, text, created_at);
    if stream {
        let body = openai_responses_local_probe_sse_body(&response);
        return Response::builder()
            .status(http::StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "text/event-stream")
            .header(http::header::CACHE_CONTROL, "no-cache, no-transform")
            .header("x-accel-buffering", "no")
            .header(LOCAL_PROBE_RESPONSE_HEADER, kind.as_str())
            .body(Body::from(body))
            .expect("local probe stream response should build");
    }

    Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(LOCAL_PROBE_RESPONSE_HEADER, kind.as_str())
        .body(Body::from(
            serde_json::to_vec(&response).expect("local probe JSON should serialize"),
        ))
        .expect("local probe JSON response should build")
}

fn build_openai_chat_local_probe_response(
    request_context: &GatewayPublicRequestContext,
    model: &str,
    text: &str,
    kind: LocalProbeKind,
    stream: bool,
) -> Response<Body> {
    let response_id = local_probe_chat_response_id(&request_context.trace_id);
    let created_at = chrono::Utc::now().timestamp().max(0);
    if stream {
        let body = openai_chat_local_probe_sse_body(&response_id, model, text, created_at);
        return Response::builder()
            .status(http::StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "text/event-stream")
            .header(http::header::CACHE_CONTROL, "no-cache, no-transform")
            .header("x-accel-buffering", "no")
            .header(LOCAL_PROBE_RESPONSE_HEADER, kind.as_str())
            .body(Body::from(body))
            .expect("local chat probe stream response should build");
    }

    let response = openai_chat_local_probe_payload(&response_id, model, text, created_at);
    Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(LOCAL_PROBE_RESPONSE_HEADER, kind.as_str())
        .body(Body::from(
            serde_json::to_vec(&response).expect("local chat probe JSON should serialize"),
        ))
        .expect("local chat probe JSON response should build")
}

fn openai_chat_local_probe_payload(
    response_id: &str,
    model: &str,
    text: &str,
    created_at: i64,
) -> Value {
    json!({
        "id": response_id,
        "object": "chat.completion",
        "created": created_at,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    })
}

fn openai_chat_local_probe_sse_body(
    response_id: &str,
    model: &str,
    text: &str,
    created_at: i64,
) -> Vec<u8> {
    let events = [
        json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created_at,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created_at,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"content": text},
                "finish_reason": null
            }]
        }),
        json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created_at,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        }),
    ];

    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(
            &serde_json::to_string(&event).expect("local chat probe event should serialize"),
        );
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

fn openai_responses_local_probe_payload(
    response_id: &str,
    model: &str,
    text: &str,
    created_at: i64,
) -> Value {
    let message_id = format!("{response_id}_msg");
    let message = json!({
        "id": message_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": []
        }]
    });
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "completed",
        "model": model,
        "output": [message],
        "output_text": text,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "input_tokens_details": {
                "cached_tokens": 0
            },
            "output_tokens_details": {
                "reasoning_tokens": 0
            }
        }
    })
}

fn openai_responses_local_probe_sse_body(response: &Value) -> Vec<u8> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp_local_probe");
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("local-probe");
    let created_at = response
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let message_id = output
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_local_probe");
    let text = response
        .get("output_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content_part = json!({
        "type": "output_text",
        "text": text,
        "annotations": []
    });
    let created = json!({
        "type": "response.created",
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "in_progress",
            "model": model,
            "output": []
        }
    });
    let events = [
        ("response.created", created),
        (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": message_id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": []
                }
            }),
        ),
        (
            "response.content_part.added",
            json!({
                "type": "response.content_part.added",
                "item_id": message_id,
                "output_index": 0,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": []
                }
            }),
        ),
        (
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": message_id,
                "output_index": 0,
                "content_index": 0,
                "delta": text
            }),
        ),
        (
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "item_id": message_id,
                "output_index": 0,
                "content_index": 0,
                "text": text
            }),
        ),
        (
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "item_id": message_id,
                "output_index": 0,
                "content_index": 0,
                "part": content_part
            }),
        ),
        (
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": output
            }),
        ),
        (
            "response.completed",
            json!({
                "type": "response.completed",
                "response": response
            }),
        ),
    ];

    let mut body = String::new();
    for (event, data) in events {
        body.push_str("event: ");
        body.push_str(event);
        body.push('\n');
        body.push_str("data: ");
        body.push_str(&serde_json::to_string(&data).expect("local probe event should serialize"));
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

async fn record_local_openai_probe_usage(
    state: &AppState,
    request_context: &GatewayPublicRequestContext,
    request_headers: Option<&HeaderMap>,
    request_body: Option<&Bytes>,
    started_at: Option<&Instant>,
    model: &str,
    answer: &LocalProbeAnswer,
    stream: bool,
) {
    if !state.usage_runtime.is_enabled() {
        return;
    }
    let decision = request_context.control_decision.as_ref();
    let auth_context = decision.and_then(|value| value.auth_context.as_ref());
    let api_format = decision
        .and_then(|value| value.auth_endpoint_signature.as_deref())
        .or_else(
            || match decision.and_then(|value| value.route_kind.as_deref()) {
                Some("chat") => Some("openai:chat"),
                Some("responses") => Some("openai:responses"),
                Some("responses:compact") => Some("openai:responses:compact"),
                _ => None,
            },
        )
        .unwrap_or("openai");
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "trace_id".to_string(),
        Value::String(request_context.trace_id.clone()),
    );
    metadata.insert("is_ping".to_string(), Value::Bool(true));
    metadata.insert(
        "ping_kind".to_string(),
        Value::String(answer.kind.as_str().to_string()),
    );
    metadata.insert(
        "execution_path".to_string(),
        Value::String(EXECUTION_PATH_LOCAL_AI_PUBLIC.to_string()),
    );
    if let Some(route_family) = decision.and_then(|value| value.route_family.as_deref()) {
        metadata.insert(
            "route_family".to_string(),
            Value::String(route_family.to_string()),
        );
    }
    if let Some(route_kind) = decision.and_then(|value| value.route_kind.as_deref()) {
        metadata.insert(
            "route_kind".to_string(),
            Value::String(route_kind.to_string()),
        );
    }
    metadata.insert(
        "request_path".to_string(),
        Value::String(request_context.request_path.clone()),
    );
    metadata.insert(
        "request_path_and_query".to_string(),
        Value::String(request_context.request_path_and_query()),
    );
    metadata.insert("client_requested_stream".to_string(), Value::Bool(stream));
    metadata.insert(UPSTREAM_IS_STREAM_KEY.to_string(), Value::Bool(false));

    let content_type = if stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    let response_headers = json!({
        "content-type": content_type,
        LOCAL_PROBE_RESPONSE_HEADER: answer.kind.as_str(),
    });
    let request_headers = request_headers.map(local_probe_request_headers_json);
    let request_metadata =
        attach_cafecode_identity_metadata(Some(Value::Object(metadata)), request_headers.as_ref());
    let request_body = local_probe_request_body_json(request_body);
    let data = UsageEventData {
        user_id: auth_context.map(|value| value.user_id.clone()),
        api_key_id: auth_context.map(|value| value.api_key_id.clone()),
        username: auth_context.and_then(|value| value.username.clone()),
        api_key_name: auth_context.and_then(|value| value.api_key_name.clone()),
        provider_name: "local".to_string(),
        model: model.to_string(),
        request_type: Some("chat".to_string()),
        api_format: Some(api_format.to_string()),
        api_family: Some("openai".to_string()),
        endpoint_kind: local_probe_endpoint_kind(api_format).map(ToOwned::to_owned),
        endpoint_api_format: Some(api_format.to_string()),
        provider_api_family: Some("openai".to_string()),
        provider_endpoint_kind: local_probe_endpoint_kind(api_format).map(ToOwned::to_owned),
        has_format_conversion: Some(false),
        is_stream: Some(false),
        input_tokens: Some(0),
        output_tokens: Some(0),
        total_tokens: Some(0),
        total_cost_usd: Some(0.0),
        actual_total_cost_usd: Some(0.0),
        status_code: Some(http::StatusCode::OK.as_u16()),
        response_time_ms: started_at.map(|value| value.elapsed().as_millis() as u64),
        first_byte_time_ms: started_at.map(|value| value.elapsed().as_millis() as u64),
        request_headers,
        request_body,
        response_headers: Some(response_headers.clone()),
        client_response_headers: Some(response_headers),
        client_response_body: Some(json!({
            "local_probe": true,
            "kind": answer.kind.as_str(),
            "output_text": answer.text
        })),
        route_family: decision.and_then(|value| value.route_family.clone()),
        route_kind: decision.and_then(|value| value.route_kind.clone()),
        execution_path: Some(EXECUTION_PATH_LOCAL_AI_PUBLIC.to_string()),
        request_metadata,
        ..UsageEventData::default()
    };

    state
        .usage_runtime
        .record_terminal_event_direct(
            state.data.as_ref(),
            UsageEvent::new(
                UsageEventType::Completed,
                request_context.trace_id.clone(),
                data,
            ),
        )
        .await;
}

fn local_probe_endpoint_kind(api_format: &str) -> Option<&'static str> {
    let normalized = api_format.trim().to_ascii_lowercase().replace('_', ":");
    if normalized.contains("responses") {
        Some("responses")
    } else if normalized.contains("chat") || normalized == "openai" {
        Some("chat")
    } else {
        None
    }
}

fn local_probe_request_headers_json(headers: &HeaderMap) -> Value {
    let mut headers = crate::headers::collect_control_headers(headers);
    for (name, value) in headers.iter_mut() {
        if local_probe_sensitive_header(name) {
            *value = local_probe_mask_header_value(value);
        }
    }
    serde_json::to_value(headers).unwrap_or_else(|err| {
        warn!(
            error = %err,
            "gateway failed to serialize local probe request headers"
        );
        json!({})
    })
}

fn local_probe_request_body_json(body: Option<&Bytes>) -> Option<Value> {
    let body = body?;
    if body.is_empty() {
        return Some(json!({}));
    }
    serde_json::from_slice::<Value>(body.as_ref()).ok()
}

fn local_probe_sensitive_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "authorization"
            | "x-api-key"
            | "api-key"
            | "x-goog-api-key"
            | "cookie"
            | "set-cookie"
            | "proxy-authorization"
    )
}

fn local_probe_mask_header_value(value: &str) -> String {
    if value.len() <= 8 {
        return "****".to_string();
    }
    let prefix = value.chars().take(4).collect::<String>();
    let suffix = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{prefix}****{suffix}")
}

fn local_probe_response_id(trace_id: &str) -> String {
    let suffix = trace_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(32)
        .collect::<String>();
    if suffix.is_empty() {
        "resp_local_probe".to_string()
    } else {
        format!("resp_local_probe_{suffix}")
    }
}

fn local_probe_chat_response_id(trace_id: &str) -> String {
    let suffix = trace_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(32)
        .collect::<String>();
    if suffix.is_empty() {
        "chatcmpl_local_probe".to_string()
    } else {
        format!("chatcmpl_local_probe_{suffix}")
    }
}

fn maybe_build_local_claude_count_tokens_response(
    request_context: &GatewayPublicRequestContext,
    request_body: Option<&Bytes>,
) -> Option<Response<Body>> {
    let decision = request_context.control_decision.as_ref()?;
    if decision.route_family.as_deref() != Some("claude")
        || decision.route_kind.as_deref() != Some("count_tokens")
        || request_context.request_method != http::Method::POST
        || request_context.request_path != "/v1/messages/count_tokens"
    {
        return None;
    }

    let Some(request_body) = request_body else {
        return Some(build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            CLAUDE_COUNT_TOKENS_MISSING_BODY_DETAIL,
        ));
    };

    let payload = match serde_json::from_slice::<serde_json::Value>(request_body) {
        Ok(payload) => payload,
        Err(_) => {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                CLAUDE_COUNT_TOKENS_INVALID_PAYLOAD_DETAIL,
            ));
        }
    };

    let input_tokens = match estimate_claude_count_tokens(&payload) {
        Ok(tokens) => tokens,
        Err(_) => {
            return Some(build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                CLAUDE_COUNT_TOKENS_INVALID_PAYLOAD_DETAIL,
            ));
        }
    };

    Some(Json(json!({ "input_tokens": input_tokens })).into_response())
}

fn maybe_build_local_antigravity_v1internal_response(
    request_context: &GatewayPublicRequestContext,
    request_body: Option<&Bytes>,
) -> Option<Response<Body>> {
    let decision = request_context.control_decision.as_ref()?;
    if decision.route_family.as_deref() != Some("antigravity")
        || request_context.request_method != http::Method::POST
    {
        return None;
    }

    match decision.route_kind.as_deref()? {
        "load_code_assist" => {
            Some(Json(build_antigravity_load_code_assist_payload()).into_response())
        }
        "fetch_available_models" => {
            Some(Json(build_antigravity_fetch_available_models_payload()).into_response())
        }
        "fetch_user_info" => {
            Some(Json(build_antigravity_fetch_user_info_payload()).into_response())
        }
        "fetch_admin_controls" => Some(Json(json!({})).into_response()),
        "list_experiments" => Some(
            Json(json!({
                "experimentIds": [],
                "flags": []
            }))
            .into_response(),
        ),
        "record_code_assist_metrics" => Some(Json(json!({})).into_response()),
        "set_user_settings" => Some(build_antigravity_set_user_settings_response(request_body)),
        "stream_generate_content" => None,
        _ => None,
    }
}

fn build_antigravity_set_user_settings_response(request_body: Option<&Bytes>) -> Response<Body> {
    let Some(request_body) = request_body else {
        return build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            ANTIGRAVITY_USER_SETTINGS_MISSING_BODY_DETAIL,
        );
    };
    let payload = match serde_json::from_slice::<Value>(request_body) {
        Ok(payload) => payload,
        Err(_) => {
            return build_ai_public_error_response(
                http::StatusCode::BAD_REQUEST,
                ANTIGRAVITY_USER_SETTINGS_INVALID_JSON_DETAIL,
            );
        }
    };
    let Some(user_settings) = payload
        .get("userSettings")
        .filter(|value| value.is_object())
        .cloned()
    else {
        return build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            ANTIGRAVITY_USER_SETTINGS_INVALID_DETAIL,
        );
    };

    Json(json!({ "userSettings": user_settings })).into_response()
}

fn build_antigravity_load_code_assist_payload() -> Value {
    json!({
        "allowedTiers": [
            antigravity_free_tier_payload(true),
            antigravity_standard_tier_payload()
        ],
        "cloudaicompanionProject": "aether-antigravity-local",
        "currentTier": antigravity_free_tier_payload(false),
        "gcpManaged": false,
        "paidTier": antigravity_paid_tier_payload(),
        "upgradeSubscriptionUri": "https://codeassist.google.com/upgrade"
    })
}

fn antigravity_free_tier_payload(include_default_marker: bool) -> Value {
    if include_default_marker {
        json!({
            "id": "free-tier",
            "name": "Antigravity",
            "description": "Gemini-powered code suggestions and chat in multiple IDEs",
            "privacyNotice": {
                "showNotice": false
            },
            "isDefault": true
        })
    } else {
        json!({
            "id": "free-tier",
            "name": "Antigravity",
            "description": "Gemini-powered code suggestions and chat in multiple IDEs",
            "privacyNotice": {
                "showNotice": false
            },
            "upgradeSubscriptionUri": "https://codeassist.google.com/upgrade",
            "upgradeSubscriptionText": "Upgrade for higher Antigravity request limits",
            "upgradeSubscriptionType": "GDP_HELIUM"
        })
    }
}

fn antigravity_standard_tier_payload() -> Value {
    json!({
        "id": "standard-tier",
        "name": "Antigravity",
        "description": "Unlimited coding assistant with the most powerful Gemini models",
        "userDefinedCloudaicompanionProject": true,
        "privacyNotice": {},
        "usesGcpTos": true
    })
}

fn antigravity_paid_tier_payload() -> Value {
    json!({
        "id": "g1-pro-tier",
        "name": "Google AI Pro",
        "description": "Google AI Pro",
        "upgradeSubscriptionUri": "https://antigravity.google/g1-upgrade",
        "upgradeSubscriptionText": "Upgrade for the highest Antigravity request limits"
    })
}

fn build_antigravity_fetch_user_info_payload() -> Value {
    json!({
        "regionCode": "US",
        "userSettings": build_antigravity_default_user_settings_payload()
    })
}

fn build_antigravity_default_user_settings_payload() -> Value {
    json!({
        "preferredModelId": "gemini-3.1-flash-lite"
    })
}

fn build_antigravity_fetch_available_models_payload() -> Value {
    json!({
        "models": {
            "gemini-3.5-flash-low": antigravity_model_payload("gemini-3.5-flash-low", "Gemini 3.5 Flash Low"),
            "gemini-3-flash-agent": antigravity_model_payload("gemini-3-flash-agent", "Gemini 3 Flash Agent"),
            "gemini-3.1-flash-lite": antigravity_model_payload("gemini-3.1-flash-lite", "Gemini 3.1 Flash Lite"),
            "gemini-3.1-pro-low": antigravity_model_payload("gemini-3.1-pro-low", "Gemini 3.1 Pro Low"),
            "gemini-3-flash": antigravity_model_payload("gemini-3-flash", "Gemini 3 Flash"),
            "gemini-2.5-flash": antigravity_model_payload("gemini-2.5-flash", "Gemini 2.5 Flash"),
            "gemini-2.5-flash-lite": antigravity_model_payload("gemini-2.5-flash-lite", "Gemini 2.5 Flash Lite"),
            "gemini-2.5-flash-thinking": antigravity_model_payload("gemini-2.5-flash-thinking", "Gemini 2.5 Flash Thinking"),
            "gemini-2.5-pro": antigravity_model_payload("gemini-2.5-pro", "Gemini 2.5 Pro"),
            "gemini-3.1-flash-image": antigravity_model_payload("gemini-3.1-flash-image", "Gemini 3.1 Flash Image"),
            "tab_flash_lite_preview": antigravity_model_payload("tab_flash_lite_preview", "Tab Flash Lite Preview"),
            "tab_jump_flash_lite_preview": antigravity_model_payload("tab_jump_flash_lite_preview", "Tab Jump Flash Lite Preview"),
            "models/proactive-observer": antigravity_model_payload("models/proactive-observer", "Proactive Observer")
        },
        "agentModelSorts": [
            {
                "displayName": "Recommended",
                "groups": [
                    {
                        "modelIds": [
                            "gemini-3.1-flash-lite",
                            "gemini-3-flash-agent",
                            "gemini-3.1-pro-low",
                            "gemini-3.5-flash-low"
                        ]
                    }
                ]
            }
        ],
        "audioTranscriptionModelIds": ["models/proactive-observer"],
        "commandModelIds": ["gemini-3-flash"],
        "commitMessageModelIds": ["gemini-3.1-flash-lite"],
        "defaultAgentModelId": "gemini-3.1-flash-lite",
        "deprecatedModelIds": {},
        "experimentIds": [],
        "imageGenerationModelIds": ["gemini-3.1-flash-image"],
        "mqueryModelIds": ["gemini-3.1-flash-lite"],
        "tabModelIds": ["tab_flash_lite_preview", "tab_jump_flash_lite_preview"],
        "tieredModelIds": {
            "flash": ["gemini-3-flash-agent"],
            "flashLite": ["gemini-3.1-flash-lite"],
            "pro": ["gemini-3.1-pro-low"]
        },
        "webSearchModelIds": ["gemini-3.1-flash-lite"]
    })
}

fn antigravity_model_payload(id: &str, display_name: &str) -> Value {
    let model = match id {
        "gemini-2.5-flash" => "MODEL_GOOGLE_GEMINI_2_5_FLASH",
        "gemini-2.5-flash-lite" => "MODEL_GOOGLE_GEMINI_2_5_FLASH_LITE",
        "gemini-2.5-flash-thinking" => "MODEL_GOOGLE_GEMINI_2_5_FLASH_THINKING",
        "gemini-2.5-pro" => "MODEL_GOOGLE_GEMINI_2_5_PRO",
        "gemini-3-flash" => "MODEL_PLACEHOLDER_M18",
        "gemini-3-flash-agent" => "MODEL_PLACEHOLDER_M132",
        "gemini-3.1-flash-image" => "MODEL_PLACEHOLDER_M21",
        "gemini-3.1-flash-lite" => "MODEL_PLACEHOLDER_M50",
        "gemini-3.1-pro-low" => "MODEL_PLACEHOLDER_M36",
        "gemini-3.5-flash-low" => "MODEL_PLACEHOLDER_M20",
        "models/proactive-observer" => "MODEL_PLACEHOLDER_M70",
        "tab_flash_lite_preview" => "MODEL_PLACEHOLDER_M19",
        "tab_jump_flash_lite_preview" => "MODEL_PLACEHOLDER_M28",
        _ => "MODEL_PLACEHOLDER_M20",
    };
    json!({
        "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
        "displayName": display_name,
        "maxOutputTokens": 65536,
        "maxTokens": 1048576,
        "minThinkingBudget": 32,
        "model": model,
        "modelProvider": "MODEL_PROVIDER_GOOGLE",
        "recommended": id == "gemini-3.1-flash-lite",
        "supportedMimeTypes": {
            "application/json": true,
            "application/pdf": true,
            "image/jpeg": true,
            "image/png": true,
            "text/markdown": true,
            "text/plain": true
        },
        "supportsImages": true,
        "supportsThinking": true,
        "supportsVideo": true,
        "thinkingBudget": 4000,
        "tokenizerType": "LLAMA_WITH_SPECIAL"
    })
}

async fn maybe_build_local_gemini_video_operations_response(
    state: &AppState,
    request_context: &GatewayPublicRequestContext,
    decision: &GatewayControlDecision,
) -> Option<Response<Body>> {
    if decision.route_family.as_deref() != Some("gemini")
        || decision.route_kind.as_deref() != Some("video")
    {
        return None;
    }

    if request_context.request_path == "/v1beta/operations" {
        return Some(match request_context.request_method {
            http::Method::GET => {
                build_local_gemini_video_operations_list_response(state, decision).await
            }
            _ => build_ai_public_error_response(
                http::StatusCode::METHOD_NOT_ALLOWED,
                AI_PUBLIC_METHOD_NOT_ALLOWED_DETAIL,
            ),
        });
    }

    let Some(operation_path) = request_context
        .request_path
        .strip_prefix("/v1beta/operations/")
    else {
        return None;
    };

    Some(match request_context.request_method {
        http::Method::GET => {
            build_local_gemini_video_operation_detail_response(state, decision, operation_path)
                .await
        }
        http::Method::POST if operation_path.ends_with(":cancel") => {
            build_local_gemini_video_operation_cancel_response(state, decision, operation_path)
                .await
        }
        _ => build_ai_public_error_response(
            http::StatusCode::METHOD_NOT_ALLOWED,
            AI_PUBLIC_METHOD_NOT_ALLOWED_DETAIL,
        ),
    })
}

async fn build_local_gemini_video_operations_list_response(
    state: &AppState,
    decision: &GatewayControlDecision,
) -> Response<Body> {
    let Some(user_id) = decision
        .auth_context
        .as_ref()
        .map(|auth_context| auth_context.user_id.trim())
        .filter(|value| !value.is_empty())
    else {
        return build_ai_public_error_response(
            http::StatusCode::UNAUTHORIZED,
            AI_PUBLIC_UNAUTHORIZED_DETAIL,
        );
    };

    let filter = VideoTaskQueryFilter {
        user_id: Some(user_id.to_string()),
        status: None,
        model_substring: None,
        client_api_format: Some("gemini:video".to_string()),
    };
    let tasks = match state.list_video_task_page(&filter, 0, 100).await {
        Ok(tasks) => tasks,
        Err(err) => {
            return build_ai_public_error_response(
                http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("{err:?}"),
            );
        }
    };
    let operations = tasks
        .into_iter()
        .filter(is_gemini_video_task)
        .map(|task| build_gemini_video_operation_payload(&task))
        .collect::<Vec<_>>();

    Json(json!({ "operations": operations })).into_response()
}

async fn build_local_gemini_video_operation_detail_response(
    state: &AppState,
    decision: &GatewayControlDecision,
    operation_path: &str,
) -> Response<Body> {
    let task =
        match find_user_gemini_video_task_for_operation(state, decision, operation_path).await {
            Ok(Some(task)) => task,
            Ok(None) => {
                return build_ai_public_error_response(
                    http::StatusCode::NOT_FOUND,
                    GEMINI_VIDEO_TASK_NOT_FOUND_DETAIL,
                );
            }
            Err(err) => {
                return build_ai_public_error_response(
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{err:?}"),
                );
            }
        };

    Json(build_gemini_video_operation_payload(&task)).into_response()
}

async fn build_local_gemini_video_operation_cancel_response(
    state: &AppState,
    decision: &GatewayControlDecision,
    operation_path: &str,
) -> Response<Body> {
    let task =
        match find_user_gemini_video_task_for_operation(state, decision, operation_path).await {
            Ok(Some(task)) => task,
            Ok(None) => {
                return build_ai_public_error_response(
                    http::StatusCode::NOT_FOUND,
                    GEMINI_VIDEO_TASK_NOT_FOUND_DETAIL,
                );
            }
            Err(err) => {
                return build_ai_public_error_response(
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{err:?}"),
                );
            }
        };

    match crate::async_task::cancel_video_task_record(state, &task.id).await {
        Ok(_) => Json(json!({})).into_response(),
        Err(CancelVideoTaskError::NotFound) => build_ai_public_error_response(
            http::StatusCode::NOT_FOUND,
            GEMINI_VIDEO_TASK_NOT_FOUND_DETAIL,
        ),
        Err(CancelVideoTaskError::InvalidStatus(status)) => build_ai_public_error_response(
            http::StatusCode::BAD_REQUEST,
            format!(
                "Cannot cancel task with status: {}",
                video_task_status_name(status)
            ),
        ),
        Err(CancelVideoTaskError::Response(response)) => response,
        Err(CancelVideoTaskError::Gateway(err)) => build_ai_public_error_response(
            http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("{err:?}"),
        ),
    }
}

async fn find_user_gemini_video_task_for_operation(
    state: &AppState,
    decision: &GatewayControlDecision,
    operation_path: &str,
) -> Result<Option<StoredVideoTask>, GatewayError> {
    let Some(user_id) = decision
        .auth_context
        .as_ref()
        .map(|auth_context| auth_context.user_id.trim())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Some(short_id) = extract_short_id_from_gemini_operation_path(operation_path) else {
        return Ok(None);
    };
    let Some(task) = state.find_video_task_by_short_id(short_id).await? else {
        return Ok(None);
    };
    if task.user_id.as_deref().map(str::trim) != Some(user_id) || !is_gemini_video_task(&task) {
        return Ok(None);
    }
    Ok(Some(task))
}

fn extract_short_id_from_gemini_operation_path(operation_path: &str) -> Option<&str> {
    let trimmed = operation_path.trim_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let short_id = trimmed
        .strip_suffix(":cancel")
        .unwrap_or(trimmed)
        .rsplit('/')
        .next()?;
    (!short_id.is_empty()).then_some(short_id)
}

fn is_gemini_video_task(task: &StoredVideoTask) -> bool {
    matches!(
        task.provider_api_format
            .as_deref()
            .or(task.client_api_format.as_deref())
            .map(str::trim),
        Some("gemini:video")
    )
}

fn build_gemini_video_operation_payload(task: &StoredVideoTask) -> serde_json::Value {
    match task.status {
        VideoTaskStatus::Completed => json!({
            "name": gemini_video_operation_name(task),
            "done": true,
            "response": {
                "generateVideoResponse": {
                    "generatedSamples": [
                        {
                            "video": {
                                "uri": format!(
                                    "/v1beta/files/aev_{}:download?alt=media",
                                    gemini_operation_short_id(task)
                                ),
                                "mimeType": "video/mp4",
                            }
                        }
                    ]
                }
            }
        }),
        VideoTaskStatus::Failed | VideoTaskStatus::Expired => json!({
            "name": gemini_video_operation_name(task),
            "done": true,
            "error": {
                "code": task.error_code.clone().unwrap_or_else(|| "UNKNOWN".to_string()),
                "message": task
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Video generation failed".to_string()),
            }
        }),
        _ => json!({
            "name": gemini_video_operation_name(task),
            "done": false,
            "metadata": gemini_video_operation_metadata(task),
        }),
    }
}

fn gemini_video_operation_name(task: &StoredVideoTask) -> String {
    format!(
        "models/{}/operations/{}",
        gemini_operation_model(task),
        gemini_operation_short_id(task)
    )
}

fn gemini_operation_model(task: &StoredVideoTask) -> String {
    task.model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            task.external_task_id.as_deref().and_then(|external_id| {
                let parts = external_id.split('/').collect::<Vec<_>>();
                if parts.len() >= 2 && parts[0] == "models" && !parts[1].trim().is_empty() {
                    Some(parts[1].trim().to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn gemini_operation_short_id(task: &StoredVideoTask) -> String {
    task.short_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(task.id.as_str())
        .to_string()
}

fn gemini_video_operation_metadata(task: &StoredVideoTask) -> serde_json::Value {
    task.request_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("rust_local_snapshot"))
        .and_then(|snapshot| snapshot.get("Gemini"))
        .and_then(|gemini| gemini.get("metadata"))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn video_task_status_name(status: VideoTaskStatus) -> &'static str {
    match status {
        VideoTaskStatus::Pending => "pending",
        VideoTaskStatus::Submitted => "submitted",
        VideoTaskStatus::Queued => "queued",
        VideoTaskStatus::Processing => "processing",
        VideoTaskStatus::Completed => "completed",
        VideoTaskStatus::Failed => "failed",
        VideoTaskStatus::Cancelled => "cancelled",
        VideoTaskStatus::Expired => "expired",
        VideoTaskStatus::Deleted => "deleted",
    }
}

fn build_ai_public_error_response(
    status: http::StatusCode,
    detail: impl Into<String>,
) -> Response<Body> {
    (status, Json(json!({ "detail": detail.into() }))).into_response()
}

fn estimate_claude_count_tokens(payload: &serde_json::Value) -> Result<u64, ()> {
    let object = payload.as_object().ok_or(())?;
    let model = object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    if model.trim().is_empty() {
        return Err(());
    }

    let messages = object
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or(())?;

    let system_tokens = estimate_claude_system_tokens(object.get("system"))?;
    let message_tokens = estimate_claude_message_tokens(messages)?;
    Ok(system_tokens.saturating_add(message_tokens))
}

fn estimate_claude_system_tokens(system: Option<&serde_json::Value>) -> Result<u64, ()> {
    let Some(system) = system else {
        return Ok(0);
    };

    match system {
        serde_json::Value::Null => Ok(0),
        serde_json::Value::String(text) => Ok(estimate_text_tokens(text)),
        serde_json::Value::Array(blocks) => {
            let mut total = 0_u64;
            for block in blocks {
                let block = block.as_object().ok_or(())?;
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    total = total.saturating_add(estimate_text_tokens(text));
                }
            }
            Ok(total)
        }
        serde_json::Value::Object(_) => Ok(0),
        _ => Err(()),
    }
}

fn estimate_claude_message_tokens(messages: &[serde_json::Value]) -> Result<u64, ()> {
    let mut total = 0_u64;

    for message in messages {
        let message = message.as_object().ok_or(())?;
        let role = message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .ok_or(())?;
        if !matches!(role, "user" | "assistant") {
            return Err(());
        }

        total = total.saturating_add(4);
        let content = message.get("content").ok_or(())?;
        match content {
            serde_json::Value::String(text) => {
                total = total.saturating_add(estimate_text_tokens(text));
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    let item = item.as_object().ok_or(())?;
                    if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                        total = total.saturating_add(estimate_text_tokens(text));
                    }
                }
            }
            _ => return Err(()),
        }
    }

    Ok(total)
}

fn estimate_text_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }

    let char_count = text.chars().count() as u64;
    std::cmp::max(1, char_count / 4)
}

#[cfg(test)]
mod tests {
    use super::{
        arithmetic_probe_answer, estimate_claude_count_tokens, local_probe_chat_response_id,
        local_probe_request_headers_json, local_probe_response_id, openai_chat_local_probe_answer,
        openai_chat_local_probe_sse_body, openai_responses_local_probe_answer,
        openai_responses_local_probe_payload, openai_responses_local_probe_sse_body,
        parse_openai_image_validation_input, validate_openai_image_n, LocalProbeKind,
        OpenAiImageOperation,
    };
    use crate::data::GatewayDataState;
    use crate::local_probe_intercept::{
        LOCAL_PROBE_INTERCEPT_ENABLED_KEY, LOCAL_PROBE_INTERCEPT_RULES_KEY,
    };
    use crate::AppState;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, HeaderValue};
    use serde_json::json;

    fn probe_test_state() -> AppState {
        AppState::new()
            .expect("gateway state should build")
            .with_data_state_for_tests(
                GatewayDataState::disabled().with_system_config_values_for_tests([
                    (LOCAL_PROBE_INTERCEPT_ENABLED_KEY.to_string(), json!(true)),
                    (
                        LOCAL_PROBE_INTERCEPT_RULES_KEY.to_string(),
                        json!([
                            {
                                "id": "ping",
                                "name": "Ping",
                                "prompt": "ping",
                                "response": "pong",
                                "kind": "ping",
                                "enabled": true,
                                "system": true,
                            },
                            {
                                "id": "reply_ok",
                                "name": "Reply OK",
                                "prompt": "Reply exactly: OK",
                                "response": "OK",
                                "kind": "health",
                                "enabled": true,
                                "system": true,
                            },
                        ]),
                    ),
                ]),
            )
    }

    fn probe_disabled_test_state() -> AppState {
        AppState::new()
            .expect("gateway state should build")
            .with_data_state_for_tests(
                GatewayDataState::disabled().with_system_config_values_for_tests([(
                    LOCAL_PROBE_INTERCEPT_ENABLED_KEY.to_string(),
                    json!(false),
                )]),
            )
    }

    fn probe_default_rules_test_state() -> AppState {
        AppState::new()
            .expect("gateway state should build")
            .with_data_state_for_tests(
                GatewayDataState::disabled().with_system_config_values_for_tests([(
                    LOCAL_PROBE_INTERCEPT_ENABLED_KEY.to_string(),
                    json!(true),
                )]),
            )
    }

    #[test]
    fn estimates_claude_count_tokens_from_system_and_messages() {
        let payload = json!({
            "model": "claude-sonnet-4-5",
            "system": [{"type": "text", "text": "abcdefghijklmnop"}],
            "messages": [
                {
                    "role": "user",
                    "content": "abcdefghijkl"
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "abcdefgh"},
                        {"type": "tool_use", "name": "ignored", "input": {"city": "SF"}}
                    ]
                }
            ]
        });

        assert_eq!(estimate_claude_count_tokens(&payload), Ok(17));
    }

    #[test]
    fn rejects_invalid_claude_count_tokens_payload() {
        let payload = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{"role": "system", "content": "bad"}]
        });

        assert_eq!(estimate_claude_count_tokens(&payload), Err(()));
    }

    #[test]
    fn arithmetic_probe_answer_solves_final_question_only() {
        let prompt = concat!(
            "Calculate and respond with ONLY the number, nothing else. ",
            "Q: 3 + 5 = ? A: 8 ",
            "Q: 12 - 7 = ? A: 5 ",
            "Q: 17 + 4 = ? A:"
        );

        assert_eq!(arithmetic_probe_answer(prompt).as_deref(), Some("21"));
    }

    #[test]
    fn arithmetic_probe_answer_ignores_ordinary_math_requests() {
        assert_eq!(arithmetic_probe_answer("what is 17 + 4?"), None);
        assert_eq!(
            arithmetic_probe_answer(
                "Calculate and respond with ONLY the number, nothing else. Q: 17 + 4 = ? A:"
            ),
            None
        );
    }

    #[tokio::test]
    async fn openai_responses_local_probe_answer_reads_responses_message_input() {
        let state = probe_test_state();
        let payload = json!({
            "model": "gpt-5.4-mini",
            "stream": true,
            "instructions": "You are GPT.",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": concat!(
                        "Calculate and respond with ONLY the number, nothing else. ",
                        "Q: 3 + 5 = ? A: 8 ",
                        "Q: 12 - 7 = ? A: 5 ",
                        "Q: 17 + 4 = ? A:"
                    )
                }]
            }]
        });

        assert_eq!(
            openai_responses_local_probe_answer(&state, &payload).await,
            Some(super::LocalProbeAnswer {
                text: "21".to_string(),
                kind: LocalProbeKind::Arithmetic,
            })
        );
    }

    #[tokio::test]
    async fn openai_responses_local_probe_answer_reads_string_input() {
        let state = probe_test_state();
        let payload = json!({
            "model": "gpt-5.4-mini",
            "input": concat!(
                "Calculate and respond with ONLY the number, nothing else. ",
                "Q: 2 * 3 = ? A: 6 ",
                "Q: 8 / 4 = ? A: 2 ",
                "Q: 9 - 5 = ? A:"
            )
        });

        assert_eq!(
            openai_responses_local_probe_answer(&state, &payload).await,
            Some(super::LocalProbeAnswer {
                text: "4".to_string(),
                kind: LocalProbeKind::Arithmetic,
            })
        );
    }

    #[tokio::test]
    async fn openai_chat_local_probe_answer_reads_configured_chat_message_input() {
        let state = probe_test_state();
        let payload = json!({
            "model": "gpt-5.4-mini",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "ping"
                }]
            }]
        });

        assert_eq!(
            openai_chat_local_probe_answer(&state, &payload).await,
            Some(super::LocalProbeAnswer {
                text: "pong".to_string(),
                kind: LocalProbeKind::Ping,
            })
        );
    }

    #[tokio::test]
    async fn openai_chat_local_probe_answer_uses_default_rules_when_rules_config_missing() {
        let state = probe_default_rules_test_state();
        let payload = json!({
            "model": "gpt-5.4-mini",
            "messages": [{
                "role": "user",
                "content": "Reply exactly: OK"
            }]
        });

        assert_eq!(
            openai_chat_local_probe_answer(&state, &payload).await,
            Some(super::LocalProbeAnswer {
                text: "OK".to_string(),
                kind: LocalProbeKind::Health,
            })
        );
    }

    #[tokio::test]
    async fn configured_probe_prompt_does_not_match_when_module_disabled() {
        let state = probe_disabled_test_state();
        let payload = json!({
            "model": "gpt-5.4-mini",
            "messages": [{
                "role": "user",
                "content": "ping"
            }]
        });

        assert_eq!(openai_chat_local_probe_answer(&state, &payload).await, None);
    }

    #[tokio::test]
    async fn arithmetic_probe_answer_does_not_match_when_module_disabled() {
        let state = probe_disabled_test_state();
        let payload = json!({
            "model": "gpt-5.4-mini",
            "input": concat!(
                "Calculate and respond with ONLY the number, nothing else. ",
                "Q: 2 * 3 = ? A: 6 ",
                "Q: 8 / 4 = ? A: 2 ",
                "Q: 9 - 5 = ? A:"
            )
        });

        assert_eq!(
            openai_responses_local_probe_answer(&state, &payload).await,
            None
        );
    }

    #[test]
    fn local_probe_sse_contains_completed_response() {
        let response = openai_responses_local_probe_payload("resp_probe", "gpt-5.4-mini", "21", 1);
        let body = String::from_utf8(openai_responses_local_probe_sse_body(&response))
            .expect("sse body should be utf-8");

        assert!(body.contains("event: response.completed"));
        assert!(body.contains("\"output_text\":\"21\""));
        assert!(body.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn local_chat_probe_sse_contains_completion_chunk() {
        let body = String::from_utf8(openai_chat_local_probe_sse_body(
            "chatcmpl_probe",
            "gpt-5.4-mini",
            "pong",
            1,
        ))
        .expect("sse body should be utf-8");

        assert!(body.contains("\"object\":\"chat.completion.chunk\""));
        assert!(body.contains("pong"));
        assert!(body.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn local_probe_response_id_uses_trace_suffix() {
        assert_eq!(
            local_probe_response_id("24d30b78-08eb-433b-b140-20b2667e6a5f"),
            "resp_local_probe_24d30b7808eb433bb14020b2667e6a5f"
        );
        assert_eq!(
            local_probe_chat_response_id("24d30b78-08eb-433b-b140-20b2667e6a5f"),
            "chatcmpl_local_probe_24d30b7808eb433bb14020b2667e6a5f"
        );
    }

    #[test]
    fn local_probe_usage_metadata_extracts_cafecode_identity_from_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("cafecode-uid", HeaderValue::from_static("124"));
        headers.insert("Cafecode-Uname", HeaderValue::from_static("qingteng2025"));

        let request_headers = local_probe_request_headers_json(&headers);
        let metadata = aether_usage_runtime::attach_cafecode_identity_metadata(
            Some(json!({ "is_ping": true })),
            Some(&request_headers),
        )
        .expect("cafecode metadata should be present");

        assert_eq!(metadata["cafecode_uid"], "124");
        assert_eq!(metadata["cafecode_uname"], "qingteng2025");
        assert_eq!(metadata["is_ping"], true);
    }

    #[test]
    fn image_validation_accepts_custom_model_name() {
        let body =
            Bytes::from_static(br#"{"model":" Custom/Image-Model:V1 ","prompt":"draw an image"}"#);

        let validation = parse_openai_image_validation_input(
            OpenAiImageOperation::Generate,
            Some("application/json"),
            &body,
        )
        .expect("custom image model should validate");

        assert_eq!(validation.model.as_deref(), Some("Custom/Image-Model:V1"));
    }

    #[test]
    fn image_validation_accepts_multipart_with_mixed_case_boundary() {
        let boundary = "------------------------OYNWsMZCt0ILTwn8naP4Gb";
        let body = Bytes::from(format!(
            concat!(
                "--{boundary}\r\n",
                "Content-Disposition: form-data; name=\"model\"\r\n\r\n",
                "gpt-image-2\r\n",
                "--{boundary}\r\n",
                "Content-Disposition: form-data; name=\"prompt\"\r\n\r\n",
                "edit this image\r\n",
                "--{boundary}\r\n",
                "Content-Disposition: form-data; name=\"image\"; filename=\"image.jpg\"\r\n",
                "Content-Type: image/jpeg\r\n\r\n",
                "image-bytes\r\n",
                "--{boundary}--\r\n"
            ),
            boundary = boundary,
        ));

        let validation = parse_openai_image_validation_input(
            OpenAiImageOperation::Edit,
            Some(&format!("multipart/form-data; boundary={boundary}")),
            &body,
        )
        .expect("multipart image edit should validate");

        assert_eq!(validation.model.as_deref(), Some("gpt-image-2"));
        assert_eq!(validation.prompt.as_deref(), Some("edit this image"));
        assert_eq!(validation.image_count, 1);
    }

    #[test]
    fn image_validation_restricts_multi_image_count_to_grok_models() {
        let openai_body = Bytes::from_static(br#"{"model":"gpt-image-2","prompt":"draw","n":2}"#);
        let openai_validation = parse_openai_image_validation_input(
            OpenAiImageOperation::Generate,
            Some("application/json"),
            &openai_body,
        )
        .expect("valid image payload should parse");

        assert_eq!(
            validate_openai_image_n(&openai_validation).as_deref(),
            Some("当前图片模型仅支持 n=1..1")
        );

        let grok_body =
            Bytes::from_static(br#"{"model":"grok-imagine-image-lite","prompt":"draw","n":4}"#);
        let grok_validation = parse_openai_image_validation_input(
            OpenAiImageOperation::Generate,
            Some("application/json"),
            &grok_body,
        )
        .expect("valid grok image payload should parse");

        assert!(validate_openai_image_n(&grok_validation).is_none());
    }
}
