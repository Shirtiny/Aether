use aether_routing_core::{
    apply_json_patch_operations, validate_header_patch, MutationError, MutationPlan,
    RoutingHeaderPatch,
};
use http::StatusCode;
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::GatewayError;

pub(crate) fn apply_routing_mutation_plan(
    body: &mut Value,
    headers: &mut HeaderMap,
    plan: &MutationPlan,
) -> Result<(), GatewayError> {
    apply_json_patch_operations(body, &plan.body_patch).map_err(|err| GatewayError::Client {
        status: StatusCode::BAD_REQUEST,
        message: err.to_string(),
    })?;
    apply_header_patch(headers, &plan.header_patch).map_err(|err| GatewayError::Client {
        status: StatusCode::BAD_REQUEST,
        message: err.to_string(),
    })?;
    Ok(())
}

fn apply_header_patch(
    headers: &mut HeaderMap,
    patch: &[RoutingHeaderPatch],
) -> Result<(), MutationError> {
    validate_header_patch(patch)?;
    for item in patch {
        match item {
            RoutingHeaderPatch::Set { name, value } => {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| MutationError::InvalidHeaderName(name.clone()))?;
                let value = HeaderValue::from_str(value)
                    .map_err(|_| MutationError::InvalidHeaderName(name.to_string()))?;
                headers.insert(name, value);
            }
            RoutingHeaderPatch::Remove { name } => {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| MutationError::InvalidHeaderName(name.clone()))?;
                headers.remove(name);
            }
        }
    }
    Ok(())
}
