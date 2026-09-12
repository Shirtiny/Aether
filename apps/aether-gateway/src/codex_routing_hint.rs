use serde_json::Value;

use crate::codex_model_catalog::CodexModelCatalogSnapshot;
use crate::AiExecutionDecision;

pub(crate) const HEADER: &str = "x-codex-routing-hint";
pub(crate) const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";

/// codex-rs `client_metadata` key carrying the lite flag on a WebSocket step
/// body (`core/src/client.rs`; the value is the string `"true"`, the map is
/// `HashMap<String, String>` there).
pub(crate) const WS_RESPONSES_LITE_METADATA_KEY: &str =
    "ws_request_header_x_openai_internal_codex_responses_lite";

/// Whether `model` is Responses Lite-capable according to the server manifest
/// (`use_responses_lite`). Three sources in order (`codex_model_catalog`): the
/// live manifest snapshot when it lists the slug, the bundled slim manifest
/// when it does not, `false` when neither knows the slug.
pub(crate) fn model_name_uses_responses_lite(model: &str) -> bool {
    model_name_uses_responses_lite_with(
        crate::codex_model_catalog::current_snapshot().as_deref(),
        model,
    )
}

/// Pure form: a live hit (`true` or `false`) wins, a miss falls back to the
/// bundled manifest.
pub(crate) fn model_name_uses_responses_lite_with(
    live: Option<&CodexModelCatalogSnapshot>,
    model: &str,
) -> bool {
    let model = model.trim();
    if model.is_empty() {
        return false;
    }
    live.and_then(|snapshot| snapshot.uses_responses_lite(model))
        .or_else(|| crate::codex_model_catalog::bundled_snapshot().uses_responses_lite(model))
        .unwrap_or(false)
}

/// codex-rs flags a Responses Lite request in two places at once, both keyed
/// on `model_info.use_responses_lite` from the server's model manifest: the
/// `x-openai-internal-codex-responses-lite: true` header and a body whose
/// `reasoning.context` is `all_turns` with instructions/tools folded into
/// `input[]` (`core/src/client.rs` `build_reasoning_param`,
/// `build_responses_request`, `add_responses_lite_header`). The backend
/// checks both halves against the model it actually serves:
///
/// * header without a lite body → 400 "requires `reasoning.context` to be
///   `all_turns`" (third-party clients, older codex-rs);
/// * header on a model that is not lite-capable → 400 "This model is not
///   supported when using X-OpenAI-Internal-Codex-Responses-Lite". Aether
///   hits this when a pool remaps a lite alias (`gpt-5.6-luna`) to a plain
///   target (`gpt-5.5`): the client shaped a lite body for luna, the target
///   is not luna;
/// * a lite body without the header on a plain target is accepted.
///
/// So the header is sent only when the body is lite-shaped *and* the outbound
/// target model is lite-capable. Aether never rewrites the body contract.
pub(crate) fn body_uses_responses_lite(body: &Value) -> bool {
    body.get("reasoning")
        .and_then(|reasoning| reasoning.get("context"))
        .and_then(Value::as_str)
        .is_some_and(|context| context == "all_turns")
}

/// `target_model` is the model the request is sent as (the pool's mapped
/// model). When absent, the body's own `model` is used.
pub(crate) fn apply_responses_lite_header(
    provider_type: &str,
    provider_api_format: &str,
    headers: &mut std::collections::BTreeMap<String, String>,
    body: Option<&Value>,
    target_model: Option<&str>,
) {
    apply_responses_lite_header_with(
        crate::codex_model_catalog::current_snapshot().as_deref(),
        provider_type,
        provider_api_format,
        headers,
        body,
        target_model,
    )
}

fn apply_responses_lite_header_with(
    live: Option<&CodexModelCatalogSnapshot>,
    provider_type: &str,
    provider_api_format: &str,
    headers: &mut std::collections::BTreeMap<String, String>,
    body: Option<&Value>,
    target_model: Option<&str>,
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
    let target = target_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| {
            body.and_then(|body| body.get("model"))
                .and_then(Value::as_str)
        });
    if body.is_some_and(body_uses_responses_lite)
        && target.is_some_and(|target| model_name_uses_responses_lite_with(live, target))
    {
        headers.insert(RESPONSES_LITE_HEADER.to_string(), "true".to_string());
    }
}

/// WebSocket step-body twin of [`apply_responses_lite_header`]. The WS
/// handshake carries no lite header; codex-rs puts the flag into every step
/// body as `client_metadata.ws_request_header_x_openai_internal_codex_responses_lite`
/// (`"true"`), and the backend checks it against the served model exactly
/// like the HTTP header. The inbound value is what the client shaped for its
/// own alias; when the pool remaps a lite alias to a plain target that flag
/// is the WS form of "header on a non-lite model" (400, client falls back to
/// HTTP). Same rule: set when the body is lite-shaped and `mapped_model` is
/// lite-capable, removed otherwise. A body without a `client_metadata` object
/// is left without one.
pub(crate) fn apply_codex_ws_responses_lite_flag(body: &mut Value, mapped_model: &str) {
    apply_codex_ws_responses_lite_flag_with(
        crate::codex_model_catalog::current_snapshot().as_deref(),
        body,
        mapped_model,
    )
}

fn apply_codex_ws_responses_lite_flag_with(
    live: Option<&CodexModelCatalogSnapshot>,
    body: &mut Value,
    mapped_model: &str,
) {
    let lite =
        body_uses_responses_lite(body) && model_name_uses_responses_lite_with(live, mapped_model);
    let Some(metadata) = body
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if lite {
        metadata.insert(
            WS_RESPONSES_LITE_METADATA_KEY.to_string(),
            Value::String("true".to_string()),
        );
    } else {
        metadata.remove(WS_RESPONSES_LITE_METADATA_KEY);
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
        decision.mapped_model.as_deref(),
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
        mapped_model: Option<&str>,
    ) -> AiExecutionDecision {
        let mut headers = json!({"authorization":"Bearer test"});
        if let Some(value) = inbound_lite {
            headers[RESPONSES_LITE_HEADER] = json!(value);
        }
        serde_json::from_value(json!({
            "action":"execute",
            "provider_api_format":api_format,
            "mapped_model":mapped_model,
            "provider_request_body":body,
            "provider_request_headers":headers,
            "report_context":{}
        }))
        .unwrap()
    }

    fn has_lite(decision: &AiExecutionDecision) -> bool {
        decision
            .provider_request_headers
            .contains_key(RESPONSES_LITE_HEADER)
    }

    #[test]
    fn responses_lite_model_list_mirrors_the_bundled_manifest() {
        for slug in [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-daybreak-blue-latest",
            "gpt-daybreak-red-latest",
            "codex-auto-review",
            " GPT-5.6-SOL ",
        ] {
            assert!(model_name_uses_responses_lite_with(None, slug), "{slug}");
        }
        for slug in ["gpt-5.5", "gpt-5.6-sol-mini", "gpt-5.6", "", "  "] {
            assert!(!model_name_uses_responses_lite_with(None, slug), "{slug:?}");
        }
    }

    fn live_snapshot(entries: &[(&str, bool)]) -> CodexModelCatalogSnapshot {
        CodexModelCatalogSnapshot::from_models(
            "0.155.0",
            None,
            0,
            entries
                .iter()
                .map(
                    |(slug, lite)| crate::codex_model_catalog::CodexModelCapability {
                        slug: slug.to_string(),
                        use_responses_lite: *lite,
                        ..Default::default()
                    },
                )
                .collect(),
        )
    }

    #[test]
    fn responses_lite_follows_the_live_manifest_before_the_bundled_copy() {
        let live = live_snapshot(&[
            ("gpt-5.7-nova", true),
            ("gpt-5.6-luna", false),
            ("gpt-5.5", false),
        ]);
        // A lite model the bundled copy does not know yet: live wins.
        assert!(model_name_uses_responses_lite_with(
            Some(&live),
            "gpt-5.7-nova"
        ));
        assert!(!model_name_uses_responses_lite_with(None, "gpt-5.7-nova"));
        // Live says luna is no longer lite: overrides the bundled `true`.
        assert!(!model_name_uses_responses_lite_with(
            Some(&live),
            "gpt-5.6-luna"
        ));
        // Not listed live (version gate): the bundled copy answers.
        assert!(model_name_uses_responses_lite_with(
            Some(&live),
            "gpt-5.6-sol"
        ));
        assert!(!model_name_uses_responses_lite_with(Some(&live), "gpt-5.5"));
        assert!(!model_name_uses_responses_lite_with(
            Some(&live),
            "gpt-5.5-unknown"
        ));
        assert!(!model_name_uses_responses_lite_with(Some(&live), " "));

        // Through the header rule: a lite body for nova gets the header only
        // once the live snapshot knows the model.
        let body = json!({"model":"gpt-5.7-nova","reasoning":{"context":"all_turns"}});
        for (live, expected) in [(Some(&live), true), (None, false)] {
            let mut headers = std::collections::BTreeMap::new();
            apply_responses_lite_header_with(
                live,
                "codex",
                "openai:responses",
                &mut headers,
                Some(&body),
                Some("gpt-5.7-nova"),
            );
            assert_eq!(headers.contains_key(RESPONSES_LITE_HEADER), expected);
        }
    }

    fn ws_step_body(model: &str, lite_body: bool, inbound_flag: Option<&str>) -> Value {
        let mut body = json!({
            "type": "response.create",
            "model": model,
            "input": [],
            "reasoning": {"effort": "high"},
            "client_metadata": {"x-codex-ws-stream-request-start-ms": "1"}
        });
        if lite_body {
            body["reasoning"]["context"] = json!("all_turns");
        }
        if let Some(flag) = inbound_flag {
            body["client_metadata"][WS_RESPONSES_LITE_METADATA_KEY] = json!(flag);
        }
        body
    }

    #[test]
    fn ws_step_body_lite_flag_follows_the_body_and_the_target_model() {
        // Lite alias served as itself, client lost the flag: restored as the
        // string codex-rs sends.
        let mut body = ws_step_body("gpt-5.6-luna", true, None);
        apply_codex_ws_responses_lite_flag_with(None, &mut body, "gpt-5.6-luna");
        assert_eq!(
            body["client_metadata"][WS_RESPONSES_LITE_METADATA_KEY],
            json!("true")
        );

        // Lite alias remapped to a plain target: the flag goes, the body stays.
        let mut body = ws_step_body("gpt-5.6-luna", true, Some("true"));
        apply_codex_ws_responses_lite_flag_with(None, &mut body, "gpt-5.5");
        assert!(body["client_metadata"]
            .get(WS_RESPONSES_LITE_METADATA_KEY)
            .is_none());
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert_eq!(
            body["client_metadata"]["x-codex-ws-stream-request-start-ms"],
            "1"
        );

        // Plain body for a lite model (third-party client): flag dropped.
        let mut body = ws_step_body("gpt-5.6-luna", false, Some("true"));
        apply_codex_ws_responses_lite_flag_with(None, &mut body, "gpt-5.6-luna");
        assert!(body["client_metadata"]
            .get(WS_RESPONSES_LITE_METADATA_KEY)
            .is_none());

        // No `client_metadata`: nothing is created.
        let mut body = ws_step_body("gpt-5.6-luna", true, None);
        body.as_object_mut().unwrap().remove("client_metadata");
        apply_codex_ws_responses_lite_flag_with(None, &mut body, "gpt-5.6-luna");
        assert!(body.get("client_metadata").is_none());

        // A non-object `client_metadata` is left alone.
        let mut body = ws_step_body("gpt-5.6-luna", true, None);
        body["client_metadata"] = json!("opaque");
        apply_codex_ws_responses_lite_flag_with(None, &mut body, "gpt-5.6-luna");
        assert_eq!(body["client_metadata"], "opaque");

        // The live manifest decides for a model the bundled copy does not know.
        let live = live_snapshot(&[("gpt-5.7-nova", true)]);
        let mut body = ws_step_body("gpt-5.7-nova", true, None);
        apply_codex_ws_responses_lite_flag_with(Some(&live), &mut body, "gpt-5.7-nova");
        assert_eq!(
            body["client_metadata"][WS_RESPONSES_LITE_METADATA_KEY],
            json!("true")
        );
    }

    #[test]
    fn responses_lite_header_needs_a_lite_body_and_a_lite_capable_target() {
        // codex-rs sends header and `reasoning.context = all_turns` together
        // for a lite model served as itself: kept, value normalised.
        for mapped in [Some("gpt-5.6-sol"), None] {
            let mut decision = lite_decision(
                "openai:responses",
                json!({"model":"gpt-5.6-sol","reasoning":{"effort":"high","context":"all_turns"}}),
                Some("stale"),
                mapped,
            );
            apply_to_decision("codex", &mut decision);
            assert_eq!(
                decision.provider_request_headers[RESPONSES_LITE_HEADER], "true",
                "{mapped:?}"
            );
        }

        // A lite-capable model name with a plain body (third-party client or
        // an older codex-rs without the manifest flag): the header must not be
        // synthesised, the backend answers 400 to header-without-body.
        for body in [
            json!({"model":"gpt-5.6-sol","reasoning":{"effort":"high"}}),
            json!({"model":"gpt-6-astra","reasoning":{"effort":"high","context":"current_turn"}}),
            json!({"model":"gpt-5.6-luna"}),
        ] {
            let mut decision = lite_decision("openai:responses", body.clone(), Some("true"), None);
            apply_to_decision("codex", &mut decision);
            assert!(!has_lite(&decision), "{body}");
        }

        // A lite body headed for a plain target: the pool remapped
        // `gpt-5.6-luna` to `gpt-5.5`. The backend rejects the header for
        // that model but accepts the body, so the header is dropped. This
        // holds whether the body already names the target or the mapping is
        // only known through `mapped_model`, on both endpoints.
        for api_format in ["openai:responses", "openai:responses:compact"] {
            for (body, mapped) in [
                (
                    json!({"model":"gpt-5.5","reasoning":{"context":"all_turns"}}),
                    None,
                ),
                (
                    json!({"model":"gpt-5.5","reasoning":{"context":"all_turns"}}),
                    Some("gpt-5.5"),
                ),
                (
                    json!({"model":"gpt-5.6-luna","reasoning":{"context":"all_turns"}}),
                    Some("gpt-5.5"),
                ),
            ] {
                let mut decision = lite_decision(api_format, body.clone(), Some("true"), mapped);
                apply_to_decision("codex", &mut decision);
                assert!(!has_lite(&decision), "{api_format} {body} {mapped:?}");
            }
        }

        // Lite alias served as itself: `mapped_model` wins over a stale body
        // model and the header is restored even if the client lost it.
        let mut decision = lite_decision(
            "openai:responses:compact",
            json!({"model":"gpt-5.6-luna","reasoning":{"context":"all_turns"}}),
            None,
            Some("gpt-5.6-luna"),
        );
        apply_to_decision("codex", &mut decision);
        assert!(has_lite(&decision));

        // No body at all: nothing to follow, header dropped.
        let mut headers = std::collections::BTreeMap::from([(
            RESPONSES_LITE_HEADER.to_string(),
            "true".to_string(),
        )]);
        apply_responses_lite_header(
            "codex",
            "openai:responses",
            &mut headers,
            None,
            Some("gpt-5.6-sol"),
        );
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
