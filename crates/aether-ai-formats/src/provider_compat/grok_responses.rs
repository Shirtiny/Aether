use serde_json::{json, Map, Value};

pub const GROK_RESPONSE_TOOL_REFS_REPORT_FIELD: &str = "grok_response_tool_refs";

const FUNCTION_TOOL_TYPE: &str = "function";
const CUSTOM_TOOL_TYPE: &str = "custom";
const NAMESPACE_TOOL_TYPE: &str = "namespace";

/// Collect the reversible identities that are lost while adapting Codex
/// namespace/custom tools to xAI's Responses schema.
pub fn collect_grok_response_tool_refs(body: &Value) -> Option<Value> {
    let mut refs = Map::new();
    collect_tool_refs_from_array(body.get("tools"), &mut refs);
    if let Some(input) = body.get("input").and_then(Value::as_array) {
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                collect_tool_refs_from_array(item.get("tools"), &mut refs);
            }
        }
    }
    (!refs.is_empty()).then_some(Value::Object(refs))
}

fn collect_tool_refs_from_array(tools: Option<&Value>, refs: &mut Map<String, Value>) {
    let Some(tools) = tools.and_then(Value::as_array) else {
        return;
    };
    for tool in tools {
        let declared_type = tool_type(tool);
        if declared_type == NAMESPACE_TOOL_TYPE {
            let namespace = string_field(tool, "name");
            if namespace.is_empty() {
                continue;
            }
            let Some(nested_tools) = tool.get("tools").and_then(Value::as_array) else {
                continue;
            };
            for nested in nested_tools {
                let nested_type = tool_type(nested);
                if !matches!(nested_type.as_str(), FUNCTION_TOOL_TYPE | CUSTOM_TOOL_TYPE) {
                    continue;
                }
                let name = string_field(nested, "name");
                let qualified = qualify_tool_name(&namespace, &name);
                if qualified.is_empty() {
                    continue;
                }
                refs.insert(
                    qualified,
                    json!({"namespace": namespace, "name": name, "type": nested_type}),
                );
            }
        } else if declared_type == CUSTOM_TOOL_TYPE {
            let name = string_field(tool, "name");
            if !name.is_empty() {
                refs.insert(
                    name.clone(),
                    json!({"name": name, "type": CUSTOM_TOOL_TYPE}),
                );
            }
        }
    }
}

/// Rewrite unsupported Codex Responses tools and historical tool-call items to
/// the function-only shape accepted by xAI.
pub fn normalize_grok_responses_request_tools(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    if let Some(tools) = object.get_mut("tools") {
        normalize_tool_array(tools);
        if tools.as_array().is_some_and(Vec::is_empty) {
            object.remove("tools");
        }
    }
    if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
        for item in input.iter_mut() {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                if let Some(tools) = item.get_mut("tools") {
                    normalize_tool_array(tools);
                }
            }
        }
        normalize_input_tool_calls(input);
    }
    normalize_tool_choice(object.get_mut("tool_choice"));
    let has_tools = object
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
        || object
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|input| {
                input.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("additional_tools")
                        && item
                            .get("tools")
                            .and_then(Value::as_array)
                            .is_some_and(|tools| !tools.is_empty())
                })
            });
    if !has_tools {
        object.remove("tool_choice");
        object.remove("parallel_tool_calls");
    }
}

fn normalize_tool_array(value: &mut Value) {
    let Some(tools) = value.as_array_mut() else {
        return;
    };
    let original = std::mem::take(tools);
    for tool in original {
        if tool_type(&tool) == NAMESPACE_TOOL_TYPE {
            let namespace = string_field(&tool, "name");
            if let Some(nested_tools) = tool.get("tools").and_then(Value::as_array) {
                for nested in nested_tools {
                    if let Some(normalized) = normalize_callable_tool(nested.clone(), &namespace) {
                        tools.push(normalized);
                    }
                }
            }
            continue;
        }
        if tool_type(&tool) == CUSTOM_TOOL_TYPE {
            if let Some(normalized) = normalize_callable_tool(tool, "") {
                tools.push(normalized);
            }
        } else {
            tools.push(tool);
        }
    }
}

fn normalize_callable_tool(mut tool: Value, namespace: &str) -> Option<Value> {
    let object = tool.as_object_mut()?;
    let is_custom = object.get("type").and_then(Value::as_str)?.trim() == CUSTOM_TOOL_TYPE;
    if !is_custom && object.get("type").and_then(Value::as_str)?.trim() != FUNCTION_TOOL_TYPE {
        return None;
    }
    object.insert(
        "type".to_string(),
        Value::String(FUNCTION_TOOL_TYPE.to_string()),
    );
    let name = object.get("name").and_then(Value::as_str)?.trim();
    let qualified = qualify_tool_name(namespace, name);
    if qualified.is_empty() {
        return None;
    }
    object.insert("name".to_string(), Value::String(qualified));
    object.remove("namespace");
    object.entry("parameters".to_string()).or_insert_with(|| {
        if is_custom {
            json!({
                "type": "object",
                "properties": {"input": {}},
                "required": ["input"],
                "additionalProperties": false
            })
        } else {
            json!({"type": "object", "properties": {}})
        }
    });
    Some(tool)
}

fn normalize_input_tool_calls(input: &mut [Value]) {
    for item in input {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("custom_tool_call") => {
                object.insert(
                    "type".to_string(),
                    Value::String("function_call".to_string()),
                );
                let custom_input = object
                    .remove("input")
                    .unwrap_or(Value::String(String::new()));
                object.insert(
                    "arguments".to_string(),
                    Value::String(custom_input_to_arguments(custom_input)),
                );
            }
            Some("custom_tool_call_output") => {
                object.insert(
                    "type".to_string(),
                    Value::String("function_call_output".to_string()),
                );
            }
            _ => {}
        }
        if object.get("type").and_then(Value::as_str) == Some("function_call") {
            let namespace = object
                .remove("namespace")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            if !namespace.trim().is_empty() {
                if let Some(name) = object.get("name").and_then(Value::as_str) {
                    object.insert(
                        "name".to_string(),
                        Value::String(qualify_tool_name(&namespace, name)),
                    );
                }
            }
        }
    }
}

fn custom_input_to_arguments(input: Value) -> String {
    match input {
        Value::String(text) => {
            let trimmed = text.trim();
            if serde_json::from_str::<Value>(trimmed).is_ok_and(|value| value.is_object()) {
                trimmed.to_string()
            } else {
                json!({"input": text}).to_string()
            }
        }
        Value::Object(_) => input.to_string(),
        other => json!({"input": other}).to_string(),
    }
}

fn normalize_tool_choice(choice: Option<&mut Value>) {
    let Some(choice) = choice else {
        return;
    };
    if let Some(object) = choice.as_object_mut() {
        if object.get("type").and_then(Value::as_str) == Some(CUSTOM_TOOL_TYPE) {
            object.insert(
                "type".to_string(),
                Value::String(FUNCTION_TOOL_TYPE.to_string()),
            );
        }
        if object.get("type").and_then(Value::as_str) == Some(FUNCTION_TOOL_TYPE) {
            let namespace = object
                .remove("namespace")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            if !namespace.trim().is_empty() {
                if let Some(name) = object.get("name").and_then(Value::as_str) {
                    object.insert(
                        "name".to_string(),
                        Value::String(qualify_tool_name(&namespace, name)),
                    );
                }
            }
        }
        if let Some(allowed) = object.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in allowed {
                normalize_tool_choice(Some(tool));
            }
        }
        if object.get("type").and_then(Value::as_str) == Some(NAMESPACE_TOOL_TYPE) {
            *choice = Value::String("auto".to_string());
        }
    }
}

pub fn restore_grok_response_tool_calls(value: &mut Value, refs: &Value) {
    let Some(refs) = refs.as_object() else {
        return;
    };
    restore_call_at_path(value.get_mut("item"), refs);
    if let Some(output) = value
        .get_mut("response")
        .and_then(Value::as_object_mut)
        .and_then(|response| response.get_mut("output"))
        .and_then(Value::as_array_mut)
    {
        for item in output {
            restore_call_at_path(Some(item), refs);
        }
    }
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        for item in output {
            restore_call_at_path(Some(item), refs);
        }
    }
}

pub fn restore_grok_response_sse_bytes(bytes: &[u8], refs: &Value) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut buffered = bytes.to_vec();
    while let Some(record) = drain_sse_record(&mut buffered) {
        restore_sse_record(&record, refs, &mut output);
    }
    if !buffered.is_empty() {
        restore_sse_record(&buffered, refs, &mut output);
    }
    output
}

fn drain_sse_record(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut line_start = 0usize;
    let mut index = 0usize;
    while index < buffer.len() {
        if buffer[index] != b'\n' {
            index += 1;
            continue;
        }
        let line_end = index + 1;
        let line = &buffer[line_start..line_end];
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return Some(buffer.drain(..line_end).collect());
        }
        line_start = line_end;
        index = line_end;
    }
    None
}

fn restore_sse_record(record: &[u8], refs: &Value, output: &mut Vec<u8>) {
    let Ok(text) = std::str::from_utf8(record) else {
        output.extend_from_slice(record);
        return;
    };
    let mut event = None;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    let Ok(mut payload) = serde_json::from_str::<Value>(data.trim()) else {
        output.extend_from_slice(record);
        return;
    };
    restore_grok_response_tool_calls(&mut payload, refs);
    if let Some(event) = event.filter(|event| !event.is_empty()) {
        output.extend_from_slice(b"event: ");
        output.extend_from_slice(event.as_bytes());
        output.push(b'\n');
    }
    output.extend_from_slice(b"data: ");
    output.extend_from_slice(payload.to_string().as_bytes());
    output.extend_from_slice(b"\n\n");
}

fn restore_call_at_path(item: Option<&mut Value>, refs: &Map<String, Value>) {
    let Some(object) = item.and_then(Value::as_object_mut) else {
        return;
    };
    if object.get("type").and_then(Value::as_str) != Some("function_call") {
        return;
    }
    let Some(qualified_name) = object.get("name").and_then(Value::as_str) else {
        return;
    };
    let Some(reference) = refs.get(qualified_name).and_then(Value::as_object) else {
        return;
    };
    let name = reference
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(qualified_name)
        .to_string();
    object.insert("name".to_string(), Value::String(name));
    if let Some(namespace) = reference.get("namespace").and_then(Value::as_str) {
        object.insert(
            "namespace".to_string(),
            Value::String(namespace.to_string()),
        );
    }
    if reference.get("type").and_then(Value::as_str) == Some(CUSTOM_TOOL_TYPE) {
        object.insert(
            "type".to_string(),
            Value::String("custom_tool_call".to_string()),
        );
        let arguments = object
            .remove("arguments")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        object.insert(
            "input".to_string(),
            Value::String(arguments_to_custom_input(&arguments)),
        );
    }
}

fn arguments_to_custom_input(arguments: &str) -> String {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    if object.len() == 1 {
        if let Some(input) = object.get("input") {
            return input
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| input.to_string());
        }
    }
    arguments.to_string()
}

fn qualify_tool_name(namespace: &str, name: &str) -> String {
    let namespace = namespace.trim();
    let name = name.trim();
    if namespace.is_empty() || name.is_empty() || name.starts_with("mcp__") {
        return name.to_string();
    }
    let prefix = if namespace.ends_with("__") {
        namespace.to_string()
    } else {
        format!("{namespace}__")
    };
    if name.starts_with(&prefix) {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

fn tool_type(tool: &Value) -> String {
    string_field(tool, "type").to_ascii_lowercase()
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_namespace_and_custom_tools() {
        let mut request = json!({
            "tools": [{
                "type": "namespace",
                "name": "multi_agent_v1",
                "tools": [
                    {"type": "function", "name": "spawn_agent"},
                    {"type": "custom", "name": "apply_patch", "format": {"type":"grammar"}}
                ]
            }],
            "input": [{
                "type": "custom_tool_call",
                "namespace": "multi_agent_v1",
                "name": "apply_patch",
                "call_id": "call_1",
                "input": "*** Begin Patch"
            }],
            "tool_choice": {"type":"function", "namespace":"multi_agent_v1", "name":"spawn_agent"}
        });
        let refs = collect_grok_response_tool_refs(&request).unwrap();
        normalize_grok_responses_request_tools(&mut request);

        assert_eq!(request["tools"][0]["name"], "multi_agent_v1__spawn_agent");
        assert_eq!(request["tools"][1]["type"], "function");
        assert_eq!(request["input"][0]["name"], "multi_agent_v1__apply_patch");
        assert!(request["input"][0].get("namespace").is_none());
        assert_eq!(
            request["tool_choice"]["name"],
            "multi_agent_v1__spawn_agent"
        );

        let mut response = json!({"response":{"output":[
            {"type":"function_call","name":"multi_agent_v1__spawn_agent","arguments":"{}"},
            {"type":"function_call","name":"multi_agent_v1__apply_patch","arguments":"{\"input\":\"*** Begin Patch\"}"}
        ]}});
        restore_grok_response_tool_calls(&mut response, &refs);
        assert_eq!(
            response["response"]["output"][0]["namespace"],
            "multi_agent_v1"
        );
        assert_eq!(response["response"]["output"][0]["name"], "spawn_agent");
        assert_eq!(
            response["response"]["output"][1]["type"],
            "custom_tool_call"
        );
        assert_eq!(
            response["response"]["output"][1]["input"],
            "*** Begin Patch"
        );
    }
}
