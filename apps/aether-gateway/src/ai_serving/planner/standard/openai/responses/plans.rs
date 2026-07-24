use async_trait::async_trait;
use tracing::warn;

use super::decision::{
    build_batched_local_openai_responses_candidate_attempt_source,
    build_local_openai_responses_candidate_attempt_source,
    maybe_build_local_openai_responses_codex_ws_planning_attempt,
    maybe_build_local_openai_responses_decision_payload_for_candidate,
    openai_search_body_requires_bound_affinity, resolve_local_openai_responses_decision_input,
    resolve_local_openai_responses_decision_input_with_required_capabilities,
    LocalOpenAiResponsesCandidateAttempt, LocalOpenAiResponsesCandidateAttemptSource,
    LocalOpenAiResponsesDecisionInput, LocalOpenAiResponsesSpec,
};
use crate::ai_serving::planner::candidate_materialization::{
    local_candidate_attempt_has_sticky_init_owner, release_pool_sticky_init_for_unbuilt_attempt,
    LocalExecutionAttemptSource,
};
use crate::ai_serving::planner::plan_builders::{
    build_openai_responses_stream_plan_from_decision,
    build_openai_responses_sync_plan_from_decision, stream_attempt_has_sticky_init_owner,
    sync_attempt_has_sticky_init_owner, AiStreamAttempt, AiSyncAttempt,
};
use crate::ai_serving::planner::redaction::codex_ws_pii_redaction_required;
use crate::ai_serving::planner::runtime_miss::{
    apply_local_runtime_candidate_evaluation_progress,
    apply_local_runtime_candidate_terminal_reason, set_local_runtime_miss_diagnostic_reason,
};
use crate::ai_serving::planner::spec_metadata::local_openai_responses_spec_metadata;
use crate::ai_serving::GatewayControlDecision;
pub(crate) use crate::ai_serving::{
    resolve_openai_responses_stream_spec as resolve_stream_spec,
    resolve_openai_responses_sync_spec as resolve_sync_spec,
};
use crate::{AppState, GatewayError};

use super::CodexWsPlanningAttempt;

pub(crate) struct LocalOpenAiResponsesSyncAttemptSource<'a> {
    state: &'a AppState,
    parts: &'a http::request::Parts,
    trace_id: &'a str,
    body_json: serde_json::Value,
    input: LocalOpenAiResponsesDecisionInput,
    spec: LocalOpenAiResponsesSpec,
    candidates: LocalOpenAiResponsesCandidateAttemptSource<'a>,
}

pub(crate) struct LocalOpenAiResponsesStreamAttemptSource<'a> {
    state: &'a AppState,
    parts: &'a http::request::Parts,
    trace_id: &'a str,
    body_json: serde_json::Value,
    input: LocalOpenAiResponsesDecisionInput,
    spec: LocalOpenAiResponsesSpec,
    candidates: LocalOpenAiResponsesCandidateAttemptSource<'a>,
}

pub(super) async fn build_local_sync_attempt_source<'a>(
    state: &'a AppState,
    parts: &'a http::request::Parts,
    trace_id: &'a str,
    decision: &'a GatewayControlDecision,
    body_json: &'a serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
) -> Result<Option<(LocalOpenAiResponsesSyncAttemptSource<'a>, usize)>, GatewayError> {
    let spec_metadata = local_openai_responses_spec_metadata(spec);
    let Some(input) = resolve_local_openai_responses_decision_input(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        spec_metadata.decision_kind,
    )
    .await?
    else {
        return Ok(None);
    };
    set_local_runtime_miss_diagnostic_reason(
        state,
        trace_id,
        decision,
        spec_metadata.decision_kind,
        Some(input.requested_model.as_str()),
        "candidate_evaluation_incomplete",
    );
    let effective_body_json = input.effective_body_json(body_json).clone();
    let (candidates, candidate_count) = build_local_openai_responses_candidate_attempt_source(
        state,
        trace_id,
        &input,
        &effective_body_json,
        spec,
    )
    .await?;
    apply_local_runtime_candidate_evaluation_progress(state, trace_id, candidate_count);
    if candidate_count == 0 {
        if spec.companion_search && openai_search_body_requires_bound_affinity(&effective_body_json)
        {
            return Err(GatewayError::Client {
                status: http::StatusCode::CONFLICT,
                message: "search_session_affinity_lost: the bound search session candidate is unavailable".to_string(),
            });
        }
        return Ok(None);
    }

    Ok(Some((
        LocalOpenAiResponsesSyncAttemptSource {
            state,
            parts,
            trace_id,
            body_json: effective_body_json,
            input,
            spec,
            candidates,
        },
        candidate_count,
    )))
}

pub(super) async fn build_local_stream_attempt_source<'a>(
    state: &'a AppState,
    parts: &'a http::request::Parts,
    trace_id: &'a str,
    decision: &'a GatewayControlDecision,
    body_json: &'a serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
) -> Result<Option<(LocalOpenAiResponsesStreamAttemptSource<'a>, usize)>, GatewayError> {
    let spec_metadata = local_openai_responses_spec_metadata(spec);
    let Some(input) = resolve_local_openai_responses_decision_input(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        spec_metadata.decision_kind,
    )
    .await?
    else {
        return Ok(None);
    };
    set_local_runtime_miss_diagnostic_reason(
        state,
        trace_id,
        decision,
        spec_metadata.decision_kind,
        Some(input.requested_model.as_str()),
        "candidate_evaluation_incomplete",
    );
    let effective_body_json = input.effective_body_json(body_json).clone();
    let (candidates, candidate_count) = build_local_openai_responses_candidate_attempt_source(
        state,
        trace_id,
        &input,
        &effective_body_json,
        spec,
    )
    .await?;
    apply_local_runtime_candidate_evaluation_progress(state, trace_id, candidate_count);
    if candidate_count == 0 {
        return Ok(None);
    }

    Ok(Some((
        LocalOpenAiResponsesStreamAttemptSource {
            state,
            parts,
            trace_id,
            body_json: effective_body_json,
            input,
            spec,
            candidates,
        },
        candidate_count,
    )))
}

#[async_trait]
impl LocalExecutionAttemptSource<AiSyncAttempt> for LocalOpenAiResponsesSyncAttemptSource<'_> {
    async fn next_execution_attempt(&mut self) -> Result<Option<AiSyncAttempt>, GatewayError> {
        while let Some(attempt) = self.candidates.next_attempt().await? {
            let cleanup_attempt = attempt.clone();
            let mut sticky_init_cleanup = attempt.pool_sticky_init_cleanup_guard(self.state);
            let built_attempt = match self.build_sync_attempt(attempt).await {
                Ok(value) => value,
                Err(err) => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                    return Err(err);
                }
            };
            match built_attempt {
                Some(attempt) => {
                    if let Some(guard) = sticky_init_cleanup.as_mut() {
                        guard.disarm();
                    }
                    return Ok(Some(attempt));
                }
                None => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                    continue;
                }
            }
        }
        apply_local_runtime_candidate_terminal_reason(
            self.state,
            self.trace_id,
            "no_local_sync_plans",
        );
        if self.spec.companion_search && openai_search_body_requires_bound_affinity(&self.body_json)
        {
            return Err(GatewayError::Client {
                status: http::StatusCode::CONFLICT,
                message: "search_session_affinity_lost: the bound search session candidate is unavailable".to_string(),
            });
        }
        Ok(None)
    }

    async fn drain_execution_attempts(&mut self) -> Result<Vec<AiSyncAttempt>, GatewayError> {
        let mut drained = Vec::new();
        for attempt in self.candidates.drain_static_attempts() {
            let cleanup_attempt = attempt.clone();
            let mut sticky_init_cleanup = attempt.pool_sticky_init_cleanup_guard(self.state);
            match self.build_sync_attempt(attempt).await {
                Ok(Some(attempt)) => drained.push(attempt),
                Ok(None) => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                }
                Err(err) => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                    return Err(err);
                }
            }
        }
        Ok(drained)
    }
}

#[async_trait]
impl LocalExecutionAttemptSource<AiStreamAttempt> for LocalOpenAiResponsesStreamAttemptSource<'_> {
    async fn next_execution_attempt(&mut self) -> Result<Option<AiStreamAttempt>, GatewayError> {
        while let Some(attempt) = self.candidates.next_attempt().await? {
            let cleanup_attempt = attempt.clone();
            let mut sticky_init_cleanup = attempt.pool_sticky_init_cleanup_guard(self.state);
            let built_attempt = match self.build_stream_attempt(attempt).await {
                Ok(value) => value,
                Err(err) => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                    return Err(err);
                }
            };
            match built_attempt {
                Some(attempt) => {
                    if let Some(guard) = sticky_init_cleanup.as_mut() {
                        guard.disarm();
                    }
                    return Ok(Some(attempt));
                }
                None => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                    continue;
                }
            }
        }
        apply_local_runtime_candidate_terminal_reason(
            self.state,
            self.trace_id,
            "no_local_stream_plans",
        );
        Ok(None)
    }

    async fn drain_execution_attempts(&mut self) -> Result<Vec<AiStreamAttempt>, GatewayError> {
        let mut drained = Vec::new();
        for attempt in self.candidates.drain_static_attempts() {
            let cleanup_attempt = attempt.clone();
            let mut sticky_init_cleanup = attempt.pool_sticky_init_cleanup_guard(self.state);
            match self.build_stream_attempt(attempt).await {
                Ok(Some(attempt)) => drained.push(attempt),
                Ok(None) => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                }
                Err(err) => {
                    release_pool_sticky_init_for_unbuilt_attempt(self.state, &cleanup_attempt)
                        .await;
                    return Err(err);
                }
            }
        }
        Ok(drained)
    }
}

impl LocalOpenAiResponsesSyncAttemptSource<'_> {
    async fn build_sync_attempt(
        &self,
        attempt: LocalOpenAiResponsesCandidateAttempt,
    ) -> Result<Option<AiSyncAttempt>, GatewayError> {
        let Some(payload) = maybe_build_local_openai_responses_decision_payload_for_candidate(
            self.state,
            self.parts,
            self.trace_id,
            &self.body_json,
            &self.input,
            attempt,
            self.spec,
        )
        .await?
        else {
            return Ok(None);
        };

        match build_openai_responses_sync_plan_from_decision(
            self.parts,
            &self.body_json,
            payload,
            self.spec.compact,
        ) {
            Ok(value) => Ok(value),
            Err(err) => {
                warn!(
                    trace_id = %self.trace_id,
                    error = ?err,
                    "gateway local openai responses sync decision plan build failed"
                );
                Ok(None)
            }
        }
    }
}

impl LocalOpenAiResponsesStreamAttemptSource<'_> {
    async fn build_stream_attempt(
        &self,
        attempt: LocalOpenAiResponsesCandidateAttempt,
    ) -> Result<Option<AiStreamAttempt>, GatewayError> {
        let Some(payload) = maybe_build_local_openai_responses_decision_payload_for_candidate(
            self.state,
            self.parts,
            self.trace_id,
            &self.body_json,
            &self.input,
            attempt,
            self.spec,
        )
        .await?
        else {
            return Ok(None);
        };

        match build_openai_responses_stream_plan_from_decision(
            self.parts,
            &self.body_json,
            payload,
            self.spec.compact,
        ) {
            Ok(value) => Ok(value),
            Err(err) => {
                warn!(
                    trace_id = %self.trace_id,
                    error = ?err,
                    "gateway local openai responses stream decision plan build failed"
                );
                Ok(None)
            }
        }
    }
}

pub(super) async fn build_local_sync_plan_and_reports(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
) -> Result<Vec<AiSyncAttempt>, GatewayError> {
    let spec_metadata = local_openai_responses_spec_metadata(spec);
    let Some(input) = resolve_local_openai_responses_decision_input(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        spec_metadata.decision_kind,
    )
    .await?
    else {
        return Ok(Vec::new());
    };
    set_local_runtime_miss_diagnostic_reason(
        state,
        trace_id,
        decision,
        spec_metadata.decision_kind,
        Some(input.requested_model.as_str()),
        "candidate_evaluation_incomplete",
    );

    let (mut source, candidate_count) = build_local_openai_responses_candidate_attempt_source(
        state, trace_id, &input, body_json, spec,
    )
    .await?;
    apply_local_runtime_candidate_evaluation_progress(state, trace_id, candidate_count);
    if candidate_count == 0 {
        return Ok(Vec::new());
    }

    let mut plans = Vec::new();
    while let Some(attempt) = source.next_attempt().await? {
        let sticky_init_attempt = local_candidate_attempt_has_sticky_init_owner(&attempt);
        let cleanup_attempt = attempt.clone();
        let payload = match maybe_build_local_openai_responses_decision_payload_for_candidate(
            state, parts, trace_id, body_json, &input, attempt, spec,
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                return Err(err);
            }
        };
        let Some(payload) = payload else {
            release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
            continue;
        };

        match build_openai_responses_sync_plan_from_decision(
            parts,
            body_json,
            payload,
            spec.compact,
        ) {
            Ok(Some(value)) => {
                let stop_after_value = sync_attempt_has_sticky_init_owner(&value);
                if sticky_init_attempt && !stop_after_value {
                    release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                }
                plans.push(value);
                if stop_after_value {
                    break;
                }
            }
            Ok(None) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
            }
            Err(err) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                warn!(
                    trace_id = %trace_id,
                    api_format = spec_metadata.api_format,
                    error = ?err,
                    "gateway local openai responses sync decision plan build failed"
                );
            }
        }
    }

    apply_local_runtime_candidate_terminal_reason(state, trace_id, "no_local_sync_plans");
    Ok(plans)
}

pub(super) async fn build_local_stream_plan_and_reports(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
) -> Result<Vec<AiStreamAttempt>, GatewayError> {
    build_local_stream_plan_and_reports_with_required_capabilities(
        state, parts, trace_id, decision, body_json, spec, None,
    )
    .await
}

pub(super) async fn build_local_stream_plan_and_reports_with_required_capabilities(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
    explicit_required_capabilities: Option<&serde_json::Value>,
) -> Result<Vec<AiStreamAttempt>, GatewayError> {
    build_local_stream_plan_and_reports_with_required_capabilities_mode(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        spec,
        explicit_required_capabilities,
        false,
    )
    .await
}

pub(super) async fn build_local_stream_plan_and_reports_with_required_capabilities_compact<
    Preflight,
    PreflightFn,
    PreflightFuture,
>(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
    explicit_required_capabilities: Option<&serde_json::Value>,
    max_materialized_attempts: usize,
    preflight: PreflightFn,
) -> Result<Vec<CodexWsPlanningAttempt<Preflight>>, GatewayError>
where
    PreflightFn: FnMut(LocalOpenAiResponsesCandidateAttempt) -> PreflightFuture,
    PreflightFuture: std::future::Future<Output = Option<Preflight>>,
{
    let spec_metadata = local_openai_responses_spec_metadata(spec);
    let Some(input) = resolve_local_openai_responses_decision_input_with_required_capabilities(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        spec_metadata.decision_kind,
        explicit_required_capabilities,
    )
    .await?
    else {
        return Ok(Vec::new());
    };
    set_local_runtime_miss_diagnostic_reason(
        state,
        trace_id,
        decision,
        spec_metadata.decision_kind,
        Some(input.requested_model.as_str()),
        "candidate_evaluation_incomplete",
    );
    if codex_ws_pii_redaction_required(state, &input.auth_context, spec_metadata.api_format).await?
    {
        set_local_runtime_miss_diagnostic_reason(
            state,
            trace_id,
            decision,
            spec_metadata.decision_kind,
            Some(input.requested_model.as_str()),
            "codex_ws_pii_redaction_requires_http_path",
        );
        return Err(GatewayError::Client {
            status: http::StatusCode::PRECONDITION_FAILED,
            message: "Codex WebSocket is unavailable when chat PII redaction is enabled"
                .to_string(),
        });
    }
    // Routing may have produced one request-scoped effective body. Borrow it
    // throughout candidate fanout; do not clone it into each candidate plan.
    let effective_body_json = input.effective_body_json(body_json);
    let (mut source, candidate_count) =
        build_batched_local_openai_responses_candidate_attempt_source(
            state,
            trace_id,
            &input,
            effective_body_json,
            spec,
        )
        .await?;
    apply_local_runtime_candidate_evaluation_progress(state, trace_id, candidate_count);

    let mut preflight = preflight;
    let mut selected =
        std::collections::VecDeque::with_capacity(max_materialized_attempts.min(candidate_count));
    while selected.len() < max_materialized_attempts {
        let Some(attempt) = source.next_attempt().await? else {
            break;
        };
        match preflight(attempt.clone()).await {
            Some(preflight) => selected.push_back((attempt, preflight)),
            None => release_pool_sticky_init_for_unbuilt_attempt(state, &attempt).await,
        }
    }
    for deferred in source.drain_static_attempts() {
        release_pool_sticky_init_for_unbuilt_attempt(state, &deferred).await;
    }

    let mut plans = Vec::with_capacity(selected.len());
    while let Some((attempt, preflight)) = selected.pop_front() {
        let sticky_init_attempt = local_candidate_attempt_has_sticky_init_owner(&attempt);
        let cleanup_attempt = attempt.clone();
        let planning_attempt = match maybe_build_local_openai_responses_codex_ws_planning_attempt(
            state,
            parts,
            trace_id,
            effective_body_json,
            &input,
            attempt,
            spec,
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                release_preflight_attempts(state, &mut selected).await;
                return Err(err);
            }
        };
        let Some((value, provider_body_patch)) = planning_attempt else {
            release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
            continue;
        };
        let stop_after_value = stream_attempt_has_sticky_init_owner(&value);
        if sticky_init_attempt && !stop_after_value {
            release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
        }
        plans.push(CodexWsPlanningAttempt {
            attempt: value,
            preflight,
            provider_body_patch,
        });
        if stop_after_value {
            release_preflight_attempts(state, &mut selected).await;
            break;
        }
    }
    apply_local_runtime_candidate_terminal_reason(state, trace_id, "no_local_stream_plans");
    Ok(plans)
}

async fn release_preflight_attempts<Preflight>(
    state: &AppState,
    selected: &mut std::collections::VecDeque<(LocalOpenAiResponsesCandidateAttempt, Preflight)>,
) {
    while let Some((attempt, _)) = selected.pop_front() {
        release_pool_sticky_init_for_unbuilt_attempt(state, &attempt).await;
    }
}

async fn build_local_stream_plan_and_reports_with_required_capabilities_mode(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    spec: LocalOpenAiResponsesSpec,
    explicit_required_capabilities: Option<&serde_json::Value>,
    compact_attempts: bool,
) -> Result<Vec<AiStreamAttempt>, GatewayError> {
    let spec_metadata = local_openai_responses_spec_metadata(spec);
    let Some(input) = resolve_local_openai_responses_decision_input_with_required_capabilities(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        spec_metadata.decision_kind,
        explicit_required_capabilities,
    )
    .await?
    else {
        return Ok(Vec::new());
    };
    set_local_runtime_miss_diagnostic_reason(
        state,
        trace_id,
        decision,
        spec_metadata.decision_kind,
        Some(input.requested_model.as_str()),
        "candidate_evaluation_incomplete",
    );

    let (mut source, candidate_count) = build_local_openai_responses_candidate_attempt_source(
        state, trace_id, &input, body_json, spec,
    )
    .await?;
    apply_local_runtime_candidate_evaluation_progress(state, trace_id, candidate_count);
    if candidate_count == 0 {
        return Ok(Vec::new());
    }

    let mut plans = Vec::new();
    while let Some(attempt) = source.next_attempt().await? {
        let sticky_init_attempt = local_candidate_attempt_has_sticky_init_owner(&attempt);
        let cleanup_attempt = attempt.clone();
        let payload = match maybe_build_local_openai_responses_decision_payload_for_candidate(
            state, parts, trace_id, body_json, &input, attempt, spec,
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                return Err(err);
            }
        };
        let Some(payload) = payload else {
            release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
            continue;
        };

        match build_openai_responses_stream_plan_from_decision(
            parts,
            body_json,
            payload,
            spec.compact,
        ) {
            Ok(Some(mut value)) => {
                let stop_after_value = stream_attempt_has_sticky_init_owner(&value);
                if sticky_init_attempt && !stop_after_value {
                    release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                }
                if compact_attempts {
                    value.plan = crate::codex_ws::compact_ws_planning_attempt_plan(&value.plan);
                    value.report_context = crate::codex_ws::compact_report_context_template(
                        value.report_context.as_ref(),
                    );
                }
                plans.push(value);
                if stop_after_value {
                    break;
                }
            }
            Ok(None) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
            }
            Err(err) => {
                release_pool_sticky_init_for_unbuilt_attempt(state, &cleanup_attempt).await;
                warn!(
                    trace_id = %trace_id,
                    api_format = spec_metadata.api_format,
                    error = ?err,
                    "gateway local openai responses stream decision plan build failed"
                );
            }
        }
    }

    apply_local_runtime_candidate_terminal_reason(state, trace_id, "no_local_stream_plans");
    Ok(plans)
}
