use super::*;
use crate::data::GatewayDataState;
use aether_data::repository::provider_catalog::InMemoryProviderCatalogReadRepository;
use aether_data_contracts::repository::provider_catalog::ProviderCatalogWriteRepository;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn runtime() -> RuntimeState {
    RuntimeState::memory(aether_runtime_state::MemoryRuntimeStateConfig::default())
}

pub(crate) fn ticket_cache(model: &str, value: &str, age: u64) -> TicketCache {
    TicketCache {
        source_provider_id: "source".into(),
        tickets: BTreeMap::from([(
            model.into(),
            Ticket {
                value: value.into(),
                fetched_at: crate::codex_client_release::unix_now_secs() - age,
            },
        )]),
        ..Default::default()
    }
}

pub(crate) async fn seed_ticket(runtime: &RuntimeState, value: &str, age: u64) {
    seed_model_ticket(runtime, "test-model", value, age).await;
}

pub(crate) async fn seed_model_ticket(runtime: &RuntimeState, model: &str, value: &str, age: u64) {
    save_cache(runtime, &ticket_cache(model, value, age))
        .await
        .unwrap();
}

async fn load_ticket(runtime: &RuntimeState, enabled: bool) -> Option<String> {
    load_tickets(runtime, enabled.then_some("source"))
        .await?
        .for_model("test-model")
        .map(str::to_owned)
}

fn source(enabled: bool) -> StoredProviderCatalogProvider {
    let mut provider =
        StoredProviderCatalogProvider::new("source".into(), "Source".into(), None, "custom".into())
            .unwrap();
    provider.config =
        Some(json!({"turn_state_collection": {"enabled": enabled, "models": ["test-model"]}}));
    provider
}

fn state(
    providers: Vec<StoredProviderCatalogProvider>,
) -> (AppState, Arc<InMemoryProviderCatalogReadRepository>) {
    let repo = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        providers,
        vec![],
        vec![],
    ));
    let state = AppState::new().unwrap().with_data_state_for_tests(
        GatewayDataState::with_provider_catalog_reader_for_tests(repo.clone()),
    );
    (state, repo)
}

#[test]
fn config_is_opt_in_and_strictly_validated() {
    assert!(validate_config(&Map::new()).is_ok());
    for value in [
        json!({"turn_state_collection": true}),
        json!({"turn_state_collection": {"enabled": true}}),
        json!({"turn_state_collection": {"enabled": true, "models": [" "]}}),
        json!({"turn_state_collection": {"enabled": "true", "models": ["test-model"]}}),
        json!({"pool_advanced": {"turn_state_source_provider_id": true}}),
    ] {
        assert!(validate_config(value.as_object().unwrap()).is_err());
    }
    assert!(validate_config(
        json!({"turn_state_collection": {"enabled": false, "models": []}})
            .as_object()
            .unwrap()
    )
    .is_ok());
    assert!(source_config(&source(false)).is_none());
    let mut inactive = source(true);
    inactive.is_active = false;
    assert!(source_config(&inactive).is_none());
}

#[test]
fn http_sources_and_bounded_printable_ascii_tickets_are_accepted() {
    for url in [
        "https://provider-a.example/v1/responses",
        "https://provider-b.example/custom",
        "http://127.0.0.1:1234/v1",
    ] {
        assert!(is_collection_url(url));
    }
    for url in [
        "ftp://provider.example",
        "file:///tmp/test",
        "https://user@provider.example",
        "not-a-url",
    ] {
        assert!(!is_collection_url(url));
    }
    for length in [1, 128, 292, 384, 4096] {
        assert!(valid_ticket(&"a".repeat(length)));
    }
    for ticket in [
        String::new(),
        "a".repeat(4097),
        "é".repeat(146),
        format!("{}\n", "a".repeat(291)),
        " ".repeat(292),
    ] {
        assert!(!valid_ticket(&ticket));
    }
}

#[test]
fn response_requires_success_and_a_single_valid_header() {
    let mut result = ExecutionResult {
        request_id: "test".into(),
        candidate_id: None,
        status_code: 200,
        headers: BTreeMap::from([("X-Codex-Turn-State".into(), "a".repeat(292))]),
        body: None,
        telemetry: None,
        error: None,
    };
    assert_eq!(ticket_from_response(&result).unwrap(), "a".repeat(292));
    result.status_code = 429;
    assert!(ticket_from_response(&result).is_err());
    result.status_code = 200;
    result.headers.insert(HEADER.into(), "b".repeat(292));
    assert!(ticket_from_response(&result).is_err());
    result.headers.clear();
    assert!(ticket_from_response(&result).is_err());
}

#[test]
fn override_inserts_missing_headers_and_replaces_case_insensitive_projections() {
    let ticket = "a".repeat(292);
    let mut headers = BTreeMap::from([
        ("X-Codex-Turn-State".into(), "old".into()),
        (HEADER.into(), "other".into()),
    ]);
    let mut body = json!({"client_metadata": {"X-Codex-Turn-State": "old", "keep": "yes"}});
    apply_ticket(&mut headers, Some(&mut body), &ticket, false);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[HEADER], ticket);
    assert_eq!(body["client_metadata"][HEADER], ticket);
    assert!(body["client_metadata"].get("X-Codex-Turn-State").is_none());
    assert_eq!(body["client_metadata"]["keep"], "yes");
    headers.clear();
    let mut body = json!({"input": []});
    apply_ticket(&mut headers, Some(&mut body), &ticket, false);
    assert_eq!(headers[HEADER], ticket);
    assert!(body.get("client_metadata").is_none());
    // WS must supply the field on every step, including the first/new turn.
    for old_metadata in [Value::Null, json!({}), json!({HEADER: "old"})] {
        let mut body = json!({"client_metadata": old_metadata});
        apply_ticket(&mut BTreeMap::new(), Some(&mut body), &ticket, true);
        assert_eq!(body["client_metadata"][HEADER], ticket);
    }
}

#[tokio::test]
async fn expired_invalid_and_disabled_tickets_do_not_override() {
    let runtime = runtime();
    assert!(load_ticket(&runtime, true).await.is_none());
    seed_ticket(&runtime, &"a".repeat(292), 0).await;
    assert!(load_ticket(&runtime, false).await.is_none());
    assert_eq!(load_ticket(&runtime, true).await.unwrap(), "a".repeat(292));
    seed_ticket(&runtime, &"a".repeat(292), TICKET_TTL.as_secs()).await;
    assert!(load_ticket(&runtime, true).await.is_none());
    seed_ticket(&runtime, "invalid\n", 0).await;
    assert!(load_ticket(&runtime, true).await.is_none());
}

#[tokio::test]
async fn scan_refreshes_once_per_forty_minutes_and_shares_the_new_ticket() {
    let (state, _) = state(vec![source(true)]);
    let calls = AtomicUsize::new(0);
    let fetch = async |_: &AppState, provider: &str, model: &str| {
        assert_eq!(provider, "source");
        assert_eq!(model, "test-model");
        calls.fetch_add(1, Ordering::SeqCst);
        Ok("a".repeat(292))
    };
    scan_with_fetch(&state, &fetch).await.unwrap();
    scan_with_fetch(&state, &fetch).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        load_ticket(&state.runtime_state, true).await.unwrap(),
        "a".repeat(292)
    );
    let marker = attempt_key("source", "test-model");
    let ttl = state
        .runtime_state
        .kv_ttl_seconds(&marker)
        .await
        .unwrap()
        .unwrap();
    assert!((2398..=2400).contains(&ttl));
    state.runtime_state.kv_delete(&marker).await.unwrap();
    scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
        Ok("b".repeat(292))
    })
    .await
    .unwrap();
    assert_eq!(
        load_ticket(&state.runtime_state, true).await.unwrap(),
        "b".repeat(292)
    );
}

#[tokio::test]
async fn disabled_or_deleted_sources_clear_only_their_own_tickets_without_fetching() {
    for providers in [vec![source(false)], vec![]] {
        let (state, _) = state(providers);
        seed_ticket(&state.runtime_state, &"a".repeat(292), 0).await;
        scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
            panic!("disabled/deleted source must not fetch");
            #[allow(unreachable_code)]
            Ok(String::new())
        })
        .await
        .unwrap();
        assert!(load_tickets(&state.runtime_state, Some("source"))
            .await
            .is_none());
    }
}

#[tokio::test]
async fn multiple_sources_keep_same_model_tickets_and_schedules_isolated() {
    let mut other = source(true);
    other.id = "other".into();
    let (state, repo) = state(vec![source(true), other.clone()]);
    let calls = AtomicUsize::new(0);
    let fetch = async |_: &AppState, provider: &str, model: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("{provider}-{model}-ticket"))
    };
    for _ in 0..3 {
        scan_with_fetch(&state, &fetch).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    for provider in ["source", "other"] {
        let cache = load_tickets(&state.runtime_state, Some(provider))
            .await
            .unwrap();
        assert_eq!(
            cache.for_model("test-model"),
            Some(format!("{provider}-test-model-ticket").as_str())
        );
    }
    assert!(load_tickets(&state.runtime_state, None).await.is_none());
    assert!(load_tickets(&state.runtime_state, Some("missing"))
        .await
        .is_none());
    repo.update_provider(&source(false)).await.unwrap();
    scan_with_fetch(&state, &fetch).await.unwrap();
    assert!(load_tickets(&state.runtime_state, Some("source"))
        .await
        .is_none());
    assert!(load_tickets(&state.runtime_state, Some("other"))
        .await
        .is_some());
    repo.delete_provider("other").await.unwrap();
    scan_with_fetch(&state, &fetch).await.unwrap();
    assert!(load_tickets(&state.runtime_state, Some("other"))
        .await
        .is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_source_does_not_stop_other_sources() {
    let mut other = source(true);
    other.id = "other".into();
    let (state, _) = state(vec![source(true), other]);
    let result = scan_with_fetch(&state, async |_: &AppState, provider: &str, _: &str| {
        if provider == "source" {
            Err("failed".into())
        } else {
            Ok("other-ticket".into())
        }
    })
    .await;
    assert!(result.is_err());
    assert!(load_tickets(&state.runtime_state, Some("source"))
        .await
        .unwrap()
        .tickets
        .is_empty());
    assert_eq!(
        load_tickets(&state.runtime_state, Some("other"))
            .await
            .unwrap()
            .for_model("test-model"),
        Some("other-ticket")
    );
}

#[test]
fn legacy_collection_is_read_but_explicit_generic_setting_wins() {
    let mut provider = source(true);
    provider.config = Some(json!({"congming_turn_state": {"enabled": true, "models": ["old"]}}));
    assert_eq!(source_config(&provider).unwrap().models, ["old"]);
    provider.config.as_mut().unwrap()["turn_state_collection"] =
        json!({"enabled": false, "models": []});
    assert!(source_config(&provider).is_none());
    provider.config.as_mut().unwrap()["turn_state_collection"] = Value::Null;
    assert!(source_config(&provider).is_none());
}

#[tokio::test]
async fn corrupted_source_identity_cannot_cross_channel_boundary() {
    let runtime = runtime();
    runtime
        .kv_set(
            &cache_key("other"),
            serde_json::to_string(&ticket_cache("test-model", "source-ticket", 0)).unwrap(),
            Some(TICKET_TTL),
        )
        .await
        .unwrap();
    assert!(load_tickets(&runtime, Some("other")).await.is_none());
}

#[tokio::test]
async fn failed_refresh_preserves_last_good_ticket_and_does_not_retry_early() {
    let (state, _) = state(vec![source(true)]);
    seed_ticket(&state.runtime_state, &"a".repeat(292), 480).await;
    assert!(
        scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| Err(
            "failed".into()
        ))
        .await
        .is_err()
    );
    assert_eq!(
        load_ticket(&state.runtime_state, true).await.unwrap(),
        "a".repeat(292)
    );
    scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
        panic!("failed requests must not retry within 40 minutes");
        #[allow(unreachable_code)]
        Ok(String::new())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn disabling_source_during_fetch_does_not_republish_credential() {
    let (state, repo) = state(vec![source(true)]);
    scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
        repo.update_provider(&source(false)).await.unwrap();
        Ok("a".repeat(292))
    })
    .await
    .unwrap();
    assert!(load_ticket(&state.runtime_state, true).await.is_none());
}

#[tokio::test]
async fn models_are_refreshed_and_selected_independently() {
    let mut provider = source(true);
    provider.config =
        Some(json!({"turn_state_collection": {"enabled": true, "models": ["model-a", "model-b"]}}));
    let (state, repo) = state(vec![provider.clone()]);
    let calls = AtomicUsize::new(0);
    let fetch = async |_: &AppState, _: &str, model: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("ticket-for-{model}"))
    };
    for _ in 0..4 {
        scan_with_fetch(&state, &fetch).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let cache = load_tickets(&state.runtime_state, Some("source"))
        .await
        .unwrap();
    assert_eq!(
        cache.for_body(&json!({"model": "model-a"})),
        Some("ticket-for-model-a")
    );
    assert_eq!(
        cache.for_body(&json!({"model": "model-b"})),
        Some("ticket-for-model-b")
    );
    assert!(cache.for_body(&json!({"model": "alias-a"})).is_none());
    assert!(cache.for_body(&json!({})).is_none());
    // Removing model-a revokes its override without waiting for expiry.
    provider.config =
        Some(json!({"turn_state_collection": {"enabled": true, "models": ["model-b"]}}));
    repo.update_provider(&provider).await.unwrap();
    scan_with_fetch(&state, &fetch).await.unwrap();
    let cache = load_tickets(&state.runtime_state, Some("source"))
        .await
        .unwrap();
    assert!(cache.for_model("model-a").is_none());
    assert_eq!(cache.for_model("model-b"), Some("ticket-for-model-b"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn model_configuration_is_trimmed_deduplicated_and_case_sensitive() {
    let cfg =
        parse_source_config(&json!({"enabled": true, "models": [" a ", "a", "B", "b"]})).unwrap();
    assert_eq!(cfg.models, ["B", "a", "b"]);
}

#[tokio::test]
async fn fetch_plan_reuses_channel_credentials_and_responses_path_without_network() {
    use aether_crypto::{encrypt_python_fernet_plaintext, DEVELOPMENT_ENCRYPTION_KEY};
    use aether_data_contracts::repository::provider_catalog::{
        StoredProviderCatalogEndpoint, StoredProviderCatalogKey,
    };
    let mut endpoint = StoredProviderCatalogEndpoint::new(
        "endpoint".into(),
        "source".into(),
        "openai:responses".into(),
        Some("openai".into()),
        Some("responses".into()),
        true,
    )
    .unwrap();
    endpoint.base_url = "https://provider-a.example".into();
    endpoint.custom_path = Some("/v1/responses".into());
    endpoint.header_rules = Some(json!([
        {"action": "set", "key": "x-codex-turn-state", "value": "must-not-send-old-ticket"},
        {"action": "set", "key": "x-channel-custom", "value": "keep"}
    ]));
    let mut key = StoredProviderCatalogKey::new(
        "key".into(),
        "source".into(),
        "test".into(),
        "api_key".into(),
        None,
        true,
    )
    .unwrap();
    key.encrypted_api_key =
        Some(encrypt_python_fernet_plaintext(DEVELOPMENT_ENCRYPTION_KEY, "test-only-key").unwrap());
    let repo = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        vec![source(true)],
        vec![endpoint],
        vec![key],
    ));
    let state = AppState::new().unwrap().with_data_state_for_tests(
        GatewayDataState::with_provider_catalog_reader_for_tests(repo)
            .with_encryption_key_for_tests(DEVELOPMENT_ENCRYPTION_KEY),
    );
    let mut transport = state
        .read_provider_transport_snapshot("source", "endpoint", "key")
        .await
        .unwrap()
        .unwrap();
    let plan = build_fetch_plan(&state, &transport, " model-a ")
        .await
        .unwrap();
    assert_eq!(plan.url, "https://provider-a.example/v1/responses");
    assert_eq!(plan.method, "POST");
    assert!(plan.stream);
    assert_eq!(plan.headers["authorization"], "Bearer test-only-key");
    assert_eq!(plan.headers["x-channel-custom"], "keep");
    assert!(!plan.headers.contains_key(HEADER));
    assert_ne!(plan.headers["user-agent"], "openai-codex/1.0");
    assert_eq!(plan.headers["thread-id"], plan.headers["session-id"]);
    let body = plan.body.json_body.as_ref().unwrap();
    assert_eq!(body["model"], "model-a");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    transport.endpoint.base_url = "https://unrelated.example".into();
    assert!(build_fetch_plan(&state, &transport, "model-a")
        .await
        .is_ok());
    transport.endpoint.base_url = "ftp://unrelated.example".into();
    assert!(build_fetch_plan(&state, &transport, "model-a")
        .await
        .is_err());
}

#[test]
fn source_selection_accepts_only_explicit_ids_or_off() {
    for value in [
        Value::Null,
        json!("source"),
        json!("e7db605a-c99c-431e-bb23-389b36626890"),
    ] {
        assert!(validate_config(
            json!({"pool_advanced": {OVERRIDE_CONFIG: value}})
                .as_object()
                .unwrap()
        )
        .is_ok());
    }
    for value in [
        json!(true),
        json!(false),
        json!(42),
        json!(""),
        json!(" source "),
        json!("*"),
        json!("a:b"),
        json!([]),
        json!({}),
        json!("a".repeat(129)),
    ] {
        assert!(validate_config(
            json!({"pool_advanced": {OVERRIDE_CONFIG: value}})
                .as_object()
                .unwrap()
        )
        .is_err());
    }
}

#[tokio::test]
async fn overlapping_scans_do_not_duplicate_a_sources_paid_request() {
    let (state, _) = state(vec![source(true)]);
    let entered = tokio::sync::Notify::new();
    let finish = tokio::sync::Notify::new();
    let calls = AtomicUsize::new(0);
    let fetch = async |_: &AppState, _: &str, _: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        entered.notify_one();
        finish.notified().await;
        Ok("ticket".into())
    };
    let (first, ()) = tokio::join!(scan_with_fetch(&state, &fetch), async {
        entered.notified().await;
        scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
            panic!("another replica must not duplicate the collection request");
            #[allow(unreachable_code)]
            Ok(String::new())
        })
        .await
        .unwrap();
        finish.notify_one();
    });
    first.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        load_ticket(&state.runtime_state, true).await.as_deref(),
        Some("ticket")
    );
}

#[tokio::test]
async fn turn_state_status_exposes_progress_success_failure_and_schedule_without_secrets() {
    let (state, _) = state(vec![source(true)]);
    let provider = source(true);
    let before = collection_status(&state.runtime_state, &provider).await;
    assert_eq!(before["models"][0]["result"], "pending");
    assert_eq!(before["models"][0]["ticket_valid"], false);
    let secret = "never-expose-the-credential-in-the-admin-api";
    scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
        let in_flight = collection_status(&state.runtime_state, &provider).await;
        assert_eq!(in_flight["models"][0]["result"], "collecting");
        Ok(secret.into())
    })
    .await
    .unwrap();
    let success = collection_status(&state.runtime_state, &provider).await;
    let row = &success["models"][0];
    assert_eq!(row["result"], "success");
    assert_eq!(row["ticket_valid"], true);
    assert_eq!(row["ticket_length"], secret.len());
    assert!(row["last_attempt_at"].as_u64().is_some());
    assert!(row["last_success_at"].as_u64().is_some());
    assert!(row["next_attempt_at"].as_u64().unwrap() > row["last_attempt_at"].as_u64().unwrap());
    assert!(!success.to_string().contains(secret));
    state
        .runtime_state
        .kv_delete(&attempt_key("source", "test-model"))
        .await
        .unwrap();
    assert!(
        scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| Err(
            "missing header".into()
        ))
        .await
        .is_err()
    );
    let failure = collection_status(&state.runtime_state, &provider).await;
    assert_eq!(failure["models"][0]["result"], "failed");
    assert_eq!(failure["models"][0]["error"], "missing header");
    assert_eq!(
        failure["models"][0]["ticket_valid"], true,
        "a failed refresh does not revoke a valid cached ticket"
    );
    assert_eq!(
        failure["models"][0]["last_success_at"],
        row["last_success_at"]
    );
    assert!(!failure.to_string().contains(secret));
    // Status reads must leave the paid-request scheduling marker untouched.
    let ttl = state
        .runtime_state
        .kv_ttl_seconds(&attempt_key("source", "test-model"))
        .await
        .unwrap()
        .unwrap();
    assert!((2397..=2400).contains(&ttl));
}

#[tokio::test]
async fn turn_state_status_handles_legacy_schedule_disabled_expired_and_corrupt_cache() {
    let runtime = runtime();
    runtime
        .kv_set_if_absent(&attempt_key("source", "test-model"), "1", REFRESH_INTERVAL)
        .await
        .unwrap();
    let legacy = collection_status(&runtime, &source(true)).await;
    assert_eq!(legacy["models"][0]["result"], "unknown");
    assert_eq!(legacy["models"][0]["last_attempt_at"], Value::Null);
    seed_ticket(&runtime, "expired-secret", TICKET_TTL.as_secs()).await;
    let expired = collection_status(&runtime, &source(true)).await;
    assert_eq!(expired["models"][0]["ticket_valid"], false);
    assert_eq!(expired["models"][0]["ticket_length"], Value::Null);
    seed_ticket(&runtime, "valid-secret", 0).await;
    let disabled = collection_status(&runtime, &source(false)).await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["models"][0]["result"], "disabled");
    assert_eq!(disabled["models"][0]["ticket_valid"], false);
    assert_eq!(disabled["models"][0]["next_attempt_at"], Value::Null);
    runtime
        .kv_set(&cache_key("source"), "bad-json", Some(TICKET_TTL))
        .await
        .unwrap();
    assert_eq!(
        collection_status(&runtime, &source(true)).await["available"],
        false
    );
    let mut wrong = ticket_cache("test-model", "other-source-secret", 0);
    wrong.source_provider_id = "other".into();
    runtime
        .kv_set(
            &cache_key("source"),
            serde_json::to_string(&wrong).unwrap(),
            Some(TICKET_TTL),
        )
        .await
        .unwrap();
    let mismatch = collection_status(&runtime, &source(true)).await;
    assert_eq!(mismatch["available"], false);
    assert!(!mismatch.to_string().contains("other-source-secret"));
}

#[tokio::test]
async fn turn_state_status_recovers_interrupted_attempt_without_stuck_collecting_label() {
    let runtime = runtime();
    let mut cache = ticket_cache("test-model", "valid-ticket", 300);
    cache.attempts.insert(
        "test-model".into(),
        status::CollectionAttempt {
            started_at: crate::codex_client_release::unix_now_secs() - 61,
            ..Default::default()
        },
    );
    save_cache(&runtime, &cache).await.unwrap();
    let status = collection_status(&runtime, &source(true)).await;
    assert_eq!(status["models"][0]["result"], "failed");
    assert!(status["models"][0]["error"]
        .as_str()
        .unwrap()
        .contains("中断"));
    assert_eq!(status["models"][0]["ticket_valid"], true);
}

#[tokio::test]
async fn turn_state_http_collection_preserves_real_response_headers_and_diagnoses_missing_ticket() {
    use axum::{routing::post, Router};
    let upstream = Router::new().route("/responses", post(|axum::Json(body): axum::Json<Value>| async move {
        let mut headers = http::HeaderMap::new();
        headers.insert("content-type", "text/event-stream".parse().unwrap());
        if body["model"] == "ticket-model" {
            headers.insert(HEADER, "test-upstream-ticket".parse().unwrap());
        }
        (headers, "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let raw = json!({
        "request_id":"local-turn-state-probe", "provider_id":"source", "endpoint_id":"test", "key_id":"test",
        "method":"POST", "url":format!("http://{addr}/responses"), "headers":{"content-type":"application/json"},
        "content_type":"application/json", "body":{"json_body":{"model":"ticket-model","stream":true}},
        "stream":true, "client_api_format":"openai:responses", "provider_api_format":"openai:responses"
    });
    let mut plan: ExecutionPlan = serde_json::from_value(raw).unwrap();
    let runtime = crate::execution_runtime::DirectSyncExecutionRuntime::new();
    let result = runtime.execute_sync(&plan).await.unwrap();
    assert_eq!(
        ticket_from_response(&result).unwrap(),
        "test-upstream-ticket"
    );
    plan.body = RequestBody::from_json(json!({"model":"headerless-model","stream":true}));
    let missing = runtime.execute_sync(&plan).await.unwrap();
    let error = ticket_from_response(&missing).unwrap_err();
    assert!(error.contains("HTTP 200"));
    assert!(error.contains("透传"));
    server.abort();
}

fn account(
    id: &str,
    enabled: bool,
) -> aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey {
    let mut key =
        aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey::new(
            id.into(),
            "source".into(),
            format!("Account {id}"),
            "api_key".into(),
            None,
            false,
        )
        .unwrap();
    set_key_collection_config(
        &mut key.fingerprint,
        Some(json!({"enabled": enabled, "models": ["test-model"]})),
    )
    .unwrap();
    key
}

fn state_with_accounts(
    provider: StoredProviderCatalogProvider,
    keys: Vec<aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey>,
) -> (AppState, Arc<InMemoryProviderCatalogReadRepository>) {
    let repo = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        vec![provider],
        vec![],
        keys,
    ));
    let state = AppState::new().unwrap().with_data_state_for_tests(
        GatewayDataState::with_provider_catalog_reader_for_tests(repo.clone()),
    );
    (state, repo)
}

#[test]
fn account_config_preserves_other_metadata_and_rejects_invalid_settings() {
    let mut fingerprint = Some(json!({"transport_profile": {"profile_id": "keep"}, "unknown": 42}));
    let before = fingerprint.clone();
    assert!(set_key_collection_config(
        &mut fingerprint,
        Some(json!({"enabled": true, "models": []}))
    )
    .is_err());
    assert_eq!(fingerprint, before);
    set_key_collection_config(
        &mut fingerprint,
        Some(json!({"enabled": true, "models": ["test-model"]})),
    )
    .unwrap();
    assert_eq!(fingerprint.as_ref().unwrap()["unknown"], 42);
    set_key_collection_config(&mut fingerprint, None).unwrap();
    assert_eq!(fingerprint, before);
    assert!(validate_config(
        json!({"pool_advanced": {KEY_OVERRIDE_CONFIG: "key"}})
            .as_object()
            .unwrap()
    )
    .is_err());
    assert!(validate_config(
        json!({"pool_advanced": {OVERRIDE_CONFIG: "source", KEY_OVERRIDE_CONFIG: "key"}})
            .as_object()
            .unwrap()
    )
    .is_ok());
    for id in ["account:source:key", "source"] {
        assert!(parse_source_id(id).is_some());
    }
    for id in [
        "account:source:",
        "account:source:key:other",
        "account::key",
        "account: source:key",
    ] {
        assert!(parse_source_id(id).is_none());
    }
}

#[test]
fn account_selection_is_pinned_and_independent_of_pool_scheduling() {
    let now = crate::codex_client_release::unix_now_secs();
    let mut key = account("key", true);
    assert!(!key.is_active);
    assert!(collection_key_eligible(
        &key,
        "source",
        Some("key"),
        "test-model",
        now
    ));
    assert!(!collection_key_eligible(
        &key,
        "source",
        None,
        "test-model",
        now
    ));
    key.is_active = true;
    assert!(!collection_key_eligible(
        &key,
        "source",
        Some("different-key"),
        "test-model",
        now
    ));
    assert!(!collection_key_eligible(
        &key,
        "another-provider",
        Some("key"),
        "test-model",
        now
    ));
    key.allowed_models = Some(json!(["other-model"]));
    assert!(!collection_key_eligible(
        &key,
        "source",
        Some("key"),
        "test-model",
        now
    ));
    key.allowed_models = None;
    key.api_formats = Some(json!(["openai:chat"]));
    assert!(!collection_key_eligible(
        &key,
        "source",
        Some("key"),
        "test-model",
        now
    ));
    key.api_formats = None;
    key.oauth_invalid_at_unix_secs = Some(now);
    assert!(!collection_key_eligible(
        &key,
        "source",
        Some("key"),
        "test-model",
        now
    ));
    key.oauth_invalid_at_unix_secs = None;
    key.expires_at_unix_secs = Some(now);
    assert!(!collection_key_eligible(
        &key,
        "source",
        Some("key"),
        "test-model",
        now
    ));
}

#[tokio::test]
async fn account_and_provider_caches_schedules_status_are_isolated() {
    let keys = vec![
        account("a", true),
        account("b", true),
        account("off", false),
    ];
    let provider = source(true);
    let (state, _) = state_with_accounts(provider.clone(), keys.clone());
    let calls = AtomicUsize::new(0);
    let fetch = async |_: &AppState, source_id: &str, _: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("secret-for-{source_id}"))
    };
    scan_with_fetch(&state, &fetch).await.unwrap();
    scan_with_fetch(&state, &fetch).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    for id in ["source", "account:source:a", "account:source:b"] {
        let cache = load_tickets(&state.runtime_state, Some(id)).await.unwrap();
        assert_eq!(
            cache.for_model("test-model"),
            Some(format!("secret-for-{id}").as_str())
        );
        assert!(cache.for_model("other-model").is_none());
    }
    assert!(
        load_tickets(&state.runtime_state, Some("account:source:off"))
            .await
            .is_none()
    );
    let status = key_collection_status(&state.runtime_state, &provider, &keys[0]).await;
    assert_eq!(status["enabled"], true);
    assert_eq!(status["models"][0]["result"], "success");
    assert_eq!(status["models"][0]["ticket_valid"], true);
    assert!(!status.to_string().contains("secret-for-"));
    assert_eq!(
        account_sources(&provider, &keys).as_array().unwrap().len(),
        2
    );
    let mut stopped = provider.clone();
    stopped.is_active = false;
    assert_eq!(account_sources(&stopped, &keys), json!([]));
}

#[tokio::test]
async fn account_disable_delete_and_parent_stop_clear_tickets_without_fallback() {
    for change in ["disable", "remove_config", "delete", "stop_parent"] {
        let key = account("a", true);
        let (state, repo) = state_with_accounts(source(false), vec![key.clone()]);
        scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
            Ok("secret".into())
        })
        .await
        .unwrap();
        let id = "account:source:a";
        assert!(load_tickets(&state.runtime_state, Some(id)).await.is_some());
        match change {
            "disable" => {
                repo.update_key(&account("a", false)).await.unwrap();
            }
            "remove_config" => {
                let mut key = key.clone();
                key.fingerprint = None;
                repo.update_key(&key).await.unwrap();
            }
            "delete" => {
                repo.delete_key("a").await.unwrap();
            }
            _ => {
                let mut provider = source(false);
                provider.is_active = false;
                repo.update_provider(&provider).await.unwrap();
            }
        }
        scan_with_fetch(
            &state,
            async |_: &AppState, _: &str, _: &str| -> Result<String, String> {
                panic!("must not fetch another account")
            },
        )
        .await
        .unwrap();
        assert!(
            load_tickets(&state.runtime_state, Some(id)).await.is_none(),
            "{change}"
        );
    }
}

#[tokio::test]
async fn inflight_account_disable_does_not_republish_and_failed_refresh_preserves_good_ticket() {
    let (state, repo) = state_with_accounts(source(false), vec![account("a", true)]);
    let id = "account:source:a";
    scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
        Ok("good-secret".into())
    })
    .await
    .unwrap();
    state
        .runtime_state
        .kv_delete(&attempt_key(id, "test-model"))
        .await
        .unwrap();
    assert!(
        scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| Err(
            "failed".into()
        ))
        .await
        .is_err()
    );
    assert_eq!(
        load_tickets(&state.runtime_state, Some(id))
            .await
            .unwrap()
            .for_model("test-model"),
        Some("good-secret")
    );
    state
        .runtime_state
        .kv_delete(&attempt_key(id, "test-model"))
        .await
        .unwrap();
    scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
        repo.update_key(&account("a", false)).await.unwrap();
        Ok("late-secret".into())
    })
    .await
    .unwrap();
    assert!(load_tickets(&state.runtime_state, Some(id)).await.is_none());
}

pub(crate) async fn seed_account_ticket(
    runtime: &RuntimeState,
    provider_id: &str,
    key_id: &str,
    value: &str,
) {
    let mut cache = ticket_cache("test-model", value, 0);
    cache.source_provider_id = account_source_id(provider_id, key_id);
    save_cache(runtime, &cache).await.unwrap();
}
