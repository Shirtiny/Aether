use serde_json::Value;

use crate::AiExecutionDecision;

pub(crate) const HEADER: &str = "x-codex-routing-hint";
pub(crate) const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";

/// codex-rs flags a Responses Lite request in two places at once, both keyed
/// on `model_info.use_responses_lite` from the server's model manifest: the
/// `x-openai-internal-codex-responses-lite: true` header and a body whose
/// `reasoning.context` is `all_turns` with instructions/tools folded into
/// `input[]` (`core/src/client.rs` `build_reasoning_param`,
/// `build_responses_request`, `add_responses_lite_header`). The backend
/// rejects the header on its own with 400 "requires `reasoning.context` to be
/// `all_turns`". Aether does not rewrite bodies into the lite contract and
/// does not fetch the manifest, so the body is the only trustworthy signal:
/// the header follows it in both directions, whatever the model is called.
pub(crate) fn body_uses_responses_lite(body: &Value) -> bool {
    body.get("reasoning")
        .and_then(|reasoning| reasoning.get("context"))
        .and_then(Value::as_str)
        .is_some_and(|context| context == "all_turns")
}

pub(crate) fn apply_responses_lite_header(
    provider_type: &str,
    provider_api_format: &str,
    headers: &mut std::collections::BTreeMap<String, String>,
    body: Option<&Value>,
) {
    if !provider_type.trim().eq_ignore_ascii_case("codex")
        || !matches!(
            crate::ai_serving::normalize_api_format_alias(provider_api_format).as_str(),
            "openai:responses" | "openai:responses:compact"
        )
    {
        return;
    }
    headers.retain(|name, _| !name.eq_ignore_ascii_case(RESPONSES_LITE_HEADER));
    if body.is_some_and(body_uses_responses_lite) {
        headers.insert(RESPONSES_LITE_HEADER.to_string(), "true".to_string());
    }
}

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
    apply_responses_lite_header(
        provider_type,
        decision.provider_api_format.as_deref().unwrap_or_default(),
        &mut decision.provider_request_headers,
        decision.provider_request_body.as_ref(),
    );
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

    fn lite_decision(
        api_format: &str,
        body: Value,
        inbound_lite: Option<&str>,
    ) -> AiExecutionDecision {
        let mut headers = json!({"authorization":"Bearer test"});
        if let Some(value) = inbound_lite {
            headers[RESPONSES_LITE_HEADER] = json!(value);
        }
        serde_json::from_value(json!({
            "action":"execute",
            "provider_api_format":api_format,
            "provider_request_body":body,
            "provider_request_headers":headers,
            "report_context":{}
        }))
        .unwrap()
    }

    #[test]
    fn responses_lite_header_follows_the_body_not_the_model_name() {
        // codex-rs sends header and `reasoning.context = all_turns` together:
        // kept, value normalised.
        let mut decision = lite_decision(
            "openai:responses",
            json!({"model":"gpt-5.6-sol","reasoning":{"effort":"high","context":"all_turns"}}),
            Some("stale"),
        );
        apply_to_decision("codex", &mut decision);
        assert_eq!(
            decision.provider_request_headers[RESPONSES_LITE_HEADER],
            "true"
        );

        // A lite-capable model name with a plain body (third-party client or
        // an older codex-rs without the manifest flag): the header must not be
        // synthesised, the backend answers 400 to header-without-body.
        for body in [
            json!({"model":"gpt-5.6-sol","reasoning":{"effort":"high"}}),
            json!({"model":"gpt-6-astra","reasoning":{"effort":"high","context":"current_turn"}}),
            json!({"model":"gpt-5.6-luna"}),
        ] {
            let mut decision = lite_decision("openai:responses", body.clone(), Some("true"));
            apply_to_decision("codex", &mut decision);
            assert!(
                !decision
                    .provider_request_headers
                    .contains_key(RESPONSES_LITE_HEADER),
                "{body}"
            );
        }

        // The body carries the lite marker but the header was lost on the way
        // in: restored, also on compact and for a model outside any list.
        for api_format in ["openai:responses", "openai:responses:compact"] {
            let mut decision = lite_decision(
                api_format,
                json!({"model":"gpt-5.5","reasoning":{"context":"all_turns"}}),
                None,
            );
            apply_to_decision("codex", &mut decision);
            assert_eq!(
                decision.provider_request_headers[RESPONSES_LITE_HEADER], "true",
                "{api_format}"
            );
        }

        // No body at all: nothing to follow, header dropped.
        let mut headers = std::collections::BTreeMap::from([(
            RESPONSES_LITE_HEADER.to_string(),
            "true".to_string(),
        )]);
        apply_responses_lite_header("codex", "openai:responses", &mut headers, None);
        assert!(headers.is_empty());
    }

    #[test]
    fn body_uses_responses_lite_reads_only_reasoning_context() {
        assert!(body_uses_responses_lite(
            &json!({"reasoning":{"context":"all_turns"}})
        ));
        for body in [
            json!({"reasoning":{"context":"current_turn"}}),
            json!({"reasoning":{"context":"auto"}}),
            json!({"reasoning":{"context":true}}),
            json!({"reasoning":{"effort":"high"}}),
            json!({"reasoning":"all_turns"}),
            json!({"context":"all_turns"}),
            json!({}),
        ] {
            assert!(!body_uses_responses_lite(&body), "{body}");
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
