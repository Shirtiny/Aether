use std::fmt::Write;

use crate::provider_compat::proxy::rules::body_rules_handle_path;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const OPENAI_RESPONSES_PROMPT_CACHE_NAMESPACE_VERSION: &str = "v1";
const SESSION_IDENTITY_FIELDS: &[&str] = &[
    "session_id",
    "sessionId",
    "conversation_id",
    "conversationId",
    "thread_id",
    "threadId",
];

/// Describes the request property used to derive an Aether-owned
/// `prompt_cache_key`. The value is safe to log and never contains request
/// identity or prompt material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiResponsesPromptCacheKeySource {
    /// An explicit conversation/session signal supplied by the request.
    Session,
    /// A stable prompt-prefix cohort inferred from append-only request history.
    ContentPrefix,
}

impl OpenAiResponsesPromptCacheKeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::ContentPrefix => "content_prefix",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPromptCacheCohort {
    prompt_cache_key: String,
    source: OpenAiResponsesPromptCacheKeySource,
}

fn build_stable_prompt_cache_key_from_seed(kind: &str, seed: &str) -> Option<String> {
    let normalized = seed.trim();
    if normalized.is_empty() {
        return None;
    }

    let normalized_kind = kind
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect::<String>();
    let normalized_kind = if normalized_kind.is_empty() {
        "seed".to_string()
    } else {
        normalized_kind
    };
    let namespace = format!(
        "aether:openai-responses:prompt-cache:{OPENAI_RESPONSES_PROMPT_CACHE_NAMESPACE_VERSION}:{normalized_kind}:{normalized}"
    );
    Some(Uuid::new_v5(&Uuid::NAMESPACE_OID, namespace.as_bytes()).to_string())
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn direct_session_identity(object: &Map<String, Value>) -> Option<&str> {
    SESSION_IDENTITY_FIELDS
        .iter()
        .find_map(|field| non_empty_str(object.get(*field)))
}

fn session_identity_from_container(container: &Value) -> Option<String> {
    fn find(value: &Value, depth: u8) -> Option<String> {
        if depth == 0 {
            return None;
        }
        if let Some(object) = value.as_object() {
            if let Some(identity) = direct_session_identity(object) {
                return Some(identity.to_string());
            }
            return object
                .values()
                .find_map(|value| find(value, depth.saturating_sub(1)));
        }
        let raw = non_empty_str(Some(value))?;
        let decoded = serde_json::from_str::<Value>(raw).ok()?;
        find(&decoded, depth.saturating_sub(1))
    }

    // Metadata is an identity carrier. Nested JSON strings such as turn
    // metadata are accepted without depending on a client family or UA.
    find(container, 3)
}

fn conversation_identity(object: &Map<String, Value>) -> Option<String> {
    match object.get("conversation")? {
        Value::String(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        }
        Value::Object(conversation) => non_empty_str(conversation.get("id")).map(ToOwned::to_owned),
        _ => None,
    }
}

fn extract_responses_prompt_cache_session_seed(provider_request_body: &Value) -> Option<String> {
    let object = provider_request_body.as_object()?;
    direct_session_identity(object)
        .map(|value| format!("body:{value}"))
        .or_else(|| conversation_identity(object).map(|value| format!("conversation:{value}")))
        .or_else(|| {
            object
                .get("metadata")
                .and_then(session_identity_from_container)
                .map(|value| format!("metadata:{value}"))
        })
        .or_else(|| {
            object
                .get("client_metadata")
                .and_then(session_identity_from_container)
                .map(|value| format!("client_metadata:{value}"))
        })
}

fn sha256_hex_from_hasher(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn update_len_prefixed(hasher: &mut Sha256, tag: u8, bytes: &[u8]) {
    hasher.update([tag]);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// Hashes JSON deterministically without cloning large prompt values. Object
/// key ordering is normalized, while array ordering and the complete string
/// content are preserved because both affect the actual reusable prefix.
fn update_canonical_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update([b'n']),
        Value::Bool(value) => hasher.update(if *value { [b't'] } else { [b'f'] }),
        Value::Number(value) => update_len_prefixed(hasher, b'#', value.to_string().as_bytes()),
        Value::String(value) => update_len_prefixed(hasher, b's', value.as_bytes()),
        Value::Array(items) => {
            hasher.update([b'[']);
            hasher.update((items.len() as u64).to_be_bytes());
            for item in items {
                update_canonical_json(hasher, item);
            }
            hasher.update([b']']);
        }
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            hasher.update([b'{']);
            hasher.update((keys.len() as u64).to_be_bytes());
            for key in keys {
                update_len_prefixed(hasher, b'k', key.as_bytes());
                if let Some(value) = object.get(key) {
                    update_canonical_json(hasher, value);
                }
            }
            hasher.update([b'}']);
        }
    }
}

fn update_named_json(hasher: &mut Sha256, name: &str, value: &Value) {
    update_len_prefixed(hasher, b'N', name.as_bytes());
    update_canonical_json(hasher, value);
}

fn update_named_json_sequence(hasher: &mut Sha256, name: &str, values: &[Value]) {
    update_len_prefixed(hasher, b'N', name.as_bytes());
    hasher.update([b'[']);
    hasher.update((values.len() as u64).to_be_bytes());
    for value in values {
        update_canonical_json(hasher, value);
    }
    hasher.update([b']']);
}

fn normalized_item_role(item: &Map<String, Value>) -> Option<String> {
    non_empty_str(item.get("role")).map(str::to_ascii_lowercase)
}

fn normalized_item_type(item: &Map<String, Value>) -> Option<String> {
    non_empty_str(item.get("type")).map(str::to_ascii_lowercase)
}

fn is_initial_client_prompt_item(value: &Value) -> bool {
    let Some(item) = value.as_object() else {
        return false;
    };
    if normalized_item_role(item)
        .as_deref()
        .is_some_and(|role| matches!(role, "system" | "developer" | "user"))
    {
        return true;
    }

    normalized_item_type(item)
        .as_deref()
        .is_some_and(|kind| matches!(kind, "input_text" | "input_image" | "input_file"))
}

fn is_user_prompt_item(value: &Value) -> bool {
    let Some(item) = value.as_object() else {
        return false;
    };
    if normalized_item_role(item).as_deref() == Some("user") {
        return true;
    }
    normalized_item_type(item)
        .as_deref()
        .is_some_and(|kind| matches!(kind, "input_text" | "input_image" | "input_file"))
}

fn value_is_present(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Array(values)) => !values.is_empty(),
        Some(Value::Object(values)) => !values.is_empty(),
        Some(Value::Bool(_)) | Some(Value::Number(_)) => true,
    }
}

fn has_explicit_prompt_cache_key(provider_request_body: &Value) -> bool {
    match provider_request_body.get("prompt_cache_key") {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(_) => true,
    }
}

fn contains_cache_control(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                if object.contains_key("cache_control") {
                    return true;
                }
                pending.extend(object.values());
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    false
}

/// Derives a cache cohort from the root prompt segment of an input array. On a
/// first turn this is the complete input; on append-only history it is the
/// leading client prompt segment before the first assistant/tool/generated
/// item. A `previous_response_id` continuation is excluded because its root
/// prefix is absent from the body.
fn responses_request_references_external_state(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        value_is_present(object.get("previous_response_id"))
            || value_is_present(object.get("conversation"))
    })
}

fn extract_responses_content_prefix_seed(provider_request_body: &Value) -> Option<String> {
    let object = provider_request_body.as_object()?;
    if responses_request_references_external_state(provider_request_body) {
        return None;
    }

    let model = object
        .get("model")
        .filter(|value| non_empty_str(Some(value)).is_some())?;
    let mut hasher = Sha256::new();
    update_len_prefixed(&mut hasher, b'V', b"content-prefix-v1");
    update_named_json(&mut hasher, "model", model);
    for key in ["instructions", "tools", "functions"] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            update_named_json(&mut hasher, key, value);
        }
    }

    if let Some(input_text) = object
        .get("input")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        update_len_prefixed(&mut hasher, b'N', b"initial_input");
        update_len_prefixed(&mut hasher, b's', input_text.as_bytes());
        return Some(format!("content_prefix:{}", sha256_hex_from_hasher(hasher)));
    }

    let input = object.get("input")?.as_array()?;
    let prefix_len = input
        .iter()
        .take_while(|item| is_initial_client_prompt_item(item))
        .count();
    if prefix_len == 0 {
        return None;
    }
    let initial_prefix = &input[..prefix_len];
    if !initial_prefix.iter().any(is_user_prompt_item) {
        return None;
    }

    update_named_json_sequence(&mut hasher, "initial_input", initial_prefix);
    Some(format!("content_prefix:{}", sha256_hex_from_hasher(hasher)))
}

fn cohort_from_seed(
    source: OpenAiResponsesPromptCacheKeySource,
    seed: &str,
) -> Option<ResolvedPromptCacheCohort> {
    let prompt_cache_key = build_stable_prompt_cache_key_from_seed(source.as_str(), seed)?;
    Some(ResolvedPromptCacheCohort {
        prompt_cache_key,
        source,
    })
}

fn resolve_prompt_cache_cohort(
    provider_request_body: &Value,
    explicit_session_key: Option<&str>,
    source_request_body: Option<&Value>,
) -> Option<ResolvedPromptCacheCohort> {
    if has_explicit_prompt_cache_key(provider_request_body)
        || contains_cache_control(provider_request_body)
    {
        return None;
    }

    explicit_session_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("resolved:{value}"))
        .or_else(|| extract_responses_prompt_cache_session_seed(provider_request_body))
        .or_else(|| source_request_body.and_then(extract_responses_prompt_cache_session_seed))
        .and_then(|seed| cohort_from_seed(OpenAiResponsesPromptCacheKeySource::Session, &seed))
        .or_else(|| {
            if source_request_body.is_some_and(responses_request_references_external_state) {
                return None;
            }
            extract_responses_content_prefix_seed(provider_request_body).and_then(|seed| {
                cohort_from_seed(OpenAiResponsesPromptCacheKeySource::ContentPrefix, &seed)
            })
        })
}

fn resolve_openai_responses_prompt_cache_cohort(
    provider_request_body: &Value,
    api_format: &str,
    explicit_session_key: Option<&str>,
    source_request_body: Option<&Value>,
) -> Option<ResolvedPromptCacheCohort> {
    if !crate::is_openai_responses_family_format(api_format) {
        return None;
    }
    resolve_prompt_cache_cohort(
        provider_request_body,
        explicit_session_key,
        source_request_body,
    )
}

/// Adds a stable cache-routing key to an OpenAI Responses wire request when
/// the client did not provide one.
///
/// The target wire format is the capability boundary. No User-Agent, client
/// family, provider type, endpoint opt-in, or model-name heuristic is used.
/// Explicit keys and body rules always take precedence.
///
/// No Aether user or API-key identity participates in the derived key. When
/// no explicit session signal exists, a key is synthesized only from a stable
/// prompt prefix proven by append-only history in the request body.
/// `source_request_body` preserves external-state markers that a provider
/// normalizer may intentionally remove from the final wire body.
pub fn apply_openai_responses_stable_prompt_cache_key(
    provider_request_body: &mut Value,
    provider_api_format: &str,
    body_rules: Option<&Value>,
    explicit_session_key: Option<&str>,
    source_request_body: Option<&Value>,
) -> Option<OpenAiResponsesPromptCacheKeySource> {
    if !crate::is_openai_responses_family_format(provider_api_format)
        || body_rules_handle_path(body_rules, "prompt_cache_key")
    {
        return None;
    }
    if has_explicit_prompt_cache_key(provider_request_body) {
        return None;
    }

    let cohort = resolve_openai_responses_prompt_cache_cohort(
        provider_request_body,
        provider_api_format,
        explicit_session_key,
        source_request_body,
    )?;
    provider_request_body.as_object_mut()?.insert(
        "prompt_cache_key".to_string(),
        Value::String(cohort.prompt_cache_key.clone()),
    );
    Some(cohort.source)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_openai_responses_stable_prompt_cache_key,
        resolve_openai_responses_prompt_cache_cohort, OpenAiResponsesPromptCacheKeySource,
    };
    use serde_json::{json, Value};

    fn continued_request(task: &str) -> Value {
        json!({
            "model": "gpt-5.6-terra",
            "instructions": "You are Codex. ",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": task}]
            }, {
                "role": "user",
                "content": "stable client capability reminder"
            }, {
                "type": "function_call",
                "call_id": "call-1",
                "name": "shell",
                "arguments": "{\"cmd\":\"pwd\"}"
            }, {
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "/workspace"
            }],
            "tools": [{
                "type": "function",
                "name": "shell",
                "parameters": {"type": "object", "properties": {}}
            }],
            "stream": true,
            "store": false
        })
    }

    #[test]
    fn append_only_history_reuses_one_content_prefix_cohort() {
        let mut later = continued_request("inspect the repository");
        let mut first = later.clone();
        first["input"]
            .as_array_mut()
            .expect("input should be an array")
            .truncate(2);
        later["input"]
            .as_array_mut()
            .expect("input should be an array")
            .extend([
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "done"}]
                }),
                json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "continue"}]
                }),
            ]);

        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut first,
                "openai:responses",
                None,
                None,
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::ContentPrefix)
        );
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut later,
                "openai:responses",
                None,
                None,
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::ContentPrefix)
        );
        assert_eq!(first["prompt_cache_key"], later["prompt_cache_key"]);
    }

    #[test]
    fn complete_long_root_prompt_distinguishes_parallel_sessions() {
        let shared_prefix = "shared environment context ".repeat(300);
        assert!(shared_prefix.len() > 4096);
        let mut session_a = continued_request(&format!("{shared_prefix}task A"));
        let mut session_b = continued_request(&format!("{shared_prefix}task B"));

        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut session_a,
                "openai:responses",
                None,
                None,
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::ContentPrefix)
        );
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut session_b,
                "openai:responses",
                None,
                None,
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::ContentPrefix)
        );
        assert_ne!(session_a["prompt_cache_key"], session_b["prompt_cache_key"]);
    }

    #[test]
    fn identical_prompt_prefixes_intentionally_share_a_cache_cohort() {
        let first = continued_request("same reusable prompt");
        let second = first.clone();
        let first_cohort =
            resolve_openai_responses_prompt_cache_cohort(&first, "openai:responses", None, None)
                .expect("continued history should have a content-prefix cohort");
        let second_cohort =
            resolve_openai_responses_prompt_cache_cohort(&second, "openai:responses", None, None)
                .expect("continued history should have a content-prefix cohort");

        assert_eq!(first_cohort, second_cohort);
        assert_eq!(
            first_cohort.source,
            OpenAiResponsesPromptCacheKeySource::ContentPrefix
        );
    }

    #[test]
    fn explicit_session_identity_wins_over_request_content() {
        let mut first = json!({"model": "model-a", "input": "first prompt"});
        let mut second = json!({"model": "model-a", "input": "different prompt"});

        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut first,
                "openai:responses",
                None,
                Some("session=conversation-1"),
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::Session)
        );
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut second,
                "openai:responses",
                None,
                Some("session=conversation-1"),
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::Session)
        );
        assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
    }

    #[test]
    fn metadata_and_conversation_ids_are_supported_without_client_detection() {
        let direct = json!({
            "model": "model-a",
            "input": "hello",
            "conversation": {"id": "conversation-1"}
        });
        let encoded = json!({
            "model": "model-a",
            "input": "different",
            "client_metadata": {
                "turn_metadata": "{\"thread_id\":\"thread-1\"}"
            }
        });

        for body in [direct, encoded] {
            let cohort =
                resolve_openai_responses_prompt_cache_cohort(&body, "openai:responses", None, None)
                    .expect("explicit request identity should be supported");
            assert_eq!(cohort.source, OpenAiResponsesPromptCacheKeySource::Session);
        }
    }

    #[test]
    fn ambiguous_stateless_requests_do_not_receive_a_coarse_fallback() {
        let body = json!({
            "model": "model-a",
            "previous_response_id": "resp_previous",
            "input": [{
                "role": "user",
                "content": "current turn"
            }, {
                "type": "function_call",
                "call_id": "call-current",
                "name": "tool",
                "arguments": "{}"
            }]
        });
        assert!(resolve_openai_responses_prompt_cache_cohort(
            &body,
            "openai:responses",
            None,
            None,
        )
        .is_none());
    }

    #[test]
    fn normalized_continuation_uses_source_state_without_guessing_a_root() {
        let source = json!({
            "model": "model-a",
            "previous_response_id": "resp_previous",
            "input": [{"role": "user", "content": "current turn"}]
        });
        let mut normalized = json!({
            "model": "model-a",
            "input": [{"role": "user", "content": "current turn"}]
        });

        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut normalized,
                "openai:responses",
                None,
                None,
                Some(&source),
            ),
            None
        );
        assert!(normalized.get("prompt_cache_key").is_none());

        let mut source_with_session = source;
        source_with_session["metadata"] = json!({"session_id": "conversation-1"});
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut normalized,
                "openai:responses",
                None,
                None,
                Some(&source_with_session),
            ),
            Some(OpenAiResponsesPromptCacheKeySource::Session)
        );
    }

    #[test]
    fn incomplete_content_does_not_create_a_coarse_cache_key() {
        for mut body in [
            json!({"input": "missing model"}),
            json!({
                "model": "model-a",
                "input": [{"type": "function_call", "name": "tool", "arguments": "{}"}]
            }),
        ] {
            assert_eq!(
                apply_openai_responses_stable_prompt_cache_key(
                    &mut body,
                    "openai:responses",
                    None,
                    None,
                    None,
                ),
                None
            );
            assert!(body.get("prompt_cache_key").is_none());
        }
    }

    #[test]
    fn one_shot_string_input_uses_a_content_cohort_without_session_guessing() {
        let body = json!({
            "model": "model-a",
            "input": "one shot prompt"
        });
        let cohort =
            resolve_openai_responses_prompt_cache_cohort(&body, "openai:responses", None, None)
                .expect("a complete string prompt is a valid content cohort");
        assert_eq!(
            cohort.source,
            OpenAiResponsesPromptCacheKeySource::ContentPrefix
        );
    }

    #[test]
    fn cache_control_requests_remain_owned_by_the_existing_bridge() {
        let mut body = continued_request("cache-controlled request");
        body["instructions"] = json!({
            "text": "stable system brief",
            "cache_control": {"type": "ephemeral"}
        });

        assert!(resolve_openai_responses_prompt_cache_cohort(
            &body,
            "openai:responses",
            None,
            None,
        )
        .is_none());
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut body,
                "openai:responses",
                None,
                Some("session=explicit"),
                None,
            ),
            None
        );
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn explicit_key_body_rules_and_non_responses_are_untouched() {
        let mut existing = continued_request("hello");
        existing["prompt_cache_key"] = json!("client-owned-key");
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut existing,
                "openai:responses",
                None,
                None,
                None,
            ),
            None
        );
        assert_eq!(existing["prompt_cache_key"], json!("client-owned-key"));

        let mut invalid_but_client_owned = continued_request("hello");
        invalid_but_client_owned["prompt_cache_key"] = json!({"unexpected": true});
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut invalid_but_client_owned,
                "openai:responses",
                None,
                None,
                None,
            ),
            None
        );
        assert_eq!(
            invalid_but_client_owned["prompt_cache_key"],
            json!({"unexpected": true})
        );

        let body_rules = json!([{"action": "drop", "path": "prompt_cache_key"}]);
        let mut rule_owned = continued_request("hello");
        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut rule_owned,
                "openai:responses",
                Some(&body_rules),
                None,
                None,
            ),
            None
        );
        assert!(rule_owned.get("prompt_cache_key").is_none());

        for format in [
            "openai:chat",
            "claude:messages",
            "gemini:generate_content",
            "openai:image",
            "openai:search",
        ] {
            let mut body = continued_request("hello");
            assert_eq!(
                apply_openai_responses_stable_prompt_cache_key(&mut body, format, None, None, None,),
                None,
                "unexpected cache-key mutation for {format}"
            );
            assert!(body.get("prompt_cache_key").is_none());
        }
    }

    #[test]
    fn compact_wire_body_uses_its_own_content_cohort() {
        let mut compact_wire_body = json!({
            "model": "provider-model",
            "input": [{"role": "user", "content": "normalized compact payload"}]
        });

        assert_eq!(
            apply_openai_responses_stable_prompt_cache_key(
                &mut compact_wire_body,
                "openai:responses:compact",
                None,
                None,
                None,
            ),
            Some(OpenAiResponsesPromptCacheKeySource::ContentPrefix)
        );
        assert!(compact_wire_body["prompt_cache_key"].is_string());
    }
}
