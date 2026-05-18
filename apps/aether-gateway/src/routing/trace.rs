use aether_routing_core::{ResolvedRoutingPolicy, RoutingDecisionTrace};

pub(crate) fn build_routing_trace_seed(
    policy: &ResolvedRoutingPolicy,
    client_api_format: &str,
) -> RoutingDecisionTrace {
    RoutingDecisionTrace {
        group_id: policy.group_id.clone(),
        group_version: policy.group_version,
        selection_source: policy.selection_source.clone(),
        selected_rules: policy
            .matched_rules
            .iter()
            .map(|rule| rule.id.clone())
            .collect(),
        original_model: policy.requested_model.clone(),
        resolved_model: policy.resolved_model.clone(),
        client_api_format: client_api_format.to_string(),
        client_request_patch_summary: routing_patch_summary(&policy.mutation_plan),
        runtime_facts: aether_routing_core::RoutingRuntimeFacts {
            scheduler_mode: Some(policy.scheduling_mode),
            priority_mode: Some(policy.priority_mode),
            ..Default::default()
        },
        ..RoutingDecisionTrace::default()
    }
}

fn routing_patch_summary(
    plan: &aether_routing_core::MutationPlan,
) -> aether_routing_core::RoutingPatchSummary {
    aether_routing_core::RoutingPatchSummary {
        body_paths: plan
            .body_patch
            .iter()
            .map(|operation| operation.path().to_string())
            .collect(),
        header_names: plan
            .header_patch
            .iter()
            .map(|operation| operation.name().to_string())
            .collect(),
        failed_action: None,
    }
}
