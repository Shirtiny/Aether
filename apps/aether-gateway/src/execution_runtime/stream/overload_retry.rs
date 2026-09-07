//! Protect the uncommitted opening of a direct SSE stream, including after the
//! HTTP response has been released. Never buffer or replay substantive output.
use std::future::Future;
use std::io::Error as IoError;

use aether_contracts::{StreamFrame, StreamFramePayload, StreamFrameType};
use async_stream::stream;
use axum::body::Bytes;
use base64::Engine as _;
use futures_util::{stream::BoxStream, StreamExt};
use serde_json::{json, Value};
use tracing::info;

use crate::ai_serving::api::openai_stream_terminal_error_body;
use crate::execution_runtime::ndjson::{decode_stream_frame_ndjson, encode_stream_frame_ndjson};
use crate::execution_runtime::overload_retry::OverloadRetry;
use crate::execution_runtime::submission::{
    has_nested_error, resolve_local_sync_error_status_code,
};

const MAX_OPENING_BYTES: usize = 256 * 1024;
const PENDING_COMMENT: &[u8] = b": aether-upstream-pending\n\n";

type Frames = BoxStream<'static, Result<Bytes, IoError>>;

fn data_frame(bytes: &[u8]) -> Result<Bytes, IoError> {
    encode_stream_frame_ndjson(&StreamFrame {
        frame_type: StreamFrameType::Data,
        payload: StreamFramePayload::Data {
            chunk_b64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            text: None,
        },
    })
}

fn data_bytes(frame: &StreamFrame) -> Result<Option<Vec<u8>>, IoError> {
    match &frame.payload {
        StreamFramePayload::Data { chunk_b64, text } => {
            if let Some(encoded) = chunk_b64 {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map(Some)
                    .map_err(IoError::other)
            } else {
                Ok(Some(
                    text.as_deref().unwrap_or_default().as_bytes().to_vec(),
                ))
            }
        }
        _ => Ok(None),
    }
}

/// Input comes from build_direct_execution_frame_stream: exactly one encoded
/// NDJSON frame per item (the provider's SSE records may span any number of items).
pub(super) fn retry_opening_stream<F, Fut>(
    mut frames: Frames,
    mut reopen: F,
    request_id: String,
) -> Frames
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<Frames, IoError>> + Send,
{
    stream! {
        let mut retry = OverloadRetry::default();
        let mut headers_sent = false;
        'attempt: loop {
            let Some(first) = frames.next().await else {
                return;
            };
            let raw = match first {
                Ok(raw) => raw,
                Err(err) => {
                    yield Err(err);
                    return;
                }
            };
            let header = match decode_stream_frame_ndjson(&raw) {
                Ok(frame) => frame,
                Err(err) => {
                    yield Err(IoError::other(format!("{err:?}")));
                    return;
                }
            };
            let StreamFramePayload::Headers {
                status_code,
                headers,
            } = header.payload
            else {
                yield Ok(raw);
                while let Some(item) = frames.next().await {
                    yield item;
                }
                return;
            };

            // A real HTTP 503 has not started a successful stream. Hold only its
            // small error body, so retrying cannot commit the failed status line.
            if status_code == 503 || (headers_sent && !(200..300).contains(&status_code)) {
                let mut held = vec![raw];
                let mut body = Vec::new();
                while body.len() < crate::MAX_ERROR_BODY_BYTES && held.len() < 128 {
                    let Some(item) = frames.next().await else {
                        break;
                    };
                    let item = match item {
                        Ok(item) => item,
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    };
                    let frame = match decode_stream_frame_ndjson(&item) {
                        Ok(frame) => frame,
                        Err(err) => {
                            yield Err(IoError::other(format!("{err:?}")));
                            return;
                        }
                    };
                    match data_bytes(&frame) {
                        Ok(Some(bytes)) => body.extend_from_slice(&bytes),
                        Ok(None) => (),
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    }
                    let done = matches!(
                        frame.payload,
                        StreamFramePayload::Eof { .. } | StreamFramePayload::Error { .. }
                    );
                    held.push(item);
                    if done {
                        break;
                    }
                }
                if retry
                    .wait(
                        &request_id,
                        status_code,
                        Some(&String::from_utf8_lossy(&body)),
                        &headers,
                    )
                    .await
                {
                    drop(frames);
                    frames = match reopen().await {
                        Ok(frames) => frames,
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    };
                    continue 'attempt;
                }
                if headers_sent {
                    // A retry can reject at the HTTP layer after the first
                    // attempt opened SSE. Surface one valid terminal SSE error,
                    // never insert another HTTP header frame into that response.
                    let mut error = serde_json::from_slice::<Value>(&body).unwrap_or_else(|_| {
                        json!({
                            "error": {"message": String::from_utf8_lossy(&body)}
                        })
                    });
                    if !error.get("error").is_some_and(Value::is_object) {
                        error = json!({"error": {"message": error.to_string()}});
                    }
                    error["error"]["code"] = json!(status_code);
                    yield data_frame(format!("event: error\ndata: {error}\n\n").as_bytes());
                    yield encode_stream_frame_ndjson(&StreamFrame::eof_with_summary(None));
                } else {
                    for item in held {
                        yield Ok(item);
                    }
                    while let Some(item) = frames.next().await {
                        yield item;
                    }
                }
                return;
            }

            let is_sse = headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    && value.to_ascii_lowercase().contains("text/event-stream")
            });
            if !headers_sent {
                yield Ok(raw);
                headers_sent = true;
            }
            if !(200..300).contains(&status_code) || !is_sse {
                if (200..300).contains(&status_code) {
                    retry.recovered(&request_id);
                }
                while let Some(item) = frames.next().await {
                    yield item;
                }
                return;
            }

            let mut opening = OpeningSse::default();
            let mut telemetry = None;
            let mut pending_sent = false;
            while let Some(item) = frames.next().await {
                let raw = match item {
                    Ok(raw) => raw,
                    Err(err) => {
                        yield Err(err);
                        return;
                    }
                };
                let frame = match decode_stream_frame_ndjson(&raw) {
                    Ok(frame) => frame,
                    Err(err) => {
                        yield Err(IoError::other(format!("{err:?}")));
                        return;
                    }
                };
                if matches!(frame.payload, StreamFramePayload::Telemetry { .. }) {
                    telemetry = Some(raw);
                    continue;
                }
                if let Some(chunk) = match data_bytes(&frame) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        yield Err(err);
                        return;
                    }
                } {
                    if chunk.is_empty() {
                        continue;
                    }
                    if opening.bytes.len().saturating_add(chunk.len()) > MAX_OPENING_BYTES {
                        info!(
                            event_name = "upstream_overload_opening_committed",
                            request_id,
                            reason = "buffer_limit",
                            "opening buffer bound reached; disabling replay"
                        );
                        if let Some(telemetry) = telemetry.take() {
                            yield Ok(telemetry);
                        }
                        if !opening.bytes.is_empty() {
                            yield data_frame(&opening.bytes);
                        }
                        yield Ok(raw);
                    } else {
                        let inspection = opening.push(&chunk);
                        if let OpeningInspection::Error(ref error) = inspection {
                            let status = resolve_local_sync_error_status_code(200, error);
                            if retry
                                .wait(&request_id, status, Some(&error.to_string()), &headers)
                                .await
                            {
                                drop(frames);
                                frames = match reopen().await {
                                    Ok(frames) => frames,
                                    Err(err) => {
                                        yield Err(err);
                                        return;
                                    }
                                };
                                continue 'attempt;
                            }
                        }
                        if matches!(inspection, OpeningInspection::Pending) {
                            // Comments carry no attempt/response IDs or content.
                            // The outer stream pump supplies periodic keepalives
                            // once its short prefetch window has elapsed.
                            if !pending_sent {
                                yield data_frame(PENDING_COMMENT);
                                pending_sent = true;
                            }
                            continue;
                        }
                        if matches!(inspection, OpeningInspection::Commit) {
                            retry.recovered(&request_id);
                        }
                        if let Some(telemetry) = telemetry.take() {
                            yield Ok(telemetry);
                        }
                        yield data_frame(&opening.bytes);
                    }
                } else {
                    // EOF/transport failure is not proof of an explicit capacity
                    // rejection. Preserve it and never replay a possibly run job.
                    if let Some(telemetry) = telemetry.take() {
                        yield Ok(telemetry);
                    }
                    if !opening.bytes.is_empty() {
                        yield data_frame(&opening.bytes);
                    }
                    yield Ok(raw);
                }
                // Commit once. The remainder stays byte-for-byte, with no JSON
                // parsing, copying, lookahead delay or mid-output replay.
                while let Some(item) = frames.next().await {
                    yield item;
                }
                return;
            }
            if let Some(telemetry) = telemetry {
                yield Ok(telemetry);
            }
            if !opening.bytes.is_empty() {
                yield data_frame(&opening.bytes);
            }
            return;
        }
    }
    .boxed()
}

#[derive(Default)]
struct OpeningSse {
    bytes: Vec<u8>,
    scanned: usize,
    line_start: usize,
    record_start: usize,
    saw_data_line: bool,
}

enum OpeningInspection {
    Pending,
    Commit,
    Error(Value),
}

impl OpeningSse {
    fn push(&mut self, chunk: &[u8]) -> OpeningInspection {
        self.bytes.extend_from_slice(chunk);
        while self.scanned < self.bytes.len() {
            let index = self.scanned;
            self.scanned += 1;
            if self.bytes[index] != b'\n' {
                continue;
            }
            let line = &self.bytes[self.line_start..index];
            let blank = line.iter().all(|byte| *byte == b'\r');
            let first_data_line = !self.saw_data_line && line.starts_with(b"data:");
            self.saw_data_line |= line.starts_with(b"data:");
            let complete_json_line =
                first_data_line && serde_json::from_slice::<Value>(&line[5..]).is_ok();
            self.line_start = index + 1;
            if !blank {
                // Preserve the existing low-latency path: content-bearing JSON
                // can commit on its data line, without waiting for the record's
                // trailing blank line in a later network chunk. Errors still
                // require a complete record; multiline/partial JSON stays held.
                if complete_json_line
                    && matches!(
                        inspect_record(&self.bytes[self.record_start..index + 1]),
                        OpeningInspection::Commit
                    )
                {
                    return OpeningInspection::Commit;
                }
                continue;
            }
            self.saw_data_line = false;
            let result = inspect_record(&self.bytes[self.record_start..index + 1]);
            self.record_start = index + 1;
            if !matches!(result, OpeningInspection::Pending) {
                return result;
            }
        }
        OpeningInspection::Pending
    }
}

fn inspect_record(record: &[u8]) -> OpeningInspection {
    let Ok(text) = std::str::from_utf8(record) else {
        return OpeningInspection::Commit;
    };
    let mut event = "";
    let mut data = String::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = value,
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            // Do not hide reconnect IDs, retries, or unknown extensions.
            _ => return OpeningInspection::Commit,
        }
    }
    if data.is_empty() {
        return OpeningInspection::Pending;
    }
    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
        return OpeningInspection::Commit;
    };
    if value.get("type").is_none() && !event.is_empty() {
        if let Some(object) = value.as_object_mut() {
            object.insert("type".into(), json!(event));
        }
    }
    if let Some(error) = openai_stream_terminal_error_body(&value)
        .or_else(|| has_nested_error(&value).then(|| value.clone()))
    {
        return OpeningInspection::Error(error);
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let empty_array =
        |value: Option<&Value>| value.is_none_or(|v| v.as_array().is_some_and(Vec::is_empty));
    let empty_text = |value: Option<&Value>| {
        value.is_none_or(|v| v.is_null() || v.as_str().is_some_and(str::is_empty))
    };
    let control = match kind {
        "response.created" | "response.in_progress" | "response.queued" => {
            empty_array(value.pointer("/response/output"))
        }
        "response.output_item.added" => match value.pointer("/item/type").and_then(Value::as_str) {
            Some("message") => empty_array(value.pointer("/item/content")),
            Some("reasoning") => {
                empty_array(value.pointer("/item/summary"))
                    && empty_array(value.pointer("/item/content"))
                    && empty_text(value.pointer("/item/encrypted_content"))
            }
            // Tool starts (even empty arguments) are a commit boundary.
            _ => false,
        },
        "response.content_part.added" | "response.reasoning_summary_part.added" => {
            matches!(
                value.pointer("/part/type").and_then(Value::as_str),
                Some("output_text" | "summary_text")
            ) && empty_text(value.pointer("/part/text"))
                && empty_text(value.pointer("/part/refusal"))
                && empty_array(value.pointer("/part/annotations"))
        }
        "message_start" => empty_array(value.pointer("/message/content")),
        "ping" => true,
        "" => value
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                !choices.is_empty()
                    && choices.iter().all(|choice| {
                        choice.get("finish_reason").is_none_or(Value::is_null)
                            && choice
                                .get("delta")
                                .and_then(Value::as_object)
                                .is_some_and(|delta| {
                                    delta.iter().all(|(key, value)| match key.as_str() {
                                        "role" => value.as_str() == Some("assistant"),
                                        "content" => empty_text(Some(value)),
                                        _ => false,
                                    })
                                })
                    })
            }),
        _ => false,
    };
    if control {
        OpeningInspection::Pending
    } else {
        OpeningInspection::Commit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    const OVERLOAD: &str = r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"503","message":"Our servers are currently overloaded. Please try again later."}}}"#;
    const CREATED: &str = r#"{"type":"response.created","response":{"id":"failed-attempt-id","output":[],"status":"in_progress"}}"#;
    const DELTA: &str = r#"{"type":"response.output_text.delta","delta":"hello"}"#;

    fn sse(value: &str) -> String {
        format!("data: {value}\n\n")
    }
    fn header(status: u16, content_type: &str) -> Result<Bytes, IoError> {
        encode_stream_frame_ndjson(&StreamFrame {
            frame_type: StreamFrameType::Headers,
            payload: StreamFramePayload::Headers {
                status_code: status,
                headers: BTreeMap::from([("content-type".into(), content_type.into())]),
            },
        })
    }
    fn attempt(status: u16, body: String) -> Frames {
        futures_util::stream::iter(vec![
            header(
                status,
                if status == 200 {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            ),
            data_frame(body.as_bytes()),
            encode_stream_frame_ndjson(&StreamFrame::eof_with_summary(None)),
        ])
        .boxed()
    }
    fn with_attempts(first: Frames, rest: Vec<Frames>) -> (Frames, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let mut rest = VecDeque::from(rest);
        let output = retry_opening_stream(
            first,
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let next = rest.pop_front();
                async move { next.ok_or_else(|| IoError::other("unexpected retry")) }
            },
            "test-overload".into(),
        );
        (output, calls)
    }
    async fn collect(mut frames: Frames) -> (Vec<u16>, String) {
        let mut statuses = Vec::new();
        let mut body = Vec::new();
        while let Some(raw) = frames.next().await {
            let frame = decode_stream_frame_ndjson(&raw.unwrap()).unwrap();
            if let StreamFramePayload::Headers { status_code, .. } = frame.payload {
                statuses.push(status_code);
            }
            if let Some(bytes) = data_bytes(&frame).unwrap() {
                body.extend_from_slice(&bytes);
            }
        }
        (statuses, String::from_utf8(body).unwrap())
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_releases_content_without_waiting_for_record_terminator() {
        let prefix = format!("event: response.output_text.delta\ndata: {DELTA}\n");
        let sent = prefix.clone();
        let first = stream! {
            yield header(200, "text/event-stream");
            yield data_frame(sent.as_bytes());
            futures_util::future::pending::<()>().await;
        }
        .boxed();
        let (mut output, calls) = with_attempts(first, vec![]);
        output.next().await.unwrap().unwrap();
        let started = tokio::time::Instant::now();
        let raw = tokio::time::timeout(Duration::from_millis(50), output.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let bytes = data_bytes(&decode_stream_frame_ndjson(&raw).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(bytes, prefix.as_bytes());
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_http_503_is_hidden_then_stream_succeeds() {
        let (output, calls) = with_attempts(
            attempt(503, r#"{"error":{"message":"Our servers are currently overloaded. Please try again later."}}"#.into()),
            vec![attempt(200, sse(DELTA))],
        );
        let (statuses, body) = collect(output).await;
        assert_eq!(statuses, [200]);
        assert_eq!(body, sse(DELTA));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_catches_late_pre_content_error_without_delaying_first_content() {
        let first = stream! {
            yield header(200, "text/event-stream");
            yield data_frame(sse(CREATED).as_bytes());
            tokio::time::sleep(Duration::from_secs(47)).await;
            yield data_frame(sse(OVERLOAD).as_bytes());
            panic!("failed stream must be dropped, not drained");
        }
        .boxed();
        let success = stream! {
            yield header(200, "text/event-stream");
            yield data_frame(sse(DELTA).as_bytes());
            futures_util::future::pending::<()>().await;
        }
        .boxed();
        let (mut output, calls) = with_attempts(first, vec![success]);
        let started = tokio::time::Instant::now();
        output.next().await.unwrap().unwrap(); // headers
        let pending = output.next().await.unwrap().unwrap();
        assert_eq!(
            data_bytes(&decode_stream_frame_ndjson(&pending).unwrap())
                .unwrap()
                .unwrap(),
            PENDING_COMMENT
        );
        assert_eq!(started.elapsed(), Duration::ZERO);
        let raw = tokio::time::timeout(Duration::from_secs(48), output.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let bytes = data_bytes(&decode_stream_frame_ndjson(&raw).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(bytes, sse(DELTA).as_bytes());
        assert!(started.elapsed() < Duration::from_millis(47_351));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_never_replays_text_reasoning_tools_or_unknown_events() {
        for content in [
            DELTA,
            r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#,
            r#"{"type":"response.output_item.added","item":{"type":"function_call","arguments":"","name":"run"}}"#,
            r#"{"type":"response.output_item.added","item":{"type":"web_search_call"}}"#,
            r#"{"type":"response.output_item.added","item":{"type":"reasoning","summary":[{"text":"thought"}]}}"#,
            r#"{"type":"unknown_event"}"#,
            r#"{"type":"content_block_start","content_block":{"type":"tool_use","id":"tool-1"}}"#,
        ] {
            let body = sse(CREATED) + &sse(content) + &sse(OVERLOAD);
            let (output, calls) = with_attempts(attempt(200, body.clone()), vec![]);
            let (_, returned) = collect(output).await;
            assert_eq!(returned, body);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{content}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_suppresses_attempt_ids_and_bounds_exhaustion() {
        let body = sse(CREATED) + &sse(OVERLOAD);
        let (output, calls) = with_attempts(
            attempt(200, body.clone()),
            vec![attempt(200, body.clone()), attempt(200, body)],
        );
        let (statuses, returned) = collect(output).await;
        assert_eq!(statuses, [200]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            returned.matches("failed-attempt-id").count(),
            1,
            "only the final failed attempt may be exposed"
        );
        assert_eq!(returned.matches("response.failed").count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_http_failure_after_sse_stays_a_valid_sse_error() {
        let (output, calls) = with_attempts(
            attempt(200, sse(CREATED) + &sse(OVERLOAD)),
            vec![attempt(
                401,
                r#"{"error":{"message":"unauthorized"}}"#.into(),
            )],
        );
        let (statuses, returned) = collect(output).await;
        assert_eq!(statuses, [200]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!returned.contains("failed-attempt-id"));
        assert!(returned.contains("event: error"));
        assert!(returned.contains("\"code\":401"));
    }

    #[test]
    fn overload_retry_sse_parser_handles_fragmentation_multiline_crlf_and_no_space() {
        let body = "event:response.created\r\ndata:{\"response\":{\"id\":\"中文\",\"output\":[]}}\r\n\r\nevent:response.failed\r\ndata:{\"response\":{\r\ndata:\"error\":{\"code\":503,\"message\":\"Our servers are currently overloaded. Please try again later.\"}}}\r\n\r\n";
        for size in 1..=body.len() {
            let mut opening = OpeningSse::default();
            let mut error = None;
            for chunk in body.as_bytes().chunks(size) {
                match opening.push(chunk) {
                    OpeningInspection::Pending => (),
                    OpeningInspection::Error(value) => error = Some(value),
                    OpeningInspection::Commit => panic!("unexpected commit with chunk size {size}"),
                }
            }
            let error = error.unwrap();
            assert_eq!(resolve_local_sync_error_status_code(200, &error), 503);
        }
    }

    #[test]
    fn overload_retry_only_buffers_known_empty_opening_events() {
        for record in [
            ": heartbeat\n\n".to_string(),
            sse(CREATED),
            sse(
                r#"{"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            ),
            sse(r#"{"type":"message_start","message":{"content":[]}}"#),
        ] {
            assert!(
                matches!(
                    inspect_record(record.as_bytes()),
                    OpeningInspection::Pending
                ),
                "{record}"
            );
        }
        for record in [
            "data: invalid json\n\n".to_string(),
            "id: response-1\ndata: {}\n\n".to_string(),
            sse(r#"{"choices":[{"delta":{"tool_calls":[{}]}}]}"#),
            sse(r#"{"choices":[{"delta":{"content":" "}}]}"#),
            sse(r#"{"type":"response.completed"}"#),
        ] {
            assert!(
                matches!(inspect_record(record.as_bytes()), OpeningInspection::Commit),
                "{record}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_buffer_limit_commits_without_losing_bytes() {
        let body = format!(": {}\n\n{}", "x".repeat(MAX_OPENING_BYTES), sse(OVERLOAD));
        let (output, calls) = with_attempts(attempt(200, body.clone()), vec![]);
        assert_eq!(collect(output).await.1, body);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_cancellation_during_backoff_never_dispatches_another_request() {
        let (output, calls) = with_attempts(attempt(503, r#"{"error":{"message":"Our servers are currently overloaded. Please try again later."}}"#.into()), vec![attempt(200, sse(DELTA))]);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), collect(output))
                .await
                .is_err()
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
