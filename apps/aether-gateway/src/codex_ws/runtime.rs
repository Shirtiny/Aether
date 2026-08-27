use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aether_codex_ws_connector::{
    CodexWebSocketConnector, IntoClientRequest, Message as OfficialMessage, OutboundRoute,
    WebSocketConnection, WebSocketError, WebSocketProtocolError, WebSocketTlsError,
    WebSocketUrlError, HANDSHAKE_HEADER_NOT_VISIBLE_ASCII_PREFIX,
};
use aether_routing_core::RoutingJsonPatchOperation;
use aether_runtime::AdmissionPermit;
use aether_runtime_state::{
    RateLimitCheck, RateLimitInput, RuntimeSemaphoreConfig, RuntimeSemaphoreError,
    RuntimeSemaphoreLeaseStatus, RuntimeSemaphorePermit,
};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::protocol::{
    MiddleRouteDisposition, OfficialRequestIdentity, ResponseCreateStep, TerminalEventSummary,
    TerminalKind,
};
use super::{
    compact_execution_plan_template, compact_report_context_template, CodexWsCandidateLifecycle,
    CodexWsSettlementCommit, CodexWsStepDisposition, CodexWsStepSettlement, CodexWsUsageCommit,
};
use crate::ai_serving::{
    build_compact_local_openai_responses_stream_plan_and_reports_for_kind_with_required_capabilities,
    AiStreamAttempt, GatewayControlDecision, OPENAI_RESPONSES_STREAM_PLAN_KIND,
};
use crate::codex_profile::{
    apply_codex_concrete_account_profile_to_body_with_policy,
    normalize_codex_turn_metadata_for_profile, CodexConcreteAccountProfile,
    CodexProfileRequestBodyPolicy,
};
use crate::orchestration::{
    apply_local_execution_effect, prepare_pool_attempt_started_effect, LocalExecutionEffect,
    LocalExecutionEffectContext, LocalFailoverClassification,
};
use crate::request_candidate_runtime::record_local_request_candidate_status;
use crate::{AppState, GatewayError};

const OFFICIAL_CODEX_RESPONSES_WS_URL: &str = "wss://chatgpt.com/backend-api/codex/responses";
const OPENAI_BETA_VALUE: &str = "responses_websockets=2026-02-06";
const MAX_INITIAL_CANDIDATES: usize = 16;
const CODEX_WS_PROVIDER_CONCURRENCY_GATE: &str = "codex_ws_provider";
const CODEX_WS_KEY_CONCURRENCY_GATE: &str = "codex_ws_provider_key";
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_WRITE_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_FIRST_BYTE_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_READ_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_TOTAL_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const MAX_RETAINED_RESPONSE_HEADERS: usize = 32;
const MAX_RETAINED_RESPONSE_HEADER_VALUE_BYTES: usize = 256;
const MAX_TERMINAL_ID_BYTES: usize = 256;
const MAX_TERMINAL_MODEL_BYTES: usize = 256;
const MAX_HANDSHAKE_ERROR_BODY_BYTES: usize = 8 * 1024;
const STEP_PERMIT_RELEASE_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_TURN_AUTH_REFRESH_ATTEMPTS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexWsRouteKind {
    TransportDefault,
    Direct,
    Proxy,
}

impl CodexWsRouteKind {
    const fn from_route(route: &OutboundRoute) -> Self {
        match route {
            OutboundRoute::TransportDefault => Self::TransportDefault,
            OutboundRoute::Direct => Self::Direct,
            OutboundRoute::Proxy { .. } => Self::Proxy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexWsTimeouts {
    pub(crate) connect: Duration,
    pub(crate) write: Duration,
    pub(crate) first_byte: Duration,
    pub(crate) read: Duration,
    pub(crate) total: Duration,
}

impl CodexWsTimeouts {
    pub(crate) fn from_plan(plan: &aether_contracts::ExecutionPlan) -> Self {
        let configured = plan.timeouts.as_ref();
        Self {
            connect: timeout_duration(
                configured.and_then(|timeouts| timeouts.connect_ms),
                DEFAULT_CONNECT_TIMEOUT_MS,
            ),
            write: timeout_duration(
                configured.and_then(|timeouts| timeouts.write_ms),
                DEFAULT_WRITE_TIMEOUT_MS,
            ),
            first_byte: timeout_duration(
                configured.and_then(|timeouts| timeouts.first_byte_ms),
                DEFAULT_FIRST_BYTE_TIMEOUT_MS,
            ),
            read: timeout_duration(
                configured.and_then(|timeouts| timeouts.stream_idle_ms.or(timeouts.read_ms)),
                DEFAULT_READ_TIMEOUT_MS,
            ),
            total: timeout_duration(
                configured.and_then(|timeouts| timeouts.total_ms),
                DEFAULT_TOTAL_TIMEOUT_MS,
            ),
        }
    }
}

fn timeout_duration(configured_ms: Option<u64>, default_ms: u64) -> Duration {
    Duration::from_millis(configured_ms.unwrap_or(default_ms).max(1))
}

fn take_outbound_route_for_connect(route: &mut OutboundRoute) -> OutboundRoute {
    std::mem::replace(route, OutboundRoute::Direct)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayFrame {
    Text(Bytes),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PeerError(pub(crate) String);

pub(crate) trait RelayPeer:
    Stream<Item = Result<RelayFrame, PeerError>> + Sink<RelayFrame, Error = PeerError> + Send + Unpin
{
}

impl<T> RelayPeer for T where
    T: Stream<Item = Result<RelayFrame, PeerError>>
        + Sink<RelayFrame, Error = PeerError>
        + Send
        + Unpin
{
}

pub(crate) struct CodexWsCandidate {
    pub(crate) attempt: Option<AiStreamAttempt>,
    pub(crate) provider_id: String,
    pub(crate) endpoint_id: String,
    pub(crate) key_id: String,
    pub(crate) model: String,
    /// Frozen upstream model selected by the planner. `model` remains the
    /// client-visible model and is used to reject a mid-connection switch.
    pub(crate) mapped_model: String,
    pub(crate) body_rules: Option<Arc<serde_json::Value>>,
    pub(crate) provider_body_patch: Arc<[RoutingJsonPatchOperation]>,
    pub(crate) force_body_stream_field: bool,
    pub(crate) enable_model_directives: bool,
    pub(crate) model_directive_mapping: Option<Arc<serde_json::Value>>,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) response_headers: BTreeMap<String, String>,
    pub(crate) account_profile: Option<Arc<CodexConcreteAccountProfile>>,
    pub(crate) report_kind: String,
    pub(crate) binding_identity: UpstreamBindingIdentity,
    pub(crate) adapter: crate::orchestration::ResponsesWebSocketAdapter,
    pub(crate) provider_type: String,
    pub(crate) identity: Option<OfficialRequestIdentity>,
    /// Full, short-lived transport plan retained only until a Standard
    /// provider's physical WebSocket has connected.
    pub(crate) connect_plan: Option<aether_contracts::ExecutionPlan>,
    pub(crate) route: OutboundRoute,
    pub(crate) timeouts: CodexWsTimeouts,
    pub(crate) lifecycle: Arc<CodexWsCandidateLifecycle>,
    /// Snapshot of the generic scheduler-affinity epoch used only to drain a
    /// bound connection after the current response settles. Account safety is
    /// fenced by the shared global/catalog/key generations below; the generic
    /// epoch also changes for unrelated provider telemetry and must never be a
    /// pre-write rejection signal.
    pub(crate) selected_scheduler_epoch: u64,
    pub(crate) provider_concurrent_limit: Option<usize>,
    pub(crate) key_concurrent_limit: Option<usize>,
    pub(crate) key_rpm_limit: Option<u32>,
    pub(crate) shared_global_generation: Option<String>,
    pub(crate) shared_key_generation: String,
    pub(crate) shared_catalog_binding: super::hot_state::CodexWsCatalogBindingLease,
    pub(crate) prewrite_cleanup_permit:
        Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamBindingIdentity {
    adapter: crate::orchestration::ResponsesWebSocketAdapter,
    provider_id: String,
    endpoint_id: String,
    key_id: String,
    websocket_url: String,
    handshake_fingerprint: [u8; 32],
    proxy_fingerprint: [u8; 32],
    transport_profile_fingerprint: [u8; 32],
}

#[cfg(test)]
impl UpstreamBindingIdentity {
    pub(crate) fn for_test(
        adapter: crate::orchestration::ResponsesWebSocketAdapter,
        provider_id: &str,
        endpoint_id: &str,
        key_id: &str,
    ) -> Self {
        Self {
            adapter,
            provider_id: provider_id.to_string(),
            endpoint_id: endpoint_id.to_string(),
            key_id: key_id.to_string(),
            websocket_url: "wss://example.test/v1/responses".to_string(),
            handshake_fingerprint: [1; 32],
            proxy_fingerprint: [2; 32],
            transport_profile_fingerprint: [3; 32],
        }
    }
}

impl CodexWsCandidate {
    pub(crate) fn can_reuse_physical_binding(&self, next: &Self) -> bool {
        self.binding_identity == next.binding_identity
            && (self.adapter != crate::orchestration::ResponsesWebSocketAdapter::Codex
                || (self.model == next.model
                    && self
                        .identity
                        .as_ref()
                        .zip(next.identity.as_ref())
                        .is_some_and(|(current, next)| current.matches_connection_binding(next))))
    }
}

struct CodexWsCandidatePreflight {
    transport: Arc<crate::ai_serving::GatewayProviderTransportSnapshot>,
    adapter: crate::orchestration::ResponsesWebSocketAdapter,
    proxy: Option<aether_contracts::ProxySnapshot>,
    route: OutboundRoute,
}

struct CodexWsHandshakeFailure {
    status_code: u16,
    response_headers: BTreeMap<String, String>,
    error_type: String,
    error_message: String,
    error_body: Option<String>,
    diagnostic_detail: Option<String>,
    route_reason: &'static str,
}

impl CodexWsCandidate {
    pub(crate) fn timeouts(&self) -> CodexWsTimeouts {
        self.timeouts
    }

    pub(crate) fn take_prewrite_cleanup_permit(
        &mut self,
    ) -> Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>> {
        self.prewrite_cleanup_permit.take()
    }

    fn restore_prewrite_cleanup_permit(
        &mut self,
        permit: Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
    ) {
        self.prewrite_cleanup_permit = permit;
    }

    fn planning_attempt(&self) -> &AiStreamAttempt {
        self.attempt
            .as_ref()
            .expect("unconnected Codex WS candidate must retain its planning attempt")
    }

    fn take_planning_attempt(&mut self) -> AiStreamAttempt {
        self.attempt
            .take()
            .expect("unconnected Codex WS candidate must retain its planning attempt")
    }

    fn take_connect_plan(&mut self) -> Option<aether_contracts::ExecutionPlan> {
        self.connect_plan.take()
    }
}

pub(crate) struct ConnectedCandidate {
    pub(crate) candidate: CodexWsCandidate,
    pub(crate) peer: Box<dyn RelayPeer>,
    pub(crate) handshake_turn_state: Option<String>,
}

pub(crate) struct PreparedStep {
    body: String,
    admission: Option<AdmissionPermit>,
    provider_concurrency: Option<RuntimeSemaphorePermit>,
    key_concurrency: Option<RuntimeSemaphorePermit>,
    usage_report: UsageReportReservation,
}

impl PreparedStep {
    pub(crate) fn into_parts(self) -> (String, StepExecutionGuard, UsageReportReservation) {
        (
            self.body,
            StepExecutionGuard {
                admission: self.admission,
                provider_concurrency: self.provider_concurrency,
                key_concurrency: self.key_concurrency,
            },
            self.usage_report,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test(body: String, admission: Option<AdmissionPermit>) -> Self {
        Self {
            body,
            admission,
            provider_concurrency: None,
            key_concurrency: None,
            usage_report: UsageReportReservation {
                permit: None,
                settlement_permit: None,
                plan: None,
                original_request_body: None,
                lifecycle_seed: None,
            },
        }
    }
}

pub(crate) struct StepExecutionGuard {
    admission: Option<AdmissionPermit>,
    provider_concurrency: Option<RuntimeSemaphorePermit>,
    key_concurrency: Option<RuntimeSemaphorePermit>,
}

#[derive(Clone, Default)]
pub(crate) struct StepExecutionLeaseStatus {
    provider: Option<RuntimeSemaphoreLeaseStatus>,
    key: Option<RuntimeSemaphoreLeaseStatus>,
}

impl StepExecutionLeaseStatus {
    pub(crate) fn is_valid(&self) -> bool {
        self.provider
            .as_ref()
            .is_none_or(|status| status.is_valid())
            && self.key.as_ref().is_none_or(|status| status.is_valid())
    }

    pub(crate) async fn lost(&self) {
        match (&self.provider, &self.key) {
            (Some(provider), Some(key)) => {
                tokio::select! {
                    _ = provider.lost() => {}
                    _ = key.lost() => {}
                }
            }
            (Some(provider), None) => provider.lost().await,
            (None, Some(key)) => key.lost().await,
            (None, None) => std::future::pending::<()>().await,
        }
    }
}

impl StepExecutionGuard {
    pub(crate) fn lease_status(&self) -> StepExecutionLeaseStatus {
        StepExecutionLeaseStatus {
            provider: self
                .provider_concurrency
                .as_ref()
                .map(RuntimeSemaphorePermit::lease_status),
            key: self
                .key_concurrency
                .as_ref()
                .map(RuntimeSemaphorePermit::lease_status),
        }
    }

    pub(crate) async fn release(mut self) {
        drop(self.admission.take());
        let provider_release = bounded_runtime_permit_release(self.provider_concurrency.take());
        let key_release = bounded_runtime_permit_release(self.key_concurrency.take());
        let _ = tokio::join!(provider_release, key_release);
    }
}

async fn bounded_runtime_permit_release(permit: Option<RuntimeSemaphorePermit>) {
    let Some(permit) = permit else {
        return;
    };
    match tokio::time::timeout(STEP_PERMIT_RELEASE_TIMEOUT, permit.release()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(
            event_name = "codex_ws_runtime_permit_release_failed",
            log_type = "ops",
            error = %error,
            "Codex WebSocket runtime permit release failed; bounded Drop fallback owns the lease"
        ),
        Err(_) => tracing::warn!(
            event_name = "codex_ws_runtime_permit_release_timeout",
            log_type = "ops",
            timeout_ms = STEP_PERMIT_RELEASE_TIMEOUT.as_millis(),
            "Codex WebSocket runtime permit release timed out; bounded Drop fallback owns the lease"
        ),
    }
}

pub(crate) struct UsageReportReservation {
    permit: Option<tokio::sync::mpsc::OwnedPermit<CodexWsUsageCommit>>,
    settlement_permit: Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
    plan: Option<aether_contracts::ExecutionPlan>,
    original_request_body: Option<serde_json::Value>,
    lifecycle_seed: Option<aether_usage_runtime::LifecycleUsageSeed>,
}

pub(crate) struct CodexWsStepUsageContext {
    plan: aether_contracts::ExecutionPlan,
    report_kind: String,
    report_context: Option<serde_json::Value>,
}

impl CodexWsStepUsageContext {
    pub(crate) fn new(candidate: &CodexWsCandidate, step: &ResponseCreateStep) -> Self {
        let mut plan = candidate.lifecycle.plan().clone();
        plan.request_id = step_usage_request_id(step);
        let report_context =
            step_report_context(candidate.lifecycle.report_context().cloned(), step, None);
        Self {
            plan,
            report_kind: candidate.report_kind.clone(),
            report_context,
        }
    }
}

impl UsageReportReservation {
    fn lifecycle_seed(&self) -> Option<&aether_usage_runtime::LifecycleUsageSeed> {
        self.lifecycle_seed.as_ref()
    }

    fn into_parts(
        mut self,
    ) -> (
        Option<tokio::sync::mpsc::OwnedPermit<CodexWsUsageCommit>>,
        Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
        Option<aether_contracts::ExecutionPlan>,
        Option<serde_json::Value>,
    ) {
        (
            self.permit.take(),
            self.settlement_permit.take(),
            self.plan.take(),
            self.original_request_body.take(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StepPreparationError {
    pub(crate) reason: &'static str,
    pub(crate) middle_route_disposition: MiddleRouteDisposition,
}

impl StepPreparationError {
    pub(crate) const fn retain(reason: &'static str) -> Self {
        Self {
            reason,
            middle_route_disposition: MiddleRouteDisposition::Retain,
        }
    }

    pub(crate) const fn exclude(reason: &'static str) -> Self {
        Self {
            reason,
            middle_route_disposition: MiddleRouteDisposition::Exclude,
        }
    }
}

const fn local_capacity_error(reason: &'static str) -> StepPreparationError {
    StepPreparationError::retain(reason)
}

#[async_trait]
pub(crate) trait CodexWsRuntimePort: Send + Sync {
    fn validate_runtime_fences(&self) -> Result<(), StepPreparationError>;

    fn validate_candidate_fences(
        &self,
        _candidate: &CodexWsCandidate,
    ) -> Result<(), StepPreparationError> {
        self.validate_runtime_fences()
    }

    async fn validate_candidate_current_state(
        &self,
        candidate: &CodexWsCandidate,
    ) -> Result<super::hot_state::CodexWsFenceDecision, StepPreparationError> {
        self.validate_candidate_fences(candidate)?;
        Ok(super::hot_state::CodexWsFenceDecision::Continue)
    }

    async fn validate_step(&self, step: &ResponseCreateStep) -> Result<(), StepPreparationError>;

    async fn select_candidates(
        &self,
        first_step: &ResponseCreateStep,
    ) -> Result<Vec<CodexWsCandidate>, StepPreparationError>;

    async fn connect(
        &self,
        candidate: CodexWsCandidate,
    ) -> Result<ConnectedCandidate, StepPreparationError>;

    async fn activate_reused_candidate(
        &self,
        _candidate: CodexWsCandidate,
    ) -> Result<CodexWsCandidate, StepPreparationError> {
        Err(StepPreparationError::retain(
            "responses_websocket_binding_reuse_unsupported",
        ))
    }

    async fn abort_candidate(&self, candidate: &CodexWsCandidate);

    fn abort_candidate_detached(
        &self,
        candidate: &CodexWsCandidate,
        cleanup_permit: Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
    );

    async fn mark_unused_candidates(&self, candidates: Vec<CodexWsCandidate>);

    fn mark_unused_candidates_detached(&self, candidates: Vec<CodexWsCandidate>);

    async fn prepare_step(
        &self,
        candidate: &CodexWsCandidate,
        step: &mut ResponseCreateStep,
    ) -> Result<PreparedStep, StepPreparationError>;

    async fn release_candidate_scheduling_resources(
        &self,
        candidate: &CodexWsCandidate,
        first_dispatch: bool,
    );

    fn record_step_pending(&self, _usage_context: &CodexWsStepUsageContext) {}

    #[allow(clippy::too_many_arguments)]
    fn record_step_rejected(
        &self,
        _usage_context: CodexWsStepUsageContext,
        _elapsed: std::time::Duration,
        _status_code: u16,
        _error_type: &'static str,
        _error_message: &'static str,
        _cancelled: bool,
    ) {
    }

    fn record_step_stream_started(
        &self,
        _candidate: &CodexWsCandidate,
        _step: &ResponseCreateStep,
        _first_byte_elapsed: std::time::Duration,
        _usage_report: &UsageReportReservation,
    ) {
    }

    fn record_step_terminal(
        &self,
        candidate: &CodexWsCandidate,
        step: &ResponseCreateStep,
        terminal_event: Option<TerminalEventSummary>,
        terminal_kind: Option<TerminalKind>,
        disposition: CodexWsStepDisposition,
        first_dispatch: bool,
        first_byte_elapsed: Option<std::time::Duration>,
        elapsed: std::time::Duration,
        usage_report: UsageReportReservation,
    );

    fn record_codex_quota_headers(&self, _key_id: &str, _headers: BTreeMap<String, String>) {}

    fn scheduler_epoch(&self) -> u64;
}

pub(crate) struct GatewayCodexWsRuntime {
    state: AppState,
    request_headers: HeaderMap,
    request_uri: Uri,
    decision: tokio::sync::RwLock<GatewayControlDecision>,
    trace_id: String,
    shared_global: super::hot_state::CodexWsHotLease,
    remote_ip: std::net::IpAddr,
    auth_context_epoch: AtomicU64,
    usage_report_tx: tokio::sync::mpsc::Sender<CodexWsUsageCommit>,
    settlement_tx: tokio::sync::mpsc::Sender<CodexWsSettlementCommit>,
    connector: CodexWebSocketConnector,
}

struct ConnectAttemptCancellationGuard {
    lifecycle: Arc<CodexWsCandidateLifecycle>,
    cleanup_permit: Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
}

impl ConnectAttemptCancellationGuard {
    fn new(candidate: &mut CodexWsCandidate) -> Self {
        Self {
            lifecycle: Arc::clone(&candidate.lifecycle),
            cleanup_permit: candidate.take_prewrite_cleanup_permit(),
        }
    }

    fn disarm(&mut self) {
        self.cleanup_permit = None;
    }

    fn take_cleanup_permit(
        &mut self,
    ) -> Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>> {
        self.cleanup_permit.take()
    }

    fn restore(mut self, candidate: &mut CodexWsCandidate) {
        candidate.restore_prewrite_cleanup_permit(self.cleanup_permit.take());
    }
}

impl Drop for ConnectAttemptCancellationGuard {
    fn drop(&mut self) {
        if let Some(permit) = self.cleanup_permit.take() {
            permit.send(CodexWsSettlementCommit::CandidateAbort {
                lifecycle: Arc::clone(&self.lifecycle),
                status: aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
                status_code: None,
                error_type: "codex_ws_candidate_future_cancelled",
                error_message: "Codex WS candidate future was cancelled",
            });
        }
    }
}

fn sanitize_runtime_request_headers(mut headers: HeaderMap) -> HeaderMap {
    let connection_scoped_names = headers
        .get_all(http::header::CONNECTION)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .filter_map(|name| HeaderName::from_bytes(name.trim_ascii()).ok())
        .collect::<Vec<_>>();
    for name in connection_scoped_names {
        headers.remove(name);
    }

    for name in [
        http::header::AUTHORIZATION.as_str(),
        "x-api-key",
        "api-key",
        "x-goog-api-key",
        http::header::COOKIE.as_str(),
        http::header::PROXY_AUTHORIZATION.as_str(),
        http::header::CONNECTION.as_str(),
        http::header::UPGRADE.as_str(),
        super::protocol::ROUTE_CONTROL_ACCEPT_HEADER,
        super::protocol::ROUTE_CONTROL_SELECTED_HEADER,
        super::protocol::ROUTE_CONTROL_CAPABILITIES_HEADER,
    ] {
        headers.remove(name);
    }
    let websocket_managed_names = headers
        .keys()
        .filter(|name| name.as_str().starts_with("sec-websocket-"))
        .cloned()
        .collect::<Vec<_>>();
    for name in websocket_managed_names {
        headers.remove(name);
    }
    headers
}

fn sanitize_runtime_request_uri(uri: Uri) -> Result<Uri, PeerError> {
    let Some(query) = uri.query() else {
        return Ok(uri);
    };
    let pairs = url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>();
    if !pairs.iter().any(|(name, _)| name == "key") {
        return Ok(uri);
    }

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        if name != "key" {
            serializer.append_pair(&name, &value);
        }
    }
    let query = serializer.finish();
    let path = uri.path().to_string();
    let path_and_query = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    let mut parts = uri.into_parts();
    parts.path_and_query = Some(
        path_and_query
            .parse()
            .map_err(|_| PeerError("Responses WebSocket request URI is invalid".into()))?,
    );
    Uri::from_parts(parts)
        .map_err(|_| PeerError("Responses WebSocket request URI is invalid".into()))
}

impl GatewayCodexWsRuntime {
    pub(crate) fn new(
        state: AppState,
        request_headers: HeaderMap,
        request_uri: Uri,
        mut decision: GatewayControlDecision,
        trace_id: String,
        shared_global: super::hot_state::CodexWsHotLease,
        remote_ip: std::net::IpAddr,
        auth_context_epoch: u64,
    ) -> Result<Self, PeerError> {
        if decision.auth_context.is_none() {
            return Err(PeerError(
                "Responses WebSocket runtime auth context is missing".into(),
            ));
        }
        let connector = CodexWebSocketConnector::new()
            .map_err(|_| PeerError("failed to initialize pinned Codex WS connector".into()))?;
        let request_headers = sanitize_runtime_request_headers(request_headers);
        let request_uri = sanitize_runtime_request_uri(request_uri)?;
        decision.public_query_string = request_uri.query().map(ToOwned::to_owned);
        let usage_report_tx = state.codex_ws_usage_reporter.sender();
        let settlement_tx = state.codex_ws_usage_reporter.settlement_sender();
        Ok(Self {
            state,
            request_headers,
            request_uri,
            decision: tokio::sync::RwLock::new(decision),
            trace_id,
            shared_global,
            remote_ip,
            auth_context_epoch: AtomicU64::new(auth_context_epoch),
            usage_report_tx,
            settlement_tx,
            connector,
        })
    }

    /// Refresh the mutable request authorization context before every
    /// response.create. The Upgrade-time decision remains the route identity,
    /// while auth state, model permissions, wallet access, and IP rules are
    /// re-read for each turn.
    async fn refresh_turn_decision(&self) -> Result<GatewayControlDecision, StepPreparationError> {
        for _ in 0..MAX_TURN_AUTH_REFRESH_ATTEMPTS {
            let refresh_epoch = self.state.auth_context_invalidation_epoch();
            let mut snapshot = self.decision.read().await.clone();
            // The shared HTTP refresh helper intentionally short-circuits an
            // already denied context. A long-lived WS must be able to recover
            // after a key is re-enabled or a wallet is replenished, so use
            // only the stable identity fields as the refresh seed and always
            // read current authorization state from the repository.
            let can_force_repository_refresh = self.state.has_auth_api_key_reader()
                && snapshot
                    .auth_endpoint_signature
                    .as_deref()
                    .is_some_and(|signature| !signature.trim().is_empty())
                && snapshot.auth_context.as_ref().is_some_and(|context| {
                    !context.user_id.trim().is_empty() && !context.api_key_id.trim().is_empty()
                });
            if can_force_repository_refresh {
                let auth_context = snapshot
                    .auth_context
                    .as_mut()
                    .expect("refresh identity was checked above");
                auth_context.access_allowed = true;
                auth_context.balance_remaining = None;
                auth_context.local_rejection = None;
            }
            let auth_context = crate::control::resolve_execution_runtime_auth_context(
                &self.state,
                &snapshot,
                &self.request_headers,
                &self.request_uri,
                &self.trace_id,
            )
            .await
            .map_err(|_| StepPreparationError::retain("step_auth_refresh_failed"))?;

            if self.state.auth_context_invalidation_epoch() != refresh_epoch {
                continue;
            }

            let mut refreshed = snapshot;
            refreshed.auth_context = auth_context;
            refreshed.local_auth_rejection = refreshed
                .auth_context
                .as_ref()
                .and_then(|auth_context| auth_context.local_rejection.clone());

            *self.decision.write().await = refreshed.clone();
            self.auth_context_epoch
                .store(refresh_epoch, Ordering::Release);
            return Ok(refreshed);
        }
        Err(StepPreparationError::retain(
            "step_principal_snapshot_invalidated",
        ))
    }

    async fn decision_snapshot(&self) -> GatewayControlDecision {
        self.decision.read().await.clone()
    }

    fn request_parts(&self) -> Result<http::request::Parts, PeerError> {
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri(self.request_uri.clone())
            .body(())
            .map_err(|_| PeerError("failed to materialize WS planner request".into()))?;
        let (mut parts, _) = request.into_parts();
        parts.headers = self.request_headers.clone();
        Ok(parts)
    }

    fn official_request(
        &self,
        candidate: &CodexWsCandidate,
    ) -> Result<aether_codex_ws_connector::Request, PeerError> {
        let identity = candidate
            .identity
            .as_ref()
            .ok_or_else(|| PeerError("official Codex WebSocket identity is missing".into()))?;
        let mut request = OFFICIAL_CODEX_RESPONSES_WS_URL
            .into_client_request()
            .map_err(|_| PeerError("failed to build official Codex WS request".into()))?;

        for name in [
            "authorization",
            "chatgpt-account-id",
            "user-agent",
            "originator",
            "version",
            "x-codex-installation-id",
            "x-oai-attestation",
            "x-openai-internal-codex-responses-lite",
        ] {
            if let Some(value) = case_insensitive_btree_value(&candidate.headers, name) {
                insert_header(request.headers_mut(), name, value)?;
            }
        }
        if !request.headers().contains_key(http::header::AUTHORIZATION) {
            return Err(PeerError(
                "selected Codex account has no materialized OAuth authorization".into(),
            ));
        }

        for name in [
            "x-codex-beta-features",
            "x-openai-memgen-request",
            "x-responsesapi-include-timing-metrics",
        ] {
            if let Some(value) = exact_request_header(&self.request_headers, name) {
                insert_header(request.headers_mut(), name, value)?;
            }
        }
        insert_header(request.headers_mut(), "session-id", &identity.session_id)?;
        insert_header(request.headers_mut(), "thread-id", &identity.thread_id)?;
        insert_header(
            request.headers_mut(),
            "x-client-request-id",
            &identity.thread_id,
        )?;
        if let Some(window_id) = identity.window_id.as_deref() {
            insert_header(request.headers_mut(), "x-codex-window-id", window_id)?;
        }
        if let Some(parent_thread_id) = identity.parent_thread_id.as_deref() {
            insert_header(
                request.headers_mut(),
                "x-codex-parent-thread-id",
                parent_thread_id,
            )?;
        }
        if let Some(subagent) = identity.subagent.as_deref() {
            insert_header(request.headers_mut(), "x-openai-subagent", subagent)?;
        }
        if identity.responses_lite {
            insert_header(
                request.headers_mut(),
                "x-openai-internal-codex-responses-lite",
                "true",
            )?;
        }
        if let Some(turn_metadata) = identity.turn_metadata.as_deref() {
            let normalized_turn_metadata;
            let turn_metadata = if let Some(profile) = candidate.account_profile.as_deref() {
                normalized_turn_metadata =
                    normalize_codex_turn_metadata_for_profile(turn_metadata, profile).ok_or_else(
                        || PeerError("x-codex-turn-metadata is not a valid JSON object".into()),
                    )?;
                normalized_turn_metadata.as_str()
            } else {
                turn_metadata
            };
            insert_header(
                request.headers_mut(),
                "x-codex-turn-metadata",
                turn_metadata,
            )?;
        }
        insert_header(request.headers_mut(), "openai-beta", OPENAI_BETA_VALUE)?;

        debug_assert!(request
            .headers()
            .keys()
            .all(|name| !name.as_str().starts_with("x-aether-")));
        Ok(request)
    }

    async fn finish_aborted_candidate(
        &self,
        candidate: &CodexWsCandidate,
        status: aether_data_contracts::repository::candidates::RequestCandidateStatus,
        error_type: &'static str,
        error_message: &str,
        record_health_failure: bool,
    ) {
        let _ = candidate
            .lifecycle
            .abort_before_write(
                &self.state,
                status,
                record_health_failure.then_some(http::StatusCode::BAD_GATEWAY.as_u16()),
                error_type,
                error_message,
            )
            .await;
    }

    async fn acquire_candidate_concurrency(
        &self,
        candidate: &CodexWsCandidate,
    ) -> Result<
        (
            Option<RuntimeSemaphorePermit>,
            Option<RuntimeSemaphorePermit>,
        ),
        StepPreparationError,
    > {
        acquire_candidate_concurrency_permits(
            self.state.runtime_state.as_ref(),
            candidate.provider_id.as_str(),
            candidate.provider_concurrent_limit,
            candidate.key_id.as_str(),
            candidate.key_concurrent_limit,
        )
        .await
    }
}

#[async_trait]
impl CodexWsRuntimePort for GatewayCodexWsRuntime {
    fn validate_runtime_fences(&self) -> Result<(), StepPreparationError> {
        if self.state.auth_context_invalidation_epoch()
            != self.auth_context_epoch.load(Ordering::Acquire)
        {
            return Err(StepPreparationError::retain(
                "step_principal_snapshot_invalidated",
            ));
        }
        Ok(())
    }

    async fn validate_step(&self, step: &ResponseCreateStep) -> Result<(), StepPreparationError> {
        let decision = self.refresh_turn_decision().await?;
        self.validate_runtime_fences()?;
        if crate::control::trusted_auth_local_rejection(Some(&decision), &self.request_headers)
            .is_some()
        {
            return Err(StepPreparationError::retain("step_auth_rejected"));
        }
        let auth_context = decision
            .auth_context
            .as_ref()
            .ok_or(StepPreparationError::retain("step_auth_required"))?;
        if !auth_context.access_allowed
            || !crate::handlers::shared::ip_rules_allow(
                auth_context.ip_rules.as_deref(),
                self.remote_ip,
            )
        {
            return Err(StepPreparationError::retain("step_access_rejected"));
        }
        if crate::control::request_model_local_rejection_from_json(
            &self.state,
            Some(&decision),
            &self.request_uri,
            &step.value,
        )
        .await
        .map_err(|_| StepPreparationError::retain("step_policy_lookup_failed"))?
        .is_some()
        {
            return Err(StepPreparationError::retain("step_policy_rejected"));
        }
        match self
            .state
            .frontdoor_user_rpm()
            .check_and_consume(&self.state, Some(&decision))
            .await
            .map_err(|_| StepPreparationError::retain("step_rate_limit_unavailable"))?
        {
            crate::FrontdoorUserRpmOutcome::NotApplicable
            | crate::FrontdoorUserRpmOutcome::Allowed => Ok(()),
            crate::FrontdoorUserRpmOutcome::Rejected(_) => {
                Err(StepPreparationError::retain("step_rate_limit_rejected"))
            }
        }
    }

    async fn select_candidates(
        &self,
        first_step: &ResponseCreateStep,
    ) -> Result<Vec<CodexWsCandidate>, StepPreparationError> {
        self.validate_runtime_fences()?;
        let decision = self.decision_snapshot().await;
        let shared_global = self.shared_global.clone();
        let shared_catalog = super::hot_state::ensure_catalog_hot_lease(&self.state)
            .await
            .map_err(|_| StepPreparationError::retain("account_catalog_hot_state_unavailable"))?;
        if !shared_catalog.eligible {
            return Err(StepPreparationError::retain(
                "account_catalog_transitioning",
            ));
        }
        super::hot_state::bind_catalog_snapshot_generation(&self.state, &shared_catalog.generation)
            .map_err(|_| StepPreparationError::retain("account_catalog_snapshot_bind_failed"))?;
        let selected_scheduler_epoch = self.state.scheduler_affinity_epoch();
        let parts = self
            .request_parts()
            .map_err(|_| StepPreparationError::retain("candidate_request_context_invalid"))?;
        // Adapter eligibility is provider-scoped. Codex capability/profile
        // requirements are checked in preflight, while Standard providers use
        // their ordinary key contract.
        let required_capabilities = json!({});
        let planning_state = self.state.clone();
        let codex_global_eligible = shared_global.eligible;
        let codex_identity_present = first_step.official_identity.is_some();
        let native_account_flags = crate::provider_transport::CodexOfficialWsGlobalFlags {
            enabled: true,
            native_codex_ws_enabled: true,
        };
        let mut attempts = build_compact_local_openai_responses_stream_plan_and_reports_for_kind_with_required_capabilities(
            &self.state,
            &parts,
            &self.trace_id,
            &decision,
            &first_step.value,
            OPENAI_RESPONSES_STREAM_PLAN_KIND,
            &required_capabilities,
            MAX_INITIAL_CANDIDATES,
            move |attempt| {
                let state = planning_state.clone();
                async move {
                    if !attempt
                        .eligible
                        .provider_api_format
                        .trim()
                        .eq_ignore_ascii_case("openai:responses")
                    {
                        return None;
                    }
                    let transport = Arc::clone(&attempt.eligible.transport);
                    let adapter = crate::orchestration::responses_websocket_adapter(
                        transport.provider.provider_type.as_str(),
                        transport.provider.config.as_ref(),
                    )?;
                    if adapter == crate::orchestration::ResponsesWebSocketAdapter::Codex
                        && (!codex_global_eligible
                            || !codex_identity_present
                            || first_step
                                .value
                                .get("background")
                                .is_some_and(|value| !value.is_null())
                            || !crate::provider_transport::resolve_codex_official_ws(
                                transport.as_ref(),
                                native_account_flags,
                            )
                            .profile_effective)
                    {
                        return None;
                    }
                    let proxy = state
                        .resolve_transport_proxy_snapshot_with_tunnel_affinity(transport.as_ref())
                        .await;
                    let route = if adapter
                        == crate::orchestration::ResponsesWebSocketAdapter::Codex
                    {
                        outbound_route(proxy.as_ref())?
                    } else {
                        OutboundRoute::Direct
                    };
                    Some(CodexWsCandidatePreflight {
                        transport,
                        adapter,
                        proxy,
                        route,
                    })
                }
            },
        )
        .await
        .map_err(|_| StepPreparationError::retain("candidate_planning_failed"))?;

        let mut catalog_resource_seeds = Vec::with_capacity(attempts.len().saturating_mul(2));
        for planned in &attempts {
            let provider = &planned.preflight.transport.provider;
            catalog_resource_seeds.push(super::hot_state::CodexWsCatalogResourceSeed {
                kind: super::hot_state::CatalogResourceKind::Provider,
                id: provider.id.clone(),
                eligible: provider.is_active
                    && crate::orchestration::responses_websocket_adapter(
                        &provider.provider_type,
                        provider.config.as_ref(),
                    )
                    .is_some(),
                ineligible_reason: "provider_ineligible",
            });
            let endpoint = &planned.preflight.transport.endpoint;
            catalog_resource_seeds.push(super::hot_state::CodexWsCatalogResourceSeed {
                kind: super::hot_state::CatalogResourceKind::Endpoint,
                id: endpoint.id.clone(),
                eligible: endpoint.is_active
                    && endpoint
                        .api_format
                        .trim()
                        .eq_ignore_ascii_case("openai:responses"),
                ineligible_reason: "endpoint_ineligible",
            });
        }
        let catalog_resource_leases = match super::hot_state::ensure_catalog_resource_hot_leases(
            &self.state,
            &catalog_resource_seeds,
        )
        .await
        {
            Ok(leases) => leases,
            Err(_) => {
                crate::executor::candidate_loop::mark_unused_local_candidates(
                    &self.state,
                    attempts
                        .into_iter()
                        .map(|planned| planned.attempt)
                        .collect(),
                )
                .await;
                return Err(StepPreparationError::retain(
                    "catalog_resource_hot_state_unavailable",
                ));
            }
        };

        // These settings are request-scoped and must remain frozen for every
        // subsequent response.create on the same provider connection.
        let enable_model_directives =
            crate::system_features::reasoning_model_directive_enabled_for_api_format_and_model(
                &self.state,
                "openai:responses",
                Some(first_step.model.as_str()),
            )
            .await;
        let model_directive_mapping =
            crate::system_features::reasoning_model_directive_mapping_for_api_format_and_model(
                &self.state,
                "openai:responses",
                Some(first_step.model.as_str()),
            )
            .await
            .map(Arc::new);
        let key_ids = attempts
            .iter()
            .map(|planned| planned.attempt.plan.key_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let (key_concurrent_limits, key_rpm_limits, key_hot_leases) =
            match self.state.read_provider_catalog_keys_by_ids(&key_ids).await {
                Ok(keys) => {
                    let now_unix_secs = crate::clock::current_unix_secs();
                    let mut concurrent = BTreeMap::new();
                    let mut rpm = BTreeMap::new();
                    let hot = super::hot_state::ensure_key_hot_leases(&self.state, &keys)
                        .await
                        .map_err(|_| {
                            StepPreparationError::retain("account_hot_state_unavailable")
                        })?;
                    for key in keys {
                        concurrent.insert(
                            key.id.clone(),
                            normalize_concurrent_limit(key.concurrent_limit),
                        );
                        rpm.insert(
                            key.id.clone(),
                            aether_scheduler_core::effective_provider_key_rpm_limit(
                                &key,
                                now_unix_secs,
                            )
                            .and_then(|limit| u32::try_from(limit).ok()),
                        );
                    }
                    (concurrent, rpm, hot)
                }
                Err(_) => {
                    crate::executor::candidate_loop::mark_unused_local_candidates(
                        &self.state,
                        attempts
                            .into_iter()
                            .map(|planned| planned.attempt)
                            .collect(),
                    )
                    .await;
                    return Err(StepPreparationError::retain(
                        "candidate_key_snapshot_unavailable",
                    ));
                }
            };

        if let Err(reason) = super::hot_state::validate_candidate_selection_hot_leases(
            &self.state,
            None,
            &shared_catalog,
        )
        .await
        {
            crate::executor::candidate_loop::mark_unused_local_candidates(
                &self.state,
                attempts
                    .into_iter()
                    .map(|planned| planned.attempt)
                    .collect(),
            )
            .await;
            return Err(StepPreparationError::retain(reason));
        }
        if attempts.iter().any(|planned| {
            planned.preflight.adapter == crate::orchestration::ResponsesWebSocketAdapter::Codex
        }) && super::hot_state::validate_global_hot_lease(&self.state, &shared_global)
            .await
            .is_err()
        {
            let mut standard_attempts = Vec::with_capacity(attempts.len());
            let mut stale_codex_attempts = Vec::new();
            for planned in attempts {
                if planned.preflight.adapter
                    == crate::orchestration::ResponsesWebSocketAdapter::Codex
                {
                    stale_codex_attempts.push(planned.attempt);
                } else {
                    standard_attempts.push(planned);
                }
            }
            crate::executor::candidate_loop::mark_unused_local_candidates(
                &self.state,
                stale_codex_attempts,
            )
            .await;
            attempts = standard_attempts;
            if attempts.is_empty() {
                return Err(StepPreparationError::retain(
                    "codex_ws_global_changed_during_selection",
                ));
            }
        }
        if let Err(error) = self.validate_runtime_fences() {
            crate::executor::candidate_loop::mark_unused_local_candidates(
                &self.state,
                attempts
                    .into_iter()
                    .map(|planned| planned.attempt)
                    .collect(),
            )
            .await;
            return Err(error);
        }
        let mut cleanup_permits = VecDeque::with_capacity(attempts.len());
        for _ in 0..attempts.len() {
            let permit = match self.settlement_tx.clone().try_reserve_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    drop(cleanup_permits);
                    crate::executor::candidate_loop::mark_unused_local_candidates(
                        &self.state,
                        attempts
                            .into_iter()
                            .map(|planned| planned.attempt)
                            .collect(),
                    )
                    .await;
                    return Err(StepPreparationError::retain(
                        "candidate_cleanup_backpressure_unavailable",
                    ));
                }
            };
            cleanup_permits.push_back(permit);
        }
        let mut candidates = Vec::with_capacity(attempts.len());
        let mut hot_rejected_attempts = Vec::new();
        for planned in attempts {
            let mut attempt = planned.attempt;
            let preflight = planned.preflight;
            let adapter = preflight.adapter;
            // Freeze reporting/settlement and the connector route from the
            // same concrete proxy resolution. Never resolve a pool key's
            // proxy again after preflight.
            attempt.plan.proxy =
                if adapter == crate::orchestration::ResponsesWebSocketAdapter::Standard {
                    preflight.proxy.clone()
                } else {
                    preflight.proxy.as_ref().map(compact_proxy_for_plan)
                };
            let provider_body_patch = planned.provider_body_patch;
            let transport = preflight.transport;
            let timeouts = CodexWsTimeouts::from_plan(&attempt.plan);
            let lifecycle = Arc::new(CodexWsCandidateLifecycle::new(
                &attempt.plan,
                attempt.report_context.as_ref(),
            ));
            let provider_id = attempt.plan.provider_id.clone();
            let endpoint_id = attempt.plan.endpoint_id.clone();
            let key_id = attempt.plan.key_id.clone();
            let Some(key_hot_lease) = key_hot_leases.get(&key_id) else {
                drop(cleanup_permits.pop_front());
                hot_rejected_attempts.push(attempt);
                continue;
            };
            if !key_hot_lease.eligible {
                drop(cleanup_permits.pop_front());
                hot_rejected_attempts.push(attempt);
                continue;
            }
            let Some(shared_catalog_binding) =
                catalog_resource_leases.binding(&provider_id, &endpoint_id)
            else {
                drop(cleanup_permits.pop_front());
                hot_rejected_attempts.push(attempt);
                continue;
            };
            let headers = attempt.plan.headers.clone();
            let report_kind = attempt
                .report_kind
                .clone()
                .unwrap_or_else(|| "openai_responses_stream_success".to_string());
            let report_context = compact_report_context_template(attempt.report_context.as_ref());
            let mapped_model = report_context
                .as_ref()
                .and_then(|context| context.get("mapped_model"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    attempt
                        .plan
                        .body
                        .json_body
                        .as_ref()
                        .and_then(|body| body.get("model"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| first_step.model.clone());
            attempt.report_context = report_context.clone();
            let connect_plan = (adapter
                == crate::orchestration::ResponsesWebSocketAdapter::Standard)
                .then(|| attempt.plan.clone());
            let provider_concurrent_limit =
                normalize_concurrent_limit(transport.provider.concurrent_limit);
            let key_concurrent_limit = key_concurrent_limits.get(&key_id).copied().flatten();
            let key_rpm_limit = key_rpm_limits.get(&key_id).copied().flatten();
            let account_profile = (adapter
                == crate::orchestration::ResponsesWebSocketAdapter::Codex)
                .then(|| {
                    crate::ai_serving::resolve_codex_pool_concrete_account_profile(
                        transport.as_ref(),
                    )
                    .map(Arc::new)
                })
                .flatten();
            let body_rules = transport.endpoint.body_rules.clone().map(Arc::new);
            let force_body_stream_field =
                crate::ai_serving::endpoint_config_forces_upstream_stream_policy(
                    transport.endpoint.config.as_ref(),
                );
            let binding_identity = upstream_binding_identity(
                adapter,
                &provider_id,
                &endpoint_id,
                &key_id,
                &attempt.plan,
                preflight.proxy.as_ref(),
                &headers,
                first_step.official_identity.as_ref(),
                account_profile.as_deref(),
                &self.request_headers,
            );
            attempt.plan = compact_execution_plan_template(&attempt.plan);
            candidates.push(CodexWsCandidate {
                attempt: Some(attempt),
                provider_id,
                endpoint_id,
                key_id,
                model: first_step.model.clone(),
                mapped_model,
                body_rules,
                provider_body_patch,
                force_body_stream_field,
                enable_model_directives,
                model_directive_mapping: model_directive_mapping.clone(),
                headers,
                response_headers: BTreeMap::new(),
                account_profile,
                report_kind,
                binding_identity,
                adapter,
                provider_type: transport.provider.provider_type.clone(),
                identity: first_step.official_identity.clone(),
                connect_plan,
                route: preflight.route,
                timeouts,
                lifecycle,
                selected_scheduler_epoch,
                provider_concurrent_limit,
                key_concurrent_limit,
                key_rpm_limit,
                shared_global_generation: (adapter
                    == crate::orchestration::ResponsesWebSocketAdapter::Codex)
                    .then(|| shared_global.generation.clone()),
                shared_key_generation: key_hot_lease.generation.clone(),
                shared_catalog_binding,
                prewrite_cleanup_permit: cleanup_permits.pop_front(),
            });
        }
        if !hot_rejected_attempts.is_empty() {
            crate::executor::candidate_loop::mark_unused_local_candidates(
                &self.state,
                hot_rejected_attempts,
            )
            .await;
        }
        Ok(candidates)
    }

    async fn connect(
        &self,
        mut candidate: CodexWsCandidate,
    ) -> Result<ConnectedCandidate, StepPreparationError> {
        let mut cancellation_guard = ConnectAttemptCancellationGuard::new(&mut candidate);
        if let Err(error) = self.validate_candidate_current_state(&candidate).await {
            self.finish_aborted_candidate(
                &candidate,
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
                "codex_ws_candidate_fence_changed_before_connect",
                error.reason,
                false,
            )
            .await;
            cancellation_guard.disarm();
            return Err(error);
        }
        let official_request = if candidate.adapter
            == crate::orchestration::ResponsesWebSocketAdapter::Codex
        {
            match self.official_request(&candidate) {
                Ok(request) => Some(request),
                Err(error) => {
                    self.finish_aborted_candidate(
                        &candidate,
                        aether_data_contracts::repository::candidates::RequestCandidateStatus::Failed,
                        "codex_ws_request_materialization_failed",
                        &error.0,
                        false,
                    )
                    .await;
                    cancellation_guard.disarm();
                    return Err(StepPreparationError::exclude(
                        "selected_account_materialization_failed",
                    ));
                }
            }
        } else {
            None
        };
        let context = LocalExecutionEffectContext {
            plan: &candidate.planning_attempt().plan,
            report_context: candidate.planning_attempt().report_context.as_ref(),
        };
        if !prepare_pool_attempt_started_effect(&self.state, context).await {
            self.finish_aborted_candidate(
                &candidate,
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Unused,
                "codex_ws_pool_attempt_not_started",
                "Codex WS pool attempt was not eligible to start",
                false,
            )
            .await;
            cancellation_guard.disarm();
            return Err(StepPreparationError::retain("candidate_pool_busy"));
        }
        apply_local_execution_effect(
            &self.state,
            context,
            LocalExecutionEffect::PoolAttemptStarted,
        )
        .await;
        candidate.lifecycle.mark_started();
        let (peer, response_headers, handshake_turn_state): (
            Box<dyn RelayPeer>,
            BTreeMap<String, String>,
            Option<String>,
        ) = match candidate.adapter {
            crate::orchestration::ResponsesWebSocketAdapter::Codex => {
                let request = official_request
                    .expect("Codex candidate request was materialized before pool start");
                let route = take_outbound_route_for_connect(&mut candidate.route);
                let route_kind = CodexWsRouteKind::from_route(&route);
                let connect_result = tokio::time::timeout(
                    candidate.timeouts().connect,
                    self.connector.connect(request, route),
                )
                .await;
                let (connection, response) = match connect_result {
                    Ok(Ok(connected)) => connected,
                    Ok(Err(error)) => {
                        let failure = classify_codex_ws_handshake_failure(error, route_kind);
                        tracing::warn!(
                            event_name = "codex_ws_handshake_failed",
                            log_type = "ops",
                            route_kind = ?route_kind,
                            error_type = %failure.error_type,
                            error_detail = failure.diagnostic_detail.as_deref().unwrap_or("none"),
                            "official Codex WebSocket handshake failed"
                        );
                        self.enqueue_handshake_failure(
                            &candidate,
                            &mut cancellation_guard,
                            &failure,
                        )
                        .await?;
                        cancellation_guard.disarm();
                        return Err(StepPreparationError::retain(failure.route_reason));
                    }
                    Err(_) => {
                        let failure = CodexWsHandshakeFailure {
                            status_code: http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                            response_headers: BTreeMap::new(),
                            error_type: "codex_ws_connect_timeout".to_string(),
                            error_message: "official Codex WebSocket connect timed out".to_string(),
                            error_body: None,
                            diagnostic_detail: None,
                            route_reason: "official_ws_connect_timeout",
                        };
                        self.enqueue_handshake_failure(
                            &candidate,
                            &mut cancellation_guard,
                            &failure,
                        )
                        .await?;
                        cancellation_guard.disarm();
                        return Err(StepPreparationError::retain("official_ws_connect_timeout"));
                    }
                };
                let handshake_turn_state = response
                    .headers()
                    .get("x-codex-turn-state")
                    .and_then(|value| value.to_str().ok())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                (
                    Box::new(OfficialPeer { connection }),
                    compact_codex_response_headers(response.headers()),
                    handshake_turn_state,
                )
            }
            crate::orchestration::ResponsesWebSocketAdapter::Standard => {
                let Some(connect_plan) = candidate.take_connect_plan() else {
                    self.finish_aborted_candidate(
                        &candidate,
                        aether_data_contracts::repository::candidates::RequestCandidateStatus::Failed,
                        "responses_websocket_connect_plan_missing",
                        "standard Responses WebSocket connect plan is missing",
                        false,
                    )
                    .await;
                    cancellation_guard.disarm();
                    return Err(StepPreparationError::exclude(
                        "selected_account_materialization_failed",
                    ));
                };
                match tokio::time::timeout(
                    candidate.timeouts().connect,
                    super::standard_transport::connect_standard_websocket(&connect_plan),
                )
                .await
                {
                    Ok(Ok(connection)) => {
                        (Box::new(connection.peer), connection.response_headers, None)
                    }
                    Ok(Err(error)) => {
                        let failure = classify_standard_ws_handshake_failure(error);
                        tracing::warn!(
                            event_name = "responses_websocket_handshake_failed",
                            log_type = "ops",
                            adapter = "standard",
                            error_type = %failure.error_type,
                            error_detail = failure.diagnostic_detail.as_deref().unwrap_or("none"),
                            "standard Responses WebSocket handshake failed"
                        );
                        self.enqueue_handshake_failure(
                            &candidate,
                            &mut cancellation_guard,
                            &failure,
                        )
                        .await?;
                        cancellation_guard.disarm();
                        return Err(StepPreparationError::retain(failure.route_reason));
                    }
                    Err(_) => {
                        let failure = CodexWsHandshakeFailure {
                            status_code: http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                            response_headers: BTreeMap::new(),
                            error_type: "responses_websocket_connect_timeout".to_string(),
                            error_message: "standard Responses WebSocket connect timed out"
                                .to_string(),
                            error_body: None,
                            diagnostic_detail: None,
                            route_reason: "responses_websocket_connect_timeout",
                        };
                        self.enqueue_handshake_failure(
                            &candidate,
                            &mut cancellation_guard,
                            &failure,
                        )
                        .await?;
                        cancellation_guard.disarm();
                        return Err(StepPreparationError::retain(failure.route_reason));
                    }
                }
            }
        };
        if let Err(error) = self.validate_candidate_current_state(&candidate).await {
            self.finish_aborted_candidate(
                &candidate,
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
                "codex_ws_candidate_fence_changed_after_connect",
                error.reason,
                false,
            )
            .await;
            cancellation_guard.disarm();
            drop(peer);
            return Err(error);
        }
        cancellation_guard.restore(&mut candidate);
        candidate.response_headers = response_headers;
        candidate.headers.clear();
        candidate.connect_plan = None;
        // Pool-start and unused-candidate effects have been transferred to the
        // lifecycle. Do not retain a duplicate plan/context for an idle,
        // potentially long-lived provider connection.
        drop(candidate.take_planning_attempt());
        Ok(ConnectedCandidate {
            candidate,
            peer,
            handshake_turn_state,
        })
    }

    async fn activate_reused_candidate(
        &self,
        mut candidate: CodexWsCandidate,
    ) -> Result<CodexWsCandidate, StepPreparationError> {
        let mut cancellation_guard = ConnectAttemptCancellationGuard::new(&mut candidate);
        if let Err(error) = self.validate_candidate_current_state(&candidate).await {
            self.finish_aborted_candidate(
                &candidate,
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
                "responses_websocket_reused_candidate_fence_changed",
                error.reason,
                false,
            )
            .await;
            cancellation_guard.disarm();
            return Err(error);
        }
        let context = LocalExecutionEffectContext {
            plan: &candidate.planning_attempt().plan,
            report_context: candidate.planning_attempt().report_context.as_ref(),
        };
        if !prepare_pool_attempt_started_effect(&self.state, context).await {
            self.finish_aborted_candidate(
                &candidate,
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Unused,
                "responses_websocket_reused_pool_attempt_not_started",
                "Responses WebSocket reused candidate was not eligible to start",
                false,
            )
            .await;
            cancellation_guard.disarm();
            return Err(StepPreparationError::retain("candidate_pool_busy"));
        }
        apply_local_execution_effect(
            &self.state,
            context,
            LocalExecutionEffect::PoolAttemptStarted,
        )
        .await;
        candidate.lifecycle.mark_started();
        if let Err(error) = self.validate_candidate_current_state(&candidate).await {
            self.finish_aborted_candidate(
                &candidate,
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
                "responses_websocket_reused_candidate_changed_after_start",
                error.reason,
                false,
            )
            .await;
            cancellation_guard.disarm();
            return Err(error);
        }
        cancellation_guard.restore(&mut candidate);
        candidate.headers.clear();
        candidate.connect_plan = None;
        drop(candidate.take_planning_attempt());
        Ok(candidate)
    }

    async fn abort_candidate(&self, candidate: &CodexWsCandidate) {
        self.finish_aborted_candidate(
            candidate,
            aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
            "codex_ws_candidate_aborted_before_write",
            "Codex WS candidate was aborted before the first provider write",
            false,
        )
        .await;
    }

    fn abort_candidate_detached(
        &self,
        candidate: &CodexWsCandidate,
        cleanup_permit: Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
    ) {
        let Some(cleanup_permit) = cleanup_permit else {
            tracing::error!(
                event_name = "codex_ws_prewrite_cleanup_reservation_missing",
                log_type = "ops",
                provider_id = %candidate.provider_id,
                key_id = %candidate.key_id,
                "Codex WebSocket pre-write cleanup reservation was missing"
            );
            return;
        };
        cleanup_permit.send(CodexWsSettlementCommit::CandidateAbort {
            lifecycle: Arc::clone(&candidate.lifecycle),
            status:
                aether_data_contracts::repository::candidates::RequestCandidateStatus::Cancelled,
            status_code: None,
            error_type: "codex_ws_candidate_future_cancelled",
            error_message: "Codex WS candidate future was cancelled",
        });
    }

    async fn mark_unused_candidates(&self, mut candidates: Vec<CodexWsCandidate>) {
        for candidate in &mut candidates {
            drop(candidate.take_prewrite_cleanup_permit());
        }
        crate::executor::candidate_loop::mark_unused_local_candidates(
            &self.state,
            candidates
                .iter_mut()
                .map(CodexWsCandidate::take_planning_attempt)
                .collect(),
        )
        .await;
    }

    fn mark_unused_candidates_detached(&self, mut candidates: Vec<CodexWsCandidate>) {
        if candidates.is_empty() {
            return;
        }
        let cleanup_permit = candidates
            .iter_mut()
            .find_map(CodexWsCandidate::take_prewrite_cleanup_permit);
        for candidate in &mut candidates {
            drop(candidate.take_prewrite_cleanup_permit());
        }
        let attempts = candidates
            .iter_mut()
            .map(CodexWsCandidate::take_planning_attempt)
            .collect();
        let Some(cleanup_permit) = cleanup_permit else {
            tracing::error!(
                event_name = "codex_ws_unused_cleanup_reservation_missing",
                log_type = "ops",
                "Codex WebSocket unused candidate cleanup reservation was missing"
            );
            return;
        };
        cleanup_permit.send(CodexWsSettlementCommit::UnusedCandidates { attempts });
    }

    async fn prepare_step(
        &self,
        candidate: &CodexWsCandidate,
        step: &mut ResponseCreateStep,
    ) -> Result<PreparedStep, StepPreparationError> {
        let usage_report = self
            .usage_report_tx
            .clone()
            .try_reserve_owned()
            .map_err(|_| StepPreparationError::retain("step_usage_backpressure_unavailable"))?;
        let settlement_report = self
            .settlement_tx
            .clone()
            .try_reserve_owned()
            .map_err(|_| {
                StepPreparationError::retain("step_settlement_backpressure_unavailable")
            })?;
        let admission = self
            .state
            .try_acquire_request_permit()
            .await
            .map_err(|_| local_capacity_error("step_admission_unavailable"))?;

        // Build the step-scoped lifecycle context before taking ownership of
        // the body. Later response.create steps may be compaction requests on
        // a reused connection, and their trigger must be visible to usage
        // settlement even though the body is moved for materialization below.
        let lifecycle_report_context =
            step_report_context(candidate.lifecycle.report_context().cloned(), step, None);
        let body = std::mem::take(&mut step.value);
        // The long-lived candidate template intentionally carries no payload,
        // but each settled WS step still needs its own accepted client body for
        // the normal usage-capture pipeline. Keep the clone step-scoped and
        // release it together with the terminal report reservation. Large
        // bodies are cloned under the same bounded blocking CPU budget used by
        // provider-body materialization.
        let (materialized_body, original_request_body) =
            if super::cpu_budget::requires_large_frame_cpu_budget(step.encoded_len) {
                let materialization_cpu =
                    super::cpu_budget::acquire_large_frame_cpu_budget(step.encoded_len)
                        .await
                        .map_err(|_| local_capacity_error("large_frame_cpu_unavailable"))?;
                let mapped_model = candidate.mapped_model.clone();
                let body_rules = candidate.body_rules.clone();
                let provider_body_patch = Arc::clone(&candidate.provider_body_patch);
                let model_directive_mapping = candidate.model_directive_mapping.clone();
                let request_headers = self.request_headers.clone();
                let account_profile = candidate.account_profile.clone();
                let adapter = candidate.adapter;
                let provider_type = candidate.provider_type.clone();
                let force_body_stream_field = candidate.force_body_stream_field;
                let enable_model_directives = candidate.enable_model_directives;
                tokio::task::spawn_blocking(move || {
                    let _materialization_cpu = materialization_cpu;
                    let original_request_body = body.clone();
                    let materialized_body = materialize_codex_ws_step_body(
                        body,
                        &mapped_model,
                        force_body_stream_field,
                        body_rules.as_deref(),
                        &request_headers,
                        enable_model_directives,
                        model_directive_mapping.as_deref(),
                        provider_body_patch.as_ref(),
                        account_profile.as_deref(),
                        adapter,
                        &provider_type,
                    )?;
                    Ok::<_, StepPreparationError>((materialized_body, original_request_body))
                })
                .await
                .map_err(|_| {
                    StepPreparationError::retain("provider_request_body_materialization_failed")
                })??
            } else {
                let original_request_body = body.clone();
                let materialized_body = materialize_codex_ws_step_body(
                    body,
                    &candidate.mapped_model,
                    candidate.force_body_stream_field,
                    candidate.body_rules.as_deref(),
                    &self.request_headers,
                    candidate.enable_model_directives,
                    candidate.model_directive_mapping.as_deref(),
                    candidate.provider_body_patch.as_ref(),
                    candidate.account_profile.as_deref(),
                    candidate.adapter,
                    &candidate.provider_type,
                )?;
                (materialized_body, original_request_body)
            };
        let (provider_concurrency, key_concurrency) =
            self.acquire_candidate_concurrency(candidate).await?;
        if let Err(error) = self.consume_bound_step_rpm(candidate).await {
            let _ = tokio::join!(
                bounded_runtime_permit_release(provider_concurrency),
                bounded_runtime_permit_release(key_concurrency),
            );
            return Err(error);
        }
        let mut usage_plan = candidate.lifecycle.plan().clone();
        usage_plan.request_id = step_usage_request_id(step);
        usage_plan.body = aether_contracts::RequestBody::from_json(materialized_body.json);
        let lifecycle_seed = aether_usage_runtime::build_lifecycle_usage_seed(
            &usage_plan,
            lifecycle_report_context.as_ref(),
        );
        Ok(PreparedStep {
            body: materialized_body.text,
            admission,
            provider_concurrency,
            key_concurrency,
            usage_report: UsageReportReservation {
                permit: Some(usage_report),
                settlement_permit: Some(settlement_report),
                plan: Some(usage_plan),
                original_request_body: Some(original_request_body),
                lifecycle_seed: Some(lifecycle_seed),
            },
        })
    }

    async fn release_candidate_scheduling_resources(
        &self,
        candidate: &CodexWsCandidate,
        first_dispatch: bool,
    ) {
        if !first_dispatch {
            return;
        }
        candidate.lifecycle.stop_pool_sticky_renewer();
        candidate
            .lifecycle
            .release_pool_lease_once(&self.state)
            .await;
    }

    fn record_step_pending(&self, usage_context: &CodexWsStepUsageContext) {
        let seed = aether_usage_runtime::build_lifecycle_usage_seed(
            &usage_context.plan,
            usage_context.report_context.as_ref(),
        );
        self.state
            .usage_runtime
            .record_pending(self.state.data.as_ref(), seed);
    }

    fn record_step_rejected(
        &self,
        usage_context: CodexWsStepUsageContext,
        elapsed: std::time::Duration,
        status_code: u16,
        error_type: &'static str,
        error_message: &'static str,
        cancelled: bool,
    ) {
        let CodexWsStepUsageContext {
            plan,
            report_kind,
            report_context,
        } = usage_context;
        let request_id = plan.request_id.clone();
        let model = plan.model_name.clone();
        let context_seed =
            aether_usage_runtime::build_terminal_usage_context_seed(&plan, report_context.as_ref());
        let payload = crate::usage::GatewayStreamReportRequest {
            trace_id: request_id,
            report_kind,
            report_context,
            status_code,
            headers: BTreeMap::new(),
            provider_body_base64: None,
            provider_body_state: None,
            client_body_base64: None,
            client_body_state: None,
            terminal_summary: Some(aether_contracts::ExecutionStreamTerminalSummary {
                standardized_usage: None,
                finish_reason: None,
                response_id: None,
                model,
                observed_finish: false,
                unknown_event_count: 0,
                parser_error: Some(format!("{error_type}: {error_message}")),
            }),
            telemetry: Some(aether_contracts::ExecutionTelemetry {
                ttfb_ms: None,
                elapsed_ms: u64::try_from(elapsed.as_millis()).ok(),
                upstream_bytes: None,
            }),
        };
        let payload_seed = aether_usage_runtime::build_stream_terminal_usage_payload_seed(&payload);
        self.state.usage_runtime.record_stream_terminal(
            self.state.data.as_ref(),
            context_seed,
            payload_seed,
            cancelled,
        );
    }

    fn record_step_stream_started(
        &self,
        _candidate: &CodexWsCandidate,
        _step: &ResponseCreateStep,
        first_byte_elapsed: std::time::Duration,
        usage_report: &UsageReportReservation,
    ) {
        let Some(seed) = usage_report.lifecycle_seed() else {
            return;
        };
        let first_byte_ms = u64::try_from(first_byte_elapsed.as_millis()).ok();
        let telemetry = aether_contracts::ExecutionTelemetry {
            ttfb_ms: first_byte_ms,
            elapsed_ms: first_byte_ms,
            upstream_bytes: None,
        };
        self.state.usage_runtime.record_stream_started(
            self.state.data.as_ref(),
            seed,
            http::StatusCode::OK.as_u16(),
            Some(&telemetry),
        );
    }

    fn record_step_terminal(
        &self,
        candidate: &CodexWsCandidate,
        step: &ResponseCreateStep,
        terminal_event: Option<TerminalEventSummary>,
        terminal_kind: Option<TerminalKind>,
        disposition: CodexWsStepDisposition,
        first_dispatch: bool,
        first_byte_elapsed: Option<std::time::Duration>,
        elapsed: std::time::Duration,
        usage_report: UsageReportReservation,
    ) {
        let step_settlement = if first_dispatch {
            candidate.lifecycle.first_settlement(disposition.clone())
        } else {
            CodexWsStepSettlement::subsequent(Arc::clone(&candidate.lifecycle), disposition.clone())
        };
        step_settlement.stop_candidate_renewer();
        let trace_id = step_usage_request_id(step);
        let report_kind = candidate.report_kind.clone();
        let mut response_headers = candidate.response_headers.clone();
        if let Some(terminal_event) = terminal_event.as_ref() {
            response_headers.extend(terminal_event.provider_headers.clone());
        }
        let fallback_model = step.model.clone();
        let first_byte_ms =
            first_byte_elapsed.and_then(|duration| u64::try_from(duration.as_millis()).ok());
        let elapsed_ms = u64::try_from(elapsed.as_millis()).ok();
        let terminal_summary = compact_terminal_summary(
            terminal_event.as_ref(),
            terminal_kind,
            &fallback_model,
            &disposition,
        );
        drop(terminal_event);
        let status_code = disposition_status_code(&disposition);
        let cancelled = matches!(disposition, CodexWsStepDisposition::Cancelled { .. });
        let (usage_permit, settlement_permit, reserved_plan, original_request_body) =
            usage_report.into_parts();
        let report_context = step_report_context(
            candidate.lifecycle.report_context().cloned(),
            step,
            original_request_body,
        );
        let mut plan = reserved_plan.unwrap_or_else(|| candidate.lifecycle.plan().clone());
        plan.request_id = trace_id.clone();
        let Some(settlement_permit) = settlement_permit else {
            tracing::warn!(
                event_name = "codex_ws_settlement_reservation_missing",
                log_type = "ops",
                provider_id = %plan.provider_id,
                key_id = %plan.key_id,
                "Codex WebSocket terminal settlement reservation was missing"
            );
            return;
        };
        settlement_permit.send(CodexWsSettlementCommit::Step {
            plan,
            trace_id,
            report_kind,
            report_context,
            response_headers,
            terminal_summary,
            status_code,
            first_byte_ms,
            elapsed_ms,
            cancelled,
            step_settlement,
            usage_permit,
        });
    }

    fn record_codex_quota_headers(&self, key_id: &str, headers: BTreeMap<String, String>) {
        if headers.is_empty() {
            return;
        }
        match self.settlement_tx.try_reserve() {
            Ok(permit) => permit.send(CodexWsSettlementCommit::QuotaHeaders {
                key_id: key_id.to_string(),
                headers,
            }),
            Err(_) => tracing::warn!(
                event_name = "codex_ws_quota_feedback_backpressure",
                log_type = "ops",
                key_id,
                "Codex WebSocket quota feedback queue was full"
            ),
        }
    }

    fn scheduler_epoch(&self) -> u64 {
        self.state.scheduler_affinity_epoch()
    }

    async fn validate_candidate_current_state(
        &self,
        candidate: &CodexWsCandidate,
    ) -> Result<super::hot_state::CodexWsFenceDecision, StepPreparationError> {
        self.validate_candidate_fences(candidate)?;
        super::hot_state::validate_hot_leases(
            &self.state,
            &candidate.provider_id,
            &candidate.endpoint_id,
            &candidate.key_id,
            candidate.shared_global_generation.as_deref(),
            &candidate.shared_key_generation,
            &candidate.shared_catalog_binding,
        )
        .await
        .map_err(StepPreparationError::retain)
    }
}

impl GatewayCodexWsRuntime {
    async fn enqueue_handshake_failure(
        &self,
        candidate: &CodexWsCandidate,
        cancellation_guard: &mut ConnectAttemptCancellationGuard,
        failure: &CodexWsHandshakeFailure,
    ) -> Result<(), StepPreparationError> {
        let Some(permit) = cancellation_guard.take_cleanup_permit() else {
            return Err(StepPreparationError::retain(
                "handshake_settlement_reservation_missing",
            ));
        };
        if !candidate.lifecycle.claim_handshake_failure_settlement() {
            drop(permit);
            return Err(StepPreparationError::retain(
                "handshake_terminal_claim_unavailable",
            ));
        }
        permit.send(CodexWsSettlementCommit::HandshakeFailure {
            lifecycle: Arc::clone(&candidate.lifecycle),
            status_code: failure.status_code,
            response_headers: failure.response_headers.clone(),
            error_type: failure.error_type.clone(),
            error_message: failure.error_message.clone(),
            error_body: failure.error_body.clone(),
        });
        candidate
            .lifecycle
            .prepare_failover_after_handshake_failure(&self.state)
            .await;
        Ok(())
    }

    async fn consume_bound_step_rpm(
        &self,
        candidate: &CodexWsCandidate,
    ) -> Result<(), StepPreparationError> {
        let Some(limit) = candidate.key_rpm_limit.filter(|limit| *limit > 0) else {
            return Ok(());
        };
        let now_unix_secs = crate::clock::current_unix_secs();
        let bucket = now_unix_secs / 60;
        let user_key = format!("codex-ws:rpm:noop:{bucket}");
        let key_key = format!("codex-ws:rpm:key:{}:{bucket}", candidate.key_id);
        match self
            .state
            .runtime_state
            .check_and_consume_rate_limit(RateLimitInput {
                user_key: &user_key,
                key_key: &key_key,
                bucket,
                user_limit: 0,
                key_limit: limit,
                ttl_seconds: 120,
            })
            .await
            .map_err(|_| StepPreparationError::retain("provider_key_rpm_state_unavailable"))?
        {
            RateLimitCheck::Allowed { .. } => Ok(()),
            RateLimitCheck::Rejected { .. } => {
                Err(StepPreparationError::retain("provider_key_rpm_exhausted"))
            }
        }
    }
}

fn classify_codex_ws_handshake_failure(
    error: WebSocketError,
    route_kind: CodexWsRouteKind,
) -> CodexWsHandshakeFailure {
    match error {
        WebSocketError::Http(response) => {
            let status_code = response.status().as_u16();
            let response_headers = compact_codex_response_headers(response.headers());
            let error_body = response.body().as_ref().and_then(|body| {
                let body = &body[..body.len().min(MAX_HANDSHAKE_ERROR_BODY_BYTES)];
                (!body.is_empty()).then(|| String::from_utf8_lossy(body).into_owned())
            });
            let (error_type, error_message, route_reason) = match status_code {
                401 | 403 => (
                    "codex_ws_handshake_unauthorized",
                    "official Codex WebSocket rejected account authorization",
                    "official_ws_account_unauthorized",
                ),
                409 => (
                    "codex_ws_handshake_connection_limit",
                    "official Codex WebSocket account connection limit was reached",
                    "official_ws_connection_limit",
                ),
                429 => (
                    "codex_ws_handshake_rate_limited",
                    "official Codex WebSocket account was rate limited",
                    "official_ws_account_rate_limited",
                ),
                500..=599 => (
                    "codex_ws_handshake_upstream_failure",
                    "official Codex WebSocket handshake failed upstream",
                    "official_ws_upstream_failure",
                ),
                _ => (
                    "codex_ws_handshake_http_error",
                    "official Codex WebSocket handshake was rejected",
                    "official_ws_handshake_rejected",
                ),
            };
            CodexWsHandshakeFailure {
                status_code,
                response_headers,
                error_type: error_type.to_string(),
                error_message: error_message.to_string(),
                error_body,
                diagnostic_detail: Some(format!("http_status={status_code}")),
                route_reason,
            }
        }
        WebSocketError::Io(error) => {
            let error_type = match route_kind {
                CodexWsRouteKind::Proxy => "codex_ws_handshake_proxy_io_error",
                CodexWsRouteKind::TransportDefault | CodexWsRouteKind::Direct => {
                    "codex_ws_handshake_io_error"
                }
            };
            transport_handshake_failure(
                error_type,
                "official Codex WebSocket transport I/O failed",
                "official_ws_handshake_io_failed",
                Some(format!("io_kind={}", io_error_kind_name(error.kind()))),
            )
        }
        WebSocketError::Tls(error) => transport_handshake_failure(
            "codex_ws_handshake_tls_error",
            "official Codex WebSocket TLS handshake failed",
            "official_ws_handshake_tls_failed",
            Some(format!("tls_kind={}", tls_error_kind_name(&error))),
        ),
        WebSocketError::Protocol(error) => transport_handshake_failure(
            "codex_ws_handshake_protocol_error",
            "official Codex WebSocket protocol handshake failed",
            "official_ws_handshake_protocol_failed",
            Some(format!(
                "protocol_kind={}",
                protocol_error_kind_name(&error)
            )),
        ),
        WebSocketError::Url(error) => {
            let (error_type, error_message, route_reason) = match &error {
                WebSocketUrlError::ProxyConnect(_) => (
                    "codex_ws_handshake_proxy_connect_error",
                    "official Codex WebSocket proxy CONNECT failed",
                    "official_ws_proxy_connect_failed",
                ),
                _ => (
                    "codex_ws_handshake_url_error",
                    "official Codex WebSocket transport route was invalid",
                    "official_ws_handshake_url_failed",
                ),
            };
            let diagnostic_detail = match &error {
                WebSocketUrlError::ProxyConnect(detail) => Some(proxy_connect_diagnostic(detail)),
                _ => Some(format!("url_kind={}", url_error_kind_name(&error))),
            };
            transport_handshake_failure(error_type, error_message, route_reason, diagnostic_detail)
        }
        WebSocketError::HttpFormat(_) => transport_handshake_failure(
            "codex_ws_handshake_http_format_error",
            "official Codex WebSocket HTTP handshake was invalid",
            "official_ws_handshake_http_format_failed",
            Some("http_format_error".to_string()),
        ),
        WebSocketError::Capacity(_) => transport_handshake_failure(
            "codex_ws_handshake_capacity_error",
            "official Codex WebSocket handshake exceeded a transport limit",
            "official_ws_handshake_capacity_failed",
            Some("capacity_error".to_string()),
        ),
        WebSocketError::Utf8(error) => transport_handshake_failure(
            "codex_ws_handshake_utf8_error",
            "official Codex WebSocket handshake contained invalid text",
            "official_ws_handshake_utf8_failed",
            Some(handshake_utf8_diagnostic(&error)),
        ),
        WebSocketError::ConnectionClosed => transport_handshake_failure(
            "codex_ws_handshake_connection_closed",
            "official Codex WebSocket closed during handshake",
            "official_ws_handshake_connection_closed",
            Some("connection_closed".to_string()),
        ),
        WebSocketError::AlreadyClosed => transport_handshake_failure(
            "codex_ws_handshake_already_closed",
            "official Codex WebSocket transport was already closed",
            "official_ws_handshake_already_closed",
            Some("already_closed".to_string()),
        ),
        WebSocketError::WriteBufferFull(_) => transport_handshake_failure(
            "codex_ws_handshake_write_buffer_full",
            "official Codex WebSocket handshake write buffer was full",
            "official_ws_handshake_write_buffer_full",
            Some("write_buffer_full".to_string()),
        ),
        WebSocketError::AttackAttempt => transport_handshake_failure(
            "codex_ws_handshake_attack_attempt",
            "official Codex WebSocket handshake was rejected as unsafe",
            "official_ws_handshake_attack_attempt",
            Some("attack_attempt".to_string()),
        ),
    }
}

fn classify_standard_ws_handshake_failure(
    error: super::standard_transport::StandardWebSocketConnectError,
) -> CodexWsHandshakeFailure {
    match error {
        super::standard_transport::StandardWebSocketConnectError::Rejected {
            status_code,
            response_headers,
            error_body,
        } => {
            let (error_type, error_message, route_reason) = match status_code {
                401 | 403 => (
                    "responses_websocket_handshake_unauthorized",
                    "standard Responses WebSocket rejected provider authorization",
                    "responses_websocket_account_unauthorized",
                ),
                409 => (
                    "responses_websocket_handshake_connection_limit",
                    "standard Responses WebSocket provider connection limit was reached",
                    "responses_websocket_connection_limit",
                ),
                429 => (
                    "responses_websocket_handshake_rate_limited",
                    "standard Responses WebSocket provider was rate limited",
                    "responses_websocket_account_rate_limited",
                ),
                500..=599 => (
                    "responses_websocket_handshake_upstream_failure",
                    "standard Responses WebSocket handshake failed upstream",
                    "responses_websocket_upstream_failure",
                ),
                _ => (
                    "responses_websocket_handshake_http_error",
                    "standard Responses WebSocket handshake was rejected",
                    "responses_websocket_handshake_rejected",
                ),
            };
            CodexWsHandshakeFailure {
                status_code,
                response_headers,
                error_type: error_type.to_string(),
                error_message: error_message.to_string(),
                error_body,
                diagnostic_detail: Some(format!("http_status={status_code}")),
                route_reason,
            }
        }
        super::standard_transport::StandardWebSocketConnectError::Transport(error) => {
            CodexWsHandshakeFailure {
                status_code: http::StatusCode::BAD_GATEWAY.as_u16(),
                response_headers: BTreeMap::new(),
                error_type: "responses_websocket_handshake_failed".to_string(),
                error_message: "standard Responses WebSocket handshake failed".to_string(),
                error_body: None,
                diagnostic_detail: Some(error.0),
                route_reason: "responses_websocket_handshake_failed",
            }
        }
    }
}

fn transport_handshake_failure(
    error_type: &'static str,
    error_message: &'static str,
    route_reason: &'static str,
    diagnostic_detail: Option<String>,
) -> CodexWsHandshakeFailure {
    CodexWsHandshakeFailure {
        status_code: http::StatusCode::BAD_GATEWAY.as_u16(),
        response_headers: BTreeMap::new(),
        error_type: error_type.to_string(),
        error_message: error_message.to_string(),
        error_body: diagnostic_detail.clone(),
        diagnostic_detail,
        route_reason,
    }
}

fn handshake_utf8_diagnostic(error: &str) -> String {
    handshake_utf8_header_name(error)
        .map(|name| format!("utf8_header={name}"))
        .unwrap_or_else(|| "utf8_error".to_string())
}

fn handshake_utf8_header_name(error: &str) -> Option<String> {
    let candidate = error
        .strip_prefix(HANDSHAKE_HEADER_NOT_VISIBLE_ASCII_PREFIX)
        .map(str::trim)
        .or_else(|| {
            error
                .split_once("for header name '")
                .and_then(|(_, suffix)| suffix.split_once('\'').map(|(name, _)| name))
        })?;
    HeaderName::from_bytes(candidate.as_bytes())
        .ok()
        .map(|name| name.as_str().to_string())
}

fn io_error_kind_name(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind;

    match kind {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::ConnectionRefused => "connection_refused",
        ErrorKind::ConnectionReset => "connection_reset",
        ErrorKind::HostUnreachable => "host_unreachable",
        ErrorKind::NetworkUnreachable => "network_unreachable",
        ErrorKind::ConnectionAborted => "connection_aborted",
        ErrorKind::NotConnected => "not_connected",
        ErrorKind::AddrInUse => "address_in_use",
        ErrorKind::AddrNotAvailable => "address_not_available",
        ErrorKind::NetworkDown => "network_down",
        ErrorKind::BrokenPipe => "broken_pipe",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::WouldBlock => "would_block",
        ErrorKind::NotADirectory => "not_a_directory",
        ErrorKind::IsADirectory => "is_a_directory",
        ErrorKind::DirectoryNotEmpty => "directory_not_empty",
        ErrorKind::ReadOnlyFilesystem => "read_only_filesystem",
        ErrorKind::StaleNetworkFileHandle => "stale_network_file_handle",
        ErrorKind::InvalidInput => "invalid_input",
        ErrorKind::InvalidData => "invalid_data",
        ErrorKind::TimedOut => "timed_out",
        ErrorKind::WriteZero => "write_zero",
        ErrorKind::StorageFull => "storage_full",
        ErrorKind::NotSeekable => "not_seekable",
        ErrorKind::QuotaExceeded => "quota_exceeded",
        ErrorKind::FileTooLarge => "file_too_large",
        ErrorKind::ResourceBusy => "resource_busy",
        ErrorKind::ExecutableFileBusy => "executable_file_busy",
        ErrorKind::Deadlock => "deadlock",
        ErrorKind::CrossesDevices => "crosses_devices",
        ErrorKind::TooManyLinks => "too_many_links",
        ErrorKind::InvalidFilename => "invalid_filename",
        ErrorKind::ArgumentListTooLong => "argument_list_too_long",
        ErrorKind::Interrupted => "interrupted",
        ErrorKind::Unsupported => "unsupported",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        ErrorKind::OutOfMemory => "out_of_memory",
        ErrorKind::Other => "other",
        _ => "unknown",
    }
}

fn tls_error_kind_name(error: &WebSocketTlsError) -> &'static str {
    match error {
        WebSocketTlsError::Rustls(_) => "rustls",
        WebSocketTlsError::InvalidDnsName => "invalid_dns_name",
        _ => "other",
    }
}

fn protocol_error_kind_name(error: &WebSocketProtocolError) -> &'static str {
    match error {
        WebSocketProtocolError::WrongHttpMethod => "wrong_http_method",
        WebSocketProtocolError::WrongHttpVersion => "wrong_http_version",
        WebSocketProtocolError::MissingConnectionUpgradeHeader => {
            "missing_connection_upgrade_header"
        }
        WebSocketProtocolError::MissingUpgradeWebSocketHeader => "missing_upgrade_websocket_header",
        WebSocketProtocolError::MissingSecWebSocketVersionHeader => {
            "missing_sec_websocket_version_header"
        }
        WebSocketProtocolError::MissingSecWebSocketKey => "missing_sec_websocket_key",
        WebSocketProtocolError::SecWebSocketAcceptKeyMismatch => {
            "sec_websocket_accept_key_mismatch"
        }
        WebSocketProtocolError::SecWebSocketSubProtocolError(_) => {
            "sec_websocket_subprotocol_error"
        }
        WebSocketProtocolError::InvalidExtensionsHeader(_) => "invalid_extensions_header",
        WebSocketProtocolError::JunkAfterRequest => "junk_after_request",
        WebSocketProtocolError::CustomResponseSuccessful => "custom_response_successful",
        WebSocketProtocolError::InvalidHeader(_) => "invalid_header",
        WebSocketProtocolError::HandshakeIncomplete => "handshake_incomplete",
        WebSocketProtocolError::HttparseError(_) => "http_parse_error",
        WebSocketProtocolError::SendAfterClosing => "send_after_closing",
        WebSocketProtocolError::ReceivedAfterClosing => "received_after_closing",
        WebSocketProtocolError::NonZeroReservedBits => "non_zero_reserved_bits",
        WebSocketProtocolError::UnmaskedFrameFromClient => "unmasked_frame_from_client",
        WebSocketProtocolError::MaskedFrameFromServer => "masked_frame_from_server",
        WebSocketProtocolError::FragmentedControlFrame => "fragmented_control_frame",
        WebSocketProtocolError::CompressedControlFrame => "compressed_control_frame",
        WebSocketProtocolError::ControlFrameTooBig => "control_frame_too_big",
        WebSocketProtocolError::UnknownControlFrameType(_) => "unknown_control_frame_type",
        WebSocketProtocolError::UnknownDataFrameType(_) => "unknown_data_frame_type",
        WebSocketProtocolError::UnexpectedContinueFrame => "unexpected_continue_frame",
        WebSocketProtocolError::CompressedContinueFrame => "compressed_continue_frame",
        WebSocketProtocolError::ExpectedFragment(_) => "expected_fragment",
        WebSocketProtocolError::ResetWithoutClosingHandshake => "reset_without_closing_handshake",
        WebSocketProtocolError::InvalidOpcode(_) => "invalid_opcode",
        WebSocketProtocolError::InvalidCloseSequence => "invalid_close_sequence",
        WebSocketProtocolError::CompressionFailure(_) => "compression_failure",
    }
}

fn url_error_kind_name(error: &WebSocketUrlError) -> &'static str {
    match error {
        WebSocketUrlError::TlsFeatureNotEnabled => "tls_feature_not_enabled",
        WebSocketUrlError::NoHostName => "no_host_name",
        WebSocketUrlError::UnableToConnect(_) => "unable_to_connect",
        WebSocketUrlError::UnsupportedUrlScheme => "unsupported_url_scheme",
        WebSocketUrlError::EmptyHostName => "empty_host_name",
        WebSocketUrlError::NoPathOrQuery => "no_path_or_query",
        WebSocketUrlError::UnsupportedProxyScheme => "unsupported_proxy_scheme",
        WebSocketUrlError::InvalidProxyConfig(_) => "invalid_proxy_config",
        WebSocketUrlError::ProxyConnect(_) => "proxy_connect",
    }
}

fn proxy_connect_diagnostic(detail: &str) -> String {
    const HTTP_STATUS_PREFIX: &str = "HTTP CONNECT failed with status ";
    if let Some(status) = detail
        .strip_prefix(HTTP_STATUS_PREFIX)
        .and_then(|status| status.parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status))
    {
        return format!("proxy_connect_http_status={status}");
    }

    let reason = match detail {
        "HTTP CONNECT response too large" => "response_too_large",
        "SOCKS5: proxy requested auth, but none provided" => "auth_required",
        "SOCKS5: no acceptable authentication method" => "no_acceptable_auth_method",
        "SOCKS5: unsupported authentication method" => "unsupported_auth_method",
        "SOCKS5 authentication failed" => "authentication_failed",
        "SOCKS5: invalid response version" => "invalid_response_version",
        "SOCKS5: invalid address type" => "invalid_address_type",
        _ => return "url_kind=proxy_connect".to_string(),
    };
    format!("proxy_connect_reason={reason}")
}

pub(crate) fn step_usage_request_id(step: &ResponseCreateStep) -> String {
    let source = format!(
        "{}:{}:{}",
        step.fence.binding_epoch_id, step.fence.binding_generation, step.fence.correlation_id
    );
    format!(
        "ws-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, source.as_bytes())
    )
}

fn compact_terminal_summary(
    terminal_event: Option<&TerminalEventSummary>,
    terminal_kind: Option<TerminalKind>,
    fallback_model: &str,
    disposition: &CodexWsStepDisposition,
) -> aether_contracts::ExecutionStreamTerminalSummary {
    let standardized_usage = terminal_event
        .and_then(|event| event.standardized_usage.clone())
        .filter(aether_contracts::StandardizedUsage::has_token_signal);
    let response_id = bounded_terminal_string(
        terminal_event.and_then(|event| event.response_id.as_deref()),
        MAX_TERMINAL_ID_BYTES,
    );
    let model = bounded_terminal_string(
        terminal_event.and_then(|event| event.model.as_deref()),
        MAX_TERMINAL_MODEL_BYTES,
    )
    .or_else(|| bounded_terminal_string(Some(fallback_model), MAX_TERMINAL_MODEL_BYTES));
    let parser_error = terminal_kind.is_none().then(|| match disposition {
        CodexWsStepDisposition::ProviderFailure { error_message, .. }
        | CodexWsStepDisposition::StreamTimeout { error_message, .. }
        | CodexWsStepDisposition::Cancelled { error_message, .. } => error_message.clone(),
        CodexWsStepDisposition::Completed => {
            "Codex WebSocket step ended before an official terminal event".to_string()
        }
    });

    aether_contracts::ExecutionStreamTerminalSummary {
        standardized_usage,
        finish_reason: terminal_kind.map(terminal_kind_name).map(str::to_string),
        response_id,
        model,
        observed_finish: terminal_kind.is_some(),
        unknown_event_count: 0,
        parser_error,
    }
}

fn bounded_terminal_string(value: Option<&str>, max_bytes: usize) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty() && value.len() <= max_bytes && value.is_ascii()).then(|| value.to_string())
}

fn terminal_kind_name(kind: TerminalKind) -> &'static str {
    match kind {
        TerminalKind::Completed => "completed",
        TerminalKind::Failed => "failed",
        TerminalKind::Incomplete => "incomplete",
        TerminalKind::Cancelled => "cancelled",
        TerminalKind::Error => "error",
    }
}

fn disposition_status_code(disposition: &CodexWsStepDisposition) -> u16 {
    match disposition {
        CodexWsStepDisposition::Completed => http::StatusCode::OK.as_u16(),
        CodexWsStepDisposition::ProviderFailure { status_code, .. } => *status_code,
        CodexWsStepDisposition::StreamTimeout { .. } => http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
        CodexWsStepDisposition::Cancelled { .. } => 499,
    }
}

fn compact_codex_response_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut compact = BTreeMap::new();
    for (name, value) in headers.iter() {
        if compact.len() >= MAX_RETAINED_RESPONSE_HEADERS {
            break;
        }
        let name = name.as_str().to_ascii_lowercase();
        if name != "retry-after"
            && !name.starts_with("x-codex-")
            && !name.starts_with("x-ratelimit-")
        {
            continue;
        }
        let Ok(value) = value.to_str() else {
            continue;
        };
        if value.len() > MAX_RETAINED_RESPONSE_HEADER_VALUE_BYTES {
            continue;
        }
        compact.insert(name, value.to_string());
    }
    compact
}

fn step_report_context(
    context: Option<serde_json::Value>,
    step: &ResponseCreateStep,
    original_request_body: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut context = match context {
        Some(serde_json::Value::Object(context)) => context,
        _ => serde_json::Map::new(),
    };
    let has_compaction_trigger = |body: &serde_json::Value| {
        body.get("input")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(serde_json::Value::as_str)
                        == Some("compaction_trigger")
                })
            })
    };
    let is_compaction_v2 = has_compaction_trigger(&step.value)
        || original_request_body
            .as_ref()
            .is_some_and(has_compaction_trigger);
    if is_compaction_v2 {
        context.insert("is_compaction".to_string(), serde_json::Value::Bool(true));
        context.insert(
            "compaction_version".to_string(),
            serde_json::Value::String("v2".to_string()),
        );
    } else if context
        .get("compaction_version")
        .and_then(serde_json::Value::as_str)
        == Some("v2")
    {
        // A Codex WebSocket is reused across turns; do not carry a previous
        // turn's compaction marker onto an ordinary response.create step.
        context.remove("is_compaction");
        context.remove("compaction_version");
    }
    context.insert(
        "request_id".to_string(),
        serde_json::Value::String(step_usage_request_id(step)),
    );
    context.insert("ws_step".to_string(), serde_json::Value::Bool(true));
    let has_session_affinity = context
        .get("client_session_affinity")
        .and_then(serde_json::Value::as_object)
        .and_then(|affinity| affinity.get("session_key"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|session_key| !session_key.trim().is_empty());
    if !has_session_affinity {
        if let Some(identity) = step.official_identity.as_ref() {
            let session_id = if !identity.session_id.trim().is_empty() {
                identity.session_id.trim()
            } else {
                identity.thread_id.trim()
            };
            if !session_id.is_empty() {
                context.insert(
                    "client_session_affinity".to_string(),
                    serde_json::json!({
                        "client_family": "codex",
                        "session_key": format!("session={session_id}")
                    }),
                );
            }
        }
    }
    if let Some(original_request_body) = original_request_body {
        context.insert("original_request_body".to_string(), original_request_body);
    }
    Some(serde_json::Value::Object(context))
}

struct MaterializedCodexWsStepBody {
    text: String,
    json: serde_json::Value,
}

#[allow(clippy::too_many_arguments)]
fn materialize_codex_ws_step_body(
    body: serde_json::Value,
    mapped_model: &str,
    force_body_stream_field: bool,
    body_rules: Option<&serde_json::Value>,
    request_headers: &HeaderMap,
    enable_model_directives: bool,
    model_directive_mapping: Option<&serde_json::Value>,
    provider_body_patch: &[RoutingJsonPatchOperation],
    account_profile: Option<&CodexConcreteAccountProfile>,
    adapter: crate::orchestration::ResponsesWebSocketAdapter,
    provider_type: &str,
) -> Result<MaterializedCodexWsStepBody, StepPreparationError> {
    let explicit_session_key =
        crate::client_session_affinity::client_session_affinity_from_request(
            request_headers,
            Some(&body),
        )
        .and_then(|affinity| affinity.session_key);

    // The shared HTTP normalizer strips connection-scoped fields. Preserve
    // the client's explicit WebSocket semantics and restore them only after
    // route/profile edits have completed.
    let explicit_store = body.get("store").cloned();
    let explicit_previous_response_id = body.get("previous_response_id").cloned();
    let explicit_generate = body.get("generate").cloned();
    if adapter == crate::orchestration::ResponsesWebSocketAdapter::Codex
        && body.get("background").is_some_and(|value| !value.is_null())
    {
        return Err(StepPreparationError::retain(
            "codex_ws_background_response_unsupported",
        ));
    }
    let mut body = crate::ai_serving::build_codex_ws_local_openai_responses_request_body(
        body,
        mapped_model,
        true,
        force_body_stream_field,
        provider_type,
        "openai:responses",
        body_rules,
        None,
        request_headers,
        enable_model_directives,
    )
    .ok_or(StepPreparationError::retain(
        "provider_request_body_materialization_failed",
    ))?;
    if let Some(mapping) = model_directive_mapping {
        crate::ai_serving::apply_model_directive_mapping_patch(&mut body, mapping);
    }
    aether_routing_core::apply_json_patch_operations(&mut body, provider_body_patch)
        .map_err(|_| StepPreparationError::retain("provider_request_body_patch_failed"))?;
    if let Some(profile) = account_profile {
        apply_codex_concrete_account_profile_to_body_with_policy(
            &mut body,
            profile,
            CodexProfileRequestBodyPolicy::NormalizeClientMetadata,
        );
    }
    let body_object = body.as_object_mut().ok_or(StepPreparationError::retain(
        "provider_request_body_materialization_failed",
    ))?;
    match adapter {
        crate::orchestration::ResponsesWebSocketAdapter::Codex => {
            body_object.insert("stream".to_string(), serde_json::Value::Bool(true));
            body_object.insert("store".to_string(), serde_json::Value::Bool(false));
        }
        crate::orchestration::ResponsesWebSocketAdapter::Standard => {
            body_object.remove("stream");
            body_object.remove("background");
            if let Some(store) = explicit_store {
                body_object.insert("store".to_string(), store);
            }
        }
    }
    if let Some(previous_response_id) = explicit_previous_response_id {
        body_object.insert("previous_response_id".to_string(), previous_response_id);
    }
    if let Some(generate) = explicit_generate {
        body_object.insert("generate".to_string(), generate);
    }
    let _ = crate::ai_serving::apply_openai_responses_stable_prompt_cache_key(
        &mut body,
        "openai:responses",
        body_rules,
        explicit_session_key.as_deref(),
        None,
    );
    let body_text = serde_json::to_string(&body)
        .map_err(|_| StepPreparationError::retain("account_profile_materialization_failed"))?;
    if body_text.len() > super::protocol::MAX_PUBLIC_CLIENT_PAYLOAD_BYTES {
        return Err(StepPreparationError::retain("materialized_step_too_large"));
    }
    Ok(MaterializedCodexWsStepBody {
        text: body_text,
        json: body,
    })
}

fn normalize_concurrent_limit(limit: Option<i32>) -> Option<usize> {
    limit
        .filter(|limit| *limit > 0)
        .and_then(|limit| usize::try_from(limit).ok())
}

fn upstream_binding_identity(
    adapter: crate::orchestration::ResponsesWebSocketAdapter,
    provider_id: &str,
    endpoint_id: &str,
    key_id: &str,
    plan: &aether_contracts::ExecutionPlan,
    resolved_proxy: Option<&aether_contracts::ProxySnapshot>,
    headers: &BTreeMap<String, String>,
    identity: Option<&OfficialRequestIdentity>,
    account_profile: Option<&CodexConcreteAccountProfile>,
    client_headers: &HeaderMap,
) -> UpstreamBindingIdentity {
    let websocket_url = match adapter {
        crate::orchestration::ResponsesWebSocketAdapter::Codex => {
            OFFICIAL_CODEX_RESPONSES_WS_URL.to_string()
        }
        crate::orchestration::ResponsesWebSocketAdapter::Standard => {
            super::standard_transport::websocket_upstream_url(&plan.url)
                .map(|url| url.to_string())
                .unwrap_or_else(|_| plan.url.trim().to_string())
        }
    };
    let codex_client_headers = if adapter == crate::orchestration::ResponsesWebSocketAdapter::Codex
    {
        [
            "x-codex-beta-features",
            "x-openai-memgen-request",
            "x-responsesapi-include-timing-metrics",
        ]
        .into_iter()
        .filter_map(|name| {
            exact_request_header(client_headers, name)
                .map(|value| (name.to_string(), value.to_string()))
        })
        .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    // Turn metadata is re-materialized in response.create.client_metadata and
    // does not identify the physical socket.
    let official_identity = identity.map(|identity| {
        json!({
            "session_id": identity.session_id,
            "thread_id": identity.thread_id,
            "window_id": identity.window_id,
            "parent_thread_id": identity.parent_thread_id,
            "subagent": identity.subagent,
            "responses_lite": identity.responses_lite,
        })
    });
    let account_profile = account_profile.map(|profile| {
        json!({
            "user_agent": profile.user_agent,
            "originator": profile.originator,
            "installation_id": profile.installation_id,
            "fingerprint_hash": profile.fingerprint_hash,
        })
    });
    UpstreamBindingIdentity {
        adapter,
        provider_id: provider_id.to_string(),
        endpoint_id: endpoint_id.to_string(),
        key_id: key_id.to_string(),
        websocket_url,
        handshake_fingerprint: sha256_serializable(&json!({
            "provider_headers": headers,
            "codex_client_headers": codex_client_headers,
            "official_identity": official_identity,
            "account_profile": account_profile,
        })),
        proxy_fingerprint: sha256_serializable(&resolved_proxy),
        transport_profile_fingerprint: sha256_serializable(&plan.transport_profile),
    }
}

fn sha256_serializable(value: &impl serde::Serialize) -> [u8; 32] {
    let encoded = serde_json::to_vec(value).expect("binding identity inputs are JSON serializable");
    Sha256::digest(encoded).into()
}

async fn acquire_keyed_concurrency_permit(
    runtime: &aether_runtime_state::RuntimeState,
    gate: &'static str,
    resource: &str,
    limit: Option<usize>,
    saturated_reason: &'static str,
    unavailable_reason: &'static str,
) -> Result<Option<RuntimeSemaphorePermit>, StepPreparationError> {
    let Some(limit) = limit else {
        return Ok(None);
    };
    let semaphore = runtime
        .keyed_semaphore(gate, resource, limit, RuntimeSemaphoreConfig::default())
        .map_err(|_| local_capacity_error(unavailable_reason))?;
    match semaphore.try_acquire().await {
        Ok(permit) => Ok(Some(permit)),
        Err(RuntimeSemaphoreError::Saturated { .. }) => {
            Err(StepPreparationError::retain(saturated_reason))
        }
        Err(RuntimeSemaphoreError::Unavailable { .. })
        | Err(RuntimeSemaphoreError::InvalidConfiguration(_)) => {
            Err(local_capacity_error(unavailable_reason))
        }
    }
}

async fn acquire_candidate_concurrency_permits(
    runtime: &aether_runtime_state::RuntimeState,
    provider_id: &str,
    provider_limit: Option<usize>,
    key_id: &str,
    key_limit: Option<usize>,
) -> Result<
    (
        Option<RuntimeSemaphorePermit>,
        Option<RuntimeSemaphorePermit>,
    ),
    StepPreparationError,
> {
    let provider = acquire_keyed_concurrency_permit(
        runtime,
        CODEX_WS_PROVIDER_CONCURRENCY_GATE,
        provider_id,
        provider_limit,
        "provider_concurrency_limit_reached",
        "step_provider_concurrency_unavailable",
    );
    let key = acquire_keyed_concurrency_permit(
        runtime,
        CODEX_WS_KEY_CONCURRENCY_GATE,
        key_id,
        key_limit,
        "provider_key_concurrency_limit_reached",
        "step_provider_key_concurrency_unavailable",
    );
    let (provider, key) = tokio::join!(provider, key);
    Ok((provider?, key?))
}

fn outbound_route(proxy: Option<&aether_contracts::ProxySnapshot>) -> Option<OutboundRoute> {
    let Some(proxy) = proxy else {
        return Some(OutboundRoute::TransportDefault);
    };
    if proxy.enabled == Some(false) || proxy.mode.as_deref() == Some("direct") {
        return Some(OutboundRoute::Direct);
    }
    if proxy.mode.as_deref() == Some("tunnel") {
        return None;
    }
    let Some(url) = proxy
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        if proxy.node_id.is_some() {
            return None;
        }
        return Some(OutboundRoute::TransportDefault);
    };
    let scheme = url::Url::parse(url).ok()?.scheme().to_ascii_lowercase();
    matches!(scheme.as_str(), "http" | "https" | "socks5" | "socks5h")
        .then(|| OutboundRoute::proxy(url))
}

fn compact_proxy_for_plan(
    proxy: &aether_contracts::ProxySnapshot,
) -> aether_contracts::ProxySnapshot {
    aether_contracts::ProxySnapshot {
        enabled: proxy.enabled,
        mode: proxy.mode.clone(),
        node_id: proxy.node_id.clone(),
        label: None,
        url: None,
        extra: None,
    }
}

fn case_insensitive_btree_value<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers.iter().find_map(|(candidate, value)| {
        candidate
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
            .filter(|value| !value.is_empty())
    })
}

fn exact_request_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?.trim();
    (values.next().is_none() && !value.is_empty()).then_some(value)
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<(), PeerError> {
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| PeerError("official Codex header name is invalid".into()))?;
    let value = HeaderValue::from_str(value)
        .map_err(|_| PeerError("official Codex header value is invalid".into()))?;
    value
        .to_str()
        .map_err(|_| PeerError("official Codex header value is not visible ASCII".into()))?;
    headers.insert(name, value);
    Ok(())
}

fn gateway_error(error: GatewayError) -> PeerError {
    let _ = error;
    PeerError("Codex WS candidate planning failed".into())
}

fn runtime_fence_error(error: StepPreparationError) -> PeerError {
    PeerError(error.reason.to_string())
}

struct OfficialPeer {
    connection: WebSocketConnection,
}

fn official_close_peer_error(close: Option<(String, String)>) -> PeerError {
    match close {
        Some((code, reason)) => PeerError(format!(
            "official Codex WS closed: code={code}, reason={reason:?}"
        )),
        None => PeerError("official Codex WS closed without close details".into()),
    }
}

impl Stream for OfficialPeer {
    type Item = Result<RelayFrame, PeerError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            let message = match std::pin::Pin::new(&mut self.connection).poll_next(context) {
                std::task::Poll::Ready(Some(Ok(message))) => message,
                std::task::Poll::Ready(Some(Err(error))) => {
                    return std::task::Poll::Ready(Some(Err(PeerError(format!(
                        "official Codex WS receive failed: {error}"
                    )))));
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            let frame = match message {
                OfficialMessage::Text(text) => RelayFrame::Text(text.into()),
                OfficialMessage::Binary(bytes) => RelayFrame::Binary(bytes),
                OfficialMessage::Ping(bytes) => RelayFrame::Ping(bytes),
                OfficialMessage::Pong(bytes) => RelayFrame::Pong(bytes),
                OfficialMessage::Close(close) => {
                    let close =
                        close.map(|frame| (frame.code.to_string(), frame.reason.to_string()));
                    return std::task::Poll::Ready(Some(Err(official_close_peer_error(close))));
                }
                OfficialMessage::Frame(_) => continue,
            };
            return std::task::Poll::Ready(Some(Ok(frame)));
        }
    }
}

impl Sink<RelayFrame> for OfficialPeer {
    type Error = PeerError;

    fn poll_ready(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.connection)
            .poll_ready(context)
            .map_err(|error| PeerError(format!("official Codex WS send readiness failed: {error}")))
    }

    fn start_send(
        mut self: std::pin::Pin<&mut Self>,
        frame: RelayFrame,
    ) -> Result<(), Self::Error> {
        let frame =
            match frame {
                RelayFrame::Text(text) => OfficialMessage::Text(text.try_into().map_err(|_| {
                    PeerError("Codex WS relay text frame was not valid UTF-8".into())
                })?),
                RelayFrame::Binary(bytes) => OfficialMessage::Binary(bytes),
                RelayFrame::Ping(bytes) => OfficialMessage::Ping(bytes),
                RelayFrame::Pong(bytes) => OfficialMessage::Pong(bytes),
                RelayFrame::Close => OfficialMessage::Close(None),
            };
        std::pin::Pin::new(&mut self.connection)
            .start_send(frame)
            .map_err(|error| PeerError(format!("official Codex WS send failed: {error}")))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.connection)
            .poll_flush(context)
            .map_err(|error| PeerError(format!("official Codex WS flush failed: {error}")))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.connection)
            .poll_close(context)
            .map_err(|error| PeerError(format!("official Codex WS close failed: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use aether_data::repository::auth::{
        AuthApiKeyWriteRepository, InMemoryAuthApiKeySnapshotRepository, StoredAuthApiKeySnapshot,
    };

    use super::*;
    use crate::codex_ws::protocol::StepFence;
    use crate::data::GatewayDataState;

    #[test]
    fn runtime_requires_upgrade_time_authorization_before_credentials_are_discarded() {
        let decision = GatewayControlDecision::synthetic(
            "/v1/responses",
            Some("ai_public".to_string()),
            Some("openai".to_string()),
            Some("responses_websocket".to_string()),
            Some("openai:responses".to_string()),
        );
        let result = GatewayCodexWsRuntime::new(
            AppState::new().expect("test state should build"),
            HeaderMap::new(),
            Uri::from_static("/v1/responses"),
            decision,
            "trace-missing-auth".to_string(),
            super::super::hot_state::CodexWsHotLease {
                generation: "generation-1".to_string(),
                eligible: false,
            },
            std::net::IpAddr::from([127, 0, 0, 1]),
            0,
        );

        let Err(error) = result else {
            panic!("missing auth context must fail");
        };
        assert_eq!(
            error.0,
            "Responses WebSocket runtime auth context is missing"
        );
    }

    #[test]
    fn runtime_request_headers_drop_downstream_credentials_and_websocket_hops() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        headers.insert("x-api-key", "secret-key".parse().unwrap());
        headers.insert(http::header::COOKIE, "session=secret".parse().unwrap());
        headers.insert(
            http::header::CONNECTION,
            "upgrade, x-private-hop".parse().unwrap(),
        );
        headers.insert("x-private-hop", "private".parse().unwrap());
        headers.insert("sec-websocket-key", "socket-secret".parse().unwrap());
        headers.insert(
            super::super::protocol::ROUTE_CONTROL_ACCEPT_HEADER,
            super::super::protocol::ROUTE_CONTROL_VERSION
                .parse()
                .unwrap(),
        );
        headers.insert("x-client-feature", "retained".parse().unwrap());

        let sanitized = sanitize_runtime_request_headers(headers);

        for name in [
            "authorization",
            "x-api-key",
            "cookie",
            "connection",
            "x-private-hop",
            "sec-websocket-key",
            super::super::protocol::ROUTE_CONTROL_ACCEPT_HEADER,
        ] {
            assert!(!sanitized.contains_key(name), "header survived: {name}");
        }
        assert_eq!(
            sanitized
                .get("x-client-feature")
                .and_then(|value| value.to_str().ok()),
            Some("retained")
        );
    }

    #[tokio::test]
    async fn turn_decision_refresh_reloads_a_revoked_api_key_and_authorization_epoch() {
        let api_key = "sk-ws-turn-refresh";
        let key_hash = format!("{:x}", Sha256::digest(api_key.as_bytes()));
        let auth_repository = Arc::new(InMemoryAuthApiKeySnapshotRepository::seed([(
            Some(key_hash),
            StoredAuthApiKeySnapshot::new(
                "user-1".to_string(),
                "alice".to_string(),
                None,
                "user".to_string(),
                "local".to_string(),
                true,
                false,
                Some(json!(["openai"])),
                Some(json!(["openai:responses"])),
                Some(json!(["gpt-test"])),
                "key-1".to_string(),
                Some("WS key".to_string()),
                true,
                false,
                true,
                None,
                None,
                Some(4_102_444_800),
                Some(json!(["openai"])),
                Some(json!(["openai:responses"])),
                Some(json!(["gpt-test"])),
            )
            .expect("auth snapshot should build"),
        )]));
        let data =
            GatewayDataState::with_auth_api_key_repository_for_tests(Arc::clone(&auth_repository));
        let state = AppState::new()
            .expect("test state should build")
            .with_data_state_for_tests(data);
        let initial_auth_epoch = state.auth_context_invalidation_epoch();
        let request_uri = Uri::from_static(
            "/v1/responses?key=query-credential-must-not-survive&client_feature=retained",
        );
        let mut decision = GatewayControlDecision::synthetic(
            "/v1/responses",
            Some("ai_public".to_string()),
            Some("openai".to_string()),
            Some("responses_websocket".to_string()),
            Some("openai:responses".to_string()),
        );
        decision.public_query_string = request_uri.query().map(ToOwned::to_owned);
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", api_key.parse().expect("API key header"));
        let auth_context = crate::control::resolve_execution_runtime_auth_context(
            &state,
            &decision,
            &headers,
            &request_uri,
            "trace-refresh-test",
        )
        .await
        .expect("initial auth resolution should succeed")
        .expect("initial auth context should exist");
        decision.local_auth_rejection = auth_context.local_rejection.clone();
        decision.auth_context = Some(auth_context);
        let runtime = GatewayCodexWsRuntime::new(
            state.clone(),
            headers,
            request_uri,
            decision,
            "trace-refresh-test".to_string(),
            super::super::hot_state::CodexWsHotLease {
                generation: "generation-1".to_string(),
                eligible: false,
            },
            std::net::IpAddr::from([127, 0, 0, 1]),
            initial_auth_epoch,
        )
        .expect("Codex WS runtime should build");
        assert!(!runtime.request_headers.contains_key("x-api-key"));
        assert_eq!(runtime.request_uri.query(), Some("client_feature=retained"));
        assert_eq!(
            runtime
                .decision_snapshot()
                .await
                .public_query_string
                .as_deref(),
            Some("client_feature=retained")
        );

        let initial = runtime
            .refresh_turn_decision()
            .await
            .expect("initial turn decision should refresh");
        assert!(runtime.validate_runtime_fences().is_ok());
        assert_eq!(
            initial
                .auth_context
                .as_ref()
                .and_then(|context| context.allowed_models.as_deref())
                .and_then(|models| models.first())
                .map(String::as_str),
            Some("gpt-test")
        );
        assert!(initial
            .auth_context
            .as_ref()
            .is_some_and(|context| context.access_allowed));

        auth_repository
            .set_standalone_api_key_active("key-1", false)
            .await
            .expect("API key update should succeed")
            .expect("API key should exist");
        state.invalidate_auth_context_cache();
        assert!(runtime.validate_runtime_fences().is_err());

        let revoked = runtime
            .refresh_turn_decision()
            .await
            .expect("revoked turn decision should refresh");
        assert!(runtime.validate_runtime_fences().is_ok());
        assert!(revoked
            .auth_context
            .as_ref()
            .is_some_and(|context| !context.access_allowed));
        assert_eq!(
            revoked
                .auth_context
                .as_ref()
                .and_then(|context| context.local_rejection.as_ref()),
            Some(&crate::control::GatewayLocalAuthRejection::InvalidApiKey)
        );
        assert_eq!(
            runtime.decision_snapshot().await.local_auth_rejection,
            Some(crate::control::GatewayLocalAuthRejection::InvalidApiKey)
        );

        auth_repository
            .set_standalone_api_key_active("key-1", true)
            .await
            .expect("API key restore should succeed")
            .expect("API key should exist");
        state.invalidate_auth_context_cache();
        let restored = runtime
            .refresh_turn_decision()
            .await
            .expect("restored turn decision should refresh");
        assert!(restored
            .auth_context
            .as_ref()
            .is_some_and(|context| context.access_allowed));
        assert!(restored
            .auth_context
            .as_ref()
            .is_some_and(|context| context.local_rejection.is_none()));
    }

    #[test]
    fn official_close_error_preserves_code_and_reason() {
        let error = official_close_peer_error(Some((
            "1012".to_string(),
            "service restart\nretry later".to_string(),
        )));

        assert_eq!(
            error.0,
            "official Codex WS closed: code=1012, reason=\"service restart\\nretry later\""
        );
        assert_eq!(
            official_close_peer_error(None).0,
            "official Codex WS closed without close details"
        );
    }

    #[test]
    fn step_report_context_creates_ws_metadata_without_base_context() {
        let step = ResponseCreateStep {
            value: json!({
                "input": [{"type": "compaction_trigger"}]
            }),
            encoded_len: 2,
            model: "gpt-test".into(),
            previous_response_id: None,
            logical_turn_id: None,
            official_identity: Some(OfficialRequestIdentity {
                session_id: "session-1".into(),
                thread_id: "thread-1".into(),
                window_id: None,
                turn_metadata: None,
                parent_thread_id: None,
                subagent: None,
                responses_lite: false,
            }),
            fence: StepFence {
                correlation_id: "correlation-1".into(),
                binding_epoch_id: "epoch-1".into(),
                binding_generation: 1,
            },
        };

        let original_request_body = json!({
            "type": "response.create",
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "ok"
            }]
        });
        let context = step_report_context(None, &step, Some(original_request_body.clone()))
            .expect("context should be created");

        assert_eq!(context["request_id"], step_usage_request_id(&step));
        assert_eq!(context["ws_step"], true);
        assert_eq!(context["is_compaction"], true);
        assert_eq!(context["compaction_version"], "v2");
        assert_eq!(context["client_session_affinity"]["client_family"], "codex");
        assert_eq!(
            context["client_session_affinity"]["session_key"],
            "session=session-1"
        );
        assert_eq!(context["original_request_body"], original_request_body);
    }

    #[test]
    fn step_report_context_preserves_existing_session_affinity() {
        let step = ResponseCreateStep {
            value: json!({}),
            encoded_len: 2,
            model: "gpt-test".into(),
            previous_response_id: None,
            logical_turn_id: None,
            official_identity: Some(OfficialRequestIdentity {
                session_id: "official-session".into(),
                thread_id: "thread-1".into(),
                window_id: None,
                turn_metadata: None,
                parent_thread_id: None,
                subagent: None,
                responses_lite: false,
            }),
            fence: StepFence {
                correlation_id: "correlation-1".into(),
                binding_epoch_id: "epoch-1".into(),
                binding_generation: 1,
            },
        };
        let context = step_report_context(
            Some(json!({
                "client_session_affinity": {
                    "client_family": "codex",
                    "session_key": "account=account-1;session=planner-session"
                }
            })),
            &step,
            None,
        )
        .expect("context should remain");

        assert_eq!(
            context["client_session_affinity"]["session_key"],
            "account=account-1;session=planner-session"
        );
    }

    #[test]
    fn step_report_context_detects_compaction_from_saved_body_after_step_body_is_taken() {
        let step = ResponseCreateStep {
            value: json!({}),
            encoded_len: 2,
            model: "gpt-test".into(),
            previous_response_id: None,
            logical_turn_id: None,
            official_identity: Some(OfficialRequestIdentity {
                session_id: "session-1".into(),
                thread_id: "thread-1".into(),
                window_id: None,
                turn_metadata: None,
                parent_thread_id: None,
                subagent: None,
                responses_lite: false,
            }),
            fence: StepFence {
                correlation_id: "correlation-1".into(),
                binding_epoch_id: "epoch-1".into(),
                binding_generation: 1,
            },
        };
        let context = step_report_context(
            None,
            &step,
            Some(json!({
                "type": "response.create",
                "input": [{"type": "compaction_trigger"}]
            })),
        )
        .expect("context should be created");

        assert_eq!(context["is_compaction"], true);
        assert_eq!(context["compaction_version"], "v2");
    }

    #[test]
    fn materialized_initial_step_uses_a_content_cache_cohort_without_session_identity() {
        let body = json!({
            "type": "response.create",
            "model": "gpt-5.6-terra",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "inspect the workspace"}]
            }]
        });

        let materialized = materialize_codex_ws_step_body(
            body,
            "gpt-5.6-terra",
            false,
            None,
            &HeaderMap::new(),
            false,
            None,
            &[],
            None,
            crate::orchestration::ResponsesWebSocketAdapter::Codex,
            "codex",
        )
        .expect("initial body should materialize");

        assert!(materialized.json["prompt_cache_key"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }

    #[test]
    fn materialized_follow_up_preserves_previous_response_for_custom_tool_output() {
        let body = json!({
            "type": "response.create",
            "model": "gpt-5.6-terra",
            "previous_response_id": "resp-1",
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "tool result"
            }]
        });

        let materialized = materialize_codex_ws_step_body(
            body,
            "gpt-5.6-terra",
            false,
            None,
            &HeaderMap::new(),
            false,
            None,
            &[],
            None,
            crate::orchestration::ResponsesWebSocketAdapter::Codex,
            "codex",
        )
        .expect("follow-up body should materialize");

        assert_eq!(materialized.json["previous_response_id"], "resp-1");
        assert_eq!(
            materialized.json["input"][0]["type"],
            "custom_tool_call_output"
        );
        assert_eq!(materialized.json["input"][0]["call_id"], "call-1");
        assert!(materialized.json.get("prompt_cache_key").is_none());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&materialized.text)
                .expect("materialized text should be JSON"),
            materialized.json
        );
    }

    #[test]
    fn standard_materialization_preserves_ws_fields_and_removes_http_fields() {
        let materialized = materialize_codex_ws_step_body(
            json!({
                "type": "response.create",
                "model": "gpt-test",
                "stream": true,
                "background": true,
                "store": true,
                "previous_response_id": "resp-1",
                "generate": false,
                "input": []
            }),
            "gpt-test",
            false,
            None,
            &HeaderMap::new(),
            false,
            None,
            &[],
            None,
            crate::orchestration::ResponsesWebSocketAdapter::Standard,
            "openai",
        )
        .expect("standard body should materialize");

        assert!(materialized.json.get("stream").is_none());
        assert!(materialized.json.get("background").is_none());
        assert_eq!(materialized.json["store"], true);
        assert_eq!(materialized.json["previous_response_id"], "resp-1");
        assert_eq!(materialized.json["generate"], false);
    }

    #[test]
    fn local_capacity_failures_retain_the_middle_route() {
        for reason in [
            "step_admission_unavailable",
            "large_frame_cpu_unavailable",
            "step_provider_concurrency_unavailable",
            "step_provider_key_concurrency_unavailable",
        ] {
            assert_eq!(
                local_capacity_error(reason),
                StepPreparationError {
                    reason,
                    middle_route_disposition: MiddleRouteDisposition::Retain,
                }
            );
        }
    }

    fn usage_plan_with_body() -> aether_contracts::ExecutionPlan {
        aether_contracts::ExecutionPlan {
            request_id: "request-1".into(),
            candidate_id: Some("candidate-1".into()),
            provider_name: Some("Codex".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "POST".into(),
            url: OFFICIAL_CODEX_RESPONSES_WS_URL.into(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody {
                json_body: Some(json!({"model": "gpt-test"})),
                body_bytes_b64: Some("e30=".into()),
                body_ref: Some("body-1".into()),
            },
            stream: true,
            client_api_format: "openai:responses".into(),
            provider_api_format: "openai:responses".into(),
            model_name: Some("gpt-test".into()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        }
    }

    #[test]
    fn compact_execution_plan_template_drops_sensitive_payload_without_mutating_source() {
        let source = usage_plan_with_body();
        let compact = compact_execution_plan_template(&source);

        assert!(compact.body.json_body.is_none());
        assert!(compact.body.body_bytes_b64.is_none());
        assert!(compact.body.body_ref.is_none());
        assert!(source.body.json_body.is_some());
        assert!(source.body.body_bytes_b64.is_some());
        assert!(source.body.body_ref.is_some());
        assert!(compact.headers.is_empty());
        assert_eq!(compact.request_id, source.request_id);
        assert_eq!(compact.candidate_id, source.candidate_id);
    }

    #[test]
    fn codex_binding_fingerprint_ignores_turn_metadata_but_tracks_connection_identity() {
        let plan = usage_plan_with_body();
        let identity = OfficialRequestIdentity {
            session_id: "session-1".into(),
            thread_id: "thread-1".into(),
            window_id: Some("window-1".into()),
            turn_metadata: Some(r#"{"turn":"one"}"#.into()),
            parent_thread_id: Some("parent-1".into()),
            subagent: Some("review".into()),
            responses_lite: false,
        };
        let original = upstream_binding_identity(
            crate::orchestration::ResponsesWebSocketAdapter::Codex,
            "provider-1",
            "endpoint-1",
            "key-1",
            &plan,
            None,
            &BTreeMap::new(),
            Some(&identity),
            None,
            &HeaderMap::new(),
        );
        let mut changed = identity.clone();
        changed.turn_metadata = Some(r#"{"turn":"two"}"#.into());
        let changed = upstream_binding_identity(
            crate::orchestration::ResponsesWebSocketAdapter::Codex,
            "provider-1",
            "endpoint-1",
            "key-1",
            &plan,
            None,
            &BTreeMap::new(),
            Some(&changed),
            None,
            &HeaderMap::new(),
        );

        assert_eq!(original, changed);

        let mut changed = identity;
        changed.window_id = Some("window-2".into());
        let changed = upstream_binding_identity(
            crate::orchestration::ResponsesWebSocketAdapter::Codex,
            "provider-1",
            "endpoint-1",
            "key-1",
            &plan,
            None,
            &BTreeMap::new(),
            Some(&changed),
            None,
            &HeaderMap::new(),
        );

        assert_ne!(original, changed);
    }

    #[test]
    fn official_header_builder_never_copies_aether_control_headers() {
        let mut candidate_headers = BTreeMap::new();
        candidate_headers.insert("authorization".into(), "Bearer selected".into());
        candidate_headers.insert("x-aether-ws-control".into(), "route-v1".into());
        assert_eq!(
            case_insensitive_btree_value(&candidate_headers, "authorization"),
            Some("Bearer selected")
        );
        assert!(case_insensitive_btree_value(&candidate_headers, "x-aether-nope").is_none());
    }

    #[test]
    fn official_header_builder_rejects_non_ascii_before_connector() {
        let mut headers = HeaderMap::new();
        let error = insert_header(
            &mut headers,
            "x-codex-turn-metadata",
            "{\"cwd\":\"/workspace/\u{9879}\u{76ee}\"}",
        )
        .expect_err("non-ASCII header should fail materialization");

        assert_eq!(error.0, "official Codex header value is not visible ASCII");
        assert!(!headers.contains_key("x-codex-turn-metadata"));
    }

    #[test]
    fn tunnel_proxy_is_not_eligible_for_the_native_connector() {
        let proxy = aether_contracts::ProxySnapshot {
            mode: Some("tunnel".into()),
            node_id: Some("node-1".into()),
            ..Default::default()
        };
        assert!(outbound_route(Some(&proxy)).is_none());
    }

    #[test]
    fn explicit_proxy_preflight_accepts_only_reviewed_connect_schemes() {
        for url in [
            "http://proxy.invalid:8080",
            "https://proxy.invalid:8443",
            "socks5://proxy.invalid:1080",
            "socks5h://proxy.invalid:1080",
        ] {
            let proxy = aether_contracts::ProxySnapshot {
                url: Some(url.into()),
                ..Default::default()
            };
            assert!(matches!(
                outbound_route(Some(&proxy)),
                Some(OutboundRoute::Proxy { .. })
            ));
        }
        let manual_node = aether_contracts::ProxySnapshot {
            mode: Some("http".into()),
            node_id: Some("manual-node-1".into()),
            url: Some("http://proxy.invalid:8080".into()),
            ..Default::default()
        };
        assert!(matches!(
            outbound_route(Some(&manual_node)),
            Some(OutboundRoute::Proxy { .. })
        ));
        for url in ["ftp://proxy.invalid:21", "not a URL"] {
            let proxy = aether_contracts::ProxySnapshot {
                url: Some(url.into()),
                ..Default::default()
            };
            assert!(outbound_route(Some(&proxy)).is_none());
        }
    }

    #[test]
    fn handshake_http_classification_keeps_body_out_of_diagnostics() {
        let secret_body = br#"{"error":{"message":"provider-secret"}}"#.to_vec();
        let response = http::Response::builder()
            .status(http::StatusCode::UNAUTHORIZED)
            .body(Some(secret_body))
            .expect("HTTP response should build");
        let failure = classify_codex_ws_handshake_failure(
            WebSocketError::Http(Box::new(response)),
            CodexWsRouteKind::Proxy,
        );

        assert_eq!(failure.error_type, "codex_ws_handshake_unauthorized");
        assert!(failure
            .error_body
            .as_deref()
            .is_some_and(|body| body.contains("provider-secret")));
        assert_eq!(
            failure.diagnostic_detail.as_deref(),
            Some("http_status=401")
        );
        assert!(!failure
            .diagnostic_detail
            .as_deref()
            .unwrap()
            .contains("provider-secret"));
    }

    #[test]
    fn standard_handshake_http_classification_preserves_provider_rejection() {
        let failure = classify_standard_ws_handshake_failure(
            crate::codex_ws::standard_transport::StandardWebSocketConnectError::Rejected {
                status_code: http::StatusCode::TOO_MANY_REQUESTS.as_u16(),
                response_headers: BTreeMap::from([("retry-after".into(), "7".into())]),
                error_body: Some(r#"{"error":"limited"}"#.into()),
            },
        );

        assert_eq!(failure.status_code, 429);
        assert_eq!(
            failure.error_type,
            "responses_websocket_handshake_rate_limited"
        );
        assert_eq!(
            failure.route_reason,
            "responses_websocket_account_rate_limited"
        );
        assert_eq!(
            failure
                .response_headers
                .get("retry-after")
                .map(String::as_str),
            Some("7")
        );
        assert_eq!(
            failure.error_body.as_deref(),
            Some(r#"{"error":"limited"}"#)
        );
        assert_eq!(
            failure.diagnostic_detail.as_deref(),
            Some("http_status=429")
        );
    }

    #[test]
    fn handshake_io_classification_keeps_only_route_and_io_kind() {
        let failure = classify_codex_ws_handshake_failure(
            WebSocketError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "http://user:password@proxy.invalid:8080",
            )),
            CodexWsRouteKind::Proxy,
        );

        assert_eq!(failure.error_type, "codex_ws_handshake_proxy_io_error");
        assert_eq!(
            failure.diagnostic_detail.as_deref(),
            Some("io_kind=connection_reset")
        );
        assert_eq!(failure.error_body, failure.diagnostic_detail);
        assert!(!failure.error_body.unwrap().contains("password"));
    }

    #[test]
    fn handshake_proxy_connect_classification_redacts_proxy_error() {
        let failure = classify_codex_ws_handshake_failure(
            WebSocketError::Url(WebSocketUrlError::ProxyConnect(
                "http://user:password@proxy.invalid:8080".into(),
            )),
            CodexWsRouteKind::Proxy,
        );

        assert_eq!(failure.error_type, "codex_ws_handshake_proxy_connect_error");
        assert_eq!(
            failure.diagnostic_detail.as_deref(),
            Some("url_kind=proxy_connect")
        );
        assert_eq!(failure.error_body, failure.diagnostic_detail);
        assert!(!failure.error_body.unwrap().contains("password"));
    }

    #[test]
    fn handshake_proxy_connect_classification_retains_safe_http_status() {
        let failure = classify_codex_ws_handshake_failure(
            WebSocketError::Url(WebSocketUrlError::ProxyConnect(
                "HTTP CONNECT failed with status 407".into(),
            )),
            CodexWsRouteKind::Proxy,
        );

        assert_eq!(
            failure.diagnostic_detail.as_deref(),
            Some("proxy_connect_http_status=407")
        );
        assert_eq!(failure.error_body, failure.diagnostic_detail);
    }

    #[test]
    fn handshake_tls_and_protocol_classification_exposes_only_variants() {
        let tls = classify_codex_ws_handshake_failure(
            WebSocketError::Tls(WebSocketTlsError::InvalidDnsName),
            CodexWsRouteKind::Direct,
        );
        let protocol = classify_codex_ws_handshake_failure(
            WebSocketError::Protocol(WebSocketProtocolError::MissingUpgradeWebSocketHeader),
            CodexWsRouteKind::Direct,
        );

        assert_eq!(tls.error_type, "codex_ws_handshake_tls_error");
        assert_eq!(
            tls.diagnostic_detail.as_deref(),
            Some("tls_kind=invalid_dns_name")
        );
        assert_eq!(tls.error_body, tls.diagnostic_detail);
        assert_eq!(protocol.error_type, "codex_ws_handshake_protocol_error");
        assert_eq!(
            protocol.diagnostic_detail.as_deref(),
            Some("protocol_kind=missing_upgrade_websocket_header")
        );
        assert_eq!(protocol.error_body, protocol.diagnostic_detail);
    }

    #[test]
    fn handshake_utf8_classification_keeps_only_the_header_name() {
        for (detail, expected_header) in [
            (
                format!("{HANDSHAKE_HEADER_NOT_VISIBLE_ASCII_PREFIX}x-codex-turn-metadata"),
                "utf8_header=x-codex-turn-metadata",
            ),
            (
                "failed to convert header for header name 'authorization' with value: Bearer secret"
                    .to_string(),
                "utf8_header=authorization",
            ),
        ] {
            let failure = classify_codex_ws_handshake_failure(
                WebSocketError::Utf8(detail),
                CodexWsRouteKind::Direct,
            );

            assert_eq!(failure.error_type, "codex_ws_handshake_utf8_error");
            assert_eq!(failure.diagnostic_detail.as_deref(), Some(expected_header));
            assert_eq!(failure.error_body, failure.diagnostic_detail);
            assert!(!failure.error_body.as_deref().unwrap().contains("secret"));
            assert!(!failure.error_body.as_deref().unwrap().contains("Bearer"));
        }

        let failure = classify_codex_ws_handshake_failure(
            WebSocketError::Utf8("Bearer secret with no parseable header".to_string()),
            CodexWsRouteKind::Direct,
        );
        assert_eq!(failure.diagnostic_detail.as_deref(), Some("utf8_error"));
        assert_eq!(failure.error_body.as_deref(), Some("utf8_error"));
        assert!(!failure.error_body.as_deref().unwrap().contains("secret"));
        assert!(!failure.error_body.as_deref().unwrap().contains("Bearer"));
    }

    #[test]
    fn moving_proxy_route_to_connector_leaves_no_idle_credentials() {
        let mut idle_route = OutboundRoute::proxy("http://user:password@proxy.invalid:8080");
        let connecting_route = take_outbound_route_for_connect(&mut idle_route);
        assert_eq!(idle_route, OutboundRoute::Direct);
        assert!(!format!("{idle_route:?}").contains("password"));
        assert!(matches!(connecting_route, OutboundRoute::Proxy { .. }));
    }

    #[test]
    fn frozen_proxy_plan_retains_topology_but_never_credentials() {
        let proxy = aether_contracts::ProxySnapshot {
            enabled: Some(true),
            mode: Some("http".into()),
            node_id: Some("node-1".into()),
            label: Some("secret label".into()),
            url: Some("http://user:password@proxy.invalid:8080".into()),
            extra: Some(json!({"password": "also-secret"})),
        };

        let compact = compact_proxy_for_plan(&proxy);

        assert_eq!(compact.enabled, Some(true));
        assert_eq!(compact.mode.as_deref(), Some("http"));
        assert_eq!(compact.node_id.as_deref(), Some("node-1"));
        assert!(compact.label.is_none());
        assert!(compact.url.is_none());
        assert!(compact.extra.is_none());
    }

    #[tokio::test]
    async fn candidate_concurrency_competes_by_resource_and_releases_partial_acquisition() {
        let runtime = aether_runtime_state::RuntimeState::memory(
            aether_runtime_state::MemoryRuntimeStateConfig::default(),
        );

        let (provider_permit, _) = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-1",
            Some(1),
            "key-unlimited",
            None,
        )
        .await
        .expect("first provider permit should be available");
        let provider_error = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-1",
            Some(1),
            "key-other",
            None,
        )
        .await
        .expect_err("matching provider should compete");
        assert_eq!(provider_error.reason, "provider_concurrency_limit_reached");
        let other_provider = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-2",
            Some(1),
            "key-other",
            None,
        )
        .await
        .expect("different provider should remain available");
        drop(provider_permit);
        drop(other_provider);

        let key_blocker = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-unlimited",
            None,
            "key-1",
            Some(1),
        )
        .await
        .expect("first key permit should be available");
        let partial_error = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-partial",
            Some(1),
            "key-1",
            Some(1),
        )
        .await
        .expect_err("blocked key should reject the combined acquisition");
        assert_eq!(
            partial_error.reason,
            "provider_key_concurrency_limit_reached"
        );

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let released_provider = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-partial",
            Some(1),
            "key-unlimited",
            None,
        )
        .await
        .expect("partial failure must release the provider permit");
        drop(released_provider);
        drop(key_blocker);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let released_key = acquire_candidate_concurrency_permits(
            &runtime,
            "provider-unlimited",
            None,
            "key-1",
            Some(1),
        )
        .await
        .expect("dropping the step guard must release the key permit");
        drop(released_key);
    }
}
