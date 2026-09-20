use std::collections::BTreeMap;

use aether_contracts::ExecutionPlan;
use serde_json::{Map, Value};
use tracing::warn;

use crate::{AppState, GatewayError};

const RESPONSE_HEADER_RULES_KEY: &str = "response_header_rules";
const RESPONSE_HEADER_RULES_CAMEL_KEY: &str = "responseHeaderRules";
const PROVIDER_RESPONSE_HEADERS_CONTEXT_KEY: &str = "provider_response_headers";
const RESPONSE_HEADER_RULE_PROTECTED_KEYS: &[&str] = &["content-length"];

fn endpoint_response_header_rules_from_config(config: Option<&Value>) -> Option<&Value> {
    let config = config?.as_object()?;
    config
        .get(RESPONSE_HEADER_RULES_KEY)
        .or_else(|| config.get(RESPONSE_HEADER_RULES_CAMEL_KEY))
        .filter(|value| !value.is_null())
}

async fn read_endpoint_response_header_rules(state: &AppState, endpoint_id: &str) -> Option<Value> {
    let endpoint_id = endpoint_id.trim();
    if endpoint_id.is_empty() {
        return None;
    }
    let endpoint_id = endpoint_id.to_string();

    match state
        .read_provider_catalog_endpoints_by_ids(std::slice::from_ref(&endpoint_id))
        .await
    {
        Ok(endpoints) => endpoints.into_iter().next().and_then(|endpoint| {
            endpoint_response_header_rules_from_config(endpoint.config.as_ref()).cloned()
        }),
        Err(err) => {
            warn!(
                event_name = "response_header_rules_endpoint_read_failed",
                log_type = "ops",
                endpoint_id = %endpoint_id,
                error = ?err,
                "gateway failed to read endpoint response header rules; skipping response header edits"
            );
            None
        }
    }
}

pub(crate) async fn apply_endpoint_response_header_rules(
    state: &AppState,
    plan: &ExecutionPlan,
    headers: &mut BTreeMap<String, String>,
    response_body: Option<&Value>,
) -> Result<(), GatewayError> {
    apply_configured_response_header_rules(state, plan, headers, response_body).await?;
    // Last, even when endpoint rules are absent/invalid, so rules cannot
    // reintroduce a ticket. Raw provider headers remain in the upstream audit.
    crate::turn_state::filter_response_headers(headers);
    Ok(())
}

async fn apply_configured_response_header_rules(
    state: &AppState,
    plan: &ExecutionPlan,
    headers: &mut BTreeMap<String, String>,
    response_body: Option<&Value>,
) -> Result<(), GatewayError> {
    let Some(rules) = read_endpoint_response_header_rules(state, plan.endpoint_id.as_str()).await
    else {
        return Ok(());
    };

    if !rules.is_array() {
        warn!(
            event_name = "response_header_rules_invalid_shape",
            log_type = "ops",
            endpoint_id = %plan.endpoint_id,
            "gateway skipped endpoint response header rules because response_header_rules is not an array"
        );
        return Ok(());
    }

    let empty_body = Value::Null;
    let body = response_body.unwrap_or(&empty_body);
    if !crate::provider_transport::apply_local_header_rules(
        headers,
        Some(&rules),
        RESPONSE_HEADER_RULE_PROTECTED_KEYS,
        body,
        response_body,
    ) {
        return Err(GatewayError::Internal(
            "response_header_rules 应用失败".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn attach_provider_response_headers_to_report_context(
    report_context: Option<Value>,
    provider_headers: &BTreeMap<String, String>,
) -> Option<Value> {
    let provider_headers = serde_json::to_value(provider_headers).ok()?;
    let mut object = match report_context {
        Some(Value::Object(object)) => object,
        Some(other) => Map::from_iter([("seed".to_string(), other)]),
        None => Map::new(),
    };
    object.insert(
        PROVIDER_RESPONSE_HEADERS_CONTEXT_KEY.to_string(),
        provider_headers,
    );
    Some(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_data::repository::provider_catalog::InMemoryProviderCatalogReadRepository;
    use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogEndpoint;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn turn_state_response_filter_runs_after_rules_and_preserves_upstream_audit() {
        for rules in [
            None,
            Some(json!(false)),
            Some(json!([{"action":"set", "key":"X-Codex-Turn-State", "value":"rule-ticket"}])),
        ] {
            let mut endpoint = StoredProviderCatalogEndpoint::new(
                "endpoint".into(),
                "provider".into(),
                "openai:responses".into(),
                None,
                None,
                true,
            )
            .unwrap();
            endpoint.config = rules.map(|rules| json!({"response_header_rules": rules}));
            let repo = Arc::new(InMemoryProviderCatalogReadRepository::seed(
                vec![],
                vec![endpoint],
                vec![],
            ));
            let state = AppState::new().unwrap().with_data_state_for_tests(
                crate::data::GatewayDataState::with_provider_catalog_reader_for_tests(repo),
            );
            for legacy_policy in [None, Some("false"), Some("true")] {
                let mut plan: ExecutionPlan = serde_json::from_value(json!({
                    "request_id":"test", "provider_id":"provider", "endpoint_id":"endpoint",
                    "key_id":"key", "method":"POST", "url":"https://example.invalid/responses",
                    "body":{"json_body":{"model":"test"}},
                    "client_api_format":"openai:responses", "provider_api_format":"openai:responses"
                }))
                .unwrap();
                if let Some(value) = legacy_policy {
                    plan.headers
                        .insert(crate::turn_state::HIDE_RESPONSE_HEADER.into(), value.into());
                }
                let upstream = BTreeMap::from([
                    ("x-codex-turn-state".into(), "upstream-ticket".into()),
                    ("X-Codex-Turn-State".into(), "mixed-case-ticket".into()),
                    ("content-type".into(), "text/event-stream".into()),
                    ("x-request-id".into(), "upstream-request".into()),
                ]);
                let context =
                    attach_provider_response_headers_to_report_context(None, &upstream).unwrap();
                let mut client = upstream.clone();
                apply_endpoint_response_header_rules(&state, &plan, &mut client, None)
                    .await
                    .unwrap();
                assert!(!client
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case(crate::turn_state::HEADER)));
                assert_eq!(client["x-request-id"], "upstream-request");
                assert_eq!(client["content-type"], "text/event-stream");
                assert_eq!(
                    context["provider_response_headers"][crate::turn_state::HEADER],
                    "upstream-ticket"
                );
            }
        }
    }
    #[tokio::test]
    async fn turn_state_response_filter_covers_sync_stream_and_errors_on_the_wire() {
        use crate::ai_serving::GatewayControlDecision;
        use crate::execution_runtime::{
            execute_execution_runtime_stream, execute_execution_runtime_sync,
        };
        use axum::{
            body::{to_bytes, Body},
            http::{HeaderMap, StatusCode},
            routing::post,
            Json, Router,
        };
        let upstream = Router::new().route("/responses", post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(
                headers.get(crate::turn_state::HEADER).map(|value| value.to_str().unwrap()),
                (body["test_override"] == true).then_some("collected-ticket")
            );
            assert!(!headers.contains_key(crate::turn_state::HIDE_RESPONSE_HEADER));
            let stream = body["stream"] == true;
            let error = body["test_error"] == true;
            let payload = if error {
                json!({"error":{"message":"test error","type":"invalid_request_error"}}).to_string()
            } else if stream {
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-test\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-test\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n".into()
            } else {
                json!({"id":"resp-test","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1}}).to_string()
            };
            http::Response::builder().status(if error {400} else {200})
                .header("content-type", if stream && !error {"text/event-stream"} else {"application/json"})
                .header("x-codex-turn-state", "upstream-ticket")
                .header("x-request-id", "keep-upstream-id")
                .body(Body::from(payload)).unwrap()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let state = AppState::new()
            .unwrap()
            .with_execution_runtime_override_base_url(String::new());
        for enabled in [false, true] {
            for stream in [false, true] {
                for error in [false, true] {
                    let mut plan: ExecutionPlan = serde_json::from_value(json!({
                        "request_id":format!("test-{enabled}-{stream}-{error}"), "provider_id":"provider", "endpoint_id":"endpoint",
                        "key_id":"key", "method":"POST", "url":format!("http://{addr}/responses"),
                        "headers":{"content-type":"application/json", "x-codex-turn-state":"collected-ticket"},
                        "body":{"json_body":{"model":"test", "stream":stream, "test_error":error, "test_override":enabled}},
                        "stream":stream, "client_api_format":"openai:responses", "provider_api_format":"openai:responses"
                    })).unwrap();
                    if !enabled {
                        plan.headers.remove(crate::turn_state::HEADER);
                    }
                    let decision = GatewayControlDecision::synthetic(
                        "/v1/responses",
                        Some("ai_public".into()),
                        Some("openai".into()),
                        Some("responses".into()),
                        Some("openai:responses".into()),
                    )
                    .with_execution_runtime_candidate(true);
                    let response = if stream {
                        execute_execution_runtime_stream(
                            &state,
                            plan,
                            "test",
                            &decision,
                            "openai_responses_stream",
                            None,
                            None,
                        )
                        .await
                    } else {
                        execute_execution_runtime_sync(
                            &state,
                            "/v1/responses",
                            plan,
                            "test",
                            &decision,
                            "openai_responses_sync",
                            None,
                            None,
                        )
                        .await
                    }
                    .unwrap()
                    .expect("client response");
                    assert_eq!(
                        response.status(),
                        if error {
                            StatusCode::BAD_REQUEST
                        } else {
                            StatusCode::OK
                        }
                    );
                    assert!(
                        !response.headers().contains_key(crate::turn_state::HEADER),
                        "enabled={enabled}, stream={stream}, error={error}"
                    );
                    assert!(!response
                        .headers()
                        .contains_key(crate::turn_state::HIDE_RESPONSE_HEADER));
                    assert_eq!(
                        response.headers().get("x-request-id").unwrap(),
                        "keep-upstream-id"
                    );
                    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                    assert!(!bytes.is_empty());
                }
            }
        }
        server.abort();
    }
}
