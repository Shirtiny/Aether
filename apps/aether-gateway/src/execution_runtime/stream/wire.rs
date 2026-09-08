//! Bounded protocol inspection for a known public text-stream endpoint. Media
//! type is a hint, not proof that an HTTP 200 body is safe to pass through.
use std::collections::BTreeMap;
use std::io::Error as IoError;

use aether_contracts::{StreamFrame, StreamFramePayload, StreamFrameType};
use async_stream::stream;
use axum::body::Bytes;
use futures_util::{stream::BoxStream, StreamExt};
use serde_json::Value;

use super::overload_retry::{data_bytes, data_frame};
use crate::ai_serving::api::openai_stream_terminal_error_body;
use crate::execution_runtime::ndjson::{decode_stream_frame_ndjson, encode_stream_frame_ndjson};
use crate::execution_runtime::submission::{
    has_nested_error, resolve_local_sync_error_status_code,
};

const MAX_PROBE_BYTES: usize = 16 * 1024;
const MAX_PROBE_FRAMES: usize = 128;
type Frames = BoxStream<'static, Result<Bytes, IoError>>;

pub(crate) fn public_text_stream_format(format: &str) -> bool {
    matches!(
        format,
        "openai:responses" | "openai:chat" | "claude:messages"
    )
}

fn set_media_type(headers: &mut BTreeMap<String, String>, value: &str) {
    headers.retain(|key, _| {
        !["content-type", "content-length", "content-encoding"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
    });
    headers.insert("content-type".into(), value.into());
}

fn header_frame(status_code: u16, headers: BTreeMap<String, String>) -> Result<Bytes, IoError> {
    encode_stream_frame_ndjson(&StreamFrame {
        frame_type: StreamFrameType::Headers,
        payload: StreamFramePayload::Headers {
            status_code,
            headers,
        },
    })
}

enum WirePrefix {
    Pending,
    Sse,
    Json,
    Other,
}

// Preserve the existing complete-JSON fallback for Content-Length responses,
// but only after their body has positively identified itself as JSON. A mislabeled
// SSE stream must never enter this whole-JSON path.
async fn collect_known_json(
    held: Vec<Bytes>,
    frames: &mut Frames,
) -> Result<(Vec<u8>, Option<Bytes>, Option<Bytes>), IoError> {
    let mut body = Vec::new();
    let mut telemetry = None;
    let mut terminal = None;
    let mut source = futures_util::stream::iter(held.into_iter().map(Ok)).chain(frames);
    while let Some(raw) = source.next().await {
        let raw = raw?;
        let frame =
            decode_stream_frame_ndjson(&raw).map_err(|err| IoError::other(format!("{err:?}")))?;
        if let Some(bytes) = data_bytes(&frame)? {
            body.extend(bytes);
        }
        match frame.payload {
            StreamFramePayload::Telemetry { .. } => telemetry = Some(raw),
            StreamFramePayload::Eof { .. } | StreamFramePayload::Error { .. } => {
                terminal = Some(raw);
                break;
            }
            _ => (),
        }
    }
    Ok((body, telemetry, terminal))
}

fn wire_prefix(bytes: &[u8]) -> WirePrefix {
    let bytes = bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(bytes)
        .trim_ascii_start();
    if bytes.is_empty() || b"\xef\xbb\xbf".starts_with(bytes) {
        return WirePrefix::Pending;
    }
    if matches!(bytes.first(), Some(b'{' | b'[')) {
        return WirePrefix::Json;
    }
    let prefixes: [&[u8]; 5] = [b"data:", b"event:", b":", b"id:", b"retry:"];
    if prefixes.iter().any(|prefix| bytes.starts_with(prefix)) {
        return WirePrefix::Sse;
    }
    if prefixes.iter().any(|prefix| prefix.starts_with(bytes)) {
        return WirePrefix::Pending;
    }
    WirePrefix::Other
}

/// `frames` is one encoded execution frame per item, not raw provider chunks.
/// Once the opening is resolved the remainder is byte-for-byte passthrough.
pub(super) fn inspect_stream_wire(mut frames: Frames, enabled: bool) -> Frames {
    if !enabled {
        return frames;
    }
    stream! {
        let Some(first) = frames.next().await else { return; };
        let raw = match first { Ok(raw) => raw, Err(err) => { yield Err(err); return; } };
        let frame = match decode_stream_frame_ndjson(&raw) {
            Ok(frame) => frame, Err(err) => { yield Err(IoError::other(format!("{err:?}"))); return; }
        };
        let StreamFramePayload::Headers { status_code, mut headers } = frame.payload else {
            yield Ok(raw); while let Some(item) = frames.next().await { yield item; } return;
        };
        if !(200..300).contains(&status_code) {
            yield Ok(raw); while let Some(item) = frames.next().await { yield item; } return;
        }
        let mut held = Vec::new();
        let mut probe = Vec::new();
        let mut status = status_code;
        let mut error_json = None;
        let mut truncated = false;
        let known_length = headers.iter().any(|(key,value)| key.eq_ignore_ascii_case("content-length") && value.parse::<u64>().is_ok());
        while probe.len() < MAX_PROBE_BYTES && held.len() < MAX_PROBE_FRAMES {
            let Some(item) = frames.next().await else {
                // An empty/truncated SSE opening must still have a usable error
                // channel; the existing terminal observer decides success/failure.
                if matches!(wire_prefix(&probe), WirePrefix::Pending | WirePrefix::Sse) {
                    set_media_type(&mut headers, "text/event-stream");
                }
                break;
            };
            let raw = match item { Ok(raw) => raw, Err(err) => {
                if matches!(wire_prefix(&probe), WirePrefix::Pending | WirePrefix::Sse) { set_media_type(&mut headers, "text/event-stream"); }
                yield header_frame(status, headers);
                for item in held { yield Ok(item); }
                yield Err(err); return;
            } };
            let frame = match decode_stream_frame_ndjson(&raw) { Ok(frame) => frame, Err(err) => { yield Err(IoError::other(format!("{err:?}"))); return; } };
            if let Some(bytes) = match data_bytes(&frame) { Ok(bytes) => bytes, Err(err) => { yield Err(err); return; } } {
                let remaining = MAX_PROBE_BYTES.saturating_sub(probe.len());
                truncated |= bytes.len() > remaining;
                probe.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
            }
            let terminal = matches!(frame.payload, StreamFramePayload::Error { .. } | StreamFramePayload::Eof { .. });
            held.push(raw);
            match wire_prefix(&probe) {
                WirePrefix::Sse => { set_media_type(&mut headers, "text/event-stream"); break; }
                WirePrefix::Json => {
                    set_media_type(&mut headers, "application/json");
                    if known_length && (truncated || probe.len() >= MAX_PROBE_BYTES) {
                        let (body, telemetry, terminal) = match collect_known_json(held, &mut frames).await {
                            Ok(result) => result,
                            Err(err) => { yield header_frame(status, headers); yield Err(err); return; }
                        };
                        let error = serde_json::from_slice::<Value>(&body).ok().and_then(|value|
                            openai_stream_terminal_error_body(&value).or_else(|| has_nested_error(&value).then_some(value)));
                        if let Some(error) = error {
                            status = resolve_local_sync_error_status_code(status, &error);
                            yield header_frame(status, headers);
                            if let Some(telemetry) = telemetry { yield Ok(telemetry); }
                            yield data_frame(error.to_string().as_bytes());
                            yield encode_stream_frame_ndjson(&StreamFrame::eof_with_summary(None));
                        } else {
                            yield header_frame(status, headers);
                            if let Some(telemetry) = telemetry { yield Ok(telemetry); }
                            yield data_frame(&body);
                            if let Some(terminal) = terminal { yield Ok(terminal); }
                        }
                        return;
                    }
                    if truncated { break; }
                    let stripped = probe.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&probe).trim_ascii();
                    if let Ok(value) = serde_json::from_slice::<Value>(stripped) {
                        if let Some(error) = openai_stream_terminal_error_body(&value).or_else(|| has_nested_error(&value).then_some(value)) {
                            status = resolve_local_sync_error_status_code(status, &error);
                            error_json = Some(error);
                        }
                        break;
                    }
                }
                WirePrefix::Other => break,
                WirePrefix::Pending if terminal => { set_media_type(&mut headers, "text/event-stream"); break; }
                WirePrefix::Pending => (),
            }
            if terminal { break; }
        }
        yield header_frame(status, headers);
        if let Some(error) = error_json {
            // An explicit JSON error is complete: do not wait for a server that
            // keeps its chunked connection open after rejecting the request.
            yield data_frame(error.to_string().as_bytes());
            yield encode_stream_frame_ndjson(&StreamFrame::eof_with_summary(None));
            return;
        }
        for item in held { yield Ok(item); }
        while let Some(item) = frames.next().await { yield item; }
    }.boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    fn input(content_type: Option<&str>, chunks: Vec<Bytes>) -> Frames {
        let headers = content_type
            .map(|value| BTreeMap::from([("content-type".into(), value.into())]))
            .unwrap_or_default();
        let mut frames = vec![header_frame(200, headers)];
        frames.extend(chunks.iter().map(|chunk| data_frame(chunk)));
        frames.push(encode_stream_frame_ndjson(&StreamFrame::eof_with_summary(
            None,
        )));
        futures_util::stream::iter(frames).boxed()
    }

    #[tokio::test]
    async fn overload_retry_wire_sniffs_missing_and_misleading_sse_headers() {
        let body = b"\xef\xbb\xbfevent: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{\"output\":[]}}\r\n\r\n";
        for media in [
            None,
            Some("application/octet-stream"),
            Some("application/json"),
            Some("text/plain"),
            Some("text/event-stream"),
        ] {
            let mut frames = inspect_stream_wire(
                input(
                    media,
                    body.iter()
                        .map(|byte| Bytes::copy_from_slice(&[*byte]))
                        .collect(),
                ),
                true,
            );
            let first = decode_stream_frame_ndjson(&frames.next().await.unwrap().unwrap()).unwrap();
            let StreamFramePayload::Headers {
                status_code,
                headers,
            } = first.payload
            else {
                panic!()
            };
            assert_eq!(status_code, 200);
            assert_eq!(
                headers.get("content-type").map(String::as_str),
                Some("text/event-stream")
            );
            let mut returned = Vec::new();
            while let Some(raw) = frames.next().await {
                if let Some(bytes) =
                    data_bytes(&decode_stream_frame_ndjson(&raw.unwrap()).unwrap()).unwrap()
                {
                    returned.extend(bytes);
                }
            }
            assert_eq!(returned, body);
        }
    }

    #[tokio::test]
    async fn overload_retry_wire_maps_json_error_before_waiting_for_eof() {
        for media in [None, Some("application/json"), Some("text/event-stream")] {
            let initial = input(media, vec![Bytes::from_static(br#"{"error":{"code":503,"message":"Our servers are currently overloaded. Please try again later."}}"#)]);
            let mut input = initial
                .take(2)
                .chain(futures_util::stream::pending())
                .boxed();
            let mut frames = inspect_stream_wire(input, true);
            let result = tokio::time::timeout(Duration::from_millis(100), async {
                let header =
                    decode_stream_frame_ndjson(&frames.next().await.unwrap().unwrap()).unwrap();
                assert!(matches!(
                    header.payload,
                    StreamFramePayload::Headers {
                        status_code: 503,
                        ..
                    }
                ));
                let error =
                    decode_stream_frame_ndjson(&frames.next().await.unwrap().unwrap()).unwrap();
                assert!(String::from_utf8(data_bytes(&error).unwrap().unwrap())
                    .unwrap()
                    .contains("currently overloaded"));
                assert!(frames.next().await.is_some());
                assert!(frames.next().await.is_none());
            })
            .await;
            assert!(result.is_ok(), "complete errors must not await server EOF");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retry_wire_is_applied_again_on_reopen_and_keeps_first_content_immediate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let first = inspect_stream_wire(input(None, vec![Bytes::from_static(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"hidden\",\"output\":[]}}\n\n"), Bytes::from_static(b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":503,\"message\":\"Our servers are currently overloaded. Please try again later.\"}}}\n\n")]), true);
        let mut result = super::super::overload_retry::retry_opening_stream(
            first,
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok(inspect_stream_wire(input(Some("application/octet-stream"), vec![Bytes::from_static(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n")]), true))
                }
            },
            "wire-retry".into(),
            "openai:responses".into(),
        );
        let mut data = Vec::new();
        while let Some(raw) = result.next().await {
            if let Some(bytes) =
                data_bytes(&decode_stream_frame_ndjson(&raw.unwrap()).unwrap()).unwrap()
            {
                data.extend(bytes);
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let text = String::from_utf8(data).unwrap();
        assert!(text.contains("hello"));
        assert!(!text.contains("hidden"));
        assert!(!text.contains("overloaded"));
    }

    #[tokio::test]
    async fn overload_retry_wire_preserves_unknown_binary_and_unselected_routes() {
        for enabled in [true, false] {
            let body = Bytes::from_static(b"\x89PNG\r\n\x1a\nnot SSE");
            let mut frames = inspect_stream_wire(
                input(Some("application/octet-stream"), vec![body.clone()]),
                enabled,
            );
            let header =
                decode_stream_frame_ndjson(&frames.next().await.unwrap().unwrap()).unwrap();
            let StreamFramePayload::Headers { headers, .. } = header.payload else {
                panic!()
            };
            assert_eq!(headers["content-type"], "application/octet-stream");
            let data = decode_stream_frame_ndjson(&frames.next().await.unwrap().unwrap()).unwrap();
            assert_eq!(data_bytes(&data).unwrap().unwrap(), body);
        }
        assert!(!public_text_stream_format("openai:image"));
        assert!(!public_text_stream_format("gemini:generate_content"));
    }

    #[tokio::test]
    async fn overload_retry_wire_empty_stream_has_a_terminal_error_channel() {
        let mut frames = inspect_stream_wire(input(None, vec![]), true);
        let header = decode_stream_frame_ndjson(&frames.next().await.unwrap().unwrap()).unwrap();
        let StreamFramePayload::Headers { headers, .. } = header.payload else {
            panic!()
        };
        assert_eq!(headers["content-type"], "text/event-stream");
    }
}
