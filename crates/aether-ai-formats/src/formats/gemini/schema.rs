use serde_json::{Map, Value};

const UNSUPPORTED_CONSTRAINTS: &[&str] = &[
    "minLength",
    "maxLength",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "pattern",
    "minItems",
    "maxItems",
    "uniqueItems",
    "format",
    "default",
    "examples",
];

const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "$schema",
    "$defs",
    "definitions",
    "const",
    "$ref",
    "$id",
    "additionalProperties",
    "propertyNames",
    "patternProperties",
    "$comment",
    "enumDescriptions",
    "enumTitles",
    "prefill",
    "deprecated",
    "nullable",
    "title",
];

const PLACEHOLDER_REASON_DESCRIPTION: &str = "Brief explanation of why you are calling this tool";

pub(crate) fn clean_gemini_tool_schema(value: &mut Value) {
    let _ = clean_schema_node(value);
}

fn clean_schema_node(value: &mut Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };

    replace_ref_with_hint(object);
    convert_const_to_enum(object);
    normalize_enum(object);
    preserve_additional_properties_hint(object);
    preserve_constraint_hints(object);
    merge_all_of(object);
    flatten_union(object);

    let nullable = flatten_type_array(object);
    clean_properties(object);
    clean_items(object);
    remove_unsupported_keywords(object);
    remove_placeholder_properties(object);
    cleanup_required(object);

    nullable
}

fn replace_ref_with_hint(object: &mut Map<String, Value>) {
    let Some(reference) = object
        .remove("$ref")
        .and_then(|value| value.as_str().map(str::to_string))
    else {
        return;
    };
    let name = reference.rsplit('/').next().unwrap_or(reference.as_str());
    let hint = format!("See: {name}");
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{value} ({hint})"))
        .unwrap_or(hint);

    object.clear();
    object.insert("type".to_string(), Value::String("object".to_string()));
    object.insert("description".to_string(), Value::String(description));
}

fn convert_const_to_enum(object: &mut Map<String, Value>) {
    if object.contains_key("enum") {
        return;
    }
    if let Some(value) = object.get("const").cloned() {
        object.insert("enum".to_string(), Value::Array(vec![value]));
    }
}

fn normalize_enum(object: &mut Map<String, Value>) {
    let Some(values) = object.get("enum").and_then(Value::as_array) else {
        return;
    };
    let values = values
        .iter()
        .map(schema_enum_value_to_string)
        .collect::<Vec<_>>();
    if (2..=10).contains(&values.len()) {
        append_hint(object, format!("Allowed: {}", values.join(", ")));
    }
    object.insert(
        "enum".to_string(),
        Value::Array(values.into_iter().map(Value::String).collect()),
    );
    object.insert("type".to_string(), Value::String("string".to_string()));
}

fn schema_enum_value_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn preserve_additional_properties_hint(object: &mut Map<String, Value>) {
    if object.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        append_hint(object, "No extra properties allowed");
    }
}

fn preserve_constraint_hints(object: &mut Map<String, Value>) {
    for key in UNSUPPORTED_CONSTRAINTS {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if value.is_array() || value.is_object() {
            continue;
        }
        append_hint(object, format!("{key}: {}", schema_hint_value(value)));
    }
}

fn schema_hint_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn merge_all_of(object: &mut Map<String, Value>) {
    let Some(branches) = object
        .remove("allOf")
        .and_then(|value| value.as_array().cloned())
    else {
        return;
    };

    for mut branch in branches {
        let _ = clean_schema_node(&mut branch);
        let Some(branch) = branch.as_object() else {
            continue;
        };
        if let Some(properties) = branch.get("properties").and_then(Value::as_object) {
            let target = object
                .entry("properties".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(target) = target.as_object_mut() {
                for (name, schema) in properties {
                    target.insert(name.clone(), schema.clone());
                }
            }
        }
        if let Some(required) = branch.get("required").and_then(Value::as_array) {
            let target = object
                .entry("required".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(target) = target.as_array_mut() {
                for name in required {
                    if !target.contains(name) {
                        target.push(name.clone());
                    }
                }
            }
        }
    }
}

fn flatten_union(object: &mut Map<String, Value>) {
    let union_key = if object.get("anyOf").is_some_and(Value::is_array) {
        "anyOf"
    } else if object.get("oneOf").is_some_and(Value::is_array) {
        "oneOf"
    } else {
        return;
    };
    let Some(mut branches) = object
        .remove(union_key)
        .and_then(|value| value.as_array().cloned())
        .filter(|branches| !branches.is_empty())
    else {
        return;
    };

    for branch in &mut branches {
        let _ = clean_schema_node(branch);
    }
    let types = branches.iter().map(schema_type_name).collect::<Vec<_>>();
    let Some(best) = branches
        .into_iter()
        .max_by_key(schema_branch_score)
        .and_then(|value| value.as_object().cloned())
    else {
        return;
    };

    let parent_description = object
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let child_description = best
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    *object = best;
    if let Some(parent_description) = parent_description {
        let description = child_description
            .map(|child| format!("{parent_description} ({child})"))
            .unwrap_or(parent_description);
        object.insert("description".to_string(), Value::String(description));
    }
    if types.len() > 1 {
        append_hint(object, format!("Accepts: {}", types.join(" | ")));
    }
}

fn schema_branch_score(value: &Value) -> u8 {
    let Some(object) = value.as_object() else {
        return 0;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("object") => 3,
        Some("array") => 2,
        Some("null") | None => {
            if object.contains_key("properties") {
                3
            } else if object.contains_key("items") {
                2
            } else {
                0
            }
        }
        Some(_) => 1,
    }
}

fn schema_type_name(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return "null".to_string();
    };
    object
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            object
                .contains_key("properties")
                .then(|| "object".to_string())
        })
        .or_else(|| object.contains_key("items").then(|| "array".to_string()))
        .unwrap_or_else(|| "null".to_string())
}

fn flatten_type_array(object: &mut Map<String, Value>) -> bool {
    let Some(types) = object.get("type").and_then(Value::as_array) else {
        return false;
    };
    let types = types
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if types.is_empty() {
        return false;
    }
    let nullable = types.iter().any(|value| value == "null");
    let non_null = types
        .iter()
        .filter(|value| value.as_str() != "null")
        .cloned()
        .collect::<Vec<_>>();
    let selected = non_null
        .first()
        .cloned()
        .unwrap_or_else(|| "string".to_string());
    object.insert("type".to_string(), Value::String(selected));
    if non_null.len() > 1 {
        append_hint(object, format!("Accepts: {}", non_null.join(" | ")));
    }
    if nullable {
        append_hint(object, "(nullable)");
    }
    nullable
}

fn clean_properties(object: &mut Map<String, Value>) {
    let mut nullable_names = Vec::new();
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, schema) in properties {
            if clean_schema_node(schema) {
                nullable_names.push(name.clone());
            }
        }
    }
    if nullable_names.is_empty() {
        return;
    }
    let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) else {
        return;
    };
    required.retain(|name| {
        name.as_str()
            .is_none_or(|name| !nullable_names.iter().any(|nullable| nullable == name))
    });
    if required.is_empty() {
        object.remove("required");
    }
}

fn clean_items(object: &mut Map<String, Value>) {
    match object.get_mut("items") {
        Some(Value::Object(_)) => {
            if let Some(items) = object.get_mut("items") {
                let _ = clean_schema_node(items);
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                let _ = clean_schema_node(item);
            }
        }
        _ => {}
    }
}

fn remove_unsupported_keywords(object: &mut Map<String, Value>) {
    for key in UNSUPPORTED_CONSTRAINTS
        .iter()
        .chain(UNSUPPORTED_KEYWORDS.iter())
    {
        object.remove(*key);
    }
    object.retain(|key, _| !key.starts_with("x-"));
}

fn remove_placeholder_properties(object: &mut Map<String, Value>) {
    let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    properties.remove("_");
    let remove_reason = properties.len() == 1
        && properties
            .get("reason")
            .and_then(Value::as_object)
            .and_then(|reason| reason.get("description"))
            .and_then(Value::as_str)
            == Some(PLACEHOLDER_REASON_DESCRIPTION);
    if remove_reason {
        properties.remove("reason");
    }
}

fn cleanup_required(object: &mut Map<String, Value>) {
    let Some(property_names) = object
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
    else {
        return;
    };
    let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) else {
        return;
    };
    required.retain(|name| {
        name.as_str()
            .is_some_and(|name| property_names.iter().any(|property| property == name))
    });
    if required.is_empty() {
        object.remove("required");
    }
}

fn append_hint(object: &mut Map<String, Value>, hint: impl AsRef<str>) {
    let hint = hint.as_ref();
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{value} ({hint})"))
        .unwrap_or_else(|| hint.to_string());
    object.insert("description".to_string(), Value::String(description));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::clean_gemini_tool_schema;

    #[test]
    fn cleans_claude_code_schema_like_cpa() {
        let mut schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "environment": {
                    "type": "object",
                    "propertyNames": {"pattern": "^[A-Z_]+$"}
                },
                "mode": {"const": "apply"},
                "retries": {"type": "integer", "exclusiveMinimum": 0}
            }
        });

        clean_gemini_tool_schema(&mut schema);

        assert!(schema.get("$schema").is_none());
        assert!(schema["properties"]["environment"]
            .get("propertyNames")
            .is_none());
        assert_eq!(schema["properties"]["mode"]["type"], "string");
        assert_eq!(schema["properties"]["mode"]["enum"], json!(["apply"]));
        assert!(schema["properties"]["retries"]
            .get("exclusiveMinimum")
            .is_none());
        assert_eq!(
            schema["properties"]["retries"]["description"],
            "exclusiveMinimum: 0"
        );
    }

    #[test]
    fn preserves_property_names_that_match_schema_keywords() {
        let mut schema = json!({
            "$schema": "remove",
            "type": "object",
            "properties": {
                "$schema": {"type": "string"},
                "propertyNames": {"type": "string"},
                "const": {"type": "boolean"},
                "x-data": {"type": "number"}
            },
            "x-metadata": "remove"
        });

        clean_gemini_tool_schema(&mut schema);

        assert!(schema.get("$schema").is_none());
        assert!(schema.get("x-metadata").is_none());
        for name in ["$schema", "propertyNames", "const", "x-data"] {
            assert!(schema["properties"].get(name).is_some());
        }
    }

    #[test]
    fn flattens_nullable_and_union_schemas() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": ["string", "null"]},
                "config": {
                    "description": "Configuration",
                    "anyOf": [
                        {"type": "string"},
                        {"type": "object", "properties": {"kind": {"type": "string"}}}
                    ]
                }
            },
            "required": ["name", "config", "missing"]
        });

        clean_gemini_tool_schema(&mut schema);

        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert_eq!(schema["properties"]["name"]["description"], "(nullable)");
        assert_eq!(schema["required"], json!(["config"]));
        assert_eq!(schema["properties"]["config"]["type"], "object");
        assert!(schema["properties"]["config"]["description"]
            .as_str()
            .is_some_and(|value| value.contains("Accepts: string | object")));
    }

    #[test]
    fn converts_refs_and_additional_properties_to_hints() {
        let mut schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "customer": {
                    "description": "Customer record",
                    "$ref": "#/$defs/Customer"
                }
            },
            "$defs": {
                "Customer": {"type": "object"}
            }
        });

        clean_gemini_tool_schema(&mut schema);

        assert!(schema.get("additionalProperties").is_none());
        assert!(schema["description"]
            .as_str()
            .is_some_and(|value| value.contains("No extra properties allowed")));
        assert_eq!(schema["properties"]["customer"]["type"], "object");
        assert_eq!(
            schema["properties"]["customer"]["description"],
            "Customer record (See: Customer)"
        );
    }

    #[test]
    fn merges_all_of_and_normalizes_enums_constraints_and_required() {
        let mut schema = json!({
            "type": "object",
            "allOf": [
                {
                    "properties": {
                        "priority": {"type": "integer", "enum": [1, 2]}
                    },
                    "required": ["priority", "stale"]
                },
                {
                    "properties": {
                        "url": {"type": "string", "format": "uri"}
                    },
                    "required": ["url"]
                }
            ]
        });

        clean_gemini_tool_schema(&mut schema);

        assert!(schema.get("allOf").is_none());
        assert_eq!(schema["properties"]["priority"]["type"], "string");
        assert_eq!(schema["properties"]["priority"]["enum"], json!(["1", "2"]));
        assert!(schema["properties"]["priority"]["description"]
            .as_str()
            .is_some_and(|value| value.contains("Allowed: 1, 2")));
        assert!(schema["properties"]["url"].get("format").is_none());
        assert_eq!(schema["properties"]["url"]["description"], "format: uri");
        assert_eq!(schema["required"], json!(["priority", "url"]));
    }
}
