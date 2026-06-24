use aether_ai_formats::api::{
    sanitize_request_path, sanitize_request_path_and_query, sanitize_request_query_string,
};
use aether_ai_formats::UPSTREAM_IS_STREAM_KEY;
use aether_contracts::ExecutionPlan;
use aether_data_contracts::repository::usage::{
    extract_provider_reasoning_effort_from_body, extract_provider_service_tier_from_body,
    PROVIDER_REASONING_EFFORT_METADATA_KEY, PROVIDER_SERVICE_TIER_METADATA_KEY,
};
use serde_json::{json, Map, Value};

const MAX_USAGE_REQUEST_METADATA_DEPTH: usize = 32;
const MAX_USAGE_REQUEST_METADATA_NODES: usize = 4_000;
const MAX_USAGE_REQUEST_METADATA_BYTES: usize = 16 * 1024;
const MAX_USAGE_REQUEST_METADATA_STRING_BYTES: usize = 1_024;
const MAX_USAGE_PROMPT_CAPTURE_ROLE_COUNTS: usize = 32;
const MAX_USAGE_PROMPT_CAPTURE_ITEMS: usize = 256;
const MAX_USAGE_PROMPT_CAPTURE_PREVIEW_BYTES: usize = 8 * 1024;
const USAGE_PROMPT_CAPTURE_PREVIEW_BYTE_BUDGETS: [usize; 8] =
    [4 * 1024, 2 * 1024, 1024, 512, 256, 128, 64, 0];

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptCaptureText {
    value: String,
    truncated: bool,
}

pub(crate) fn build_usage_request_metadata_seed(
    _plan: &ExecutionPlan,
    context: Option<&Map<String, Value>>,
) -> Option<Value> {
    let mut metadata = Map::new();
    if let Some(context) = context {
        copy_allowed_metadata_fields(context, &mut metadata);
    }
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

pub(crate) fn merge_usage_request_metadata(
    base: Option<Value>,
    override_value: Option<Value>,
) -> Option<Value> {
    let mut metadata = Map::new();
    if let Some(Value::Object(base)) = base.as_ref() {
        copy_allowed_metadata_fields(base, &mut metadata);
    }
    if let Some(Value::Object(override_object)) = override_value.as_ref() {
        copy_allowed_metadata_fields(override_object, &mut metadata);
    }
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

pub(crate) fn merge_usage_request_metadata_owned(
    base: Option<Value>,
    override_value: Option<Value>,
) -> Option<Value> {
    let mut metadata = match base {
        Some(Value::Object(base)) => base,
        _ => Map::new(),
    };
    if let Some(Value::Object(override_object)) = override_value {
        move_allowed_metadata_fields(override_object, &mut metadata);
    }
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

pub(crate) fn sanitize_usage_request_metadata(value: Option<Value>) -> Option<Value> {
    let Value::Object(object) = value? else {
        return None;
    };

    let mut filtered = Map::new();
    move_allowed_metadata_fields(object, &mut filtered);

    (!filtered.is_empty()).then_some(Value::Object(filtered))
}

pub(crate) fn sanitize_usage_request_metadata_ref(value: Option<&Value>) -> Option<Value> {
    let object = value.and_then(Value::as_object)?;

    let mut filtered = Map::new();
    copy_allowed_metadata_fields(object, &mut filtered);

    (!filtered.is_empty()).then_some(Value::Object(filtered))
}

pub(crate) fn attach_provider_request_body_metadata(
    metadata: Option<Value>,
    provider_request_body: Option<&Value>,
) -> Option<Value> {
    let provider_body_is_object = provider_request_body.and_then(Value::as_object).is_some();
    let reasoning_effort = extract_provider_reasoning_effort_from_body(provider_request_body);
    let service_tier = extract_provider_service_tier_from_body(provider_request_body);
    if !provider_body_is_object && reasoning_effort.is_none() && service_tier.is_none() {
        return metadata;
    }
    let mut object = match metadata {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    };
    if provider_body_is_object {
        object.remove(PROVIDER_REASONING_EFFORT_METADATA_KEY);
        object.remove(PROVIDER_SERVICE_TIER_METADATA_KEY);
    }
    if let Some(reasoning_effort) = reasoning_effort {
        object.insert(
            PROVIDER_REASONING_EFFORT_METADATA_KEY.to_string(),
            Value::String(reasoning_effort),
        );
    }
    if let Some(service_tier) = service_tier {
        object.insert(
            PROVIDER_SERVICE_TIER_METADATA_KEY.to_string(),
            Value::String(service_tier),
        );
    }
    (!object.is_empty()).then_some(Value::Object(object))
}

pub fn attach_cafecode_identity_metadata(
    metadata: Option<Value>,
    headers: Option<&Value>,
) -> Option<Value> {
    let Some(identity) = extract_cafecode_identity_from_headers(headers) else {
        return metadata;
    };
    let mut object = match metadata {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    };
    if let Some(uid) = identity.uid {
        object.insert("cafecode_uid".to_string(), Value::String(uid));
    }
    if let Some(uname) = identity.uname {
        object.insert("cafecode_uname".to_string(), Value::String(uname));
    }
    (!object.is_empty()).then_some(Value::Object(object))
}

struct CafecodeIdentity {
    uid: Option<String>,
    uname: Option<String>,
}

fn extract_cafecode_identity_from_headers(headers: Option<&Value>) -> Option<CafecodeIdentity> {
    let object = headers.and_then(Value::as_object)?;
    let uid = header_string(object, "cafecode-uid");
    let uname = header_string(object, "cafecode-uname");
    (uid.is_some() || uname.is_some()).then_some(CafecodeIdentity { uid, uname })
}

fn header_string(headers: &Map<String, Value>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(truncate_usage_request_metadata_string)
}

fn copy_allowed_metadata_fields(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    copy_non_empty_string(source, target, "trace_id");
    copy_non_empty_string(source, target, "client_ip");
    copy_non_empty_string(source, target, "user_agent");
    copy_non_empty_string(source, target, "cafecode_uid");
    copy_non_empty_string(source, target, "cafecode_uname");
    copy_non_empty_string(source, target, "client_family");
    copy_bool(source, target, "client_requested_stream");
    copy_bool(source, target, UPSTREAM_IS_STREAM_KEY);
    copy_bool(source, target, "is_risk_control");
    copy_bool(source, target, "is_ping");
    copy_non_empty_string(source, target, "ping_kind");
    copy_non_null_value(source, target, "client_session_affinity");
    copy_bool(source, target, "api_key_is_standalone");
    copy_non_empty_string(source, target, "request_path");
    copy_non_empty_string(source, target, "request_query_string");
    copy_non_empty_string(source, target, "request_path_and_query");
    copy_non_empty_string(source, target, PROVIDER_REASONING_EFFORT_METADATA_KEY);
    copy_non_empty_string(source, target, PROVIDER_SERVICE_TIER_METADATA_KEY);
    copy_number(source, target, "provider_request_body_base64_bytes");
    copy_number(source, target, "provider_response_body_base64_bytes");
    copy_number(source, target, "client_response_body_base64_bytes");
    copy_number(source, target, "client_response_status_code");
    copy_prompt_capture(source, target);
    copy_non_null_value(source, target, "billing_snapshot");
    copy_non_empty_string(source, target, "billing_snapshot_schema_version");
    copy_non_empty_string(source, target, "billing_snapshot_status");
    copy_non_null_value(source, target, "settlement_snapshot");
    copy_non_empty_string(source, target, "settlement_snapshot_schema_version");
    copy_non_null_value(source, target, "billing_dimensions");
    copy_non_empty_string(source, target, "model_id");
    copy_non_empty_string(source, target, "global_model_id");
    copy_non_empty_string(source, target, "global_model_name");
    copy_non_null_value(source, target, "dimensions");
    copy_non_null_value(source, target, "billing_rule_snapshot");
    copy_non_null_value(source, target, "scheduling_audit");
    copy_non_null_value(source, target, "tls_fingerprint");
    copy_number(source, target, "rate_multiplier");
    copy_bool(source, target, "is_free_tier");
    copy_number(source, target, "input_price_per_1m");
    copy_number(source, target, "output_price_per_1m");
    copy_number(source, target, "cache_creation_price_per_1m");
    copy_number(source, target, "cache_read_price_per_1m");
    copy_number(source, target, "price_per_request");
    copy_non_null_value(source, target, "proxy");
    sanitize_request_path_metadata_fields(target);
}

fn move_allowed_metadata_fields(mut source: Map<String, Value>, target: &mut Map<String, Value>) {
    remove_non_empty_string(&mut source, target, "trace_id");
    remove_non_empty_string(&mut source, target, "client_ip");
    remove_non_empty_string(&mut source, target, "user_agent");
    remove_non_empty_string(&mut source, target, "cafecode_uid");
    remove_non_empty_string(&mut source, target, "cafecode_uname");
    remove_non_empty_string(&mut source, target, "client_family");
    remove_bool(&mut source, target, "client_requested_stream");
    remove_bool(&mut source, target, UPSTREAM_IS_STREAM_KEY);
    remove_bool(&mut source, target, "is_risk_control");
    remove_bool(&mut source, target, "is_ping");
    remove_non_empty_string(&mut source, target, "ping_kind");
    remove_non_null_value(&mut source, target, "client_session_affinity");
    remove_bool(&mut source, target, "api_key_is_standalone");
    remove_non_empty_string(&mut source, target, "request_path");
    remove_non_empty_string(&mut source, target, "request_query_string");
    remove_non_empty_string(&mut source, target, "request_path_and_query");
    remove_non_empty_string(&mut source, target, PROVIDER_REASONING_EFFORT_METADATA_KEY);
    remove_non_empty_string(&mut source, target, PROVIDER_SERVICE_TIER_METADATA_KEY);
    remove_number(&mut source, target, "provider_request_body_base64_bytes");
    remove_number(&mut source, target, "provider_response_body_base64_bytes");
    remove_number(&mut source, target, "client_response_body_base64_bytes");
    remove_number(&mut source, target, "client_response_status_code");
    remove_prompt_capture(&mut source, target);
    remove_non_null_value(&mut source, target, "billing_snapshot");
    remove_non_empty_string(&mut source, target, "billing_snapshot_schema_version");
    remove_non_empty_string(&mut source, target, "billing_snapshot_status");
    remove_non_null_value(&mut source, target, "settlement_snapshot");
    remove_non_empty_string(&mut source, target, "settlement_snapshot_schema_version");
    remove_non_null_value(&mut source, target, "billing_dimensions");
    remove_non_empty_string(&mut source, target, "model_id");
    remove_non_empty_string(&mut source, target, "global_model_id");
    remove_non_empty_string(&mut source, target, "global_model_name");
    remove_non_null_value(&mut source, target, "dimensions");
    remove_non_null_value(&mut source, target, "billing_rule_snapshot");
    remove_non_null_value(&mut source, target, "scheduling_audit");
    remove_non_null_value(&mut source, target, "tls_fingerprint");
    remove_number(&mut source, target, "rate_multiplier");
    remove_bool(&mut source, target, "is_free_tier");
    remove_number(&mut source, target, "input_price_per_1m");
    remove_number(&mut source, target, "output_price_per_1m");
    remove_number(&mut source, target, "cache_creation_price_per_1m");
    remove_number(&mut source, target, "cache_read_price_per_1m");
    remove_number(&mut source, target, "price_per_request");
    remove_non_null_value(&mut source, target, "proxy");
    sanitize_request_path_metadata_fields(target);
}

fn sanitize_request_path_metadata_fields(target: &mut Map<String, Value>) {
    let path = target
        .get("request_path")
        .and_then(Value::as_str)
        .and_then(sanitize_request_path);
    let query = target
        .get("request_query_string")
        .and_then(Value::as_str)
        .and_then(sanitize_request_query_string);
    let path_and_query = target
        .get("request_path_and_query")
        .and_then(Value::as_str)
        .and_then(|value| sanitize_request_path_and_query(value, None))
        .or_else(|| {
            path.as_deref()
                .and_then(|path| sanitize_request_path_and_query(path, query.as_deref()))
        });

    apply_optional_string_field(target, "request_path", path.as_deref());
    apply_optional_string_field(target, "request_query_string", query.as_deref());
    apply_optional_string_field(target, "request_path_and_query", path_and_query.as_deref());
}

fn apply_optional_string_field(target: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        target.insert(key.to_string(), Value::String(value.to_string()));
    } else {
        target.remove(key);
    }
}

fn copy_non_empty_string(source: &Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let Some(value) = source
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    target.insert(
        key.to_string(),
        Value::String(truncate_usage_request_metadata_string(value)),
    );
}

fn remove_non_empty_string(
    source: &mut Map<String, Value>,
    target: &mut Map<String, Value>,
    key: &str,
) {
    let Some(Value::String(value)) = source.remove(key) else {
        return;
    };
    let Some(value) = trim_and_truncate_usage_request_metadata_string_owned(value) else {
        return;
    };
    target.insert(key.to_string(), Value::String(value));
}

fn copy_number(source: &Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let Some(value) = source.get(key).filter(|value| value.is_number()) else {
        return;
    };
    target.insert(key.to_string(), value.clone());
}

fn remove_number(source: &mut Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let Some(value) = source.remove(key).filter(|value| value.is_number()) else {
        return;
    };
    target.insert(key.to_string(), value);
}

fn copy_bool(source: &Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let Some(value) = source.get(key).filter(|value| value.is_boolean()) else {
        return;
    };
    target.insert(key.to_string(), value.clone());
}

fn remove_bool(source: &mut Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let Some(value) = source.remove(key).filter(|value| value.is_boolean()) else {
        return;
    };
    target.insert(key.to_string(), value);
}

fn copy_non_null_value(source: &Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let Some(value) = source.get(key).filter(|value| !value.is_null()) else {
        return;
    };
    target.insert(
        key.to_string(),
        sanitize_usage_request_metadata_value(value),
    );
}

fn copy_prompt_capture(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    let Some(value) = source
        .get("prompt_capture")
        .filter(|value| !value.is_null())
    else {
        return;
    };
    let Some(prompt_capture) = sanitize_prompt_capture_value(value) else {
        return;
    };
    target.insert("prompt_capture".to_string(), prompt_capture);
}

fn remove_prompt_capture(source: &mut Map<String, Value>, target: &mut Map<String, Value>) {
    let Some(value) = source
        .remove("prompt_capture")
        .filter(|value| !value.is_null())
    else {
        return;
    };
    let Some(prompt_capture) = sanitize_prompt_capture_value(&value) else {
        return;
    };
    target.insert("prompt_capture".to_string(), prompt_capture);
}

fn sanitize_prompt_capture_value(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mut capture = Map::new();

    copy_prompt_capture_number(object, &mut capture, "version");
    copy_prompt_capture_number(object, &mut capture, "item_count");
    copy_prompt_capture_role_counts(object, &mut capture);
    copy_prompt_capture_items(object, &mut capture);

    if capture.is_empty() {
        return None;
    }

    Some(compact_prompt_capture_to_metadata_limits(Value::Object(
        capture,
    )))
}

fn copy_prompt_capture_number(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    key: &str,
) {
    let Some(value) = source.get(key).filter(|value| value.is_number()) else {
        return;
    };
    target.insert(key.to_string(), value.clone());
}

fn copy_prompt_capture_role_counts(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    let Some(role_counts) = source.get("role_counts").and_then(Value::as_object) else {
        return;
    };
    let counts = role_counts
        .iter()
        .take(MAX_USAGE_PROMPT_CAPTURE_ROLE_COUNTS)
        .filter_map(|(role, count)| {
            let role = trim_and_truncate_prompt_capture_text(role, 64)?;
            let count = prompt_capture_count_value(count)?;
            Some((role, serde_json::json!(count)))
        })
        .collect::<Map<_, _>>();
    if !counts.is_empty() {
        target.insert("role_counts".to_string(), Value::Object(counts));
    }
}

fn copy_prompt_capture_items(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    let Some(items) = source.get("items").and_then(Value::as_array) else {
        return;
    };
    let mut items = items
        .iter()
        .rev()
        .filter_map(sanitize_prompt_capture_item)
        .take(MAX_USAGE_PROMPT_CAPTURE_ITEMS)
        .collect::<Vec<_>>();
    items.reverse();
    if !items.is_empty() {
        target.insert("items".to_string(), Value::Array(items));
    }
}

fn sanitize_prompt_capture_item(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mut item = Map::new();

    if let Some(source) = prompt_capture_string(object, "source", 128) {
        item.insert("source".to_string(), Value::String(source));
    }
    if let Some(role) = prompt_capture_string(object, "role", 64) {
        item.insert("role".to_string(), Value::String(role));
    }
    if let Some(sha256) = prompt_capture_string(object, "sha256", 128) {
        item.insert("sha256".to_string(), Value::String(sha256));
    }
    if let Some(index) = object.get("index").and_then(prompt_capture_count_value) {
        item.insert("index".to_string(), serde_json::json!(index));
    }
    if let Some(chars) = object.get("chars").and_then(prompt_capture_count_value) {
        item.insert("chars".to_string(), serde_json::json!(chars));
    }
    let preview_truncated = if let Some(preview) =
        prompt_capture_text(object, "preview", MAX_USAGE_PROMPT_CAPTURE_PREVIEW_BYTES)
    {
        let truncated = preview.truncated;
        item.insert("preview".to_string(), Value::String(preview.value));
        truncated
    } else {
        false
    };
    if let Some(truncated) = object
        .get("truncated")
        .and_then(Value::as_bool)
        .map(|truncated| truncated || preview_truncated)
        .or_else(|| preview_truncated.then_some(true))
    {
        item.insert("truncated".to_string(), Value::Bool(truncated));
    }

    (!item.is_empty()).then_some(Value::Object(item))
}

fn compact_prompt_capture_to_metadata_limits(mut capture: Value) -> Value {
    if usage_request_metadata_within_limits(&capture) {
        return capture;
    }

    for max_preview_bytes in USAGE_PROMPT_CAPTURE_PREVIEW_BYTE_BUDGETS {
        limit_prompt_capture_previews(&mut capture, max_preview_bytes);
        if usage_request_metadata_within_limits(&capture) {
            return capture;
        }
    }

    truncate_prompt_capture_items_to_fit(&mut capture);
    capture
}

fn limit_prompt_capture_previews(capture: &mut Value, max_preview_bytes: usize) {
    let Some(items) = capture.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let Some(item_object) = item.as_object_mut() else {
            continue;
        };
        let Some(preview) = item_object.get("preview").and_then(Value::as_str) else {
            continue;
        };
        let Some(preview) = trim_and_truncate_prompt_capture_preview(preview, max_preview_bytes)
        else {
            item_object.remove("preview");
            continue;
        };
        let truncated = preview.truncated;
        item_object.insert("preview".to_string(), Value::String(preview.value));
        if truncated {
            item_object.insert("truncated".to_string(), Value::Bool(true));
        }
    }
}

fn truncate_prompt_capture_items_to_fit(capture: &mut Value) {
    if usage_request_metadata_within_limits(capture) {
        return;
    }
    let Some(original_len) = capture.get("items").and_then(Value::as_array).map(Vec::len) else {
        return;
    };

    let mut low = 0usize;
    let mut high = original_len;
    while low < high {
        let mid = (low + high + 1) / 2;
        let mut candidate = capture.clone();
        retain_prompt_capture_items(&mut candidate, mid);
        if usage_request_metadata_within_limits(&candidate) {
            low = mid;
        } else {
            high = mid.saturating_sub(1);
        }
    }
    retain_prompt_capture_items(capture, low);
}

fn retain_prompt_capture_items(capture: &mut Value, max_items: usize) {
    let Some(items) = capture.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    if items.len() <= max_items {
        return;
    }
    if max_items == 0 {
        items.clear();
        return;
    }
    let start = items.len().saturating_sub(max_items);
    *items = items.split_off(start);
}

fn prompt_capture_string(
    source: &Map<String, Value>,
    key: &str,
    max_bytes: usize,
) -> Option<String> {
    prompt_capture_text(source, key, max_bytes).map(|text| text.value)
}

fn prompt_capture_text(
    source: &Map<String, Value>,
    key: &str,
    max_bytes: usize,
) -> Option<PromptCaptureText> {
    source
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| trim_and_truncate_prompt_capture_preview(value, max_bytes))
}

fn trim_and_truncate_prompt_capture_text(value: &str, max_bytes: usize) -> Option<String> {
    trim_and_truncate_prompt_capture_preview(value, max_bytes)
        .map(|preview| preview.value)
        .filter(|value| !value.is_empty())
}

fn trim_and_truncate_prompt_capture_preview(
    value: &str,
    max_bytes: usize,
) -> Option<PromptCaptureText> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= max_bytes {
        return Some(PromptCaptureText {
            value: trimmed.to_string(),
            truncated: false,
        });
    }
    Some(PromptCaptureText {
        value: truncate_string_to_bytes(trimmed, max_bytes),
        truncated: true,
    })
}

fn prompt_capture_count_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
}

fn remove_non_null_value(
    source: &mut Map<String, Value>,
    target: &mut Map<String, Value>,
    key: &str,
) {
    let Some(value) = source.remove(key).filter(|value| !value.is_null()) else {
        return;
    };
    target.insert(
        key.to_string(),
        sanitize_usage_request_metadata_value_owned(value),
    );
}

fn sanitize_usage_request_metadata_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_usage_request_metadata_string(text)),
        _ if usage_request_metadata_within_limits(value) => value.clone(),
        _ => truncated_usage_request_metadata_value(value),
    }
}

fn sanitize_usage_request_metadata_value_owned(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_usage_request_metadata_string_owned(text)),
        _ if usage_request_metadata_within_limits(&value) => value,
        _ => truncated_usage_request_metadata_value(&value),
    }
}

fn truncate_usage_request_metadata_string(value: &str) -> String {
    const TRUNCATED_SUFFIX: &str = "...[truncated]";

    if value.len() <= MAX_USAGE_REQUEST_METADATA_STRING_BYTES {
        return value.to_string();
    }

    let target_bytes =
        MAX_USAGE_REQUEST_METADATA_STRING_BYTES.saturating_sub(TRUNCATED_SUFFIX.len());
    let prefix = truncate_string_to_bytes(value, target_bytes);
    if prefix.is_empty() {
        return TRUNCATED_SUFFIX.to_string();
    }

    format!("{prefix}{TRUNCATED_SUFFIX}")
}

fn truncate_string_to_bytes(value: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0usize;
    for (idx, ch) in value.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }

    if end == 0 {
        return String::new();
    }

    value[..end].to_string()
}

fn trim_and_truncate_usage_request_metadata_string_owned(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() == value.len() {
        return Some(truncate_usage_request_metadata_string_owned(value));
    }
    Some(truncate_usage_request_metadata_string(trimmed))
}

fn truncate_usage_request_metadata_string_owned(value: String) -> String {
    if value.len() <= MAX_USAGE_REQUEST_METADATA_STRING_BYTES {
        return value;
    }
    truncate_usage_request_metadata_string(value.as_str())
}

fn truncated_usage_request_metadata_value(value: &Value) -> Value {
    json!({
        "truncated": true,
        "reason": "usage_request_metadata_limits_exceeded",
        "max_depth": MAX_USAGE_REQUEST_METADATA_DEPTH,
        "max_nodes": MAX_USAGE_REQUEST_METADATA_NODES,
        "max_bytes": MAX_USAGE_REQUEST_METADATA_BYTES,
        "value_kind": usage_request_metadata_value_kind(value),
    })
}

fn usage_request_metadata_within_limits(value: &Value) -> bool {
    let mut nodes = 0usize;
    let mut estimated_bytes = 0usize;
    let mut stack = vec![(value, 1usize)];

    while let Some((current, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        estimated_bytes =
            estimated_bytes.saturating_add(usage_request_metadata_value_size_hint(current));
        if depth > MAX_USAGE_REQUEST_METADATA_DEPTH
            || nodes > MAX_USAGE_REQUEST_METADATA_NODES
            || estimated_bytes > MAX_USAGE_REQUEST_METADATA_BYTES
        {
            return false;
        }
        match current {
            Value::Array(items) => {
                estimated_bytes = estimated_bytes.saturating_add(items.len().saturating_mul(2));
                for item in items.iter().rev() {
                    stack.push((item, depth + 1));
                }
            }
            Value::Object(object) => {
                estimated_bytes = estimated_bytes
                    .saturating_add(object.len().saturating_mul(3))
                    .saturating_add(
                        object
                            .keys()
                            .map(|key| key.len().saturating_add(2))
                            .sum::<usize>(),
                    );
                for item in object.values() {
                    stack.push((item, depth + 1));
                }
            }
            _ => {}
        }
    }

    true
}

fn usage_request_metadata_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn usage_request_metadata_value_size_hint(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(false) => 5,
        Value::Bool(true) => 4,
        Value::Number(number) => number.to_string().len(),
        Value::String(text) => text.len().saturating_add(2),
        Value::Array(_) | Value::Object(_) => 2,
    }
}

#[cfg(test)]
mod tests {
    use aether_contracts::{ExecutionPlan, RequestBody};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    use super::{
        attach_provider_request_body_metadata, build_usage_request_metadata_seed,
        merge_usage_request_metadata, merge_usage_request_metadata_owned,
        sanitize_usage_request_metadata, sanitize_usage_request_metadata_ref,
        usage_request_metadata_within_limits, MAX_USAGE_PROMPT_CAPTURE_PREVIEW_BYTES,
        MAX_USAGE_REQUEST_METADATA_BYTES, MAX_USAGE_REQUEST_METADATA_DEPTH,
        MAX_USAGE_REQUEST_METADATA_NODES,
    };

    fn sample_plan() -> ExecutionPlan {
        ExecutionPlan {
            request_id: "req-1".to_string(),
            candidate_id: Some("cand-1".to_string()),
            provider_name: Some("OpenAI".to_string()),
            provider_id: "provider-1".to_string(),
            endpoint_id: "endpoint-1".to_string(),
            key_id: "key-1".to_string(),
            method: "POST".to_string(),
            url: "https://example.com/v1/chat/completions".to_string(),
            headers: BTreeMap::new(),
            content_type: None,
            content_encoding: None,
            body: RequestBody::from_json(json!({"model": "gpt-5"})),
            stream: false,
            client_api_format: "openai:chat".to_string(),
            provider_api_format: "openai:chat".to_string(),
            model_name: Some("gpt-5".to_string()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        }
    }

    #[test]
    fn sanitizes_request_metadata_to_allowlist() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "request_id": "req-1",
            "provider_id": "provider-1",
            "provider_name": "OpenAI",
            "model": "gpt-5",
            "candidate_index": 2,
            "trace_id": "trace-1",
            "client_ip": "203.0.113.8",
            "user_agent": "Claude-Code/1.0",
            "client_requested_stream": false,
            "upstream_is_stream": true,
            "is_risk_control": true,
            "api_key_is_standalone": true,
            "provider_request_body_base64_bytes": 512,
            "provider_response_body_base64_bytes": 1024,
            "client_response_body_base64_bytes": 2048,
            "billing_snapshot": {"status": "complete"},
            "billing_snapshot_schema_version": "2.0",
            "billing_snapshot_status": "complete",
            "model_id": "model-1",
            "global_model_id": "global-model-1",
            "global_model_name": "gpt-5",
            "dimensions": {"total_input_context": 10},
            "rate_multiplier": 1.25,
            "is_free_tier": false,
            "input_price_per_1m": 3.0,
            "output_price_per_1m": 15.0,
            "cache_creation_price_per_1m": 3.75,
            "cache_read_price_per_1m": 0.3,
            "price_per_request": 0.02,
            "original_headers": {"authorization": "Bearer secret"},
            "original_request_body": {"messages": []},
            "provider_request_headers": {"authorization": "Bearer secret"},
            "upstream_url": "https://example.com/v1/chat/completions"
        })))
        .expect("metadata should remain");

        assert_eq!(
            metadata,
            json!({
                "trace_id": "trace-1",
                "client_ip": "203.0.113.8",
                "user_agent": "Claude-Code/1.0",
                "client_requested_stream": false,
                "upstream_is_stream": true,
                "is_risk_control": true,
                "api_key_is_standalone": true,
                "provider_request_body_base64_bytes": 512,
                "provider_response_body_base64_bytes": 1024,
                "client_response_body_base64_bytes": 2048,
                "billing_snapshot": {"status": "complete"},
                "billing_snapshot_schema_version": "2.0",
                "billing_snapshot_status": "complete",
                "model_id": "model-1",
                "global_model_id": "global-model-1",
                "global_model_name": "gpt-5",
                "dimensions": {"total_input_context": 10},
                "rate_multiplier": 1.25,
                "is_free_tier": false,
                "input_price_per_1m": 3.0,
                "output_price_per_1m": 15.0,
                "cache_creation_price_per_1m": 3.75,
                "cache_read_price_per_1m": 0.3,
                "price_per_request": 0.02
            })
        );
    }

    #[test]
    fn sanitizes_request_path_query_metadata() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "request_path": "/v1beta/models/gemini-2.5-pro:streamGenerateContent?key=secret",
            "request_query_string": "key=secret&alt=sse&pageSize=10&token=hidden",
            "request_path_and_query": "/v1beta/models/gemini-2.5-pro:streamGenerateContent?key=secret&alt=sse&pageSize=10&token=hidden",
        })))
        .expect("metadata should remain");

        assert_eq!(
            metadata,
            json!({
                "request_path": "/v1beta/models/gemini-2.5-pro:streamGenerateContent",
                "request_query_string": "alt=sse&pageSize=10",
                "request_path_and_query": "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse&pageSize=10",
            })
        );
    }

    #[test]
    fn sanitizes_large_allowed_metadata_values_to_bounded_representations() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "trace_id": "t".repeat(2_048),
            "billing_snapshot": {
                "payload": "x".repeat(32 * 1024)
            }
        })))
        .expect("metadata should remain");

        assert!(metadata
            .get("trace_id")
            .and_then(Value::as_str)
            .is_some_and(|value| value.ends_with("...[truncated]")));
        assert_eq!(
            metadata.get("billing_snapshot"),
            Some(&json!({
                "truncated": true,
                "reason": "usage_request_metadata_limits_exceeded",
                "max_depth": MAX_USAGE_REQUEST_METADATA_DEPTH,
                "max_nodes": MAX_USAGE_REQUEST_METADATA_NODES,
                "max_bytes": MAX_USAGE_REQUEST_METADATA_BYTES,
                "value_kind": "object",
            }))
        );
    }

    #[test]
    fn sanitizes_large_prompt_capture_without_dropping_items() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "prompt_capture": {
                "version": 1,
                "item_count": 32,
                "role_counts": { "system": 2, "user": 30 },
                "items": (0..32).map(|index| {
                    json!({
                        "source": "request",
                        "role": if index < 2 { "system" } else { "user" },
                        "sha256": format!("{index:064x}"),
                        "chars": 16 * 1024,
                        "preview": "prompt preview ".repeat(700),
                        "truncated": true,
                    })
                }).collect::<Vec<_>>()
            }
        })))
        .expect("metadata should remain");
        let capture = metadata
            .get("prompt_capture")
            .and_then(Value::as_object)
            .expect("prompt capture should remain an object");

        assert_eq!(capture.get("item_count"), Some(&json!(32)));
        assert_eq!(
            capture.get("items").and_then(Value::as_array).map(Vec::len),
            Some(32)
        );
        assert!(usage_request_metadata_within_limits(
            metadata
                .get("prompt_capture")
                .expect("prompt capture should remain")
        ));
        assert!(
            serde_json::to_string(
                metadata
                    .get("prompt_capture")
                    .expect("prompt capture should remain")
            )
            .expect("prompt capture should serialize")
            .len()
                < MAX_USAGE_REQUEST_METADATA_BYTES
        );
        assert_ne!(
            capture.get("reason"),
            Some(&json!("usage_request_metadata_limits_exceeded"))
        );
    }

    #[test]
    fn sanitizes_prompt_capture_preview_to_bounded_items() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "prompt_capture": {
                "version": 1,
                "item_count": 1,
                "role_counts": { "user": 1 },
                "items": [{
                    "source": "request",
                    "role": "user",
                    "sha256": "a".repeat(64),
                    "index": 7,
                    "chars": 40_000,
                    "preview": "x".repeat(16 * 1024),
                    "truncated": true,
                }]
            }
        })))
        .expect("metadata should remain");
        let preview = metadata
            .get("prompt_capture")
            .and_then(|capture| capture.get("items"))
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("preview"))
            .and_then(Value::as_str)
            .expect("preview should remain");

        assert_eq!(preview.len(), MAX_USAGE_PROMPT_CAPTURE_PREVIEW_BYTES);
        assert_eq!(
            metadata
                .get("prompt_capture")
                .and_then(|capture| capture.get("items"))
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("index")),
            Some(&json!(7))
        );
    }

    #[test]
    fn sanitizes_prompt_capture_truncates_extreme_item_count_to_metadata_limits() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "prompt_capture": {
                "version": 1,
                "item_count": 300,
                "role_counts": { "user": 300 },
                "items": (0..300).map(|index| {
                    json!({
                        "source": "request",
                        "role": "user",
                        "sha256": format!("{index:064x}"),
                        "chars": 400,
                        "preview": "",
                        "truncated": true,
                    })
                }).collect::<Vec<_>>()
            }
        })))
        .expect("metadata should remain");
        let capture = metadata
            .get("prompt_capture")
            .expect("prompt capture should remain");
        let items = capture
            .get("items")
            .and_then(Value::as_array)
            .expect("items should remain");

        assert_eq!(capture.get("item_count"), Some(&json!(300)));
        assert!(items.len() < 256);
        let expected_first = format!("{:064x}", 300 - items.len());
        let expected_last = format!("{:064x}", 299);
        assert_eq!(
            items
                .first()
                .and_then(|item| item.get("sha256"))
                .and_then(Value::as_str),
            Some(expected_first.as_str())
        );
        assert_eq!(
            items
                .last()
                .and_then(|item| item.get("sha256"))
                .and_then(Value::as_str),
            Some(expected_last.as_str())
        );
        assert!(usage_request_metadata_within_limits(capture));
        assert_ne!(
            capture.get("reason"),
            Some(&json!("usage_request_metadata_limits_exceeded"))
        );
    }

    #[test]
    fn sanitizes_prompt_capture_marks_preview_truncated_when_runtime_trims_bytes() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "prompt_capture": {
                "version": 1,
                "item_count": 1,
                "role_counts": { "user": 1 },
                "items": [{
                    "source": "request",
                    "role": "user",
                    "sha256": "b".repeat(64),
                    "chars": 20_000,
                    "preview": "界".repeat(4_000),
                    "truncated": false,
                }]
            }
        })))
        .expect("metadata should remain");
        let item = metadata
            .get("prompt_capture")
            .and_then(|capture| capture.get("items"))
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_object)
            .expect("prompt capture item should remain");

        assert_eq!(item.get("truncated"), Some(&json!(true)));
        assert!(
            item.get("preview")
                .and_then(Value::as_str)
                .expect("preview should remain")
                .len()
                <= MAX_USAGE_PROMPT_CAPTURE_PREVIEW_BYTES
        );
    }

    #[test]
    fn sanitizes_request_metadata_preserves_tls_fingerprint() {
        let metadata = sanitize_usage_request_metadata(Some(json!({
            "tls_fingerprint": {
                "incoming": {
                    "source": "forwarded_header",
                    "ja3": "incoming-ja3",
                    "ja4": "incoming-ja4"
                },
                "outgoing": {
                    "source": "aether_transport_config",
                    "backend": "reqwest_rustls",
                    "observed": false
                }
            },
            "untrusted_tls_fingerprint": {
                "ja3": "spoofed"
            }
        })))
        .expect("metadata should remain");

        assert_eq!(
            metadata,
            json!({
                "tls_fingerprint": {
                    "incoming": {
                        "source": "forwarded_header",
                        "ja3": "incoming-ja3",
                        "ja4": "incoming-ja4"
                    },
                    "outgoing": {
                        "source": "aether_transport_config",
                        "backend": "reqwest_rustls",
                        "observed": false
                    }
                }
            })
        );
    }

    #[test]
    fn builds_seed_from_context_and_allowlisted_metadata_only() {
        let metadata = build_usage_request_metadata_seed(
            &sample_plan(),
            Some(
                json!({
                    "request_id": "req-1",
                    "candidate_index": 0,
                    "client_requested_stream": false,
                    "upstream_is_stream": true,
                    "api_key_is_standalone": true,
                    "provider_id": "provider-1",
                    "model_id": "model-1",
                    "global_model_id": "global-model-1",
                    "global_model_name": "gpt-5",
                    "client_ip": "203.0.113.8",
                    "user_agent": "Claude-Code/1.0",
                    "billing_snapshot": {"status": "complete"}
                })
                .as_object()
                .expect("object"),
            ),
        )
        .expect("metadata should remain");

        assert_eq!(
            metadata,
            json!({
                "client_requested_stream": false,
                "upstream_is_stream": true,
                "api_key_is_standalone": true,
                "model_id": "model-1",
                "global_model_id": "global-model-1",
                "global_model_name": "gpt-5",
                "client_ip": "203.0.113.8",
                "user_agent": "Claude-Code/1.0",
                "billing_snapshot": {"status": "complete"}
            })
        );
    }

    #[test]
    fn merges_and_filters_request_metadata() {
        let metadata = merge_usage_request_metadata(
            Some(json!({
                "request_id": "req-1"
            })),
            Some(json!({
                "candidate_index": 0,
                "provider_name": "OpenAI"
            })),
        );

        assert_eq!(metadata, None);
    }

    #[test]
    fn provider_request_body_metadata_uses_final_provider_body_as_source_of_truth() {
        let metadata = Some(json!({
            "trace_id": "trace-1",
            "provider_reasoning_effort": "high",
            "provider_service_tier": "priority"
        }));

        let updated = attach_provider_request_body_metadata(
            metadata.clone(),
            Some(&json!({
                "model": "gpt-5",
                "reasoning": { "effort": "low" },
                "service_tier": "standard"
            })),
        )
        .expect("metadata should remain");

        assert_eq!(
            updated,
            json!({
                "trace_id": "trace-1",
                "provider_reasoning_effort": "low",
                "provider_service_tier": "standard"
            })
        );

        let cleared = attach_provider_request_body_metadata(
            metadata,
            Some(&json!({
                "model": "gpt-5"
            })),
        )
        .expect("metadata should retain unrelated fields");

        assert_eq!(
            cleared,
            json!({
                "trace_id": "trace-1"
            })
        );
    }

    #[test]
    fn owned_merge_matches_filtered_merge_for_trusted_objects() {
        let base = Some(json!({
            "trace_id": "trace-1",
            "provider_request_body_base64_bytes": 128
        }));
        let override_value = Some(json!({
            "billing_snapshot_status": "complete",
            "trace_id": "trace-2"
        }));

        assert_eq!(
            merge_usage_request_metadata_owned(base.clone(), override_value.clone()),
            merge_usage_request_metadata(base, override_value)
        );
    }

    #[test]
    fn borrowed_sanitize_matches_owned_sanitize() {
        let value = json!({
            "trace_id": "trace-1",
            "billing_snapshot": {"status": "complete"},
            "provider_name": "OpenAI"
        });

        assert_eq!(
            sanitize_usage_request_metadata_ref(Some(&value)),
            sanitize_usage_request_metadata(Some(value))
        );
    }
}
