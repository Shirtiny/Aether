use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::actions::{RoutingHeaderPatch, RoutingJsonPatchOperation};

const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "set-cookie",
    "x-aether-trace-id",
    "x-aether-internal",
    "x-aether-scheduler-group",
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MutationError {
    #[error("json patch path must be an absolute JSON pointer: {0}")]
    InvalidJsonPointer(String),
    #[error("json patch cannot target reserved path: {0}")]
    ReservedJsonPath(String),
    #[error("json patch target does not exist: {0}")]
    MissingTarget(String),
    #[error("json patch parent is not an object: {0}")]
    InvalidParent(String),
    #[error("header patch targets reserved header: {0}")]
    ReservedHeader(String),
    #[error("header patch has invalid header name: {0}")]
    InvalidHeaderName(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderMutation {
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationPlan {
    pub body_patch: Vec<RoutingJsonPatchOperation>,
    pub header_patch: Vec<RoutingHeaderPatch>,
}

impl MutationPlan {
    pub fn is_empty(&self) -> bool {
        self.body_patch.is_empty() && self.header_patch.is_empty()
    }
}

pub fn validate_json_patch_operations(
    operations: &[RoutingJsonPatchOperation],
) -> Result<(), MutationError> {
    for operation in operations {
        let path = operation.path();
        validate_json_pointer(path)?;
        if is_reserved_json_path(path) {
            return Err(MutationError::ReservedJsonPath(path.to_string()));
        }
    }
    Ok(())
}

pub fn apply_json_patch_operations(
    body: &mut Value,
    operations: &[RoutingJsonPatchOperation],
) -> Result<(), MutationError> {
    validate_json_patch_operations(operations)?;
    for operation in operations {
        match operation {
            RoutingJsonPatchOperation::Add { path, value } => {
                set_json_pointer(body, path, value.clone(), true)?;
            }
            RoutingJsonPatchOperation::Replace { path, value } => {
                set_json_pointer(body, path, value.clone(), false)?;
            }
            RoutingJsonPatchOperation::Remove { path } => {
                remove_json_pointer(body, path)?;
            }
        }
    }
    Ok(())
}

pub fn validate_header_patch(patch: &[RoutingHeaderPatch]) -> Result<(), MutationError> {
    let reserved = RESERVED_HEADERS.iter().copied().collect::<BTreeSet<_>>();
    for item in patch {
        let name = item.name().trim().to_ascii_lowercase();
        if name.is_empty()
            || name
                .chars()
                .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '-'))
        {
            return Err(MutationError::InvalidHeaderName(item.name().to_string()));
        }
        if reserved.contains(name.as_str()) {
            return Err(MutationError::ReservedHeader(item.name().to_string()));
        }
    }
    Ok(())
}

fn validate_json_pointer(path: &str) -> Result<(), MutationError> {
    if !path.starts_with('/') {
        return Err(MutationError::InvalidJsonPointer(path.to_string()));
    }
    Ok(())
}

fn is_reserved_json_path(path: &str) -> bool {
    matches!(
        path,
        "/authorization"
            | "/api_key"
            | "/provider_secret"
            | "/upstream_url"
            | "/upstream_base_url"
            | "/auth"
    )
}

fn set_json_pointer(
    root: &mut Value,
    pointer: &str,
    value: Value,
    allow_create: bool,
) -> Result<(), MutationError> {
    let tokens = pointer_tokens(pointer);
    if tokens.is_empty() {
        *root = value;
        return Ok(());
    }
    let (parents, leaf) = tokens.split_at(tokens.len() - 1);
    let parent = descend_mut(root, parents, pointer)?;
    match parent {
        Value::Object(map) => {
            if !allow_create && !map.contains_key(&leaf[0]) {
                return Err(MutationError::MissingTarget(pointer.to_string()));
            }
            map.insert(leaf[0].clone(), value);
            Ok(())
        }
        _ => Err(MutationError::InvalidParent(pointer.to_string())),
    }
}

fn remove_json_pointer(root: &mut Value, pointer: &str) -> Result<(), MutationError> {
    let tokens = pointer_tokens(pointer);
    if tokens.is_empty() {
        *root = Value::Null;
        return Ok(());
    }
    let (parents, leaf) = tokens.split_at(tokens.len() - 1);
    let parent = descend_mut(root, parents, pointer)?;
    match parent {
        Value::Object(map) => map
            .remove(&leaf[0])
            .map(|_| ())
            .ok_or_else(|| MutationError::MissingTarget(pointer.to_string())),
        _ => Err(MutationError::InvalidParent(pointer.to_string())),
    }
}

fn descend_mut<'a>(
    root: &'a mut Value,
    tokens: &[String],
    pointer: &str,
) -> Result<&'a mut Value, MutationError> {
    let mut current = root;
    for token in tokens {
        match current {
            Value::Object(map) => {
                current = map
                    .get_mut(token)
                    .ok_or_else(|| MutationError::MissingTarget(pointer.to_string()))?;
            }
            Value::Null => {
                *current = Value::Object(Map::new());
                if let Value::Object(map) = current {
                    current = map
                        .entry(token.clone())
                        .or_insert_with(|| Value::Object(Map::new()));
                }
            }
            _ => return Err(MutationError::InvalidParent(pointer.to_string())),
        }
    }
    Ok(current)
}

fn pointer_tokens(pointer: &str) -> Vec<String> {
    pointer
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::actions::RoutingJsonPatchOperation;

    use super::*;

    #[test]
    fn applies_body_patch() {
        let mut body = json!({"metadata":{}});
        apply_json_patch_operations(
            &mut body,
            &[RoutingJsonPatchOperation::Add {
                path: "/metadata/routing".to_string(),
                value: json!("high"),
            }],
        )
        .expect("patch should apply");

        assert_eq!(body["metadata"]["routing"], json!("high"));
    }

    #[test]
    fn rejects_reserved_headers() {
        assert_eq!(
            validate_header_patch(&[RoutingHeaderPatch::Set {
                name: "authorization".to_string(),
                value: "secret".to_string()
            }]),
            Err(MutationError::ReservedHeader("authorization".to_string()))
        );
    }
}
