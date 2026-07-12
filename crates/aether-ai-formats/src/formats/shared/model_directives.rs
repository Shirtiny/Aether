use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDirective {
    pub base_model: String,
    pub overrides: Vec<ModelOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelOverride {
    ReasoningEffort(ReasoningEffort),
    ServiceTier(ServiceTier),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }

    pub fn as_openai_chat_value(self, model: &str) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "high",
            Self::Max if model_supports_codex_max_ultra(model) => "max",
            Self::Max => "high",
            // Codex implements `ultra` as wire-level `max` plus client-side delegation behavior.
            Self::Ultra => "max",
        }
    }

    pub fn as_openai_responses_value(self, model: &str) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max if model_supports_codex_max_ultra(model) => "max",
            Self::Max => "xhigh",
            // Codex keeps `ultra` as a client mode and sends `max` to Responses.
            Self::Ultra => "max",
        }
    }

    pub fn as_claude_output_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max | Self::Ultra => "max",
        }
    }

    pub fn as_gemini_level_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::XHigh | Self::Max | Self::Ultra => "high",
        }
    }

    pub fn thinking_budget_tokens(self) -> u64 {
        match self {
            Self::Low => 1280,
            Self::Medium => 2048,
            Self::High => 4096,
            Self::XHigh | Self::Max | Self::Ultra => 8192,
        }
    }
}

pub fn model_supports_codex_max_ultra(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "gpt-5.6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceTier {
    Priority,
}

impl ServiceTier {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(Self::Priority),
            _ => None,
        }
    }

    pub fn as_openai_value(self) -> &'static str {
        match self {
            Self::Priority => "priority",
        }
    }
}

pub fn parse_model_directive(model: &str) -> Option<ModelDirective> {
    let (base_model, overrides) = parse_model_directive_parts(model)?;
    Some(ModelDirective {
        base_model,
        overrides,
    })
}

fn parse_model_directive_parts(model: &str) -> Option<(String, Vec<ModelOverride>)> {
    let mut base_model = model.trim();
    let mut overrides = ModelOverrideAccumulator::default();
    while let Some((candidate_base, suffix)) = base_model.rsplit_once('-') {
        let Some(override_item) = parse_model_override(suffix) else {
            break;
        };
        overrides.insert(override_item)?;
        base_model = candidate_base.trim();
    }
    if base_model.is_empty() {
        return None;
    }
    if overrides.reasoning_effort == Some(ReasoningEffort::Ultra)
        && !model_supports_codex_max_ultra(base_model)
    {
        return None;
    }
    let overrides = overrides.into_overrides()?;
    Some((base_model.to_string(), overrides))
}

fn parse_model_override(suffix: &str) -> Option<ModelOverride> {
    ReasoningEffort::parse(suffix)
        .map(ModelOverride::ReasoningEffort)
        .or_else(|| ServiceTier::parse(suffix).map(ModelOverride::ServiceTier))
}

#[derive(Default)]
struct ModelOverrideAccumulator {
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<ServiceTier>,
}

impl ModelOverrideAccumulator {
    fn insert(&mut self, override_item: ModelOverride) -> Option<()> {
        match override_item {
            ModelOverride::ReasoningEffort(value) => {
                if self.reasoning_effort.replace(value).is_some() {
                    return None;
                }
            }
            ModelOverride::ServiceTier(value) => {
                if self.service_tier.replace(value).is_some() {
                    return None;
                }
            }
        }
        Some(())
    }

    fn into_overrides(self) -> Option<Vec<ModelOverride>> {
        let mut overrides = Vec::new();
        if let Some(reasoning_effort) = self.reasoning_effort {
            overrides.push(ModelOverride::ReasoningEffort(reasoning_effort));
        }
        if let Some(service_tier) = self.service_tier {
            overrides.push(ModelOverride::ServiceTier(service_tier));
        }
        (!overrides.is_empty()).then_some(overrides)
    }
}

pub fn model_directive_base_model(model: &str) -> Option<String> {
    parse_model_directive(model).map(|directive| directive.base_model)
}

pub(crate) fn model_directive_display_model(model: &str) -> Option<String> {
    let model = model.trim();
    parse_model_directive(model)?;
    Some(model.to_string())
}

pub(crate) fn model_directive_display_model_from_report_context(
    report_context: &Value,
) -> Option<String> {
    report_context
        .get("model")
        .and_then(Value::as_str)
        .and_then(model_directive_display_model)
}

pub fn normalize_model_directive_model(model: &str) -> String {
    parse_model_directive(model)
        .map(|directive| directive.base_model)
        .unwrap_or_else(|| model.trim().to_string())
}

pub fn apply_model_directive_overrides_from_request(
    provider_request_body: &mut Value,
    provider_api_format: &str,
    provider_model: &str,
    request_body: &Value,
    request_path: Option<&str>,
) -> Option<ModelDirective> {
    let source_model = request_body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| request_path.and_then(extract_gemini_model_from_path))?;

    apply_model_directive_overrides_from_model(
        provider_request_body,
        provider_api_format,
        provider_model,
        &source_model,
    )
}

pub fn apply_model_directive_overrides_from_model(
    provider_request_body: &mut Value,
    provider_api_format: &str,
    provider_model: &str,
    source_model: &str,
) -> Option<ModelDirective> {
    normalize_codex_effort_alias(provider_request_body, provider_api_format, source_model);
    let directive = parse_model_directive(source_model)?;
    let mut patched_body = provider_request_body.clone();
    for override_item in &directive.overrides {
        match override_item {
            ModelOverride::ReasoningEffort(effort) => {
                apply_reasoning_effort_override(
                    &mut patched_body,
                    provider_api_format,
                    provider_model,
                    &directive.base_model,
                    *effort,
                )?;
            }
            ModelOverride::ServiceTier(tier) => {
                apply_service_tier_override(&mut patched_body, provider_api_format, *tier)?;
            }
        }
    }
    *provider_request_body = patched_body;
    Some(directive)
}

fn normalize_codex_effort_alias(
    provider_request_body: &mut Value,
    provider_api_format: &str,
    source_model: &str,
) {
    let source_base_model = parse_model_directive(source_model)
        .map(|directive| directive.base_model)
        .unwrap_or_else(|| source_model.trim().to_string());
    if !model_supports_codex_max_ultra(&source_base_model) {
        return;
    }

    match crate::normalize_api_format_alias(provider_api_format).as_str() {
        "openai:chat" => {
            let Some(object) = provider_request_body.as_object_mut() else {
                return;
            };
            if let Some(effort) = object
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .and_then(normalize_gpt_5_6_reasoning_effort)
            {
                object.insert(
                    "reasoning_effort".to_string(),
                    Value::String(effort.to_string()),
                );
            }
        }
        "openai:responses" | "openai:responses:compact" => {
            let Some(reasoning) = provider_request_body
                .get_mut("reasoning")
                .and_then(Value::as_object_mut)
            else {
                return;
            };
            if let Some(effort) = reasoning
                .get("effort")
                .and_then(Value::as_str)
                .and_then(normalize_gpt_5_6_reasoning_effort)
            {
                reasoning.insert("effort".to_string(), Value::String(effort.to_string()));
            }
        }
        _ => {}
    }
}

fn normalize_gpt_5_6_reasoning_effort(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "ultra" => Some("max"),
        // GPT-5.6 does not accept a disabled-reasoning level. Preserve the
        // caller's lowest-effort intent instead of forwarding an upstream 400.
        "off" | "none" | "minimal" => Some("low"),
        _ => None,
    }
}

pub fn apply_model_directive_mapping_patch(
    provider_request_body: &mut Value,
    patch: &Value,
) -> Option<()> {
    deep_merge_json(provider_request_body, patch);
    Some(())
}

fn deep_merge_json(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target_object), Value::Object(patch_object)) => {
            for (key, patch_value) in patch_object {
                match target_object.get_mut(key) {
                    Some(target_value) => deep_merge_json(target_value, patch_value),
                    None => {
                        target_object.insert(key.clone(), patch_value.clone());
                    }
                }
            }
        }
        (target, patch) => {
            *target = patch.clone();
        }
    }
}

fn apply_reasoning_effort_override(
    provider_request_body: &mut Value,
    provider_api_format: &str,
    provider_model: &str,
    source_model: &str,
    effort: ReasoningEffort,
) -> Option<()> {
    match crate::normalize_api_format_alias(provider_api_format).as_str() {
        "openai:chat" => set_object_string(
            provider_request_body,
            "reasoning_effort",
            effort.as_openai_chat_value(source_model),
        ),
        "openai:responses" | "openai:responses:compact" => {
            set_openai_responses_reasoning_effort(provider_request_body, effort, source_model)
        }
        "claude:messages" | "gemini:generate_content" if effort == ReasoningEffort::Ultra => None,
        "claude:messages" => {
            set_claude_reasoning_effort(provider_request_body, effort, provider_model)
        }
        "gemini:generate_content" => {
            set_gemini_reasoning_effort(provider_request_body, effort, provider_model)
        }
        _ => None,
    }
}

fn apply_service_tier_override(
    provider_request_body: &mut Value,
    provider_api_format: &str,
    tier: ServiceTier,
) -> Option<()> {
    match crate::normalize_api_format_alias(provider_api_format).as_str() {
        "openai:chat" | "openai:responses" | "openai:responses:compact" => set_object_string(
            provider_request_body,
            "service_tier",
            tier.as_openai_value(),
        ),
        _ => None,
    }
}

fn set_object_string(body: &mut Value, key: &str, value: &str) -> Option<()> {
    body.as_object_mut()?
        .insert(key.to_string(), Value::String(value.to_string()));
    Some(())
}

fn set_openai_responses_reasoning_effort(
    body: &mut Value,
    effort: ReasoningEffort,
    source_model: &str,
) -> Option<()> {
    let body_object = body.as_object_mut()?;
    let reasoning = body_object
        .entry("reasoning".to_string())
        .or_insert_with(|| json!({}));
    if !reasoning.is_object() {
        *reasoning = json!({});
    }
    reasoning.as_object_mut()?.insert(
        "effort".to_string(),
        Value::String(effort.as_openai_responses_value(source_model).to_string()),
    );
    Some(())
}

fn set_claude_reasoning_effort(
    body: &mut Value,
    effort: ReasoningEffort,
    provider_model: &str,
) -> Option<()> {
    let body_object = body.as_object_mut()?;
    let output_config = body_object
        .entry("output_config".to_string())
        .or_insert_with(|| json!({}));
    if !output_config.is_object() {
        *output_config = json!({});
    }
    output_config.as_object_mut()?.insert(
        "effort".to_string(),
        Value::String(effort.as_claude_output_value().to_string()),
    );

    let thinking = body_object
        .entry("thinking".to_string())
        .or_insert_with(|| json!({}));
    if !thinking.is_object() {
        *thinking = json!({});
    }
    let thinking = thinking.as_object_mut()?;
    if claude_model_uses_adaptive_effort(provider_model) {
        thinking.insert("type".to_string(), Value::String("adaptive".to_string()));
        thinking.remove("budget_tokens");
    } else {
        thinking.insert("type".to_string(), Value::String("enabled".to_string()));
        thinking.insert(
            "budget_tokens".to_string(),
            Value::from(effort.thinking_budget_tokens()),
        );
    }
    Some(())
}

fn set_gemini_reasoning_effort(
    body: &mut Value,
    effort: ReasoningEffort,
    provider_model: &str,
) -> Option<()> {
    let body_object = body.as_object_mut()?;
    let generation_key = if body_object.contains_key("generation_config")
        && !body_object.contains_key("generationConfig")
    {
        "generation_config"
    } else {
        "generationConfig"
    };
    let generation_config = body_object
        .entry(generation_key.to_string())
        .or_insert_with(|| json!({}));
    if !generation_config.is_object() {
        *generation_config = json!({});
    }
    let generation_config = generation_config.as_object_mut()?;
    let thinking_key = if generation_config.contains_key("thinking_config")
        && !generation_config.contains_key("thinkingConfig")
    {
        "thinking_config"
    } else {
        "thinkingConfig"
    };
    generation_config.insert(
        thinking_key.to_string(),
        gemini_reasoning_effort_config(effort, provider_model, thinking_key),
    );
    Some(())
}

fn gemini_reasoning_effort_config(
    effort: ReasoningEffort,
    provider_model: &str,
    thinking_key: &str,
) -> Value {
    if gemini_model_uses_thinking_level(provider_model) {
        if thinking_key == "thinking_config" {
            return json!({
                "include_thoughts": true,
                "thinking_level": effort.as_gemini_level_value(),
            });
        }
        return json!({
            "includeThoughts": true,
            "thinkingLevel": effort.as_gemini_level_value(),
        });
    }

    if thinking_key == "thinking_config" {
        return json!({
            "include_thoughts": true,
            "thinking_budget": effort.thinking_budget_tokens(),
        });
    }
    json!({
        "includeThoughts": true,
        "thinkingBudget": effort.thinking_budget_tokens(),
    })
}

pub fn claude_model_uses_adaptive_effort(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase().replace(['.', '_'], "-");
    model.contains("mythos")
        || model.contains("opus-4-7")
        || model.contains("opus-4-6")
        || model.contains("sonnet-4-6")
}

pub fn gemini_model_uses_thinking_level(model: &str) -> bool {
    model
        .trim()
        .to_ascii_lowercase()
        .split('/')
        .any(|part| part.starts_with("gemini-3"))
}

pub fn extract_gemini_model_from_path(path: &str) -> Option<String> {
    let marker = "/models/";
    let start = path.find(marker)? + marker.len();
    let tail = &path[start..];
    let end = tail.find(':').unwrap_or(tail.len());
    let model = tail[..end].trim();
    (!model.is_empty()).then(|| model.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        apply_model_directive_overrides_from_model, parse_model_directive, ModelDirective,
        ModelOverride, ReasoningEffort, ServiceTier,
    };

    #[test]
    fn parses_supported_reasoning_effort_suffixes() {
        assert_eq!(
            parse_model_directive("gpt-5.4-xhigh"),
            Some(ModelDirective {
                base_model: "gpt-5.4".to_string(),
                overrides: vec![ModelOverride::ReasoningEffort(ReasoningEffort::XHigh)],
            })
        );
        assert_eq!(
            parse_model_directive("gpt-5.4-MAX"),
            Some(ModelDirective {
                base_model: "gpt-5.4".to_string(),
                overrides: vec![ModelOverride::ReasoningEffort(ReasoningEffort::Max)],
            })
        );
        for base_model in ["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"] {
            assert_eq!(
                parse_model_directive(&format!("{base_model}-ULTRA")),
                Some(ModelDirective {
                    base_model: base_model.to_string(),
                    overrides: vec![ModelOverride::ReasoningEffort(ReasoningEffort::Ultra)],
                })
            );
        }
    }

    #[test]
    fn parses_supported_service_tier_suffixes() {
        assert_eq!(
            parse_model_directive("gpt-5.4-fast"),
            Some(ModelDirective {
                base_model: "gpt-5.4".to_string(),
                overrides: vec![ModelOverride::ServiceTier(ServiceTier::Priority)],
            })
        );
    }

    #[test]
    fn parses_combined_suffixes_in_canonical_order() {
        let expected = Some(ModelDirective {
            base_model: "gpt-5.4".to_string(),
            overrides: vec![
                ModelOverride::ReasoningEffort(ReasoningEffort::XHigh),
                ModelOverride::ServiceTier(ServiceTier::Priority),
            ],
        });
        assert_eq!(parse_model_directive("gpt-5.4-fast-xhigh"), expected);
        assert_eq!(parse_model_directive("gpt-5.4-xhigh-fast"), expected);
    }

    #[test]
    fn ignores_unknown_or_incomplete_suffixes() {
        assert_eq!(parse_model_directive("gpt-5.4-extreme"), None);
        assert_eq!(parse_model_directive("gpt-5.4-ultra"), None);
        assert_eq!(parse_model_directive("gpt-5.4"), None);
        assert_eq!(parse_model_directive("-high"), None);
        assert_eq!(parse_model_directive("gpt-5.4-high-json"), None);
        assert_eq!(parse_model_directive("gpt-5.4-low-high"), None);
    }

    #[test]
    fn applies_reasoning_effort_to_provider_body_shapes() {
        let mut openai_chat = json!({"model": "gpt-5-upstream", "reasoning_effort": "low"});
        apply_model_directive_overrides_from_model(
            &mut openai_chat,
            "openai:chat",
            "gpt-5-upstream",
            "gpt-5.4-xhigh",
        )
        .expect("directive should apply");
        assert_eq!(openai_chat["reasoning_effort"], "high");

        apply_model_directive_overrides_from_model(
            &mut openai_chat,
            "openai:chat",
            "gpt-5.6-sol",
            "gpt-5.6-sol-max",
        )
        .expect("max directive should apply to chat");
        assert_eq!(openai_chat["reasoning_effort"], "max");

        apply_model_directive_overrides_from_model(
            &mut openai_chat,
            "openai:chat",
            "gpt-5.6-sol",
            "gpt-5.6-sol-ultra",
        )
        .expect("ultra directive should clamp to the highest chat effort");
        assert_eq!(openai_chat["reasoning_effort"], "max");

        let mut responses = json!({
            "model": "gpt-5-upstream",
            "reasoning": {"effort": "low", "summary": "auto"}
        });
        apply_model_directive_overrides_from_model(
            &mut responses,
            "openai:responses",
            "gpt-5-upstream",
            "gpt-5.4-max",
        )
        .expect("directive should apply");
        assert_eq!(responses["reasoning"]["effort"], "xhigh");
        assert_eq!(responses["reasoning"]["summary"], "auto");

        apply_model_directive_overrides_from_model(
            &mut responses,
            "openai:responses",
            "gpt-5.6-sol",
            "gpt-5.6-sol-ultra",
        )
        .expect("ultra directive should apply");
        assert_eq!(responses["reasoning"]["effort"], "max");

        let mut claude = json!({"model": "claude-sonnet-4-5"});
        apply_model_directive_overrides_from_model(
            &mut claude,
            "claude:messages",
            "claude-sonnet-4-5",
            "gpt-5.4-high",
        )
        .expect("directive should apply");
        assert_eq!(claude["thinking"]["budget_tokens"], 4096);

        let mut gemini = json!({});
        apply_model_directive_overrides_from_model(
            &mut gemini,
            "gemini:generate_content",
            "gemini-2.5-pro",
            "gpt-5.4-medium",
        )
        .expect("directive should apply");
        assert_eq!(
            gemini["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            2048
        );
    }

    #[test]
    fn normalizes_direct_gpt_5_6_ultra_body_effort_to_wire_max() {
        let mut responses = json!({
            "model": "gpt-5.6-sol",
            "reasoning": {"effort": "ULTRA", "summary": "detailed"}
        });

        assert!(apply_model_directive_overrides_from_model(
            &mut responses,
            "openai:responses",
            "gpt-5.6-sol",
            "gpt-5.6-sol",
        )
        .is_none());
        assert_eq!(responses["reasoning"]["effort"], "max");
        assert_eq!(responses["reasoning"]["summary"], "detailed");

        let mut legacy = json!({
            "model": "gpt-5.4",
            "reasoning": {"effort": "ultra"}
        });
        assert!(apply_model_directive_overrides_from_model(
            &mut legacy,
            "openai:responses",
            "gpt-5.4",
            "gpt-5.4",
        )
        .is_none());
        assert_eq!(legacy["reasoning"]["effort"], "ultra");
    }

    #[test]
    fn normalizes_disabled_gpt_5_6_effort_to_wire_low() {
        for unsupported in ["off", "none", "minimal"] {
            let mut responses = json!({
                "model": "gpt-5.6-sol",
                "reasoning": {"effort": unsupported, "summary": "auto"}
            });
            assert!(apply_model_directive_overrides_from_model(
                &mut responses,
                "openai:responses",
                "gpt-5.6-sol",
                "gpt-5.6-sol",
            )
            .is_none());
            assert_eq!(responses["reasoning"]["effort"], "low");
            assert_eq!(responses["reasoning"]["summary"], "auto");

            let mut chat = json!({
                "model": "gpt-5.6-terra",
                "reasoning_effort": unsupported
            });
            assert!(apply_model_directive_overrides_from_model(
                &mut chat,
                "openai:chat",
                "gpt-5.6-terra",
                "gpt-5.6-terra",
            )
            .is_none());
            assert_eq!(chat["reasoning_effort"], "low");
        }

        let mut legacy = json!({
            "model": "gpt-5.4",
            "reasoning": {"effort": "off"}
        });
        assert!(apply_model_directive_overrides_from_model(
            &mut legacy,
            "openai:responses",
            "gpt-5.4",
            "gpt-5.4",
        )
        .is_none());
        assert_eq!(legacy["reasoning"]["effort"], "off");
    }

    #[test]
    fn applies_fast_suffix_to_openai_service_tier() {
        let mut openai_chat = json!({"model": "gpt-5-upstream"});
        apply_model_directive_overrides_from_model(
            &mut openai_chat,
            "openai:chat",
            "gpt-5-upstream",
            "gpt-5.4-fast",
        )
        .expect("directive should apply");
        assert_eq!(openai_chat["service_tier"], "priority");

        let mut responses = json!({"model": "gpt-5-upstream"});
        apply_model_directive_overrides_from_model(
            &mut responses,
            "openai:responses",
            "gpt-5-upstream",
            "gpt-5.4-fast",
        )
        .expect("directive should apply");
        assert_eq!(responses["service_tier"], "priority");
    }

    #[test]
    fn applies_combined_suffixes_to_openai_body() {
        let mut openai_chat = json!({"model": "gpt-5-upstream", "reasoning_effort": "low"});
        apply_model_directive_overrides_from_model(
            &mut openai_chat,
            "openai:chat",
            "gpt-5-upstream",
            "gpt-5.4-fast-xhigh",
        )
        .expect("directive should apply");
        assert_eq!(openai_chat["reasoning_effort"], "high");
        assert_eq!(openai_chat["service_tier"], "priority");

        let mut reversed = json!({"model": "gpt-5-upstream", "reasoning_effort": "low"});
        apply_model_directive_overrides_from_model(
            &mut reversed,
            "openai:chat",
            "gpt-5-upstream",
            "gpt-5.4-xhigh-fast",
        )
        .expect("directive should apply");
        assert_eq!(reversed, openai_chat);
    }

    #[test]
    fn unsupported_combined_suffix_leaves_body_unchanged() {
        let mut claude = json!({"model": "claude-sonnet-4-5"});
        let original = claude.clone();
        assert!(apply_model_directive_overrides_from_model(
            &mut claude,
            "claude:messages",
            "claude-sonnet-4-5",
            "gpt-5.4-fast-xhigh",
        )
        .is_none());
        assert_eq!(claude, original);
    }
}
