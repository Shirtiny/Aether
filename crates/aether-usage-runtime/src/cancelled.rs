use aether_contracts::{ExecutionPlan, StandardizedUsage};
use serde_json::{json, Value};

const CANCELLED_INPUT_ESTIMATE_MAX_TOKENS: u64 = 8_000_000;
const CANCELLED_INPUT_ESTIMATE_MAX_SCAN_BYTES: u64 = 1024 * 1024;
const CANCELLED_INPUT_ESTIMATE_MAX_NODES: u64 = 32_768;
const CANCELLED_INPUT_ESTIMATE_MAX_DEPTH: usize = 64;
pub const CANCELLED_INPUT_ESTIMATE_SOURCE: &str = "gateway_cached_input_floor";
pub const CANCELLED_CONTEXT_FLOOR_SOURCE: &str = "previous_response_context_floor";

pub fn terminal_usage_is_cancelled(status_code: u16, report_kind: &str) -> bool {
    status_code == 499
        || report_kind
            .as_bytes()
            .windows(b"cancel".len())
            .any(|window| window.eq_ignore_ascii_case(b"cancel"))
}

/// Builds a conservative, explicitly requested billing floor for a
/// provider-reached request that was cancelled before authoritative usage was
/// available.
///
/// Callers must establish that provider dispatch started. This helper never
/// inspects a partial response and therefore adds no per-chunk streaming work.
pub fn cancelled_usage_billing_floor(
    plan: &ExecutionPlan,
    report_context: Option<&Value>,
    existing_usage: Option<StandardizedUsage>,
) -> Option<StandardizedUsage> {
    let had_existing_usage = existing_usage.is_some();
    let mut usage = existing_usage.unwrap_or_default();
    let context_floor = usage
        .dimensions
        .get("usage_source")
        .and_then(Value::as_str)
        .is_some_and(|source| source == CANCELLED_CONTEXT_FLOOR_SOURCE);
    let has_estimateable_endpoint = cancellation_floor_supports_endpoint(plan, report_context);
    if !has_estimateable_endpoint {
        return had_existing_usage.then_some(usage);
    }
    if had_existing_usage && usage.input_tokens > 0 && !context_floor {
        return Some(usage);
    }
    let Some(input_tokens) = cancelled_input_estimate(plan, report_context) else {
        return (had_existing_usage || usage.has_token_signal()).then_some(usage);
    };
    let is_kiro = plan
        .provider_name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("kiro"));
    let context_cache_creation = is_kiro
        .then(|| context_u64(report_context, "cache_creation_input_tokens"))
        .flatten()
        .unwrap_or(0)
        .min(input_tokens);
    let context_cache_read = is_kiro
        .then(|| context_u64(report_context, "cache_read_input_tokens"))
        .flatten()
        .unwrap_or(0)
        .min(input_tokens.saturating_sub(context_cache_creation));
    if usage.cache_creation_tokens <= 0 && context_cache_creation > 0 {
        usage.cache_creation_tokens = i64::try_from(context_cache_creation).unwrap_or(i64::MAX);
    }
    if usage.cache_read_tokens <= 0 && context_cache_read > 0 {
        usage.cache_read_tokens = i64::try_from(context_cache_read).unwrap_or(i64::MAX);
    }
    if context_floor {
        usage.input_tokens = usage
            .cache_read_tokens
            .max(0)
            .saturating_add(i64::try_from(input_tokens).unwrap_or(i64::MAX));
    } else if usage.input_tokens <= 0 {
        let billed_input_tokens = if is_kiro {
            input_tokens
                .saturating_sub(context_cache_creation)
                .saturating_sub(context_cache_read)
        } else {
            input_tokens
        };
        usage.input_tokens = i64::try_from(billed_input_tokens).unwrap_or(i64::MAX);
        if usage.cache_read_tokens <= 0
            && usage.cache_creation_tokens <= 0
            && usage_format_supports_cache_floor(plan, report_context)
        {
            usage.cache_read_tokens = usage.input_tokens;
        }
    } else {
        return Some(usage);
    }

    usage.dimensions.insert(
        "usage_source".to_string(),
        json!(if context_floor {
            "gateway_cached_context_plus_input_estimate"
        } else {
            CANCELLED_INPUT_ESTIMATE_SOURCE
        }),
    );
    usage
        .dimensions
        .insert("usage_confidence".to_string(), json!("billing_floor"));
    normalize_estimated_total(&mut usage);
    Some(usage)
}

fn context_u64(report_context: Option<&Value>, key: &str) -> Option<u64> {
    report_context
        .and_then(Value::as_object)
        .and_then(|context| context.get(key))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        })
}

fn cancelled_input_estimate(plan: &ExecutionPlan, report_context: Option<&Value>) -> Option<u64> {
    let is_kiro = plan
        .provider_name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("kiro"));
    if is_kiro {
        if let Some(input_tokens) =
            context_u64(report_context, "input_tokens").filter(|tokens| *tokens > 0)
        {
            return Some(input_tokens.min(CANCELLED_INPUT_ESTIMATE_MAX_TOKENS));
        }
    }
    let body = plan
        .body
        .json_body
        .as_ref()
        .or_else(|| report_context.and_then(|value| value.get("original_request_body")))?;
    estimate_request_input_tokens(body)
}

fn usage_format_supports_cache_floor(plan: &ExecutionPlan, report_context: Option<&Value>) -> bool {
    let format = cancellation_format(plan, report_context);
    format.starts_with("openai:") || format.starts_with("gemini:")
}

fn cancellation_floor_supports_endpoint(
    plan: &ExecutionPlan,
    report_context: Option<&Value>,
) -> bool {
    let formats = [
        report_context
            .and_then(Value::as_object)
            .and_then(|context| context.get("provider_api_format"))
            .and_then(Value::as_str),
        report_context
            .and_then(Value::as_object)
            .and_then(|context| context.get("client_api_format"))
            .and_then(Value::as_str),
        Some(plan.provider_api_format.as_str()),
        Some(plan.client_api_format.as_str()),
    ];
    !formats.into_iter().flatten().any(|format| {
        matches!(
            format
                .split_once(':')
                .map(|(_, endpoint)| endpoint.trim().to_ascii_lowercase())
                .as_deref(),
            Some("image" | "video")
        )
    })
}

fn cancellation_format(plan: &ExecutionPlan, report_context: Option<&Value>) -> String {
    report_context
        .and_then(Value::as_object)
        .and_then(|context| {
            context
                .get("provider_api_format")
                .or_else(|| context.get("client_api_format"))
        })
        .and_then(Value::as_str)
        .unwrap_or(plan.provider_api_format.as_str())
        .trim()
        .to_ascii_lowercase()
}

fn estimate_request_input_tokens(value: &Value) -> Option<u64> {
    let is_continuation = value
        .get("previous_response_id")
        .is_some_and(|previous| !previous.is_null());
    let fields: &[&str] = if is_continuation {
        &["input"]
    } else {
        &[
            "instructions",
            "input",
            "messages",
            "prompt",
            "contents",
            "system",
            "tools",
        ]
    };
    let mut budget = EstimateBudget::default();
    let preferred_total = value.as_object().map(|object| {
        fields
            .iter()
            .copied()
            .filter_map(|field| object.get(field))
            .map(|value| estimate_json_tokens(value, &mut budget, 0))
            .fold(0_u64, u64::saturating_add)
    });
    let preferred_total = preferred_total.unwrap_or_default();
    let estimate = if preferred_total > 0 {
        preferred_total
    } else {
        budget = EstimateBudget::default();
        estimate_json_tokens(value, &mut budget, 0)
    };
    Some(estimate.min(CANCELLED_INPUT_ESTIMATE_MAX_TOKENS)).filter(|value| *value > 0)
}

#[derive(Debug, Clone, Copy)]
struct EstimateBudget {
    bytes: u64,
    nodes: u64,
}

impl Default for EstimateBudget {
    fn default() -> Self {
        Self {
            bytes: CANCELLED_INPUT_ESTIMATE_MAX_SCAN_BYTES,
            nodes: CANCELLED_INPUT_ESTIMATE_MAX_NODES,
        }
    }
}

fn estimate_json_tokens(value: &Value, budget: &mut EstimateBudget, depth: usize) -> u64 {
    if depth > CANCELLED_INPUT_ESTIMATE_MAX_DEPTH || budget.nodes == 0 {
        return 0;
    }
    budget.nodes -= 1;
    match value {
        Value::String(text) => estimate_text_tokens_bounded(text, budget),
        Value::Array(items) => {
            let mut total = 0_u64;
            for item in items {
                if budget.nodes == 0 || budget.bytes == 0 {
                    break;
                }
                total = total.saturating_add(estimate_json_tokens(item, budget, depth + 1));
            }
            total
        }
        Value::Object(object) => {
            let mut total = 0_u64;
            let has_inline_binary_data = object_declares_inline_binary_data(object);
            for (key, value) in object {
                if budget.nodes == 0 || budget.bytes == 0 {
                    break;
                }
                total = total.saturating_add(estimate_text_tokens_bounded(key, budget));
                if (has_inline_binary_data && is_inline_binary_value_key(key))
                    || is_explicit_base64_value_key(key)
                {
                    continue;
                }
                total = total.saturating_add(estimate_json_tokens(value, budget, depth + 1));
            }
            total
        }
        Value::Null => 0,
        _ => 1,
    }
}

fn object_declares_inline_binary_data(object: &serde_json::Map<String, Value>) -> bool {
    let media_type = object
        .get("media_type")
        .or_else(|| object.get("mime_type"))
        .or_else(|| object.get("mimeType"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if media_type.starts_with("image/")
        || media_type.starts_with("audio/")
        || media_type.starts_with("video/")
        || media_type == "application/octet-stream"
    {
        return true;
    }

    object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            matches!(
                kind.trim().to_ascii_lowercase().as_str(),
                "base64" | "image" | "input_image" | "audio" | "input_audio" | "video"
            )
        })
}

fn is_inline_binary_value_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("data")
        || key.eq_ignore_ascii_case("bytes")
        || key.eq_ignore_ascii_case("file_data")
}

fn is_explicit_base64_value_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "b64_json" || key == "body_bytes_b64" || key.ends_with("_base64")
}

fn estimate_text_tokens_bounded(text: &str, budget: &mut EstimateBudget) -> u64 {
    if text.is_empty() || budget.bytes == 0 || is_inline_binary_data(text) {
        0
    } else {
        let bytes = (text.len() as u64).min(budget.bytes);
        budget.bytes = budget.bytes.saturating_sub(bytes);
        bytes.div_ceil(4).max(1)
    }
}

fn is_inline_binary_data(text: &str) -> bool {
    let prefix = &text.as_bytes()[..text.len().min(128)];
    prefix.starts_with(b"data:")
        && prefix
            .windows(b";base64,".len())
            .any(|window| window == b";base64,")
}

fn normalize_estimated_total(usage: &mut StandardizedUsage) {
    let estimated_total = usage
        .input_tokens
        .max(0)
        .saturating_add(usage.output_tokens.max(0))
        .saturating_add(
            usage
                .reasoning_output_tokens
                .max(usage.reasoning_tokens)
                .max(0),
        );
    let explicit_total = usage
        .dimensions
        .get("total_tokens")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|tokens| i64::try_from(tokens).ok()))
        })
        .unwrap_or_default();
    if estimated_total > explicit_total {
        usage
            .dimensions
            .insert("total_tokens".to_string(), json!(estimated_total));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn plan(body: Value, format: &str) -> ExecutionPlan {
        ExecutionPlan {
            request_id: "cancel-floor-test".into(),
            candidate_id: None,
            provider_name: Some("provider".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "POST".into(),
            url: "https://example.test/v1/responses".into(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(body),
            stream: true,
            client_api_format: format.into(),
            provider_api_format: format.into(),
            model_name: Some("model".into()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        }
    }

    #[test]
    fn openai_floor_is_non_zero_and_cache_priced() {
        let plan = plan(json!({"input": "hello world"}), "openai:responses");
        let usage = cancelled_usage_billing_floor(&plan, None, None).expect("usage floor");
        assert!(usage.input_tokens > 0);
        assert_eq!(usage.cache_read_tokens, usage.input_tokens);
    }

    #[test]
    fn cancellation_marker_matching_is_case_insensitive() {
        assert!(terminal_usage_is_cancelled(499, "success"));
        assert!(terminal_usage_is_cancelled(200, "OpenAI_Stream_Cancelled"));
        assert!(!terminal_usage_is_cancelled(200, "openai_stream_success"));
    }

    #[test]
    fn continuation_combines_cached_context_with_new_input() {
        let plan = plan(
            json!({"previous_response_id": "resp-1", "input": "new"}),
            "openai:responses",
        );
        let mut existing = StandardizedUsage::new();
        existing.input_tokens = 100;
        existing.cache_read_tokens = 100;
        existing
            .dimensions
            .insert("usage_source".into(), json!(CANCELLED_CONTEXT_FLOOR_SOURCE));
        let usage =
            cancelled_usage_billing_floor(&plan, None, Some(existing)).expect("usage floor");
        assert!(usage.input_tokens > 100);
        assert_eq!(usage.cache_read_tokens, 100);
    }

    #[test]
    fn authoritative_input_usage_is_unchanged() {
        let plan = plan(json!({"input": "hello"}), "openai:responses");
        let mut existing = StandardizedUsage::new();
        existing.input_tokens = 42;
        existing.output_tokens = 7;
        let usage =
            cancelled_usage_billing_floor(&plan, None, Some(existing.clone())).expect("usage");
        assert_eq!(usage, existing);
    }

    #[test]
    fn inline_base64_is_not_counted_as_prompt_text() {
        let plan = plan(
            json!({"input": [{"image_url": "data:image/png;base64,AAAA"}]}),
            "openai:responses",
        );
        let usage = cancelled_usage_billing_floor(&plan, None, None).expect("usage floor");
        assert!(usage.input_tokens < 100);
    }

    #[test]
    fn structured_raw_base64_media_is_not_counted_as_prompt_text() {
        let plan = plan(
            json!({
                "messages": [{
                    "content": [{
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "A".repeat(100_000)
                        }
                    }]
                }]
            }),
            "claude:messages",
        );
        let usage = cancelled_usage_billing_floor(&plan, None, None).expect("usage floor");
        assert!(usage.input_tokens < 100);
    }

    #[test]
    fn report_context_input_estimate_takes_precedence() {
        let mut plan = plan(json!({"input": "plan body"}), "claude:messages");
        plan.body = aether_contracts::RequestBody {
            json_body: None,
            body_bytes_b64: None,
            body_ref: Some("usage://request/body".into()),
        };
        let context = json!({"original_request_body": {"input": "context body with more text"}});
        let usage =
            cancelled_usage_billing_floor(&plan, Some(&context), None).expect("usage floor");
        assert!(usage.input_tokens > 1);
        assert_eq!(usage.cache_read_tokens, 0);
    }

    #[test]
    fn existing_usage_is_not_discarded_when_no_estimate_is_available() {
        let mut plan = plan(json!({"input": "small"}), "openai:responses");
        plan.body = aether_contracts::RequestBody {
            json_body: None,
            body_bytes_b64: None,
            body_ref: Some("usage://request/body".into()),
        };
        let mut existing = StandardizedUsage::new();
        existing
            .dimensions
            .insert("provider_marker".into(), json!("retained"));

        let usage = cancelled_usage_billing_floor(&plan, None, Some(existing.clone()))
            .expect("existing usage");
        assert_eq!(usage, existing);
    }

    #[test]
    fn video_cancellation_does_not_infer_prompt_token_billing() {
        let plan = plan(json!({"prompt": "make a video"}), "openai:video");
        assert!(cancelled_usage_billing_floor(&plan, None, None).is_none());
    }

    #[test]
    fn kiro_context_cache_usage_is_converted_to_billed_input() {
        let mut plan = plan(json!({"conversationState": {}}), "claude:messages");
        plan.provider_name = Some("Kiro".into());
        let context = json!({
            "input_tokens": 100,
            "cache_creation_input_tokens": 10,
            "cache_read_input_tokens": 60,
            "original_request_body": {"messages": [{"content": "prompt"}]}
        });
        let usage =
            cancelled_usage_billing_floor(&plan, Some(&context), None).expect("usage floor");
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.cache_read_tokens, 60);
    }

    #[test]
    fn kiro_cache_usage_is_clamped_to_total_input() {
        let mut plan = plan(json!({"conversationState": {}}), "claude:messages");
        plan.provider_name = Some("Kiro".into());
        let context = json!({
            "input_tokens": 100,
            "cache_creation_input_tokens": 80,
            "cache_read_input_tokens": 80
        });
        let usage =
            cancelled_usage_billing_floor(&plan, Some(&context), None).expect("usage floor");
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 20);
    }

    #[test]
    fn image_client_conversion_does_not_infer_prompt_token_billing() {
        let mut plan = plan(json!({"prompt": "draw an image"}), "openai:responses");
        plan.client_api_format = "openai:image".into();
        assert!(cancelled_usage_billing_floor(&plan, None, None).is_none());
    }

    #[test]
    fn estimate_walk_is_bounded_for_large_and_deep_json() {
        let mut value = Value::Array((0..10_000).map(|_| Value::Null).collect());
        for _ in 0..CANCELLED_INPUT_ESTIMATE_MAX_DEPTH + 10 {
            value = Value::Array(vec![value]);
        }
        let estimate = estimate_request_input_tokens(&value).unwrap_or_default();
        assert!(estimate <= CANCELLED_INPUT_ESTIMATE_MAX_TOKENS);
    }
}
