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
    }
}

pub(crate) async fn seed_ticket(runtime: &RuntimeState, value: &str, age: u64) {
    save_cache(runtime, &ticket_cache("test-model", value, age))
        .await
        .unwrap();
}

async fn load_ticket(runtime: &RuntimeState, enabled: bool) -> Option<String> {
    load_tickets(runtime, enabled)
        .await?
        .for_model("test-model")
        .map(str::to_owned)
}

fn source(enabled: bool) -> StoredProviderCatalogProvider {
    let mut provider =
        StoredProviderCatalogProvider::new("source".into(), "聪明".into(), None, "custom".into())
            .unwrap();
    provider.config =
        Some(json!({"congming_turn_state": {"enabled": enabled, "models": ["test-model"]}}));
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
        json!({"congming_turn_state": true}),
        json!({"congming_turn_state": {"enabled": true}}),
        json!({"congming_turn_state": {"enabled": true, "models": [" "]}}),
        json!({"congming_turn_state": {"enabled": "true", "models": ["test-model"]}}),
        json!({"pool_advanced": {"congming_turn_state_override": "true"}}),
    ] {
        assert!(validate_config(value.as_object().unwrap()).is_err());
    }
    assert!(validate_config(
        json!({"congming_turn_state": {"enabled": false, "models": []}})
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
fn only_exact_host_and_bounded_printable_ascii_tickets_are_accepted() {
    assert!(is_congming_url("https://sub2.congmingai.com/v1/responses"));
    for url in [
        "http://sub2.congmingai.com",
        "https://sub2.congmingai.com.evil.test",
        "https://example.test",
        "https://user@sub2.congmingai.com",
    ] {
        assert!(!is_congming_url(url));
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
async fn disabled_or_multiple_sources_clear_ticket_without_sending_requests() {
    for providers in [
        vec![source(false)],
        vec![source(true), {
            let mut other = source(true);
            other.id = "other".into();
            other
        }],
    ] {
        let (state, _) = state(providers);
        seed_ticket(&state.runtime_state, &"a".repeat(292), 0).await;
        let _ = scan_with_fetch(&state, async |_: &AppState, _: &str, _: &str| {
            panic!("disabled or ambiguous sources must never fetch");
            #[allow(unreachable_code)]
            Ok(String::new())
        })
        .await;
        assert!(load_ticket(&state.runtime_state, true).await.is_none());
    }
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
        Some(json!({"congming_turn_state": {"enabled": true, "models": ["model-a", "model-b"]}}));
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
    let cache = load_tickets(&state.runtime_state, true).await.unwrap();
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
        Some(json!({"congming_turn_state": {"enabled": true, "models": ["model-b"]}}));
    repo.update_provider(&provider).await.unwrap();
    scan_with_fetch(&state, &fetch).await.unwrap();
    let cache = load_tickets(&state.runtime_state, true).await.unwrap();
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
    endpoint.base_url = "https://sub2.congmingai.com".into();
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
    assert_eq!(plan.url, "https://sub2.congmingai.com/v1/responses");
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
        .is_err());
}
