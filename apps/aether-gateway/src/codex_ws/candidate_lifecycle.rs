use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aether_data_contracts::repository::candidates::RequestCandidateStatus;
use aether_scheduler_core::SchedulerRequestCandidateStatusUpdate;

use crate::orchestration::{
    apply_local_execution_effect, apply_local_pool_terminal_effect_after_lease_release,
    prepare_pool_failover_after_candidate_failure, release_local_pool_key_lease_for_attempt_strict,
    resolve_local_failover_analysis_for_attempt, stop_local_pool_sticky_init_renewer_for_attempt,
    LocalAdaptiveRateLimitEffect, LocalAdaptiveSuccessEffect, LocalAttemptFailureEffect,
    LocalExecutionEffect, LocalExecutionEffectContext, LocalFailoverClassification,
    LocalHealthFailureEffect, LocalHealthSuccessEffect, LocalOAuthInvalidationEffect,
    LocalPoolErrorEffect,
};
use crate::request_candidate_runtime::record_local_request_candidate_status;
use crate::usage::GatewayStreamReportRequest;
use crate::AppState;

const POOL_LEASE_RELEASE_IDLE: u8 = 0;
const POOL_LEASE_RELEASE_IN_FLIGHT: u8 = 1;
const POOL_LEASE_RELEASED: u8 = 2;
const POOL_LEASE_RELEASE_TIMEOUT: Duration = Duration::from_millis(100);
const TERMINAL_IDLE: u8 = 0;
const TERMINAL_IN_FLIGHT: u8 = 1;
const TERMINAL_COMPLETE: u8 = 2;
const TERMINAL_QUEUED: u8 = 3;
const SETTLEMENT_LEASE_RELEASE_RETRIES: usize = 4;
const SETTLEMENT_LEASE_RELEASE_BACKOFF_MS: [u64; 3] = [10, 25, 50];
static POOL_LEASE_RELEASE_EXHAUSTED_TOTAL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexWsStepDisposition {
    Completed,
    ProviderFailure {
        status_code: u16,
        error_type: String,
        error_message: String,
        error_body: Option<String>,
    },
    StreamTimeout {
        error_type: String,
        error_message: String,
    },
    Cancelled {
        error_type: String,
        error_message: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct CodexWsCandidateSettlement {
    lifecycle: Arc<CodexWsCandidateLifecycle>,
    disposition: CodexWsStepDisposition,
}

impl CodexWsCandidateSettlement {
    pub(crate) fn disposition(&self) -> &CodexWsStepDisposition {
        &self.disposition
    }

    async fn release_pool_lease(&self, state: &AppState) {
        self.lifecycle
            .release_pool_lease_for_settlement(state)
            .await;
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CodexWsStepSettlement {
    First(CodexWsCandidateSettlement),
    Subsequent {
        lifecycle: Arc<CodexWsCandidateLifecycle>,
        disposition: CodexWsStepDisposition,
        progress: Arc<SettlementProgress>,
    },
}

impl CodexWsStepSettlement {
    pub(crate) fn subsequent(
        lifecycle: Arc<CodexWsCandidateLifecycle>,
        disposition: CodexWsStepDisposition,
    ) -> Self {
        Self::Subsequent {
            lifecycle,
            disposition,
            progress: Arc::new(SettlementProgress::default()),
        }
    }

    pub(crate) fn disposition(&self) -> &CodexWsStepDisposition {
        match self {
            Self::First(settlement) => settlement.disposition(),
            Self::Subsequent { disposition, .. } => disposition,
        }
    }

    pub(crate) fn stop_candidate_renewer(&self) {
        if let Self::First(settlement) = self {
            settlement.lifecycle.stop_pool_sticky_renewer();
        }
    }

    pub(crate) async fn release_candidate_lease(&self, state: &AppState) {
        if let Self::First(settlement) = self {
            settlement.release_pool_lease(state).await;
        }
    }

    pub(crate) async fn settle_fast(
        self,
        state: &AppState,
        plan: &aether_contracts::ExecutionPlan,
        report_context: Option<&serde_json::Value>,
        payload: &GatewayStreamReportRequest,
    ) {
        let Some(terminal_claim) = self.claim_terminal() else {
            return;
        };
        self.stop_candidate_renewer();
        self.release_candidate_lease(state).await;
        let disposition = self.disposition().clone();
        let failure_classification =
            resolve_step_failure_classification(state, plan, report_context, &disposition).await;
        let progress = self.progress();
        if let Self::First(settlement) = &self {
            if !settlement
                .lifecycle
                .settle_first_dispatch(
                    state,
                    payload,
                    &disposition,
                    failure_classification,
                    disposition_error_body(&disposition),
                    progress,
                )
                .await
            {
                return;
            }
        } else if let Self::Subsequent {
            lifecycle,
            disposition,
            ..
        } = &self
        {
            if !run_settlement_phase(
                &progress.pool_effect_state,
                lifecycle.apply_pool_terminal_effect(
                    state,
                    payload,
                    disposition,
                    failure_classification,
                    disposition_error_body(disposition),
                ),
            )
            .await
            {
                return;
            }
        }
        if !apply_step_health_effects(
            state,
            plan,
            report_context,
            payload,
            &disposition,
            failure_classification,
            progress,
        )
        .await
        {
            return;
        }
        terminal_claim.complete();
    }

    fn progress(&self) -> &SettlementProgress {
        match self {
            Self::First(settlement) => settlement.lifecycle.settlement_progress.as_ref(),
            Self::Subsequent { progress, .. } => progress.as_ref(),
        }
    }

    fn claim_terminal(&self) -> Option<TerminalClaim> {
        TerminalClaim::begin(Arc::clone(&self.progress().terminal_state))
    }
}

#[derive(Debug)]
pub(crate) struct SettlementProgress {
    terminal_state: Arc<AtomicU8>,
    pool_effect_state: AtomicU8,
    candidate_status_state: AtomicU8,
    attempt_failure_state: AtomicU8,
    adaptive_state: AtomicU8,
    health_state: AtomicU8,
    oauth_state: AtomicU8,
}

impl Default for SettlementProgress {
    fn default() -> Self {
        Self {
            terminal_state: Arc::new(AtomicU8::new(TERMINAL_IDLE)),
            pool_effect_state: AtomicU8::new(TERMINAL_IDLE),
            candidate_status_state: AtomicU8::new(TERMINAL_IDLE),
            attempt_failure_state: AtomicU8::new(TERMINAL_IDLE),
            adaptive_state: AtomicU8::new(TERMINAL_IDLE),
            health_state: AtomicU8::new(TERMINAL_IDLE),
            oauth_state: AtomicU8::new(TERMINAL_IDLE),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CodexWsCandidateLifecycle {
    plan: Arc<aether_contracts::ExecutionPlan>,
    report_context: Option<Arc<serde_json::Value>>,
    started_at_unix_ms: AtomicU64,
    pool_lease_release_state: AtomicU8,
    settlement_progress: Arc<SettlementProgress>,
}

impl CodexWsCandidateLifecycle {
    pub(crate) fn new(
        plan: &aether_contracts::ExecutionPlan,
        report_context: Option<&serde_json::Value>,
    ) -> Self {
        Self {
            plan: Arc::new(compact_execution_plan_template(plan)),
            report_context: compact_report_context_template(report_context).map(Arc::new),
            started_at_unix_ms: AtomicU64::new(0),
            pool_lease_release_state: AtomicU8::new(POOL_LEASE_RELEASE_IDLE),
            settlement_progress: Arc::new(SettlementProgress::default()),
        }
    }

    pub(crate) fn plan(&self) -> &aether_contracts::ExecutionPlan {
        self.plan.as_ref()
    }

    pub(crate) fn report_context(&self) -> Option<&serde_json::Value> {
        self.report_context.as_deref()
    }

    pub(crate) fn original_request_id(&self) -> &str {
        self.plan.request_id.as_str()
    }

    pub(crate) fn is_terminal_claimed(&self) -> bool {
        self.settlement_progress
            .terminal_state
            .load(Ordering::Acquire)
            != TERMINAL_IDLE
    }

    pub(crate) fn first_settlement(
        self: &Arc<Self>,
        disposition: CodexWsStepDisposition,
    ) -> CodexWsStepSettlement {
        CodexWsStepSettlement::First(CodexWsCandidateSettlement {
            lifecycle: Arc::clone(self),
            disposition,
        })
    }

    pub(crate) fn mark_started(&self) {
        let started_at_unix_ms = crate::clock::current_unix_ms();
        let _ = self.started_at_unix_ms.compare_exchange(
            0,
            started_at_unix_ms,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) async fn abort_before_write(
        &self,
        state: &AppState,
        status: RequestCandidateStatus,
        status_code: Option<u16>,
        error_type: &'static str,
        error_message: &str,
    ) -> bool {
        let Some(terminal_claim) = self.claim_terminal() else {
            return false;
        };
        self.release_pool_lease_for_settlement(state).await;
        let context = LocalExecutionEffectContext {
            plan: self.plan(),
            report_context: self.report_context(),
        };
        if !run_settlement_phase(
            &self.settlement_progress.pool_effect_state,
            apply_local_pool_terminal_effect_after_lease_release(
                state,
                context,
                LocalExecutionEffect::PoolAttemptAborted,
            ),
        )
        .await
        {
            return false;
        }
        if !run_settlement_phase(
            &self.settlement_progress.candidate_status_state,
            record_local_request_candidate_status(
                state,
                self.plan(),
                self.report_context(),
                SchedulerRequestCandidateStatusUpdate {
                    status,
                    status_code,
                    error_type: Some(error_type.to_string()),
                    error_message: Some(error_message.to_string()),
                    latency_ms: None,
                    started_at_unix_ms: self.started_at_unix_ms(),
                    finished_at_unix_ms: Some(crate::clock::current_unix_ms()),
                },
            ),
        )
        .await
        {
            return false;
        }
        if let Some(status_code) = status_code {
            if !run_settlement_phase(
                &self.settlement_progress.health_state,
                apply_local_execution_effect(
                    state,
                    context,
                    LocalExecutionEffect::HealthFailure(LocalHealthFailureEffect {
                        status_code,
                        classification: LocalFailoverClassification::RetryUpstreamFailure,
                    }),
                ),
            )
            .await
            {
                return false;
            }
        }
        terminal_claim.complete();
        true
    }

    pub(crate) async fn settle_handshake_failure(
        self: &Arc<Self>,
        state: &AppState,
        status_code: u16,
        response_headers: std::collections::BTreeMap<String, String>,
        error_type: String,
        error_message: String,
        error_body: Option<String>,
    ) {
        let disposition = CodexWsStepDisposition::ProviderFailure {
            status_code,
            error_type,
            error_message,
            error_body: error_body.clone(),
        };
        let payload = GatewayStreamReportRequest {
            trace_id: self.original_request_id().to_string(),
            report_kind: "codex_ws_handshake_failure".to_string(),
            report_context: self.report_context().cloned(),
            status_code,
            headers: response_headers,
            provider_body_base64: None,
            provider_body_state: None,
            client_body_base64: None,
            client_body_state: None,
            terminal_summary: None,
            telemetry: None,
        };
        self.first_settlement(disposition)
            .settle_fast(state, self.plan(), self.report_context(), &payload)
            .await;
    }

    /// Claims terminal ownership before the reserved settlement commit is sent. This step must
    /// not await so cancellation cannot strand the lifecycle after its cleanup permit is taken.
    pub(crate) fn claim_handshake_failure_settlement(&self) -> bool {
        if self
            .settlement_progress
            .terminal_state
            .compare_exchange(
                TERMINAL_IDLE,
                TERMINAL_QUEUED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        self.stop_pool_sticky_renewer();
        true
    }

    /// Releases scheduling state needed by the next candidate. The full failure settlement has
    /// already been queued, so cancellation during this fast path cannot lose terminal cleanup.
    pub(crate) async fn prepare_failover_after_handshake_failure(&self, state: &AppState) {
        let _ = self.release_pool_lease_once(state).await;
        prepare_pool_failover_after_candidate_failure(
            state,
            LocalExecutionEffectContext {
                plan: self.plan(),
                report_context: self.report_context(),
            },
        )
        .await;
    }

    async fn settle_first_dispatch(
        &self,
        state: &AppState,
        payload: &GatewayStreamReportRequest,
        disposition: &CodexWsStepDisposition,
        failure_classification: Option<LocalFailoverClassification>,
        terminal_error_body: Option<&str>,
        progress: &SettlementProgress,
    ) -> bool {
        let (status, status_code, error_type, error_message) = match disposition {
            CodexWsStepDisposition::Completed => (
                RequestCandidateStatus::Success,
                Some(payload.status_code),
                None,
                None,
            ),
            CodexWsStepDisposition::ProviderFailure {
                status_code,
                error_type,
                error_message,
                ..
            } => (
                RequestCandidateStatus::Failed,
                Some(*status_code),
                Some(error_type.clone()),
                Some(error_message.clone()),
            ),
            CodexWsStepDisposition::StreamTimeout {
                error_type,
                error_message,
            } => (
                RequestCandidateStatus::Failed,
                Some(http::StatusCode::GATEWAY_TIMEOUT.as_u16()),
                Some(error_type.clone()),
                Some(error_message.clone()),
            ),
            CodexWsStepDisposition::Cancelled {
                error_type,
                error_message,
            } => (
                RequestCandidateStatus::Cancelled,
                None,
                Some(error_type.clone()),
                Some(error_message.clone()),
            ),
        };
        let finished_at_unix_ms = crate::clock::current_unix_ms();
        if !run_settlement_phase(
            &progress.pool_effect_state,
            self.apply_pool_terminal_effect(
                state,
                payload,
                disposition,
                failure_classification,
                terminal_error_body,
            ),
        )
        .await
        {
            return false;
        }
        if !run_settlement_phase(
            &progress.candidate_status_state,
            record_local_request_candidate_status(
                state,
                self.plan(),
                self.report_context(),
                SchedulerRequestCandidateStatusUpdate {
                    status,
                    status_code,
                    error_type,
                    error_message,
                    latency_ms: payload
                        .telemetry
                        .as_ref()
                        .and_then(|telemetry| telemetry.elapsed_ms),
                    started_at_unix_ms: self.started_at_unix_ms(),
                    finished_at_unix_ms: Some(finished_at_unix_ms),
                },
            ),
        )
        .await
        {
            return false;
        }
        true
    }

    async fn apply_pool_terminal_effect(
        &self,
        state: &AppState,
        payload: &GatewayStreamReportRequest,
        disposition: &CodexWsStepDisposition,
        failure_classification: Option<LocalFailoverClassification>,
        terminal_error_body: Option<&str>,
    ) {
        let context = LocalExecutionEffectContext {
            plan: self.plan(),
            report_context: self.report_context(),
        };
        match disposition {
            CodexWsStepDisposition::Completed => {
                apply_local_pool_terminal_effect_after_lease_release(
                    state,
                    context,
                    LocalExecutionEffect::PoolSuccessStream { payload },
                )
                .await;
            }
            CodexWsStepDisposition::ProviderFailure {
                status_code,
                error_body,
                ..
            } => {
                apply_local_pool_terminal_effect_after_lease_release(
                    state,
                    context,
                    LocalExecutionEffect::PoolError(LocalPoolErrorEffect {
                        status_code: *status_code,
                        classification: failure_classification
                            .unwrap_or(LocalFailoverClassification::UseDefault),
                        headers: &payload.headers,
                        error_body: error_body.as_deref().or(terminal_error_body),
                    }),
                )
                .await;
            }
            CodexWsStepDisposition::StreamTimeout { .. } => {
                apply_local_pool_terminal_effect_after_lease_release(
                    state,
                    context,
                    LocalExecutionEffect::PoolStreamTimeout,
                )
                .await;
            }
            CodexWsStepDisposition::Cancelled { .. } => {
                apply_local_pool_terminal_effect_after_lease_release(
                    state,
                    context,
                    LocalExecutionEffect::PoolAttemptAborted,
                )
                .await;
            }
        }
    }

    fn claim_terminal(&self) -> Option<TerminalClaim> {
        TerminalClaim::begin(Arc::clone(&self.settlement_progress.terminal_state))
    }

    pub(crate) async fn release_pool_lease_once(&self, state: &AppState) -> bool {
        self.stop_pool_sticky_renewer();
        let release = release_local_pool_key_lease_for_attempt_strict(
            state,
            LocalExecutionEffectContext {
                plan: self.plan(),
                report_context: self.report_context(),
            },
        );
        match run_pool_lease_release_attempt(
            &self.pool_lease_release_state,
            POOL_LEASE_RELEASE_TIMEOUT,
            release,
        )
        .await
        {
            Ok(()) => true,
            Err(PoolLeaseReleaseAttemptError::Busy) => false,
            Err(PoolLeaseReleaseAttemptError::Backend(error)) => {
                tracing::warn!(
                    event_name = "codex_ws_pool_lease_release_failed",
                    log_type = "ops",
                    provider_id = %self.plan().provider_id,
                    key_id = %self.plan().key_id,
                    error = ?error,
                    "Codex WebSocket pool lease release failed and remains retryable"
                );
                false
            }
            Err(PoolLeaseReleaseAttemptError::Timeout) => {
                tracing::warn!(
                    event_name = "codex_ws_pool_lease_release_timeout",
                    log_type = "ops",
                    provider_id = %self.plan().provider_id,
                    key_id = %self.plan().key_id,
                    timeout_ms = POOL_LEASE_RELEASE_TIMEOUT.as_millis(),
                    "Codex WebSocket pool lease release exceeded its bounded deadline"
                );
                false
            }
        }
    }

    pub(crate) async fn release_pool_lease_for_settlement(&self, state: &AppState) -> bool {
        for attempt in 0..SETTLEMENT_LEASE_RELEASE_RETRIES {
            if self.release_pool_lease_once(state).await {
                return true;
            }
            if let Some(delay_ms) = SETTLEMENT_LEASE_RELEASE_BACKOFF_MS.get(attempt) {
                tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            }
        }
        let exhausted_total = POOL_LEASE_RELEASE_EXHAUSTED_TOTAL.fetch_add(1, Ordering::AcqRel) + 1;
        tracing::error!(
            event_name = "codex_ws_pool_lease_release_exhausted",
            log_type = "ops",
            provider_id = %self.plan().provider_id,
            key_id = %self.plan().key_id,
            exhausted_total,
            final_strategy = "lease_ttl_expiry",
            "Codex WebSocket pool lease release retries were exhausted; the bounded lease TTL is the explicit final fallback"
        );
        false
    }

    pub(crate) fn stop_pool_sticky_renewer(&self) {
        stop_local_pool_sticky_init_renewer_for_attempt(LocalExecutionEffectContext {
            plan: self.plan(),
            report_context: self.report_context(),
        });
    }

    fn started_at_unix_ms(&self) -> Option<u64> {
        match self.started_at_unix_ms.load(Ordering::Acquire) {
            0 => None,
            value => Some(value),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PoolLeaseReleaseAttemptError<E> {
    Busy,
    Backend(E),
    Timeout,
}

async fn run_pool_lease_release_attempt<F, E>(
    state: &AtomicU8,
    timeout: Duration,
    release: F,
) -> Result<(), PoolLeaseReleaseAttemptError<E>>
where
    F: Future<Output = Result<bool, E>>,
{
    if state.load(Ordering::Acquire) == POOL_LEASE_RELEASED {
        return Ok(());
    }
    if state
        .compare_exchange(
            POOL_LEASE_RELEASE_IDLE,
            POOL_LEASE_RELEASE_IN_FLIGHT,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return Err(PoolLeaseReleaseAttemptError::Busy);
    }
    let claim = PoolLeaseReleaseClaim {
        state,
        completed: false,
    };
    match tokio::time::timeout(timeout, release).await {
        Ok(Ok(_)) => {
            claim.complete();
            Ok(())
        }
        Ok(Err(error)) => Err(PoolLeaseReleaseAttemptError::Backend(error)),
        Err(_) => Err(PoolLeaseReleaseAttemptError::Timeout),
    }
}

struct PoolLeaseReleaseClaim<'a> {
    state: &'a AtomicU8,
    completed: bool,
}

struct TerminalClaim {
    state: Arc<AtomicU8>,
    fallback_state: u8,
    completed: bool,
}

struct SettlementPhaseClaim<'a> {
    state: &'a AtomicU8,
    completed: bool,
}

async fn run_settlement_phase<F>(state: &AtomicU8, effect: F) -> bool
where
    F: Future<Output = ()>,
{
    if state.load(Ordering::Acquire) == TERMINAL_COMPLETE {
        return true;
    }
    let Some(claim) = SettlementPhaseClaim::begin(state) else {
        return false;
    };
    effect.await;
    claim.complete();
    true
}

impl TerminalClaim {
    fn begin(state: Arc<AtomicU8>) -> Option<Self> {
        let mut current = state.load(Ordering::Acquire);
        loop {
            if current != TERMINAL_IDLE && current != TERMINAL_QUEUED {
                return None;
            }
            match state.compare_exchange_weak(
                current,
                TERMINAL_IN_FLIGHT,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Self {
                        state,
                        fallback_state: current,
                        completed: false,
                    })
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn complete(mut self) {
        self.state.store(TERMINAL_COMPLETE, Ordering::Release);
        self.completed = true;
    }
}

impl Drop for TerminalClaim {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.state.compare_exchange(
                TERMINAL_IN_FLIGHT,
                self.fallback_state,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

impl<'a> SettlementPhaseClaim<'a> {
    fn begin(state: &'a AtomicU8) -> Option<Self> {
        state
            .compare_exchange(
                TERMINAL_IDLE,
                TERMINAL_IN_FLIGHT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| Self {
                state,
                completed: false,
            })
    }

    fn complete(mut self) {
        self.state.store(TERMINAL_COMPLETE, Ordering::Release);
        self.completed = true;
    }
}

impl Drop for SettlementPhaseClaim<'_> {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.state.compare_exchange(
                TERMINAL_IN_FLIGHT,
                TERMINAL_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

impl PoolLeaseReleaseClaim<'_> {
    fn complete(mut self) {
        self.state.store(POOL_LEASE_RELEASED, Ordering::Release);
        self.completed = true;
    }
}

impl Drop for PoolLeaseReleaseClaim<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.state.store(POOL_LEASE_RELEASE_IDLE, Ordering::Release);
        }
    }
}

fn disposition_error_body(disposition: &CodexWsStepDisposition) -> Option<&str> {
    match disposition {
        CodexWsStepDisposition::ProviderFailure { error_body, .. } => error_body.as_deref(),
        CodexWsStepDisposition::Completed
        | CodexWsStepDisposition::StreamTimeout { .. }
        | CodexWsStepDisposition::Cancelled { .. } => None,
    }
}

async fn resolve_step_failure_classification(
    state: &AppState,
    plan: &aether_contracts::ExecutionPlan,
    report_context: Option<&serde_json::Value>,
    disposition: &CodexWsStepDisposition,
) -> Option<LocalFailoverClassification> {
    match disposition {
        CodexWsStepDisposition::ProviderFailure {
            status_code,
            error_body,
            ..
        } => Some(
            resolve_local_failover_analysis_for_attempt(
                state,
                plan,
                report_context,
                *status_code,
                error_body.as_deref(),
            )
            .await
            .classification,
        ),
        CodexWsStepDisposition::StreamTimeout { .. } => Some(
            resolve_local_failover_analysis_for_attempt(
                state,
                plan,
                report_context,
                http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                None,
            )
            .await
            .classification,
        ),
        CodexWsStepDisposition::Completed | CodexWsStepDisposition::Cancelled { .. } => None,
    }
}

async fn apply_step_health_effects(
    state: &AppState,
    plan: &aether_contracts::ExecutionPlan,
    report_context: Option<&serde_json::Value>,
    payload: &GatewayStreamReportRequest,
    disposition: &CodexWsStepDisposition,
    failure_classification: Option<LocalFailoverClassification>,
    progress: &SettlementProgress,
) -> bool {
    let context = LocalExecutionEffectContext {
        plan,
        report_context,
    };
    match disposition {
        CodexWsStepDisposition::Completed => {
            if !run_settlement_phase(
                &progress.health_state,
                apply_local_execution_effect(
                    state,
                    context,
                    LocalExecutionEffect::HealthSuccess(LocalHealthSuccessEffect),
                ),
            )
            .await
            {
                return false;
            }
            if !run_settlement_phase(
                &progress.adaptive_state,
                apply_local_execution_effect(
                    state,
                    context,
                    LocalExecutionEffect::AdaptiveSuccess(LocalAdaptiveSuccessEffect),
                ),
            )
            .await
            {
                return false;
            }
        }
        CodexWsStepDisposition::ProviderFailure {
            status_code,
            error_body,
            ..
        } => {
            if !apply_failure_health_effects(
                state,
                context,
                payload,
                *status_code,
                failure_classification.unwrap_or(LocalFailoverClassification::UseDefault),
                error_body.as_deref(),
                progress,
            )
            .await
            {
                return false;
            }
        }
        CodexWsStepDisposition::StreamTimeout { .. } => {
            if !apply_failure_health_effects(
                state,
                context,
                payload,
                http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                failure_classification.unwrap_or(LocalFailoverClassification::RetryUpstreamFailure),
                None,
                progress,
            )
            .await
            {
                return false;
            }
        }
        CodexWsStepDisposition::Cancelled { .. } => {}
    }
    true
}

async fn apply_failure_health_effects(
    state: &AppState,
    context: LocalExecutionEffectContext<'_>,
    payload: &GatewayStreamReportRequest,
    status_code: u16,
    classification: LocalFailoverClassification,
    error_body: Option<&str>,
    progress: &SettlementProgress,
) -> bool {
    if !run_settlement_phase(
        &progress.attempt_failure_state,
        apply_local_execution_effect(
            state,
            context,
            LocalExecutionEffect::AttemptFailure(LocalAttemptFailureEffect {
                status_code,
                classification,
            }),
        ),
    )
    .await
    {
        return false;
    }
    if !run_settlement_phase(
        &progress.adaptive_state,
        apply_local_execution_effect(
            state,
            context,
            LocalExecutionEffect::AdaptiveRateLimit(LocalAdaptiveRateLimitEffect {
                status_code,
                classification,
                headers: Some(&payload.headers),
            }),
        ),
    )
    .await
    {
        return false;
    }
    if !run_settlement_phase(
        &progress.health_state,
        apply_local_execution_effect(
            state,
            context,
            LocalExecutionEffect::HealthFailure(LocalHealthFailureEffect {
                status_code,
                classification,
            }),
        ),
    )
    .await
    {
        return false;
    }
    if !run_settlement_phase(
        &progress.oauth_state,
        apply_local_execution_effect(
            state,
            context,
            LocalExecutionEffect::OauthInvalidation(LocalOAuthInvalidationEffect {
                status_code,
                response_text: error_body,
            }),
        ),
    )
    .await
    {
        return false;
    }
    true
}

pub(crate) fn compact_execution_plan_template(
    plan: &aether_contracts::ExecutionPlan,
) -> aether_contracts::ExecutionPlan {
    aether_contracts::ExecutionPlan {
        request_id: plan.request_id.clone(),
        candidate_id: plan.candidate_id.clone(),
        provider_name: plan.provider_name.clone(),
        provider_id: plan.provider_id.clone(),
        endpoint_id: plan.endpoint_id.clone(),
        key_id: plan.key_id.clone(),
        method: plan.method.clone(),
        url: plan.url.clone(),
        headers: std::collections::BTreeMap::new(),
        content_type: plan.content_type.clone(),
        content_encoding: plan.content_encoding.clone(),
        body: aether_contracts::RequestBody {
            json_body: None,
            body_bytes_b64: None,
            body_ref: None,
        },
        stream: plan.stream,
        client_api_format: plan.client_api_format.clone(),
        provider_api_format: plan.provider_api_format.clone(),
        model_name: plan.model_name.clone(),
        proxy: plan
            .proxy
            .as_ref()
            .map(|proxy| aether_contracts::ProxySnapshot {
                enabled: proxy.enabled,
                mode: proxy.mode.clone(),
                node_id: proxy.node_id.clone(),
                label: None,
                url: None,
                extra: None,
            }),
        transport_profile: None,
        timeouts: None,
    }
}

pub(crate) fn compact_ws_planning_attempt_plan(
    plan: &aether_contracts::ExecutionPlan,
) -> aether_contracts::ExecutionPlan {
    let mut compact = compact_execution_plan_template(plan);
    compact.headers = plan.headers.clone();
    // Standard Responses WebSocket candidates need the resolved client
    // profile until their physical connection has been established.
    compact.transport_profile = plan.transport_profile.clone();
    compact.timeouts = plan.timeouts.clone();
    compact
}

pub(crate) fn compact_report_context_template(
    report_context: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    const FIELDS: &[&str] = &[
        "request_id",
        "trace_id",
        "user_id",
        "api_key_id",
        "username",
        "api_key_name",
        "provider_name",
        "model",
        "mapped_model",
        "model_id",
        "global_model_id",
        "provider_id",
        "endpoint_id",
        "key_id",
        "client_contract",
        "client_api_format",
        "provider_contract",
        "provider_api_format",
        "candidate_id",
        "candidate_index",
        "retry_index",
        "key_name",
        "planner_kind",
        "route_family",
        "route_kind",
        "execution_path",
        "local_execution_runtime_miss_reason",
        "needs_conversion",
        "client_ip",
        "user_agent",
        "cafecode_uid",
        "cafecode_uname",
        "client_requested_stream",
        "upstream_is_stream",
        "is_compaction",
        "compaction_version",
        "api_key_is_standalone",
        "candidate_group_id",
        "pool_key_index",
        "pool_key_lease_key",
        "pool_key_lease_owner",
        "pool_key_lease_token",
        "pool_key_lease_ttl_ms",
        "pool_sticky_init_owner",
        "pool_sticky_session_token",
        "pool_sticky_bound_key_ineligible",
        "pool_sticky_bound_key_id",
        "pool_sticky_bound_key_ineligible_reason",
        "scheduler_affinity_epoch",
        "client_session_affinity",
        "local_failover_policy",
        "request_auth_channel",
    ];
    const MAX_FIELD_BYTES: usize = 4 * 1024;
    const MAX_CONTEXT_BYTES: usize = 16 * 1024;
    let object = report_context?.as_object()?;
    let mut compact = serde_json::Map::new();
    let mut retained_bytes = 0usize;
    for field in FIELDS {
        let Some(value) = object.get(*field) else {
            continue;
        };
        let Some(value_bytes) = bounded_json_upper_size(value, MAX_FIELD_BYTES, 0) else {
            continue;
        };
        if retained_bytes
            .saturating_add(field.len())
            .saturating_add(value_bytes)
            > MAX_CONTEXT_BYTES
        {
            continue;
        }
        retained_bytes = retained_bytes
            .saturating_add(field.len())
            .saturating_add(value_bytes);
        compact.insert((*field).to_string(), value.clone());
    }
    if let Some(proxy) = compact_safe_proxy_context(object.get("proxy")) {
        if let Some(value_bytes) = bounded_json_upper_size(&proxy, MAX_FIELD_BYTES, 0) {
            if retained_bytes
                .saturating_add("proxy".len())
                .saturating_add(value_bytes)
                <= MAX_CONTEXT_BYTES
            {
                compact.insert("proxy".to_string(), proxy);
            }
        }
    }
    if let Some(serde_json::Value::Object(identity)) =
        aether_usage_runtime::attach_cafecode_identity_metadata(
            None,
            object.get("original_headers"),
        )
    {
        for field in ["cafecode_uid", "cafecode_uname"] {
            if let Some(value) = identity.get(field) {
                compact.insert(field.to_string(), value.clone());
            }
        }
    }
    Some(serde_json::Value::Object(compact))
}

fn compact_safe_proxy_context(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let object = value?.as_object()?;
    let mut compact = serde_json::Map::new();
    for field in ["node_id", "node_name", "source"] {
        let Some(value) = object
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 512)
        else {
            continue;
        };
        compact.insert(
            field.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
    (!compact.is_empty()).then_some(serde_json::Value::Object(compact))
}

fn bounded_json_upper_size(
    value: &serde_json::Value,
    remaining: usize,
    depth: usize,
) -> Option<usize> {
    if depth > 16 {
        return None;
    }
    let primitive = match value {
        serde_json::Value::Null => Some(4),
        serde_json::Value::Bool(true) => Some(4),
        serde_json::Value::Bool(false) => Some(5),
        serde_json::Value::Number(_) => Some(32),
        serde_json::Value::String(value) => value.len().checked_mul(6)?.checked_add(2),
        serde_json::Value::Array(values) => {
            let mut size = 2usize;
            for value in values {
                size = size.checked_add(1)?.checked_add(bounded_json_upper_size(
                    value,
                    remaining.saturating_sub(size),
                    depth + 1,
                )?)?;
                if size > remaining {
                    return None;
                }
            }
            Some(size)
        }
        serde_json::Value::Object(values) => {
            let mut size = 2usize;
            for (key, value) in values {
                size = size
                    .checked_add(1)?
                    .checked_add(key.len().checked_mul(6)?.checked_add(3)?)?
                    .checked_add(bounded_json_upper_size(
                        value,
                        remaining.saturating_sub(size),
                        depth + 1,
                    )?)?;
                if size > remaining {
                    return None;
                }
            }
            Some(size)
        }
    }?;
    (primitive <= remaining).then_some(primitive)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;

    use serde_json::json;

    use super::*;

    fn plan_with_large_body() -> aether_contracts::ExecutionPlan {
        aether_contracts::ExecutionPlan {
            request_id: "request-original".into(),
            candidate_id: Some("candidate-original".into()),
            provider_name: Some("Codex".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "POST".into(),
            url: "wss://chatgpt.com/backend-api/codex/responses".into(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(json!({
                "input": "x".repeat(1024 * 1024)
            })),
            stream: true,
            client_api_format: "openai:responses".into(),
            provider_api_format: "openai:responses".into(),
            model_name: Some("gpt-test".into()),
            proxy: None,
            transport_profile: None,
            timeouts: Some(aether_contracts::ExecutionTimeouts {
                connect_ms: Some(1234),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn lifecycle_templates_drop_bodies_and_keep_bounded_identity_pool_fields() {
        let plan = plan_with_large_body();
        let context = json!({
            "request_id": "request-original",
            "candidate_id": "candidate-original",
            "provider_id": "provider-1",
            "pool_key_lease_key": "lease-key-1",
            "pool_key_lease_owner": "lease-owner-1",
            "pool_key_lease_token": "lease-token-1",
            "pool_key_lease_ttl_ms": 30000,
            "scheduler_affinity_key": "affinity-1",
            "original_request_body": {"input": "x".repeat(1024 * 1024)},
            "provider_request_body": {"input": "x".repeat(1024 * 1024)}
        });
        let lifecycle = CodexWsCandidateLifecycle::new(&plan, Some(&context));

        assert!(lifecycle.plan().body.json_body.is_none());
        assert!(lifecycle.plan().body.body_bytes_b64.is_none());
        assert!(lifecycle.plan().body.body_ref.is_none());
        assert!(lifecycle.plan().timeouts.is_none());
        let compact_context = lifecycle.report_context().expect("compact context");
        assert!(compact_context.get("original_request_body").is_none());
        assert!(compact_context.get("provider_request_body").is_none());
        assert_eq!(
            compact_context
                .get("pool_key_lease_token")
                .and_then(serde_json::Value::as_str),
            Some("lease-token-1")
        );
        assert_eq!(lifecycle.original_request_id(), "request-original");
        assert_eq!(
            lifecycle.plan().candidate_id.as_deref(),
            Some("candidate-original")
        );
        assert!(plan.body.json_body.is_some());
        assert!(!lifecycle.is_terminal_claimed());
    }

    #[test]
    fn lifecycle_report_context_keeps_only_cafecode_identity_from_original_headers() {
        let context = json!({
            "request_id": "request-original",
            "is_compaction": true,
            "compaction_version": "v2",
            "original_headers": {
                "authorization": "Bearer secret",
                "cafecode-uid": "372",
                "Cafecode-Uname": "xiapeng8618"
            }
        });

        let compact = compact_report_context_template(Some(&context)).expect("compact context");

        assert_eq!(compact["cafecode_uid"], "372");
        assert_eq!(compact["cafecode_uname"], "xiapeng8618");
        assert_eq!(compact["is_compaction"], true);
        assert_eq!(compact["compaction_version"], "v2");
        assert!(compact.get("original_headers").is_none());
        assert!(!compact.to_string().contains("Bearer secret"));
    }

    #[test]
    fn cancelled_terminal_claim_returns_to_idle_and_complete_claim_stays_closed() {
        let lifecycle = CodexWsCandidateLifecycle::new(&plan_with_large_body(), None);
        let claim = lifecycle.claim_terminal().expect("first claim");
        assert!(lifecycle.is_terminal_claimed());
        drop(claim);
        assert!(!lifecycle.is_terminal_claimed());

        lifecycle.claim_terminal().expect("retry claim").complete();
        assert!(lifecycle.is_terminal_claimed());
        assert!(lifecycle.claim_terminal().is_none());
    }

    #[test]
    fn handshake_failure_claim_is_synchronous_and_reserved_for_settlement() {
        let lifecycle = CodexWsCandidateLifecycle::new(&plan_with_large_body(), None);

        assert!(lifecycle.claim_handshake_failure_settlement());
        assert!(lifecycle.is_terminal_claimed());
        assert!(!lifecycle.claim_handshake_failure_settlement());

        lifecycle
            .claim_terminal()
            .expect("queued settlement should claim terminal ownership")
            .complete();
        assert!(lifecycle.claim_terminal().is_none());
    }

    #[tokio::test]
    async fn cancelled_settlement_phase_retries_only_the_unconfirmed_phase() {
        let phase = AtomicU8::new(TERMINAL_IDLE);
        let calls = AtomicUsize::new(0);
        let mut cancelled = Box::pin(run_settlement_phase(&phase, async {
            calls.fetch_add(1, Ordering::AcqRel);
            std::future::pending::<()>().await;
        }));
        assert!(futures_util::poll!(cancelled.as_mut()).is_pending());
        assert_eq!(phase.load(Ordering::Acquire), TERMINAL_IN_FLIGHT);
        drop(cancelled);
        assert_eq!(phase.load(Ordering::Acquire), TERMINAL_IDLE);

        assert!(
            run_settlement_phase(&phase, async {
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .await
        );
        assert_eq!(phase.load(Ordering::Acquire), TERMINAL_COMPLETE);
        assert!(
            run_settlement_phase(&phase, async {
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .await
        );
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn pool_lease_release_error_timeout_and_contention_remain_retryable() {
        let backend_state = AtomicU8::new(POOL_LEASE_RELEASE_IDLE);
        assert_eq!(
            run_pool_lease_release_attempt(
                &backend_state,
                Duration::from_secs(1),
                std::future::ready(Err("backend")),
            )
            .await,
            Err(PoolLeaseReleaseAttemptError::Backend("backend"))
        );
        assert_eq!(
            backend_state.load(Ordering::Acquire),
            POOL_LEASE_RELEASE_IDLE
        );
        assert!(run_pool_lease_release_attempt(
            &backend_state,
            Duration::from_secs(1),
            std::future::ready(Ok::<_, &str>(true)),
        )
        .await
        .is_ok());
        assert_eq!(backend_state.load(Ordering::Acquire), POOL_LEASE_RELEASED);

        let timeout_state = AtomicU8::new(POOL_LEASE_RELEASE_IDLE);
        assert_eq!(
            run_pool_lease_release_attempt(
                &timeout_state,
                Duration::ZERO,
                std::future::pending::<Result<bool, &str>>(),
            )
            .await,
            Err(PoolLeaseReleaseAttemptError::Timeout)
        );
        assert_eq!(
            timeout_state.load(Ordering::Acquire),
            POOL_LEASE_RELEASE_IDLE
        );

        let contended_state = AtomicU8::new(POOL_LEASE_RELEASE_IDLE);
        let calls = AtomicUsize::new(0);
        let mut in_flight = Box::pin(run_pool_lease_release_attempt(
            &contended_state,
            Duration::from_secs(60),
            async {
                calls.fetch_add(1, Ordering::AcqRel);
                std::future::pending::<Result<bool, &str>>().await
            },
        ));
        assert!(futures_util::poll!(in_flight.as_mut()).is_pending());
        assert_eq!(
            run_pool_lease_release_attempt(&contended_state, Duration::from_secs(1), async {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok::<_, &str>(true)
            },)
            .await,
            Err(PoolLeaseReleaseAttemptError::Busy)
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
        drop(in_flight);
        assert_eq!(
            contended_state.load(Ordering::Acquire),
            POOL_LEASE_RELEASE_IDLE
        );
        assert!(run_pool_lease_release_attempt(
            &contended_state,
            Duration::from_secs(1),
            std::future::ready(Ok::<_, &str>(true)),
        )
        .await
        .is_ok());
        assert_eq!(contended_state.load(Ordering::Acquire), POOL_LEASE_RELEASED);
    }

    #[test]
    fn compact_templates_drop_large_proxy_profile_and_secret_context_fields() {
        let mut plan = plan_with_large_body();
        plan.proxy = Some(aether_contracts::ProxySnapshot {
            enabled: Some(true),
            mode: Some("forward".into()),
            node_id: Some("node-1".into()),
            label: Some("x".repeat(1024 * 1024)),
            url: Some("http://user:password@proxy.invalid:8080".into()),
            extra: Some(json!({"blob": "x".repeat(1024 * 1024)})),
        });
        plan.transport_profile = Some(aether_contracts::ResolvedTransportProfile {
            extra: Some(json!({"blob": "y".repeat(1024 * 1024)})),
            ..Default::default()
        });
        let compact_plan = compact_execution_plan_template(&plan);
        let proxy = compact_plan.proxy.expect("sanitized proxy marker");
        assert_eq!(proxy.node_id.as_deref(), Some("node-1"));
        assert!(proxy.url.is_none());
        assert!(proxy.label.is_none());
        assert!(proxy.extra.is_none());
        assert!(compact_plan.transport_profile.is_none());
        assert!(compact_plan.timeouts.is_none());

        let context = json!({
            "request_id": "request-1",
            "provider_id": "provider-1",
            "pool_key_lease_token": "lease-token-1",
            "client_session_affinity": {"kind": "header", "value": "session-1"},
            "provider_request_headers": {"authorization": "Bearer upstream-secret"},
            "original_headers": {"authorization": "Bearer client-secret"},
            "header_rules": {"blob": "z".repeat(1024 * 1024)},
            "body_rules": {"blob": "z".repeat(1024 * 1024)},
            "ranking": {"blob": "z".repeat(1024 * 1024)},
            "proxy": {
                "node_id": "node-1",
                "node_name": "proxy-one",
                "source": "key",
                "url": "https://proxy.example:443"
            },
        });
        let compact_context = compact_report_context_template(Some(&context)).expect("context");
        let encoded = serde_json::to_vec(&compact_context).expect("compact context JSON");
        assert!(encoded.len() <= 16 * 1024);
        let encoded = String::from_utf8(encoded).expect("JSON is UTF-8");
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("upstream-secret"));
        assert!(!encoded.contains("client-secret"));
        assert!(!encoded.contains(&"z".repeat(64 * 1024)));
        assert_eq!(compact_context["proxy"]["node_id"], "node-1");
        assert_eq!(compact_context["proxy"]["source"], "key");
        assert!(compact_context["proxy"].get("url").is_none());
    }
}
