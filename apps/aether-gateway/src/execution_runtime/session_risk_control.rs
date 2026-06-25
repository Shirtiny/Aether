use std::collections::BTreeMap;

use aether_contracts::ExecutionPlan;
use aether_usage_runtime::{usage_json_text_matches_risk_control, usage_text_matches_risk_control};
use base64::Engine as _;
use serde_json::Value;
use tracing::warn;

use crate::log_ids::short_request_id;
use crate::scheduler::session_risk_control::{
    client_session_key_from_metadata, provider_session_risk_control_avoidance_mode,
};
use crate::AppState;

pub(crate) async fn should_return_and_record_session_risk_control_block_response(
    state: &AppState,
    plan: &ExecutionPlan,
    report_context: Option<&Value>,
    status_code: u16,
    headers: &BTreeMap<String, String>,
    response_text: Option<&str>,
    response_json: Option<&Value>,
    response_body: &[u8],
) -> bool {
    if status_code < 400
        || !response_matches_risk_control(response_text, response_json, response_body)
    {
        return false;
    }

    let Some(session_key) = client_session_key_from_metadata(report_context) else {
        return false;
    };

    let provider_ids = [plan.provider_id.clone()];
    let mode = state
        .read_provider_catalog_providers_by_ids(&provider_ids)
        .await
        .ok()
        .and_then(|providers| {
            providers
                .into_iter()
                .find(|provider| provider.id == plan.provider_id)
        })
        .map(|provider| provider_session_risk_control_avoidance_mode(provider.config.as_ref()));

    if !mode.is_some_and(|value| value.blocks_session()) {
        return false;
    }

    let body_base64 = base64::engine::general_purpose::STANDARD.encode(response_body);
    if let Err(err) = state
        .remember_provider_session_risk_control_block_response_if_enabled(
            &plan.provider_id,
            session_key,
            status_code,
            headers,
            body_base64.as_str(),
        )
        .await
    {
        warn!(
            event_name = "provider_session_risk_control_block_record_failed",
            log_type = "ops",
            request_id = %short_request_id(plan.request_id.as_str()),
            candidate_id = ?plan.candidate_id,
            provider_id = %plan.provider_id,
            error = ?err,
            "gateway failed to persist provider session risk-control block before returning upstream response"
        );
    }

    true
}

fn response_matches_risk_control(
    response_text: Option<&str>,
    response_json: Option<&Value>,
    response_body: &[u8],
) -> bool {
    usage_text_matches_risk_control(response_text)
        || response_json.is_some_and(usage_json_text_matches_risk_control)
        || std::str::from_utf8(response_body)
            .ok()
            .is_some_and(|body| usage_text_matches_risk_control(Some(body)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::response_matches_risk_control;

    #[test]
    fn response_matching_detects_risk_control_from_json_text_and_body() {
        assert!(response_matches_risk_control(
            None,
            Some(&json!({
                "error": {
                    "message": "Flagged for possible cybersecurity risk"
                }
            })),
            b"",
        ));
        assert!(response_matches_risk_control(
            Some("Visit chatgpt.com/cyber to request trusted access for cyber"),
            None,
            b"",
        ));
        assert!(response_matches_risk_control(
            None,
            None,
            b"possible cybersecurity risk",
        ));
        assert!(!response_matches_risk_control(
            Some("ordinary bad request"),
            None,
            b"{}",
        ));
    }
}
