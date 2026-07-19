use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

pub const GROK_RESPONSE_TOOL_REFS_REPORT_FIELD: &str = "grok_response_tool_refs";
pub const GROK_RESPONSE_INTERNAL_X_SEARCH_REPORT_FIELD: &str = "grok_response_internal_x_search";

const FUNCTION_TOOL_TYPE: &str = "function";
const CUSTOM_TOOL_TYPE: &str = "custom";
const NAMESPACE_TOOL_TYPE: &str = "namespace";
const TOOL_SEARCH_TOOL_TYPE: &str = "tool_search";
const X_SEARCH_TOOL_TYPE: &str = "x_search";
const CODEX_APP_NAMESPACE: &str = "codex_app";
const AUTOMATION_UPDATE_TOOL_NAME: &str = "automation_update";

const XAI_SUPPORTED_TOOL_TYPES: &[&str] = &[
    FUNCTION_TOOL_TYPE,
    "web_search",
    X_SEARCH_TOOL_TYPE,
    "image_generation",
    "collections_search",
    "file_search",
    "code_execution",
    "code_interpreter",
    "mcp",
    "shell",
];

const INTERNAL_X_SEARCH_TOOL_NAMES: &[&str] = &[
    "x_user_search",
    "x_semantic_search",
    "x_keyword_search",
    "x_thread_fetch",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolChoiceKey {
    tool_type: String,
    name: String,
}

/// Collect the reversible identities that are lost while adapting Codex
/// namespace/custom tools to xAI's Responses schema.
pub fn collect_grok_response_tool_refs(body: &Value) -> Option<Value> {
    let mut refs = Map::new();
    let mut seen_callable_names = BTreeSet::new();
    collect_tool_refs_from_array(body.get("tools"), &mut refs, &mut seen_callable_names);
    if let Some(input) = body.get("input").and_then(Value::as_array) {
        // Request normalization appends client-side tool-search discoveries
        // before promoted additional_tools, so collect refs in the same order.
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("tool_search_output")
                && item.get("execution").and_then(Value::as_str) != Some("server")
            {
                collect_tool_refs_from_array(
                    item.get("tools"),
                    &mut refs,
                    &mut seen_callable_names,
                );
            }
        }
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                collect_tool_refs_from_array(
                    item.get("tools"),
                    &mut refs,
                    &mut seen_callable_names,
                );
            }
        }
    }
    (!refs.is_empty()).then_some(Value::Object(refs))
}

pub fn grok_response_request_uses_x_search(body: &Value) -> bool {
    tool_arrays(body).any(|tools| {
        tools
            .iter()
            .any(|tool| tool_type(tool).eq_ignore_ascii_case(X_SEARCH_TOOL_TYPE))
    })
}

fn tool_arrays(body: &Value) -> impl Iterator<Item = &[Value]> {
    let top_level = body.get("tools").and_then(Value::as_array).into_iter();
    let embedded = body
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("additional_tools")
                || (item.get("type").and_then(Value::as_str) == Some("tool_search_output")
                    && item.get("execution").and_then(Value::as_str) != Some("server"))
        })
        .filter_map(|item| item.get("tools").and_then(Value::as_array));
    top_level.chain(embedded).map(Vec::as_slice)
}

fn collect_tool_refs_from_array(
    tools: Option<&Value>,
    refs: &mut Map<String, Value>,
    seen_callable_names: &mut BTreeSet<String>,
) {
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
                if qualified.is_empty() || !seen_callable_names.insert(qualified.clone()) {
                    continue;
                }
                refs.insert(
                    qualified,
                    json!({"namespace": namespace, "name": name, "type": nested_type}),
                );
            }
        } else if matches!(
            declared_type.as_str(),
            FUNCTION_TOOL_TYPE | CUSTOM_TOOL_TYPE
        ) {
            let name = string_field(tool, "name");
            let namespace = string_field(tool, "namespace");
            let qualified = qualify_tool_name(&namespace, &name);
            if qualified.is_empty() || !seen_callable_names.insert(qualified.clone()) {
                continue;
            }
            if declared_type == FUNCTION_TOOL_TYPE && namespace.is_empty() {
                continue;
            }
            let mut reference = json!({"name": name, "type": declared_type});
            if !namespace.is_empty() {
                reference["namespace"] = Value::String(namespace);
            }
            refs.insert(qualified, reference);
        }
    }
}

/// Rewrite unsupported Codex Responses tools and historical tool-call items to
/// the subset accepted by xAI, preserving callable namespace/custom tools as
/// ordinary functions.
pub fn normalize_grok_responses_request_tools(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };

    if object.get("tools").is_some_and(|tools| !tools.is_array()) {
        object.remove("tools");
    }

    let discovered_tools = object
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_search_output")
                && item.get("execution").and_then(Value::as_str) != Some("server")
        })
        .filter_map(|item| item.get("tools").and_then(Value::as_array))
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    if !discovered_tools.is_empty() {
        let tools = object
            .entry("tools".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(tools) = tools.as_array_mut() {
            tools.extend(discovered_tools);
        }
    }

    if let Some(tools) = object.get_mut("tools") {
        normalize_tool_array(tools);
        if tools.as_array().is_some_and(Vec::is_empty) {
            object.remove("tools");
        }
    }
    let mut promoted_tools = Vec::new();
    if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
        for item in input.iter_mut() {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                if let Some(tools) = item.get_mut("tools") {
                    normalize_tool_array(tools);
                }
            }
        }
        promoted_tools = take_additional_tools(input);
        normalize_input_tool_calls(input);
    }
    if !promoted_tools.is_empty() {
        let tools = object
            .entry("tools".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !tools.is_array() {
            *tools = Value::Array(Vec::new());
        }
        tools
            .as_array_mut()
            .expect("tools was initialized as an array")
            .append(&mut promoted_tools);
        normalize_tool_array(tools);
    }
    if object
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        object.remove("tools");
    }
    let available_tools = collect_available_tool_choice_keys(object);
    let keep_tool_choice = object
        .get_mut("tool_choice")
        .is_none_or(|choice| normalize_tool_choice(choice, &available_tools, true));
    if !keep_tool_choice {
        object.remove("tool_choice");
    }
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

/// xAI does not accept Responses Lite `additional_tools` input items. Move
/// their already-normalized declarations into the top-level tools array while
/// retaining the remaining conversation input unchanged.
fn take_additional_tools(input: &mut Vec<Value>) -> Vec<Value> {
    let mut promoted = Vec::new();
    input.retain_mut(|item| {
        if item.get("type").and_then(Value::as_str) != Some("additional_tools") {
            return true;
        }
        if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
            promoted.append(tools);
        }
        false
    });
    promoted
}

fn normalize_tool_array(value: &mut Value) {
    let Some(tools) = value.as_array_mut() else {
        return;
    };
    let original = std::mem::take(tools);
    for tool in original {
        let declared_type = tool_type(&tool);
        if declared_type == NAMESPACE_TOOL_TYPE {
            let namespace = string_field(&tool, "name");
            if namespace.is_empty() {
                continue;
            }
            if let Some(nested_tools) = tool.get("tools").and_then(Value::as_array) {
                for nested in nested_tools {
                    if let Some(normalized) = normalize_callable_tool(nested.clone(), &namespace) {
                        tools.push(normalized);
                    }
                }
            }
            continue;
        }
        if matches!(
            declared_type.as_str(),
            FUNCTION_TOOL_TYPE | CUSTOM_TOOL_TYPE
        ) {
            if let Some(normalized) = normalize_callable_tool(tool, "") {
                tools.push(normalized);
            }
        } else if declared_type != TOOL_SEARCH_TOOL_TYPE
            && XAI_SUPPORTED_TOOL_TYPES.contains(&declared_type.as_str())
        {
            tools.push(tool);
        }
    }
    let mut seen = BTreeSet::new();
    tools.retain(|tool| seen.insert(normalized_tool_dedupe_key(tool)));
}

fn normalized_tool_dedupe_key(tool: &Value) -> String {
    let declared_type = tool_type(tool);
    if declared_type == FUNCTION_TOOL_TYPE {
        return format!("{FUNCTION_TOOL_TYPE}:{}", string_field(tool, "name"));
    }
    format!(
        "{declared_type}:{}",
        serde_json::to_string(tool).unwrap_or_default()
    )
}

fn normalize_callable_tool(mut tool: Value, namespace: &str) -> Option<Value> {
    let object = tool.as_object_mut()?;
    let declared_type = object
        .get("type")
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    let is_custom = declared_type == CUSTOM_TOOL_TYPE;
    if !is_custom && declared_type != FUNCTION_TOOL_TYPE {
        return None;
    }
    object.insert(
        "type".to_string(),
        Value::String(FUNCTION_TOOL_TYPE.to_string()),
    );
    let embedded_namespace = object
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let effective_namespace = if namespace.trim().is_empty() {
        embedded_namespace.as_str()
    } else {
        namespace
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    let qualified = qualify_tool_name(effective_namespace, &name);
    if qualified.is_empty() {
        return None;
    }
    object.insert("name".to_string(), Value::String(qualified));
    object.remove("namespace");
    object.remove("defer_loading");
    if is_custom {
        object.remove("format");
        object.insert(
            "parameters".to_string(),
            json!({
                "type": "object",
                "properties": {"input": {}},
                "required": ["input"],
                "additionalProperties": false
            }),
        );
        if object.get("strict").and_then(Value::as_bool) == Some(true) {
            object.insert("strict".to_string(), Value::Bool(false));
        }
    } else {
        object
            .entry("parameters".to_string())
            .or_insert_with(|| json!({"type": "object", "properties": {}}));
    }
    normalize_object_root_union_branch_types(object);
    if function_parameters_need_simplification(object, effective_namespace, &name, is_custom) {
        object.insert(
            "parameters".to_string(),
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
        );
        if object.get("strict").and_then(Value::as_bool) == Some(true) {
            object.insert("strict".to_string(), Value::Bool(false));
        }
    }
    Some(tool)
}

/// Untyped branches of an object-only root union still validate as arbitrary
/// JSON values in xAI. Mark those branches as objects when the root already
/// establishes that constraint, preserving the original schema instead of
/// replacing it with the permissive fallback.
fn normalize_object_root_union_branch_types(tool: &mut Map<String, Value>) {
    let Some(parameters) = tool.get_mut("parameters").and_then(Value::as_object_mut) else {
        return;
    };
    if !parameters
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("object"))
    {
        return;
    }
    for union_name in ["anyOf", "oneOf"] {
        let Some(branches) = parameters.get_mut(union_name).and_then(Value::as_array_mut) else {
            continue;
        };
        for branch in branches.iter_mut().filter_map(Value::as_object_mut) {
            branch
                .entry("type".to_string())
                .or_insert_with(|| Value::String("object".to_string()));
        }
    }
}

fn function_parameters_need_simplification(
    tool: &Map<String, Value>,
    namespace: &str,
    original_name: &str,
    is_custom: bool,
) -> bool {
    let qualified_automation_name = format!("{CODEX_APP_NAMESPACE}__{AUTOMATION_UPDATE_TOOL_NAME}");
    if !is_custom
        && (original_name.eq_ignore_ascii_case(&qualified_automation_name)
            || (namespace.trim().eq_ignore_ascii_case(CODEX_APP_NAMESPACE)
                && original_name.eq_ignore_ascii_case(AUTOMATION_UPDATE_TOOL_NAME)))
    {
        return true;
    }

    let Some(parameters) = tool.get("parameters").and_then(Value::as_object) else {
        return false;
    };
    ["anyOf", "oneOf"].into_iter().any(|union_name| {
        parameters
            .get(union_name)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches
                    .iter()
                    .any(|branch| !schema_branch_is_object_only(branch))
            })
    })
}

fn schema_branch_is_object_only(branch: &Value) -> bool {
    match branch.get("type") {
        Some(Value::String(value)) => value.trim().eq_ignore_ascii_case("object"),
        Some(Value::Array(values)) if !values.is_empty() => values.iter().all(|value| {
            value
                .as_str()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("object"))
        }),
        _ => false,
    }
}

fn normalize_input_tool_calls(input: &mut Vec<Value>) {
    input.retain(|item| {
        !matches!(
            item.get("type").and_then(Value::as_str),
            Some("tool_search_call" | "tool_search_output")
        )
    });
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
                object.remove("namespace");
                object.remove("name");
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
    json!({"input": input}).to_string()
}

fn collect_available_tool_choice_keys(object: &Map<String, Value>) -> BTreeSet<ToolChoiceKey> {
    let mut keys = BTreeSet::new();
    collect_available_tool_choice_keys_from_array(object.get("tools"), &mut keys);
    if let Some(input) = object.get("input").and_then(Value::as_array) {
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                collect_available_tool_choice_keys_from_array(item.get("tools"), &mut keys);
            }
        }
    }
    keys
}

fn collect_available_tool_choice_keys_from_array(
    tools: Option<&Value>,
    keys: &mut BTreeSet<ToolChoiceKey>,
) {
    let Some(tools) = tools.and_then(Value::as_array) else {
        return;
    };
    for tool in tools {
        let tool_type = tool_type(tool);
        if tool_type.is_empty() {
            continue;
        }
        let name = if tool_type == FUNCTION_TOOL_TYPE {
            let name = string_field(tool, "name");
            if name.is_empty() {
                continue;
            }
            name
        } else {
            String::new()
        };
        keys.insert(ToolChoiceKey { tool_type, name });
    }
}

fn normalize_tool_choice(
    choice: &mut Value,
    available_tools: &BTreeSet<ToolChoiceKey>,
    namespace_as_auto: bool,
) -> bool {
    if choice.is_string() {
        return true;
    }
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
            allowed.retain_mut(|tool| normalize_tool_choice(tool, available_tools, false));
            if allowed.is_empty() {
                return false;
            }
        }
        if object.get("type").and_then(Value::as_str) == Some(NAMESPACE_TOOL_TYPE) {
            if namespace_as_auto {
                *choice = Value::String("auto".to_string());
                return true;
            }
            return false;
        }
        let choice_type = object
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if choice_type == "allowed_tools" {
            return object
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty());
        }
        if choice_type.is_empty() {
            return true;
        }
        let name = if choice_type == FUNCTION_TOOL_TYPE {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if name.is_empty() {
                return false;
            }
            name.to_string()
        } else {
            String::new()
        };
        return available_tools.contains(&ToolChoiceKey {
            tool_type: choice_type.to_string(),
            name,
        });
    }
    false
}

#[derive(Debug, Default)]
pub struct GrokInternalXSearchStreamFilter {
    dropped_output_indexes: BTreeSet<u64>,
    dropped_item_ids: BTreeSet<String>,
}

impl GrokInternalXSearchStreamFilter {
    /// Returns false when the complete SSE payload belongs to an internal
    /// x_search trace and must not be forwarded to the Responses client.
    pub fn apply(&mut self, payload: &mut Value) -> bool {
        if payload
            .get("item")
            .is_some_and(is_grok_internal_x_search_call)
        {
            self.record_dropped_item(payload);
            return false;
        }

        filter_grok_internal_x_search_tool_calls(payload);
        if self.references_dropped_item(payload) {
            return false;
        }
        self.compact_output_index(payload);
        true
    }

    fn record_dropped_item(&mut self, payload: &Value) {
        if let Some(index) = payload.get("output_index").and_then(Value::as_u64) {
            self.dropped_output_indexes.insert(index);
        }
        if let Some(item) = payload.get("item").and_then(Value::as_object) {
            for field in ["id", "call_id"] {
                if let Some(id) = item
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                {
                    self.dropped_item_ids.insert(id.to_string());
                }
            }
        }
    }

    fn references_dropped_item(&self, payload: &Value) -> bool {
        payload
            .get("output_index")
            .and_then(Value::as_u64)
            .is_some_and(|index| self.dropped_output_indexes.contains(&index))
            || ["item_id", "call_id"].into_iter().any(|field| {
                payload
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .is_some_and(|id| self.dropped_item_ids.contains(id))
            })
    }

    fn compact_output_index(&self, payload: &mut Value) {
        let Some(original) = payload.get("output_index").and_then(Value::as_u64) else {
            return;
        };
        let removed_before = self.dropped_output_indexes.range(..original).count() as u64;
        if removed_before == 0 {
            return;
        }
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "output_index".to_string(),
                Value::from(original.saturating_sub(removed_before)),
            );
        }
    }
}

pub fn filter_grok_internal_x_search_tool_calls(value: &mut Value) {
    filter_internal_x_search_output_at_path(
        value
            .get_mut("response")
            .and_then(Value::as_object_mut)
            .and_then(|response| response.get_mut("output")),
    );
    filter_internal_x_search_output_at_path(value.get_mut("output"));
}

fn filter_internal_x_search_output_at_path(output: Option<&mut Value>) {
    let Some(output) = output.and_then(Value::as_array_mut) else {
        return;
    };
    output.retain(|item| !is_grok_internal_x_search_call(item));
}

fn is_grok_internal_x_search_call(item: &Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    if object
        .get("namespace")
        .and_then(Value::as_str)
        .is_some_and(|namespace| !namespace.trim().is_empty())
    {
        return false;
    }
    let internal_name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|name| INTERNAL_X_SEARCH_TOOL_NAMES.contains(&name));
    if !internal_name {
        return false;
    }
    match object.get("type").and_then(Value::as_str) {
        Some("custom_tool_call") => true,
        Some("function_call") => object
            .get("call_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|call_id| call_id.starts_with("xs_call")),
        _ => false,
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

pub fn restore_grok_response_sse_bytes(
    bytes: &[u8],
    refs: &Value,
    filter_internal_x_search: bool,
) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut buffered = bytes.to_vec();
    let mut internal_x_search_filter = GrokInternalXSearchStreamFilter::default();
    while let Some(record) = drain_sse_record(&mut buffered) {
        restore_sse_record(
            &record,
            refs,
            filter_internal_x_search,
            &mut internal_x_search_filter,
            &mut output,
        );
    }
    if !buffered.is_empty() {
        restore_sse_record(
            &buffered,
            refs,
            filter_internal_x_search,
            &mut internal_x_search_filter,
            &mut output,
        );
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

fn restore_sse_record(
    record: &[u8],
    refs: &Value,
    filter_internal_x_search: bool,
    internal_x_search_filter: &mut GrokInternalXSearchStreamFilter,
    output: &mut Vec<u8>,
) {
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
    if filter_internal_x_search && !internal_x_search_filter.apply(&mut payload) {
        return;
    }
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
            Value::String(grok_custom_arguments_to_input(&arguments)),
        );
    }
}

pub fn grok_custom_arguments_to_input(arguments: &str) -> String {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    if let Some(input) = object.get("input") {
        return input
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| input.to_string());
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

    #[test]
    fn round_trips_callable_tools_with_an_inline_namespace_field() {
        let mut request = json!({
            "tools":[{
                "type":"function",
                "namespace":"functions",
                "name":"lookup"
            }]
        });
        let refs = collect_grok_response_tool_refs(&request).unwrap();

        normalize_grok_responses_request_tools(&mut request);

        assert_eq!(request["tools"][0]["name"], "functions__lookup");
        assert!(request["tools"][0].get("namespace").is_none());
        let mut response = json!({
            "output":[{
                "type":"function_call",
                "name":"functions__lookup",
                "arguments":"{}"
            }]
        });
        restore_grok_response_tool_calls(&mut response, &refs);
        assert_eq!(response["output"][0]["namespace"], "functions");
        assert_eq!(response["output"][0]["name"], "lookup");
    }

    #[test]
    fn wraps_json_shaped_custom_input_without_losing_its_raw_text() {
        let original_input = r#"{"input":"literal custom payload"}"#;
        let mut request = json!({
            "tools":[{"type":"custom","name":"custom_json"}],
            "input":[{
                "type":"custom_tool_call",
                "name":"custom_json",
                "call_id":"call_json",
                "input": original_input
            }]
        });
        let refs = collect_grok_response_tool_refs(&request).unwrap();

        normalize_grok_responses_request_tools(&mut request);

        assert_eq!(
            request["input"][0]["arguments"],
            json!({"input": original_input}).to_string()
        );
        let mut response = json!({
            "output":[{
                "type":"function_call",
                "name":"custom_json",
                "call_id":"call_json",
                "arguments": request["input"][0]["arguments"]
            }]
        });
        restore_grok_response_tool_calls(&mut response, &refs);
        assert_eq!(response["output"][0]["input"], original_input);
        assert_eq!(
            grok_custom_arguments_to_input(r#"{"input":"patch","ignored":true}"#),
            "patch"
        );
    }

    #[test]
    fn custom_tools_always_use_the_lossless_input_wrapper_schema() {
        let mut request = json!({
            "tools":[{
                "type":"custom",
                "name":"custom_with_schema",
                "strict":true,
                "parameters":{
                    "type":"object",
                    "properties":{"legacy":{"type":"string"}}
                }
            }]
        });

        normalize_grok_responses_request_tools(&mut request);

        assert_eq!(request["tools"][0]["type"], "function");
        assert_eq!(
            request["tools"][0]["parameters"],
            json!({
                "type":"object",
                "properties":{"input":{}},
                "required":["input"],
                "additionalProperties":false
            })
        );
        assert_eq!(request["tools"][0]["strict"], false);
    }

    #[test]
    fn response_refs_follow_the_first_tool_that_survives_name_deduplication() {
        let mut plain_first = json!({
            "tools":[
                {"type":"function","name":"agents__spawn"},
                {"type":"namespace","name":"agents","tools":[
                    {"type":"function","name":"spawn"}
                ]}
            ]
        });
        assert!(collect_grok_response_tool_refs(&plain_first).is_none());
        normalize_grok_responses_request_tools(&mut plain_first);
        assert_eq!(plain_first["tools"].as_array().unwrap().len(), 1);
        assert_eq!(plain_first["tools"][0]["name"], "agents__spawn");

        let mut namespace_first = json!({
            "tools":[
                {"type":"namespace","name":"agents","tools":[
                    {"type":"function","name":"spawn"}
                ]},
                {"type":"function","name":"agents__spawn"}
            ]
        });
        let refs = collect_grok_response_tool_refs(&namespace_first).unwrap();
        normalize_grok_responses_request_tools(&mut namespace_first);
        let mut response = json!({
            "output":[{"type":"function_call","name":"agents__spawn","arguments":"{}"}]
        });
        restore_grok_response_tool_calls(&mut response, &refs);
        assert_eq!(response["output"][0]["namespace"], "agents");
        assert_eq!(response["output"][0]["name"], "spawn");
    }

    #[test]
    fn filters_tool_search_and_prunes_orphaned_allowed_tool_choices() {
        let mut request = json!({
            "tools": [
                {"type":"tool_search", "description":"discover deferred tools"},
                {"type":"computer_use_preview"},
                {"type":"function", "name":"exec_command"}
            ],
            "input": [{
                "type":"additional_tools",
                "tools":[
                    {"type":"tool_search"},
                    {"type":"x_search"}
                ]
            }, {
                "type":"tool_search_call",
                "id":"ts_1",
                "call_id":"call_ts_1",
                "arguments":"{\"query\":\"agent tools\"}"
            }, {
                "type":"tool_search_output",
                "call_id":"call_ts_1",
                "execution":"client",
                "tools":[{
                    "type":"namespace",
                    "name":"mcp_calendar",
                    "tools":[{
                        "type":"function",
                        "name":"create_event",
                        "defer_loading":true
                    }]
                }]
            }],
            "tool_choice": {
                "type":"allowed_tools",
                "mode":"auto",
                "tools":[
                    {"type":"tool_search"},
                    {"type":"namespace", "name":"multi_agent_v1"},
                    {"type":"function", "name":"exec_command"},
                    {"type":"x_search"}
                ]
            }
        });

        let refs = collect_grok_response_tool_refs(&request).unwrap();
        normalize_grok_responses_request_tools(&mut request);

        assert_eq!(
            request["tools"],
            json!([
                {
                    "type":"function",
                    "name":"exec_command",
                    "parameters":{"type":"object","properties":{}}
                },
                {
                    "type":"function",
                    "name":"mcp_calendar__create_event",
                    "parameters":{"type":"object","properties":{}}
                },
                {"type":"x_search"}
            ])
        );
        assert_eq!(
            refs["mcp_calendar__create_event"]["namespace"],
            "mcp_calendar"
        );
        assert!(request["input"].as_array().unwrap().is_empty());
        assert_eq!(
            request["tool_choice"]["tools"],
            json!([
                {"type":"function", "name":"exec_command"},
                {"type":"x_search"}
            ])
        );
    }

    #[test]
    fn removes_tool_choice_when_tool_search_was_the_only_tool() {
        let mut request = json!({
            "tools": [{"type":"tool_search"}],
            "tool_choice": {"type":"tool_search"},
            "parallel_tool_calls": true
        });

        normalize_grok_responses_request_tools(&mut request);

        assert!(request.get("tools").is_none());
        assert!(request.get("tool_choice").is_none());
        assert!(request.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn removes_malformed_allowed_tools_choice_without_a_tools_array() {
        let mut request = json!({
            "tools": [{"type":"function","name":"exec_command"}],
            "tool_choice": {"type":"allowed_tools","mode":"auto"}
        });

        normalize_grok_responses_request_tools(&mut request);

        assert!(request.get("tool_choice").is_none());
    }

    #[test]
    fn simplifies_codex_automation_and_non_object_root_union_schemas() {
        let mut request = json!({
            "tools": [
                {
                    "type":"namespace",
                    "name":"codex_app",
                    "tools":[{
                        "type":"function",
                        "name":"automation_update",
                        "strict":true,
                        "parameters":{
                            "oneOf":[
                                {"$ref":"#/$defs/create"},
                                {"$ref":"#/$defs/update"}
                            ],
                            "$defs":{"create":{"type":"object"}}
                        }
                    }]
                },
                {
                    "type":"function",
                    "name":"nullable_input",
                    "strict":true,
                    "parameters":{
                        "anyOf":[
                            {"type":"object","properties":{"value":{"type":"string"}}},
                            {"type":"null"}
                        ]
                    }
                },
                {
                    "type":"function",
                    "name":"object_union",
                    "parameters":{
                        "oneOf":[
                            {"type":"object","properties":{"a":{"type":"string"}}},
                            {"type":["object"],"properties":{"b":{"type":"number"}}}
                        ]
                    }
                },
                {
                    "type":"function",
                    "name":"implicit_object_union",
                    "parameters":{
                        "type":"object",
                        "oneOf":[
                            {"properties":{"a":{"type":"string"}}},
                            {"$ref":"#/$defs/update"}
                        ],
                        "$defs":{"update":{"type":"object"}}
                    }
                }
            ]
        });

        normalize_grok_responses_request_tools(&mut request);

        let safe_parameters = json!({
            "type":"object",
            "properties":{},
            "additionalProperties":true
        });
        assert_eq!(request["tools"][0]["name"], "codex_app__automation_update");
        assert_eq!(request["tools"][0]["parameters"], safe_parameters);
        assert_eq!(request["tools"][0]["strict"], false);
        assert_eq!(request["tools"][1]["parameters"], safe_parameters);
        assert_eq!(request["tools"][1]["strict"], false);
        assert_eq!(
            request["tools"][2]["parameters"]["oneOf"][1]["type"],
            json!(["object"])
        );
        assert_eq!(
            request["tools"][3]["parameters"]["oneOf"][0]["type"],
            "object"
        );
        assert_eq!(
            request["tools"][3]["parameters"]["oneOf"][1]["type"],
            "object"
        );
        assert_eq!(
            request["tools"][3]["parameters"]["oneOf"][1]["$ref"],
            "#/$defs/update"
        );
    }

    #[test]
    fn filters_internal_x_search_calls_without_touching_client_function_calls() {
        let mut response = json!({
            "response": {
                "output": [
                    {
                        "type":"custom_tool_call",
                        "id":"xs_item_1",
                        "call_id":"xs_call_1",
                        "name":"x_keyword_search",
                        "input":"rust"
                    },
                    {
                        "type":"function_call",
                        "id":"xs_fc_1",
                        "call_id":"xs_call_function_1",
                        "name":"x_thread_fetch",
                        "arguments":"{}"
                    },
                    {
                        "type":"function_call",
                        "id":"fc_1",
                        "call_id":"call_1",
                        "name":"x_keyword_search",
                        "arguments":"{}"
                    },
                    {"type":"message","role":"assistant","content":[]}
                ]
            }
        });

        filter_grok_internal_x_search_tool_calls(&mut response);

        assert_eq!(response["response"]["output"].as_array().unwrap().len(), 2);
        assert_eq!(response["response"]["output"][0]["type"], "function_call");
        assert_eq!(response["response"]["output"][1]["type"], "message");
    }

    #[test]
    fn x_search_detection_checks_top_level_and_additional_tools() {
        assert!(grok_response_request_uses_x_search(&json!({
            "tools":[{"type":"x_search"}]
        })));
        assert!(grok_response_request_uses_x_search(&json!({
            "input":[{
                "type":"additional_tools",
                "execution":"server",
                "tools":[{"type":"x_search"}]
            }]
        })));
        assert!(!grok_response_request_uses_x_search(&json!({
            "tools":[{"type":"web_search"}]
        })));
    }
}
