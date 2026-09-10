use serde_json::Value;

use crate::AiExecutionDecision;

pub(crate) const HEADER: &str = "x-codex-routing-hint";

/// Derive the hint from the provider body, never from the client model alias.
pub(crate) fn from_body(body: &Value) -> Option<String> {
    let model = body.get("model")?.as_str()?;
    if model.is_empty() || model.contains(';') {
        return None;
    }
    let hint = match body.get("service_tier").and_then(Value::as_str) {
        Some(tier) if !tier.is_empty() => {
            if tier.contains(';') {
                return None;
            }
            format!("model={model};tier={tier}")
        }
        _ => format!("model={model}"),
    };
    hint.bytes()
        .all(|byte| (b' '..=b'~').contains(&byte))
        .then_some(hint)
}

pub(crate) fn apply_to_decision(provider_type: &str, decision: &mut AiExecutionDecision) {
    if !provider_type.trim().eq_ignore_ascii_case("codex")
        || !matches!(
            crate::ai_serving::normalize_api_format_alias(
                decision.provider_api_format.as_deref().unwrap_or_default()
            )
            .as_str(),
            "openai:responses" | "openai:responses:compact"
        )
    {
        return;
    }
    decision
        .provider_request_headers
        .retain(|name, _| !name.eq_ignore_ascii_case(HEADER));
    if let Some(hint) = decision.provider_request_body.as_ref().and_then(from_body) {
        decision
            .provider_request_headers
            .insert(HEADER.to_string(), hint);
    }
    if let Some(context) = decision
        .report_context
        .as_mut()
        .and_then(Value::as_object_mut)
    {
        context.insert(
            "provider_request_headers".to_string(),
            serde_json::json!(decision.provider_request_headers),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn routing_hint_uses_model_and_optional_service_tier() {
        for (body, expected) in [
            (json!({"model":"gpt-5.5"}), Some("model=gpt-5.5")),
            (
                json!({"model":"gpt-5.5", "service_tier":null}),
                Some("model=gpt-5.5"),
            ),
            (
                json!({"model":"gpt-5.5", "service_tier":"priority"}),
                Some("model=gpt-5.5;tier=priority"),
            ),
            (
                json!({"model":"gpt-5.5", "service_tier":"flex"}),
                Some("model=gpt-5.5;tier=flex"),
            ),
            (
                json!({"model":"gpt-5.5", "service_tier":""}),
                Some("model=gpt-5.5"),
            ),
            (json!({}), None),
            (json!({"model":""}), None),
            (json!({"model":"gpt-5.5\r\nx-other: injected"}), None),
            (json!({"model":"gpt-5.5;tier=priority"}), None),
            (
                json!({"model":"gpt-5.5", "service_tier":"priority;model=other"}),
                None,
            ),
        ] {
            assert_eq!(from_body(&body).as_deref(), expected, "{body}");
        }
    }

    #[test]
    fn routing_hint_replaces_stale_headers_and_updates_audit_after_body_rules() {
        for api_format in ["openai:responses", "openai:responses:compact"] {
            let mut decision: AiExecutionDecision = serde_json::from_value(json!({
                "action":"execute",
                "provider_api_format":api_format,
                "model_name":"gpt-5.6-luna",
                "mapped_model":"gpt-5.5",
                "provider_request_body":{"model":"gpt-5.5", "service_tier":"priority"},
                "provider_request_headers":{
                    "X-Codex-Routing-Hint":"model=stale",
                    "x-codex-routing-hint":"model=another-stale",
                    "authorization":"Bearer test"
                },
                "report_context":{}
            }))
            .unwrap();
            apply_to_decision("codex", &mut decision);
            assert_eq!(decision.provider_request_headers.len(), 2);
            assert_eq!(
                decision.provider_request_headers[HEADER],
                "model=gpt-5.5;tier=priority"
            );
            assert_eq!(
                decision.report_context.as_ref().unwrap()["provider_request_headers"],
                json!(decision.provider_request_headers)
            );
            decision.provider_request_body = Some(json!({"model":"gpt-5.5"}));
            apply_to_decision("codex", &mut decision);
            assert_eq!(decision.provider_request_headers[HEADER], "model=gpt-5.5");
        }
    }

    #[test]
    fn routing_hint_does_not_change_other_providers_or_endpoints() {
        for (provider, format) in [
            ("openai", "openai:responses"),
            ("codex", "openai:search"),
            ("codex", "openai:image"),
        ] {
            let mut decision: AiExecutionDecision = serde_json::from_value(json!({
                "action":"execute", "provider_api_format":format,
                "provider_request_body":{"model":"gpt-5.5"},
                "provider_request_headers":{"x-codex-routing-hint":"custom"}
            }))
            .unwrap();
            apply_to_decision(provider, &mut decision);
            assert_eq!(decision.provider_request_headers[HEADER], "custom");
        }
    }
}
