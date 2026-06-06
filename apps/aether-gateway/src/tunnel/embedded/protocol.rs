use bytes::Bytes;

pub use aether_contracts::tunnel::{
    decode_payload, encode_connection_close, encode_frame, encode_goaway, encode_goaway_v3,
    encode_hello, encode_load_report, encode_ping, encode_pong, encode_reset_stream,
    encode_settings, encode_stream_error, encode_window_update, frame_payload_by_header,
    ConnectionClosePayload, FrameHeader, GoAwayPayload, HelloPayload, LoadReportPayload,
    RequestMeta, ResetStreamPayload, ResponseMeta, SettingsPayload, WindowUpdatePayload,
    CONNECTION_CLOSE, FLAG_END_STREAM, FLAG_GZIP_COMPRESSED, GOAWAY, HEADER_SIZE, HEARTBEAT_ACK,
    HEARTBEAT_DATA, HELLO, LOAD_REPORT, PING, PONG, REQUEST_BODY, REQUEST_HEADERS, RESET_STREAM,
    RESPONSE_BODY, RESPONSE_HEADERS, SETTINGS, STREAM_END, STREAM_ERROR, WINDOW_UPDATE,
};

pub fn compress_payload(payload: &[u8]) -> Result<(Vec<u8>, u8), std::io::Error> {
    let (compressed, flags) =
        aether_contracts::tunnel::compress_payload(Bytes::copy_from_slice(payload));
    Ok((compressed.to_vec(), flags))
}

pub fn raw_payload(payload: &[u8]) -> (Vec<u8>, u8) {
    let (payload, flags) = aether_contracts::tunnel::raw_payload(Bytes::copy_from_slice(payload));
    (payload.to_vec(), flags)
}
