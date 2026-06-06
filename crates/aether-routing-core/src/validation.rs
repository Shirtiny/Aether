use std::collections::BTreeSet;

use thiserror::Error;

use crate::model::RoutingGroupConfig;
use crate::mutations::{validate_header_patch, validate_json_patch_operations};
use crate::RoutingAction;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RoutingValidationError {
    #[error("routing rule id is empty")]
    EmptyRuleId,
    #[error("duplicate routing rule id: {0}")]
    DuplicateRuleId(String),
    #[error("routing model policy selector is empty")]
    EmptyModelSelector,
    #[error("invalid mutation action: {0}")]
    InvalidMutation(String),
}

pub fn validate_routing_group_config(
    config: &RoutingGroupConfig,
) -> Result<(), RoutingValidationError> {
    let mut rule_ids = BTreeSet::new();
    for model_policy in &config.model_policies {
        if model_policy.model.trim().is_empty() {
            return Err(RoutingValidationError::EmptyModelSelector);
        }
    }
    for rule in &config.rules {
        if rule.id.trim().is_empty() {
            return Err(RoutingValidationError::EmptyRuleId);
        }
        if !rule_ids.insert(rule.id.clone()) {
            return Err(RoutingValidationError::DuplicateRuleId(rule.id.clone()));
        }
        for action in &rule.actions {
            match action {
                RoutingAction::JsonPatchBody { patch } => {
                    validate_json_patch_operations(patch).map_err(|error| {
                        RoutingValidationError::InvalidMutation(error.to_string())
                    })?;
                }
                RoutingAction::PatchHeaders { patch } => {
                    validate_header_patch(patch).map_err(|error| {
                        RoutingValidationError::InvalidMutation(error.to_string())
                    })?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
