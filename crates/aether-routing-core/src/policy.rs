use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::actions::{
    RoutingAction, RoutingRulePhase, RoutingSchedulingMode, RoutingSetPriorityMode,
};
use crate::conditions::RoutingConditionContext;
use crate::model::{RoutingGroupConfig, RoutingModelPolicy, RoutingPoolPolicyOverride};
use crate::mutations::{validate_header_patch, validate_json_patch_operations, MutationPlan};
use crate::ranking::RankingOverlay;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RoutingPolicyError {
    #[error("routing group config is invalid: {0}")]
    InvalidConfig(String),
    #[error("model is not allowed by routing group: {0}")]
    ModelNotAllowed(String),
    #[error("mutation action is invalid: {0}")]
    InvalidMutation(String),
}

#[derive(Debug, Clone)]
pub struct RoutingPolicyInput<'a> {
    pub group_id: Option<&'a str>,
    pub group_version: Option<i64>,
    pub selection_source: &'a str,
    pub requested_model: &'a str,
    pub resolved_model: &'a str,
    pub api_format: &'a str,
    pub user_id: Option<&'a str>,
    pub api_key_id: Option<&'a str>,
    pub headers: &'a Value,
    pub body: &'a Value,
    pub phase: RoutingRulePhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedRoutingRule {
    pub id: String,
    pub priority: i32,
    pub phase: RoutingRulePhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRoutingPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_version: Option<i64>,
    pub selection_source: String,
    pub requested_model: String,
    pub resolved_model: String,
    pub priority_mode: RoutingSetPriorityMode,
    pub scheduling_mode: RoutingSchedulingMode,
    pub keep_priority_on_conversion: bool,
    pub ranking_overlay: RankingOverlay,
    pub mutation_plan: MutationPlan,
    #[serde(default)]
    pub pool_policy_overrides: BTreeMap<String, RoutingPoolPolicyOverride>,
    #[serde(default)]
    pub matched_rules: Vec<MatchedRoutingRule>,
}

pub fn resolve_routing_policy(
    config: &RoutingGroupConfig,
    input: RoutingPolicyInput<'_>,
) -> Result<ResolvedRoutingPolicy, RoutingPolicyError> {
    if !model_allowed(&config.allowed_models, input.requested_model)
        && !model_allowed(&config.allowed_models, input.resolved_model)
    {
        return Err(RoutingPolicyError::ModelNotAllowed(
            input.requested_model.to_string(),
        ));
    }

    let mut policy = ResolvedRoutingPolicy {
        group_id: input.group_id.map(str::to_string),
        group_version: input.group_version,
        selection_source: input.selection_source.to_string(),
        requested_model: input.requested_model.to_string(),
        resolved_model: input.resolved_model.to_string(),
        priority_mode: config.default_policy.priority_mode,
        scheduling_mode: config.default_policy.scheduling_mode,
        keep_priority_on_conversion: config.default_policy.keep_priority_on_conversion,
        ranking_overlay: RankingOverlay::default(),
        mutation_plan: MutationPlan::default(),
        pool_policy_overrides: BTreeMap::new(),
        matched_rules: Vec::new(),
    };

    for model_policy in matching_model_policies(config, input.requested_model, input.resolved_model)
    {
        apply_model_policy(&mut policy, model_policy);
    }

    let condition_context = RoutingConditionContext {
        model: input.requested_model,
        api_format: input.api_format,
        user_id: input.user_id,
        api_key_id: input.api_key_id,
        headers: input.headers,
        body: input.body,
    };

    let mut rules = config
        .rules
        .iter()
        .filter(|rule| rule.enabled && rule.phase == input.phase)
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then(left.id.cmp(&right.id))
    });
    for rule in rules {
        if !rule.conditions.matches(&condition_context) {
            continue;
        }
        for action in &rule.actions {
            apply_action(
                &mut policy,
                action,
                input.requested_model,
                input.resolved_model,
            )?;
        }
        policy.matched_rules.push(MatchedRoutingRule {
            id: rule.id.clone(),
            priority: rule.priority,
            phase: rule.phase,
        });
        if rule.stop_processing {
            break;
        }
    }

    Ok(policy)
}

fn apply_model_policy(policy: &mut ResolvedRoutingPolicy, model_policy: &RoutingModelPolicy) {
    if !model_policy.allowed_providers.is_empty() {
        policy.ranking_overlay.allowed_providers = model_policy.allowed_providers.clone();
    }
    if !model_policy.allowed_keys.is_empty() {
        policy.ranking_overlay.allowed_keys = model_policy.allowed_keys.clone();
    }
    policy.ranking_overlay.provider_priority_overrides.extend(
        model_policy
            .provider_priority_overrides
            .iter()
            .map(|(key, value)| (key.clone(), *value)),
    );
    policy.ranking_overlay.key_priority_overrides.extend(
        model_policy
            .key_priority_overrides
            .iter()
            .map(|(key, value)| (key.clone(), *value)),
    );
    policy.ranking_overlay.pool_priority_overrides.extend(
        model_policy
            .pool_priority_overrides
            .iter()
            .map(|(key, value)| (key.clone(), *value)),
    );
    policy
        .pool_policy_overrides
        .extend(model_policy.pool_policy_overrides.clone());
}

fn apply_action(
    policy: &mut ResolvedRoutingPolicy,
    action: &RoutingAction,
    requested_model: &str,
    resolved_model: &str,
) -> Result<(), RoutingPolicyError> {
    match action {
        RoutingAction::RestrictModels { models } => {
            if !model_allowed(models, requested_model) && !model_allowed(models, resolved_model) {
                return Err(RoutingPolicyError::ModelNotAllowed(
                    requested_model.to_string(),
                ));
            }
        }
        RoutingAction::RestrictProviders { provider_ids } => {
            policy.ranking_overlay.allowed_providers = provider_ids.clone();
        }
        RoutingAction::RestrictKeys { key_ids } => {
            policy.ranking_overlay.allowed_keys = key_ids.clone();
        }
        RoutingAction::SetScheduling {
            priority_mode,
            scheduling_mode,
            keep_priority_on_conversion,
        } => {
            if let Some(priority_mode) = priority_mode {
                policy.priority_mode = *priority_mode;
            }
            if let Some(scheduling_mode) = scheduling_mode {
                policy.scheduling_mode = *scheduling_mode;
            }
            if let Some(keep_priority_on_conversion) = keep_priority_on_conversion {
                policy.keep_priority_on_conversion = *keep_priority_on_conversion;
            }
        }
        RoutingAction::SetProviderPriority {
            provider_id,
            priority,
        } => {
            policy
                .ranking_overlay
                .provider_priority_overrides
                .insert(provider_id.clone(), *priority);
        }
        RoutingAction::SetKeyPriority { key_id, priority } => {
            policy
                .ranking_overlay
                .key_priority_overrides
                .insert(key_id.clone(), *priority);
        }
        RoutingAction::JsonPatchBody { patch } => {
            validate_json_patch_operations(patch)
                .map_err(|error| RoutingPolicyError::InvalidMutation(error.to_string()))?;
            policy.mutation_plan.body_patch.extend(patch.clone());
        }
        RoutingAction::PatchHeaders { patch } => {
            validate_header_patch(patch)
                .map_err(|error| RoutingPolicyError::InvalidMutation(error.to_string()))?;
            policy.mutation_plan.header_patch.extend(patch.clone());
        }
    }
    Ok(())
}

fn matching_model_policies<'a>(
    config: &'a RoutingGroupConfig,
    requested_model: &str,
    resolved_model: &str,
) -> Vec<&'a RoutingModelPolicy> {
    config
        .model_policies
        .iter()
        .filter(|policy| {
            model_pattern_matches(&policy.model, requested_model)
                || model_pattern_matches(&policy.model, resolved_model)
        })
        .collect()
}

fn model_allowed(patterns: &[String], requested_model: &str) -> bool {
    patterns.is_empty()
        || patterns
            .iter()
            .any(|pattern| model_pattern_matches(pattern, requested_model))
}

fn model_pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    pattern == value
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::actions::{RoutingJsonPatchOperation, RoutingRulePhase};
    use crate::conditions::{RoutingCondition, RoutingConditionOp};
    use crate::model::{RoutingDefaultPolicy, RoutingRule};

    use super::*;

    #[test]
    fn resolves_model_policy_and_matching_rule() {
        let config = RoutingGroupConfig {
            allowed_models: vec!["gpt-*".to_string()],
            default_policy: RoutingDefaultPolicy::default(),
            model_policies: vec![RoutingModelPolicy {
                model: "gpt-5".to_string(),
                allowed_providers: vec!["provider-a".to_string()],
                provider_priority_overrides: BTreeMap::from([("provider-a".to_string(), 0)]),
                pool_priority_overrides: BTreeMap::from([("provider-a".to_string(), 3)]),
                ..RoutingModelPolicy::default()
            }],
            rules: vec![RoutingRule {
                id: "high".to_string(),
                priority: 10,
                enabled: true,
                phase: RoutingRulePhase::ClientRequest,
                conditions: RoutingCondition::Predicate {
                    field: "body.reasoning_effort".to_string(),
                    op: RoutingConditionOp::Eq,
                    value: Some(json!("high")),
                },
                actions: vec![RoutingAction::JsonPatchBody {
                    patch: vec![RoutingJsonPatchOperation::Add {
                        path: "/metadata/routing".to_string(),
                        value: json!("high"),
                    }],
                }],
                stop_processing: false,
            }],
        };

        let policy = resolve_routing_policy(
            &config,
            RoutingPolicyInput {
                group_id: Some("group-1"),
                group_version: Some(1),
                selection_source: "explicit",
                requested_model: "gpt-5",
                resolved_model: "gpt-5",
                api_format: "openai:chat",
                user_id: Some("user-1"),
                api_key_id: Some("api-key-1"),
                headers: &json!({}),
                body: &json!({"reasoning_effort":"high"}),
                phase: RoutingRulePhase::ClientRequest,
            },
        )
        .expect("policy should resolve");

        assert_eq!(policy.ranking_overlay.allowed_providers, vec!["provider-a"]);
        assert_eq!(
            policy
                .ranking_overlay
                .provider_priority_overrides
                .get("provider-a"),
            Some(&0)
        );
        assert_eq!(
            policy
                .ranking_overlay
                .pool_priority_overrides
                .get("provider-a"),
            Some(&3)
        );
        assert_eq!(policy.matched_rules.len(), 1);
        assert_eq!(policy.mutation_plan.body_patch.len(), 1);
    }

    #[test]
    fn rejects_disallowed_model() {
        let config = RoutingGroupConfig {
            allowed_models: vec!["gpt-5".to_string()],
            ..RoutingGroupConfig::default()
        };

        let err = resolve_routing_policy(
            &config,
            RoutingPolicyInput {
                group_id: None,
                group_version: None,
                selection_source: "test",
                requested_model: "claude",
                resolved_model: "claude",
                api_format: "openai:chat",
                user_id: None,
                api_key_id: None,
                headers: &json!({}),
                body: &json!({}),
                phase: RoutingRulePhase::ClientRequest,
            },
        )
        .unwrap_err();

        assert_eq!(
            err,
            RoutingPolicyError::ModelNotAllowed("claude".to_string())
        );
    }

    #[test]
    fn restrict_model_action_rejects_matching_request() {
        let config = RoutingGroupConfig {
            allowed_models: vec!["*".to_string()],
            rules: vec![RoutingRule {
                id: "restrict".to_string(),
                priority: 1,
                enabled: true,
                phase: RoutingRulePhase::ClientRequest,
                conditions: RoutingCondition::default(),
                actions: vec![RoutingAction::RestrictModels {
                    models: vec!["gpt-5".to_string()],
                }],
                stop_processing: false,
            }],
            ..RoutingGroupConfig::default()
        };

        let err = resolve_routing_policy(
            &config,
            RoutingPolicyInput {
                group_id: None,
                group_version: None,
                selection_source: "test",
                requested_model: "claude",
                resolved_model: "claude",
                api_format: "openai:chat",
                user_id: None,
                api_key_id: None,
                headers: &json!({}),
                body: &json!({}),
                phase: RoutingRulePhase::ClientRequest,
            },
        )
        .unwrap_err();

        assert_eq!(
            err,
            RoutingPolicyError::ModelNotAllowed("claude".to_string())
        );
    }
}
