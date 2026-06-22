use crate::ai_serving::{AiExecutionDecision, AiExecutionPlanPayload, GatewayControlDecision};
use crate::{AppState, GatewayError};

mod candidate_affinity_cache;
mod candidate_materialization;
mod candidate_metadata;
mod candidate_preparation;
mod candidate_ranking;
mod candidate_resolution;
mod candidate_source;
mod candidate_transport_ranking_facts;
mod common;
mod decision;
mod decision_input;
mod gemini_cli;
mod materialization_policy;
mod passthrough;
mod plan_builders;
mod pool_scheduler;
pub(crate) mod pool_scores;
mod redaction;
mod report_context;
mod route;
mod runtime_miss;
mod spec_metadata;
mod specialized;
mod standard;
mod state;

pub(crate) use self::candidate_materialization::LocalExecutionAttemptSource;
pub(crate) use self::candidate_resolution::{
    candidate_auth_channel_skip_reason, read_candidate_transport_snapshot,
    EligibleLocalExecutionCandidate, LocalExecutionCandidateKind, SkippedLocalExecutionCandidate,
};
pub(crate) use self::passthrough::{
    build_local_same_format_stream_attempt_source, build_local_same_format_stream_plan_and_reports,
    build_local_same_format_sync_attempt_source, build_local_same_format_sync_plan_and_reports,
};
pub(crate) use self::plan_builders::{
    build_gemini_stream_plan_from_decision, build_gemini_sync_plan_from_decision,
    build_openai_responses_stream_plan_from_decision,
    build_openai_responses_sync_plan_from_decision, build_passthrough_sync_plan_from_decision,
    build_standard_stream_plan_from_decision, build_standard_sync_plan_from_decision,
    AiStreamAttempt, AiSyncAttempt,
};
pub(crate) use self::pool_scores::{
    build_provider_key_pool_score_upsert, provider_key_pool_score_id, provider_key_pool_score_scope,
};
pub(crate) use self::route::is_matching_stream_request as planner_is_matching_stream_request;
pub(crate) use self::runtime_miss::{
    apply_local_runtime_candidate_terminal_reason, record_local_runtime_candidate_skip_reason,
};
pub(crate) use self::specialized::{
    build_local_gemini_files_stream_attempt_source_for_kind,
    build_local_gemini_files_stream_plan_and_reports_for_kind,
    build_local_gemini_files_sync_attempt_source_for_kind,
    build_local_gemini_files_sync_plan_and_reports_for_kind,
    build_local_image_stream_attempt_source_for_kind,
    build_local_image_stream_plan_and_reports_for_kind,
    build_local_image_sync_attempt_source_for_kind,
    build_local_image_sync_plan_and_reports_for_kind,
    build_local_video_sync_attempt_source_for_kind,
    build_local_video_sync_plan_and_reports_for_kind,
    set_local_openai_image_execution_exhausted_diagnostic,
};
pub(crate) use self::standard::{
    apply_codex_pool_stable_client_headers, build_local_openai_chat_stream_attempt_source_for_kind,
    build_local_openai_chat_stream_plan_and_reports_for_kind,
    build_local_openai_chat_sync_attempt_source_for_kind,
    build_local_openai_chat_sync_plan_and_reports_for_kind,
    build_local_openai_responses_stream_attempt_source_for_kind,
    build_local_openai_responses_stream_plan_and_reports_for_kind,
    build_local_openai_responses_sync_attempt_source_for_kind,
    build_local_openai_responses_sync_plan_and_reports_for_kind,
    build_local_stream_attempt_source as build_standard_family_stream_attempt_source,
    build_local_stream_plan_and_reports as build_standard_family_stream_plan_and_reports,
    build_local_sync_attempt_source as build_standard_family_sync_attempt_source,
    build_local_sync_plan_and_reports as build_standard_family_sync_plan_and_reports,
    set_local_openai_chat_execution_exhausted_diagnostic,
};
pub(crate) use self::state::{
    GatewayAuthApiKeySnapshot, GatewayProviderTransportSnapshot, LocalResolvedOAuthRequestAuth,
    PlannerAppState,
};
pub(crate) use aether_ai_serving::extract_ai_pool_sticky_session_token as extract_pool_sticky_session_token;
pub(crate) use aether_ai_serving::{
    build_ai_execution_decision_response, AiExecutionDecisionResponseParts,
    CandidateFailureDiagnostic, CandidateFailureDiagnosticKind,
};

pub(crate) fn pool_sticky_session_token_for_request(
    body_json: &serde_json::Value,
    client_session_affinity: Option<&aether_scheduler_core::ClientSessionAffinity>,
) -> Option<String> {
    extract_pool_sticky_session_token(body_json).or_else(|| {
        client_session_affinity?
            .session_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

pub(crate) async fn maybe_build_sync_decision_payload(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    body_base64: Option<&str>,
    body_is_empty: bool,
) -> Result<Option<AiExecutionDecision>, GatewayError> {
    decision::maybe_build_sync_decision_payload(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        body_base64,
        body_is_empty,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::pool_sticky_session_token_for_request;
    use crate::client_session_affinity::client_session_affinity_from_request;
    use aether_scheduler_core::ClientSessionAffinity;
    use http::{HeaderMap, HeaderValue};
    use serde_json::json;

    #[test]
    fn pool_sticky_session_token_prefers_body_session() {
        let body = json!({
            "session_id": "body-session"
        });
        let affinity = ClientSessionAffinity::new(
            Some("codex".to_string()),
            Some("session=header-session".to_string()),
        );

        assert_eq!(
            pool_sticky_session_token_for_request(&body, Some(&affinity)).as_deref(),
            Some("body-session")
        );
    }

    #[test]
    fn pool_sticky_session_token_uses_client_affinity_when_body_has_no_session() {
        let body = json!({
            "model": "gpt-5.4"
        });
        let affinity = ClientSessionAffinity::new(
            Some("codex".to_string()),
            Some("session=header-session".to_string()),
        );

        assert_eq!(
            pool_sticky_session_token_for_request(&body, Some(&affinity)).as_deref(),
            Some("session=header-session")
        );
    }

    #[test]
    fn pool_sticky_session_token_uses_codex_header_session_after_affinity_detection() {
        let body = json!({
            "model": "gpt-5.4"
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            HeaderValue::from_static("Codex Desktop/0.136.0-alpha.2"),
        );
        headers.insert("session_id", HeaderValue::from_static("de391a2896c46f3a"));

        let affinity = client_session_affinity_from_request(&headers, Some(&body))
            .expect("codex header session should build client affinity");

        assert_eq!(
            pool_sticky_session_token_for_request(&body, Some(&affinity)).as_deref(),
            Some("session=de391a2896c46f3a")
        );
    }

    #[test]
    fn pool_sticky_session_token_ignores_blank_client_affinity() {
        let body = json!({
            "model": "gpt-5.4"
        });
        let affinity =
            ClientSessionAffinity::new(Some("codex".to_string()), Some("  ".to_string()));

        assert_eq!(
            pool_sticky_session_token_for_request(&body, Some(&affinity)),
            None
        );
    }
}

pub(crate) async fn maybe_build_stream_decision_payload(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    body_base64: Option<&str>,
) -> Result<Option<AiExecutionDecision>, GatewayError> {
    decision::maybe_build_stream_decision_payload(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        body_base64,
    )
    .await
}

pub(crate) async fn maybe_build_sync_plan_payload(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    body_base64: Option<&str>,
    body_is_empty: bool,
) -> Result<Option<AiExecutionPlanPayload>, GatewayError> {
    decision::maybe_build_sync_plan_payload_impl(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        body_base64,
        body_is_empty,
    )
    .await
}

pub(crate) async fn maybe_build_stream_plan_payload(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    decision: &GatewayControlDecision,
    body_json: &serde_json::Value,
    body_base64: Option<&str>,
) -> Result<Option<AiExecutionPlanPayload>, GatewayError> {
    decision::maybe_build_stream_plan_payload_impl(
        state,
        parts,
        trace_id,
        decision,
        body_json,
        body_base64,
    )
    .await
}
