use aether_ai_serving::{
    run_ai_attempt_loop, AiAttemptLoopOutcome, AiAttemptLoopPort, AiExecutionAttempt,
};
use aether_contracts::ExecutionPlan;
use aether_data_contracts::repository::candidates::{
    RequestCandidateStatus, StoredRequestCandidate,
};
use aether_scheduler_core::{
    parse_request_candidate_report_context, SchedulerRequestCandidateStatusUpdate,
};
use aether_usage_runtime::{usage_json_text_matches_risk_control, usage_text_matches_risk_control};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::Response;
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn, Instrument};

use crate::ai_serving::LocalExecutionAttemptSource;
use crate::clock::current_unix_ms;
use crate::control::GatewayControlDecision;
use crate::execution_runtime::{execute_execution_runtime_stream, execute_execution_runtime_sync};
use crate::executor::{build_local_execution_exhaustion, LocalExecutionRequestOutcome};
use crate::handlers::shared::provider_pool::{
    admin_provider_pool_key_terminal_error_reason, release_admin_provider_pool_key_lease,
};
use crate::log_ids::short_request_id;
use crate::orchestration::local_execution_candidate_metadata_from_report_context;
use crate::orchestration::{
    apply_local_execution_effect, prepare_pool_attempt_started_effect,
    release_pool_sticky_initialization_for_owner, LocalExecutionEffect,
    LocalExecutionEffectContext, PoolAttemptStartCleanupGuard,
};
use crate::privacy::RedactionExecutionCandidateId;
use crate::request_candidate_runtime::{
    record_local_request_candidate_status, record_skipped_local_request_candidate,
    RequestCandidateRuntimeWriter,
};
use crate::scheduler::session_risk_control::{
    client_session_key_from_metadata, provider_session_risk_control_avoidance_mode,
    ProviderSessionRiskControlAvoidanceMode,
};
use crate::{AppState, GatewayError};

const DEFAULT_STREAM_FIRST_BYTE_WATCHDOG_TIMEOUT_MS: u64 = 30_000;
const POOL_STICKY_COLLATERAL_AVOIDANCE_SKIP_REASON: &str = "pool_sticky_collateral_avoidance";

fn attach_redaction_execution_candidate(response: &mut Response<Body>, candidate_id: Option<&str>) {
    if let Some(candidate_id) = candidate_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        response
            .extensions_mut()
            .insert(RedactionExecutionCandidateId::new(candidate_id));
    }
}

#[derive(Debug, Default)]
struct ProviderSessionRiskControlLoopBlocks {
    session_blocked: bool,
    provider_ids: BTreeSet<String>,
}

fn new_provider_session_risk_control_blocks(
) -> Arc<tokio::sync::Mutex<ProviderSessionRiskControlLoopBlocks>> {
    Arc::new(tokio::sync::Mutex::new(
        ProviderSessionRiskControlLoopBlocks::default(),
    ))
}

#[derive(Debug, Default)]
struct PoolStickyCollateralLoopBlocks {
    provider_ids: BTreeSet<String>,
}

fn new_pool_sticky_collateral_blocks() -> Arc<tokio::sync::Mutex<PoolStickyCollateralLoopBlocks>> {
    Arc::new(tokio::sync::Mutex::new(
        PoolStickyCollateralLoopBlocks::default(),
    ))
}

async fn provider_risk_control_session_avoidance_mode(
    state: &AppState,
    provider_id: &str,
) -> ProviderSessionRiskControlAvoidanceMode {
    let provider_ids = [provider_id.to_string()];
    state
        .read_provider_catalog_providers_by_ids(&provider_ids)
        .await
        .ok()
        .and_then(|providers| {
            providers
                .into_iter()
                .find(|provider| provider.id == provider_id)
        })
        .map(|provider| provider_session_risk_control_avoidance_mode(provider.config.as_ref()))
        .unwrap_or(ProviderSessionRiskControlAvoidanceMode::Disabled)
}

fn value_matches_risk_control(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(usage_json_text_matches_risk_control)
}

fn candidate_matches_risk_control(candidate: &StoredRequestCandidate) -> bool {
    usage_text_matches_risk_control(candidate.error_message.as_deref())
        || value_matches_risk_control(
            candidate
                .extra_data
                .as_ref()
                .and_then(|value| value.get("upstream_response"))
                .and_then(|value| value.get("body")),
        )
        || value_matches_risk_control(
            candidate
                .extra_data
                .as_ref()
                .and_then(|value| value.get("error_flow")),
        )
}

async fn read_request_candidate(
    state: &AppState,
    request_id: &str,
    candidate_id: &str,
) -> Option<StoredRequestCandidate> {
    state
        .read_request_candidates_by_request_id(request_id)
        .await
        .ok()?
        .into_iter()
        .find(|candidate| candidate.id == candidate_id)
}

async fn record_provider_session_risk_control_block_if_needed(
    state: &AppState,
    blocks: &Arc<tokio::sync::Mutex<ProviderSessionRiskControlLoopBlocks>>,
    plan: &ExecutionPlan,
) {
    let Some(candidate_id) = plan
        .candidate_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Some(candidate) = read_request_candidate(state, &plan.request_id, candidate_id).await
    else {
        return;
    };
    if !candidate_matches_risk_control(&candidate) {
        return;
    }
    let mode = provider_risk_control_session_avoidance_mode(state, &plan.provider_id).await;
    if !mode.is_enabled() {
        return;
    }
    if let Some(session_key) = client_session_key_from_metadata(candidate.extra_data.as_ref()) {
        if let Err(err) = state
            .remember_provider_session_risk_control_block_if_enabled(&plan.provider_id, session_key)
            .await
        {
            warn!(
                event_name = "provider_session_risk_control_block_record_failed",
                log_type = "ops",
                request_id = %short_request_id(plan.request_id.as_str()),
                candidate_id = ?plan.candidate_id,
                provider_id = %plan.provider_id,
                error = ?err,
                "gateway failed to persist provider session risk-control avoidance block"
            );
        }
    }
    let mut blocks = blocks.lock().await;
    if mode.blocks_session() {
        blocks.session_blocked = true;
    }
    blocks.provider_ids.insert(plan.provider_id.clone());
}

async fn provider_session_risk_control_skip_reason(
    state: &AppState,
    blocks: &Arc<tokio::sync::Mutex<ProviderSessionRiskControlLoopBlocks>>,
    plan: &ExecutionPlan,
    report_context: Option<&serde_json::Value>,
) -> Option<&'static str> {
    let skip_reason = {
        let blocks = blocks.lock().await;
        if blocks.session_blocked {
            Some("session_risk_control_blocked")
        } else if blocks.provider_ids.contains(&plan.provider_id) {
            Some("provider_session_risk_control_avoidance")
        } else {
            None
        }
    }?;
    record_skipped_local_request_candidate(
        state,
        plan,
        report_context,
        skip_reason,
        current_unix_ms(),
    )
    .await;
    Some(skip_reason)
}

async fn remember_pool_sticky_collateral_block_if_enabled(
    state: &AppState,
    blocks: &Arc<tokio::sync::Mutex<PoolStickyCollateralLoopBlocks>>,
    plan: &ExecutionPlan,
    sticky_session_token: &str,
) -> bool {
    match state
        .remember_provider_pool_sticky_collateral_block_if_enabled(
            &plan.provider_id,
            sticky_session_token,
        )
        .await
    {
        Ok(true) => {
            let mut blocks = blocks.lock().await;
            blocks.provider_ids.insert(plan.provider_id.clone());
            true
        }
        Ok(false) => false,
        Err(err) => {
            warn!(
                event_name = "provider_pool_sticky_collateral_block_record_failed",
                log_type = "ops",
                request_id = %short_request_id(plan.request_id.as_str()),
                candidate_id = ?plan.candidate_id,
                provider_id = %plan.provider_id,
                error = ?err,
                "gateway failed to persist provider pool sticky collateral avoidance block"
            );
            false
        }
    }
}

async fn pool_sticky_collateral_skip_reason(
    state: &AppState,
    blocks: &Arc<tokio::sync::Mutex<PoolStickyCollateralLoopBlocks>>,
    plan: &ExecutionPlan,
    report_context: Option<&serde_json::Value>,
) -> Option<&'static str> {
    let sticky_session_token = pool_sticky_session_token_for_attempt(plan, report_context);
    let mut should_skip = {
        let blocks = blocks.lock().await;
        blocks.provider_ids.contains(&plan.provider_id)
    };

    if !should_skip {
        if let Some(sticky_session_token) = sticky_session_token.as_deref() {
            match state
                .provider_session_has_runtime_pool_sticky_collateral_block_if_enabled(
                    &plan.provider_id,
                    sticky_session_token,
                )
                .await
            {
                Ok(true) => {
                    let mut blocks = blocks.lock().await;
                    blocks.provider_ids.insert(plan.provider_id.clone());
                    should_skip = true;
                }
                Ok(false) => {}
                Err(err) => {
                    warn!(
                        event_name = "provider_pool_sticky_collateral_block_lookup_failed",
                        log_type = "ops",
                        request_id = %short_request_id(plan.request_id.as_str()),
                        candidate_id = ?plan.candidate_id,
                        provider_id = %plan.provider_id,
                        error = ?err,
                        "gateway failed to read provider pool sticky collateral avoidance block"
                    );
                }
            }
        }
    }

    if !should_skip {
        let metadata = local_execution_candidate_metadata_from_report_context(report_context);
        if metadata.pool_sticky_bound_key_ineligible {
            if let Some(sticky_session_token) = sticky_session_token.as_deref() {
                should_skip = remember_pool_sticky_collateral_block_if_enabled(
                    state,
                    blocks,
                    plan,
                    sticky_session_token,
                )
                .await;
            }
        }
    }

    if !should_skip {
        return None;
    }

    record_skipped_local_request_candidate(
        state,
        plan,
        report_context,
        POOL_STICKY_COLLATERAL_AVOIDANCE_SKIP_REASON,
        current_unix_ms(),
    )
    .await;
    Some(POOL_STICKY_COLLATERAL_AVOIDANCE_SKIP_REASON)
}

async fn record_pool_sticky_collateral_block_after_failure_if_needed(
    state: &AppState,
    blocks: &Arc<tokio::sync::Mutex<PoolStickyCollateralLoopBlocks>>,
    plan: &ExecutionPlan,
    report_context: Option<&serde_json::Value>,
) {
    let Some(sticky_session_token) = pool_sticky_session_token_for_attempt(plan, report_context)
    else {
        return;
    };
    let Some(candidate_id) = plan
        .candidate_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Some(candidate) = read_request_candidate(state, &plan.request_id, candidate_id).await
    else {
        return;
    };
    if !candidate_matches_pool_sticky_collateral_failure(&candidate) {
        return;
    }
    remember_pool_sticky_collateral_block_if_enabled(state, blocks, plan, &sticky_session_token)
        .await;
}

fn candidate_matches_pool_sticky_collateral_failure(candidate: &StoredRequestCandidate) -> bool {
    if !matches!(
        candidate.status,
        RequestCandidateStatus::Failed | RequestCandidateStatus::Cancelled
    ) {
        return false;
    }
    let status_code = candidate.status_code.unwrap_or(0);
    let error_text = candidate_pool_error_text(candidate);
    pool_sticky_collateral_failure_status_is_account_invalid(status_code, error_text.as_deref())
}

fn pool_sticky_collateral_failure_status_is_account_invalid(
    status_code: u16,
    error_body: Option<&str>,
) -> bool {
    if let Some(reason) = admin_provider_pool_key_terminal_error_reason(status_code, error_body) {
        return !reason.starts_with("payment_required_");
    }
    if matches!(status_code, 401 | 403) {
        return true;
    }
    let body = error_body.unwrap_or_default().to_ascii_lowercase();
    let account_related = body.contains("account")
        || body.contains("user")
        || body.contains("workspace")
        || body.contains("organization");
    (body.contains("invalid") && body.contains("token"))
        || body.contains("banned")
        || body.contains("suspended")
        || (account_related
            && (body.contains("blocked")
                || body.contains("disabled")
                || body.contains("deactivated")))
}

fn candidate_pool_error_text(candidate: &StoredRequestCandidate) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(message) = candidate
        .error_message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(message.to_string());
    }
    if let Some(value) = candidate_json_path(candidate.extra_data.as_ref(), &["error_flow"]) {
        if let Some(text) = request_candidate_json_text(value) {
            parts.push(text);
        }
    }
    if let Some(value) = candidate_json_path(
        candidate.extra_data.as_ref(),
        &["upstream_response", "body"],
    ) {
        if let Some(text) = request_candidate_json_text(value) {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn candidate_json_path<'a>(
    value: Option<&'a serde_json::Value>,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut current = value?;
    for field in path {
        current = current.get(*field)?;
    }
    Some(current)
}

fn request_candidate_json_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| serde_json::to_string(value).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) async fn execute_sync_plan_and_reports<T>(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    plan_kind: &str,
    plan_and_reports: Vec<T>,
) -> Result<LocalExecutionRequestOutcome, GatewayError>
where
    T: AiExecutionAttempt + Send + Sync + 'static,
{
    let candidate_count = plan_and_reports.len();
    let first_provider = plan_and_reports
        .first()
        .and_then(|item| item.execution_plan().provider_name.as_deref())
        .unwrap_or("-")
        .to_string();
    let span = tracing::debug_span!(
        "candidates",
        trace_id = %trace_id,
        plan_kind,
        candidate_count,
    );

    async move {
        tracing::debug!(
            event_name = "candidate_loop_started",
            log_type = "event",
            trace_id = %trace_id,
            plan_kind,
            candidate_count,
            first_provider = first_provider.as_str(),
            "candidate loop started"
        );

        let port = SyncAttemptLoopPort {
            state,
            parts,
            trace_id,
            decision,
            plan_kind,
            provider_session_risk_control_blocks: new_provider_session_risk_control_blocks(),
            pool_sticky_collateral_blocks: new_pool_sticky_collateral_blocks(),
        };
        match run_ai_attempt_loop(&port, plan_and_reports).await? {
            AiAttemptLoopOutcome::Responded(response) => {
                Ok(LocalExecutionRequestOutcome::responded(response))
            }
            AiAttemptLoopOutcome::Exhausted(exhaustion) => {
                Ok(LocalExecutionRequestOutcome::Exhausted(exhaustion))
            }
            AiAttemptLoopOutcome::NoPath => Ok(LocalExecutionRequestOutcome::NoPath),
        }
    }
    .instrument(span)
    .await
}

pub(crate) async fn execute_sync_attempt_source<T, S>(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    plan_kind: &str,
    mut source: S,
) -> Result<LocalExecutionRequestOutcome, GatewayError>
where
    T: AiExecutionAttempt + Send + Sync + 'static,
    S: LocalExecutionAttemptSource<T>,
{
    let span = tracing::debug_span!("candidates", trace_id = %trace_id, plan_kind);

    async move {
        tracing::debug!(
            event_name = "candidate_loop_started",
            log_type = "event",
            trace_id = %trace_id,
            plan_kind,
            "dynamic candidate loop started"
        );

        let port = SyncAttemptLoopPort {
            state,
            parts,
            trace_id,
            decision,
            plan_kind,
            provider_session_risk_control_blocks: new_provider_session_risk_control_blocks(),
            pool_sticky_collateral_blocks: new_pool_sticky_collateral_blocks(),
        };
        run_dynamic_attempt_loop(
            &port,
            &mut source,
            trace_id,
            plan_kind,
            state
                .frontdoor_runtime_guards
                .local_execution_planning_timeout,
        )
        .await
    }
    .instrument(span)
    .await
}

struct SyncAttemptLoopPort<'a> {
    state: &'a AppState,
    parts: &'a http::request::Parts,
    trace_id: &'a str,
    decision: &'a GatewayControlDecision,
    plan_kind: &'a str,
    provider_session_risk_control_blocks:
        Arc<tokio::sync::Mutex<ProviderSessionRiskControlLoopBlocks>>,
    pool_sticky_collateral_blocks: Arc<tokio::sync::Mutex<PoolStickyCollateralLoopBlocks>>,
}

#[async_trait]
impl<T> AiAttemptLoopPort<T> for SyncAttemptLoopPort<'_>
where
    T: AiExecutionAttempt + Send + Sync + 'static,
{
    type Response = Response<Body>;
    type Exhaustion = crate::executor::LocalExecutionExhaustion;
    type Error = GatewayError;

    async fn execute_attempt(&self, attempt: &T) -> Result<Option<Self::Response>, Self::Error> {
        let report_context = attempt.report_context();
        let context = LocalExecutionEffectContext {
            plan: attempt.execution_plan(),
            report_context: report_context.as_ref(),
        };
        if let Some(skip_reason) = pool_sticky_collateral_skip_reason(
            self.state,
            &self.pool_sticky_collateral_blocks,
            attempt.execution_plan(),
            report_context.as_ref(),
        )
        .await
        {
            apply_local_execution_effect(
                self.state,
                context,
                LocalExecutionEffect::PoolAttemptAborted,
            )
            .await;
            warn!(
                event_name = "local_sync_candidate_skipped_by_pool_sticky_collateral_avoidance",
                log_type = "event",
                trace_id = %self.trace_id,
                plan_kind = self.plan_kind,
                request_id = %short_request_id(attempt.execution_plan().request_id.as_str()),
                candidate_id = ?attempt.execution_plan().candidate_id,
                provider_id = %attempt.execution_plan().provider_id,
                endpoint_id = %attempt.execution_plan().endpoint_id,
                key_id = %attempt.execution_plan().key_id,
                skip_reason = skip_reason,
                "gateway skipped local sync candidate after sticky pool collateral avoidance"
            );
            return Ok(None);
        }
        let mut sticky_init_cleanup = PoolAttemptStartCleanupGuard::new(self.state, context);
        let should_execute = prepare_pool_attempt_started_effect(self.state, context).await;
        if !should_execute {
            return Ok(None);
        }
        if let Some(skip_reason) = provider_session_risk_control_skip_reason(
            self.state,
            &self.provider_session_risk_control_blocks,
            attempt.execution_plan(),
            report_context.as_ref(),
        )
        .await
        {
            apply_local_execution_effect(
                self.state,
                LocalExecutionEffectContext {
                    plan: attempt.execution_plan(),
                    report_context: report_context.as_ref(),
                },
                LocalExecutionEffect::PoolAttemptAborted,
            )
            .await;
            warn!(
                event_name = "local_sync_candidate_skipped_by_provider_session_risk_control",
                log_type = "event",
                trace_id = %self.trace_id,
                plan_kind = self.plan_kind,
                request_id = %short_request_id(attempt.execution_plan().request_id.as_str()),
                candidate_id = ?attempt.execution_plan().candidate_id,
                provider_id = %attempt.execution_plan().provider_id,
                endpoint_id = %attempt.execution_plan().endpoint_id,
                key_id = %attempt.execution_plan().key_id,
                skip_reason = skip_reason,
                "gateway skipped local sync candidate after same request triggered risk-control avoidance"
            );
            return Ok(None);
        }
        let result = execute_execution_runtime_sync(
            self.state,
            self.parts.uri.path(),
            attempt.execution_plan().clone(),
            self.trace_id,
            self.decision,
            self.plan_kind,
            attempt.report_kind(),
            report_context,
        )
        .await;
        let mut response = match result {
            Ok(response) => response,
            Err(err) => {
                apply_local_execution_effect(
                    self.state,
                    LocalExecutionEffectContext {
                        plan: attempt.execution_plan(),
                        report_context: attempt.report_context().as_ref(),
                    },
                    LocalExecutionEffect::PoolAttemptAborted,
                )
                .await;
                return Err(err);
            }
        };
        if response.is_none() {
            apply_local_execution_effect(
                self.state,
                LocalExecutionEffectContext {
                    plan: attempt.execution_plan(),
                    report_context: attempt.report_context().as_ref(),
                },
                LocalExecutionEffect::PoolAttemptAborted,
            )
            .await;
            record_provider_session_risk_control_block_if_needed(
                self.state,
                &self.provider_session_risk_control_blocks,
                attempt.execution_plan(),
            )
            .await;
            record_pool_sticky_collateral_block_after_failure_if_needed(
                self.state,
                &self.pool_sticky_collateral_blocks,
                attempt.execution_plan(),
                attempt.report_context().as_ref(),
            )
            .await;
        }
        if let Some(response) = response.as_mut() {
            record_provider_session_risk_control_block_if_needed(
                self.state,
                &self.provider_session_risk_control_blocks,
                attempt.execution_plan(),
            )
            .await;
            record_pool_sticky_collateral_block_after_failure_if_needed(
                self.state,
                &self.pool_sticky_collateral_blocks,
                attempt.execution_plan(),
                attempt.report_context().as_ref(),
            )
            .await;
            if let Some(guard) = sticky_init_cleanup.as_mut() {
                guard.disarm();
            }
            attach_redaction_execution_candidate(
                response,
                attempt.execution_plan().candidate_id.as_deref(),
            );
        }
        Ok(response)
    }

    async fn mark_unused_attempts(&self, attempts: Vec<T>) -> Result<(), Self::Error> {
        mark_unused_local_candidates(self.state, attempts).await;
        Ok(())
    }

    async fn build_exhaustion(
        &self,
        last_plan: aether_contracts::ExecutionPlan,
        last_report_context: Option<serde_json::Value>,
    ) -> Result<Self::Exhaustion, Self::Error> {
        warn!(
            event_name = "candidate_loop_exhausted",
            log_type = "ops",
            trace_id = %self.trace_id,
            plan_kind = self.plan_kind,
            request_id = %short_request_id(last_plan.request_id.as_str()),
            candidate_id = ?last_plan.candidate_id,
            provider_name = last_plan.provider_name.as_deref().unwrap_or("-"),
            endpoint_id = %last_plan.endpoint_id,
            key_id = %last_plan.key_id,
            model_name = last_plan.model_name.as_deref().unwrap_or("-"),
            "candidate loop exhausted local sync candidates"
        );
        Ok(
            build_local_execution_exhaustion(self.state, &last_plan, last_report_context.as_ref())
                .await,
        )
    }
}

pub(crate) async fn execute_stream_plan_and_reports<T>(
    state: &AppState,
    trace_id: &str,
    decision: &GatewayControlDecision,
    plan_kind: &str,
    plan_and_reports: Vec<T>,
) -> Result<LocalExecutionRequestOutcome, GatewayError>
where
    T: AiExecutionAttempt + Send + Sync + 'static,
{
    let candidate_count = plan_and_reports.len();
    let first_provider = plan_and_reports
        .first()
        .and_then(|item| item.execution_plan().provider_name.as_deref())
        .unwrap_or("-")
        .to_string();
    let span = tracing::debug_span!(
        "candidates",
        trace_id = %trace_id,
        plan_kind,
        candidate_count,
    );

    async move {
        tracing::debug!(
            event_name = "candidate_loop_started",
            log_type = "event",
            trace_id = %trace_id,
            plan_kind,
            candidate_count,
            first_provider = first_provider.as_str(),
            "candidate loop started"
        );

        let port = StreamAttemptLoopPort {
            state,
            trace_id,
            decision,
            plan_kind,
            provider_session_risk_control_blocks: new_provider_session_risk_control_blocks(),
            pool_sticky_collateral_blocks: new_pool_sticky_collateral_blocks(),
        };
        match run_ai_attempt_loop(&port, plan_and_reports).await? {
            AiAttemptLoopOutcome::Responded(response) => {
                Ok(LocalExecutionRequestOutcome::responded(response))
            }
            AiAttemptLoopOutcome::Exhausted(exhaustion) => {
                Ok(LocalExecutionRequestOutcome::Exhausted(exhaustion))
            }
            AiAttemptLoopOutcome::NoPath => Ok(LocalExecutionRequestOutcome::NoPath),
        }
    }
    .instrument(span)
    .await
}

pub(crate) async fn execute_stream_attempt_source<T, S>(
    state: &AppState,
    trace_id: &str,
    decision: &GatewayControlDecision,
    plan_kind: &str,
    mut source: S,
) -> Result<LocalExecutionRequestOutcome, GatewayError>
where
    T: AiExecutionAttempt + Send + Sync + 'static,
    S: LocalExecutionAttemptSource<T>,
{
    let span = tracing::debug_span!("candidates", trace_id = %trace_id, plan_kind);

    async move {
        tracing::debug!(
            event_name = "candidate_loop_started",
            log_type = "event",
            trace_id = %trace_id,
            plan_kind,
            "dynamic candidate loop started"
        );

        let port = StreamAttemptLoopPort {
            state,
            trace_id,
            decision,
            plan_kind,
            provider_session_risk_control_blocks: new_provider_session_risk_control_blocks(),
            pool_sticky_collateral_blocks: new_pool_sticky_collateral_blocks(),
        };
        run_dynamic_attempt_loop(
            &port,
            &mut source,
            trace_id,
            plan_kind,
            state
                .frontdoor_runtime_guards
                .local_execution_planning_timeout,
        )
        .await
    }
    .instrument(span)
    .await
}

async fn run_dynamic_attempt_loop<Port, Source, Attempt>(
    port: &Port,
    source: &mut Source,
    trace_id: &str,
    plan_kind: &str,
    planning_timeout: Duration,
) -> Result<LocalExecutionRequestOutcome, GatewayError>
where
    Port: AiAttemptLoopPort<
        Attempt,
        Response = Response<Body>,
        Exhaustion = crate::executor::LocalExecutionExhaustion,
        Error = GatewayError,
    >,
    Source: LocalExecutionAttemptSource<Attempt>,
    Attempt: AiExecutionAttempt + Send + Sync + 'static,
{
    let mut last_attempted = None;

    while let Some(attempt) =
        next_execution_attempt_with_timeout(source, trace_id, plan_kind, planning_timeout).await?
    {
        last_attempted = Some((attempt.execution_plan().clone(), attempt.report_context()));
        if let Some(response) = port.execute_attempt(&attempt).await? {
            let remaining = source.drain_execution_attempts().await?;
            port.mark_unused_attempts(remaining).await?;
            return Ok(LocalExecutionRequestOutcome::responded(response));
        }
    }

    let Some((last_plan, last_report_context)) = last_attempted else {
        return Ok(LocalExecutionRequestOutcome::NoPath);
    };

    Ok(LocalExecutionRequestOutcome::Exhausted(
        port.build_exhaustion(last_plan, last_report_context)
            .await?,
    ))
}

async fn next_execution_attempt_with_timeout<Source, Attempt>(
    source: &mut Source,
    trace_id: &str,
    plan_kind: &str,
    planning_timeout: Duration,
) -> Result<Option<Attempt>, GatewayError>
where
    Source: LocalExecutionAttemptSource<Attempt>,
{
    match timeout(planning_timeout, source.next_execution_attempt()).await {
        Ok(result) => result,
        Err(_) => {
            let timeout_ms = planning_timeout.as_millis() as u64;
            warn!(
                event_name = "local_execution_candidate_planning_timeout",
                log_type = "ops",
                trace_id,
                plan_kind,
                timeout_ms,
                phase = "next_execution_attempt",
                "gateway timed out while planning the next local execution candidate"
            );
            Err(GatewayError::LocalExecutionPlanningTimeout {
                trace_id: trace_id.to_string(),
                phase: "next_execution_attempt",
                timeout_ms,
            })
        }
    }
}

struct StreamAttemptLoopPort<'a> {
    state: &'a AppState,
    trace_id: &'a str,
    decision: &'a GatewayControlDecision,
    plan_kind: &'a str,
    provider_session_risk_control_blocks:
        Arc<tokio::sync::Mutex<ProviderSessionRiskControlLoopBlocks>>,
    pool_sticky_collateral_blocks: Arc<tokio::sync::Mutex<PoolStickyCollateralLoopBlocks>>,
}

#[async_trait]
impl<T> AiAttemptLoopPort<T> for StreamAttemptLoopPort<'_>
where
    T: AiExecutionAttempt + Send + Sync + 'static,
{
    type Response = Response<Body>;
    type Exhaustion = crate::executor::LocalExecutionExhaustion;
    type Error = GatewayError;

    async fn execute_attempt(&self, attempt: &T) -> Result<Option<Self::Response>, Self::Error> {
        let plan = attempt.execution_plan().clone();
        let report_context = attempt.report_context();
        let context = LocalExecutionEffectContext {
            plan: &plan,
            report_context: report_context.as_ref(),
        };
        if let Some(skip_reason) = pool_sticky_collateral_skip_reason(
            self.state,
            &self.pool_sticky_collateral_blocks,
            &plan,
            report_context.as_ref(),
        )
        .await
        {
            apply_local_execution_effect(
                self.state,
                context,
                LocalExecutionEffect::PoolAttemptAborted,
            )
            .await;
            warn!(
                event_name = "local_stream_candidate_skipped_by_pool_sticky_collateral_avoidance",
                log_type = "event",
                trace_id = %self.trace_id,
                plan_kind = self.plan_kind,
                request_id = %short_request_id(plan.request_id.as_str()),
                candidate_id = ?plan.candidate_id,
                provider_id = %plan.provider_id,
                endpoint_id = %plan.endpoint_id,
                key_id = %plan.key_id,
                skip_reason = skip_reason,
                "gateway skipped local stream candidate after sticky pool collateral avoidance"
            );
            return Ok(None);
        }
        let mut sticky_init_cleanup = PoolAttemptStartCleanupGuard::new(self.state, context);
        let should_execute = prepare_pool_attempt_started_effect(self.state, context).await;
        if !should_execute {
            return Ok(None);
        }
        if let Some(skip_reason) = provider_session_risk_control_skip_reason(
            self.state,
            &self.provider_session_risk_control_blocks,
            &plan,
            report_context.as_ref(),
        )
        .await
        {
            apply_local_execution_effect(
                self.state,
                LocalExecutionEffectContext {
                    plan: &plan,
                    report_context: report_context.as_ref(),
                },
                LocalExecutionEffect::PoolAttemptAborted,
            )
            .await;
            warn!(
                event_name = "local_stream_candidate_skipped_by_provider_session_risk_control",
                log_type = "event",
                trace_id = %self.trace_id,
                plan_kind = self.plan_kind,
                request_id = %short_request_id(plan.request_id.as_str()),
                candidate_id = ?plan.candidate_id,
                provider_id = %plan.provider_id,
                endpoint_id = %plan.endpoint_id,
                key_id = %plan.key_id,
                skip_reason = skip_reason,
                "gateway skipped local stream candidate after same request triggered risk-control avoidance"
            );
            return Ok(None);
        }
        let candidate_index = parse_request_candidate_report_context(report_context.as_ref())
            .and_then(|context| context.candidate_index)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        debug!(
            event_name = "candidate_loop_attempt_started",
            log_type = "debug",
            trace_id = %self.trace_id,
            plan_kind = self.plan_kind,
            request_id = %short_request_id(plan.request_id.as_str()),
            candidate_id = ?plan.candidate_id,
            provider_name = plan.provider_name.as_deref().unwrap_or("-"),
            endpoint_id = %plan.endpoint_id,
            key_id = %plan.key_id,
            model_name = plan.model_name.as_deref().unwrap_or("-"),
            candidate_index = candidate_index.as_str(),
            "candidate loop attempting stream execution candidate"
        );
        let watchdog_plan = plan.clone();
        let watchdog_report_context = report_context.clone();
        let execution_state = self.state.clone();
        let execution_trace_id = self.trace_id.to_string();
        let execution_plan_kind = self.plan_kind.to_string();
        let execution_decision = self.decision.clone();
        let execution_report_kind = attempt.report_kind();
        let result = execute_stream_candidate_with_watchdog(
            self.state,
            self.trace_id,
            self.plan_kind,
            &watchdog_plan,
            watchdog_report_context.as_ref(),
            move || async move {
                execute_execution_runtime_stream(
                    &execution_state,
                    plan,
                    execution_trace_id.as_str(),
                    &execution_decision,
                    execution_plan_kind.as_str(),
                    execution_report_kind,
                    report_context,
                )
                .await
            },
        )
        .await;
        let mut response = match result {
            Ok(response) => response,
            Err(err) => {
                apply_local_execution_effect(
                    self.state,
                    LocalExecutionEffectContext {
                        plan: &watchdog_plan,
                        report_context: watchdog_report_context.as_ref(),
                    },
                    LocalExecutionEffect::PoolAttemptAborted,
                )
                .await;
                return Err(err);
            }
        };
        if response.is_none() {
            apply_local_execution_effect(
                self.state,
                LocalExecutionEffectContext {
                    plan: &watchdog_plan,
                    report_context: watchdog_report_context.as_ref(),
                },
                LocalExecutionEffect::PoolAttemptAborted,
            )
            .await;
            record_provider_session_risk_control_block_if_needed(
                self.state,
                &self.provider_session_risk_control_blocks,
                &watchdog_plan,
            )
            .await;
            record_pool_sticky_collateral_block_after_failure_if_needed(
                self.state,
                &self.pool_sticky_collateral_blocks,
                &watchdog_plan,
                watchdog_report_context.as_ref(),
            )
            .await;
        }
        if let Some(response) = response.as_mut() {
            record_provider_session_risk_control_block_if_needed(
                self.state,
                &self.provider_session_risk_control_blocks,
                &watchdog_plan,
            )
            .await;
            record_pool_sticky_collateral_block_after_failure_if_needed(
                self.state,
                &self.pool_sticky_collateral_blocks,
                &watchdog_plan,
                watchdog_report_context.as_ref(),
            )
            .await;
            if let Some(guard) = sticky_init_cleanup.as_mut() {
                guard.disarm();
            }
            attach_redaction_execution_candidate(response, watchdog_plan.candidate_id.as_deref());
        }
        Ok(response)
    }

    async fn mark_unused_attempts(&self, attempts: Vec<T>) -> Result<(), Self::Error> {
        mark_unused_local_candidates(self.state, attempts).await;
        Ok(())
    }

    async fn build_exhaustion(
        &self,
        last_plan: aether_contracts::ExecutionPlan,
        last_report_context: Option<serde_json::Value>,
    ) -> Result<Self::Exhaustion, Self::Error> {
        warn!(
            event_name = "candidate_loop_exhausted",
            log_type = "ops",
            trace_id = %self.trace_id,
            plan_kind = self.plan_kind,
            request_id = %short_request_id(last_plan.request_id.as_str()),
            candidate_id = ?last_plan.candidate_id,
            provider_name = last_plan.provider_name.as_deref().unwrap_or("-"),
            endpoint_id = %last_plan.endpoint_id,
            key_id = %last_plan.key_id,
            model_name = last_plan.model_name.as_deref().unwrap_or("-"),
            "candidate loop exhausted local stream candidates"
        );
        Ok(
            build_local_execution_exhaustion(self.state, &last_plan, last_report_context.as_ref())
                .await,
        )
    }
}

pub(crate) async fn mark_unused_local_candidates<T>(state: &AppState, remaining: Vec<T>)
where
    T: AiExecutionAttempt,
{
    for plan_and_report in remaining {
        let report_context = plan_and_report.report_context();
        let metadata =
            local_execution_candidate_metadata_from_report_context(report_context.as_ref());
        let plan = plan_and_report.execution_plan();
        if let Some(lease) = metadata.pool_key_lease.as_ref() {
            if let Err(err) =
                release_admin_provider_pool_key_lease(state.runtime_state.as_ref(), lease).await
            {
                warn!(
                    error = ?err,
                    "gateway candidate loop: failed to release unused pool key lease"
                );
            }
        }
        release_pool_sticky_initialization_for_owner(
            state.runtime_state.as_ref(),
            &plan.provider_id,
            &plan.key_id,
            pool_sticky_session_token_for_attempt(plan, report_context.as_ref()).as_deref(),
            metadata.pool_sticky_init_owner.as_deref(),
        )
        .await;
        if should_skip_unused_persistence_from_metadata(&metadata) {
            continue;
        }
        record_local_request_candidate_status(
            state,
            plan,
            report_context.as_ref(),
            SchedulerRequestCandidateStatusUpdate {
                status: RequestCandidateStatus::Unused,
                status_code: None,
                error_type: None,
                error_message: None,
                latency_ms: None,
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
        )
        .await;
    }
}

fn pool_sticky_session_token_for_attempt(
    plan: &aether_contracts::ExecutionPlan,
    report_context: Option<&serde_json::Value>,
) -> Option<String> {
    local_execution_candidate_metadata_from_report_context(report_context)
        .pool_sticky_session_token
        .or_else(|| {
            report_context
                .and_then(serde_json::Value::as_object)
                .and_then(|object| object.get("original_request_body"))
                .filter(|value| !value.is_null())
                .or(plan.body.json_body.as_ref())
                .and_then(crate::ai_serving::extract_pool_sticky_session_token)
        })
}

fn should_skip_unused_persistence(report_context: Option<&serde_json::Value>) -> bool {
    let metadata = local_execution_candidate_metadata_from_report_context(report_context);
    should_skip_unused_persistence_from_metadata(&metadata)
}

fn should_skip_unused_persistence_from_metadata(
    metadata: &crate::orchestration::LocalExecutionCandidateMetadata,
) -> bool {
    metadata.candidate_group_id.is_some() && metadata.pool_key_index.is_some()
}

fn resolve_stream_candidate_watchdog_timeout(
    plan: &aether_contracts::ExecutionPlan,
    _report_context: Option<&serde_json::Value>,
) -> Duration {
    let timeout_ms = plan
        .timeouts
        .as_ref()
        .and_then(|timeouts| timeouts.first_byte_ms)
        .unwrap_or(DEFAULT_STREAM_FIRST_BYTE_WATCHDOG_TIMEOUT_MS)
        .max(1);
    Duration::from_millis(timeout_ms)
}

fn stream_candidate_watchdog_timeout_message() -> &'static str {
    "Stream first byte timeout"
}

async fn execute_stream_candidate_with_watchdog<Fut>(
    state: &(impl RequestCandidateRuntimeWriter + ?Sized),
    trace_id: &str,
    plan_kind: &str,
    plan: &aether_contracts::ExecutionPlan,
    report_context: Option<&serde_json::Value>,
    execute: impl FnOnce() -> Fut,
) -> Result<Option<Response<Body>>, GatewayError>
where
    Fut:
        std::future::Future<Output = Result<Option<Response<Body>>, GatewayError>> + Send + 'static,
{
    let timeout_duration = resolve_stream_candidate_watchdog_timeout(plan, report_context);
    let candidate_started_unix_ms = current_unix_ms();
    let mut join_handle = tokio::spawn(execute());
    match timeout(timeout_duration, &mut join_handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_error)) => Err(GatewayError::Internal(format!(
            "local stream candidate task join failed: {join_error}"
        ))),
        Err(_) => {
            join_handle.abort();
            let finished_at_unix_ms = current_unix_ms();
            let request_id = short_request_id(plan.request_id.as_str());
            let provider_name = plan.provider_name.as_deref().unwrap_or("-");
            let model_name = plan.model_name.as_deref().unwrap_or("-");
            let candidate_index = parse_request_candidate_report_context(report_context)
                .and_then(|context| context.candidate_index)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let timeout_ms = u64::try_from(timeout_duration.as_millis()).unwrap_or(u64::MAX);
            record_local_request_candidate_status(
                state,
                plan,
                report_context,
                SchedulerRequestCandidateStatusUpdate {
                    status: RequestCandidateStatus::Failed,
                    status_code: Some(http::StatusCode::GATEWAY_TIMEOUT.as_u16()),
                    error_type: Some("local_stream_candidate_watchdog_timeout".to_string()),
                    error_message: Some(stream_candidate_watchdog_timeout_message().to_string()),
                    latency_ms: None,
                    started_at_unix_ms: Some(candidate_started_unix_ms),
                    finished_at_unix_ms: Some(finished_at_unix_ms),
                },
            )
            .await;
            warn!(
                event_name = "local_stream_candidate_watchdog_timed_out",
                log_type = "event",
                trace_id = %trace_id,
                plan_kind,
                request_id = %request_id,
                candidate_id = ?plan.candidate_id,
                provider_name,
                endpoint_id = %plan.endpoint_id,
                key_id = %plan.key_id,
                model_name,
                candidate_index = candidate_index.as_str(),
                timeout_ms,
                "gateway local stream candidate watchdog timed out"
            );
            Ok(None)
        }
    }
}

pub(crate) async fn mark_unused_local_candidate_items<T, FPlan, FContext>(
    state: &AppState,
    remaining: Vec<T>,
    plan: FPlan,
    report_context: FContext,
) where
    FPlan: Fn(&T) -> &aether_contracts::ExecutionPlan,
    FContext: Fn(&T) -> Option<&serde_json::Value>,
{
    for item in remaining {
        let report_context = report_context(&item);
        if should_skip_unused_persistence(report_context) {
            continue;
        }
        record_local_request_candidate_status(
            state,
            plan(&item),
            report_context,
            SchedulerRequestCandidateStatusUpdate {
                status: RequestCandidateStatus::Unused,
                status_code: None,
                error_type: None,
                error_message: None,
                latency_ms: None,
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aether_contracts::{ExecutionPlan, ExecutionTimeouts, RequestBody};
    use aether_data_contracts::repository::candidates::{
        RequestCandidateStatus, UpsertRequestCandidateRecord,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    struct TestRequestCandidateWriter {
        records: Mutex<Vec<UpsertRequestCandidateRecord>>,
    }

    #[async_trait]
    impl RequestCandidateRuntimeWriter for TestRequestCandidateWriter {
        fn has_request_candidate_data_writer(&self) -> bool {
            true
        }

        async fn upsert_request_candidate(
            &self,
            candidate: UpsertRequestCandidateRecord,
        ) -> Result<
            Option<aether_data_contracts::repository::candidates::StoredRequestCandidate>,
            GatewayError,
        > {
            self.records.lock().await.push(candidate);
            Ok(None)
        }
    }

    struct PendingAttemptSource;

    #[async_trait]
    impl LocalExecutionAttemptSource<()> for PendingAttemptSource {
        async fn next_execution_attempt(&mut self) -> Result<Option<()>, GatewayError> {
            std::future::pending::<()>().await;
            Ok(None)
        }

        async fn drain_execution_attempts(&mut self) -> Result<Vec<()>, GatewayError> {
            Ok(Vec::new())
        }
    }

    fn test_plan(timeouts: Option<ExecutionTimeouts>) -> ExecutionPlan {
        ExecutionPlan {
            request_id: "req_watchdog".to_string(),
            candidate_id: Some("cand_watchdog".to_string()),
            provider_name: Some("provider".to_string()),
            provider_id: "provider_id".to_string(),
            endpoint_id: "endpoint_id".to_string(),
            key_id: "key_id".to_string(),
            method: "POST".to_string(),
            url: "https://example.com/v1/messages".to_string(),
            headers: Default::default(),
            content_type: Some("application/json".to_string()),
            content_encoding: None,
            body: RequestBody::from_json(json!({"model": "gpt-test"})),
            stream: true,
            client_api_format: "claude:messages".to_string(),
            provider_api_format: "openai:chat".to_string(),
            model_name: Some("gpt-test".to_string()),
            proxy: None,
            transport_profile: None,
            timeouts,
        }
    }

    #[tokio::test]
    async fn next_execution_attempt_times_out_instead_of_waiting_forever() {
        let mut source = PendingAttemptSource;

        let err = next_execution_attempt_with_timeout(
            &mut source,
            "trace-planning-timeout",
            "openai_responses_sync",
            Duration::from_millis(5),
        )
        .await
        .expect_err("pending candidate planning should time out");

        match err {
            GatewayError::LocalExecutionPlanningTimeout {
                trace_id,
                phase,
                timeout_ms,
            } => {
                assert_eq!(trace_id, "trace-planning-timeout");
                assert_eq!(phase, "next_execution_attempt");
                assert_eq!(timeout_ms, 5);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn test_report_context() -> serde_json::Value {
        json!({
            "request_id": "req_watchdog",
            "candidate_id": "cand_watchdog",
            "candidate_index": 2,
            "retry_index": 0,
            "user_id": "user_1",
            "api_key_id": "api_key_1",
        })
    }

    #[test]
    fn stream_candidate_watchdog_prefers_first_byte_timeout() {
        let report_context = json!({"upstream_is_stream": true});
        let timeout = resolve_stream_candidate_watchdog_timeout(
            &test_plan(Some(ExecutionTimeouts {
                first_byte_ms: Some(12_345),
                total_ms: Some(90_000),
                ..ExecutionTimeouts::default()
            })),
            Some(&report_context),
        );

        assert_eq!(timeout, Duration::from_millis(12_345));
    }

    #[test]
    fn stream_candidate_watchdog_uses_default_when_timeouts_missing() {
        let timeout = resolve_stream_candidate_watchdog_timeout(&test_plan(None), None);

        assert_eq!(
            timeout,
            Duration::from_millis(DEFAULT_STREAM_FIRST_BYTE_WATCHDOG_TIMEOUT_MS)
        );
    }

    #[test]
    fn pool_sticky_collateral_failure_detects_account_invalid_statuses() {
        assert!(pool_sticky_collateral_failure_status_is_account_invalid(
            401, None
        ));
        assert!(pool_sticky_collateral_failure_status_is_account_invalid(
            403,
            Some("forbidden")
        ));
        assert!(pool_sticky_collateral_failure_status_is_account_invalid(
            400,
            Some("invalid token")
        ));
        assert!(pool_sticky_collateral_failure_status_is_account_invalid(
            400,
            Some("account suspended")
        ));
    }

    #[test]
    fn pool_sticky_collateral_failure_ignores_quota_and_cooldown_statuses() {
        assert!(!pool_sticky_collateral_failure_status_is_account_invalid(
            402,
            Some("quota exceeded")
        ));
        assert!(!pool_sticky_collateral_failure_status_is_account_invalid(
            429,
            Some("rate limited")
        ));
        assert!(!pool_sticky_collateral_failure_status_is_account_invalid(
            503,
            Some("upstream overloaded")
        ));
    }

    #[test]
    fn stream_candidate_watchdog_ignores_total_timeout_for_stream_upstream() {
        let report_context = json!({"upstream_is_stream": true});
        let timeout = resolve_stream_candidate_watchdog_timeout(
            &test_plan(Some(ExecutionTimeouts {
                total_ms: Some(90_000),
                ..ExecutionTimeouts::default()
            })),
            Some(&report_context),
        );

        assert_eq!(
            timeout,
            Duration::from_millis(DEFAULT_STREAM_FIRST_BYTE_WATCHDOG_TIMEOUT_MS)
        );
    }

    #[test]
    fn stream_candidate_watchdog_prefers_first_byte_timeout_when_upstream_non_stream() {
        let report_context = json!({"upstream_is_stream": false});
        let timeout = resolve_stream_candidate_watchdog_timeout(
            &test_plan(Some(ExecutionTimeouts {
                first_byte_ms: Some(12_345),
                total_ms: Some(599_000),
                ..ExecutionTimeouts::default()
            })),
            Some(&report_context),
        );

        assert_eq!(timeout, Duration::from_millis(12_345));
    }

    #[test]
    fn stream_candidate_watchdog_ignores_total_timeout_when_upstream_non_stream() {
        let report_context = json!({"upstream_is_stream": false});
        let timeout = resolve_stream_candidate_watchdog_timeout(
            &test_plan(Some(ExecutionTimeouts {
                total_ms: Some(599_000),
                ..ExecutionTimeouts::default()
            })),
            Some(&report_context),
        );

        assert_eq!(
            timeout,
            Duration::from_millis(DEFAULT_STREAM_FIRST_BYTE_WATCHDOG_TIMEOUT_MS)
        );
    }

    #[test]
    fn stream_candidate_watchdog_defaults_to_streaming_when_flag_missing() {
        let report_context = json!({});
        let timeout = resolve_stream_candidate_watchdog_timeout(
            &test_plan(Some(ExecutionTimeouts {
                first_byte_ms: Some(12_345),
                total_ms: Some(90_000),
                ..ExecutionTimeouts::default()
            })),
            Some(&report_context),
        );

        assert_eq!(timeout, Duration::from_millis(12_345));
    }

    #[test]
    fn unused_persistence_skips_pool_internal_candidates() {
        assert!(should_skip_unused_persistence(Some(&json!({
            "candidate_group_id": "pool-group",
            "pool_key_index": 0,
        }))));
        assert!(should_skip_unused_persistence(Some(&json!({
            "candidate_group_id": "pool-group",
            "pool_key_index": 1,
        }))));
        assert!(!should_skip_unused_persistence(Some(&json!({
            "candidate_group_id": "pool-group",
        }))));
        assert!(!should_skip_unused_persistence(Some(&json!({
            "candidate_index": 1,
        }))));
    }

    #[test]
    fn pool_sticky_session_token_for_attempt_prefers_report_context_metadata() {
        let plan = test_plan(None);
        let report_context = json!({
            "pool_sticky_session_token": "metadata-session",
            "original_request_body": {
                "model": "gpt-test",
                "session_id": "body-session"
            }
        });

        assert_eq!(
            pool_sticky_session_token_for_attempt(&plan, Some(&report_context)).as_deref(),
            Some("metadata-session")
        );
    }

    #[tokio::test]
    async fn stream_candidate_watchdog_marks_failed_candidate_and_continues() {
        let writer = Arc::new(TestRequestCandidateWriter::default());
        let plan = test_plan(Some(ExecutionTimeouts {
            first_byte_ms: Some(25),
            ..ExecutionTimeouts::default()
        }));
        let report_context = test_report_context();
        let writer_for_task = writer.clone();

        let task = tokio::spawn(async move {
            execute_stream_candidate_with_watchdog(
                writer_for_task.as_ref(),
                "trace_watchdog",
                "claude_cli_stream",
                &plan,
                Some(&report_context),
                || std::future::pending::<Result<Option<Response<Body>>, GatewayError>>(),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(40)).await;
        let result = task.await.expect("watchdog task should join");
        assert!(matches!(result, Ok(None)));

        let records = writer.records.lock().await;
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.status, RequestCandidateStatus::Failed);
        assert_eq!(
            record.status_code,
            Some(http::StatusCode::GATEWAY_TIMEOUT.as_u16())
        );
        assert_eq!(
            record.error_type.as_deref(),
            Some("local_stream_candidate_watchdog_timeout")
        );
        assert!(record
            .error_message
            .as_deref()
            .is_some_and(|message| message == "Stream first byte timeout"));
        assert_eq!(record.candidate_index, 2);
    }
}
