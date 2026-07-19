use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;

use super::protocol::{
    classify_server_event, parse_response_create, route_control_event,
    validate_official_turn_state, MiddleRouteDisposition, ProtocolError, ResponseCreateContext,
    ResponseCreateStep, RouteControlAction, TerminalEventSummary, TerminalKind,
    FIRST_FRAME_TIMEOUT, MAX_PUBLIC_CLIENT_PAYLOAD_BYTES,
};
use super::runtime::{
    CodexWsCandidate, CodexWsRuntimePort, CodexWsStepUsageContext, CodexWsTimeouts,
    ConnectedCandidate, RelayFrame, RelayPeer, StepExecutionGuard, StepExecutionLeaseStatus,
    StepPreparationError, UsageReportReservation,
};
use super::CodexWsStepDisposition;

const CONTROL_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const TIMEOUT_CLOSE_GRACE: Duration = Duration::from_millis(100);
const TERMINAL_DELIVERY_MIN_GRACE: Duration = Duration::from_secs(5);
const TERMINAL_DELIVERY_MAX_GRACE: Duration = Duration::from_secs(30);
const MAX_INITIAL_CONNECT_BUDGET: Duration = Duration::from_secs(60);

enum OwnedResponseCreateContext {
    First,
    Bound {
        model: String,
        expected_previous_response_id: Option<String>,
        turn_state: Option<(String, String)>,
    },
}

impl OwnedResponseCreateContext {
    fn borrowed(&self) -> ResponseCreateContext<'_> {
        match self {
            Self::First => ResponseCreateContext::First,
            Self::Bound {
                model,
                expected_previous_response_id,
                turn_state,
            } => ResponseCreateContext::Bound {
                model,
                expected_previous_response_id: expected_previous_response_id.as_deref(),
                turn_state: turn_state
                    .as_ref()
                    .map(|(turn_id, state)| (turn_id.as_str(), state.as_str())),
            },
        }
    }
}

async fn parse_response_create_with_cpu_budget(
    text: Bytes,
    context: OwnedResponseCreateContext,
) -> Result<ResponseCreateStep, ProtocolError> {
    if !super::cpu_budget::requires_large_frame_cpu_budget(text.len()) {
        return parse_response_create(&text, context.borrowed());
    }

    let permit = super::cpu_budget::acquire_large_frame_cpu_budget(text.len())
        .await
        .map_err(|_| ProtocolError::Policy("large response.create CPU capacity unavailable"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        parse_response_create(&text, context.borrowed())
    })
    .await
    .map_err(|_| ProtocolError::Policy("large response.create processing failed"))?
}

async fn classify_server_event_with_cpu_budget(
    text: &Bytes,
) -> Result<super::protocol::ServerEventClassification, ProtocolError> {
    if !super::cpu_budget::requires_large_frame_cpu_budget(text.len()) {
        return classify_server_event(text);
    }
    let permit = super::cpu_budget::acquire_large_frame_cpu_budget(text.len())
        .await
        .map_err(|_| ProtocolError::Upstream("large official frame CPU capacity is unavailable"))?;
    let text = text.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        classify_server_event(&text)
    })
    .await
    .map_err(|_| ProtocolError::Upstream("large official frame processing failed"))?
}

fn official_text_frame_within_public_limit(frame: &Bytes) -> bool {
    frame.len() <= MAX_PUBLIC_CLIENT_PAYLOAD_BYTES
}

struct RemainingCandidatesGuard<'a> {
    runtime: &'a dyn CodexWsRuntimePort,
    candidates: VecDeque<CodexWsCandidate>,
}

impl<'a> RemainingCandidatesGuard<'a> {
    fn new(runtime: &'a dyn CodexWsRuntimePort, candidates: Vec<CodexWsCandidate>) -> Self {
        Self {
            runtime,
            candidates: candidates.into(),
        }
    }

    fn next(&mut self) -> Option<CodexWsCandidate> {
        self.candidates.pop_front()
    }

    fn finish(mut self) {
        self.runtime
            .mark_unused_candidates_detached(self.candidates.drain(..).collect());
    }
}

impl Drop for RemainingCandidatesGuard<'_> {
    fn drop(&mut self) {
        self.runtime
            .mark_unused_candidates_detached(self.candidates.drain(..).collect());
    }
}

struct CandidateAttemptGuard<'a> {
    runtime: &'a dyn CodexWsRuntimePort,
    candidate: &'a CodexWsCandidate,
    cleanup_permit: Option<tokio::sync::mpsc::OwnedPermit<super::CodexWsSettlementCommit>>,
    pending_before_write: bool,
}

impl<'a> CandidateAttemptGuard<'a> {
    fn new(
        runtime: &'a dyn CodexWsRuntimePort,
        candidate: &'a CodexWsCandidate,
        cleanup_permit: Option<tokio::sync::mpsc::OwnedPermit<super::CodexWsSettlementCommit>>,
    ) -> Self {
        Self {
            runtime,
            candidate,
            cleanup_permit,
            pending_before_write: true,
        }
    }

    async fn abort(&mut self) {
        if !self.pending_before_write {
            return;
        }
        self.runtime.abort_candidate(self.candidate).await;
        drop(self.cleanup_permit.take());
        self.pending_before_write = false;
    }

    fn mark_provider_write_attempted(&mut self) -> bool {
        let first_dispatch = self.pending_before_write;
        drop(self.cleanup_permit.take());
        self.pending_before_write = false;
        first_dispatch
    }
}

impl Drop for CandidateAttemptGuard<'_> {
    fn drop(&mut self) {
        if self.pending_before_write {
            self.runtime
                .abort_candidate_detached(self.candidate, self.cleanup_permit.take());
        }
    }
}

struct StepUsageLifecycleGuard<'a> {
    runtime: &'a dyn CodexWsRuntimePort,
    usage_context: Option<CodexWsStepUsageContext>,
    started_at: tokio::time::Instant,
}

impl<'a> StepUsageLifecycleGuard<'a> {
    fn new(runtime: &'a dyn CodexWsRuntimePort, started_at: tokio::time::Instant) -> Self {
        Self {
            runtime,
            usage_context: None,
            started_at,
        }
    }

    fn bind(&mut self, candidate: &CodexWsCandidate, step: &ResponseCreateStep) {
        let usage_context = CodexWsStepUsageContext::new(candidate, step);
        if self.usage_context.is_none() {
            self.runtime.record_step_pending(&usage_context);
        }
        self.usage_context = Some(usage_context);
    }

    fn started_at(&self) -> tokio::time::Instant {
        self.started_at
    }

    fn reject(
        mut self,
        status_code: u16,
        error_type: &'static str,
        error_message: &'static str,
        cancelled: bool,
    ) {
        self.record_rejection(status_code, error_type, error_message, cancelled);
    }

    fn disarm(mut self) {
        self.usage_context = None;
    }

    fn record_rejection(
        &mut self,
        status_code: u16,
        error_type: &'static str,
        error_message: &'static str,
        cancelled: bool,
    ) {
        let Some(usage_context) = self.usage_context.take() else {
            return;
        };
        self.runtime.record_step_rejected(
            usage_context,
            self.started_at.elapsed(),
            status_code,
            error_type,
            error_message,
            cancelled,
        );
    }
}

impl Drop for StepUsageLifecycleGuard<'_> {
    fn drop(&mut self) {
        self.record_rejection(
            499,
            "codex_ws_step_cancelled_before_execution",
            "Codex WebSocket step was cancelled before provider execution",
            true,
        );
    }
}

struct StepSettlementGuard<'a> {
    runtime: &'a dyn CodexWsRuntimePort,
    candidate: &'a CodexWsCandidate,
    step: &'a ResponseCreateStep,
    started_at: tokio::time::Instant,
    execution_guard: Option<StepExecutionGuard>,
    usage_report: Option<UsageReportReservation>,
    first_dispatch: bool,
    first_byte_elapsed: Option<Duration>,
}

impl<'a> StepSettlementGuard<'a> {
    fn new(
        runtime: &'a dyn CodexWsRuntimePort,
        candidate: &'a CodexWsCandidate,
        step: &'a ResponseCreateStep,
        started_at: tokio::time::Instant,
        execution_guard: StepExecutionGuard,
        usage_report: UsageReportReservation,
        first_dispatch: bool,
    ) -> Self {
        Self {
            runtime,
            candidate,
            step,
            started_at,
            execution_guard: Some(execution_guard),
            usage_report: Some(usage_report),
            first_dispatch,
            first_byte_elapsed: None,
        }
    }

    fn record_stream_started(&mut self, first_byte_elapsed: Duration) {
        if self.first_byte_elapsed.is_some() {
            return;
        }
        self.first_byte_elapsed = Some(first_byte_elapsed);
        let Some(usage_report) = self.usage_report.as_ref() else {
            return;
        };
        self.runtime.record_step_stream_started(
            self.candidate,
            self.step,
            first_byte_elapsed,
            usage_report,
        );
    }

    async fn finish(
        mut self,
        terminal_event: Option<TerminalEventSummary>,
        terminal_kind: Option<TerminalKind>,
        disposition: CodexWsStepDisposition,
    ) {
        let execution_guard = self.execution_guard.take();
        let execution_release = async move {
            if let Some(execution_guard) = execution_guard {
                execution_guard.release().await;
            }
        };
        let candidate_release = self
            .runtime
            .release_candidate_scheduling_resources(self.candidate, self.first_dispatch);
        join_step_resource_releases(execution_release, candidate_release).await;
        self.record(terminal_event, terminal_kind, disposition);
    }

    fn record(
        &mut self,
        terminal_event: Option<TerminalEventSummary>,
        terminal_kind: Option<TerminalKind>,
        disposition: CodexWsStepDisposition,
    ) {
        let Some(usage_report) = self.usage_report.take() else {
            return;
        };
        drop(self.execution_guard.take());
        self.runtime.record_step_terminal(
            self.candidate,
            self.step,
            terminal_event,
            terminal_kind,
            disposition,
            self.first_dispatch,
            self.first_byte_elapsed,
            self.started_at.elapsed(),
            usage_report,
        );
    }
}

async fn join_step_resource_releases<A, B>(execution_release: A, candidate_release: B)
where
    A: Future<Output = ()>,
    B: Future<Output = ()>,
{
    tokio::join!(execution_release, candidate_release);
}

impl Drop for StepSettlementGuard<'_> {
    fn drop(&mut self) {
        self.record(
            None,
            None,
            CodexWsStepDisposition::Cancelled {
                error_type: "codex_ws_step_task_cancelled".to_string(),
                error_message: "Codex WS step task was cancelled".to_string(),
            },
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct StepDeadlines {
    write: Duration,
    read: Duration,
    first_byte: Duration,
    total_at: tokio::time::Instant,
}

impl StepDeadlines {
    fn new(started_at: tokio::time::Instant, timeouts: CodexWsTimeouts) -> Self {
        Self {
            write: timeouts.write,
            read: timeouts.read,
            first_byte: timeouts.first_byte,
            total_at: started_at + timeouts.total,
        }
    }

    fn write_deadline(&self) -> tokio::time::Instant {
        std::cmp::min(self.total_at, tokio::time::Instant::now() + self.write)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundedSendError {
    Peer,
    Timeout,
}

pub(crate) async fn run_codex_ws_session(
    mut client: Box<dyn RelayPeer>,
    runtime: &dyn CodexWsRuntimePort,
) {
    let first_text =
        match tokio::time::timeout(FIRST_FRAME_TIMEOUT, receive_first_text(&mut client)).await {
            Ok(Ok(Some(text))) => text,
            Ok(Ok(None)) => return,
            Ok(Err(error)) => {
                close_with_error(&mut client, error.message()).await;
                return;
            }
            Err(_) => {
                close_with_error(&mut client, "first response.create timed out").await;
                return;
            }
        };
    let first_step_started = tokio::time::Instant::now();
    let first_step =
        match parse_response_create_with_cpu_budget(first_text, OwnedResponseCreateContext::First)
            .await
        {
            Ok(step) => step,
            Err(error) => {
                close_with_error(&mut client, error.message()).await;
                return;
            }
        };
    if let Err(error) = runtime.validate_step(&first_step).await {
        send_not_executed_control(
            &mut client,
            &first_step,
            error.reason,
            error.middle_route_disposition,
        )
        .await;
        return;
    }

    let candidates = match runtime.select_candidates(&first_step).await {
        Ok(candidates) => candidates,
        Err(error) => {
            send_not_executed_control(
                &mut client,
                &first_step,
                error.reason,
                error.middle_route_disposition,
            )
            .await;
            return;
        }
    };
    let initial_connect_budget = initial_connect_budget(&candidates);
    let initial_connect_deadline = tokio::time::Instant::now() + initial_connect_budget;
    let mut remaining_candidates = RemainingCandidatesGuard::new(runtime, candidates);
    let mut step_usage = StepUsageLifecycleGuard::new(runtime, first_step_started);
    let mut connected = None;
    let mut last_connect_error = None;
    loop {
        if tokio::time::Instant::now() >= initial_connect_deadline {
            last_connect_error = Some(StepPreparationError::retain(
                "initial_connect_budget_exhausted",
            ));
            break;
        }
        let Some(candidate) = remaining_candidates.next() else {
            break;
        };
        step_usage.bind(&candidate, &first_step);
        let connect_result = match tokio::time::timeout_at(
            initial_connect_deadline,
            connect_candidate_while_client_open(&mut client, runtime, candidate),
        )
        .await
        {
            Ok(Some(connect_result)) => connect_result,
            Ok(None) => {
                step_usage.reject(
                    499,
                    "codex_ws_client_disconnected_while_connecting",
                    "client disconnected while the provider WebSocket was connecting",
                    true,
                );
                return;
            }
            Err(_) => {
                last_connect_error = Some(StepPreparationError::retain(
                    "initial_connect_budget_exhausted",
                ));
                break;
            }
        };
        match connect_result {
            Ok(value) => {
                connected = Some(value);
                break;
            }
            Err(error) => last_connect_error = Some(error),
        }
    }
    let Some(connected) = connected else {
        let (reason, middle_route_disposition) = runtime
            .validate_runtime_fences()
            .err()
            .map(|error| (error.reason, error.middle_route_disposition))
            .or_else(|| {
                last_connect_error.map(|error| (error.reason, error.middle_route_disposition))
            })
            .unwrap_or(("candidate_unavailable", MiddleRouteDisposition::Retain));
        step_usage.reject(
            http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            reason,
            "no official Codex WebSocket candidate could be connected",
            false,
        );
        send_not_executed_control(&mut client, &first_step, reason, middle_route_disposition).await;
        return;
    };
    remaining_candidates.finish();
    let ConnectedCandidate {
        mut candidate,
        mut peer,
        handshake_turn_state,
    } = connected;
    let mut binding = match BindingState::new(handshake_turn_state, &first_step) {
        Ok(binding) => binding,
        Err(error) => {
            step_usage.reject(
                http::StatusCode::BAD_REQUEST.as_u16(),
                "codex_ws_binding_invalid",
                "Codex WebSocket binding state was invalid",
                false,
            );
            runtime.abort_candidate(&candidate).await;
            close_with_error(&mut client, error.message()).await;
            best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
            return;
        }
    };
    let cleanup_permit = candidate.take_prewrite_cleanup_permit();
    let mut candidate_attempt = CandidateAttemptGuard::new(runtime, &candidate, cleanup_permit);
    if let Err(error) = runtime.validate_candidate_current_state(&candidate).await {
        step_usage.reject(
            http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            error.reason,
            "Codex WebSocket candidate changed before execution",
            false,
        );
        candidate_attempt.abort().await;
        send_not_executed_control(
            &mut client,
            &first_step,
            error.reason,
            error.middle_route_disposition,
        )
        .await;
        best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
        return;
    }

    let mut next_step = Some((first_step, true, step_usage));
    loop {
        let (mut step, already_validated, mut step_usage) = match next_step.take() {
            Some(step) => step,
            None => match receive_idle_step(&mut client, peer.as_mut(), &binding).await {
                Ok(Some((step, started_at))) => (
                    step,
                    false,
                    StepUsageLifecycleGuard::new(runtime, started_at),
                ),
                Ok(None) => {
                    candidate_attempt.abort().await;
                    best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                    return;
                }
                Err(error) => {
                    candidate_attempt.abort().await;
                    close_with_error(&mut client, error.message()).await;
                    best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                    return;
                }
            },
        };

        if !already_validated {
            if let Err(error) = binding.accept_step_fence(&step.fence) {
                candidate_attempt.abort().await;
                send_not_executed_control(
                    &mut client,
                    &step,
                    match error {
                        ProtocolError::Policy(reason) => reason,
                        _ => "codex_step_fence_invalid",
                    },
                    MiddleRouteDisposition::Retain,
                )
                .await;
                best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                return;
            }
            if let Err(error) = runtime.validate_step(&step).await {
                candidate_attempt.abort().await;
                send_not_executed_control(
                    &mut client,
                    &step,
                    error.reason,
                    error.middle_route_disposition,
                )
                .await;
                best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                return;
            }
        }
        if step.model != candidate.model {
            step_usage.reject(
                http::StatusCode::BAD_REQUEST.as_u16(),
                "bound_model_changed",
                "the model changed on a bound Codex WebSocket connection",
                false,
            );
            candidate_attempt.abort().await;
            send_not_executed_control(
                &mut client,
                &step,
                "bound_model_changed",
                MiddleRouteDisposition::Retain,
            )
            .await;
            best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
            return;
        }
        if !step
            .official_identity
            .matches_connection_binding(&candidate.identity)
        {
            step_usage.reject(
                http::StatusCode::BAD_REQUEST.as_u16(),
                "codex_identity_changed",
                "the Codex identity changed on a bound WebSocket connection",
                false,
            );
            candidate_attempt.abort().await;
            send_not_executed_control(
                &mut client,
                &step,
                "codex_identity_changed",
                MiddleRouteDisposition::Retain,
            )
            .await;
            best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
            return;
        }
        step_usage.bind(&candidate, &step);
        let prepared_step = match runtime.prepare_step(&candidate, &mut step).await {
            Ok(prepared_step) => prepared_step,
            Err(error) => {
                step_usage.reject(
                    http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                    error.reason,
                    "Codex WebSocket step preparation failed",
                    false,
                );
                candidate_attempt.abort().await;
                send_not_executed_control(
                    &mut client,
                    &step,
                    error.reason,
                    error.middle_route_disposition,
                )
                .await;
                best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                return;
            }
        };
        let (materialized_step, step_execution_guard, usage_report) = prepared_step.into_parts();
        let step_started = step_usage.started_at();
        let deadlines = StepDeadlines::new(step_started, candidate.timeouts());
        let provider_write_deadline = deadlines.write_deadline();
        if let Err(ready_error) = wait_until_ready(peer.as_mut(), provider_write_deadline).await {
            let reason = match ready_error {
                BoundedSendError::Timeout => "official_provider_not_ready_timeout",
                BoundedSendError::Peer => "official_provider_not_ready",
            };
            let status_code = match ready_error {
                BoundedSendError::Timeout => http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                BoundedSendError::Peer => http::StatusCode::BAD_GATEWAY.as_u16(),
            };
            step_usage.reject(
                status_code,
                reason,
                "official Codex WebSocket was not ready for the request",
                false,
            );
            step_execution_guard.release().await;
            drop(usage_report);
            candidate_attempt.abort().await;
            send_not_executed_control(&mut client, &step, reason, MiddleRouteDisposition::Retain)
                .await;
            best_effort_step_send(peer.as_mut(), RelayFrame::Close, &deadlines).await;
            return;
        }
        if let Err(error) = runtime.validate_candidate_current_state(&candidate).await {
            step_usage.reject(
                http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                error.reason,
                "Codex WebSocket candidate changed before provider execution",
                false,
            );
            step_execution_guard.release().await;
            drop(usage_report);
            candidate_attempt.abort().await;
            send_not_executed_control(
                &mut client,
                &step,
                error.reason,
                error.middle_route_disposition,
            )
            .await;
            best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
            return;
        }
        if tokio::time::Instant::now() >= provider_write_deadline {
            step_usage.reject(
                http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                "official_provider_write_budget_exhausted",
                "official Codex WebSocket write budget was exhausted",
                false,
            );
            step_execution_guard.release().await;
            drop(usage_report);
            candidate_attempt.abort().await;
            send_not_executed_control(
                &mut client,
                &step,
                "official_provider_write_budget_exhausted",
                MiddleRouteDisposition::Retain,
            )
            .await;
            best_effort_step_send(peer.as_mut(), RelayFrame::Close, &deadlines).await;
            return;
        }
        let execution_lease_status = step_execution_guard.lease_status();
        if !execution_lease_status.is_valid() {
            step_usage.reject(
                http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                "runtime_permit_lease_lost",
                "runtime concurrency permit was lost before provider execution",
                false,
            );
            step_execution_guard.release().await;
            drop(usage_report);
            candidate_attempt.abort().await;
            send_not_executed_control(
                &mut client,
                &step,
                "runtime_permit_lease_lost",
                MiddleRouteDisposition::Retain,
            )
            .await;
            best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
            return;
        }
        // This acquisition is synchronous so the authoritative shared-state
        // validation above remains adjacent to start_send without holding a
        // CPU worker over Redis I/O.
        let provider_write_cpu =
            match super::cpu_budget::try_acquire_large_frame_cpu_budget(materialized_step.len()) {
                Ok(permit) => permit,
                Err(()) => {
                    step_usage.reject(
                        http::StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                        "large_frame_cpu_unavailable",
                        "large-frame CPU capacity was unavailable before provider execution",
                        false,
                    );
                    step_execution_guard.release().await;
                    drop(usage_report);
                    candidate_attempt.abort().await;
                    send_not_executed_control(
                        &mut client,
                        &step,
                        "large_frame_cpu_unavailable",
                        MiddleRouteDisposition::Retain,
                    )
                    .await;
                    best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                    return;
                }
            };
        if tokio::time::Instant::now() >= provider_write_deadline {
            step_usage.reject(
                http::StatusCode::GATEWAY_TIMEOUT.as_u16(),
                "official_provider_write_budget_exhausted",
                "official Codex WebSocket write budget was exhausted",
                false,
            );
            drop(provider_write_cpu);
            step_execution_guard.release().await;
            drop(usage_report);
            candidate_attempt.abort().await;
            send_not_executed_control(
                &mut client,
                &step,
                "official_provider_write_budget_exhausted",
                MiddleRouteDisposition::Retain,
            )
            .await;
            best_effort_step_send(peer.as_mut(), RelayFrame::Close, &deadlines).await;
            return;
        }
        let first_dispatch = candidate_attempt.mark_provider_write_attempted();
        let mut settlement = StepSettlementGuard::new(
            runtime,
            &candidate,
            &step,
            step_started,
            step_execution_guard,
            usage_report,
            first_dispatch,
        );
        step_usage.disarm();
        let provider_write = send_ready_until_with_optional_cpu_budget(
            peer.as_mut(),
            RelayFrame::Text(materialized_step.into()),
            provider_write_deadline,
            provider_write_cpu,
        )
        .await;
        if let Err(write_error) = provider_write {
            let reason = match write_error {
                BoundedSendError::Timeout => "official_provider_write_timeout",
                BoundedSendError::Peer => "official_provider_write_failed",
            };
            settlement
                .finish(
                    None,
                    None,
                    CodexWsStepDisposition::Cancelled {
                        error_type: reason.to_string(),
                        error_message: "official provider write outcome is unknown".to_string(),
                    },
                )
                .await;
            send_execution_unknown_control(&mut client, &step, reason, Some(&deadlines)).await;
            best_effort_step_send(peer.as_mut(), RelayFrame::Close, &deadlines).await;
            return;
        }
        if let Err(error) = runtime.validate_runtime_fences() {
            settlement
                .finish(
                    None,
                    None,
                    CodexWsStepDisposition::Cancelled {
                        error_type: error.reason.to_string(),
                        error_message:
                            "runtime configuration changed while the official provider write was in flight"
                                .to_string(),
                    },
                )
                .await;
            send_execution_unknown_control(&mut client, &step, error.reason, Some(&deadlines))
                .await;
            best_effort_step_send(peer.as_mut(), RelayFrame::Close, &deadlines).await;
            return;
        }

        let outcome = relay_one_response(
            &mut client,
            peer.as_mut(),
            &mut binding,
            &step,
            runtime,
            &candidate.key_id,
            candidate.selected_catalog_epoch,
            &execution_lease_status,
            &deadlines,
            step_started,
            |first_byte_elapsed| settlement.record_stream_started(first_byte_elapsed),
        )
        .await;
        match outcome {
            StepOutcome::Completed {
                mut close_after_terminal,
                terminal_event,
                terminal_frame,
            } => {
                close_after_terminal |= !execution_lease_status.is_valid();
                settlement
                    .finish(
                        Some(terminal_event),
                        Some(TerminalKind::Completed),
                        CodexWsStepDisposition::Completed,
                    )
                    .await;
                if !deliver_terminal_after_settlement(
                    &mut client,
                    &step,
                    terminal_frame,
                    close_after_terminal,
                    &deadlines,
                )
                .await
                {
                    best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                    return;
                }
                if close_after_terminal {
                    best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                    best_effort_control_send(client.as_mut(), RelayFrame::Close).await;
                    return;
                }
            }
            StepOutcome::ClientClosed => {
                settlement
                    .finish(
                        None,
                        None,
                        CodexWsStepDisposition::Cancelled {
                            error_type: "codex_ws_client_disconnected".to_string(),
                            error_message:
                                "client disconnected while provider response was in flight"
                                    .to_string(),
                        },
                    )
                    .await;
                best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                return;
            }
            StepOutcome::Poisoned {
                terminal_event,
                terminal_kind,
                disposition,
                terminal_frame,
            } => {
                settlement
                    .finish(terminal_event, terminal_kind, disposition)
                    .await;
                if let Some(terminal_frame) = terminal_frame {
                    let _ = deliver_terminal_after_settlement(
                        &mut client,
                        &step,
                        terminal_frame,
                        false,
                        &deadlines,
                    )
                    .await;
                }
                best_effort_control_send(peer.as_mut(), RelayFrame::Close).await;
                return;
            }
        }
    }
}

fn initial_connect_budget(candidates: &[CodexWsCandidate]) -> Duration {
    candidates
        .iter()
        .map(|candidate| candidate.timeouts().connect)
        .fold(Duration::ZERO, Duration::saturating_add)
        .min(MAX_INITIAL_CONNECT_BUDGET)
        .max(Duration::from_millis(1))
}

async fn connect_candidate_while_client_open(
    client: &mut Box<dyn RelayPeer>,
    runtime: &dyn CodexWsRuntimePort,
    candidate: CodexWsCandidate,
) -> Option<Result<ConnectedCandidate, super::runtime::StepPreparationError>> {
    let connect = runtime.connect(candidate);
    tokio::pin!(connect);
    loop {
        tokio::select! {
            biased;
            client_frame = receive_peer(client.as_mut()) => {
                match client_frame {
                    Ok(Some(RelayFrame::Ping(bytes))) => {
                        if bounded_control_send(client.as_mut(), RelayFrame::Pong(bytes)).await.is_err() {
                            return None;
                        }
                    }
                    Ok(Some(RelayFrame::Pong(_))) => {}
                    Ok(Some(RelayFrame::Close)) | Ok(None) | Err(_) => return None,
                    Ok(Some(RelayFrame::Text(_))) | Ok(Some(RelayFrame::Binary(_))) => {
                        close_with_error(client, "response.create is already connecting").await;
                        return None;
                    }
                }
            }
            result = &mut connect => return Some(result),
        }
    }
}

struct BindingState {
    model: String,
    binding_epoch_id: String,
    binding_generation: u64,
    seen_step_correlations: SettledResponseHistory,
    last_completed_response_id: Option<String>,
    turn_state: Option<(String, String)>,
    settled_response_ids: SettledResponseHistory,
}

const SETTLED_RESPONSE_HISTORY_CAPACITY: usize = 64;
const SETTLED_RESPONSE_HISTORY_BYTE_CAPACITY: usize = 8 * 1024;

struct SettledResponseHistory {
    ids: HashSet<Arc<str>>,
    insertion_order: VecDeque<Arc<str>>,
    total_bytes: usize,
}

impl SettledResponseHistory {
    fn new() -> Self {
        Self {
            ids: HashSet::new(),
            insertion_order: VecDeque::new(),
            total_bytes: 0,
        }
    }

    fn contains(&self, response_id: &str) -> bool {
        self.ids.contains(response_id)
    }

    fn insert(&mut self, response_id: String) {
        let response_id = Arc::<str>::from(response_id);
        if !self.ids.insert(Arc::clone(&response_id)) {
            return;
        }
        self.total_bytes = self.total_bytes.saturating_add(response_id.len());
        self.insertion_order.push_back(response_id);
        while self.insertion_order.len() > SETTLED_RESPONSE_HISTORY_CAPACITY
            || self.total_bytes > SETTLED_RESPONSE_HISTORY_BYTE_CAPACITY
        {
            if let Some(expired) = self.insertion_order.pop_front() {
                if self.ids.remove(&expired) {
                    self.total_bytes = self.total_bytes.saturating_sub(expired.len());
                }
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.ids.len()
    }

    #[cfg(test)]
    fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

impl BindingState {
    fn new(
        handshake_turn_state: Option<String>,
        first_step: &ResponseCreateStep,
    ) -> Result<Self, ProtocolError> {
        let handshake_turn_state = handshake_turn_state
            .map(|state| validate_official_turn_state(&state))
            .transpose()?;
        let turn_state = handshake_turn_state
            .zip(first_step.logical_turn_id.as_ref())
            .map(|(state, turn_id)| (turn_id.clone(), state));
        Ok(Self {
            model: first_step.model.clone(),
            binding_epoch_id: first_step.fence.binding_epoch_id.clone(),
            binding_generation: first_step.fence.binding_generation,
            seen_step_correlations: {
                let mut seen = SettledResponseHistory::new();
                seen.insert(first_step.fence.correlation_id.clone());
                seen
            },
            last_completed_response_id: None,
            turn_state,
            settled_response_ids: SettledResponseHistory::new(),
        })
    }

    fn accept_step_fence(
        &mut self,
        fence: &super::protocol::StepFence,
    ) -> Result<(), ProtocolError> {
        if fence.binding_epoch_id != self.binding_epoch_id
            || fence.binding_generation != self.binding_generation
        {
            return Err(ProtocolError::Policy(
                "sub2api binding epoch changed on a bound connection",
            ));
        }
        if self.seen_step_correlations.contains(&fence.correlation_id) {
            return Err(ProtocolError::Policy(
                "sub2api step correlation was replayed on a bound connection",
            ));
        }
        self.seen_step_correlations
            .insert(fence.correlation_id.clone());
        Ok(())
    }
}

async fn send_until(
    peer: &mut dyn RelayPeer,
    frame: RelayFrame,
    deadline: tokio::time::Instant,
) -> Result<(), BoundedSendError> {
    wait_until_ready(peer, deadline).await?;
    send_ready_until_with_optional_cpu_budget(peer, frame, deadline, None).await
}

async fn wait_until_ready(
    peer: &mut dyn RelayPeer,
    deadline: tokio::time::Instant,
) -> Result<(), BoundedSendError> {
    if tokio::time::Instant::now() >= deadline {
        return Err(BoundedSendError::Timeout);
    }
    let ready = std::future::poll_fn(|context| Pin::new(&mut *peer).poll_ready(context));
    match tokio::time::timeout_at(deadline, ready).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(BoundedSendError::Peer),
        Err(_) => Err(BoundedSendError::Timeout),
    }
}

async fn send_ready_until_with_optional_cpu_budget(
    peer: &mut dyn RelayPeer,
    frame: RelayFrame,
    deadline: tokio::time::Instant,
    cpu: Option<super::cpu_budget::LargeFrameCpuPermit>,
) -> Result<(), BoundedSendError> {
    if tokio::time::Instant::now() >= deadline {
        return Err(BoundedSendError::Timeout);
    }
    let start_result = Pin::new(&mut *peer)
        .start_send(frame)
        .map_err(|_| BoundedSendError::Peer);
    // Compression and frame serialization happen synchronously in
    // start_send. Socket readiness was awaited before acquiring the permit,
    // and flushing happens after releasing it.
    drop(cpu);
    start_result?;
    let flush = std::future::poll_fn(|context| Pin::new(&mut *peer).poll_flush(context));
    match tokio::time::timeout_at(deadline, flush).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(BoundedSendError::Peer),
        Err(_) => Err(BoundedSendError::Timeout),
    }
}

async fn receive_peer(
    peer: &mut dyn RelayPeer,
) -> Result<Option<RelayFrame>, super::runtime::PeerError> {
    peer.next().await.transpose()
}

async fn send_step_frame(
    peer: &mut dyn RelayPeer,
    frame: RelayFrame,
    deadlines: &StepDeadlines,
) -> Result<(), BoundedSendError> {
    send_until(peer, frame, deadlines.write_deadline()).await
}

fn relay_frame_payload_len(frame: &RelayFrame) -> usize {
    match frame {
        RelayFrame::Text(bytes)
        | RelayFrame::Binary(bytes)
        | RelayFrame::Ping(bytes)
        | RelayFrame::Pong(bytes) => bytes.len(),
        RelayFrame::Close => 0,
    }
}

async fn send_client_step_data_frame(
    client: &mut dyn RelayPeer,
    frame: RelayFrame,
    deadlines: &StepDeadlines,
) -> Result<(), BoundedSendError> {
    let deadline = deadlines.write_deadline();
    wait_until_ready(client, deadline).await?;
    let cpu = super::cpu_budget::acquire_large_frame_cpu_budget(relay_frame_payload_len(&frame))
        .await
        .map_err(|_| BoundedSendError::Peer)?;
    send_ready_until_with_optional_cpu_budget(client, frame, deadline, cpu).await
}

async fn send_client_data_until(
    client: &mut dyn RelayPeer,
    frame: RelayFrame,
    deadline: tokio::time::Instant,
) -> Result<(), BoundedSendError> {
    wait_until_ready(client, deadline).await?;
    let cpu = super::cpu_budget::acquire_large_frame_cpu_budget_until(
        relay_frame_payload_len(&frame),
        deadline,
    )
    .await
    .map_err(|_| BoundedSendError::Timeout)?;
    send_ready_until_with_optional_cpu_budget(client, frame, deadline, cpu).await
}

async fn best_effort_step_send(
    peer: &mut dyn RelayPeer,
    frame: RelayFrame,
    deadlines: &StepDeadlines,
) {
    let _ = send_step_frame(peer, frame, deadlines).await;
}

async fn bounded_control_send(
    peer: &mut dyn RelayPeer,
    frame: RelayFrame,
) -> Result<(), BoundedSendError> {
    send_until(
        peer,
        frame,
        tokio::time::Instant::now() + CONTROL_SEND_TIMEOUT,
    )
    .await
}

async fn best_effort_control_send(peer: &mut dyn RelayPeer, frame: RelayFrame) {
    let _ = bounded_control_send(peer, frame).await;
}

async fn receive_first_text(
    client: &mut Box<dyn RelayPeer>,
) -> Result<Option<Bytes>, ProtocolError> {
    loop {
        match receive_peer(client.as_mut())
            .await
            .map_err(|_| ProtocolError::Policy("downstream WebSocket receive failed"))?
        {
            Some(RelayFrame::Text(text)) => return Ok(Some(text)),
            Some(RelayFrame::Ping(bytes)) => {
                bounded_control_send(client.as_mut(), RelayFrame::Pong(bytes))
                    .await
                    .map_err(|_| ProtocolError::Policy("downstream WebSocket send failed"))?;
            }
            Some(RelayFrame::Pong(_)) => {}
            Some(RelayFrame::Binary(_)) => {
                return Err(ProtocolError::Policy(
                    "binary response.create frames are unsupported",
                ))
            }
            Some(RelayFrame::Close) | None => return Ok(None),
        }
    }
}

async fn receive_idle_step(
    client: &mut Box<dyn RelayPeer>,
    official: &mut dyn RelayPeer,
    binding: &BindingState,
) -> Result<Option<(ResponseCreateStep, tokio::time::Instant)>, ProtocolError> {
    loop {
        tokio::select! {
            biased;
            official_frame = receive_peer(official) => {
                let frame = official_frame
                    .map_err(|_| ProtocolError::Upstream("official connection failed while idle"))?;
                match frame {
                    Some(RelayFrame::Ping(bytes)) => {
                        bounded_control_send(official, RelayFrame::Pong(bytes))
                            .await
                            .map_err(|_| ProtocolError::Upstream("official WebSocket pong failed"))?;
                    }
                    Some(RelayFrame::Pong(_)) => {}
                    Some(RelayFrame::Text(text)) => {
                        if !official_text_frame_within_public_limit(&text) {
                            return Err(ProtocolError::Upstream(
                                "official Codex frame exceeds the public relay limit",
                            ));
                        }
                        let classification = classify_server_event_with_cpu_budget(&text).await?;
                        let response_id = classification
                            .terminal_response_id
                            .as_deref()
                            .or(classification.created_response_id.as_deref())
                            .or(classification.provenance_response_id.as_deref());
                        if response_id.is_some_and(|id| binding.settled_response_ids.contains(id)) {
                            continue;
                        }
                        if classification.recognized_business || classification.terminal.is_some() {
                            return Err(ProtocolError::Upstream(
                                "official Codex emitted an unexpected idle business frame",
                            ));
                        }
                    }
                    Some(RelayFrame::Binary(_)) => {
                        return Err(ProtocolError::Upstream(
                            "official Codex emitted an idle binary frame",
                        ));
                    }
                    Some(RelayFrame::Close) | None => {
                        return Err(ProtocolError::Upstream(
                            "official connection closed while idle",
                        ));
                    }
                }
            }
            client_frame = receive_peer(client.as_mut()) => {
                let frame = client_frame
                    .map_err(|_| ProtocolError::Policy("downstream WebSocket receive failed"))?;
                match frame {
                    Some(RelayFrame::Text(text)) => {
                        let started_at = tokio::time::Instant::now();
                        let bound_turn_state = binding
                            .turn_state
                            .as_ref()
                            .map(|(turn_id, state)| (turn_id.clone(), state.clone()));
                        return parse_response_create_with_cpu_budget(
                            text,
                            OwnedResponseCreateContext::Bound {
                                model: binding.model.clone(),
                                expected_previous_response_id: binding.last_completed_response_id.clone(),
                                turn_state: bound_turn_state,
                            },
                        )
                        .await
                        .map(|step| Some((step, started_at)));
                    }
                    Some(RelayFrame::Ping(bytes)) => {
                        bounded_control_send(client.as_mut(), RelayFrame::Pong(bytes))
                            .await
                            .map_err(|_| ProtocolError::Policy("downstream WebSocket send failed"))?;
                    }
                    Some(RelayFrame::Pong(_)) => {}
                    Some(RelayFrame::Binary(_)) => {
                        return Err(ProtocolError::Policy(
                            "binary response.create frames are unsupported",
                        ));
                    }
                    Some(RelayFrame::Close) | None => return Ok(None),
                }
            }
        }
    }
}

enum StepOutcome {
    Completed {
        close_after_terminal: bool,
        terminal_event: TerminalEventSummary,
        terminal_frame: Bytes,
    },
    ClientClosed,
    Poisoned {
        terminal_event: Option<TerminalEventSummary>,
        terminal_kind: Option<TerminalKind>,
        disposition: CodexWsStepDisposition,
        terminal_frame: Option<Bytes>,
    },
}

impl StepOutcome {
    fn poisoned() -> Self {
        Self::Poisoned {
            terminal_event: None,
            terminal_kind: None,
            disposition: CodexWsStepDisposition::ProviderFailure {
                status_code: http::StatusCode::BAD_GATEWAY.as_u16(),
                error_type: "codex_ws_official_protocol_error".to_string(),
                error_message: "official Codex WebSocket protocol failed".to_string(),
                error_body: None,
            },
            terminal_frame: None,
        }
    }

    fn stream_timeout(error_type: &'static str, error_message: &'static str) -> Self {
        Self::Poisoned {
            terminal_event: None,
            terminal_kind: None,
            disposition: CodexWsStepDisposition::StreamTimeout {
                error_type: error_type.to_string(),
                error_message: error_message.to_string(),
            },
            terminal_frame: None,
        }
    }

    fn cancelled(error_type: &'static str, error_message: &'static str) -> Self {
        Self::Poisoned {
            terminal_event: None,
            terminal_kind: None,
            disposition: CodexWsStepDisposition::Cancelled {
                error_type: error_type.to_string(),
                error_message: error_message.to_string(),
            },
            terminal_frame: None,
        }
    }
}

fn terminal_outcome(
    kind: TerminalKind,
    terminal_event: TerminalEventSummary,
    close_after_terminal: bool,
    terminal_frame: Bytes,
) -> StepOutcome {
    if kind == TerminalKind::Completed {
        StepOutcome::Completed {
            close_after_terminal,
            terminal_event,
            terminal_frame,
        }
    } else {
        let status_code = terminal_event
            .provider_status_code
            .unwrap_or(http::StatusCode::BAD_GATEWAY.as_u16());
        let error_type = terminal_event
            .provider_error_code
            .as_deref()
            .map(|code| format!("codex_ws_official_{code}"))
            .unwrap_or_else(|| format!("codex_ws_official_{}", terminal_kind_label(kind)));
        let error_message = terminal_event
            .provider_error_message
            .clone()
            .unwrap_or_else(|| {
                format!(
                    "official Codex response ended as {}",
                    terminal_kind_label(kind)
                )
            });
        let error_body = terminal_event.provider_error_body.clone();
        StepOutcome::Poisoned {
            terminal_event: Some(terminal_event),
            terminal_kind: Some(kind),
            disposition: CodexWsStepDisposition::ProviderFailure {
                status_code,
                error_type,
                error_message,
                error_body,
            },
            terminal_frame: Some(terminal_frame),
        }
    }
}

fn terminal_kind_label(kind: TerminalKind) -> &'static str {
    match kind {
        TerminalKind::Completed => "completed",
        TerminalKind::Failed => "failed",
        TerminalKind::Incomplete => "incomplete",
        TerminalKind::Error => "error",
    }
}

async fn relay_one_response<F>(
    client: &mut Box<dyn RelayPeer>,
    official: &mut dyn RelayPeer,
    binding: &mut BindingState,
    step: &ResponseCreateStep,
    runtime: &dyn CodexWsRuntimePort,
    key_id: &str,
    selected_catalog_epoch: u64,
    execution_lease_status: &StepExecutionLeaseStatus,
    deadlines: &StepDeadlines,
    step_started: tokio::time::Instant,
    mut on_first_business_frame: F,
) -> StepOutcome
where
    F: FnMut(Duration),
{
    let mut active_response_id = None::<String>;
    let mut received_business_frame = false;
    let mut upstream_deadline = tokio::time::Instant::now() + deadlines.first_byte;
    let upstream_timer =
        tokio::time::sleep_until(std::cmp::min(upstream_deadline, deadlines.total_at));
    tokio::pin!(upstream_timer);
    let execution_lease_lost = execution_lease_status.lost();
    tokio::pin!(execution_lease_lost);
    loop {
        upstream_timer
            .as_mut()
            .reset(std::cmp::min(upstream_deadline, deadlines.total_at));
        tokio::select! {
            biased;
            _ = &mut execution_lease_lost => {
                close_with_error_step(
                    client,
                    "runtime concurrency permit lease was lost",
                    deadlines,
                )
                .await;
                return StepOutcome::cancelled(
                    "runtime_permit_lease_lost",
                    "runtime concurrency permit lease was lost while the provider response was in flight",
                );
            }
            _ = &mut upstream_timer => {
                let (error_type, message) = if deadlines.total_at <= upstream_deadline {
                    ("codex_ws_total_timeout", "Codex WebSocket step total timed out")
                } else if received_business_frame {
                    ("codex_ws_read_timeout", "official Codex upstream idle read timed out")
                } else {
                    ("codex_ws_first_byte_timeout", "official Codex first business frame timed out")
                };
                close_with_timeout_error(client, message).await;
                return StepOutcome::stream_timeout(error_type, message);
            }
            client_frame = receive_peer(client.as_mut()) => {
                match client_frame {
                    Ok(Some(RelayFrame::Ping(bytes))) => {
                        if send_step_frame(client.as_mut(), RelayFrame::Pong(bytes), deadlines).await.is_err() {
                            return StepOutcome::ClientClosed;
                        }
                    }
                    Ok(Some(RelayFrame::Pong(_))) => {}
                    Ok(Some(RelayFrame::Close)) | Ok(None) | Err(_) => {
                        return StepOutcome::ClientClosed;
                    }
                    Ok(Some(RelayFrame::Text(_))) | Ok(Some(RelayFrame::Binary(_))) => {
                        close_with_error_step(client, "one response.create is already in flight", deadlines).await;
                        return StepOutcome::cancelled(
                            "codex_ws_client_inflight_violation",
                            "client sent another request while a response was in flight",
                        );
                    }
                }
            }
            official_frame = receive_peer(official) => {
                let frame = match official_frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => {
                        close_with_error_step(client, "official connection ended before a terminal event", deadlines).await;
                        return StepOutcome::poisoned();
                    }
                };
                match frame {
                    RelayFrame::Text(text) => {
                        if !official_text_frame_within_public_limit(&text) {
                            close_with_error_step(
                                client,
                                "official Codex frame exceeds the public relay limit",
                                deadlines,
                            )
                            .await;
                            return StepOutcome::poisoned();
                        }
                        let classification = match classify_server_event_with_cpu_budget(&text).await {
                            Ok(classification) => classification,
                            Err(error) => {
                                close_with_error_step(client, error.message(), deadlines).await;
                                return StepOutcome::poisoned();
                            }
                        };
                        let super::protocol::ServerEventClassification {
                            recognized_business,
                            created,
                            terminal,
                            provenance_response_id,
                            created_response_id,
                            terminal_response_id,
                            turn_state,
                            provider_headers,
                            terminal_event,
                        } = classification;
                        if !provider_headers.is_empty() {
                            runtime.record_codex_quota_headers(key_id, provider_headers);
                        }
                        if created {
                            let Some(response_id) = created_response_id else {
                                close_with_error_step(client, "response.created omitted response.id", deadlines).await;
                                return StepOutcome::poisoned();
                            };
                            if binding.settled_response_ids.contains(&response_id) {
                                continue;
                            }
                            if let Some(active) = active_response_id.as_deref() {
                                if active == response_id {
                                    continue;
                                }
                                close_with_error_step(client, "response.created provenance mismatch", deadlines).await;
                                return StepOutcome::poisoned();
                            }
                            active_response_id = Some(response_id);
                        }
                        if !created {
                            if let Some(response_id) = provenance_response_id.as_deref() {
                                if binding.settled_response_ids.contains(response_id) {
                                    continue;
                                }
                                if active_response_id.as_deref() != Some(response_id) {
                                    close_with_error_step(client, "official event provenance mismatch", deadlines).await;
                                    return StepOutcome::poisoned();
                                }
                            }
                        }
                        if let Some(kind) = terminal {
                            if terminal_response_id.as_ref().is_some_and(|response_id| {
                                binding.settled_response_ids.contains(response_id)
                            }) {
                                continue;
                            }
                            if kind == TerminalKind::Completed {
                                let Some(response_id) = terminal_response_id.as_ref() else {
                                    close_with_error_step(client, "response.completed omitted response.id", deadlines).await;
                                    return StepOutcome::poisoned();
                                };
                                if active_response_id.as_deref() != Some(response_id.as_str())
                                {
                                    close_with_error_step(client, "response terminal provenance mismatch", deadlines).await;
                                    return StepOutcome::poisoned();
                                }
                            } else if terminal_response_id.as_ref().is_some_and(|response_id| {
                                active_response_id.as_ref().is_none_or(|active| active != response_id)
                            }) {
                                close_with_error_step(client, "response terminal provenance mismatch", deadlines).await;
                                return StepOutcome::poisoned();
                            }
                            if let Some(response_id) = terminal_response_id.as_ref() {
                                binding.settled_response_ids.insert(response_id.clone());
                            }
                        }
                        if recognized_business && !received_business_frame {
                            on_first_business_frame(step_started.elapsed());
                        }
                        if recognized_business {
                            received_business_frame = true;
                            upstream_deadline = tokio::time::Instant::now() + deadlines.read;
                        }
                        if let Some(turn_state) = turn_state {
                            if let Some(turn_id) = step.logical_turn_id.as_ref() {
                                binding.turn_state = Some((turn_id.clone(), turn_state));
                            }
                        }
                        let close_after_terminal = terminal == Some(TerminalKind::Completed)
                            && (runtime.catalog_epoch() != selected_catalog_epoch
                                || runtime.validate_runtime_fences().is_err());
                        if terminal == Some(TerminalKind::Completed) {
                            let response_id = terminal_response_id
                                .clone()
                                .expect("completed response id was validated");
                            binding.last_completed_response_id = Some(response_id);
                        }
                        if let Some((kind, event)) = terminal.zip(terminal_event) {
                            return terminal_outcome(kind, event, close_after_terminal, text);
                        }
                        if send_client_step_data_frame(
                            client.as_mut(),
                            RelayFrame::Text(text),
                            deadlines,
                        )
                            .await
                            .is_err()
                        {
                            return StepOutcome::ClientClosed;
                        }
                    }
                    RelayFrame::Ping(bytes) => {
                        if send_step_frame(official, RelayFrame::Pong(bytes), deadlines).await.is_err() {
                            close_with_error_step(client, "official WebSocket pong failed", deadlines).await;
                            return StepOutcome::poisoned();
                        }
                    }
                    RelayFrame::Pong(_) => {}
                    RelayFrame::Binary(_) => {
                        close_with_error_step(client, "official Codex emitted a binary business frame", deadlines).await;
                        return StepOutcome::poisoned();
                    }
                    RelayFrame::Close => {
                        close_with_error_step(client, "official connection closed before a terminal event", deadlines).await;
                        return StepOutcome::poisoned();
                    }
                }
            }
        }
    }
}

async fn deliver_terminal_after_settlement(
    client: &mut Box<dyn RelayPeer>,
    step: &ResponseCreateStep,
    terminal_frame: Bytes,
    close_after_terminal: bool,
    deadlines: &StepDeadlines,
) -> bool {
    let deadline = terminal_delivery_deadline(deadlines);
    if send_client_data_until(client.as_mut(), RelayFrame::Text(terminal_frame), deadline)
        .await
        .is_err()
    {
        return false;
    }
    if close_after_terminal {
        let control = route_control_event(
            RouteControlAction::CloseAfterTerminal,
            None,
            "account_soft_drained",
            &step.fence,
            "terminal",
            "confirmed",
            "terminal",
            false,
        );
        if send_until(client.as_mut(), RelayFrame::Text(control.into()), deadline)
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

fn terminal_delivery_deadline(deadlines: &StepDeadlines) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    let configured = deadlines
        .write
        .clamp(TERMINAL_DELIVERY_MIN_GRACE, TERMINAL_DELIVERY_MAX_GRACE);
    let remaining_total = deadlines.total_at.saturating_duration_since(now);
    let budget = if remaining_total < TERMINAL_DELIVERY_MIN_GRACE {
        TERMINAL_DELIVERY_MIN_GRACE
    } else {
        configured.min(remaining_total)
    };
    now + budget
}

async fn send_not_executed_control(
    client: &mut Box<dyn RelayPeer>,
    step: &ResponseCreateStep,
    reason: &'static str,
    middle_route_disposition: MiddleRouteDisposition,
) {
    let control = route_control_event(
        RouteControlAction::ClientReconnect,
        Some(middle_route_disposition),
        reason,
        &step.fence,
        "rejected_before_execution",
        "not_started",
        "proven_not_executed",
        true,
    );
    best_effort_control_send(client.as_mut(), RelayFrame::Text(control.into())).await;
    best_effort_control_send(client.as_mut(), RelayFrame::Close).await;
}

async fn send_execution_unknown_control(
    client: &mut Box<dyn RelayPeer>,
    step: &ResponseCreateStep,
    reason: &'static str,
    deadlines: Option<&StepDeadlines>,
) {
    let control = route_control_event(
        RouteControlAction::ClientReconnect,
        Some(MiddleRouteDisposition::Retain),
        reason,
        &step.fence,
        "provider_write_attempted",
        "unknown",
        "unknown",
        false,
    );
    if let Some(deadlines) = deadlines {
        best_effort_step_send(client.as_mut(), RelayFrame::Text(control.into()), deadlines).await;
        best_effort_step_send(client.as_mut(), RelayFrame::Close, deadlines).await;
    } else {
        best_effort_control_send(client.as_mut(), RelayFrame::Text(control.into())).await;
        best_effort_control_send(client.as_mut(), RelayFrame::Close).await;
    }
}

async fn close_with_error(client: &mut Box<dyn RelayPeer>, message: &'static str) {
    let event = protocol_error_event(message);
    best_effort_control_send(client.as_mut(), RelayFrame::Text(event.into())).await;
    best_effort_control_send(client.as_mut(), RelayFrame::Close).await;
}

async fn close_with_error_step(
    client: &mut Box<dyn RelayPeer>,
    message: &'static str,
    deadlines: &StepDeadlines,
) {
    let event = protocol_error_event(message);
    best_effort_step_send(client.as_mut(), RelayFrame::Text(event.into()), deadlines).await;
    best_effort_step_send(client.as_mut(), RelayFrame::Close, deadlines).await;
}

async fn close_with_timeout_error(client: &mut Box<dyn RelayPeer>, message: &'static str) {
    let deadline = tokio::time::Instant::now() + TIMEOUT_CLOSE_GRACE;
    let event = protocol_error_event(message);
    let _ = send_until(client.as_mut(), RelayFrame::Text(event.into()), deadline).await;
    let _ = send_until(client.as_mut(), RelayFrame::Close, deadline).await;
}

fn protocol_error_event(message: &'static str) -> String {
    serde_json::json!({
        "type": "error",
        "status": 400,
        "error": {
            "type": "invalid_request_error",
            "code": "aether_codex_ws_protocol_error",
            "message": message,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use aether_codex_ws_connector::OutboundRoute;
    use aether_runtime::ConcurrencyGate;
    use async_trait::async_trait;
    use futures_util::{Sink, Stream};
    use serde_json::json;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn step_resource_releases_wait_for_the_slowest_side_not_the_sum() {
        let started_at = tokio::time::Instant::now();

        join_step_resource_releases(
            tokio::time::sleep(Duration::from_millis(80)),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await;

        assert_eq!(started_at.elapsed(), Duration::from_millis(100));
    }
    use crate::codex_ws::runtime::{
        CodexWsCandidate, PeerError, PreparedStep, StepPreparationError, UsageReportReservation,
    };
    use crate::codex_ws::CodexWsCandidateLifecycle;

    struct ScriptedPeer {
        incoming: VecDeque<(Duration, RelayFrame)>,
        sent: Arc<Mutex<Vec<RelayFrame>>>,
        send_delay: Duration,
        flush_delay: Duration,
        send_started: Option<Arc<AtomicUsize>>,
        receive_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
        send_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
        flush_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
        send_ready: bool,
    }

    impl ScriptedPeer {
        fn new(
            incoming: impl IntoIterator<Item = (Duration, RelayFrame)>,
        ) -> (Self, Arc<Mutex<Vec<RelayFrame>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    incoming: incoming.into_iter().collect(),
                    sent: Arc::clone(&sent),
                    send_delay: Duration::ZERO,
                    flush_delay: Duration::ZERO,
                    send_started: None,
                    receive_sleep: None,
                    send_sleep: None,
                    flush_sleep: None,
                    send_ready: false,
                },
                sent,
            )
        }

        fn with_send_delay(mut self, send_delay: Duration) -> Self {
            self.send_delay = send_delay;
            self
        }

        fn with_flush_delay(mut self, flush_delay: Duration) -> Self {
            self.flush_delay = flush_delay;
            self
        }

        fn with_send_started_counter(mut self, send_started: Arc<AtomicUsize>) -> Self {
            self.send_started = Some(send_started);
            self
        }
    }

    impl Stream for ScriptedPeer {
        type Item = Result<RelayFrame, PeerError>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            let Some((delay, _)) = self.incoming.front() else {
                return Poll::Pending;
            };
            if self.receive_sleep.is_none() {
                self.receive_sleep = Some(Box::pin(tokio::time::sleep(*delay)));
            }
            let sleep = self.receive_sleep.as_mut().expect("receive sleep");
            if sleep.as_mut().poll(context).is_pending() {
                return Poll::Pending;
            }
            self.receive_sleep = None;
            let (_, frame) = self
                .incoming
                .pop_front()
                .expect("front scripted frame should remain until its delay completes");
            Poll::Ready(Some(Ok(frame)))
        }
    }

    impl Sink<RelayFrame> for ScriptedPeer {
        type Error = PeerError;

        fn poll_ready(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            if self.send_ready {
                return Poll::Ready(Ok(()));
            }
            if self.send_sleep.is_none() {
                if let Some(send_started) = self.send_started.as_ref() {
                    send_started.fetch_add(1, Ordering::Release);
                }
                self.send_sleep = Some(Box::pin(tokio::time::sleep(self.send_delay)));
            }
            let sleep = self.send_sleep.as_mut().expect("send sleep");
            if sleep.as_mut().poll(context).is_pending() {
                return Poll::Pending;
            }
            self.send_sleep = None;
            self.send_ready = true;
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, frame: RelayFrame) -> Result<(), Self::Error> {
            if !self.send_ready {
                return Err(PeerError("scripted peer was not ready".into()));
            }
            self.send_ready = false;
            self.sent
                .lock()
                .expect("sent frames should lock")
                .push(frame);
            Ok(())
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            if self.flush_sleep.is_none() {
                self.flush_sleep = Some(Box::pin(tokio::time::sleep(self.flush_delay)));
            }
            let sleep = self.flush_sleep.as_mut().expect("flush sleep");
            if sleep.as_mut().poll(context).is_pending() {
                return Poll::Pending;
            }
            self.flush_sleep = None;
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    struct InvalidateFenceAfterSendPeer {
        inner: Box<dyn RelayPeer>,
        runtime_fences_valid: Arc<AtomicBool>,
    }

    impl Stream for InvalidateFenceAfterSendPeer {
        type Item = Result<RelayFrame, PeerError>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            Pin::new(self.inner.as_mut()).poll_next(context)
        }
    }

    impl Sink<RelayFrame> for InvalidateFenceAfterSendPeer {
        type Error = PeerError;

        fn poll_ready(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(self.inner.as_mut()).poll_ready(context)
        }

        fn start_send(mut self: Pin<&mut Self>, frame: RelayFrame) -> Result<(), Self::Error> {
            Pin::new(self.inner.as_mut()).start_send(frame)?;
            self.runtime_fences_valid.store(false, Ordering::Release);
            Ok(())
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(self.inner.as_mut()).poll_flush(context)
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(self.inner.as_mut()).poll_close(context)
        }
    }

    struct TestRuntime {
        official: Mutex<Option<Box<dyn RelayPeer>>>,
        fail_first_connect: bool,
        connect_delays: Mutex<VecDeque<Duration>>,
        prepare_delay: Mutex<Option<Duration>>,
        timeouts: Mutex<Option<aether_contracts::ExecutionTimeouts>>,
        gate: ConcurrencyGate,
        validate_calls: AtomicUsize,
        connect_calls: AtomicUsize,
        prepare_calls: AtomicUsize,
        pending_calls: AtomicUsize,
        rejected_calls: AtomicUsize,
        rejected_reasons: Mutex<Vec<&'static str>>,
        stream_started_calls: AtomicUsize,
        stream_first_byte_ms: Mutex<Vec<u64>>,
        report_calls: AtomicUsize,
        report_after_release: AtomicUsize,
        report_terminal_kinds: Mutex<Vec<Option<TerminalKind>>>,
        report_first_byte_ms: Mutex<Vec<Option<u64>>>,
        started_candidates: Mutex<Vec<String>>,
        aborted_candidates: Mutex<Vec<String>>,
        unused_candidates: Mutex<Vec<String>>,
        epoch: AtomicUsize,
        bump_epoch_during_prepare: AtomicBool,
        runtime_fences_valid: Arc<AtomicBool>,
        invalidate_fences_during_connect: AtomicBool,
        invalidate_fences_during_prepare: AtomicBool,
    }

    impl TestRuntime {
        fn new(official: Box<dyn RelayPeer>, fail_first_connect: bool) -> Self {
            Self::with_runtime_fences(
                official,
                fail_first_connect,
                Arc::new(AtomicBool::new(true)),
            )
        }

        fn with_runtime_fences(
            official: Box<dyn RelayPeer>,
            fail_first_connect: bool,
            runtime_fences_valid: Arc<AtomicBool>,
        ) -> Self {
            Self {
                official: Mutex::new(Some(official)),
                fail_first_connect,
                connect_delays: Mutex::new(VecDeque::new()),
                prepare_delay: Mutex::new(None),
                timeouts: Mutex::new(None),
                gate: ConcurrencyGate::new("codex-ws-test", 1),
                validate_calls: AtomicUsize::new(0),
                connect_calls: AtomicUsize::new(0),
                prepare_calls: AtomicUsize::new(0),
                pending_calls: AtomicUsize::new(0),
                rejected_calls: AtomicUsize::new(0),
                rejected_reasons: Mutex::new(Vec::new()),
                stream_started_calls: AtomicUsize::new(0),
                stream_first_byte_ms: Mutex::new(Vec::new()),
                report_calls: AtomicUsize::new(0),
                report_after_release: AtomicUsize::new(0),
                report_terminal_kinds: Mutex::new(Vec::new()),
                report_first_byte_ms: Mutex::new(Vec::new()),
                started_candidates: Mutex::new(Vec::new()),
                aborted_candidates: Mutex::new(Vec::new()),
                unused_candidates: Mutex::new(Vec::new()),
                epoch: AtomicUsize::new(7),
                bump_epoch_during_prepare: AtomicBool::new(false),
                runtime_fences_valid,
                invalidate_fences_during_connect: AtomicBool::new(false),
                invalidate_fences_during_prepare: AtomicBool::new(false),
            }
        }

        fn set_timeouts(&self, timeouts: aether_contracts::ExecutionTimeouts) {
            *self.timeouts.lock().expect("timeouts should lock") = Some(timeouts);
        }

        fn push_connect_delay(&self, delay: Duration) {
            self.connect_delays
                .lock()
                .expect("connect delays should lock")
                .push_back(delay);
        }

        fn set_prepare_delay(&self, delay: Duration) {
            *self
                .prepare_delay
                .lock()
                .expect("prepare delay should lock") = Some(delay);
        }

        fn configured_candidate(
            &self,
            first_step: &ResponseCreateStep,
            provider_id: &str,
        ) -> CodexWsCandidate {
            let mut candidate = candidate(first_step, provider_id);
            if let Some(timeouts) = self.timeouts.lock().expect("timeouts should lock").clone() {
                let attempt = candidate
                    .attempt
                    .as_mut()
                    .expect("test candidate should retain its planning attempt");
                attempt.plan.timeouts = Some(timeouts);
                candidate.timeouts = CodexWsTimeouts::from_plan(&attempt.plan);
            }
            candidate
        }
    }

    #[async_trait]
    impl CodexWsRuntimePort for TestRuntime {
        fn validate_runtime_fences(&self) -> Result<(), StepPreparationError> {
            self.runtime_fences_valid
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or(StepPreparationError::exclude(
                    "codex_ws_global_configuration_changed",
                ))
        }

        async fn validate_step(
            &self,
            _step: &ResponseCreateStep,
        ) -> Result<(), StepPreparationError> {
            self.validate_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn select_candidates(
            &self,
            first_step: &ResponseCreateStep,
        ) -> Result<Vec<CodexWsCandidate>, StepPreparationError> {
            let mut candidates = Vec::new();
            if self.fail_first_connect {
                candidates.push(self.configured_candidate(first_step, "provider-failed"));
            }
            candidates.push(self.configured_candidate(first_step, "provider-selected"));
            candidates.push(self.configured_candidate(first_step, "provider-unused"));
            Ok(candidates)
        }

        async fn connect(
            &self,
            candidate: CodexWsCandidate,
        ) -> Result<ConnectedCandidate, StepPreparationError> {
            self.started_candidates
                .lock()
                .expect("started candidates should lock")
                .push(candidate.provider_id.clone());
            let call = self.connect_calls.fetch_add(1, Ordering::Relaxed);
            let connect_delay = self
                .connect_delays
                .lock()
                .expect("connect delays should lock")
                .pop_front();
            if let Some(connect_delay) = connect_delay {
                if tokio::time::timeout(
                    candidate.timeouts().connect,
                    tokio::time::sleep(connect_delay),
                )
                .await
                .is_err()
                {
                    self.aborted_candidates
                        .lock()
                        .expect("aborted candidates should lock")
                        .push(candidate.provider_id.clone());
                    return Err(StepPreparationError::exclude("planned_connect_timeout"));
                }
            }
            if self.fail_first_connect && call == 0 {
                self.aborted_candidates
                    .lock()
                    .expect("aborted candidates should lock")
                    .push(candidate.provider_id.clone());
                return Err(StepPreparationError::exclude("planned_handshake_failure"));
            }
            let peer = self
                .official
                .lock()
                .expect("official peer should lock")
                .take()
                .expect("one official peer should connect");
            if self
                .invalidate_fences_during_connect
                .swap(false, Ordering::AcqRel)
            {
                self.runtime_fences_valid.store(false, Ordering::Release);
            }
            Ok(ConnectedCandidate {
                candidate,
                peer,
                handshake_turn_state: None,
            })
        }

        async fn abort_candidate(&self, candidate: &CodexWsCandidate) {
            self.aborted_candidates
                .lock()
                .expect("aborted candidates should lock")
                .push(candidate.provider_id.clone());
        }

        fn abort_candidate_detached(
            &self,
            candidate: &CodexWsCandidate,
            _cleanup_permit: Option<
                tokio::sync::mpsc::OwnedPermit<super::super::CodexWsSettlementCommit>,
            >,
        ) {
            self.aborted_candidates
                .lock()
                .expect("aborted candidates should lock")
                .push(candidate.provider_id.clone());
        }

        async fn mark_unused_candidates(&self, candidates: Vec<CodexWsCandidate>) {
            self.unused_candidates
                .lock()
                .expect("unused candidates should lock")
                .extend(
                    candidates
                        .into_iter()
                        .map(|candidate| candidate.provider_id),
                );
        }

        fn mark_unused_candidates_detached(&self, candidates: Vec<CodexWsCandidate>) {
            self.unused_candidates
                .lock()
                .expect("unused candidates should lock")
                .extend(
                    candidates
                        .into_iter()
                        .map(|candidate| candidate.provider_id),
                );
        }

        async fn prepare_step(
            &self,
            _candidate: &CodexWsCandidate,
            _step: &mut ResponseCreateStep,
        ) -> Result<PreparedStep, StepPreparationError> {
            self.prepare_calls.fetch_add(1, Ordering::Relaxed);
            let prepare_delay = self
                .prepare_delay
                .lock()
                .expect("prepare delay should lock")
                .take();
            if let Some(prepare_delay) = prepare_delay {
                tokio::time::sleep(prepare_delay).await;
            }
            let permit = self
                .gate
                .try_acquire()
                .expect("test step should acquire admission");
            if self.bump_epoch_during_prepare.swap(false, Ordering::AcqRel) {
                self.epoch.store(8, Ordering::Release);
            }
            if self
                .invalidate_fences_during_prepare
                .swap(false, Ordering::AcqRel)
            {
                self.runtime_fences_valid.store(false, Ordering::Release);
            }
            Ok(PreparedStep::for_test(
                "materialized-step".to_string(),
                Some(permit.into()),
            ))
        }

        async fn release_candidate_scheduling_resources(
            &self,
            _candidate: &CodexWsCandidate,
            _first_dispatch: bool,
        ) {
        }

        fn record_step_pending(&self, _usage_context: &CodexWsStepUsageContext) {
            self.pending_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn record_step_rejected(
            &self,
            _usage_context: CodexWsStepUsageContext,
            _elapsed: Duration,
            _status_code: u16,
            error_type: &'static str,
            _error_message: &'static str,
            _cancelled: bool,
        ) {
            self.rejected_calls.fetch_add(1, Ordering::Relaxed);
            self.rejected_reasons
                .lock()
                .expect("rejected reasons should lock")
                .push(error_type);
        }

        fn record_step_stream_started(
            &self,
            _candidate: &CodexWsCandidate,
            _step: &ResponseCreateStep,
            first_byte_elapsed: Duration,
            _usage_report: &UsageReportReservation,
        ) {
            self.stream_started_calls.fetch_add(1, Ordering::Relaxed);
            self.stream_first_byte_ms
                .lock()
                .expect("stream first-byte values should lock")
                .push(first_byte_elapsed.as_millis() as u64);
        }

        fn record_step_terminal(
            &self,
            _candidate: &CodexWsCandidate,
            _step: &ResponseCreateStep,
            _terminal_event: Option<TerminalEventSummary>,
            terminal_kind: Option<TerminalKind>,
            _disposition: CodexWsStepDisposition,
            _first_dispatch: bool,
            first_byte_elapsed: Option<Duration>,
            _elapsed: Duration,
            _usage_report: UsageReportReservation,
        ) {
            self.report_calls.fetch_add(1, Ordering::Relaxed);
            self.report_terminal_kinds
                .lock()
                .expect("terminal kinds should lock")
                .push(terminal_kind);
            self.report_first_byte_ms
                .lock()
                .expect("terminal first-byte values should lock")
                .push(first_byte_elapsed.map(|elapsed| elapsed.as_millis() as u64));
            if self.gate.snapshot().in_flight == 0 {
                self.report_after_release.fetch_add(1, Ordering::Relaxed);
            }
        }

        fn catalog_epoch(&self) -> u64 {
            self.epoch.load(Ordering::Acquire) as u64
        }
    }

    fn request() -> String {
        request_step("step-1", None)
    }

    fn relay_text(value: impl Into<Bytes>) -> RelayFrame {
        RelayFrame::Text(value.into())
    }

    fn text_bytes_contains(text: &Bytes, needle: &str) -> bool {
        std::str::from_utf8(text).is_ok_and(|text| text.contains(needle))
    }

    fn request_step(correlation_id: &str, previous_response_id: Option<&str>) -> String {
        json!({
            "type": "response.create",
            "model": "gpt-5.4",
            "previous_response_id": previous_response_id,
            "client_metadata": {
                "session_id": "session-1",
                "thread_id": "thread-1",
                "aether.sub2api_step_control": {
                    "version": 1,
                    "sub2api_step_correlation_id": correlation_id,
                    "sub2api_binding_epoch_id": "epoch-1",
                    "sub2api_binding_generation": 1
                }
            }
        })
        .to_string()
    }

    fn request_step_without_model(
        correlation_id: &str,
        previous_response_id: Option<&str>,
    ) -> String {
        let mut request: serde_json::Value =
            serde_json::from_str(&request_step(correlation_id, previous_response_id))
                .expect("request fixture should parse");
        request
            .as_object_mut()
            .expect("request fixture should be an object")
            .remove("model");
        request.to_string()
    }

    fn candidate(step: &ResponseCreateStep, provider_id: &str) -> CodexWsCandidate {
        let report_context = Some(json!({"request_id": "request-1"}));
        let plan = aether_contracts::ExecutionPlan {
            request_id: "request-1".to_string(),
            candidate_id: None,
            provider_name: Some("Codex".to_string()),
            provider_id: provider_id.to_string(),
            endpoint_id: "endpoint-1".to_string(),
            key_id: "key-1".to_string(),
            method: "GET".to_string(),
            url: "wss://chatgpt.com/backend-api/codex/responses".to_string(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".to_string()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(step.value.clone()),
            stream: true,
            client_api_format: "openai:responses".to_string(),
            provider_api_format: "openai:responses".to_string(),
            model_name: Some(step.model.clone()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        };
        let timeouts = CodexWsTimeouts::from_plan(&plan);
        let lifecycle = Arc::new(CodexWsCandidateLifecycle::new(
            &plan,
            report_context.as_ref(),
        ));
        CodexWsCandidate {
            attempt: Some(crate::ai_serving::AiStreamAttempt {
                plan: plan.clone(),
                report_kind: Some("openai_responses_stream_success".to_string()),
                report_context: report_context.clone(),
            }),
            provider_id: provider_id.to_string(),
            endpoint_id: "endpoint-1".to_string(),
            key_id: "key-1".to_string(),
            model: step.model.clone(),
            mapped_model: step.model.clone(),
            body_rules: None,
            provider_body_patch: Arc::from([]),
            force_body_stream_field: false,
            enable_model_directives: false,
            model_directive_mapping: None,
            client_api_key_id: String::new(),
            headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            account_profile: None,
            report_kind: "openai_responses_stream_success".to_string(),
            identity: step.official_identity.clone(),
            route: OutboundRoute::Direct,
            timeouts,
            lifecycle,
            selected_catalog_epoch: 7,
            provider_concurrent_limit: None,
            key_concurrent_limit: None,
            key_rpm_limit: None,
            shared_global_generation: "global-generation".to_string(),
            shared_catalog_generation: "catalog-generation".to_string(),
            shared_key_generation: "key-generation".to_string(),
            prewrite_cleanup_permit: None,
        }
    }

    fn short_timeouts() -> aether_contracts::ExecutionTimeouts {
        aether_contracts::ExecutionTimeouts {
            connect_ms: Some(10),
            write_ms: Some(10),
            first_byte_ms: Some(10),
            read_ms: Some(10),
            total_ms: Some(100),
            ..aether_contracts::ExecutionTimeouts::default()
        }
    }

    #[test]
    fn initial_connect_budget_is_one_shared_bounded_deadline() {
        assert_eq!(initial_connect_budget(&[]), Duration::from_millis(1));

        let step = parse_response_create(&request(), ResponseCreateContext::First)
            .expect("request fixture should parse");
        let mut short = candidate(&step, "provider-short");
        short.timeouts.connect = Duration::from_millis(40);
        let mut long = candidate(&step, "provider-long");
        long.timeouts.connect = Duration::from_secs(120);

        assert_eq!(
            initial_connect_budget(&[short, long]),
            MAX_INITIAL_CONNECT_BUDGET
        );

        let mut first = candidate(&step, "provider-first");
        first.timeouts.connect = Duration::from_millis(10);
        let mut second = candidate(&step, "provider-second");
        second.timeouts.connect = Duration::from_millis(20);
        assert_eq!(
            initial_connect_budget(&[first, second]),
            Duration::from_millis(30)
        );
    }

    #[test]
    fn official_text_frame_limit_matches_the_public_relay_contract() {
        assert!(official_text_frame_within_public_limit(&Bytes::from(
            vec![b'x'; MAX_PUBLIC_CLIENT_PAYLOAD_BYTES],
        )));
        assert!(!official_text_frame_within_public_limit(&Bytes::from(
            vec![b'x'; MAX_PUBLIC_CLIENT_PAYLOAD_BYTES + 1],
        )));
    }

    #[test]
    fn official_terminal_failure_uses_the_preserved_provider_status() {
        let outcome = terminal_outcome(
            TerminalKind::Failed,
            TerminalEventSummary {
                provider_status_code: Some(429),
                provider_error_code: Some("rate_limit_exceeded".into()),
                provider_error_message: Some("retry later".into()),
                provider_error_body: Some(r#"{"error":{"code":"rate_limit_exceeded"}}"#.into()),
                ..TerminalEventSummary::default()
            },
            false,
            Bytes::from_static(b"{}"),
        );
        let StepOutcome::Poisoned { disposition, .. } = outcome else {
            panic!("provider failure should poison the current binding");
        };
        assert_eq!(
            disposition,
            CodexWsStepDisposition::ProviderFailure {
                status_code: 429,
                error_type: "codex_ws_official_rate_limit_exceeded".into(),
                error_message: "retry later".into(),
                error_body: Some(r#"{"error":{"code":"rate_limit_exceeded"}}"#.into()),
            }
        );
    }

    fn idle_binding() -> BindingState {
        BindingState {
            model: "gpt-5.4".into(),
            binding_epoch_id: "epoch-1".into(),
            binding_generation: 1,
            seen_step_correlations: SettledResponseHistory::new(),
            last_completed_response_id: None,
            turn_state: None,
            settled_response_ids: SettledResponseHistory::new(),
        }
    }

    #[tokio::test]
    async fn idle_official_ping_is_answered_without_blocking_the_next_step() {
        let ping = Bytes::from_static(b"official-ping");
        let (mut official, official_sent) =
            ScriptedPeer::new([(Duration::ZERO, RelayFrame::Ping(ping.clone()))]);
        let (client, _) = ScriptedPeer::new([(Duration::from_millis(1), relay_text(request()))]);
        let mut client: Box<dyn RelayPeer> = Box::new(client);

        let (step, _started_at) = receive_idle_step(&mut client, &mut official, &idle_binding())
            .await
            .expect("idle receive should succeed")
            .expect("the next step should parse");

        assert_eq!(step.fence.correlation_id, "step-1");
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .as_slice(),
            &[RelayFrame::Pong(ping)]
        );
    }

    #[tokio::test]
    async fn idle_official_close_poisons_the_bound_connection_immediately() {
        let (mut official, _) = ScriptedPeer::new([(Duration::ZERO, RelayFrame::Close)]);
        let (client, _) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);
        let mut client: Box<dyn RelayPeer> = Box::new(client);

        let error = receive_idle_step(&mut client, &mut official, &idle_binding())
            .await
            .expect_err("an idle upstream close must poison the binding");
        assert_eq!(error.message(), "official connection closed while idle");
    }

    fn sent_text_contains(sent: &Arc<Mutex<Vec<RelayFrame>>>, needle: &str) -> bool {
        sent.lock()
            .expect("sent frames should lock")
            .iter()
            .any(|frame| {
                matches!(
                    frame,
                    RelayFrame::Text(text)
                        if std::str::from_utf8(text)
                            .is_ok_and(|text| text.contains(needle))
                )
            })
    }

    #[tokio::test(start_paused = true)]
    async fn falls_back_before_write_and_releases_step_permit_after_exactly_one_report() {
        let created = json!({
            "type": "response.created",
            "response": {"id": "resp-1", "model": "gpt-5.4"}
        })
        .to_string();
        let terminal = json!({
            "type": "response.completed",
            "response": {"id": "resp-1", "model": "gpt-5.4"}
        })
        .to_string();
        let (official, official_sent) = ScriptedPeer::new([
            (Duration::from_millis(7), relay_text(created)),
            (Duration::ZERO, relay_text(terminal.clone())),
        ]);
        let runtime = TestRuntime::new(Box::new(official), true);
        runtime.push_connect_delay(Duration::from_millis(11));
        let (client, client_sent) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (Duration::from_millis(20), RelayFrame::Close),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.validate_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.connect_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.stream_started_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_after_release.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            *runtime
                .stream_first_byte_ms
                .lock()
                .expect("stream first-byte values should lock"),
            vec![18]
        );
        assert_eq!(
            *runtime
                .report_first_byte_ms
                .lock()
                .expect("terminal first-byte values should lock"),
            vec![Some(18)]
        );
        assert_eq!(
            *runtime
                .started_candidates
                .lock()
                .expect("started candidates should lock"),
            vec!["provider-failed", "provider-selected"]
        );
        assert_eq!(
            *runtime
                .aborted_candidates
                .lock()
                .expect("aborted candidates should lock"),
            vec!["provider-failed"]
        );
        assert_eq!(
            *runtime
                .unused_candidates
                .lock()
                .expect("unused candidates should lock"),
            vec!["provider-unused"]
        );
        assert_eq!(
            official_sent.lock().expect("official frames should lock")[0],
            relay_text("materialized-step")
        );
        assert!(client_sent
            .lock()
            .expect("client frames should lock")
            .contains(&relay_text(terminal)));
    }

    #[tokio::test]
    async fn rejects_second_in_flight_create_without_a_second_provider_write() {
        let (official, official_sent) = ScriptedPeer::new([]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let (client, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (Duration::ZERO, relay_text(request())),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        let official_sent = official_sent.lock().expect("official frames should lock");
        assert_eq!(
            official_sent
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            1
        );
        assert_eq!(runtime.validate_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test]
    async fn catalog_change_during_prepare_is_rejected_before_provider_write() {
        let (official, official_sent) = ScriptedPeer::new([]);
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime
            .bump_epoch_during_prepare
            .store(true, Ordering::Release);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["account_catalog_changed"]
        );
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            0
        );
        let client_sent = client_sent.lock().expect("client frames should lock");
        assert!(client_sent.iter().any(|frame| match frame {
            RelayFrame::Text(text) => {
                text_bytes_contains(text, "account_catalog_changed")
                    && text_bytes_contains(text, "codex_official_ws.not_executed")
            }
            _ => false,
        }));
    }

    #[tokio::test]
    async fn global_disable_while_connecting_is_rejected_before_provider_write() {
        let (official, official_sent) = ScriptedPeer::new([]);
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime
            .invalidate_fences_during_connect
            .store(true, Ordering::Release);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["codex_ws_global_configuration_changed"]
        );
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            0
        );
        assert_eq!(
            *runtime
                .aborted_candidates
                .lock()
                .expect("aborted candidates should lock"),
            vec!["provider-selected"]
        );
        let client_sent = client_sent.lock().expect("client frames should lock");
        assert!(client_sent.iter().any(|frame| match frame {
            RelayFrame::Text(text) => {
                text_bytes_contains(text, "codex_ws_global_configuration_changed")
                    && text_bytes_contains(text, "codex_official_ws.not_executed")
            }
            _ => false,
        }));
    }

    #[tokio::test]
    async fn global_disable_during_prepare_is_rejected_before_provider_write() {
        let (official, official_sent) = ScriptedPeer::new([]);
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime
            .invalidate_fences_during_prepare
            .store(true, Ordering::Release);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["codex_ws_global_configuration_changed"]
        );
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            0
        );
        let client_sent = client_sent.lock().expect("client frames should lock");
        assert!(client_sent.iter().any(|frame| match frame {
            RelayFrame::Text(text) => {
                text_bytes_contains(text, "codex_ws_global_configuration_changed")
                    && text_bytes_contains(text, "codex_official_ws.not_executed")
            }
            _ => false,
        }));
    }

    #[tokio::test]
    async fn global_disable_racing_provider_write_never_claims_not_executed() {
        let runtime_fences_valid = Arc::new(AtomicBool::new(true));
        let (official, official_sent) = ScriptedPeer::new([]);
        let official = InvalidateFenceAfterSendPeer {
            inner: Box::new(official),
            runtime_fences_valid: Arc::clone(&runtime_fences_valid),
        };
        let runtime =
            TestRuntime::with_runtime_fences(Box::new(official), false, runtime_fences_valid);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            *runtime
                .report_terminal_kinds
                .lock()
                .expect("terminal kinds should lock"),
            vec![None]
        );
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            1
        );
        assert!(runtime
            .aborted_candidates
            .lock()
            .expect("aborted candidates should lock")
            .is_empty());
        let client_sent = client_sent.lock().expect("client frames should lock");
        assert!(client_sent.iter().any(|frame| match frame {
            RelayFrame::Text(text) => {
                text_bytes_contains(text, "codex_ws_global_configuration_changed")
                    && text_bytes_contains(text, "\"provider_write_state\":\"unknown\"")
                    && !text_bytes_contains(text, "codex_official_ws.not_executed")
            }
            _ => false,
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn stale_and_duplicate_provenance_events_are_consumed_without_mis_settlement() {
        let first_created = json!({
            "type": "response.created",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let first_terminal = json!({
            "type": "response.completed",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let second_created = json!({
            "type": "response.created",
            "response": {"id": "resp-2"}
        })
        .to_string();
        let stale_delta = json!({
            "type": "response.output_text.delta",
            "response_id": "resp-1",
            "delta": "stale"
        })
        .to_string();
        let stale_boundary = json!({
            "type": "response.output_item.done",
            "response_id": "resp-1",
            "turn_state": "must-not-bind"
        })
        .to_string();
        let second_delta = json!({
            "type": "response.output_text.delta",
            "response_id": "resp-2",
            "delta": "current"
        })
        .to_string();
        let second_terminal = json!({
            "type": "response.completed",
            "response": {"id": "resp-2"}
        })
        .to_string();
        let (official, official_sent) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(first_created.clone())),
            (Duration::ZERO, relay_text(first_created.clone())),
            (Duration::ZERO, relay_text(first_terminal.clone())),
            (Duration::ZERO, relay_text(first_terminal.clone())),
            (Duration::ZERO, relay_text(first_created.clone())),
            (Duration::ZERO, relay_text(stale_delta.clone())),
            (Duration::ZERO, relay_text(stale_boundary.clone())),
            (
                Duration::from_millis(25),
                relay_text(second_created.clone()),
            ),
            (Duration::ZERO, relay_text(second_delta.clone())),
            (Duration::ZERO, relay_text(second_terminal.clone())),
        ]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let (client, client_sent) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (
                Duration::from_millis(20),
                relay_text(request_step("step-2", Some("resp-1"))),
            ),
            (Duration::from_millis(100), RelayFrame::Close),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            *runtime
                .report_terminal_kinds
                .lock()
                .expect("terminal kinds should lock"),
            vec![Some(TerminalKind::Completed), Some(TerminalKind::Completed)]
        );
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            2
        );
        let client_sent = client_sent.lock().expect("client frames should lock");
        for once_only in [
            first_created,
            first_terminal,
            second_created,
            second_delta,
            second_terminal,
        ] {
            let target = relay_text(once_only);
            assert_eq!(
                client_sent.iter().filter(|frame| *frame == &target).count(),
                1
            );
        }
        assert!(!client_sent.contains(&relay_text(stale_delta)));
        assert!(!client_sent.contains(&relay_text(stale_boundary)));
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test]
    async fn mismatched_id_bearing_delta_is_never_forwarded() {
        let created = json!({
            "type": "response.created",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let mismatched_delta = json!({
            "type": "response.output_text.delta",
            "response_id": "resp-other",
            "delta": "must-not-forward"
        })
        .to_string();
        let (official, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(created.clone())),
            (Duration::ZERO, relay_text(mismatched_delta.clone())),
        ]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .report_terminal_kinds
                .lock()
                .expect("terminal kinds should lock"),
            vec![None]
        );
        let client_sent = client_sent.lock().expect("client frames should lock");
        assert!(client_sent.contains(&relay_text(created)));
        assert!(!client_sent.contains(&relay_text(mismatched_delta)));
        assert!(client_sent.iter().any(|frame| match frame {
            RelayFrame::Text(text) => {
                text_bytes_contains(text, "official event provenance mismatch")
            }
            _ => false,
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn bound_follow_up_may_omit_model_but_may_not_change_it() {
        let first_created = json!({
            "type": "response.created",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let first_terminal = json!({
            "type": "response.completed",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let second_created = json!({
            "type": "response.created",
            "response": {"id": "resp-2"}
        })
        .to_string();
        let second_terminal = json!({
            "type": "response.completed",
            "response": {"id": "resp-2"}
        })
        .to_string();
        let (official, official_sent) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(first_created)),
            (Duration::ZERO, relay_text(first_terminal)),
            (Duration::from_millis(25), relay_text(second_created)),
            (Duration::ZERO, relay_text(second_terminal)),
        ]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let (client, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (
                Duration::from_millis(20),
                relay_text(request_step_without_model("step-2", Some("resp-1"))),
            ),
            (Duration::from_millis(20), RelayFrame::Close),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            2
        );

        let (official, official_sent) = ScriptedPeer::new([
            (
                Duration::ZERO,
                relay_text(
                    json!({"type":"response.created","response":{"id":"resp-1"}}).to_string(),
                ),
            ),
            (
                Duration::ZERO,
                relay_text(
                    json!({"type":"response.completed","response":{"id":"resp-1"}}).to_string(),
                ),
            ),
        ]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let mut changed: serde_json::Value =
            serde_json::from_str(&request_step("step-2", Some("resp-1")))
                .expect("request fixture should parse");
        changed["model"] = json!("gpt-other");
        let (client, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (Duration::from_millis(20), relay_text(changed.to_string())),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn connect_timeout_falls_back_before_any_provider_write() {
        let created = json!({
            "type": "response.created",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let terminal = json!({
            "type": "response.completed",
            "response": {"id": "resp-1"}
        })
        .to_string();
        let (official, official_sent) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(created)),
            (Duration::ZERO, relay_text(terminal)),
        ]);
        let runtime = TestRuntime::new(Box::new(official), true);
        runtime.set_timeouts(short_timeouts());
        runtime.push_connect_delay(Duration::from_millis(100));
        let (client, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (Duration::from_millis(20), RelayFrame::Close),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.connect_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            *runtime
                .aborted_candidates
                .lock()
                .expect("aborted candidates should lock"),
            vec!["provider-failed"]
        );
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pending_usage_is_visible_while_the_initial_provider_connect_is_in_flight() {
        let (official, _) = ScriptedPeer::new([]);
        let runtime = Arc::new(TestRuntime::new(Box::new(official), false));
        runtime.push_connect_delay(Duration::from_secs(30));
        let (client, _) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);
        let task_runtime = Arc::clone(&runtime);
        let task = tokio::spawn(async move {
            run_codex_ws_session(Box::new(client), task_runtime.as_ref()).await;
        });
        while runtime.connect_calls.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }

        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 0);

        task.abort();
        assert!(task
            .await
            .expect_err("task should be cancelled")
            .is_cancelled());
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["codex_ws_step_cancelled_before_execution"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn initial_connect_budget_bounds_all_sequential_candidates() {
        let (official, _) = ScriptedPeer::new([]);
        let runtime = TestRuntime::new(Box::new(official), true);
        let mut timeouts = short_timeouts();
        timeouts.connect_ms = Some(30_000);
        runtime.set_timeouts(timeouts);
        runtime.push_connect_delay(Duration::from_secs(40));
        runtime.push_connect_delay(Duration::from_secs(40));
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);
        let started_at = tokio::time::Instant::now();

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(started_at.elapsed(), MAX_INITIAL_CONNECT_BUDGET);
        assert_eq!(runtime.connect_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["initial_connect_budget_exhausted"]
        );
        assert_eq!(
            *runtime
                .started_candidates
                .lock()
                .expect("started candidates should lock"),
            vec!["provider-failed", "provider-selected"]
        );
        assert_eq!(
            *runtime
                .unused_candidates
                .lock()
                .expect("unused candidates should lock"),
            vec!["provider-unused"]
        );
        assert!(client_sent
            .lock()
            .expect("client frames should lock")
            .iter()
            .any(|frame| matches!(frame, RelayFrame::Text(text) if text_bytes_contains(text, "initial_connect_budget_exhausted"))));
    }

    #[tokio::test(start_paused = true)]
    async fn large_terminal_delivery_uses_bounded_step_write_budget_after_settlement() {
        let step = parse_response_create(&request(), ResponseCreateContext::First)
            .expect("request fixture should parse");
        let timeouts = CodexWsTimeouts {
            connect: Duration::from_secs(1),
            write: Duration::from_secs(2),
            first_byte: Duration::from_secs(1),
            read: Duration::from_secs(1),
            total: Duration::from_secs(10),
        };
        let deadlines = StepDeadlines::new(tokio::time::Instant::now(), timeouts);
        let (client, sent) = ScriptedPeer::new([]);
        let mut client: Box<dyn RelayPeer> =
            Box::new(client.with_send_delay(Duration::from_millis(500)));
        let terminal = Bytes::from(vec![b'x'; MAX_PUBLIC_CLIENT_PAYLOAD_BYTES]);
        let started_at = tokio::time::Instant::now();

        assert!(
            deliver_terminal_after_settlement(&mut client, &step, terminal, false, &deadlines)
                .await
        );
        assert_eq!(started_at.elapsed(), Duration::from_millis(500));
        assert!(sent
            .lock()
            .expect("client frames should lock")
            .iter()
            .any(|frame| matches!(frame, RelayFrame::Text(text) if text.len() == MAX_PUBLIC_CLIENT_PAYLOAD_BYTES)));
    }

    #[tokio::test(start_paused = true)]
    async fn provider_write_timeout_is_unknown_and_never_replayed() {
        let (official, official_sent) = ScriptedPeer::new([]);
        let official = official.with_flush_delay(Duration::from_millis(100));
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime.set_timeouts(short_timeouts());
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.connect_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_after_release.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.stream_started_calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            *runtime
                .report_first_byte_ms
                .lock()
                .expect("terminal first-byte values should lock"),
            vec![None]
        );
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            official_sent
                .lock()
                .expect("official frames should lock")
                .iter()
                .filter(|frame| matches!(frame, RelayFrame::Text(_)))
                .count(),
            1
        );
        assert!(sent_text_contains(
            &client_sent,
            "official_provider_write_timeout"
        ));
        assert!(!sent_text_contains(&client_sent, "proven_not_executed"));
    }

    #[tokio::test(start_paused = true)]
    async fn provider_readiness_timeout_is_proven_not_executed() {
        let (official, official_sent) = ScriptedPeer::new([]);
        let official = official.with_send_delay(Duration::from_millis(100));
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime.set_timeouts(short_timeouts());
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["official_provider_not_ready_timeout"]
        );
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert!(official_sent
            .lock()
            .expect("official frames should lock")
            .is_empty());
        assert!(sent_text_contains(
            &client_sent,
            "official_provider_not_ready_timeout"
        ));
        assert!(sent_text_contains(&client_sent, "proven_not_executed"));
    }

    #[tokio::test(start_paused = true)]
    async fn first_business_frame_timeout_releases_and_reports_once() {
        let (official, _) = ScriptedPeer::new([]);
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime.set_timeouts(short_timeouts());
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert!(sent_text_contains(
            &client_sent,
            "official Codex first business frame timed out"
        ));
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_after_release.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn upstream_read_timeout_is_not_extended_by_unknown_text() {
        let created = json!({"type":"response.created","response":{"id":"resp-1"}}).to_string();
        let unknown = json!({"type":"response.future_metadata","value":true}).to_string();
        let (official, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(created)),
            (Duration::from_millis(8), relay_text(unknown.clone())),
            (Duration::from_millis(8), relay_text(unknown)),
        ]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let mut timeouts = short_timeouts();
        timeouts.total_ms = Some(20);
        runtime.set_timeouts(timeouts);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert!(sent_text_contains(
            &client_sent,
            "official Codex upstream idle read timed out"
        ));
        assert!(!sent_text_contains(
            &client_sent,
            "Codex WebSocket step total timed out"
        ));
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_settled_frames_do_not_satisfy_next_step_first_byte() {
        let first_created =
            json!({"type":"response.created","response":{"id":"resp-1"}}).to_string();
        let first_terminal =
            json!({"type":"response.completed","response":{"id":"resp-1"}}).to_string();
        let stale_created = first_created.clone();
        let (official, _) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(first_created)),
            (Duration::ZERO, relay_text(first_terminal)),
            (Duration::from_millis(8), relay_text(stale_created.clone())),
            (Duration::from_millis(8), relay_text(stale_created)),
        ]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let mut timeouts = short_timeouts();
        timeouts.total_ms = Some(20);
        runtime.set_timeouts(timeouts);
        let (client, client_sent) = ScriptedPeer::new([
            (Duration::ZERO, relay_text(request())),
            (
                Duration::from_millis(1),
                relay_text(request_step("step-2", Some("resp-1"))),
            ),
        ]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert!(sent_text_contains(
            &client_sent,
            "official Codex first business frame timed out"
        ));
        assert_eq!(runtime.prepare_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn total_timeout_wins_even_when_read_deadline_is_later() {
        let created = json!({"type":"response.created","response":{"id":"resp-1"}}).to_string();
        let (official, _) = ScriptedPeer::new([(Duration::from_millis(5), relay_text(created))]);
        let runtime = TestRuntime::new(Box::new(official), false);
        let mut timeouts = short_timeouts();
        timeouts.first_byte_ms = Some(10);
        timeouts.read_ms = Some(100);
        timeouts.total_ms = Some(15);
        runtime.set_timeouts(timeouts);
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert!(sent_text_contains(
            &client_sent,
            "Codex WebSocket step total timed out"
        ));
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_client_write_is_bounded_and_releases_step_guard() {
        let created = json!({"type":"response.created","response":{"id":"resp-1"}}).to_string();
        let (official, _) = ScriptedPeer::new([(Duration::ZERO, relay_text(created))]);
        let runtime = TestRuntime::new(Box::new(official), false);
        runtime.set_timeouts(short_timeouts());
        let (client, client_sent) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);
        let client = client.with_send_delay(Duration::from_millis(100));

        run_codex_ws_session(Box::new(client), &runtime).await;

        assert!(client_sent
            .lock()
            .expect("client frames should lock")
            .is_empty());
        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_after_release.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn task_cancel_before_provider_write_aborts_candidate_and_cancels_pending_usage() {
        let (official, _) = ScriptedPeer::new([]);
        let runtime = Arc::new(TestRuntime::new(Box::new(official), false));
        runtime.set_prepare_delay(Duration::from_secs(60));
        let (client, _) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);
        let task_runtime = Arc::clone(&runtime);
        let task = tokio::spawn(async move {
            run_codex_ws_session(Box::new(client), task_runtime.as_ref()).await;
        });
        while runtime.prepare_calls.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }

        task.abort();
        assert!(task
            .await
            .expect_err("task should be cancelled")
            .is_cancelled());

        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 0);
        assert_eq!(runtime.pending_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.rejected_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *runtime
                .rejected_reasons
                .lock()
                .expect("rejected reasons should lock"),
            vec!["codex_ws_step_cancelled_before_execution"]
        );
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert_eq!(
            *runtime
                .aborted_candidates
                .lock()
                .expect("aborted candidates should lock"),
            vec!["provider-selected"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn task_cancel_after_provider_write_reports_unknown_without_aborting_candidate() {
        let send_started = Arc::new(AtomicUsize::new(0));
        let (official, _) = ScriptedPeer::new([]);
        let official = official.with_send_started_counter(Arc::clone(&send_started));
        let runtime = Arc::new(TestRuntime::new(Box::new(official), false));
        let (client, _) = ScriptedPeer::new([(Duration::ZERO, relay_text(request()))]);
        let task_runtime = Arc::clone(&runtime);
        let task = tokio::spawn(async move {
            run_codex_ws_session(Box::new(client), task_runtime.as_ref()).await;
        });
        while send_started.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }

        task.abort();
        assert!(task
            .await
            .expect_err("task should be cancelled")
            .is_cancelled());

        assert_eq!(runtime.report_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.report_after_release.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.gate.snapshot().in_flight, 0);
        assert!(runtime
            .aborted_candidates
            .lock()
            .expect("aborted candidates should lock")
            .is_empty());
    }

    #[test]
    fn long_lived_binding_keeps_only_bounded_settled_response_history() {
        let mut history = SettledResponseHistory::new();
        for index in 0..(SETTLED_RESPONSE_HISTORY_CAPACITY + 3) {
            history.insert(format!("resp-{index}"));
        }

        assert_eq!(history.len(), SETTLED_RESPONSE_HISTORY_CAPACITY);
        assert!(!history.contains("resp-0"));
        assert!(!history.contains("resp-2"));
        assert!(history.contains("resp-3"));
        assert!(history.contains(&format!("resp-{}", SETTLED_RESPONSE_HISTORY_CAPACITY + 2)));
    }

    #[test]
    fn settled_response_history_also_obeys_its_total_byte_budget() {
        let mut history = SettledResponseHistory::new();
        for index in 0..(SETTLED_RESPONSE_HISTORY_CAPACITY + 3) {
            let response_id = format!(
                "{index:03}{}",
                "x".repeat(crate::codex_ws::protocol::MAX_RESPONSE_ID_BYTES - 3)
            );
            history.insert(response_id);
        }

        assert!(history.len() < SETTLED_RESPONSE_HISTORY_CAPACITY);
        assert!(history.total_bytes() <= SETTLED_RESPONSE_HISTORY_BYTE_CAPACITY);
        assert!(!history.contains(&format!(
            "000{}",
            "x".repeat(crate::codex_ws::protocol::MAX_RESPONSE_ID_BYTES - 3)
        )));
        assert!(history.contains(&format!(
            "{:03}{}",
            SETTLED_RESPONSE_HISTORY_CAPACITY + 2,
            "x".repeat(crate::codex_ws::protocol::MAX_RESPONSE_ID_BYTES - 3)
        )));
    }
}
