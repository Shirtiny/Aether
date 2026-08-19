use std::borrow::Cow;

use bytes::Bytes;
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

pub(crate) const ROUTE_CONTROL_ACCEPT_HEADER: &str = "x-aether-ws-control-accept";
pub(crate) const ROUTE_CONTROL_SELECTED_HEADER: &str = "x-aether-ws-control";
pub(crate) const ROUTE_CONTROL_CAPABILITIES_HEADER: &str = "x-aether-ws-capabilities";
pub(crate) const ROUTE_CONTROL_VERSION: &str = "route-v1";
pub(crate) const ROUTE_CONTROL_CAPABILITIES: &str = "close-after-terminal,client-reconnect";
pub(crate) const ROUTE_CONTROL_EVENT_TYPE: &str = "aether.route_control";
pub(crate) const NOT_EXECUTED_PROOF_CLASS: &str = "codex_official_ws.not_executed";
pub(crate) const NOT_EXECUTED_PROOF_VERSION: u32 = 1;
pub(crate) const FIRST_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const MAX_PUBLIC_CLIENT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_ROUTE_FENCE_OVERHEAD_BYTES: usize = 4 * 1024;
pub(crate) const MAX_CLIENT_MESSAGE_BYTES: usize =
    MAX_PUBLIC_CLIENT_PAYLOAD_BYTES + MAX_ROUTE_FENCE_OVERHEAD_BYTES;
const MAX_CONTROL_ID_BYTES: usize = 160;
const MAX_TURN_METADATA_BYTES: usize = 16 * 1024;
pub(crate) const MAX_RESPONSE_ID_BYTES: usize = 256;
pub(crate) const MAX_LOGICAL_TURN_ID_BYTES: usize = 256;
pub(crate) const MAX_TURN_STATE_BYTES: usize = 4 * 1024;
const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_PROVIDER_ERROR_CODE_BYTES: usize = 128;
const MAX_PROVIDER_ERROR_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_PROVIDER_HEADER_VALUE_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteNegotiation;

pub(crate) fn negotiate_route_control(
    headers: &HeaderMap,
) -> Result<RouteNegotiation, ProtocolError> {
    let version = exact_single_header(headers, ROUTE_CONTROL_ACCEPT_HEADER)?;
    if version != ROUTE_CONTROL_VERSION {
        return Err(ProtocolError::Precondition(
            "route-v1 control negotiation is required",
        ));
    }
    Ok(RouteNegotiation)
}

fn exact_single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<&'a str, ProtocolError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next().ok_or(ProtocolError::Precondition(
        "route control header is missing",
    ))?;
    if values.next().is_some() {
        return Err(ProtocolError::Precondition(
            "duplicate route control headers are not allowed",
        ));
    }
    first
        .to_str()
        .map(str::trim)
        .map_err(|_| ProtocolError::Precondition("route control header is not ASCII"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepFence {
    pub(crate) correlation_id: String,
    pub(crate) binding_epoch_id: String,
    pub(crate) binding_generation: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ResponseCreateStep {
    pub(crate) value: Value,
    pub(crate) encoded_len: usize,
    pub(crate) model: String,
    pub(crate) previous_response_id: Option<String>,
    pub(crate) logical_turn_id: Option<String>,
    pub(crate) official_identity: Option<OfficialRequestIdentity>,
    pub(crate) fence: StepFence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OfficialRequestIdentity {
    pub(crate) session_id: String,
    pub(crate) thread_id: String,
    pub(crate) window_id: Option<String>,
    pub(crate) turn_metadata: Option<String>,
    pub(crate) parent_thread_id: Option<String>,
    pub(crate) subagent: Option<String>,
    pub(crate) responses_lite: bool,
}

impl OfficialRequestIdentity {
    pub(crate) fn matches_connection_binding(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.thread_id == other.thread_id
            && self.window_id == other.window_id
            && self.responses_lite == other.responses_lite
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ResponseCreateContext<'a> {
    First,
    Bound {
        model: &'a str,
        expected_previous_response_id: Option<&'a str>,
        turn_state: Option<(&'a str, &'a str)>,
    },
}

pub(crate) fn parse_response_create(
    text: impl AsRef<[u8]>,
    context: ResponseCreateContext<'_>,
) -> Result<ResponseCreateStep, ProtocolError> {
    let text = text.as_ref();
    if text.len() > MAX_CLIENT_MESSAGE_BYTES {
        return Err(ProtocolError::Policy("response.create frame is too large"));
    }
    reject_duplicate_client_fields(text)?;
    let mut value: Value = serde_json::from_slice(text)
        .map_err(|_| ProtocolError::Policy("client frame must be text JSON"))?;
    let object = value
        .as_object_mut()
        .ok_or(ProtocolError::Policy("client frame must be a JSON object"))?;
    if object.get("type").and_then(Value::as_str) != Some("response.create") {
        return Err(ProtocolError::Policy(
            "only response.create client messages are supported",
        ));
    }
    let explicit_model = match object.get("model") {
        None => None,
        Some(Value::String(model))
            if !model.trim().is_empty()
                && model.trim().len() <= MAX_MODEL_ID_BYTES
                && model.trim().is_ascii() =>
        {
            Some(model.trim().to_string())
        }
        Some(Value::String(_)) => {
            return Err(ProtocolError::Policy("response.create model is invalid"));
        }
        Some(_) => {
            return Err(ProtocolError::Policy(
                "response.create model must be a non-empty string",
            ));
        }
    };
    let previous_response_id = optional_non_empty_string(object.get("previous_response_id"))?;
    let (model, bound_turn_state) = match context {
        ResponseCreateContext::First => {
            if previous_response_id.is_some() {
                return Err(ProtocolError::Policy(
                    "the first response.create must be protocol self-contained",
                ));
            }
            (
                explicit_model.ok_or(ProtocolError::Policy("response.create model is required"))?,
                None,
            )
        }
        ResponseCreateContext::Bound {
            model,
            expected_previous_response_id,
            turn_state,
        } => {
            if let Some(previous_response_id) = previous_response_id.as_deref() {
                let Some(expected) = expected_previous_response_id else {
                    return Err(ProtocolError::Policy(
                        "previous_response_id does not belong to this connection epoch",
                    ));
                };
                if previous_response_id != expected {
                    return Err(ProtocolError::Policy(
                        "previous_response_id does not belong to this connection epoch",
                    ));
                }
                if explicit_model
                    .as_deref()
                    .is_some_and(|explicit| explicit != model)
                {
                    return Err(ProtocolError::Policy(
                        "response.create model does not match the bound connection",
                    ));
                }
                (model.to_string(), turn_state)
            } else {
                (explicit_model.unwrap_or_else(|| model.to_string()), None)
            }
        }
    };
    object.insert("model".to_string(), Value::String(model.clone()));

    let metadata = normalize_client_metadata(object)?;
    let fence = take_step_fence(metadata)?;
    let official_identity = official_request_identity(metadata)?;
    let logical_turn_id = logical_turn_id(metadata)?;
    metadata.remove("x-codex-turn-state");
    if let (Some((bound_turn_id, turn_state)), Some(current_turn_id)) =
        (bound_turn_state, logical_turn_id.as_deref())
    {
        if bound_turn_id == current_turn_id {
            metadata.insert(
                "x-codex-turn-state".to_string(),
                Value::String(turn_state.to_string()),
            );
        }
    }

    Ok(ResponseCreateStep {
        value,
        encoded_len: text.len(),
        model,
        previous_response_id,
        logical_turn_id,
        official_identity,
        fence,
    })
}

fn official_request_identity(
    metadata: &Map<String, Value>,
) -> Result<Option<OfficialRequestIdentity>, ProtocolError> {
    let session_id = bounded_metadata_string(metadata, "session_id", false, MAX_CONTROL_ID_BYTES)?;
    let thread_id = bounded_metadata_string(metadata, "thread_id", false, MAX_CONTROL_ID_BYTES)?;
    let (session_id, thread_id) = match (session_id, thread_id) {
        (None, None) => return Ok(None),
        (Some(session_id), Some(thread_id)) => (session_id, thread_id),
        _ => {
            return Err(ProtocolError::Policy(
                "Codex session_id and thread_id must be provided together",
            ))
        }
    };
    Ok(Some(OfficialRequestIdentity {
        session_id,
        thread_id,
        window_id: bounded_metadata_string(
            metadata,
            "x-codex-window-id",
            false,
            MAX_CONTROL_ID_BYTES,
        )?,
        turn_metadata: bounded_metadata_string(
            metadata,
            "x-codex-turn-metadata",
            false,
            MAX_TURN_METADATA_BYTES,
        )?,
        parent_thread_id: bounded_metadata_string(
            metadata,
            "x-codex-parent-thread-id",
            false,
            MAX_CONTROL_ID_BYTES,
        )?,
        subagent: bounded_metadata_string(
            metadata,
            "x-openai-subagent",
            false,
            MAX_CONTROL_ID_BYTES,
        )?,
        responses_lite: metadata_flag(
            metadata,
            "ws_request_header_x_openai_internal_codex_responses_lite",
        )?,
    }))
}

fn metadata_flag(
    metadata: &Map<String, Value>,
    field: &'static str,
) -> Result<bool, ProtocolError> {
    match metadata.get(field) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("true") => Ok(true),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("false") => Ok(false),
        Some(_) => Err(ProtocolError::Policy(
            "Codex client metadata flag is invalid",
        )),
    }
}

fn bounded_metadata_string(
    metadata: &Map<String, Value>,
    field: &'static str,
    required: bool,
    max_bytes: usize,
) -> Result<Option<String>, ProtocolError> {
    match metadata.get(field) {
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() || value.len() > max_bytes || !value.is_ascii() {
                return Err(ProtocolError::Policy("Codex client identity is invalid"));
            }
            Ok(Some(value.to_string()))
        }
        None if !required => Ok(None),
        _ => Err(ProtocolError::Policy(
            "canonical Codex session_id and thread_id metadata are required",
        )),
    }
}

fn optional_non_empty_string(value: Option<&Value>) -> Result<Option<String>, ProtocolError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.trim().is_empty()
                && value.trim().len() <= MAX_RESPONSE_ID_BYTES
                && value.trim().is_ascii() =>
        {
            Ok(Some(value.trim().to_string()))
        }
        _ => Err(ProtocolError::Policy(
            "previous_response_id must be null or a non-empty string",
        )),
    }
}

fn normalize_client_metadata(
    object: &mut Map<String, Value>,
) -> Result<&mut Map<String, Value>, ProtocolError> {
    if !object.contains_key("client_metadata") {
        object.insert("client_metadata".to_string(), Value::Object(Map::new()));
    }
    object
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
        .ok_or(ProtocolError::Policy("client_metadata must be an object"))
}

fn take_step_fence(metadata: &mut Map<String, Value>) -> Result<StepFence, ProtocolError> {
    let value = metadata
        .remove("aether.sub2api_step_control")
        .ok_or(ProtocolError::Policy("sub2api step fence is required"))?;
    let object = value.as_object().ok_or(ProtocolError::Policy(
        "sub2api step fence must be an object",
    ))?;
    let version = object.get("version").and_then(Value::as_u64);
    if version != Some(1) {
        return Err(ProtocolError::Policy(
            "sub2api step fence version is unsupported",
        ));
    }
    let correlation_id = bounded_control_id(object, "sub2api_step_correlation_id")?;
    let binding_epoch_id = bounded_control_id(object, "sub2api_binding_epoch_id")?;
    let binding_generation = object
        .get("sub2api_binding_generation")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or(ProtocolError::Policy(
            "sub2api binding generation is invalid",
        ))?;
    Ok(StepFence {
        correlation_id,
        binding_epoch_id,
        binding_generation,
    })
}

fn bounded_control_id(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<String, ProtocolError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_CONTROL_ID_BYTES)
        .map(str::to_string)
        .ok_or(ProtocolError::Policy("sub2api step fence id is invalid"))
}

fn logical_turn_id(metadata: &Map<String, Value>) -> Result<Option<String>, ProtocolError> {
    if let Some(turn_id) = metadata.get("turn_id") {
        return parse_logical_turn_id(turn_id).map(Some);
    }
    let Some(encoded) = metadata
        .get("x-codex-turn-metadata")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let Ok(decoded) = serde_json::from_str::<Value>(encoded) else {
        return Ok(None);
    };
    decoded
        .get("turn_id")
        .map(parse_logical_turn_id)
        .transpose()
}

fn parse_logical_turn_id(value: &Value) -> Result<String, ProtocolError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= MAX_LOGICAL_TURN_ID_BYTES && value.is_ascii()
        })
        .map(str::to_string)
        .ok_or(ProtocolError::Policy("Codex turn id is invalid"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    Completed,
    Failed,
    Incomplete,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ServerEventClassification {
    pub(crate) recognized_business: bool,
    pub(crate) created: bool,
    pub(crate) terminal: Option<TerminalKind>,
    pub(crate) provenance_response_id: Option<String>,
    pub(crate) created_response_id: Option<String>,
    pub(crate) terminal_response_id: Option<String>,
    pub(crate) turn_state: Option<String>,
    pub(crate) provider_headers: std::collections::BTreeMap<String, String>,
    pub(crate) terminal_event: Option<TerminalEventSummary>,
    pub(crate) codex_relay: CodexRelayDirective,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum CodexRelayDirective {
    #[default]
    ForwardOriginal,
    ForwardEvents(Vec<Bytes>),
    SuppressProviderPrivate,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TerminalEventSummary {
    pub(crate) standardized_usage: Option<aether_contracts::StandardizedUsage>,
    pub(crate) response_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) provider_status_code: Option<u16>,
    pub(crate) provider_error_code: Option<String>,
    pub(crate) provider_error_message: Option<String>,
    pub(crate) provider_error_body: Option<String>,
    pub(crate) provider_headers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct BorrowedServerEvent<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    response_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    response: Option<BorrowedServerResponse<'a>>,
    #[serde(borrow)]
    headers: Option<BorrowedServerHeaders<'a>>,
    status: Option<ServerEventStatus>,
    status_code: Option<u16>,
    #[serde(borrow)]
    error: Option<BorrowedServerError<'a>>,
    #[serde(borrow)]
    incomplete_details: Option<BorrowedIncompleteDetails<'a>>,
    rate_limits: Option<BorrowedRateLimitDetails>,
    #[serde(borrow)]
    credits: Option<BorrowedRateLimitCredits<'a>>,
    #[serde(rename = "x-codex-turn-state", borrow)]
    codex_turn_state: Option<Cow<'a, str>>,
    #[serde(borrow)]
    turn_state: Option<Cow<'a, str>>,
}

#[derive(Debug, Clone, Copy)]
struct ServerEventStatus {
    code: Option<u16>,
}

impl ServerEventStatus {
    fn code(self) -> Option<u16> {
        self.code
    }
}

impl<'de> Deserialize<'de> for ServerEventStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StatusVisitor;

        impl serde::de::Visitor<'_> for StatusVisitor {
            type Value = ServerEventStatus;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a response lifecycle status string or HTTP status code")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let code = u16::try_from(value)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Unsigned(value), &self))?;
                Ok(ServerEventStatus { code: Some(code) })
            }

            fn visit_borrowed_str<E>(self, _value: &'_ str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ServerEventStatus { code: None })
            }

            fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ServerEventStatus { code: None })
            }
        }

        deserializer.deserialize_any(StatusVisitor)
    }
}

#[derive(Debug, Deserialize)]
struct BorrowedServerResponse<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    usage: Option<CompactOpenAiUsage>,
    #[serde(borrow)]
    error: Option<BorrowedServerError<'a>>,
    #[serde(borrow)]
    incomplete_details: Option<BorrowedIncompleteDetails<'a>>,
}

#[derive(Debug, Deserialize)]
struct BorrowedServerHeaders<'a> {
    #[serde(rename = "x-codex-turn-state", borrow)]
    codex_turn_state: Option<Cow<'a, str>>,
    #[serde(rename = "retry-after", borrow)]
    retry_after: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-primary-used-percent", borrow)]
    primary_used_percent: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-secondary-used-percent", borrow)]
    secondary_used_percent: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-primary-over-secondary-limit-percent", borrow)]
    primary_over_secondary_limit_percent: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-primary-window-minutes", borrow)]
    primary_window_minutes: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-secondary-window-minutes", borrow)]
    secondary_window_minutes: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-primary-reset-at", borrow)]
    primary_reset_at: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-secondary-reset-at", borrow)]
    secondary_reset_at: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-rate-limit-reached-type", borrow)]
    rate_limit_reached_type: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-credits-balance", borrow)]
    credits_balance: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-credits-has-credits", borrow)]
    credits_has_credits: Option<CompactHeaderValue<'a>>,
    #[serde(rename = "x-codex-credits-unlimited", borrow)]
    credits_unlimited: Option<CompactHeaderValue<'a>>,
}

#[derive(Debug, Deserialize)]
struct BorrowedServerError<'a> {
    #[serde(borrow)]
    code: Option<Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize)]
struct BorrowedIncompleteDetails<'a> {
    #[serde(borrow)]
    reason: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize)]
struct BorrowedRateLimitDetails {
    primary: Option<BorrowedRateLimitWindow>,
    secondary: Option<BorrowedRateLimitWindow>,
}

#[derive(Debug, Deserialize)]
struct BorrowedRateLimitWindow {
    used_percent: f64,
    window_minutes: Option<i64>,
    reset_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct BorrowedRateLimitCredits<'a> {
    has_credits: bool,
    unlimited: bool,
    #[serde(borrow)]
    balance: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CompactHeaderValue<'a> {
    Text(#[serde(borrow)] Cow<'a, str>),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Bool(bool),
}

impl BorrowedServerError<'_> {
    fn code(&self) -> Option<&str> {
        self.code.as_deref().or(self.kind.as_deref())
    }
}

impl BorrowedServerHeaders<'_> {
    fn compact_provider_headers(&self) -> std::collections::BTreeMap<String, String> {
        let mut headers = std::collections::BTreeMap::new();
        for (name, value) in [
            ("retry-after", self.retry_after.as_ref()),
            (
                "x-codex-primary-used-percent",
                self.primary_used_percent.as_ref(),
            ),
            (
                "x-codex-secondary-used-percent",
                self.secondary_used_percent.as_ref(),
            ),
            (
                "x-codex-primary-over-secondary-limit-percent",
                self.primary_over_secondary_limit_percent.as_ref(),
            ),
            (
                "x-codex-primary-window-minutes",
                self.primary_window_minutes.as_ref(),
            ),
            (
                "x-codex-secondary-window-minutes",
                self.secondary_window_minutes.as_ref(),
            ),
            ("x-codex-primary-reset-at", self.primary_reset_at.as_ref()),
            (
                "x-codex-secondary-reset-at",
                self.secondary_reset_at.as_ref(),
            ),
            (
                "x-codex-rate-limit-reached-type",
                self.rate_limit_reached_type.as_ref(),
            ),
            ("x-codex-credits-balance", self.credits_balance.as_ref()),
            (
                "x-codex-credits-has-credits",
                self.credits_has_credits.as_ref(),
            ),
            ("x-codex-credits-unlimited", self.credits_unlimited.as_ref()),
        ] {
            let Some(value) = value.and_then(CompactHeaderValue::bounded_string) else {
                continue;
            };
            headers.insert(name.to_string(), value);
        }
        headers
    }
}

impl CompactHeaderValue<'_> {
    fn bounded_string(&self) -> Option<String> {
        let value = match self {
            Self::Text(value) => {
                let value = value.trim();
                if value.is_empty() || value.len() > MAX_PROVIDER_HEADER_VALUE_BYTES {
                    return None;
                }
                return Some(value.to_string());
            }
            Self::Unsigned(value) => value.to_string(),
            Self::Signed(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
        };
        (!value.is_empty() && value.len() <= MAX_PROVIDER_HEADER_VALUE_BYTES).then_some(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct JsonStringToken {
    start: usize,
    end: usize,
    escaped: bool,
}

struct JsonScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonScanner<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, ProtocolError> {
        std::str::from_utf8(bytes)
            .map_err(|_| ProtocolError::Upstream("official server emitted invalid JSON"))?;
        Ok(Self { bytes, pos: 0 })
    }

    fn scan_fast_server_event(
        mut self,
    ) -> Result<Option<ServerEventClassification>, ProtocolError> {
        self.skip_whitespace();
        self.expect(b'{')?;
        self.skip_whitespace();
        let mut kind = None;
        let mut saw_type = false;
        let mut requires_fallback = false;
        if self.consume(b'}') {
            self.finish()?;
            return Ok(None);
        }
        loop {
            self.skip_whitespace();
            let key = self.scan_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            if key.escaped {
                // An escaped key can alias a field that carries provenance or
                // terminal state. Let serde perform the decoded duplicate scan.
                requires_fallback = true;
                self.scan_value(1)?;
            } else {
                let key = &self.bytes[key.start..key.end];
                if key == b"type" {
                    if saw_type {
                        requires_fallback = true;
                    }
                    saw_type = true;
                    if self.peek() == Some(b'"') {
                        let value = self.scan_string()?;
                        if value.escaped {
                            requires_fallback = true;
                        } else {
                            kind = Some(&self.bytes[value.start..value.end]);
                        }
                    } else {
                        requires_fallback = true;
                        self.scan_value(1)?;
                    }
                } else {
                    if matches!(
                        key,
                        b"response_id"
                            | b"response"
                            | b"headers"
                            | b"status"
                            | b"status_code"
                            | b"error"
                            | b"x-codex-turn-state"
                            | b"turn_state"
                    ) {
                        requires_fallback = true;
                    }
                    self.scan_value(1)?;
                }
            }
            self.skip_whitespace();
            if self.consume(b'}') {
                break;
            }
            self.expect(b',')?;
        }
        self.finish()?;
        let Some(kind) = kind else {
            return Ok(None);
        };
        if requires_fallback {
            return Ok(None);
        }
        if kind == ROUTE_CONTROL_EVENT_TYPE.as_bytes() {
            return Err(ProtocolError::Upstream(
                "official server emitted a reserved route-control event",
            ));
        }
        if is_recognized_business_kind(kind)
            && !matches!(
                kind,
                b"response.created"
                    | b"response.completed"
                    | b"response.failed"
                    | b"response.incomplete"
                    | b"response.cancelled"
                    | b"error"
                    | b"codex.rate_limits"
                    | b"codex.response.metadata"
            )
        {
            return Ok(Some(ServerEventClassification {
                recognized_business: true,
                ..ServerEventClassification::default()
            }));
        }
        Ok(None)
    }

    fn scan_client_duplicates(mut self) -> Result<(), ProtocolError> {
        self.skip_whitespace();
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.consume(b'}') {
            return self.finish();
        }
        let mut seen_root = 0u16;
        loop {
            self.skip_whitespace();
            let key = self.scan_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            if let Some(index) = client_root_field_index(self.bytes, key) {
                let bit = 1u16 << index;
                if seen_root & bit != 0 {
                    return Err(ProtocolError::Policy(
                        "duplicate response.create field is not allowed",
                    ));
                }
                seen_root |= bit;
                if index == 5 && self.peek() == Some(b'{') {
                    self.scan_client_metadata_duplicates(1)?;
                } else {
                    self.scan_value(1)?;
                }
            } else {
                self.scan_value(1)?;
            }
            self.skip_whitespace();
            if self.consume(b'}') {
                return self.finish();
            }
            self.expect(b',')?;
        }
    }

    fn scan_client_metadata_duplicates(&mut self, depth: usize) -> Result<(), ProtocolError> {
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(());
        }
        let mut seen = 0u16;
        loop {
            self.skip_whitespace();
            let key = self.scan_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            if let Some(index) = client_metadata_field_index(self.bytes, key) {
                let bit = 1u16 << index;
                if seen & bit != 0 {
                    return Err(ProtocolError::Policy(
                        "duplicate Codex client metadata field is not allowed",
                    ));
                }
                seen |= bit;
            }
            self.scan_value(depth)?;
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }

    fn scan_value(&mut self, depth: usize) -> Result<(), ProtocolError> {
        if depth > 128 {
            return self.invalid();
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'"') => {
                self.scan_string()?;
                Ok(())
            }
            Some(b'{') => self.scan_object(depth + 1),
            Some(b'[') => self.scan_array(depth + 1),
            Some(b't') => self.scan_literal(b"true"),
            Some(b'f') => self.scan_literal(b"false"),
            Some(b'n') => self.scan_literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.scan_number(),
            _ => self.invalid(),
        }
    }

    fn scan_object(&mut self, depth: usize) -> Result<(), ProtocolError> {
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            self.scan_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.scan_value(depth)?;
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }

    fn scan_array(&mut self, depth: usize) -> Result<(), ProtocolError> {
        self.expect(b'[')?;
        self.skip_whitespace();
        if self.consume(b']') {
            return Ok(());
        }
        loop {
            self.scan_value(depth)?;
            self.skip_whitespace();
            if self.consume(b']') {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }

    fn scan_string(&mut self) -> Result<JsonStringToken, ProtocolError> {
        self.expect(b'"')?;
        let start = self.pos;
        let mut escaped = false;
        loop {
            let Some(byte) = self.next() else {
                return self.invalid();
            };
            match byte {
                b'"' => {
                    return Ok(JsonStringToken {
                        start,
                        end: self.pos - 1,
                        escaped,
                    });
                }
                b'\\' => {
                    escaped = true;
                    let Some(escape) = self.next() else {
                        return self.invalid();
                    };
                    match escape {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            let code = self.scan_hex_quad()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                                    return self.invalid();
                                }
                                let low = self.scan_hex_quad()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return self.invalid();
                                }
                            } else if (0xDC00..=0xDFFF).contains(&code) {
                                return self.invalid();
                            }
                        }
                        _ => return self.invalid(),
                    }
                }
                0x00..=0x1f => return self.invalid(),
                _ => {}
            }
        }
    }

    fn scan_hex_quad(&mut self) -> Result<u16, ProtocolError> {
        let mut value = 0u16;
        for _ in 0..4 {
            let digit = match self.next() {
                Some(b'0'..=b'9') => self.bytes[self.pos - 1] - b'0',
                Some(b'a'..=b'f') => self.bytes[self.pos - 1] - b'a' + 10,
                Some(b'A'..=b'F') => self.bytes[self.pos - 1] - b'A' + 10,
                _ => return self.invalid(),
            };
            value = (value << 4) | u16::from(digit);
        }
        Ok(value)
    }

    fn scan_number(&mut self) -> Result<(), ProtocolError> {
        self.consume(b'-');
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return self.invalid();
                }
            }
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return self.invalid(),
        }
        if self.consume(b'.') {
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.invalid();
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.invalid();
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        Ok(())
    }

    fn scan_literal(&mut self, literal: &[u8]) -> Result<(), ProtocolError> {
        if self.bytes.get(self.pos..self.pos + literal.len()) == Some(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            self.invalid()
        }
    }

    fn finish(&mut self) -> Result<(), ProtocolError> {
        self.skip_whitespace();
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            self.invalid()
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ProtocolError> {
        if self.consume(expected) {
            Ok(())
        } else {
            self.invalid()
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let value = self.peek()?;
        self.pos += 1;
        Some(value)
    }

    fn invalid<T>(&self) -> Result<T, ProtocolError> {
        Err(ProtocolError::Upstream(
            "official server emitted invalid JSON",
        ))
    }
}

fn reject_duplicate_client_fields(bytes: &[u8]) -> Result<(), ProtocolError> {
    JsonScanner::new(bytes)?.scan_client_duplicates()
}

fn client_root_field_index(bytes: &[u8], token: JsonStringToken) -> Option<usize> {
    [
        b"type".as_slice(),
        b"model".as_slice(),
        b"previous_response_id".as_slice(),
        b"background".as_slice(),
        b"store".as_slice(),
        b"client_metadata".as_slice(),
    ]
    .iter()
    .position(|expected| json_string_token_equals(bytes, token, expected))
}

fn client_metadata_field_index(bytes: &[u8], token: JsonStringToken) -> Option<usize> {
    [
        b"session_id".as_slice(),
        b"thread_id".as_slice(),
        b"x-codex-window-id".as_slice(),
        b"x-codex-turn-metadata".as_slice(),
        b"x-codex-parent-thread-id".as_slice(),
        b"x-openai-subagent".as_slice(),
        b"turn_id".as_slice(),
        b"x-codex-turn-state".as_slice(),
        b"sub2api_step_correlation_id".as_slice(),
        b"sub2api_binding_epoch_id".as_slice(),
        b"sub2api_binding_generation".as_slice(),
        b"ws_request_header_x_openai_internal_codex_responses_lite".as_slice(),
    ]
    .iter()
    .position(|expected| json_string_token_equals(bytes, token, expected))
}

fn json_string_token_equals(bytes: &[u8], token: JsonStringToken, expected: &[u8]) -> bool {
    if !token.escaped {
        return &bytes[token.start..token.end] == expected;
    }
    let mut output_index = 0usize;
    let mut index = token.start;
    while index < token.end {
        let value = if bytes[index] != b'\\' {
            let value = bytes[index];
            index += 1;
            value
        } else {
            index += 1;
            let Some(&escape) = bytes.get(index) else {
                return false;
            };
            index += 1;
            match escape {
                b'"' => b'"',
                b'\\' => b'\\',
                b'/' => b'/',
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'u' => {
                    if index + 4 > token.end {
                        return false;
                    }
                    let mut code = 0u16;
                    for digit in &bytes[index..index + 4] {
                        code = match digit {
                            b'0'..=b'9' => (code << 4) | u16::from(*digit - b'0'),
                            b'a'..=b'f' => (code << 4) | u16::from(*digit - b'a' + 10),
                            b'A'..=b'F' => (code << 4) | u16::from(*digit - b'A' + 10),
                            _ => return false,
                        };
                    }
                    index += 4;
                    if code > 0x7f {
                        return false;
                    }
                    code as u8
                }
                _ => return false,
            }
        };
        if expected.get(output_index).copied() != Some(value) {
            return false;
        }
        output_index += 1;
    }
    output_index == expected.len()
}

#[derive(Debug, Default, Deserialize)]
struct CompactOpenAiUsage {
    input_tokens: Option<serde_json::Number>,
    prompt_tokens: Option<serde_json::Number>,
    output_tokens: Option<serde_json::Number>,
    completion_tokens: Option<serde_json::Number>,
    reasoning_output_tokens: Option<serde_json::Number>,
    cache_creation_input_tokens: Option<serde_json::Number>,
    cache_read_input_tokens: Option<serde_json::Number>,
    total_tokens: Option<serde_json::Number>,
    input_tokens_details: Option<CompactOpenAiTokenDetails>,
    prompt_tokens_details: Option<CompactOpenAiTokenDetails>,
    output_tokens_details: Option<CompactOpenAiTokenDetails>,
    completion_tokens_details: Option<CompactOpenAiTokenDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct CompactOpenAiTokenDetails {
    cached_tokens: Option<serde_json::Number>,
    cached_creation_tokens: Option<serde_json::Number>,
    cache_creation_tokens: Option<serde_json::Number>,
    cache_write_tokens: Option<serde_json::Number>,
    reasoning_tokens: Option<serde_json::Number>,
}

pub(crate) fn classify_server_event(
    text: impl AsRef<[u8]>,
) -> Result<ServerEventClassification, ProtocolError> {
    let bytes = text.as_ref();
    // Codex may batch standard Responses events in a private `chunks`
    // envelope. Observe every event in document order while the caller keeps
    // the original frame bytes for opaque relay.
    if bytes
        .windows(b"\"chunks\"".len())
        .any(|window| window == b"\"chunks\"")
    {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| ProtocolError::Upstream("official server emitted invalid JSON"))?;
        if value.get("chunks").and_then(Value::as_array).is_some() {
            return classify_chunked_server_frame(&value);
        }
    }
    classify_direct_server_event(bytes)
}

pub(crate) fn classify_standard_server_event(
    text: impl AsRef<[u8]>,
) -> Result<ServerEventClassification, ProtocolError> {
    classify_direct_server_event(text.as_ref())
}

fn classify_direct_server_event(bytes: &[u8]) -> Result<ServerEventClassification, ProtocolError> {
    if let Some(classification) = JsonScanner::new(bytes)?.scan_fast_server_event()? {
        return Ok(classification);
    }
    #[cfg(test)]
    SERVER_EVENT_JSON_PARSE_COUNT.with(|count| count.set(count.get() + 1));
    // The scanner already validated JSON syntax. This pass enforces the fields
    // whose types or duplicate presence affect routing and accounting.
    let value: BorrowedServerEvent<'_> = serde_json::from_slice(bytes)
        .map_err(|_| ProtocolError::Upstream("official server emitted invalid event schema"))?;
    let kind = value.kind.as_deref().unwrap_or_default();
    if kind == ROUTE_CONTROL_EVENT_TYPE {
        return Err(ProtocolError::Upstream(
            "official server emitted a reserved route-control event",
        ));
    }
    let terminal = match kind {
        "response.completed" => Some(TerminalKind::Completed),
        "response.failed" => Some(TerminalKind::Failed),
        "response.incomplete" => Some(TerminalKind::Incomplete),
        "response.cancelled" => Some(TerminalKind::Cancelled),
        "error" => Some(TerminalKind::Error),
        _ => None,
    };
    let nested_response_id = value
        .response
        .as_ref()
        .and_then(|response| response.id.as_deref())
        .map(parse_official_response_id)
        .transpose()?;
    let top_level_response_id = value
        .response_id
        .as_deref()
        .map(parse_official_response_id)
        .transpose()?;
    if nested_response_id
        .as_ref()
        .zip(top_level_response_id.as_ref())
        .is_some_and(|(nested, top_level)| nested != top_level)
    {
        return Err(ProtocolError::Upstream(
            "official server emitted conflicting response ids",
        ));
    }
    let event_response_id = nested_response_id.or(top_level_response_id);
    let created_response_id = (kind == "response.created")
        .then(|| event_response_id.clone())
        .flatten();
    let terminal_response_id = terminal.and(event_response_id.clone());
    let mut turn_state = None;
    for source in [
        value
            .headers
            .as_ref()
            .and_then(|headers| headers.codex_turn_state.as_deref()),
        value.codex_turn_state.as_deref(),
        value.turn_state.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let source = validate_official_turn_state(source)?;
        if turn_state
            .as_ref()
            .is_some_and(|current: &String| current != &source)
        {
            return Err(ProtocolError::Upstream(
                "official server emitted conflicting turn states",
            ));
        }
        turn_state = Some(source);
    }
    let mut provider_headers = value
        .headers
        .as_ref()
        .map(BorrowedServerHeaders::compact_provider_headers)
        .unwrap_or_default();
    if kind == "codex.rate_limits" {
        append_rate_limit_event_headers(&value, &mut provider_headers);
    }
    let status_code_from_status = value.status.and_then(ServerEventStatus::code);
    if status_code_from_status
        .zip(value.status_code)
        .is_some_and(|(status, status_code)| status != status_code)
    {
        return Err(ProtocolError::Upstream(
            "official server emitted conflicting status codes",
        ));
    }
    let explicit_status_code = value.status_code.or(status_code_from_status);
    let terminal_event = terminal.map(|kind| {
        let provider_error = value
            .response
            .as_ref()
            .and_then(|response| response.error.as_ref())
            .or(value.error.as_ref());
        let provider_error_code = provider_error
            .and_then(BorrowedServerError::code)
            .and_then(|code| bounded_ascii_string(code, MAX_PROVIDER_ERROR_CODE_BYTES));
        let provider_error_message = provider_error
            .and_then(|error| error.message.as_deref())
            .and_then(|message| bounded_utf8_string(message, MAX_PROVIDER_ERROR_MESSAGE_BYTES));
        let incomplete_reason = value
            .response
            .as_ref()
            .and_then(|response| response.incomplete_details.as_ref())
            .or(value.incomplete_details.as_ref())
            .and_then(|details| details.reason.as_deref())
            .and_then(|reason| bounded_ascii_string(reason, MAX_PROVIDER_ERROR_CODE_BYTES));
        let provider_status_code = explicit_status_code
            .filter(|status| (400..=599).contains(status))
            .or_else(|| {
                inferred_provider_status_code(
                    kind,
                    provider_error_code.as_deref(),
                    incomplete_reason.as_deref(),
                )
            });
        let provider_error_body = compact_provider_error_body(
            provider_error_code.as_deref(),
            provider_error_message.as_deref(),
            incomplete_reason.as_deref(),
        );
        TerminalEventSummary {
            standardized_usage: value
                .response
                .as_ref()
                .and_then(|response| response.usage.as_ref())
                .map(CompactOpenAiUsage::standardize)
                .filter(aether_contracts::StandardizedUsage::has_token_signal),
            response_id: event_response_id.clone(),
            model: value
                .response
                .as_ref()
                .and_then(|response| response.model.as_deref())
                .and_then(|model| bounded_ascii_string(model, MAX_MODEL_ID_BYTES)),
            provider_status_code,
            provider_error_code,
            provider_error_message,
            provider_error_body,
            provider_headers: provider_headers.clone(),
        }
    });
    let codex_private = is_codex_private_event_type(kind);
    Ok(ServerEventClassification {
        recognized_business: is_recognized_business_kind(kind.as_bytes()) && !codex_private,
        created: kind == "response.created",
        terminal,
        provenance_response_id: event_response_id.clone(),
        created_response_id,
        terminal_response_id,
        turn_state,
        provider_headers,
        terminal_event,
        codex_relay: if codex_private {
            CodexRelayDirective::SuppressProviderPrivate
        } else {
            CodexRelayDirective::ForwardOriginal
        },
    })
}

fn classify_chunked_server_frame(
    frame: &Value,
) -> Result<ServerEventClassification, ProtocolError> {
    let mut merged = ServerEventClassification::default();
    let mut events = Vec::new();
    if frame.get("type").and_then(Value::as_str).is_some() {
        events.push(frame);
    }
    if let Some(chunks) = frame.get("chunks").and_then(Value::as_array) {
        events.extend(
            chunks
                .iter()
                .filter(|event| event.get("type").and_then(Value::as_str).is_some()),
        );
    }
    for event in events {
        let encoded = serde_json::to_vec(event)
            .map_err(|_| ProtocolError::Upstream("official server emitted invalid event schema"))?;
        merge_server_event_classification(&mut merged, classify_direct_server_event(&encoded)?)?;
    }
    if is_explicit_codex_batch_envelope(frame) {
        let public_events = frame
            .get("chunks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|event| {
                !event
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(is_codex_private_event_type)
            })
            .map(|event| {
                serde_json::to_vec(event).map(Bytes::from).map_err(|_| {
                    ProtocolError::Upstream("official server emitted invalid event schema")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        merged.codex_relay = if public_events.is_empty() {
            CodexRelayDirective::SuppressProviderPrivate
        } else {
            CodexRelayDirective::ForwardEvents(public_events)
        };
    } else {
        merged.codex_relay = CodexRelayDirective::ForwardOriginal;
    }
    Ok(merged)
}

fn is_explicit_codex_batch_envelope(frame: &Value) -> bool {
    if frame
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(is_codex_private_event_type)
    {
        return true;
    }
    frame.as_object().is_some_and(|object| {
        object.len() == 1
            && object.contains_key("chunks")
            && frame.get("type").and_then(Value::as_str).is_none()
    })
}

fn is_codex_private_event_type(kind: &str) -> bool {
    matches!(kind, "codex.rate_limits" | "codex.response.metadata")
}

fn merge_server_event_classification(
    merged: &mut ServerEventClassification,
    next: ServerEventClassification,
) -> Result<(), ProtocolError> {
    merged.recognized_business |= next.recognized_business;
    merged.created |= next.created;
    merge_consistent_value(
        &mut merged.provenance_response_id,
        next.provenance_response_id,
        "official server emitted conflicting response ids",
    )?;
    merge_consistent_value(
        &mut merged.created_response_id,
        next.created_response_id,
        "official server emitted conflicting response ids",
    )?;
    merge_consistent_value(
        &mut merged.turn_state,
        next.turn_state,
        "official server emitted conflicting turn states",
    )?;
    merged.provider_headers.extend(next.provider_headers);
    if merged.terminal.is_none() {
        merged.terminal = next.terminal;
        merged.terminal_response_id = next.terminal_response_id;
        merged.terminal_event = next.terminal_event;
    }
    Ok(())
}

fn merge_consistent_value(
    current: &mut Option<String>,
    next: Option<String>,
    conflict: &'static str,
) -> Result<(), ProtocolError> {
    let Some(next) = next else {
        return Ok(());
    };
    if current.as_ref().is_some_and(|current| current != &next) {
        return Err(ProtocolError::Upstream(conflict));
    }
    *current = Some(next);
    Ok(())
}

fn append_rate_limit_event_headers(
    event: &BorrowedServerEvent<'_>,
    headers: &mut std::collections::BTreeMap<String, String>,
) {
    for (prefix, window) in [
        (
            "x-codex-primary",
            event
                .rate_limits
                .as_ref()
                .and_then(|limits| limits.primary.as_ref()),
        ),
        (
            "x-codex-secondary",
            event
                .rate_limits
                .as_ref()
                .and_then(|limits| limits.secondary.as_ref()),
        ),
    ] {
        let Some(window) = window else {
            continue;
        };
        if window.used_percent.is_finite() {
            headers.insert(
                format!("{prefix}-used-percent"),
                window.used_percent.to_string(),
            );
        }
        if let Some(window_minutes) = window.window_minutes {
            headers.insert(
                format!("{prefix}-window-minutes"),
                window_minutes.to_string(),
            );
        }
        if let Some(reset_at) = window.reset_at {
            headers.insert(format!("{prefix}-reset-at"), reset_at.to_string());
        }
    }
    if let Some(credits) = event.credits.as_ref() {
        headers.insert(
            "x-codex-credits-has-credits".to_string(),
            credits.has_credits.to_string(),
        );
        headers.insert(
            "x-codex-credits-unlimited".to_string(),
            credits.unlimited.to_string(),
        );
        if let Some(balance) = credits
            .balance
            .as_deref()
            .and_then(|balance| bounded_utf8_string(balance, MAX_PROVIDER_HEADER_VALUE_BYTES))
        {
            headers.insert("x-codex-credits-balance".to_string(), balance);
        }
    }
}

fn parse_official_response_id(value: &str) -> Result<String, ProtocolError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_RESPONSE_ID_BYTES || !value.is_ascii() {
        return Err(ProtocolError::Upstream(
            "official server emitted an invalid response id",
        ));
    }
    Ok(value.to_string())
}

fn bounded_ascii_string(value: &str, max_bytes: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max_bytes && value.is_ascii()).then(|| value.to_string())
}

fn bounded_utf8_string(value: &str, max_bytes: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (end > 0).then(|| value[..end].to_string())
}

fn inferred_provider_status_code(
    terminal: TerminalKind,
    error_code: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Option<u16> {
    let status = match error_code.unwrap_or_default() {
        "rate_limit_exceeded"
        | "insufficient_quota"
        | "usage_limit_reached"
        | "usage_not_included" => 429,
        "invalid_api_key"
        | "authentication_error"
        | "token_expired"
        | "refresh_token_invalidated"
        | "unauthorized" => 401,
        "context_length_exceeded" | "invalid_prompt" | "bio_policy" | "cyber_policy" => 400,
        "websocket_connection_limit_reached" => 409,
        "server_error"
        | "server_overloaded"
        | "server_overloaded_error"
        | "server_is_overloaded"
        | "slow_down" => 503,
        _ if terminal == TerminalKind::Cancelled => 499,
        _ if terminal == TerminalKind::Incomplete && error_code.is_some() => 502,
        _ if terminal == TerminalKind::Incomplete => {
            match incomplete_reason
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
            {
                Some(reason)
                    if !reason.eq_ignore_ascii_case("error")
                        && !reason.eq_ignore_ascii_case("server_error") =>
                {
                    200
                }
                _ => 502,
            }
        }
        _ => 502,
    };
    Some(status)
}

fn compact_provider_error_body(
    code: Option<&str>,
    message: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Option<String> {
    (code.is_some() || message.is_some() || incomplete_reason.is_some()).then(|| {
        serde_json::to_string(&json!({
            "error": {
                "code": code,
                "message": message,
            },
            "incomplete_details": {
                "reason": incomplete_reason,
            }
        }))
        .expect("bounded provider error summary is serializable")
    })
}

fn is_recognized_business_kind(kind: &[u8]) -> bool {
    kind.starts_with(b"response.")
        || matches!(
            kind,
            b"error" | b"codex.response.metadata" | b"codex.rate_limits"
        )
}

impl CompactOpenAiUsage {
    fn standardize(&self) -> aether_contracts::StandardizedUsage {
        let mut usage = aether_contracts::StandardizedUsage::new();
        usage.input_tokens =
            first_number([self.input_tokens.as_ref(), self.prompt_tokens.as_ref()]);
        usage.output_tokens =
            first_number([self.output_tokens.as_ref(), self.completion_tokens.as_ref()]);
        usage.cache_read_tokens = first_number([
            self.input_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens.as_ref()),
            self.prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens.as_ref()),
            self.cache_read_input_tokens.as_ref(),
        ]);
        usage.cache_creation_tokens = first_number([
            self.input_tokens_details
                .as_ref()
                .and_then(|details| details.cache_write_tokens.as_ref()),
            self.prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cache_write_tokens.as_ref()),
            self.input_tokens_details
                .as_ref()
                .and_then(|details| details.cache_creation_tokens.as_ref()),
            self.prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cache_creation_tokens.as_ref()),
            self.input_tokens_details
                .as_ref()
                .and_then(|details| details.cached_creation_tokens.as_ref()),
            self.prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_creation_tokens.as_ref()),
            self.cache_creation_input_tokens.as_ref(),
        ]);
        usage.reasoning_tokens = first_number([
            self.output_tokens_details
                .as_ref()
                .and_then(|details| details.reasoning_tokens.as_ref()),
            self.completion_tokens_details
                .as_ref()
                .and_then(|details| details.reasoning_tokens.as_ref()),
            self.reasoning_output_tokens.as_ref(),
        ]);
        usage.reasoning_output_tokens = usage.reasoning_tokens;
        if let Some(total_tokens) = positive_number(self.total_tokens.as_ref()) {
            usage
                .dimensions
                .insert("total_tokens".to_string(), serde_json::json!(total_tokens));
        }
        usage
    }
}

fn first_number<const N: usize>(values: [Option<&serde_json::Number>; N]) -> i64 {
    values
        .into_iter()
        .find_map(positive_number)
        .unwrap_or_default()
}

fn positive_number(value: Option<&serde_json::Number>) -> Option<i64> {
    value
        .and_then(|number| {
            number
                .as_i64()
                .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
        .filter(|value| *value > 0)
}

pub(crate) fn validate_official_turn_state(value: &str) -> Result<String, ProtocolError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_TURN_STATE_BYTES || !value.is_ascii() {
        return Err(ProtocolError::Upstream(
            "official server emitted an invalid turn state",
        ));
    }
    Ok(value.to_string())
}

#[cfg(test)]
thread_local! {
    static SERVER_EVENT_JSON_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_server_event_json_parse_count() {
    SERVER_EVENT_JSON_PARSE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn server_event_json_parse_count() -> usize {
    SERVER_EVENT_JSON_PARSE_COUNT.with(std::cell::Cell::get)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RouteControlAction {
    CloseAfterTerminal,
    ClientReconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MiddleRouteDisposition {
    Retain,
    Exclude,
}

pub(crate) fn route_control_event(
    action: RouteControlAction,
    middle_route_disposition: Option<MiddleRouteDisposition>,
    reason: &'static str,
    fence: &StepFence,
    current_attempt_state: &'static str,
    provider_write_state: &'static str,
    provider_execution_disposition: &'static str,
    include_not_executed_proof: bool,
) -> String {
    let action_text = match action {
        RouteControlAction::CloseAfterTerminal => "close_after_terminal",
        RouteControlAction::ClientReconnect => "client_reconnect",
    };
    #[derive(Serialize)]
    struct RouteControlEvent<'a> {
        #[serde(rename = "type")]
        event_type: &'static str,
        version: u32,
        action: &'static str,
        control_id: String,
        scope: &'static str,
        effective_after: &'static str,
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        middle_route_disposition: Option<MiddleRouteDisposition>,
        sub2api_step_correlation_id: &'a str,
        sub2api_binding_epoch_id: &'a str,
        sub2api_binding_generation: u64,
        aether_step_id: String,
        aether_attempt_id: String,
        current_attempt_state: &'static str,
        provider_write_state: &'static str,
        provider_execution_disposition: &'static str,
        adapter_proof_class: Option<&'static str>,
        adapter_proof_version: Option<u32>,
        retry_after_ms: u32,
        recommended_action: &'static str,
    }

    serde_json::to_string(&RouteControlEvent {
        event_type: ROUTE_CONTROL_EVENT_TYPE,
        version: 1,
        action: action_text,
        control_id: Uuid::new_v4().to_string(),
        scope: if action == RouteControlAction::CloseAfterTerminal {
            "next_binding"
        } else {
            "current_step"
        },
        effective_after: if action == RouteControlAction::CloseAfterTerminal {
            "current_terminal"
        } else {
            "immediate"
        },
        reason,
        middle_route_disposition,
        sub2api_step_correlation_id: &fence.correlation_id,
        sub2api_binding_epoch_id: &fence.binding_epoch_id,
        sub2api_binding_generation: fence.binding_generation,
        aether_step_id: Uuid::new_v4().to_string(),
        aether_attempt_id: Uuid::new_v4().to_string(),
        current_attempt_state,
        provider_write_state,
        provider_execution_disposition,
        adapter_proof_class: include_not_executed_proof.then_some(NOT_EXECUTED_PROOF_CLASS),
        adapter_proof_version: include_not_executed_proof.then_some(NOT_EXECUTED_PROOF_VERSION),
        retry_after_ms: 250,
        recommended_action: action_text,
    })
    .expect("route control events contain only serializable values")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    Precondition(&'static str),
    Policy(&'static str),
    Upstream(&'static str),
}

impl ProtocolError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Precondition(message) | Self::Policy(message) | Self::Upstream(message) => {
                message
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use serde_json::json;

    use super::*;

    #[test]
    fn downstream_message_limit_reserves_only_the_fixed_route_fence_overhead() {
        assert_eq!(MAX_PUBLIC_CLIENT_PAYLOAD_BYTES, 16 * 1024 * 1024);
        assert_eq!(MAX_ROUTE_FENCE_OVERHEAD_BYTES, 4 * 1024);
        assert_eq!(MAX_CLIENT_MESSAGE_BYTES, 16 * 1024 * 1024 + 4 * 1024);
        assert_eq!(
            aether_codex_ws_connector::MAX_MESSAGE_SIZE_BYTES,
            64 * 1024 * 1024
        );
    }

    fn fenced_request(extra: Value) -> String {
        json!({
            "type": "response.create",
            "model": "gpt-5",
            "previous_response_id": null,
            "client_metadata": {
                "keep": "yes",
                "session_id": "session-1",
                "thread_id": "thread-1",
                "x-codex-installation-id": "installation-1",
                "x-codex-window-id": "window-1",
                "aether.sub2api_step_control": {
                    "version": 1,
                    "sub2api_step_correlation_id": "step-1",
                    "sub2api_binding_epoch_id": "binding-1",
                    "sub2api_binding_generation": 7
                },
                "extra": extra
            }
        })
        .to_string()
    }

    #[test]
    fn negotiation_is_exact_and_rejects_duplicates() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ROUTE_CONTROL_ACCEPT_HEADER,
            HeaderValue::from_static(ROUTE_CONTROL_VERSION),
        );
        assert_eq!(negotiate_route_control(&headers), Ok(RouteNegotiation));
        headers.append(
            ROUTE_CONTROL_ACCEPT_HEADER,
            HeaderValue::from_static(ROUTE_CONTROL_VERSION),
        );
        assert!(negotiate_route_control(&headers).is_err());
    }

    #[test]
    fn response_create_strips_trusted_hop_control_before_provider_write() {
        let parsed =
            parse_response_create(&fenced_request(json!(true)), ResponseCreateContext::First)
                .expect("request should parse");
        assert_eq!(parsed.model, "gpt-5");
        assert_eq!(parsed.fence.correlation_id, "step-1");
        let identity = parsed
            .official_identity
            .as_ref()
            .expect("official identity should parse");
        assert_eq!(identity.session_id, "session-1");
        assert_eq!(identity.thread_id, "thread-1");
        assert!(parsed.value.get("store").is_none());
        assert_eq!(parsed.value["client_metadata"]["keep"], "yes");
        assert!(parsed.value["client_metadata"]
            .get("aether.sub2api_step_control")
            .is_none());
    }

    #[test]
    fn previous_response_id_is_connection_epoch_fenced() {
        let mut request: Value = serde_json::from_str(&fenced_request(json!(true))).unwrap();
        request["previous_response_id"] = json!("resp-1");
        let text = request.to_string();
        assert!(parse_response_create(&text, ResponseCreateContext::First).is_err());
        assert!(parse_response_create(
            &text,
            ResponseCreateContext::Bound {
                model: "gpt-5",
                expected_previous_response_id: Some("resp-other"),
                turn_state: None,
            },
        )
        .is_err());
        assert!(parse_response_create(
            &text,
            ResponseCreateContext::Bound {
                model: "gpt-5",
                expected_previous_response_id: Some("resp-1"),
                turn_state: None,
            },
        )
        .is_ok());
    }

    #[test]
    fn bound_steps_inherit_the_model_but_independent_turns_may_change_it() {
        let mut request: Value = serde_json::from_str(&fenced_request(json!(true))).unwrap();
        request.as_object_mut().unwrap().remove("model");
        let text = request.to_string();

        assert!(parse_response_create(&text, ResponseCreateContext::First).is_err());
        let inherited = parse_response_create(
            &text,
            ResponseCreateContext::Bound {
                model: "gpt-5",
                expected_previous_response_id: None,
                turn_state: None,
            },
        )
        .expect("bound follow-up should inherit model");
        assert_eq!(inherited.model, "gpt-5");
        assert_eq!(inherited.value["model"], "gpt-5");

        request["model"] = json!("gpt-other");
        let independent = parse_response_create(
            &request.to_string(),
            ResponseCreateContext::Bound {
                model: "gpt-5",
                expected_previous_response_id: None,
                turn_state: None,
            },
        )
        .expect("independent turn should be allowed to change model");
        assert_eq!(independent.model, "gpt-other");

        request["previous_response_id"] = json!("resp-1");
        assert!(parse_response_create(
            &request.to_string(),
            ResponseCreateContext::Bound {
                model: "gpt-5",
                expected_previous_response_id: Some("resp-1"),
                turn_state: None,
            },
        )
        .is_err());
    }

    #[test]
    fn retained_client_and_official_identifiers_are_bounded_ascii() {
        let mut request: Value = serde_json::from_str(&fenced_request(json!(true))).unwrap();
        request["model"] = json!("m".repeat(MAX_MODEL_ID_BYTES + 1));
        assert!(parse_response_create(&request.to_string(), ResponseCreateContext::First).is_err());
        request["model"] = json!("gpt-5");
        request["client_metadata"]["turn_id"] = json!("t".repeat(MAX_LOGICAL_TURN_ID_BYTES + 1));
        assert!(parse_response_create(&request.to_string(), ResponseCreateContext::First).is_err());
        request["client_metadata"]["turn_id"] =
            json!(format!("turn-{}", char::from_u32(0x2603).unwrap()));
        assert!(parse_response_create(&request.to_string(), ResponseCreateContext::First).is_err());
        request["client_metadata"]
            .as_object_mut()
            .unwrap()
            .remove("turn_id");
        request["previous_response_id"] = json!("p".repeat(MAX_RESPONSE_ID_BYTES + 1));
        assert!(parse_response_create(
            &request.to_string(),
            ResponseCreateContext::Bound {
                model: "gpt-5",
                expected_previous_response_id: Some("resp-1"),
                turn_state: None,
            },
        )
        .is_err());

        let oversized_response_id = json!({
            "type": "response.output_text.delta",
            "response_id": "r".repeat(MAX_RESPONSE_ID_BYTES + 1),
            "delta": "x"
        })
        .to_string();
        assert!(classify_server_event(&oversized_response_id).is_err());

        let oversized_turn_state = json!({
            "type": "response.output_item.done",
            "response_id": "resp-1",
            "turn_state": "s".repeat(MAX_TURN_STATE_BYTES + 1)
        })
        .to_string();
        assert!(classify_server_event(&oversized_turn_state).is_err());
        let non_ascii_turn_state = format!("state-{}", char::from_u32(0x2603).unwrap());
        assert!(validate_official_turn_state(&non_ascii_turn_state).is_err());
    }

    #[test]
    fn terminal_classifier_does_not_treat_metadata_as_terminal() {
        assert_eq!(
            classify_server_event(r#"{"type":"response.metadata"}"#)
                .unwrap()
                .terminal,
            None
        );
        assert_eq!(
            classify_server_event(r#"{"type":"response.completed"}"#)
                .unwrap()
                .terminal,
            Some(TerminalKind::Completed)
        );
        assert!(classify_server_event(r#"{"type":"aether.route_control"}"#).is_err());
    }

    #[test]
    fn classifies_response_created_for_step_provenance() {
        let event = classify_server_event(
            r#"{"type":"response.created","response":{"id":"resp-created-1"}}"#,
        )
        .expect("created event should classify");

        assert!(event.created);
        assert_eq!(
            event.provenance_response_id.as_deref(),
            Some("resp-created-1")
        );
        assert_eq!(event.created_response_id.as_deref(), Some("resp-created-1"));
        assert_eq!(event.terminal, None);
    }

    #[test]
    fn codex_continuation_binding_ignores_turn_scoped_identity_fields() {
        let identity = OfficialRequestIdentity {
            session_id: "session-1".into(),
            thread_id: "thread-1".into(),
            window_id: Some("window-1".into()),
            turn_metadata: Some(r#"{"turn":"one"}"#.into()),
            parent_thread_id: Some("parent-1".into()),
            subagent: Some("review".into()),
            responses_lite: false,
        };
        assert!(identity.matches_connection_binding(&identity));

        let mut changed = identity.clone();
        changed.turn_metadata = Some(r#"{"turn":"two"}"#.into());
        assert!(identity.matches_connection_binding(&changed));
        changed = identity.clone();
        changed.parent_thread_id = Some("parent-2".into());
        assert!(identity.matches_connection_binding(&changed));
        changed = identity.clone();
        changed.subagent = Some("compact".into());
        assert!(identity.matches_connection_binding(&changed));
        changed = identity.clone();
        changed.window_id = Some("window-2".into());
        assert!(!identity.matches_connection_binding(&changed));
    }

    #[test]
    fn standard_classifier_does_not_interpret_codex_chunk_envelopes() {
        let event = classify_standard_server_event(
            r#"{"chunks":[{"type":"response.completed","response":{"id":"resp-1"}}]}"#,
        )
        .expect("a standard provider extension should remain opaque");

        assert!(!event.recognized_business);
        assert_eq!(event.terminal, None);
        assert_eq!(event.terminal_response_id, None);
        assert_eq!(event.codex_relay, CodexRelayDirective::ForwardOriginal);
    }

    #[test]
    fn delta_frames_use_zero_allocating_fast_scan_without_retaining_payload_trees() {
        reset_server_event_json_parse_count();
        for index in 0..1_000 {
            let frame = format!(r#"{{"type":"response.output_text.delta","delta":"{index}"}}"#);
            let classified = classify_server_event(&frame).unwrap();
            assert!(classified.recognized_business);
            assert!(classified.provenance_response_id.is_none());
        }
        let classified = classify_server_event(
            r#"{"type":"response.output_text.delta","delta":"literal response_id text"}"#,
        )
        .unwrap();
        assert!(classified.recognized_business);
        assert!(classified.provenance_response_id.is_none());
        assert_eq!(server_event_json_parse_count(), 0);

        let classified = classify_server_event(
            r#"{"type":"response.output_text.delta","response_id":"resp-1","delta":"x"}"#,
        )
        .expect("ID-bearing delta should classify");
        assert_eq!(classified.provenance_response_id.as_deref(), Some("resp-1"));
        assert_eq!(server_event_json_parse_count(), 1);
    }

    #[test]
    fn fast_scanner_recognizes_codex_tool_and_reasoning_deltas() {
        reset_server_event_json_parse_count();
        for frame in [
            r#"{"type":"response.custom_tool_call_input.delta","delta":"{}"}"#,
            r#"{"type":"response.reasoning_text.delta","delta":"thinking"}"#,
            r#"{"type":"response.function_call_arguments.delta","delta":"{}"}"#,
        ] {
            let classified = classify_server_event(frame).expect("delta should classify");
            assert!(classified.recognized_business);
            assert_eq!(classified.terminal, None);
        }
        assert_eq!(server_event_json_parse_count(), 0);
    }

    #[test]
    fn official_reasoning_part_done_accepts_string_lifecycle_status() {
        let frame = r#"{"type":"response.reasoning_summary_part.done","status":"incomplete","item_id":"rs_0ce8a3aec6cb9147016a8489e6109c87d0adf71b5ab7c85aaf","output_index":0,"part":{"type":"summary_text","text":"summary"},"sequence_number":6,"summary_index":0}"#;

        reset_server_event_json_parse_count();
        let classified = classify_server_event(frame)
            .expect("official string lifecycle status should be accepted");

        assert!(classified.recognized_business);
        assert_eq!(classified.terminal, None);
        assert_eq!(classified.provenance_response_id, None);
        assert_eq!(server_event_json_parse_count(), 1);
    }

    #[test]
    fn current_and_future_response_events_count_as_business_activity() {
        reset_server_event_json_parse_count();
        let unknown = classify_server_event(
            r#"{"type":"response.future_protocol_event","sequence_number":7}"#,
        )
        .expect("future response event should classify");

        assert!(unknown.recognized_business);
        assert_eq!(unknown.terminal, None);
        assert_eq!(server_event_json_parse_count(), 0);

        for kind in [
            "response.in_progress",
            "response.content_part.added",
            "response.content_part.done",
            "response.output_text.done",
            "response.custom_tool_call_input.done",
            "response.function_call_arguments.done",
            "response.reasoning_summary_part.done",
        ] {
            let frame = format!(r#"{{"type":"{kind}"}}"#);
            let known = classify_server_event(frame).expect("known response event should classify");
            assert!(
                known.recognized_business,
                "event was not recognized: {kind}"
            );
        }

        let transport_metadata =
            classify_server_event(r#"{"type":"responsesapi.websocket_timing","response_ms":12}"#)
                .expect("transport metadata should classify");
        assert!(!transport_metadata.recognized_business);

        let private = classify_server_event(r#"{"type":"codex.response.metadata"}"#)
            .expect("private metadata should classify");
        assert!(!private.recognized_business);
        assert_eq!(
            private.codex_relay,
            CodexRelayDirective::SuppressProviderPrivate
        );
    }

    #[test]
    fn codex_rate_limit_event_projects_quota_feedback_headers() {
        let classified = classify_server_event(
            r#"{"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":75.5,"window_minutes":300,"reset_at":1700000000},"secondary":{"used_percent":25.0,"window_minutes":10080,"reset_at":1700600000}},"credits":{"has_credits":true,"unlimited":false,"balance":"12.50"}}"#,
        )
        .expect("rate-limit event should classify");
        assert!(!classified.recognized_business);
        assert_eq!(
            classified.codex_relay,
            CodexRelayDirective::SuppressProviderPrivate
        );
        assert_eq!(
            classified
                .provider_headers
                .get("x-codex-primary-used-percent")
                .map(String::as_str),
            Some("75.5")
        );
        assert_eq!(
            classified
                .provider_headers
                .get("x-codex-secondary-window-minutes")
                .map(String::as_str),
            Some("10080")
        );
        assert_eq!(
            classified
                .provider_headers
                .get("x-codex-credits-balance")
                .map(String::as_str),
            Some("12.50")
        );
    }

    #[test]
    fn malformed_and_duplicate_server_fields_are_rejected() {
        for frame in [
            r#"{"type":"response.output_text.delta","delta":"truncated""#,
            r#"{"type":"response.output_text.delta","delta":"x",}"#,
            r#"{"type":"response.output_text.delta","\u0074ype":"response.completed"}"#,
            r#"{"type":"response.created","response":{"id":"resp-1","\u0069d":"resp-2"}}"#,
        ] {
            assert!(
                classify_server_event(frame).is_err(),
                "malformed or duplicate frame was accepted: {frame}"
            );
        }
    }

    #[test]
    fn duplicate_client_routing_and_identity_fields_are_rejected_before_value_parse() {
        for frame in [
            r#"{"type":"response.create","type":"response.create","model":"gpt-5","client_metadata":{"session_id":"s","thread_id":"t","sub2api_step_correlation_id":"c","sub2api_binding_epoch_id":"e","sub2api_binding_generation":1}}"#,
            r#"{"type":"response.create","model":"gpt-5","client_metadata":{"session_id":"s","\u0073ession_id":"s2","thread_id":"t","sub2api_step_correlation_id":"c","sub2api_binding_epoch_id":"e","sub2api_binding_generation":1}}"#,
        ] {
            assert!(
                parse_response_create(frame, ResponseCreateContext::First).is_err(),
                "duplicate client fields were accepted: {frame}"
            );
        }
    }

    #[test]
    fn conflicting_turn_state_sources_are_rejected() {
        assert!(classify_server_event(
            r#"{"type":"response.output_item.done","headers":{"x-codex-turn-state":"state-1"},"x-codex-turn-state":"state-2"}"#,
        )
        .is_err());
        assert!(classify_server_event(
            r#"{"type":"response.output_item.done","x-codex-turn-state":"state-1","turn_state":"state-2"}"#,
        )
        .is_err());

        let matching = classify_server_event(
            r#"{"type":"response.output_item.done","headers":{"x-codex-turn-state":"state-1"},"turn_state":"state-1"}"#,
        )
        .expect("matching turn-state sources should be accepted");
        assert_eq!(matching.turn_state.as_deref(), Some("state-1"));
    }

    #[test]
    fn official_error_terminals_preserve_bounded_status_code_and_quota_headers() {
        let top_level = classify_server_event(
            r#"{"type":"error","status":429,"error":{"type":"usage_limit_reached","message":"limit reached"},"headers":{"retry-after":"12","x-codex-primary-used-percent":"100.0","x-codex-primary-window-minutes":15}}"#,
        )
        .expect("top-level error should classify")
        .terminal_event
        .expect("top-level error should have a summary");
        assert_eq!(top_level.provider_status_code, Some(429));
        assert_eq!(
            top_level.provider_error_code.as_deref(),
            Some("usage_limit_reached")
        );
        assert_eq!(
            top_level
                .provider_headers
                .get("retry-after")
                .map(String::as_str),
            Some("12")
        );
        assert_eq!(
            top_level
                .provider_headers
                .get("x-codex-primary-window-minutes")
                .map(String::as_str),
            Some("15")
        );

        let failed = classify_server_event(
            r#"{"type":"response.failed","response":{"id":"resp-1","error":{"code":"rate_limit_exceeded","message":"retry later"}}}"#,
        )
        .expect("response.failed should classify")
        .terminal_event
        .expect("response.failed should have a summary");
        assert_eq!(failed.provider_status_code, Some(429));
        assert!(failed
            .provider_error_body
            .as_deref()
            .is_some_and(|body| body.contains("rate_limit_exceeded")));

        let incomplete = classify_server_event(
            r#"{"type":"response.incomplete","response":{"id":"resp-2","incomplete_details":{"reason":"max_output_tokens"}}}"#,
        )
        .expect("response.incomplete should classify")
        .terminal_event
        .expect("response.incomplete should have a summary");
        assert_eq!(incomplete.provider_status_code, Some(200));

        let string_status_with_code = classify_server_event(
            r#"{"type":"error","status":"failed","status_code":503,"error":{"type":"server_error","message":"retry later"}}"#,
        )
        .expect("string lifecycle status and numeric status code should coexist")
        .terminal_event
        .expect("error should have a summary");
        assert_eq!(string_status_with_code.provider_status_code, Some(503));
    }

    #[test]
    fn official_status_fields_reject_conflicts_and_invalid_shapes() {
        assert!(classify_server_event(
            r#"{"type":"error","status":429,"status_code":503,"error":{"type":"server_error"}}"#,
        )
        .is_err());
        let invalid_shape = classify_server_event(
            r#"{"type":"response.output_text.done","status":{"state":"completed"}}"#,
        )
        .expect_err("object lifecycle status should be rejected");
        assert_eq!(
            invalid_shape.message(),
            "official server emitted invalid event schema"
        );
        assert!(classify_server_event(
            r#"{"type":"response.output_text.done","status":"completed","status":"incomplete"}"#,
        )
        .is_err());
    }

    #[test]
    fn terminal_and_boundary_events_expose_response_provenance() {
        let boundary = classify_server_event(
            r#"{"type":"response.output_item.added","response_id":"resp-1","output_index":0}"#,
        )
        .expect("boundary should classify");
        assert_eq!(boundary.provenance_response_id.as_deref(), Some("resp-1"));
        assert_eq!(boundary.terminal, None);

        let terminal =
            classify_server_event(r#"{"type":"response.completed","response":{"id":"resp-1"}}"#)
                .expect("terminal should classify");
        assert_eq!(terminal.provenance_response_id.as_deref(), Some("resp-1"));
        assert_eq!(terminal.terminal_response_id.as_deref(), Some("resp-1"));
        assert_eq!(terminal.terminal, Some(TerminalKind::Completed));
    }

    #[test]
    fn chunked_events_preserve_lifecycle_usage_and_terminal_order() {
        let classification = classify_server_event(
            r#"{"chunks":[{"type":"response.created","response":{"id":"resp-batch"}},{"type":"response.output_text.delta","response_id":"resp-batch","delta":"hi","future_field":{"kept":true}},{"type":"response.completed","response":{"id":"resp-batch","model":"gpt-test","usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}]}"#,
        )
        .expect("batch should classify");

        assert!(classification.recognized_business);
        assert!(classification.created);
        assert_eq!(classification.terminal, Some(TerminalKind::Completed));
        assert_eq!(
            classification.terminal_response_id.as_deref(),
            Some("resp-batch")
        );
        let CodexRelayDirective::ForwardEvents(relay_events) = classification.codex_relay.clone()
        else {
            panic!("explicit Codex batch should expose public events");
        };
        assert_eq!(relay_events.len(), 3);
        assert!(relay_events[1]
            .windows(b"future_field".len())
            .any(|window| window == b"future_field"));
        let usage = classification
            .terminal_event
            .and_then(|event| event.standardized_usage)
            .expect("terminal usage should survive batching");
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn codex_batch_filters_only_explicit_private_events() {
        let classification = classify_server_event(
            r#"{"type":"codex.response.metadata","chunks":[{"type":"codex.rate_limits","rate_limits":{}},{"type":"response.future.delta","future":{"kept":true}},{"provider_future_event":{"unknown":"must survive"}}]}"#,
        )
        .expect("Codex batch should classify");

        let CodexRelayDirective::ForwardEvents(events) = classification.codex_relay else {
            panic!("public event should be retained");
        };
        assert_eq!(events.len(), 2);
        assert!(events[0]
            .windows(b"response.future.delta".len())
            .any(|window| window == b"response.future.delta"));
        assert!(events[1]
            .windows(b"provider_future_event".len())
            .any(|window| window == b"provider_future_event"));
    }

    #[test]
    fn direct_future_response_event_keeps_opaque_chunks() {
        let classification = classify_server_event(
            r#"{"type":"response.future.delta","chunks":[{"future_payload":true}]}"#,
        )
        .expect("future event with opaque chunks should classify");

        assert!(classification.recognized_business);
        assert_eq!(
            classification.codex_relay,
            CodexRelayDirective::ForwardOriginal
        );
    }

    #[test]
    fn cancelled_and_future_incomplete_reasons_are_terminal_without_synthetic_502() {
        let cancelled = classify_server_event(
            r#"{"type":"response.cancelled","response":{"id":"resp-cancelled"}}"#,
        )
        .expect("cancelled should classify");
        assert_eq!(cancelled.terminal, Some(TerminalKind::Cancelled));
        assert_eq!(
            cancelled
                .terminal_event
                .as_ref()
                .and_then(|event| event.provider_status_code),
            Some(499)
        );

        let incomplete = classify_server_event(
            r#"{"type":"response.incomplete","response":{"id":"resp-future","incomplete_details":{"reason":"future_context_boundary"}}}"#,
        )
        .expect("future incomplete reason should classify");
        assert_eq!(incomplete.terminal, Some(TerminalKind::Incomplete));
        assert_eq!(
            incomplete
                .terminal_event
                .as_ref()
                .and_then(|event| event.provider_status_code),
            Some(200)
        );
    }

    #[test]
    fn not_executed_control_uses_the_pinned_adapter_proof() {
        let control = route_control_event(
            RouteControlAction::ClientReconnect,
            Some(MiddleRouteDisposition::Retain),
            "candidate_unavailable",
            &StepFence {
                correlation_id: "step-1".into(),
                binding_epoch_id: "binding-1".into(),
                binding_generation: 1,
            },
            "rejected_before_execution",
            "not_started",
            "proven_not_executed",
            true,
        );
        let value: Value = serde_json::from_str(&control).unwrap();
        assert_eq!(value["adapter_proof_class"], NOT_EXECUTED_PROOF_CLASS);
        assert_eq!(value["adapter_proof_version"], 1);
        assert_eq!(value["scope"], "current_step");
        assert_eq!(value["effective_after"], "immediate");
        assert_eq!(value["middle_route_disposition"], "retain");
    }

    #[test]
    fn close_after_terminal_omits_middle_route_disposition() {
        let control = route_control_event(
            RouteControlAction::CloseAfterTerminal,
            None,
            "account_soft_drained",
            &StepFence {
                correlation_id: "step-1".into(),
                binding_epoch_id: "binding-1".into(),
                binding_generation: 1,
            },
            "terminal",
            "confirmed",
            "terminal",
            false,
        );
        let value: Value = serde_json::from_str(&control).unwrap();
        assert!(!value
            .as_object()
            .expect("control should be an object")
            .contains_key("middle_route_disposition"));
    }

    #[test]
    fn route_controls_for_distinct_attempts_never_reuse_control_ids() {
        let fence = StepFence {
            correlation_id: "step-1".into(),
            binding_epoch_id: "binding-1".into(),
            binding_generation: 1,
        };
        let first: Value = serde_json::from_str(&route_control_event(
            RouteControlAction::ClientReconnect,
            Some(MiddleRouteDisposition::Retain),
            "candidate_unavailable",
            &fence,
            "rejected_before_execution",
            "not_started",
            "proven_not_executed",
            true,
        ))
        .unwrap();
        let second: Value = serde_json::from_str(&route_control_event(
            RouteControlAction::ClientReconnect,
            Some(MiddleRouteDisposition::Retain),
            "candidate_unavailable",
            &fence,
            "rejected_before_execution",
            "not_started",
            "proven_not_executed",
            true,
        ))
        .unwrap();
        assert_ne!(first["control_id"], second["control_id"]);
        assert_ne!(first["aether_attempt_id"], second["aether_attempt_id"]);
    }

    #[test]
    fn large_terminal_payload_retains_only_bounded_usage_and_identity_summary() {
        let frame = json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1",
                "model": "gpt-5",
                "output": [{"type": "message", "content": "x".repeat(8 * 1024 * 1024)}],
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": {"cached_tokens": 4},
                    "output_tokens": 5,
                    "output_tokens_details": {"reasoning_tokens": 2},
                    "total_tokens": 15,
                    "ignored": "y".repeat(1024 * 1024)
                }
            }
        })
        .to_string();
        let classified = classify_server_event(frame.as_bytes()).expect("terminal should parse");
        let summary = classified.terminal_event.expect("terminal summary");
        assert_eq!(summary.response_id.as_deref(), Some("resp-1"));
        assert_eq!(summary.model.as_deref(), Some("gpt-5"));
        let usage = summary.standardized_usage.expect("usage summary");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 4);
        assert_eq!(usage.reasoning_tokens, 2);
        assert!(serde_json::to_vec(&usage).unwrap().len() < 1024);
    }
}
