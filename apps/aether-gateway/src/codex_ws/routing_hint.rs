use aether_routing_core::RoutingJsonPatchOperation;
use http::HeaderMap;
use serde_json::{json, Value};

use super::protocol::ResponseCreateStep;
use super::runtime::{CodexWsCandidate, StepPreparationError};

pub(super) async fn for_step(
    candidate: &CodexWsCandidate,
    step: &ResponseCreateStep,
    headers: &HeaderMap,
) -> Result<Option<String>, StepPreparationError> {
    let full_body_needed =
        crate::provider_transport::body_rules_have_enabled_rules(candidate.body_rules.as_deref())
            || !candidate.provider_body_patch.is_empty();
    let cpu = super::cpu_budget::acquire_large_frame_cpu_budget(if full_body_needed {
        step.encoded_len
    } else {
        0
    })
    .await
    .map_err(|_| StepPreparationError::retain("large_frame_cpu_unavailable"))?;
    // Only a connecting account evaluates body-dependent rules. Candidate
    // fanout remains body-free, and ordinary requests need just these fields.
    let body = if full_body_needed {
        step.value.clone()
    } else {
        json!({
            "model": step.model,
            "service_tier": step.value.get("service_tier"),
            "tool_choice": step.value.get("tool_choice")
        })
    };
    let model = candidate.mapped_model.clone();
    let rules = candidate.body_rules.clone();
    let mapping = candidate.model_directive_mapping.clone();
    let patch = candidate.provider_body_patch.clone();
    let headers = headers.clone();
    let enable_directives = candidate.enable_model_directives;
    let normalize = move || {
        let _cpu = cpu;
        normalize_body(
            body,
            &model,
            rules.as_deref(),
            &headers,
            enable_directives,
            mapping.as_deref(),
            &patch,
        )
        .map(|body| crate::codex_routing_hint::from_body(&body))
    };
    if full_body_needed && super::cpu_budget::requires_large_frame_cpu_budget(step.encoded_len) {
        tokio::task::spawn_blocking(normalize).await.map_err(|_| {
            StepPreparationError::retain("provider_request_body_materialization_failed")
        })?
    } else {
        normalize()
    }
}

pub(super) fn normalize_body(
    body: Value,
    mapped_model: &str,
    body_rules: Option<&Value>,
    request_headers: &HeaderMap,
    enable_model_directives: bool,
    model_directive_mapping: Option<&Value>,
    provider_body_patch: &[RoutingJsonPatchOperation],
) -> Result<Value, StepPreparationError> {
    let mut body = crate::ai_serving::build_codex_ws_local_openai_responses_request_body(
        body,
        mapped_model,
        true,
        false,
        "codex",
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
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_hint_follows_mapping_directives_and_provider_rules() {
        let mut headers = HeaderMap::new();
        headers.insert("x-tier", "flex".parse().unwrap());
        let body = json!({"model":"gpt-5.6-luna-fast", "input":[]});
        let normalized =
            normalize_body(body.clone(), "gpt-5.5", None, &headers, true, None, &[]).unwrap();
        assert_eq!(
            crate::codex_routing_hint::from_body(&normalized).as_deref(),
            Some("model=gpt-5.5;tier=priority")
        );

        let rules = json!([{"action":"set", "path":"service_tier", "value":"flex"}]);
        let normalized = normalize_body(
            body.clone(),
            "gpt-5.5",
            Some(&rules),
            &headers,
            true,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(
            crate::codex_routing_hint::from_body(&normalized).as_deref(),
            Some("model=gpt-5.5;tier=flex")
        );

        let patch = serde_json::from_value::<Vec<RoutingJsonPatchOperation>>(json!([
            {"op":"replace", "path":"/model", "value":"gpt-5.6-sol"},
            {"op":"remove", "path":"/service_tier"}
        ]))
        .unwrap();
        let normalized =
            normalize_body(body, "gpt-5.5", Some(&rules), &headers, true, None, &patch).unwrap();
        assert_eq!(
            crate::codex_routing_hint::from_body(&normalized).as_deref(),
            Some("model=gpt-5.6-sol")
        );
    }
}
