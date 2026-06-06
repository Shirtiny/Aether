use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingConditionOp {
    Eq,
    Ne,
    In,
    Contains,
    Exists,
    Prefix,
    Suffix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoutingCondition {
    All {
        all: Vec<RoutingCondition>,
    },
    Any {
        any: Vec<RoutingCondition>,
    },
    Not {
        not: Box<RoutingCondition>,
    },
    Predicate {
        field: String,
        op: RoutingConditionOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
    Empty {},
}

impl Default for RoutingCondition {
    fn default() -> Self {
        Self::Empty {}
    }
}

#[derive(Debug, Clone)]
pub struct RoutingConditionContext<'a> {
    pub model: &'a str,
    pub api_format: &'a str,
    pub user_id: Option<&'a str>,
    pub api_key_id: Option<&'a str>,
    pub headers: &'a Value,
    pub body: &'a Value,
}

impl RoutingCondition {
    pub fn matches(&self, context: &RoutingConditionContext<'_>) -> bool {
        match self {
            Self::All { all } => all.iter().all(|condition| condition.matches(context)),
            Self::Any { any } => any.iter().any(|condition| condition.matches(context)),
            Self::Not { not } => !not.matches(context),
            Self::Predicate { field, op, value } => {
                let actual = resolve_field(context, field);
                compare_condition(actual, *op, value.as_ref())
            }
            Self::Empty {} => true,
        }
    }
}

fn resolve_field(context: &RoutingConditionContext<'_>, field: &str) -> Option<Value> {
    let normalized = field.trim();
    match normalized {
        "model" => return Some(Value::String(context.model.to_string())),
        "api_format" | "client_api_format" => {
            return Some(Value::String(context.api_format.to_string()))
        }
        "user_id" => {
            return context
                .user_id
                .map(|value| Value::String(value.to_string()))
        }
        "api_key_id" => {
            return context
                .api_key_id
                .map(|value| Value::String(value.to_string()))
        }
        _ => {}
    }

    if let Some(path) = normalized.strip_prefix("headers.") {
        return lookup_dotted_path(context.headers, path).cloned();
    }
    if let Some(path) = normalized.strip_prefix("body.") {
        return lookup_dotted_path(context.body, path).cloned();
    }
    None
}

fn lookup_dotted_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for part in path.split('.').filter(|part| !part.is_empty()) {
        match current {
            Value::Object(map) => current = map.get(part)?,
            Value::Array(items) => {
                let index = part.parse::<usize>().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn compare_condition(
    actual: Option<Value>,
    op: RoutingConditionOp,
    expected: Option<&Value>,
) -> bool {
    match op {
        RoutingConditionOp::Exists => actual.is_some(),
        RoutingConditionOp::Eq => actual
            .as_ref()
            .zip(expected)
            .is_some_and(|(actual, expected)| values_equal(actual, expected)),
        RoutingConditionOp::Ne => actual
            .as_ref()
            .zip(expected)
            .is_none_or(|(actual, expected)| !values_equal(actual, expected)),
        RoutingConditionOp::In => {
            actual
                .as_ref()
                .zip(expected)
                .is_some_and(|(actual, expected)| {
                    expected
                        .as_array()
                        .is_some_and(|items| items.iter().any(|item| values_equal(actual, item)))
                })
        }
        RoutingConditionOp::Contains => {
            actual
                .as_ref()
                .zip(expected)
                .is_some_and(|(actual, expected)| {
                    let Some(expected) = expected.as_str() else {
                        return false;
                    };
                    value_as_string(actual).is_some_and(|actual| actual.contains(expected))
                })
        }
        RoutingConditionOp::Prefix => {
            actual
                .as_ref()
                .zip(expected)
                .is_some_and(|(actual, expected)| {
                    let Some(expected) = expected.as_str() else {
                        return false;
                    };
                    value_as_string(actual).is_some_and(|actual| actual.starts_with(expected))
                })
        }
        RoutingConditionOp::Suffix => {
            actual
                .as_ref()
                .zip(expected)
                .is_some_and(|(actual, expected)| {
                    let Some(expected) = expected.as_str() else {
                        return false;
                    };
                    value_as_string(actual).is_some_and(|actual| actual.ends_with(expected))
                })
        }
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (value_as_string(left), value_as_string(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn context<'a>(headers: &'a Value, body: &'a Value) -> RoutingConditionContext<'a> {
        RoutingConditionContext {
            model: "gpt-5",
            api_format: "openai:chat",
            user_id: Some("user-1"),
            api_key_id: Some("key-1"),
            headers,
            body,
        }
    }

    #[test]
    fn matches_all_body_and_header_predicates() {
        let condition = RoutingCondition::All {
            all: vec![
                RoutingCondition::Predicate {
                    field: "model".to_string(),
                    op: RoutingConditionOp::Eq,
                    value: Some(json!("gpt-5")),
                },
                RoutingCondition::Predicate {
                    field: "headers.x-app".to_string(),
                    op: RoutingConditionOp::Eq,
                    value: Some(json!("coding")),
                },
                RoutingCondition::Predicate {
                    field: "body.reasoning_effort".to_string(),
                    op: RoutingConditionOp::In,
                    value: Some(json!(["high", "xhigh"])),
                },
            ],
        };

        let headers = json!({"x-app":"coding"});
        assert!(condition.matches(&context(&headers, &json!({"reasoning_effort":"high"}))));
        assert!(!condition.matches(&context(&headers, &json!({"reasoning_effort":"low"}))));
    }
}
