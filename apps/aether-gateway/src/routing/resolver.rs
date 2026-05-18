use aether_routing_core::{
    resolve_routing_policy, ResolvedRoutingPolicy, RoutingGroupConfig, RoutingPolicyInput,
    RoutingRulePhase,
};
use http::StatusCode;
use serde_json::Value;

use crate::GatewayError;

#[derive(Debug, Clone)]
pub(crate) struct GatewayRoutingPolicyInput<'a> {
    pub group_id: Option<&'a str>,
    pub group_version: Option<i64>,
    pub group_config_json: &'a Value,
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

pub(crate) fn resolve_gateway_routing_policy(
    input: GatewayRoutingPolicyInput<'_>,
) -> Result<ResolvedRoutingPolicy, GatewayError> {
    let config = serde_json::from_value::<RoutingGroupConfig>(input.group_config_json.clone())
        .map_err(|err| GatewayError::Client {
            status: StatusCode::BAD_REQUEST,
            message: format!("invalid routing group config: {err}"),
        })?;
    resolve_routing_policy(
        &config,
        RoutingPolicyInput {
            group_id: input.group_id,
            group_version: input.group_version,
            selection_source: input.selection_source,
            requested_model: input.requested_model,
            resolved_model: input.resolved_model,
            api_format: input.api_format,
            user_id: input.user_id,
            api_key_id: input.api_key_id,
            headers: input.headers,
            body: input.body,
            phase: input.phase,
        },
    )
    .map_err(|err| GatewayError::Client {
        status: StatusCode::BAD_REQUEST,
        message: err.to_string(),
    })
}
