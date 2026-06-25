use std::collections::BTreeMap;

use aether_contracts::{StreamFrame, StreamFramePayload};
use axum::http::StatusCode;
use base64::Engine as _;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::warn;

use crate::execution_runtime::ndjson::decode_stream_frame_ndjson;
use crate::execution_runtime::submission::{has_nested_error, strip_utf8_bom_and_ws};
use crate::GatewayError;
use crate::MAX_ERROR_BODY_BYTES;
use aether_ai_formats::api::openai_stream_terminal_error_body;

#[derive(Debug)]
pub(super) enum StreamPrefetchInspection {
    NeedMore,
    NonError,
    EmbeddedError(serde_json::Value),
}

pub(super) fn decode_stream_error_body(
    headers: &BTreeMap<String, String>,
    error_body: &[u8],
) -> (Option<serde_json::Value>, Option<String>) {
    if error_body.is_empty() {
        return (None, None);
    }

    let content_type = headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let looks_json = content_type.contains("json") || content_type.ends_with("+json");
    if looks_json {
        if let Ok(json_body) = serde_json::from_slice::<serde_json::Value>(error_body) {
            return (Some(json_body), None);
        }
    }

    (
        None,
        Some(base64::engine::general_purpose::STANDARD.encode(error_body)),
    )
}

fn header_value_case_insensitive<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn remove_header_case_insensitive(headers: &mut BTreeMap<String, String>, name: &str) {
    let keys = headers
        .keys()
        .filter(|key| key.eq_ignore_ascii_case(name))
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        headers.remove(&key);
    }
}

pub(super) fn should_synthesize_non_success_stream_error_body(
    status_code: u16,
    error_body: &[u8],
) -> bool {
    !(200..300).contains(&status_code)
        && ((300..400).contains(&status_code) || error_body.is_empty())
}

pub(super) fn build_synthetic_non_success_stream_error_body(
    status_code: u16,
    headers: &BTreeMap<String, String>,
) -> Value {
    let mut error = Map::from_iter([
        (
            "type".to_string(),
            Value::String("execution_runtime_non_success_status".to_string()),
        ),
        (
            "message".to_string(),
            Value::String(format!(
                "execution runtime stream returned non-success status {status_code}"
            )),
        ),
        ("code".to_string(), Value::from(status_code)),
        ("upstream_status".to_string(), Value::from(status_code)),
    ]);
    if let Some(location) = header_value_case_insensitive(headers, "location") {
        error.insert("location".to_string(), Value::String(location.to_string()));
    }

    Value::Object(Map::from_iter([(
        "error".to_string(),
        Value::Object(error),
    )]))
}

pub(super) fn synthetic_error_response_headers(
    mut headers: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    remove_header_case_insensitive(&mut headers, "content-encoding");
    remove_header_case_insensitive(&mut headers, "content-length");
    remove_header_case_insensitive(&mut headers, "content-type");
    remove_header_case_insensitive(&mut headers, "location");
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers
}

fn client_error_status_code_for_upstream_status(status_code: u16) -> u16 {
    if (300..400).contains(&status_code) || status_code < 200 {
        StatusCode::BAD_GATEWAY.as_u16()
    } else {
        status_code
    }
}

pub(super) fn stream_client_error_status_code_for_upstream_status(status_code: u16) -> u16 {
    client_error_status_code_for_upstream_status(status_code)
}

pub(super) fn inspect_prefetched_stream_body(
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> StreamPrefetchInspection {
    if body.is_empty() {
        return StreamPrefetchInspection::NeedMore;
    }

    let stripped = strip_utf8_bom_and_ws(body);
    let content_type = headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let looks_json = content_type.contains("json") || content_type.ends_with("+json");
    if looks_json || stripped.starts_with(b"{") || stripped.starts_with(b"[") {
        if let Ok(json_body) = serde_json::from_slice::<serde_json::Value>(stripped) {
            return if let Some(error_body) = stream_json_error_body(&json_body) {
                StreamPrefetchInspection::EmbeddedError(error_body)
            } else {
                StreamPrefetchInspection::NonError
            };
        }
    }

    let text = String::from_utf8_lossy(body);
    let mut current_event_type: Option<String> = None;
    let mut saw_meaningful_line = false;
    let mut saw_only_control_events = false;
    let mut saw_incomplete_json_line = false;
    for line in text.lines() {
        let line = line.trim_matches('\r').trim();
        if line.is_empty() || line.starts_with(':') {
            current_event_type = None;
            continue;
        }
        if let Some(event_type) = line.strip_prefix("event:") {
            current_event_type = Some(event_type.trim().to_string());
            continue;
        }

        let data_line = line.strip_prefix("data: ").unwrap_or(line).trim();
        if data_line.is_empty() {
            continue;
        }
        if data_line == "[DONE]" {
            return StreamPrefetchInspection::NonError;
        }

        saw_meaningful_line = true;
        match serde_json::from_str::<serde_json::Value>(data_line) {
            Ok(json_body) => {
                let json_body =
                    stream_json_body_with_event_type(json_body, current_event_type.as_deref());
                current_event_type = None;
                if let Some(error_body) = stream_json_error_body(&json_body) {
                    return StreamPrefetchInspection::EmbeddedError(error_body);
                }
                if stream_json_body_is_prefetch_control(&json_body) {
                    saw_only_control_events = true;
                    continue;
                }
                return StreamPrefetchInspection::NonError;
            }
            Err(_) => {
                if data_line.ends_with('}') || data_line.ends_with(']') {
                    return StreamPrefetchInspection::NonError;
                }
                if current_event_type
                    .as_deref()
                    .is_some_and(is_error_sse_event_type)
                {
                    return StreamPrefetchInspection::EmbeddedError(json!({
                        "error": {
                            "type": current_event_type.as_deref().unwrap_or("error"),
                            "message": data_line,
                        }
                    }));
                }
                saw_incomplete_json_line = true;
            }
        }
    }

    if saw_incomplete_json_line || saw_only_control_events {
        return StreamPrefetchInspection::NeedMore;
    }

    if saw_meaningful_line {
        StreamPrefetchInspection::NonError
    } else {
        StreamPrefetchInspection::NeedMore
    }
}

fn stream_json_body_with_event_type(mut value: Value, event_type: Option<&str>) -> Value {
    let Some(event_type) = event_type else {
        return value;
    };
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    if !object.contains_key("type") {
        object.insert("type".to_string(), Value::String(event_type.to_string()));
    }
    value
}

fn stream_json_error_body(value: &Value) -> Option<Value> {
    openai_stream_terminal_error_body(value)
        .or_else(|| has_nested_error(value).then(|| value.clone()))
}

fn stream_json_body_is_prefetch_control(value: &Value) -> bool {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(
        event_type,
        "response.created" | "response.in_progress" | "response.queued"
    ) {
        return true;
    }
    if openai_responses_structural_event_is_prefetch_control(value, event_type) {
        return true;
    }

    value
        .get("response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "created" | "in_progress" | "queued"))
}

fn openai_responses_structural_event_is_prefetch_control(value: &Value, event_type: &str) -> bool {
    match event_type {
        "response.output_item.added" => response_output_item_added_is_prefetch_control(value),
        "response.content_part.added" | "response.reasoning_summary_part.added" => {
            response_part_added_is_prefetch_control(value)
        }
        _ => false,
    }
}

fn response_output_item_added_is_prefetch_control(value: &Value) -> bool {
    let Some(item) = value.get("item").and_then(Value::as_object) else {
        return false;
    };
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "reasoning" => true,
        "message" => !message_item_has_visible_content(item),
        "function_call" => !non_empty_json_string(item.get("arguments")),
        "image_generation_call" => {
            !non_empty_json_string(item.get("result"))
                && !non_empty_json_string(item.get("partial_image_b64"))
        }
        _ => false,
    }
}

fn response_part_added_is_prefetch_control(value: &Value) -> bool {
    let Some(part) = value.get("part").and_then(Value::as_object) else {
        return false;
    };
    match part.get("type").and_then(Value::as_str).unwrap_or_default() {
        "output_text" | "summary_text" => !non_empty_json_string(part.get("text")),
        _ => false,
    }
}

fn message_item_has_visible_content(item: &Map<String, Value>) -> bool {
    item.get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|part| {
                non_empty_json_string(part.get("text"))
                    || non_empty_json_string(part.get("refusal"))
                    || part
                        .get("annotations")
                        .and_then(Value::as_array)
                        .is_some_and(|annotations| !annotations.is_empty())
            })
        })
}

fn non_empty_json_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

fn is_error_sse_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "error" | "response.failed" | "response.incomplete"
    )
}

pub(super) async fn collect_error_body<R>(
    lines: &mut FramedRead<R, LinesCodec>,
) -> Result<Vec<u8>, GatewayError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut body = Vec::new();
    while let Some(frame) = read_next_frame(lines).await? {
        match frame.payload {
            StreamFramePayload::Data { chunk_b64, text } => {
                let chunk = if let Some(chunk_b64) = chunk_b64 {
                    base64::engine::general_purpose::STANDARD
                        .decode(chunk_b64)
                        .map_err(|err| GatewayError::Internal(err.to_string()))?
                } else {
                    text.unwrap_or_default().into_bytes()
                };
                body.extend_from_slice(&chunk);
                if body.len() >= MAX_ERROR_BODY_BYTES {
                    body.truncate(MAX_ERROR_BODY_BYTES);
                    break;
                }
            }
            StreamFramePayload::Telemetry { .. } => {}
            StreamFramePayload::Eof { .. } => break,
            StreamFramePayload::Error { error } => {
                warn!(error = %error.message, "execution runtime stream emitted error frame while collecting error body");
                break;
            }
            StreamFramePayload::Headers { .. } => {}
        }
    }
    Ok(body)
}

pub(super) async fn read_next_frame<R>(
    lines: &mut FramedRead<R, LinesCodec>,
) -> Result<Option<StreamFrame>, GatewayError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    while let Some(line) = lines.next().await {
        let line = line.map_err(|err| GatewayError::Internal(err.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let frame = decode_stream_frame_ndjson(line.as_bytes())?;
        return Ok(Some(frame));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{inspect_prefetched_stream_body, StreamPrefetchInspection};
    use std::collections::BTreeMap;

    fn event_stream_headers() -> BTreeMap<String, String> {
        BTreeMap::from([("content-type".to_string(), "text/event-stream".to_string())])
    }

    #[test]
    fn inspect_prefetched_stream_body_keeps_openai_response_control_events_pending() {
        let inspection = inspect_prefetched_stream_body(
            &event_stream_headers(),
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n"
            )
            .as_bytes(),
        );

        assert!(matches!(inspection, StreamPrefetchInspection::NeedMore));
    }

    #[test]
    fn inspect_prefetched_stream_body_detects_openai_response_failed_after_control_event() {
        let inspection = inspect_prefetched_stream_body(
            &event_stream_headers(),
            concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n",
                "event: response.failed\n",
                "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"503\",\"message\":\"Our servers are currently overloaded. Please try again later.\"}}}\n\n"
            )
            .as_bytes(),
        );

        let StreamPrefetchInspection::EmbeddedError(body) = inspection else {
            panic!("response.failed should be treated as an embedded error");
        };
        assert_eq!(body["error"]["code"], "503");
        assert_eq!(
            body["error"]["message"],
            "Our servers are currently overloaded. Please try again later."
        );
    }

    #[test]
    fn inspect_prefetched_stream_body_keeps_empty_responses_structure_pending() {
        let inspection = inspect_prefetched_stream_body(
            &event_stream_headers(),
            concat!(
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"status\":\"in_progress\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                "event: response.content_part.added\n",
                "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\",\"annotations\":[]}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"reasoning\",\"summary\":[],\"encrypted_content\":\"EWxvY2tlZA==\"}}\n\n"
            )
            .as_bytes(),
        );

        assert!(matches!(inspection, StreamPrefetchInspection::NeedMore));
    }

    #[test]
    fn inspect_prefetched_stream_body_detects_failed_after_empty_responses_structure() {
        let inspection = inspect_prefetched_stream_body(
            &event_stream_headers(),
            concat!(
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"status\":\"in_progress\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                "event: response.content_part.added\n",
                "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\",\"annotations\":[]}}\n\n",
                "event: response.failed\n",
                "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"503\",\"message\":\"Our servers are currently overloaded. Please try again later.\"}}}\n\n"
            )
            .as_bytes(),
        );

        assert!(matches!(
            inspection,
            StreamPrefetchInspection::EmbeddedError(_)
        ));
    }

    #[test]
    fn inspect_prefetched_stream_body_releases_visible_responses_text() {
        let inspection = inspect_prefetched_stream_body(
            &event_stream_headers(),
            concat!(
                "event: response.content_part.added\n",
                "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}}\n\n"
            )
            .as_bytes(),
        );

        assert!(matches!(inspection, StreamPrefetchInspection::NonError));
    }
}
