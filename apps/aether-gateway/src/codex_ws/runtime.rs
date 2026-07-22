use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aether_codex_ws_connector::{
    CodexWebSocketConnector, IntoClientRequest, Message as OfficialMessage, OutboundRoute,
    WebSocketConnection, WebSocketError, WebSocketProtocolError, WebSocketTlsError,
    WebSocketUrlError,
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
                configured.and_then(|timeouts| timeouts.read_ms),
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
    pub(crate) client_api_key_id: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) response_headers: BTreeMap<String, String>,
    pub(crate) account_profile: Option<Arc<CodexConcreteAccountProfile>>,
    pub(crate) report_kind: String,
    pub(crate) identity: OfficialRequestIdentity,
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
    pub(crate) shared_global_generation: String,
    pub(crate) shared_catalog_generation: String,
    pub(crate) shared_key_generation: String,
    pub(crate) prewrite_cleanup_permit:
        Option<tokio::sync::mpsc::OwnedPermit<CodexWsSettlementCommit>>,
}

struct CodexWsCandidatePreflight {
    transport: Arc<crate::ai_serving::GatewayProviderTransportSnapshot>,
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
    ) -> Result<(), StepPreparationError> {
        self.validate_candidate_fences(candidate)
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
    decision: GatewayControlDecision,
    trace_id: String,
    shared_global: super::hot_state::CodexWsHotLease,
    remote_ip: std::net::IpAddr,
    auth_context_epoch: u64,
    cold_policy_validated: AtomicBool,
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

impl GatewayCodexWsRuntime {
    pub(crate) fn new(
        state: AppState,
        request_headers: HeaderMap,
        request_uri: Uri,
        decision: GatewayControlDecision,
        trace_id: String,
        shared_global: super::hot_state::CodexWsHotLease,
        remote_ip: std::net::IpAddr,
        auth_context_epoch: u64,
    ) -> Result<Self, PeerError> {
        let connector = CodexWebSocketConnector::new()
            .map_err(|_| PeerError("failed to initialize pinned Codex WS connector".into()))?;
        let usage_report_tx = state.codex_ws_usage_reporter.sender();
        let settlement_tx = state.codex_ws_usage_reporter.settlement_sender();
        Ok(Self {
            state,
            request_headers,
            request_uri,
            decision,
            trace_id,
            shared_global,
            remote_ip,
            auth_context_epoch,
            cold_policy_validated: AtomicBool::new(false),
            usage_report_tx,
            settlement_tx,
            connector,
        })
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
        insert_header(
            request.headers_mut(),
            "session-id",
            &candidate.identity.session_id,
        )?;
        insert_header(
            request.headers_mut(),
            "thread-id",
            &candidate.identity.thread_id,
        )?;
        insert_header(
            request.headers_mut(),
            "x-client-request-id",
            &candidate.identity.thread_id,
        )?;
        if let Some(window_id) = candidate.identity.window_id.as_deref() {
            insert_header(request.headers_mut(), "x-codex-window-id", window_id)?;
        }
        if let Some(parent_thread_id) = candidate.identity.parent_thread_id.as_deref() {
            insert_header(
                request.headers_mut(),
                "x-codex-parent-thread-id",
                parent_thread_id,
            )?;
        }
        if let Some(subagent) = candidate.identity.subagent.as_deref() {
            insert_header(request.headers_mut(), "x-openai-subagent", subagent)?;
        }
        if candidate.identity.responses_lite {
            insert_header(
                request.headers_mut(),
                "x-openai-internal-codex-responses-lite",
                "true",
            )?;
        }
        if let Some(turn_metadata) = candidate.identity.turn_metadata.as_deref() {
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
        if self.state.auth_context_invalidation_epoch() != self.auth_context_epoch {
            return Err(StepPreparationError::retain(
                "step_principal_snapshot_invalidated",
            ));
        }
        Ok(())
    }

    async fn validate_step(&self, step: &ResponseCreateStep) -> Result<(), StepPreparationError> {
        self.validate_runtime_fences()?;
        let decision = &self.decision;
        if crate::control::trusted_auth_local_rejection(Some(decision), &self.request_headers)
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
        if !self.cold_policy_validated.load(Ordering::Acquire) {
            if crate::control::request_model_local_rejection_from_json(
                &self.state,
                Some(decision),
                &self.request_uri,
                &step.value,
            )
            .await
            .map_err(|_| StepPreparationError::retain("step_policy_lookup_failed"))?
            .is_some()
            {
                return Err(StepPreparationError::retain("step_policy_rejected"));
            }
            self.cold_policy_validated.store(true, Ordering::Release);
        }
        match self
            .state
            .frontdoor_user_rpm()
            .check_and_consume(&self.state, Some(decision))
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
        let shared_global = self.shared_global.clone();
        if !shared_global.eligible {
            return Err(StepPreparationError::retain("codex_ws_global_disabled"));
        }
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
        let required_capabilities = json!({"codex_official_ws": true});
        let planning_state = self.state.clone();
        let native_account_flags = crate::provider_transport::CodexOfficialWsGlobalFlags {
            enabled: true,
            native_codex_ws_enabled: true,
        };
        let attempts = build_compact_local_openai_responses_stream_plan_and_reports_for_kind_with_required_capabilities(
            &self.state,
            &parts,
            &self.trace_id,
            &self.decision,
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
                    if !crate::provider_transport::resolve_codex_official_ws(
                        transport.as_ref(),
                        native_account_flags,
                    )
                    .profile_effective
                    {
                        return None;
                    }
                    let proxy = state
                        .resolve_transport_proxy_snapshot_with_tunnel_affinity(transport.as_ref())
                        .await;
                    let route = outbound_route(proxy.as_ref())?;
                    Some(CodexWsCandidatePreflight {
                        transport,
                        proxy,
                        route,
                    })
                }
            },
        )
        .await
        .map_err(|_| StepPreparationError::retain("candidate_planning_failed"))?;

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
        let client_api_key_id = self
            .decision
            .auth_context
            .as_ref()
            .map(|auth| auth.api_key_id.clone())
            .unwrap_or_default();

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
            // Freeze reporting/settlement and the connector route from the
            // same concrete proxy resolution. Never resolve a pool key's
            // proxy again after preflight.
            attempt.plan.proxy = preflight.proxy.as_ref().map(compact_proxy_for_plan);
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
            attempt.plan = compact_execution_plan_template(&attempt.plan);
            let provider_concurrent_limit =
                normalize_concurrent_limit(transport.provider.concurrent_limit);
            let key_concurrent_limit = key_concurrent_limits.get(&key_id).copied().flatten();
            let key_rpm_limit = key_rpm_limits.get(&key_id).copied().flatten();
            let account_profile =
                crate::ai_serving::resolve_codex_pool_concrete_account_profile(transport.as_ref())
                    .map(Arc::new);
            let body_rules = transport.endpoint.body_rules.clone().map(Arc::new);
            let force_body_stream_field =
                crate::ai_serving::endpoint_config_forces_upstream_stream_policy(
                    transport.endpoint.config.as_ref(),
                );
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
                client_api_key_id: client_api_key_id.clone(),
                headers,
                response_headers: BTreeMap::new(),
                account_profile,
                report_kind,
                identity: first_step.official_identity.clone(),
                route: preflight.route,
                timeouts,
                lifecycle,
                selected_scheduler_epoch,
                provider_concurrent_limit,
                key_concurrent_limit,
                key_rpm_limit,
                shared_global_generation: shared_global.generation.clone(),
                shared_catalog_generation: shared_catalog.generation.clone(),
                shared_key_generation: key_hot_lease.generation.clone(),
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
        let request = match self.official_request(&candidate) {
            Ok(request) => request,
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
                self.enqueue_handshake_failure(&candidate, &mut cancellation_guard, &failure)
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
                self.enqueue_handshake_failure(&candidate, &mut cancellation_guard, &failure)
                    .await?;
                cancellation_guard.disarm();
                return Err(StepPreparationError::retain("official_ws_connect_timeout"));
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
            drop(connection);
            return Err(error);
        }
        cancellation_guard.restore(&mut candidate);
        let handshake_turn_state = response
            .headers()
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        candidate.response_headers = compact_codex_response_headers(response.headers());
        candidate.headers.clear();
        // Pool-start and unused-candidate effects have been transferred to the
        // lifecycle. Do not retain a duplicate plan/context for an idle,
        // potentially long-lived provider connection.
        drop(candidate.take_planning_attempt());
        Ok(ConnectedCandidate {
            candidate,
            peer: Box::new(OfficialPeer { connection }),
            handshake_turn_state,
        })
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
                let client_api_key_id = candidate.client_api_key_id.clone();
                let request_headers = self.request_headers.clone();
                let account_profile = candidate.account_profile.clone();
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
                        &client_api_key_id,
                        &request_headers,
                        enable_model_directives,
                        model_directive_mapping.as_deref(),
                        provider_body_patch.as_ref(),
                        account_profile.as_deref(),
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
                    &candidate.client_api_key_id,
                    &self.request_headers,
                    candidate.enable_model_directives,
                    candidate.model_directive_mapping.as_deref(),
                    candidate.provider_body_patch.as_ref(),
                    candidate.account_profile.as_deref(),
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
        let lifecycle_report_context =
            step_report_context(candidate.lifecycle.report_context().cloned(), step, None);
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
    ) -> Result<(), StepPreparationError> {
        self.validate_candidate_fences(candidate)?;
        super::hot_state::validate_hot_leases(
            &self.state,
            &candidate.key_id,
            &candidate.shared_global_generation,
            &candidate.shared_catalog_generation,
            &candidate.shared_key_generation,
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
        if !candidate
            .lifecycle
            .queue_handshake_failure(&self.state)
            .await
        {
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
            let (error_type, error_message, route_reason) = match error {
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
            transport_handshake_failure(
                error_type,
                error_message,
                route_reason,
                Some(format!("url_kind={}", url_error_kind_name(&error))),
            )
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
        WebSocketError::Utf8(_) => transport_handshake_failure(
            "codex_ws_handshake_utf8_error",
            "official Codex WebSocket handshake contained invalid text",
            "official_ws_handshake_utf8_failed",
            Some("utf8_error".to_string()),
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

fn step_usage_request_id(step: &ResponseCreateStep) -> String {
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
        let session_id = if !step.official_identity.session_id.trim().is_empty() {
            step.official_identity.session_id.trim()
        } else {
            step.official_identity.thread_id.trim()
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
    client_api_key_id: &str,
    request_headers: &HeaderMap,
    enable_model_directives: bool,
    model_directive_mapping: Option<&serde_json::Value>,
    provider_body_patch: &[RoutingJsonPatchOperation],
    account_profile: Option<&CodexConcreteAccountProfile>,
) -> Result<MaterializedCodexWsStepBody, StepPreparationError> {
    // The shared Codex HTTP normalizer strips previous_response_id because
    // HTTP requests are self-contained. Native Responses WebSocket follow-up
    // steps are different: custom/function tool outputs are resolved against
    // the preceding response on the bound provider connection.
    let previous_response_id = body
        .get("previous_response_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let mut body = crate::ai_serving::build_codex_ws_local_openai_responses_request_body(
        body,
        mapped_model,
        true,
        force_body_stream_field,
        "codex",
        "openai:responses",
        body_rules,
        Some(client_api_key_id),
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
    body_object.insert("stream".to_string(), serde_json::Value::Bool(true));
    if let Some(previous_response_id) = previous_response_id {
        // Restore this after all route/profile edits so a validated connection
        // fence cannot silently turn into an unlinked tool-output request.
        body_object.insert(
            "previous_response_id".to_string(),
            serde_json::Value::String(previous_response_id),
        );
    }
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

impl Stream for OfficialPeer {
    type Item = Result<RelayFrame, PeerError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            let message = match std::pin::Pin::new(&mut self.connection).poll_next(context) {
                std::task::Poll::Ready(Some(Ok(message))) => message,
                std::task::Poll::Ready(Some(Err(_))) => {
                    return std::task::Poll::Ready(Some(Err(PeerError(
                        "official Codex WS receive failed".into(),
                    ))))
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            let frame = match message {
                OfficialMessage::Text(text) => RelayFrame::Text(text.into()),
                OfficialMessage::Binary(bytes) => RelayFrame::Binary(bytes),
                OfficialMessage::Ping(bytes) => RelayFrame::Ping(bytes),
                OfficialMessage::Pong(bytes) => RelayFrame::Pong(bytes),
                OfficialMessage::Close(_) => RelayFrame::Close,
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
            .map_err(|_| PeerError("official Codex WS send failed".into()))
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
            .map_err(|_| PeerError("official Codex WS send failed".into()))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.connection)
            .poll_flush(context)
            .map_err(|_| PeerError("official Codex WS send failed".into()))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.connection)
            .poll_close(context)
            .map_err(|_| PeerError("official Codex WS close failed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_ws::protocol::StepFence;

    #[test]
    fn step_report_context_creates_ws_metadata_without_base_context() {
        let step = ResponseCreateStep {
            value: json!({}),
            encoded_len: 2,
            model: "gpt-test".into(),
            previous_response_id: None,
            logical_turn_id: None,
            official_identity: OfficialRequestIdentity {
                session_id: "session-1".into(),
                thread_id: "thread-1".into(),
                window_id: None,
                turn_metadata: None,
                parent_thread_id: None,
                subagent: None,
                responses_lite: false,
            },
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
            official_identity: OfficialRequestIdentity {
                session_id: "official-session".into(),
                thread_id: "thread-1".into(),
                window_id: None,
                turn_metadata: None,
                parent_thread_id: None,
                subagent: None,
                responses_lite: false,
            },
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
            "client-key-1",
            &HeaderMap::new(),
            false,
            None,
            &[],
            None,
        )
        .expect("follow-up body should materialize");

        assert_eq!(materialized.json["previous_response_id"], "resp-1");
        assert_eq!(
            materialized.json["input"][0]["type"],
            "custom_tool_call_output"
        );
        assert_eq!(materialized.json["input"][0]["call_id"], "call-1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&materialized.text)
                .expect("materialized text should be JSON"),
            materialized.json
        );
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
