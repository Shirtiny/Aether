use super::*;
use serde_json::Value;
use tokio::task::JoinHandle;

const CLIENT_UA: &str = "codex-tui/0.153.4 (Linux 6.8; x86_64) Terminal";

struct ModelTestGateway {
    url: String,
    api_format: String,
    model: &'static str,
    plans: Arc<Mutex<Vec<ExecutionPlan>>>,
    handles: [JoinHandle<()>; 2],
}

impl Drop for ModelTestGateway {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

impl ModelTestGateway {
    async fn new(api_format: &str, provider_type: &str, headers: bool, identity: bool) -> Self {
        let plans = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&plans);
        // Execute only against a loopback mock. Catalog and identity state are
        // in memory; no Codex account, production database or Redis is used.
        let execution_runtime = Router::new().route(
            "/v1/execute/sync",
            any(move |Json(plan): Json<ExecutionPlan>| {
                captured.lock().expect("plans lock").push(plan.clone());
                async move {
                    Json(json!({
                        "request_id": plan.request_id,
                        "candidate_id": plan.candidate_id,
                        "status_code": 200,
                        "headers": { "content-type": "application/json" },
                        "body": { "json_body": {
                            "id": "resp-model-test",
                            "model": plan.model_name,
                            "output_text": "ok",
                            "data": [{ "b64_json": "aGVsbG8=" }]
                        } },
                        "telemetry": { "elapsed_ms": 1 }
                    }))
                }
            }),
        );
        let (execution_url, execution_handle) = start_server(execution_runtime).await;
        let mut provider = sample_provider("provider-codex-test", "Codex test", 10);
        provider.provider_type = provider_type.to_string();
        provider.config = Some(json!({ "pool_advanced": {
            "codex_client_headers": {
                "enabled": headers,
                "profiles": [{ "user_agent": CLIENT_UA, "originator": "codex-tui" }]
            },
            "codex_runtime_identity": {
                "enabled": identity,
                "expected_threads_per_day": 32,
                "expected_turns_per_day": 256
            }
        } }));
        crate::codex_runtime_identity::validate_codex_runtime_identity_config(
            &provider.config.as_ref().unwrap()["pool_advanced"]["codex_runtime_identity"],
        )
        .expect("valid identity fixture config");
        let endpoint = sample_endpoint(
            "endpoint-codex-test",
            &provider.id,
            api_format,
            "https://codex-model-test.invalid/v1",
        );
        let keys = ["one", "two"].map(|account| {
            let mut key = sample_key(
                &format!("key-{account}"),
                &provider.id,
                api_format,
                "sk-local-model-test",
            );
            key.name = format!("account-{account}");
            key
        });
        let catalog = Arc::new(InMemoryProviderCatalogReadRepository::seed(
            vec![provider],
            vec![endpoint],
            keys.to_vec(),
        ));
        let state = build_state_with_execution_runtime_override(execution_url)
            .with_data_state_for_tests(GatewayDataState::with_provider_transport_reader_for_tests(
                catalog,
                DEVELOPMENT_ENCRYPTION_KEY.to_string(),
            ));
        let (url, gateway_handle) = start_server(build_router_with_state(state)).await;
        Self {
            url,
            api_format: api_format.to_string(),
            model: if api_format == "openai:image" {
                "gpt-image-2"
            } else {
                "gpt-6-astra"
            },
            plans,
            handles: [gateway_handle, execution_handle],
        }
    }

    async fn request(
        &self,
        mode: &str,
        key: &str,
        headers: Value,
        body: Value,
    ) -> (ExecutionPlan, Value) {
        let path = if mode == "pool" {
            "test-model-failover"
        } else {
            "test-model"
        };
        let response = reqwest::Client::new()
            .post(format!("{}/api/admin/provider-query/{path}", self.url))
            .header(GATEWAY_HEADER, "rust-phase3b")
            .header(TRUSTED_ADMIN_USER_ID_HEADER, "admin-model-test")
            .header(TRUSTED_ADMIN_USER_ROLE_HEADER, "admin")
            .header(
                TRUSTED_ADMIN_SESSION_ID_HEADER,
                "admin-session-not-client-identity",
            )
            .json(&json!({
                "provider_id": "provider-codex-test",
                "endpoint_id": "endpoint-codex-test",
                "api_format": self.api_format,
                "api_key_ids": [key],
                "mode": mode,
                "model": self.model,
                "failover_models": [self.model],
                "request_headers": headers,
                "request_body": body
            }))
            .send()
            .await
            .expect("model test request");
        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value = response.json().await.expect("model test response");
        assert_eq!(payload["success"], true, "{payload}");
        let plan = self
            .plans
            .lock()
            .expect("plans lock")
            .last()
            .expect("executed plan")
            .clone();
        assert_eq!(plan.key_id, key);
        (plan, payload)
    }
}

fn assert_stable_profile(plan: &ExecutionPlan) {
    assert_eq!(plan.headers["user-agent"], CLIENT_UA);
    assert_eq!(plan.headers["originator"], "codex-tui");
    assert_eq!(plan.headers["version"], "0.153.4");
    assert!(plan.headers.contains_key("x-codex-installation-id"));
    assert!(!plan.headers.contains_key("x-trace-id"));
}

fn assert_synthetic_responses_identity(plan: &ExecutionPlan) {
    let body = plan.body.json_body.as_ref().expect("wire body");
    let blob: Value =
        serde_json::from_str(&plan.headers["x-codex-turn-metadata"]).expect("metadata");
    assert_eq!(blob["session_id"], plan.headers["session-id"]);
    assert_eq!(blob["thread_id"], plan.headers["thread-id"]);
    assert_eq!(blob["window_id"], plan.headers["x-codex-window-id"]);
    assert_eq!(blob["request_kind"], "turn");
    assert_eq!(blob["agent_name"], "/root");
    assert_eq!(blob["sandbox"], "seccomp");
    assert_eq!(body["prompt_cache_key"], plan.headers["session-id"]);
    assert_eq!(body["client_metadata"]["session_id"], blob["session_id"]);
    assert_eq!(body["client_metadata"]["turn_id"], blob["turn_id"]);
    assert_eq!(
        body["client_metadata"]["x-codex-turn-metadata"],
        plan.headers["x-codex-turn-metadata"]
    );
    assert!(!plan.headers.contains_key("x-codex-turn-state"));
}

#[tokio::test]
async fn admin_codex_model_test_applies_pool_identity_to_direct_and_pool_requests() {
    let gateway = ModelTestGateway::new("openai:responses", "codex", true, true).await;
    let mut previous_session = None;
    for mode in ["direct", "pool"] {
        // Cover both a native saved Responses template and the dialog's
        // default Chat-shaped body converted to Responses.
        let body = if mode == "direct" {
            json!({ "input": "hello", "stream": true })
        } else {
            json!({ "messages": [{ "role": "user", "content": "hello" }] })
        };
        let (plan, response) = gateway
            .request(
                mode,
                "key-one",
                json!({
                    "user-agent": "relay/1.0", "version": "1.0", "x-trace-id": "relay-trace"
                }),
                body,
            )
            .await;
        assert_stable_profile(&plan);
        assert_synthetic_responses_identity(&plan);
        assert!(plan.stream);
        let session = plan.headers["session-id"].clone();
        if let Some(previous) = previous_session {
            assert_eq!(
                session, previous,
                "the same account and prompt reuse their identity"
            );
        }
        previous_session = Some(session);
        if mode == "pool" {
            assert_eq!(
                response["attempts"][0]["request_headers"]["user-agent"],
                CLIENT_UA
            );
            assert_eq!(
                response["attempts"][0]["request_body"],
                *plan.body.json_body.as_ref().unwrap()
            );
        }
    }
    let (other, _) = gateway
        .request("pool", "key-two", json!({}), json!({ "input": "hello" }))
        .await;
    assert_synthetic_responses_identity(&other);
    assert_ne!(
        Some(other.headers["session-id"].clone()),
        previous_session,
        "accounts must not share a synthetic session"
    );
}

#[tokio::test]
async fn admin_codex_model_test_honors_independent_profile_and_identity_switches() {
    for (headers, identity) in [(true, false), (false, true), (false, false)] {
        let gateway = ModelTestGateway::new("openai:responses", "codex", headers, identity).await;
        let (plan, _) = gateway
            .request(
                "direct",
                "key-one",
                json!({
                    "user-agent": "relay/1.0", "originator": "relay", "version": "1.0"
                }),
                json!({ "input": "hello" }),
            )
            .await;
        if headers {
            assert_stable_profile(&plan);
        } else {
            assert_eq!(plan.headers["user-agent"], "relay/1.0");
            assert_eq!(plan.headers["originator"], "relay");
            assert_eq!(plan.headers["version"], "1.0");
        }
        if identity {
            assert_synthetic_responses_identity(&plan);
        } else {
            assert!(!plan.headers.contains_key("x-codex-turn-metadata"));
            assert!(plan.body.json_body.as_ref().unwrap()["client_metadata"]
                .get("session_id")
                .is_none());
        }
    }
}

fn official_headers() -> Value {
    json!({
        "session-id": "inbound-session",
        "thread-id": "inbound-thread",
        "x-codex-window-id": "inbound-window",
        "x-codex-turn-metadata": json!({
            "session_id": "inbound-session", "thread_id": "inbound-thread",
            "turn_id": "inbound-turn", "window_id": "inbound-window", "request_kind": "turn"
        }).to_string()
    })
}

#[tokio::test]
async fn admin_codex_model_test_rewrites_original_identity_and_preserves_explicit_cache_keys() {
    let gateway = ModelTestGateway::new("openai:responses", "codex", true, true).await;
    let mut body = json!({ "input": "hello", "prompt_cache_key": "explicit-cache-key" });
    body["client_metadata"] = json!({
        "session_id": "inbound-session", "thread_id": "inbound-thread", "turn_id": "inbound-turn",
        "x-codex-turn-metadata": official_headers()["x-codex-turn-metadata"]
    });
    let (plan, _) = gateway
        .request("direct", "key-one", official_headers(), body)
        .await;
    assert_stable_profile(&plan);
    assert_ne!(plan.headers["session-id"], "inbound-session");
    assert_ne!(plan.headers["thread-id"], "inbound-thread");
    let body = plan.body.json_body.as_ref().unwrap();
    assert_eq!(body["prompt_cache_key"], "explicit-cache-key");
    assert_eq!(
        body["client_metadata"]["session_id"],
        plan.headers["session-id"]
    );
    assert_eq!(
        body["client_metadata"]["thread_id"],
        plan.headers["thread-id"]
    );
    assert_eq!(body["input"], "hello");
}

#[tokio::test]
async fn admin_codex_model_test_keeps_compact_rewrite_only_and_strips_body_metadata() {
    let gateway = ModelTestGateway::new("openai:responses:compact", "codex", true, true).await;
    for has_identity in [false, true] {
        let headers = if has_identity {
            official_headers()
        } else {
            json!({})
        };
        let (plan, _) = gateway
            .request(
                "direct",
                "key-one",
                headers,
                json!({
                    "input": "hello", "prompt_cache_key": "explicit-cache-key",
                    "client_metadata": { "x-codex-installation-id": "old-installation" }
                }),
            )
            .await;
        assert_stable_profile(&plan);
        let body = plan.body.json_body.as_ref().unwrap();
        assert!(body.get("client_metadata").is_none());
        assert_eq!(body["prompt_cache_key"], "explicit-cache-key");
        if has_identity {
            assert_ne!(plan.headers["session-id"], "inbound-session");
            assert!(!plan.headers.contains_key("x-client-request-id"));
        } else {
            assert!(!plan.headers.contains_key("x-codex-turn-metadata"));
        }
    }
}

#[tokio::test]
async fn admin_codex_model_test_applies_header_only_identity_to_image() {
    let gateway = ModelTestGateway::new("openai:image", "codex", true, true).await;
    for has_identity in [false, true] {
        let headers = if has_identity {
            official_headers()
        } else {
            json!({})
        };
        let body = json!({ "prompt": "Draw a small blue square", "n": 1 });
        let (plan, _) = gateway.request("direct", "key-one", headers, body).await;
        assert_stable_profile(&plan);
        if has_identity {
            assert_ne!(plan.headers["session-id"], "inbound-session");
        } else {
            assert!(!plan.headers.contains_key("x-codex-turn-metadata"));
        }
        assert!(plan.body.json_body.as_ref().unwrap()["client_metadata"]
            .get("session_id")
            .is_none());
    }
}

#[tokio::test]
async fn admin_codex_model_test_identity_passes_ignore_other_provider_types() {
    let gateway = ModelTestGateway::new("openai:responses", "custom", true, true).await;
    let (plan, _) = gateway
        .request(
            "direct",
            "key-one",
            json!({ "user-agent": "relay/1.0" }),
            json!({ "input": "hello" }),
        )
        .await;
    assert_eq!(plan.headers["user-agent"], "relay/1.0");
    assert!(!plan.headers.contains_key("originator"));
    assert!(!plan.headers.contains_key("x-codex-turn-metadata"));
    assert!(plan
        .body
        .json_body
        .as_ref()
        .unwrap()
        .get("client_metadata")
        .is_none());
}
