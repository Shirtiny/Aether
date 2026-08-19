use std::collections::HashSet;
use std::io::{self, Write};

use aether_data_contracts::repository::usage::{
    UpsertUsageRecord, UsageBodyCaptureState, UsageBodyField,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::event::UsageEvent;
use crate::runtime::{UsageBodyCapturePolicy, UsagePromptCapturePolicy, UsageRequestRecordLevel};

const TRUNCATED_BODY_STRING_SUFFIX: &str = "...[truncated]";

#[derive(Debug)]
struct LimitedUsageBodyCapture {
    value: Value,
    source_bytes: Option<u64>,
    stored_bytes: Option<u64>,
    truncated: bool,
    reason: Option<&'static str>,
}

struct UsageBodyCapturePayloadMut<'a> {
    request_body: &'a mut Option<Value>,
    request_body_ref: &'a mut Option<String>,
    request_body_state: &'a mut Option<UsageBodyCaptureState>,
    provider_request_body: &'a mut Option<Value>,
    provider_request_body_ref: &'a mut Option<String>,
    provider_request_body_state: &'a mut Option<UsageBodyCaptureState>,
    response_body: &'a mut Option<Value>,
    response_body_ref: &'a mut Option<String>,
    response_body_state: &'a mut Option<UsageBodyCaptureState>,
    client_response_body: &'a mut Option<Value>,
    client_response_body_ref: &'a mut Option<String>,
    client_response_body_state: &'a mut Option<UsageBodyCaptureState>,
    request_metadata: &'a mut Option<Value>,
}

impl<'a> UsageBodyCapturePayloadMut<'a> {
    fn from_event(event: &'a mut UsageEvent) -> Self {
        Self {
            request_body: &mut event.data.request_body,
            request_body_ref: &mut event.data.request_body_ref,
            request_body_state: &mut event.data.request_body_state,
            provider_request_body: &mut event.data.provider_request_body,
            provider_request_body_ref: &mut event.data.provider_request_body_ref,
            provider_request_body_state: &mut event.data.provider_request_body_state,
            response_body: &mut event.data.response_body,
            response_body_ref: &mut event.data.response_body_ref,
            response_body_state: &mut event.data.response_body_state,
            client_response_body: &mut event.data.client_response_body,
            client_response_body_ref: &mut event.data.client_response_body_ref,
            client_response_body_state: &mut event.data.client_response_body_state,
            request_metadata: &mut event.data.request_metadata,
        }
    }

    fn from_record(record: &'a mut UpsertUsageRecord) -> Self {
        Self {
            request_body: &mut record.request_body,
            request_body_ref: &mut record.request_body_ref,
            request_body_state: &mut record.request_body_state,
            provider_request_body: &mut record.provider_request_body,
            provider_request_body_ref: &mut record.provider_request_body_ref,
            provider_request_body_state: &mut record.provider_request_body_state,
            response_body: &mut record.response_body,
            response_body_ref: &mut record.response_body_ref,
            response_body_state: &mut record.response_body_state,
            client_response_body: &mut record.client_response_body,
            client_response_body_ref: &mut record.client_response_body_ref,
            client_response_body_state: &mut record.client_response_body_state,
            request_metadata: &mut record.request_metadata,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UsageBodyCaptureEngine {
    policy: UsageBodyCapturePolicy,
}

#[derive(Default)]
struct CountingWriter {
    bytes: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeBodyCaptureStates {
    pub request: UsageBodyCaptureState,
    pub provider_request: UsageBodyCaptureState,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeBodyCaptureMetadataInput<'a> {
    pub request_has_inline_body: bool,
    pub request_body_ref: Option<&'a str>,
    pub provider_request_has_inline_body: bool,
    pub provider_request_body_ref: Option<&'a str>,
    pub provider_request_source_bytes: Option<u64>,
    pub provider_request_unavailable: bool,
    pub provider_request_unavailable_reason: Option<&'a str>,
}

impl UsageBodyCaptureEngine {
    pub fn new(policy: UsageBodyCapturePolicy) -> Self {
        Self { policy }
    }

    pub fn apply_to_event(self, event: &mut UsageEvent) {
        self.apply_to_payload(UsageBodyCapturePayloadMut::from_event(event));
    }

    pub fn apply_to_record(self, record: &mut UpsertUsageRecord) {
        self.apply_to_payload(UsageBodyCapturePayloadMut::from_record(record));
    }

    fn apply_to_payload(self, payload: UsageBodyCapturePayloadMut<'_>) {
        if !metadata_has_prompt_capture(payload.request_metadata.as_ref()) {
            append_prompt_capture_metadata(
                payload.request_metadata,
                self.policy.prompt_capture,
                payload.request_body.as_ref(),
                payload.provider_request_body.as_ref(),
            );
        }

        if matches!(self.policy.record_level, UsageRequestRecordLevel::Basic) {
            disable_usage_body_capture_field(
                UsageBodyField::RequestBody,
                "request",
                payload.request_body,
                payload.request_body_ref,
                payload.request_body_state,
                payload.request_metadata,
            );
            disable_usage_body_capture_field(
                UsageBodyField::ProviderRequestBody,
                "provider_request",
                payload.provider_request_body,
                payload.provider_request_body_ref,
                payload.provider_request_body_state,
                payload.request_metadata,
            );
            disable_usage_body_capture_field(
                UsageBodyField::ResponseBody,
                "response",
                payload.response_body,
                payload.response_body_ref,
                payload.response_body_state,
                payload.request_metadata,
            );
            disable_usage_body_capture_field(
                UsageBodyField::ClientResponseBody,
                "client_response",
                payload.client_response_body,
                payload.client_response_body_ref,
                payload.client_response_body_state,
                payload.request_metadata,
            );
            return;
        }

        apply_usage_body_capture_limit(
            UsageBodyField::RequestBody,
            "request",
            self.policy.max_request_body_bytes,
            payload.request_body,
            payload.request_body_ref,
            payload.request_body_state,
            payload.request_metadata,
        );
        apply_usage_body_capture_limit(
            UsageBodyField::ProviderRequestBody,
            "provider_request",
            self.policy.max_request_body_bytes,
            payload.provider_request_body,
            payload.provider_request_body_ref,
            payload.provider_request_body_state,
            payload.request_metadata,
        );
        apply_usage_body_capture_limit(
            UsageBodyField::ResponseBody,
            "response",
            self.policy.max_response_body_bytes,
            payload.response_body,
            payload.response_body_ref,
            payload.response_body_state,
            payload.request_metadata,
        );
        apply_usage_body_capture_limit(
            UsageBodyField::ClientResponseBody,
            "client_response",
            self.policy.max_response_body_bytes,
            payload.client_response_body,
            payload.client_response_body_ref,
            payload.client_response_body_state,
            payload.request_metadata,
        );
    }
}

pub fn apply_usage_body_capture_policy_to_event(
    policy: UsageBodyCapturePolicy,
    event: &mut UsageEvent,
) {
    UsageBodyCaptureEngine::new(policy).apply_to_event(event);
}

pub fn apply_usage_body_capture_policy_to_record(
    policy: UsageBodyCapturePolicy,
    record: &mut UpsertUsageRecord,
) {
    UsageBodyCaptureEngine::new(policy).apply_to_record(record);
}

fn disable_usage_body_capture_field(
    field: UsageBodyField,
    metadata_key: &str,
    body: &mut Option<Value>,
    body_ref: &mut Option<String>,
    state: &mut Option<UsageBodyCaptureState>,
    request_metadata: &mut Option<Value>,
) {
    *body = None;
    *body_ref = None;
    *state = Some(UsageBodyCaptureState::Disabled);
    sync_usage_body_ref_metadata(request_metadata, field, None);
    upsert_body_capture_metadata_value_entry(
        request_metadata,
        metadata_key,
        Some(UsageBodyCaptureState::Disabled),
        None,
        None,
        Some("request_record_level_basic"),
    );
}

fn apply_usage_body_capture_limit(
    field: UsageBodyField,
    metadata_key: &str,
    max_bytes: Option<usize>,
    body: &mut Option<Value>,
    body_ref: &mut Option<String>,
    state: &mut Option<UsageBodyCaptureState>,
    request_metadata: &mut Option<Value>,
) {
    *body_ref = sanitize_usage_body_ref(body_ref.take());
    if body.is_some() && body_ref.is_some() {
        *body = None;
    }

    if let Some(body_ref_value) = body_ref.as_ref() {
        *state = Some(UsageBodyCaptureState::Reference);
        sync_usage_body_ref_metadata(request_metadata, field, Some(body_ref_value));
        upsert_body_capture_metadata_value_entry(
            request_metadata,
            metadata_key,
            Some(UsageBodyCaptureState::Reference),
            None,
            None,
            None,
        );
        return;
    }

    let Some(value) = body.take() else {
        if matches!(state, Some(UsageBodyCaptureState::Unavailable)) {
            upsert_body_capture_metadata_value_entry(
                request_metadata,
                metadata_key,
                *state,
                None,
                None,
                None,
            );
        } else if state.is_none() {
            *state = Some(UsageBodyCaptureState::None);
        }
        sync_usage_body_ref_metadata(request_metadata, field, None);
        return;
    };

    let limited = limit_usage_body_capture_value(value, max_bytes);
    let next_state = if limited.truncated {
        UsageBodyCaptureState::Truncated
    } else {
        UsageBodyCaptureState::Inline
    };
    *state = Some(next_state);
    *body = Some(limited.value);
    sync_usage_body_ref_metadata(request_metadata, field, None);
    upsert_body_capture_metadata_value_entry(
        request_metadata,
        metadata_key,
        Some(next_state),
        limited.stored_bytes,
        limited.source_bytes,
        limited.reason,
    );
}

fn limit_usage_body_capture_value(
    value: Value,
    max_bytes: Option<usize>,
) -> LimitedUsageBodyCapture {
    let source_bytes = json_serialized_len(&value);
    let Some(limit) = max_bytes.filter(|value| *value > 0) else {
        return LimitedUsageBodyCapture {
            stored_bytes: source_bytes,
            source_bytes,
            value,
            truncated: false,
            reason: None,
        };
    };
    let Some(source_len) = source_bytes else {
        return LimitedUsageBodyCapture {
            stored_bytes: None,
            source_bytes: None,
            value,
            truncated: false,
            reason: None,
        };
    };
    if source_len <= limit as u64 {
        return LimitedUsageBodyCapture {
            stored_bytes: Some(source_len),
            source_bytes: Some(source_len),
            value,
            truncated: false,
            reason: None,
        };
    }

    let truncated_value = match value {
        Value::String(text) => Value::String(truncate_usage_body_string(&text, limit)),
        other => json!({
            "truncated": true,
            "reason": "body_capture_limit_exceeded",
            "max_bytes": limit,
            "source_bytes": source_len,
            "value_kind": usage_value_kind(&other),
        }),
    };
    let stored_bytes = json_serialized_len(&truncated_value);
    LimitedUsageBodyCapture {
        value: truncated_value,
        source_bytes: Some(source_len),
        stored_bytes,
        truncated: true,
        reason: Some("body_capture_limit_exceeded"),
    }
}

fn truncate_usage_body_string(value: &str, max_bytes: usize) -> String {
    let mut end = value.len();
    while end > 0 {
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        let mut candidate = value[..end].to_string();
        candidate.push_str(TRUNCATED_BODY_STRING_SUFFIX);
        if json_serialized_len(&candidate).is_some_and(|bytes| bytes <= max_bytes as u64) {
            return candidate;
        }
        end = value[..end]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
        if end == 0 {
            break;
        }
    }

    json!({
        "truncated": true,
        "reason": "body_capture_limit_exceeded",
        "max_bytes": max_bytes,
        "value_kind": "string",
    })
    .to_string()
}

fn json_serialized_len<T: Serialize>(value: &T) -> Option<u64> {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.bytes)
}

pub(crate) fn sync_usage_body_ref_metadata(
    metadata: &mut Option<Value>,
    field: UsageBodyField,
    body_ref: Option<&str>,
) {
    let key = field.as_ref_key();
    let Some(body_ref) = body_ref.map(str::trim).filter(|value| !value.is_empty()) else {
        let clear_metadata = match metadata.as_mut() {
            Some(Value::Object(object)) => {
                object.remove(key);
                object.is_empty()
            }
            _ => false,
        };
        if clear_metadata {
            *metadata = None;
        }
        return;
    };
    if let Some(Value::Object(object)) = metadata.as_mut() {
        if object.get(key).and_then(Value::as_str) == Some(body_ref) {
            return;
        }
        object.insert(key.to_owned(), Value::String(body_ref.to_owned()));
        return;
    }
    let object = metadata
        .get_or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut();
    let Some(object) = object else {
        return;
    };
    object.insert(key.to_owned(), Value::String(body_ref.to_owned()));
}

pub(crate) fn build_payload_body_capture_metadata(
    provider_body_base64: Option<&str>,
    client_body_base64: Option<&str>,
    provider_body_state: Option<UsageBodyCaptureState>,
    client_body_state: Option<UsageBodyCaptureState>,
) -> Option<Value> {
    let provider_decoded_len = provider_body_base64.and_then(decoded_base64_len_hint);
    let client_decoded_len = client_body_base64.and_then(decoded_base64_len_hint);
    let body_capture_capacity =
        usize::from(provider_body_state.is_some()) + usize::from(client_body_state.is_some());
    let mut metadata = Map::with_capacity(
        usize::from(provider_decoded_len.is_some())
            + usize::from(client_decoded_len.is_some())
            + usize::from(body_capture_capacity > 0),
    );
    if let Some(decoded_len) = provider_decoded_len {
        metadata.insert(
            "provider_response_body_base64_bytes".to_string(),
            Value::Number(decoded_len.into()),
        );
    }
    if let Some(decoded_len) = client_decoded_len {
        metadata.insert(
            "client_response_body_base64_bytes".to_string(),
            Value::Number(decoded_len.into()),
        );
    }

    if body_capture_capacity > 0 {
        let mut body_capture = Map::with_capacity(body_capture_capacity);
        append_body_capture_metadata_entry(
            &mut body_capture,
            "response",
            provider_body_state,
            provider_decoded_len,
            provider_decoded_len,
        );
        append_body_capture_metadata_entry(
            &mut body_capture,
            "client_response",
            client_body_state,
            client_decoded_len,
            client_decoded_len,
        );
        metadata.insert("body_capture".to_string(), Value::Object(body_capture));
    }

    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

pub(crate) fn build_runtime_body_capture_states(
    request_has_inline_body: bool,
    request_body_ref: Option<&str>,
    provider_request_has_inline_body: bool,
    provider_request_body_ref: Option<&str>,
    provider_request_unavailable: bool,
) -> RuntimeBodyCaptureStates {
    RuntimeBodyCaptureStates {
        request: UsageBodyCaptureState::from_capture_parts(
            request_has_inline_body,
            request_body_ref.is_some(),
            false,
        ),
        provider_request: UsageBodyCaptureState::from_capture_parts(
            provider_request_has_inline_body,
            provider_request_body_ref.is_some(),
            provider_request_unavailable,
        ),
    }
}

pub(crate) fn append_runtime_body_capture_metadata(
    metadata: &mut Map<String, Value>,
    input: RuntimeBodyCaptureMetadataInput<'_>,
) {
    let states = build_runtime_body_capture_states(
        input.request_has_inline_body,
        input.request_body_ref,
        input.provider_request_has_inline_body,
        input.provider_request_body_ref,
        input.provider_request_unavailable,
    );
    let Some(body_capture_object) = body_capture_object_mut(metadata, 2) else {
        return;
    };
    body_capture_object.insert(
        "request".to_string(),
        build_body_capture_metadata_entry(states.request, None, None, None),
    );
    body_capture_object.insert(
        "provider_request".to_string(),
        build_body_capture_metadata_entry(
            states.provider_request,
            input.provider_request_source_bytes,
            input.provider_request_source_bytes,
            input.provider_request_unavailable_reason,
        ),
    );
}

pub(crate) fn build_plan_body_capture_metadata(
    provider_request_body_base64: Option<&str>,
) -> Option<Value> {
    provider_request_body_base64?;
    let mut metadata = Map::with_capacity(2);
    append_plan_body_capture_metadata(&mut metadata, provider_request_body_base64);
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

pub(crate) fn append_plan_body_capture_metadata(
    metadata: &mut Map<String, Value>,
    provider_request_body_base64: Option<&str>,
) {
    if let Some(body_bytes_b64) = provider_request_body_base64 {
        let decoded_len = decoded_base64_len_hint(body_bytes_b64);
        if let Some(decoded_len) = decoded_len {
            metadata.insert(
                "provider_request_body_base64_bytes".to_string(),
                Value::Number(decoded_len.into()),
            );
        }
        let Some(body_capture_object) = body_capture_object_mut(metadata, 1) else {
            return;
        };
        body_capture_object.insert(
            "provider_request".to_string(),
            build_body_capture_metadata_entry(
                UsageBodyCaptureState::Unavailable,
                decoded_len,
                decoded_len,
                Some("body_bytes_base64_only"),
            ),
        );
    }
}

fn append_body_capture_metadata_entry(
    target: &mut Map<String, Value>,
    key: &str,
    state: Option<UsageBodyCaptureState>,
    stored_bytes: Option<u64>,
    source_bytes: Option<u64>,
) {
    let Some(state) = state else {
        return;
    };
    target.insert(
        key.to_string(),
        build_body_capture_metadata_entry(
            state,
            stored_bytes,
            source_bytes,
            matches!(state, UsageBodyCaptureState::Truncated)
                .then_some("body_capture_limit_exceeded"),
        ),
    );
}

fn upsert_body_capture_metadata_value_entry(
    metadata: &mut Option<Value>,
    key: &str,
    state: Option<UsageBodyCaptureState>,
    stored_bytes: Option<u64>,
    source_bytes: Option<u64>,
    reason: Option<&str>,
) {
    let Some(state) = state else {
        return;
    };
    let Some(body_capture_object) = body_capture_value_object_mut(metadata, 1) else {
        return;
    };
    body_capture_object.insert(
        key.to_string(),
        build_body_capture_metadata_entry(state, stored_bytes, source_bytes, reason),
    );
}

fn body_capture_object_mut(
    metadata: &mut Map<String, Value>,
    capacity: usize,
) -> Option<&mut Map<String, Value>> {
    let body_capture = metadata
        .entry("body_capture".to_string())
        .or_insert_with(|| Value::Object(Map::with_capacity(capacity)));
    body_capture.as_object_mut()
}

fn body_capture_value_object_mut(
    metadata: &mut Option<Value>,
    capacity: usize,
) -> Option<&mut Map<String, Value>> {
    let metadata_object = metadata
        .get_or_insert_with(|| Value::Object(Map::with_capacity(1)))
        .as_object_mut();
    let metadata_object = metadata_object?;
    body_capture_object_mut(metadata_object, capacity)
}

fn build_body_capture_metadata_entry(
    state: UsageBodyCaptureState,
    stored_bytes: Option<u64>,
    source_bytes: Option<u64>,
    reason: Option<&str>,
) -> Value {
    let mut entry = Map::with_capacity(
        1 + usize::from(stored_bytes.is_some())
            + usize::from(source_bytes.is_some())
            + usize::from(reason.is_some()),
    );
    entry.insert(
        "state".to_string(),
        Value::String(state.as_str().to_owned()),
    );
    if let Some(bytes) = stored_bytes {
        entry.insert("stored_bytes".to_string(), json!(bytes));
    }
    if let Some(bytes) = source_bytes {
        entry.insert("source_bytes".to_string(), json!(bytes));
    }
    if let Some(reason) = reason {
        entry.insert("reason".to_string(), Value::String(reason.to_owned()));
    }
    Value::Object(entry)
}

pub(crate) fn decoded_base64_len_hint(body_base64: &str) -> Option<u64> {
    let body_base64 = body_base64.trim();
    if body_base64.is_empty() {
        return None;
    }

    let usable_len = body_base64.len();
    if usable_len % 4 == 1 {
        return None;
    }

    let padding = body_base64
        .chars()
        .rev()
        .take_while(|char| *char == '=')
        .count();
    let full_quads = usable_len / 4;
    let remainder = usable_len % 4;
    let base_len = full_quads.saturating_mul(3);
    let remainder_len = match remainder {
        0 => 0,
        2 => 1,
        3 => 2,
        _ => return None,
    };
    let decoded_len = base_len
        .saturating_add(remainder_len)
        .saturating_sub(padding.min(2));

    Some(decoded_len as u64)
}

fn sanitize_usage_body_ref(value: Option<String>) -> Option<String> {
    value.and_then(trim_owned_non_empty_string)
}

fn trim_owned_non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() == value.len() {
        return Some(value);
    }
    Some(trimmed.to_string())
}

fn usage_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptCaptureRole {
    System,
    Developer,
    User,
    Tool,
}

impl PromptCaptureRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::User => "user",
            Self::Tool => "tool",
        }
    }
}

#[derive(Debug)]
struct CapturedPrompt {
    source: String,
    index: Option<usize>,
    role: PromptCaptureRole,
    sha256: [u8; 32],
    chars: usize,
    preview: String,
    truncated: bool,
}

#[derive(Debug)]
struct PromptTextSummary {
    sha256: [u8; 32],
    chars: usize,
    preview: String,
    truncated: bool,
}

pub(crate) fn build_prompt_capture_metadata(
    policy: UsagePromptCapturePolicy,
    request_body: Option<&Value>,
    provider_request_body: Option<&Value>,
) -> Option<Value> {
    let mut metadata = None;
    append_prompt_capture_metadata(&mut metadata, policy, request_body, provider_request_body);
    metadata
}

fn metadata_has_prompt_capture(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(Value::as_object)
        .is_some_and(|object| object.contains_key("prompt_capture"))
}

fn append_prompt_capture_metadata(
    metadata: &mut Option<Value>,
    policy: UsagePromptCapturePolicy,
    request_body: Option<&Value>,
    provider_request_body: Option<&Value>,
) {
    if !policy.enabled || policy.max_items == 0 {
        return;
    }

    let mut prompts = request_body
        .map(|body| collect_prompt_capture_items("request", body, policy))
        .unwrap_or_default();
    if prompts.len() < policy.max_items {
        if let Some(body) = provider_request_body {
            let provider_prompts = collect_prompt_capture_items("provider_request", body, policy);
            append_supplemental_prompt_capture_items(
                &mut prompts,
                provider_prompts,
                policy.max_items,
            );
        }
    }
    if prompts.is_empty() {
        return;
    }

    prompts.truncate(policy.max_items);
    let items = prompts
        .iter()
        .map(prompt_capture_item_value)
        .collect::<Vec<_>>();
    let mut role_counts = Map::new();
    for prompt in &prompts {
        let key = prompt.role.as_str().to_string();
        let next = role_counts
            .get(&key)
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_add(1);
        role_counts.insert(key, json!(next));
    }

    let mut prompt_capture = Map::with_capacity(4);
    prompt_capture.insert("version".to_string(), json!(1));
    prompt_capture.insert("items".to_string(), Value::Array(items));
    prompt_capture.insert("item_count".to_string(), json!(prompts.len()));
    prompt_capture.insert("role_counts".to_string(), Value::Object(role_counts));

    let object = metadata
        .get_or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut();
    let Some(object) = object else {
        return;
    };
    object.insert("prompt_capture".to_string(), Value::Object(prompt_capture));
}

fn append_supplemental_prompt_capture_items(
    output: &mut Vec<CapturedPrompt>,
    candidates: Vec<CapturedPrompt>,
    max_items: usize,
) {
    let remaining = max_items.saturating_sub(output.len());
    if remaining == 0 || candidates.is_empty() {
        return;
    }

    let mut selected = Vec::new();
    for candidate in candidates.into_iter().rev() {
        if selected.len() >= remaining {
            break;
        }
        if output
            .iter()
            .any(|prompt| prompt.sha256 == candidate.sha256)
            || selected
                .iter()
                .any(|prompt: &CapturedPrompt| prompt.sha256 == candidate.sha256)
        {
            continue;
        }
        selected.push(candidate);
    }
    selected.reverse();
    output.extend(selected);
}

fn collect_prompt_capture_items<'a>(
    source: &str,
    value: &'a Value,
    policy: UsagePromptCapturePolicy,
) -> Vec<CapturedPrompt> {
    let mut output = Vec::with_capacity(policy.max_items.min(32));
    let mut seen = HashSet::with_capacity(policy.max_items.min(32));
    let mut seen_raw_texts = Vec::with_capacity(policy.max_items.min(32));
    collect_message_prompts_reverse(
        source,
        value,
        policy,
        &mut output,
        &mut seen,
        &mut seen_raw_texts,
    );
    collect_top_level_prompt_fields_reverse(
        source,
        value,
        policy,
        &mut output,
        &mut seen,
        &mut seen_raw_texts,
    );
    output.reverse();
    output
}

fn collect_top_level_prompt_fields_reverse<'a>(
    source: &str,
    value: &'a Value,
    policy: UsagePromptCapturePolicy,
    output: &mut Vec<CapturedPrompt>,
    seen: &mut HashSet<[u8; 32]>,
    seen_raw_texts: &mut Vec<&'a str>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    if object.get("input").is_some_and(Value::is_string) {
        collect_text_values_for_role_reverse(
            format!("{source}.input"),
            object.get("input"),
            PromptCaptureRole::User,
            policy,
            output,
            seen,
            seen_raw_texts,
            None,
        );
    }
    for key in [
        "systemInstruction",
        "system_instruction",
        "system",
        "instructions",
    ] {
        if output.len() >= policy.max_items {
            return;
        }
        collect_text_values_for_role_reverse(
            format!("{source}.{key}"),
            object.get(key),
            PromptCaptureRole::System,
            policy,
            output,
            seen,
            seen_raw_texts,
            None,
        );
    }
}

fn collect_message_prompts_reverse<'a>(
    source: &str,
    value: &'a Value,
    policy: UsagePromptCapturePolicy,
    output: &mut Vec<CapturedPrompt>,
    seen: &mut HashSet<[u8; 32]>,
    seen_raw_texts: &mut Vec<&'a str>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in ["contents", "messages", "input"] {
        if output.len() >= policy.max_items {
            return;
        }
        collect_message_array_reverse(
            source,
            key,
            object.get(key),
            policy,
            output,
            seen,
            seen_raw_texts,
        );
    }
}

fn collect_message_array_reverse<'a>(
    source: &str,
    array_key: &str,
    value: Option<&'a Value>,
    policy: UsagePromptCapturePolicy,
    output: &mut Vec<CapturedPrompt>,
    seen: &mut HashSet<[u8; 32]>,
    seen_raw_texts: &mut Vec<&'a str>,
) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for (message_index, item) in items.iter().enumerate().rev() {
        if output.len() >= policy.max_items {
            return;
        }
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(role) = object
            .get("role")
            .and_then(Value::as_str)
            .and_then(prompt_capture_role_from_str)
        else {
            continue;
        };
        if !prompt_capture_role_enabled(policy, role) {
            continue;
        }
        for key in ["parts", "text", "content"] {
            if output.len() >= policy.max_items {
                return;
            }
            collect_text_values_for_role_reverse(
                format!("{source}.{array_key}[{message_index}].{key}"),
                object.get(key),
                role,
                policy,
                output,
                seen,
                seen_raw_texts,
                Some(message_index),
            );
        }
    }
}

fn collect_text_values_for_role_reverse<'a>(
    source: String,
    value: Option<&'a Value>,
    role: PromptCaptureRole,
    policy: UsagePromptCapturePolicy,
    output: &mut Vec<CapturedPrompt>,
    seen: &mut HashSet<[u8; 32]>,
    seen_raw_texts: &mut Vec<&'a str>,
    index: Option<usize>,
) {
    if output.len() >= policy.max_items || !prompt_capture_role_enabled(policy, role) {
        return;
    }
    let Some(value) = value else {
        return;
    };
    match value {
        Value::String(text) => push_prompt_text(
            source,
            index,
            role,
            text,
            policy,
            output,
            seen,
            seen_raw_texts,
        ),
        Value::Array(items) => {
            for (item_index, item) in items.iter().enumerate().rev() {
                if output.len() >= policy.max_items {
                    return;
                }
                collect_text_values_for_role_reverse(
                    format!("{source}[{item_index}]"),
                    Some(item),
                    role,
                    policy,
                    output,
                    seen,
                    seen_raw_texts,
                    index,
                );
            }
        }
        Value::Object(object) => {
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("tool_call"))
                && !policy.include_tools
            {
                return;
            }
            for key in ["input", "content", "text"] {
                if output.len() >= policy.max_items {
                    return;
                }
                collect_text_values_for_role_reverse(
                    format!("{source}.{key}"),
                    object.get(key),
                    role,
                    policy,
                    output,
                    seen,
                    seen_raw_texts,
                    index,
                );
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn push_prompt_text<'a>(
    source: String,
    index: Option<usize>,
    role: PromptCaptureRole,
    text: &'a str,
    policy: UsagePromptCapturePolicy,
    output: &mut Vec<CapturedPrompt>,
    seen: &mut HashSet<[u8; 32]>,
    seen_raw_texts: &mut Vec<&'a str>,
) {
    if output.len() >= policy.max_items {
        return;
    }
    if seen_raw_texts.iter().any(|seen| *seen == text) {
        return;
    }
    let Some(summary) = summarize_prompt_text(text, policy.preview_chars) else {
        return;
    };
    if !seen.insert(summary.sha256) {
        return;
    }
    seen_raw_texts.push(text);
    output.push(CapturedPrompt {
        source,
        index,
        role,
        sha256: summary.sha256,
        chars: summary.chars,
        preview: summary.preview,
        truncated: summary.truncated,
    });
}

fn prompt_capture_role_from_str(value: &str) -> Option<PromptCaptureRole> {
    if value.eq_ignore_ascii_case("system") {
        Some(PromptCaptureRole::System)
    } else if value.eq_ignore_ascii_case("developer") {
        Some(PromptCaptureRole::Developer)
    } else if value.eq_ignore_ascii_case("user") {
        Some(PromptCaptureRole::User)
    } else if value.eq_ignore_ascii_case("tool") || value.eq_ignore_ascii_case("function") {
        Some(PromptCaptureRole::Tool)
    } else {
        None
    }
}

fn prompt_capture_role_enabled(policy: UsagePromptCapturePolicy, role: PromptCaptureRole) -> bool {
    match role {
        PromptCaptureRole::System => policy.include_system,
        PromptCaptureRole::Developer => policy.include_developer,
        PromptCaptureRole::User => policy.include_user,
        PromptCaptureRole::Tool => policy.include_tools,
    }
}

fn prompt_capture_item_value(prompt: &CapturedPrompt) -> Value {
    json!({
        "source": prompt.source,
        "index": prompt.index,
        "role": prompt.role.as_str(),
        "sha256": sha256_hex(&prompt.sha256),
        "chars": prompt.chars,
        "preview": prompt.preview,
        "truncated": prompt.truncated
    })
}

fn summarize_prompt_text(text: &str, preview_chars: usize) -> Option<PromptTextSummary> {
    let mut hasher = Sha256::new();
    let mut preview = String::new();
    let mut normalized_chars = 0usize;
    let mut preview_len = 0usize;
    let mut has_token = false;

    for token in text.split_whitespace() {
        if has_token {
            hasher.update(b" ");
            normalized_chars = normalized_chars.saturating_add(1);
            if preview_len < preview_chars {
                preview.push(' ');
                preview_len += 1;
            }
        }
        has_token = true;
        hasher.update(token.as_bytes());
        for character in token.chars() {
            normalized_chars = normalized_chars.saturating_add(1);
            if preview_len < preview_chars {
                preview.push(character);
                preview_len += 1;
            }
        }
    }

    has_token.then(|| PromptTextSummary {
        sha256: hasher.finalize().into(),
        chars: normalized_chars,
        truncated: preview_len < normalized_chars,
        preview,
    })
}

fn sha256_hex(digest: &[u8; 32]) -> String {
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::{
        build_plan_body_capture_metadata, build_prompt_capture_metadata,
        prompt_capture_role_enabled, prompt_capture_role_from_str, sync_usage_body_ref_metadata,
        trim_owned_non_empty_string, truncate_usage_body_string,
        upsert_body_capture_metadata_value_entry, PromptCaptureRole,
    };
    use crate::runtime::{UsageBodyCapturePolicy, UsagePromptCapturePolicy};
    use crate::{apply_usage_body_capture_policy_to_record, UsageRequestRecordLevel};
    use aether_data_contracts::repository::usage::{
        UpsertUsageRecord, UsageBodyCaptureState, UsageBodyField,
    };
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};
    use std::time::Instant;

    #[test]
    fn build_plan_body_capture_metadata_returns_none_without_base64_body() {
        assert!(build_plan_body_capture_metadata(None).is_none());
    }

    #[test]
    fn trim_owned_non_empty_string_preserves_clean_values_and_drops_blank_ones() {
        assert_eq!(
            trim_owned_non_empty_string("blob://body-ref-1".to_string()),
            Some("blob://body-ref-1".to_string()),
        );
        assert_eq!(
            trim_owned_non_empty_string("  blob://body-ref-1  ".to_string()),
            Some("blob://body-ref-1".to_string()),
        );
        assert_eq!(trim_owned_non_empty_string("   ".to_string()), None);
    }

    #[test]
    fn upsert_body_capture_metadata_value_entry_ignores_none_state() {
        let mut metadata = Some(Value::Object(Map::<String, Value>::new()));
        upsert_body_capture_metadata_value_entry(&mut metadata, "response", None, None, None, None);
        assert_eq!(metadata, Some(Value::Object(Map::new())));
    }

    #[test]
    fn upsert_body_capture_metadata_value_entry_preserves_existing_metadata_fields() {
        let mut metadata = Some(Value::Object(Map::from_iter([(
            "request_body_ref".to_string(),
            Value::String("blob://body-ref-1".to_string()),
        )])));

        upsert_body_capture_metadata_value_entry(
            &mut metadata,
            "response",
            Some(UsageBodyCaptureState::Reference),
            None,
            None,
            None,
        );

        assert_eq!(
            metadata,
            Some(Value::Object(Map::from_iter([
                (
                    "request_body_ref".to_string(),
                    Value::String("blob://body-ref-1".to_string()),
                ),
                (
                    "body_capture".to_string(),
                    Value::Object(Map::from_iter([(
                        "response".to_string(),
                        Value::Object(Map::from_iter([(
                            "state".to_string(),
                            Value::String("reference".to_string()),
                        )])),
                    )])),
                ),
            ]))),
        );
    }

    #[test]
    fn sync_usage_body_ref_metadata_clears_empty_metadata_object() {
        let mut metadata = Some(Value::Object(Map::from_iter([(
            "request_body_ref".to_string(),
            Value::String("blob://body-ref-1".to_string()),
        )])));

        sync_usage_body_ref_metadata(&mut metadata, UsageBodyField::RequestBody, None);

        assert!(metadata.is_none());
    }

    #[test]
    fn sync_usage_body_ref_metadata_preserves_existing_ref_value() {
        let mut metadata = Some(Value::Object(Map::from_iter([(
            "request_body_ref".to_string(),
            Value::String("blob://body-ref-1".to_string()),
        )])));

        sync_usage_body_ref_metadata(
            &mut metadata,
            UsageBodyField::RequestBody,
            Some("blob://body-ref-1"),
        );

        assert_eq!(
            metadata,
            Some(Value::Object(Map::from_iter([(
                "request_body_ref".to_string(),
                Value::String("blob://body-ref-1".to_string()),
            )]))),
        );
    }

    #[test]
    fn truncate_usage_body_string_respects_json_byte_limit() {
        let limit = 32usize;
        let truncated = truncate_usage_body_string("x".repeat(256).as_str(), limit);

        assert!(truncated.ends_with("...[truncated]"));
        assert!(serde_json::to_vec(&truncated)
            .ok()
            .is_some_and(|bytes| bytes.len() <= limit));
    }

    #[test]
    fn prompt_capture_metadata_extracts_prompts_before_basic_body_strip() {
        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "instructions": "  You are Codex.\nBe concise.  ",
            "input": [
                {"role": "developer", "content": [{"type": "text", "text": "Prefer safe changes."}]},
                {"role": "user", "content": "Please inspect this request."}
            ]
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                record_level: UsageRequestRecordLevel::Basic,
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    preview_chars: 12,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        assert!(record.request_body.is_none());
        let prompt_capture = record
            .request_metadata
            .as_ref()
            .and_then(|value| value.get("prompt_capture"))
            .expect("prompt capture metadata should exist");
        assert_eq!(prompt_capture["item_count"], json!(3));
        assert_eq!(prompt_capture["role_counts"]["system"], json!(1));
        assert_eq!(prompt_capture["role_counts"]["developer"], json!(1));
        assert_eq!(prompt_capture["role_counts"]["user"], json!(1));
        assert_eq!(prompt_capture["items"][0]["preview"], json!("You are Code"));
        assert_eq!(
            prompt_capture["items"][0]["source"],
            json!("request.instructions")
        );
        assert_eq!(prompt_capture["items"][0]["truncated"], json!(true));
        assert!(prompt_capture["items"][0]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64));
    }

    #[test]
    fn prompt_capture_metadata_reuses_precomputed_items() {
        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "messages": [{"role": "user", "content": "body should not replace metadata"}]
        }));
        record.request_metadata = Some(json!({
            "prompt_capture": {
                "version": 1,
                "item_count": 1,
                "items": [{
                    "source": "request.messages[0].content",
                    "role": "user",
                    "sha256": "a".repeat(64),
                    "chars": 11,
                    "preview": "precomputed",
                    "truncated": false
                }]
            }
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                record_level: UsageRequestRecordLevel::Basic,
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        assert_eq!(
            record
                .request_metadata
                .as_ref()
                .and_then(|metadata| metadata.pointer("/prompt_capture/items/0/preview")),
            Some(&json!("precomputed"))
        );
    }

    #[test]
    fn prompt_capture_metadata_deduplicates_normalized_prompt_text() {
        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "input": [
                {"role": "user", "content": "Repeat this prompt."},
                {"role": "user", "content": "Unique request prompt."}
            ]
        }));
        record.provider_request_body = Some(json!({
            "messages": [
                {"role": "user", "content": " Repeat   this\nprompt. "},
                {"role": "developer", "content": "Unique provider prompt."}
            ]
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    max_items: 8,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        let prompt_capture = record
            .request_metadata
            .as_ref()
            .and_then(|value| value.get("prompt_capture"))
            .expect("prompt capture metadata should exist");
        assert_eq!(prompt_capture["item_count"], json!(3));
        assert_eq!(prompt_capture["role_counts"]["user"], json!(2));
        assert_eq!(prompt_capture["role_counts"]["developer"], json!(1));
        assert_eq!(
            prompt_capture["items"][0]["preview"],
            json!("Repeat this prompt.")
        );
        assert_eq!(
            prompt_capture["items"][1]["preview"],
            json!("Unique request prompt.")
        );
        assert_eq!(
            prompt_capture["items"][2]["preview"],
            json!("Unique provider prompt.")
        );
    }

    #[test]
    fn prompt_capture_metadata_provider_supplements_without_evicting_request_prompts() {
        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "input": [
                {"role": "user", "content": "request prompt 1"},
                {"role": "user", "content": "request prompt 2"},
                {"role": "user", "content": "request prompt 3"}
            ]
        }));
        record.provider_request_body = Some(json!({
            "messages": [
                {"role": "user", "content": "provider prompt 1"},
                {"role": "user", "content": "provider prompt 2"}
            ]
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    max_items: 4,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        let prompt_capture = record
            .request_metadata
            .as_ref()
            .and_then(|value| value.get("prompt_capture"))
            .expect("prompt capture metadata should exist");
        assert_eq!(prompt_capture["item_count"], json!(4));
        assert_eq!(
            prompt_capture["items"][0]["preview"],
            json!("request prompt 1")
        );
        assert_eq!(
            prompt_capture["items"][1]["preview"],
            json!("request prompt 2")
        );
        assert_eq!(
            prompt_capture["items"][2]["preview"],
            json!("request prompt 3")
        );
        assert_eq!(
            prompt_capture["items"][3]["preview"],
            json!("provider prompt 2")
        );
        assert_eq!(
            prompt_capture["items"][3]["source"],
            json!("provider_request.messages[1].content")
        );
        assert!(!prompt_capture["items"]
            .as_array()
            .expect("items should be an array")
            .iter()
            .any(|item| item["preview"] == json!("provider prompt 1")));
    }

    #[test]
    fn prompt_capture_metadata_prefers_latest_duplicate_message_source() {
        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "input": [
                {"role": "user", "content": "Repeat this prompt."},
                {"role": "user", "content": "Unique request prompt."},
                {"role": "user", "content": " Repeat   this\nprompt. "}
            ]
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    max_items: 8,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        let prompt_capture = record
            .request_metadata
            .as_ref()
            .and_then(|value| value.get("prompt_capture"))
            .expect("prompt capture metadata should exist");
        assert_eq!(prompt_capture["item_count"], json!(2));
        assert_eq!(
            prompt_capture["items"][1]["preview"],
            json!("Repeat this prompt.")
        );
        assert_eq!(
            prompt_capture["items"][1]["source"],
            json!("request.input[2].content")
        );
        assert_eq!(prompt_capture["items"][1]["index"], json!(2));
    }

    #[test]
    fn prompt_capture_metadata_extracts_openai_responses_string_input() {
        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "model": "gpt-5",
            "input": "Summarize this incident."
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        let prompt_capture = record
            .request_metadata
            .as_ref()
            .and_then(|value| value.get("prompt_capture"))
            .expect("prompt capture metadata should exist");
        assert_eq!(prompt_capture["item_count"], json!(1));
        assert_eq!(prompt_capture["role_counts"]["user"], json!(1));
        assert_eq!(
            prompt_capture["items"][0]["preview"],
            json!("Summarize this incident.")
        );
    }

    #[test]
    fn prompt_capture_metadata_keeps_recent_messages_when_history_exceeds_limit() {
        let mut history = (1..=31)
            .map(|index| json!({"role": "user", "content": format!("old prompt {index}")}))
            .collect::<Vec<_>>();
        history.push(json!({"role": "user", "content": "current request prompt"}));

        let mut record = sample_usage_record();
        record.request_body = Some(json!({
            "instructions": "system prompt",
            "input": history
        }));

        apply_usage_body_capture_policy_to_record(
            UsageBodyCapturePolicy {
                prompt_capture: UsagePromptCapturePolicy {
                    enabled: true,
                    max_items: 32,
                    ..UsagePromptCapturePolicy::default()
                },
                ..UsageBodyCapturePolicy::default()
            },
            &mut record,
        );

        let prompt_capture = record
            .request_metadata
            .as_ref()
            .and_then(|value| value.get("prompt_capture"))
            .expect("prompt capture metadata should exist");
        assert_eq!(prompt_capture["item_count"], json!(32));
        assert!(prompt_capture["role_counts"]["system"].is_null());
        assert_eq!(prompt_capture["role_counts"]["user"], json!(32));
        assert_eq!(prompt_capture["items"][0]["preview"], json!("old prompt 1"));
        assert_eq!(
            prompt_capture["items"][0]["source"],
            json!("request.input[0].content")
        );
        assert_eq!(prompt_capture["items"][0]["index"], json!(0));
        assert_eq!(
            prompt_capture["items"][31]["preview"],
            json!("current request prompt")
        );
        assert_eq!(
            prompt_capture["items"][31]["source"],
            json!("request.input[31].content")
        );
        assert_eq!(prompt_capture["items"][31]["index"], json!(31));
        assert!(!prompt_capture["items"]
            .as_array()
            .expect("items should be an array")
            .iter()
            .any(|item| item["preview"] == json!("system prompt")));
    }

    #[derive(Debug, Clone)]
    struct LegacyCapturedPrompt {
        source: String,
        index: Option<usize>,
        role: PromptCaptureRole,
        text: String,
    }

    fn legacy_prompt_capture_metadata(
        policy: UsagePromptCapturePolicy,
        request_body: Option<&Value>,
        provider_request_body: Option<&Value>,
    ) -> Option<Value> {
        if !policy.enabled || policy.max_items == 0 {
            return None;
        }
        let mut prompts = request_body
            .map(|body| legacy_collect_prompt_capture_items("request", body, policy))
            .unwrap_or_default();
        if prompts.len() < policy.max_items {
            if let Some(body) = provider_request_body {
                let candidates =
                    legacy_collect_prompt_capture_items("provider_request", body, policy);
                let remaining = policy.max_items.saturating_sub(prompts.len());
                let mut selected = Vec::new();
                for candidate in candidates.into_iter().rev() {
                    if selected.len() >= remaining {
                        break;
                    }
                    if prompts.iter().any(|prompt| prompt.text == candidate.text)
                        || selected
                            .iter()
                            .any(|prompt: &LegacyCapturedPrompt| prompt.text == candidate.text)
                    {
                        continue;
                    }
                    selected.push(candidate);
                }
                selected.reverse();
                prompts.extend(selected);
            }
        }
        if prompts.is_empty() {
            return None;
        }
        prompts.truncate(policy.max_items);

        let mut role_counts = Map::new();
        for prompt in &prompts {
            let role = prompt.role.as_str().to_string();
            let count = role_counts
                .get(&role)
                .and_then(Value::as_u64)
                .unwrap_or_default()
                .saturating_add(1);
            role_counts.insert(role, json!(count));
        }
        let items = prompts
            .iter()
            .map(|prompt| legacy_prompt_capture_item(prompt, policy.preview_chars))
            .collect::<Vec<_>>();
        Some(json!({
            "prompt_capture": {
                "version": 1,
                "items": items,
                "item_count": prompts.len(),
                "role_counts": role_counts
            }
        }))
    }

    fn legacy_collect_prompt_capture_items(
        source: &str,
        value: &Value,
        policy: UsagePromptCapturePolicy,
    ) -> Vec<LegacyCapturedPrompt> {
        let mut output = Vec::new();
        let Some(object) = value.as_object() else {
            return output;
        };
        for key in [
            "instructions",
            "system",
            "system_instruction",
            "systemInstruction",
        ] {
            legacy_collect_text_values(
                format!("{source}.{key}"),
                object.get(key),
                PromptCaptureRole::System,
                policy,
                &mut output,
                None,
            );
        }
        if object.get("input").is_some_and(Value::is_string) {
            legacy_collect_text_values(
                format!("{source}.input"),
                object.get("input"),
                PromptCaptureRole::User,
                policy,
                &mut output,
                None,
            );
        }
        for array_key in ["input", "messages", "contents"] {
            let Some(Value::Array(items)) = object.get(array_key) else {
                continue;
            };
            for (message_index, item) in items.iter().enumerate() {
                let Some(message) = item.as_object() else {
                    continue;
                };
                let Some(role) = message
                    .get("role")
                    .and_then(Value::as_str)
                    .and_then(prompt_capture_role_from_str)
                else {
                    continue;
                };
                if !prompt_capture_role_enabled(policy, role) {
                    continue;
                }
                for key in ["content", "text", "parts"] {
                    legacy_collect_text_values(
                        format!("{source}.{array_key}[{message_index}].{key}"),
                        message.get(key),
                        role,
                        policy,
                        &mut output,
                        Some(message_index),
                    );
                }
            }
        }
        output
    }

    fn legacy_collect_text_values(
        source: String,
        value: Option<&Value>,
        role: PromptCaptureRole,
        policy: UsagePromptCapturePolicy,
        output: &mut Vec<LegacyCapturedPrompt>,
        index: Option<usize>,
    ) {
        if !prompt_capture_role_enabled(policy, role) {
            return;
        }
        let Some(value) = value else {
            return;
        };
        match value {
            Value::String(text) => {
                let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if normalized.is_empty() || policy.max_items == 0 {
                    return;
                }
                if let Some(position) = output.iter().position(|prompt| prompt.text == normalized) {
                    output.remove(position);
                }
                if output.len() >= policy.max_items {
                    output.remove(0);
                }
                output.push(LegacyCapturedPrompt {
                    source,
                    index,
                    role,
                    text: normalized,
                });
            }
            Value::Array(items) => {
                for (item_index, item) in items.iter().enumerate() {
                    legacy_collect_text_values(
                        format!("{source}[{item_index}]"),
                        Some(item),
                        role,
                        policy,
                        output,
                        index,
                    );
                }
            }
            Value::Object(object) => {
                if object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("tool_call"))
                    && !policy.include_tools
                {
                    return;
                }
                for key in ["text", "content", "input"] {
                    legacy_collect_text_values(
                        format!("{source}.{key}"),
                        object.get(key),
                        role,
                        policy,
                        output,
                        index,
                    );
                }
            }
            _ => {}
        }
    }

    fn legacy_prompt_capture_item(prompt: &LegacyCapturedPrompt, preview_chars: usize) -> Value {
        let digest = Sha256::digest(prompt.text.as_bytes());
        let mut sha256 = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut sha256, "{byte:02x}");
        }
        let chars = prompt.text.chars().count();
        let preview = prompt.text.chars().take(preview_chars).collect::<String>();
        json!({
            "source": prompt.source,
            "index": prompt.index,
            "role": prompt.role.as_str(),
            "sha256": sha256,
            "chars": chars,
            "preview": preview,
            "truncated": preview.chars().count() < chars
        })
    }

    #[test]
    fn bounded_prompt_capture_matches_legacy_reference_across_shapes_and_policies() {
        let bodies = [
            json!({
                "instructions": "  system\n prompt  ",
                "input": [
                    {"role": "developer", "content": [{"type": "text", "text": "dev prompt"}]},
                    {"role": "user", "content": "same\ttext"},
                    {"role": "user", "content": [{"text": "same text"}, {"text": "latest user"}]}
                ]
            }),
            json!({
                "messages": [
                    {"role": "system", "content": "chat system"},
                    {"role": "tool", "content": {"type": "tool_call", "input": "tool input"}},
                    {"role": "user", "content": ["first", {"text": "second"}]}
                ]
            }),
            json!({
                "system_instruction": {"parts": [{"text": "gemini system"}]},
                "contents": [
                    {"role": "user", "parts": [{"text": "gemini user"}]},
                    {"role": "developer", "parts": ["unicode\u{2003}space"]}
                ]
            }),
        ];

        for request_index in 0..bodies.len() {
            for max_items in [1, 2, 4, 8] {
                for preview_chars in [0, 1, 7, 64] {
                    for flags in 0_u8..16 {
                        let policy = UsagePromptCapturePolicy {
                            enabled: true,
                            include_system: flags & 1 != 0,
                            include_developer: flags & 2 != 0,
                            include_user: flags & 4 != 0,
                            include_tools: flags & 8 != 0,
                            preview_chars,
                            max_items,
                        };
                        let request = Some(&bodies[request_index]);
                        let provider = Some(&bodies[(request_index + 1) % bodies.len()]);
                        assert_eq!(
                            build_prompt_capture_metadata(policy, request, provider),
                            legacy_prompt_capture_metadata(policy, request, provider),
                            "shape={request_index} max_items={max_items} preview={preview_chars} flags={flags}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_prompt_capture_matches_legacy_reference_for_generated_histories() {
        let roles = ["system", "developer", "user", "tool"];
        let mut state = 0x9e37_79b9_u64;
        for case in 0..128_usize {
            let count = 1 + (next_test_random(&mut state) as usize % 48);
            let mut messages = Vec::with_capacity(count);
            for index in 0..count {
                let role = roles[next_test_random(&mut state) as usize % roles.len()];
                let text_id = next_test_random(&mut state) % 13;
                let text = match next_test_random(&mut state) % 4 {
                    0 => format!(" prompt  {text_id}\nvalue "),
                    1 => format!("prompt\t{text_id}\u{2003}value"),
                    2 => String::new(),
                    _ => format!("unique-{case}-{index}"),
                };
                let content = match next_test_random(&mut state) % 3 {
                    0 => Value::String(text),
                    1 => json!([{"type": "text", "text": text}]),
                    _ => json!({"text": text}),
                };
                messages.push(json!({"role": role, "content": content}));
            }
            let request = json!({
                "instructions": format!("system-{}", next_test_random(&mut state) % 5),
                "input": messages
            });
            let provider = json!({
                "messages": request["input"]
                    .as_array()
                    .expect("generated messages")
                    .iter()
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let policy = UsagePromptCapturePolicy {
                enabled: true,
                include_system: next_test_random(&mut state) & 1 != 0,
                include_developer: next_test_random(&mut state) & 1 != 0,
                include_user: next_test_random(&mut state) & 1 != 0,
                include_tools: next_test_random(&mut state) & 1 != 0,
                preview_chars: next_test_random(&mut state) as usize % 33,
                max_items: 1 + (next_test_random(&mut state) as usize % 16),
            };
            assert_eq!(
                build_prompt_capture_metadata(policy, Some(&request), Some(&provider)),
                legacy_prompt_capture_metadata(policy, Some(&request), Some(&provider)),
                "generated case {case}",
            );
        }
    }

    fn next_test_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    #[test]
    #[ignore = "manual 1/10/40 MiB prompt capture benchmark"]
    fn prompt_capture_large_body_benchmark() {
        let policy = UsagePromptCapturePolicy {
            enabled: true,
            include_system: true,
            include_developer: true,
            include_user: true,
            include_tools: false,
            preview_chars: 512,
            max_items: 32,
        };
        for size_mib in [1_usize, 10, 40] {
            for all_duplicate in [false, true] {
                let body = benchmark_prompt_body(size_mib * 1024 * 1024, all_duplicate);
                let legacy_started = Instant::now();
                let legacy = legacy_prompt_capture_metadata(policy, Some(&body), None);
                let legacy_elapsed = legacy_started.elapsed();
                let bounded_started = Instant::now();
                let bounded = build_prompt_capture_metadata(policy, Some(&body), None);
                let bounded_elapsed = bounded_started.elapsed();
                assert_eq!(bounded, legacy);
                let speedup =
                    legacy_elapsed.as_secs_f64() / bounded_elapsed.as_secs_f64().max(f64::EPSILON);
                eprintln!(
                    "prompt_capture_bench size_mib={size_mib} all_duplicate={all_duplicate} legacy_ms={:.3} bounded_ms={:.3} speedup={speedup:.2}x",
                    legacy_elapsed.as_secs_f64() * 1000.0,
                    bounded_elapsed.as_secs_f64() * 1000.0,
                );
                if size_mib == 40 && !all_duplicate {
                    assert!(
                        speedup >= 3.0,
                        "40 MiB recent-32 scenario must be at least 3x faster; measured {speedup:.2}x"
                    );
                }
            }
        }
    }

    fn benchmark_prompt_body(target_bytes: usize, all_duplicate: bool) -> Value {
        const PAYLOAD_BYTES: usize = 32 * 1024;
        let message_count = target_bytes.div_ceil(PAYLOAD_BYTES).max(32);
        let payload = "x".repeat(PAYLOAD_BYTES);
        let messages = (0..message_count)
            .map(|index| {
                let label = if all_duplicate || index + 32 < message_count {
                    "repeated".to_string()
                } else {
                    format!("recent-{index}")
                };
                json!({
                    "role": "user",
                    "content": format!("{label} {payload}")
                })
            })
            .collect::<Vec<_>>();
        json!({"input": messages})
    }

    fn sample_usage_record() -> UpsertUsageRecord {
        UpsertUsageRecord {
            request_id: "req-prompt-1".to_string(),
            user_id: Some("user-1".to_string()),
            api_key_id: None,
            username: None,
            api_key_name: None,
            provider_name: "openai".to_string(),
            model: "gpt-5".to_string(),
            target_model: None,
            provider_id: None,
            provider_endpoint_id: None,
            provider_api_key_id: None,
            request_type: Some("chat".to_string()),
            api_format: Some("openai:responses".to_string()),
            api_family: None,
            endpoint_kind: None,
            endpoint_api_format: None,
            provider_api_family: None,
            provider_endpoint_kind: None,
            has_format_conversion: None,
            is_stream: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cache_creation_input_tokens: None,
            cache_creation_ephemeral_5m_input_tokens: None,
            cache_creation_ephemeral_1h_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_cost_usd: None,
            cache_read_cost_usd: None,
            output_price_per_1m: None,
            total_cost_usd: None,
            actual_total_cost_usd: None,
            status_code: Some(200),
            error_message: None,
            error_category: None,
            response_time_ms: None,
            first_byte_time_ms: None,
            status: "completed".to_string(),
            billing_status: "void".to_string(),
            request_headers: None,
            request_body: None,
            request_body_ref: None,
            request_body_state: None,
            provider_request_headers: None,
            provider_request_body: None,
            provider_request_body_ref: None,
            provider_request_body_state: None,
            response_headers: None,
            response_body: None,
            response_body_ref: None,
            response_body_state: None,
            client_response_headers: None,
            client_response_body: None,
            client_response_body_ref: None,
            client_response_body_state: None,
            candidate_id: None,
            candidate_index: None,
            key_name: None,
            planner_kind: None,
            route_family: None,
            route_kind: None,
            execution_path: None,
            local_execution_runtime_miss_reason: None,
            request_metadata: None,
            finalized_at_unix_secs: None,
            created_at_unix_ms: None,
            updated_at_unix_secs: 1,
        }
    }
}
