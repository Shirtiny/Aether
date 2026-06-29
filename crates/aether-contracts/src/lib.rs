mod error;
mod frame;
mod plan;
mod result;
pub mod tunnel;
pub mod tunnel_security;
mod usage;

pub use error::{ExecutionError, ExecutionErrorKind, ExecutionPhase};
pub use frame::{StreamFrame, StreamFramePayload, StreamFrameType};
pub use plan::{
    codex_default_tls_fingerprint_metadata, codex_default_transport_profile_extra, ExecutionPlan,
    ExecutionTimeouts, ProxySnapshot, RequestBody, ResolvedTransportProfile,
    CODEX_DEFAULT_TLS_FINGERPRINT_SOURCE, CODEX_DEFAULT_TLS_JA3, CODEX_DEFAULT_TLS_JA3_HASH,
    EXECUTION_REQUEST_ACCEPT_INVALID_CERTS_HEADER, EXECUTION_REQUEST_FOLLOW_REDIRECTS_HEADER,
    EXECUTION_REQUEST_HTTP1_ONLY_HEADER, TRANSPORT_BACKEND_BROWSER_WREQ,
    TRANSPORT_BACKEND_HYPER_RUSTLS, TRANSPORT_BACKEND_REQWEST_DEFAULT_TLS,
    TRANSPORT_BACKEND_REQWEST_RUSTLS, TRANSPORT_HTTP_MODE_AUTO, TRANSPORT_HTTP_MODE_HTTP1_ONLY,
    TRANSPORT_POOL_SCOPE_KEY, TRANSPORT_PROFILE_CODEX_LEGACY_REQWEST_RUSTLS_AUTO,
    TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO,
};
pub use result::{ExecutionResult, ExecutionTelemetry, ResponseBody};
pub use usage::{
    ExecutionStreamTerminalSummary, StandardizedUsage, USAGE_SERVER_NOW_UNIX_MS_HEADER,
};
