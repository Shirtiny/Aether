use aether_routing_core::{
    validate_header_patch, validate_json_patch_operations, RoutingHeaderPatch,
    RoutingJsonPatchOperation, RoutingRulePhase,
};
use http::StatusCode;
use std::sync::Arc;

use crate::ai_serving::ai_local_execution_contract_for_formats;
use crate::ai_serving::planner::plan_builders::AiStreamAttempt;
use crate::ai_serving::planner::report_context::{
    build_local_execution_report_context, insert_provider_stream_event_api_format,
    LocalExecutionReportContextParts,
};
use crate::ai_serving::planner::spec_metadata::local_openai_responses_spec_metadata;
use crate::ai_serving::transport::{
    build_codex_official_ws_planning_plan, resolve_transport_execution_timeouts,
    resolve_transport_profile, CodexOfficialWsPlanningPlanInput,
};
use crate::{
    append_execution_contract_fields_to_value, append_local_failover_policy_to_value, AppState,
    GatewayError,
};

const MAX_CODEX_WS_PROVIDER_BODY_PATCH_OPERATIONS: usize = 64;
const MAX_CODEX_WS_PROVIDER_BODY_PATCH_BYTES: usize = 8 * 1024;

use super::request::resolve_local_openai_responses_codex_ws_candidate_parts;
use super::support::{LocalOpenAiResponsesCandidateAttempt, LocalOpenAiResponsesDecisionInput};
use super::LocalOpenAiResponsesSpec;

/// Builds the scheduler/lifecycle plan for an official Codex WS candidate.
/// The response.create body remains borrowed and is never retained in the
/// returned attempt. The winning account materializes its provider body later.
pub(crate) async fn maybe_build_local_openai_responses_codex_ws_planning_attempt(
    state: &AppState,
    parts: &http::request::Parts,
    trace_id: &str,
    body_json: &serde_json::Value,
    input: &LocalOpenAiResponsesDecisionInput,
    attempt: LocalOpenAiResponsesCandidateAttempt,
    spec: LocalOpenAiResponsesSpec,
) -> Result<Option<(AiStreamAttempt, Arc<[RoutingJsonPatchOperation]>)>, GatewayError> {
    let spec_metadata = local_openai_responses_spec_metadata(spec);
    let attempt_identity = attempt.attempt_identity();
    let LocalOpenAiResponsesCandidateAttempt {
        eligible,
        candidate_id,
        ..
    } = attempt;
    let Some(mut resolved) = resolve_local_openai_responses_codex_ws_candidate_parts(
        state,
        parts,
        trace_id,
        body_json,
        input,
        &eligible,
        attempt_identity.candidate_index,
        &candidate_id,
        spec,
    )
    .await?
    else {
        return Ok(None);
    };

    let frozen_body_patch = freeze_codex_ws_provider_request_routing(
        input,
        resolved.provider_api_format.as_str(),
        resolved.mapped_model.as_str(),
        &mut resolved.provider_request_headers,
        body_json,
    )?;

    let candidate = &eligible.candidate;
    let effective_headers = input.effective_headers(&parts.headers);
    let mut extra_fields = serde_json::Map::new();
    insert_provider_stream_event_api_format(
        &mut extra_fields,
        resolved.transport.provider.provider_type.as_str(),
    );
    let (execution_strategy, conversion_mode) = ai_local_execution_contract_for_formats(
        spec_metadata.api_format,
        resolved.provider_api_format.as_str(),
    );
    let report_context = append_local_failover_policy_to_value(
        append_execution_contract_fields_to_value(
            build_local_execution_report_context(LocalExecutionReportContextParts {
                auth_context: &input.auth_context,
                request_id: trace_id,
                candidate_id: &candidate_id,
                attempt_identity,
                model: &input.requested_model,
                provider_name: &resolved.transport.provider.name,
                provider_id: &candidate.provider_id,
                endpoint_id: &candidate.endpoint_id,
                key_id: &candidate.key_id,
                key_name: Some(&candidate.key_name),
                model_id: Some(&candidate.model_id),
                global_model_id: Some(&candidate.global_model_id),
                global_model_name: Some(&candidate.global_model_name),
                provider_api_format: resolved.provider_api_format.as_str(),
                client_api_format: spec_metadata.api_format,
                mapped_model: Some(&resolved.mapped_model),
                candidate_group_id: eligible.orchestration.candidate_group_id.as_deref(),
                pool_key_lease: eligible.orchestration.pool_key_lease.as_ref(),
                pool_sticky_init_owner: eligible.orchestration.pool_sticky_init_owner.as_deref(),
                pool_sticky_session_token: eligible
                    .orchestration
                    .pool_sticky_session_token
                    .as_deref(),
                pool_sticky_bound_key_ineligible: eligible
                    .orchestration
                    .pool_sticky_bound_key_ineligible,
                pool_sticky_bound_key_id: eligible
                    .orchestration
                    .pool_sticky_bound_key_id
                    .as_deref(),
                pool_sticky_bound_key_ineligible_reason: eligible
                    .orchestration
                    .pool_sticky_bound_key_ineligible_reason
                    .as_deref(),
                ranking: eligible.ranking.as_ref(),
                upstream_url: Some(&resolved.upstream_url),
                // The compact report intentionally does not clone rule configs,
                // provider headers or either request body.
                header_rules: None,
                body_rules: None,
                provider_request_method: None,
                provider_request_headers: None,
                original_headers: effective_headers,
                request_path: Some(parts.uri.path()),
                request_query_string: parts.uri.query(),
                request_origin: Some(crate::ai_serving::request_origin_from_parts(parts)),
                original_request_body_json: None,
                original_request_body_base64: None,
                client_session_affinity: input.client_session_affinity.as_ref(),
                scheduler_affinity_epoch: eligible.orchestration.scheduler_affinity_epoch,
                client_requested_stream: body_json
                    .get("stream")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                upstream_is_stream: resolved.upstream_is_stream,
                has_envelope: false,
                needs_conversion: false,
                extra_fields,
            }),
            execution_strategy,
            conversion_mode,
            spec_metadata.api_format,
            candidate.endpoint_api_format.as_str(),
        ),
        resolved.transport.as_ref(),
    );
    let report_context = crate::codex_ws::compact_report_context_template(Some(&report_context));
    let plan = build_codex_official_ws_planning_plan(CodexOfficialWsPlanningPlanInput {
        request_id: trace_id.to_string(),
        candidate_id,
        provider_name: resolved.transport.provider.name.clone(),
        provider_id: candidate.provider_id.clone(),
        endpoint_id: candidate.endpoint_id.clone(),
        key_id: candidate.key_id.clone(),
        url: resolved.upstream_url,
        headers: resolved.provider_request_headers,
        client_api_format: spec_metadata.api_format.to_string(),
        provider_api_format: resolved.provider_api_format,
        model_name: input.requested_model.clone(),
        // The WS preflight resolves the concrete proxy exactly once after the
        // pool key is known, derives the connector route from that same value,
        // and installs it into this body-free lifecycle plan.
        proxy: None,
        transport_profile: resolve_transport_profile(resolved.transport.as_ref()),
        timeouts: resolve_transport_execution_timeouts(resolved.transport.as_ref()),
    });

    Ok(Some((
        AiStreamAttempt {
            plan: crate::codex_ws::compact_ws_planning_attempt_plan(&plan),
            report_kind: spec_metadata.report_kind.map(ToOwned::to_owned),
            report_context,
        },
        frozen_body_patch,
    )))
}

fn freeze_codex_ws_provider_request_routing(
    input: &LocalOpenAiResponsesDecisionInput,
    provider_api_format: &str,
    mapped_model: &str,
    headers: &mut std::collections::BTreeMap<String, String>,
    body: &serde_json::Value,
) -> Result<Arc<[RoutingJsonPatchOperation]>, GatewayError> {
    let Some(context) = input.routing_context.as_ref() else {
        return Ok(Arc::from([]));
    };
    let header_values = serde_json::Value::Object(
        headers
            .iter()
            .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
            .collect(),
    );
    let policy = crate::routing::resolve_gateway_routing_policy(
        crate::routing::GatewayRoutingPolicyInput {
            group_id: context.group_id.as_deref(),
            group_version: context.group_version,
            group_config_json: &context.group_config_json,
            selection_source: context.selection_source.as_str(),
            requested_model: input.requested_model.as_str(),
            resolved_model: mapped_model,
            api_format: provider_api_format,
            user_id: Some(input.auth_context.user_id.as_str()),
            api_key_id: Some(input.auth_context.api_key_id.as_str()),
            headers: &header_values,
            body,
            phase: RoutingRulePhase::ProviderRequest,
        },
    )?;
    let mutation = policy.mutation_plan;
    apply_codex_ws_header_patch(headers, &mutation.header_patch)?;
    if mutation.body_patch.is_empty() {
        return Ok(Arc::from([]));
    }
    validate_codex_ws_body_patch(body, &mutation.body_patch)?;
    Ok(Arc::from(mutation.body_patch))
}

fn apply_codex_ws_header_patch(
    headers: &mut std::collections::BTreeMap<String, String>,
    patch: &[RoutingHeaderPatch],
) -> Result<(), GatewayError> {
    validate_header_patch(patch).map_err(codex_ws_routing_patch_error)?;
    for operation in patch {
        match operation {
            RoutingHeaderPatch::Set { name, value } => {
                headers.insert(name.trim().to_ascii_lowercase(), value.clone());
            }
            RoutingHeaderPatch::Remove { name } => {
                headers.remove(name.trim().to_ascii_lowercase().as_str());
            }
        }
    }
    Ok(())
}

fn validate_codex_ws_body_patch(
    body: &serde_json::Value,
    patch: &[RoutingJsonPatchOperation],
) -> Result<(), GatewayError> {
    if patch.len() > MAX_CODEX_WS_PROVIDER_BODY_PATCH_OPERATIONS {
        return Err(codex_ws_routing_patch_error(format!(
            "provider body patch has {} operations; maximum is {}",
            patch.len(),
            MAX_CODEX_WS_PROVIDER_BODY_PATCH_OPERATIONS
        )));
    }
    validate_json_patch_operations(patch).map_err(codex_ws_routing_patch_error)?;
    let serialized = serde_json::to_vec(patch).map_err(|error| {
        GatewayError::Internal(format!(
            "failed to size Codex WS provider body patch: {error}"
        ))
    })?;
    if serialized.len() > MAX_CODEX_WS_PROVIDER_BODY_PATCH_BYTES {
        return Err(codex_ws_routing_patch_error(format!(
            "provider body patch is {} bytes; maximum is {}",
            serialized.len(),
            MAX_CODEX_WS_PROVIDER_BODY_PATCH_BYTES
        )));
    }

    for (index, operation) in patch.iter().enumerate() {
        let path = operation.path();
        for previous in &patch[..index] {
            if json_pointer_paths_overlap(previous.path(), path) {
                return Err(codex_ws_routing_patch_error(format!(
                    "provider body patch paths overlap: {} and {path}",
                    previous.path()
                )));
            }
        }
        match operation {
            RoutingJsonPatchOperation::Add { .. } => {
                let parent = json_pointer_parent(path);
                let parent_value = if parent.is_empty() {
                    Some(body)
                } else {
                    body.pointer(parent)
                };
                if !parent_value.is_some_and(serde_json::Value::is_object) {
                    return Err(codex_ws_routing_patch_error(format!(
                        "provider body add parent does not exist in the original request: {path}"
                    )));
                }
            }
            RoutingJsonPatchOperation::Replace { .. }
            | RoutingJsonPatchOperation::Remove { .. } => {
                if body.pointer(path).is_none() {
                    return Err(codex_ws_routing_patch_error(format!(
                        "provider body patch target does not exist in the original request: {path}"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn json_pointer_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn json_pointer_paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn codex_ws_routing_patch_error(error: impl std::fmt::Display) -> GatewayError {
    GatewayError::Client {
        status: StatusCode::BAD_REQUEST,
        message: format!("Codex WS provider routing patch rejected: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use aether_routing_core::RoutingJsonPatchOperation;

    use super::{validate_codex_ws_body_patch, MAX_CODEX_WS_PROVIDER_BODY_PATCH_OPERATIONS};

    #[test]
    fn codex_ws_provider_body_patch_is_bounded_and_order_independent() {
        let body = serde_json::json!({
            "model": "gpt-5.4",
            "metadata": {},
            "store": false,
        });
        let valid = vec![
            RoutingJsonPatchOperation::Add {
                path: "/metadata/route".to_string(),
                value: serde_json::json!("fast"),
            },
            RoutingJsonPatchOperation::Replace {
                path: "/store".to_string(),
                value: serde_json::json!(true),
            },
        ];
        assert!(validate_codex_ws_body_patch(&body, &valid).is_ok());

        let dependent = vec![
            RoutingJsonPatchOperation::Add {
                path: "/metadata/route".to_string(),
                value: serde_json::json!({}),
            },
            RoutingJsonPatchOperation::Add {
                path: "/metadata/route/tier".to_string(),
                value: serde_json::json!("priority"),
            },
        ];
        assert!(validate_codex_ws_body_patch(&body, &dependent).is_err());
    }

    #[test]
    fn codex_ws_provider_body_patch_rejects_large_values_and_operation_counts() {
        let body = serde_json::json!({"metadata": {}});
        let oversized = vec![RoutingJsonPatchOperation::Add {
            path: "/metadata/blob".to_string(),
            value: serde_json::json!("x".repeat(8 * 1024)),
        }];
        assert!(validate_codex_ws_body_patch(&body, &oversized).is_err());

        let too_many = (0..=MAX_CODEX_WS_PROVIDER_BODY_PATCH_OPERATIONS)
            .map(|index| RoutingJsonPatchOperation::Add {
                path: format!("/metadata/value_{index}"),
                value: serde_json::json!(index),
            })
            .collect::<Vec<_>>();
        assert!(validate_codex_ws_body_patch(&body, &too_many).is_err());
    }
}
