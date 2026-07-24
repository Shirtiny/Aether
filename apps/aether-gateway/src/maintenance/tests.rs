use std::sync::{Arc, Mutex};

use aether_crypto::{encrypt_python_fernet_plaintext, DEVELOPMENT_ENCRYPTION_KEY};
use aether_data::repository::background_tasks::InMemoryBackgroundTaskRepository;
use aether_data::repository::candidates::InMemoryRequestCandidateRepository;
use aether_data::repository::provider_catalog::InMemoryProviderCatalogReadRepository;
use aether_data_contracts::repository::background_tasks::{
    BackgroundTaskKind, BackgroundTaskListQuery, BackgroundTaskReadRepository,
    BackgroundTaskStatus, StoredBackgroundTaskRun,
};
use aether_data_contracts::repository::candidates::{
    RequestCandidateReadRepository, RequestCandidateStatus, RequestCandidateWriteRepository,
    UpsertRequestCandidateRecord,
};
use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogProvider;
use axum::body::Body;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use super::{BackgroundTaskCleanupSummary, ProviderCheckinRunSummary};
use crate::AppState;

async fn start_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = crate::test_support::bind_loopback_listener()
        .await
        .expect("listener should bind");
    let addr = listener.local_addr().expect("local addr should resolve");
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server should run");
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn gateway_background_request_candidate_cleanup_deletes_expired_entries_in_batches() {
    fn now_unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    async fn seed_candidate(
        repository: &InMemoryRequestCandidateRepository,
        id: &str,
        created_at_unix_ms: u64,
    ) {
        repository
            .upsert(UpsertRequestCandidateRecord {
                id: id.to_string(),
                request_id: format!("req-{id}"),
                user_id: Some("user-1".to_string()),
                api_key_id: Some("api-key-1".to_string()),
                username: Some("alice".to_string()),
                api_key_name: Some("default".to_string()),
                candidate_index: 0,
                retry_index: 0,
                provider_id: Some("provider-1".to_string()),
                endpoint_id: Some("endpoint-1".to_string()),
                key_id: Some("key-1".to_string()),
                status: RequestCandidateStatus::Success,
                skip_reason: None,
                is_cached: Some(false),
                status_code: Some(200),
                error_type: None,
                error_message: None,
                latency_ms: Some(10),
                concurrent_requests: Some(1),
                extra_data: None,
                required_capabilities: None,
                created_at_unix_ms: Some(created_at_unix_ms),
                started_at_unix_ms: Some(created_at_unix_ms),
                finished_at_unix_ms: Some(created_at_unix_ms.saturating_add(1)),
            })
            .await
            .expect("candidate should seed");
    }

    let repository = Arc::new(InMemoryRequestCandidateRepository::default());
    seed_candidate(&repository, "cand-expired-1", 1_000).await;
    seed_candidate(&repository, "cand-expired-2", 2_000).await;
    seed_candidate(&repository, "cand-active", now_unix_ms()).await;

    let data_state = crate::data::GatewayDataState::with_request_candidate_repository_for_tests(
        Arc::clone(&repository),
    )
    .with_system_config_values_for_tests([
        ("enable_auto_cleanup".to_string(), json!(true)),
        ("cleanup_batch_size".to_string(), json!(1)),
        (
            "request_candidates_cleanup_batch_size".to_string(),
            json!(1),
        ),
        ("request_candidates_retention_days".to_string(), json!(30)),
    ]);

    let gateway_state = AppState::new()
        .expect("gateway state should build")
        .with_data_state_for_tests(data_state);
    let background_tasks = gateway_state.spawn_background_tasks();
    assert!(!background_tasks.is_empty(), "cleanup worker should spawn");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let rows = repository
            .list_recent(10)
            .await
            .expect("list recent should succeed");
        if rows.len() == 1 {
            assert_eq!(rows[0].id, "cand-active");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cleanup worker did not delete expired request candidates within 500ms"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    background_tasks.shutdown().await;
}

#[tokio::test]
async fn gateway_provider_checkin_runs_local_query_balance_for_configured_provider() {
    fn sample_provider(provider_id: &str, ops_url: &str) -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            provider_id.to_string(),
            "openai".to_string(),
            Some("https://example.com".to_string()),
            "custom".to_string(),
        )
        .expect("provider should build")
        .with_routing_fields(10)
        .with_transport_fields(
            true,
            false,
            true,
            None,
            None,
            None,
            None,
            None,
            Some(json!({
                "provider_ops": {
                    "architecture_id": "generic_api",
                    "base_url": ops_url,
                    "connector": {
                        "auth_type": "api_key",
                        "config": {
                            "auth_method": "bearer"
                        },
                        "credentials": {
                            "api_key": encrypt_python_fernet_plaintext(
                                DEVELOPMENT_ENCRYPTION_KEY,
                                "live-secret-api-key",
                            ).expect("api key should encrypt"),
                        }
                    }
                }
            })),
        )
    }

    let checkin_hits = Arc::new(Mutex::new(0usize));
    let checkin_hits_clone = Arc::clone(&checkin_hits);
    let balance_hits = Arc::new(Mutex::new(0usize));
    let balance_hits_clone = Arc::clone(&balance_hits);
    let ops = Router::new()
        .route(
            "/api/user/checkin",
            post(move |headers: axum::http::HeaderMap| {
                let checkin_hits_inner = Arc::clone(&checkin_hits_clone);
                async move {
                    *checkin_hits_inner.lock().expect("mutex should lock") += 1;
                    assert_eq!(
                        headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        Some("Bearer live-secret-api-key")
                    );
                    (
                        StatusCode::OK,
                        Json(json!({
                            "success": true,
                            "message": "签到成功",
                        })),
                    )
                }
            }),
        )
        .route(
            "/api/user/balance",
            get(move |headers: axum::http::HeaderMap| {
                let balance_hits_inner = Arc::clone(&balance_hits_clone);
                async move {
                    *balance_hits_inner.lock().expect("mutex should lock") += 1;
                    assert_eq!(
                        headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        Some("Bearer live-secret-api-key")
                    );
                    (
                        StatusCode::OK,
                        Json(json!({
                            "success": true,
                            "data": {
                                "quota": 5000000,
                                "used_quota": 1000000
                            }
                        })),
                    )
                }
            }),
        );

    let (ops_url, ops_handle) = start_server(ops).await;
    let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        vec![sample_provider("provider-openai", &ops_url)],
        vec![],
        vec![],
    ));
    let gateway_state = AppState::new()
        .expect("gateway state should build")
        .with_data_state_for_tests(
            crate::data::GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                .with_system_config_values_for_tests([
                    ("enable_provider_checkin".to_string(), json!(true)),
                    ("provider_checkin_time".to_string(), json!("01:05")),
                ])
                .with_encryption_key_for_tests(DEVELOPMENT_ENCRYPTION_KEY),
        );

    let summary = crate::maintenance::perform_provider_checkin_once(&gateway_state)
        .await
        .expect("provider checkin should succeed");

    assert_eq!(
        summary,
        ProviderCheckinRunSummary {
            attempted: 1,
            succeeded: 1,
            failed: 0,
            skipped: 0,
        }
    );
    assert_eq!(*checkin_hits.lock().expect("mutex should lock"), 1);
    assert_eq!(*balance_hits.lock().expect("mutex should lock"), 1);

    ops_handle.abort();
}

#[tokio::test]
async fn gateway_provider_checkin_counts_anyrouter_auto_signin_as_success() {
    fn sample_provider(provider_id: &str, ops_url: &str) -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            provider_id.to_string(),
            "openai".to_string(),
            Some("https://example.com".to_string()),
            "custom".to_string(),
        )
        .expect("provider should build")
        .with_routing_fields(10)
        .with_transport_fields(
            true,
            false,
            true,
            None,
            None,
            None,
            None,
            None,
            Some(json!({
                "provider_ops": {
                    "architecture_id": "anyrouter",
                    "base_url": ops_url,
                    "connector": {
                        "auth_type": "cookie",
                        "config": {},
                        "credentials": {
                            "session_cookie": encrypt_python_fernet_plaintext(
                                DEVELOPMENT_ENCRYPTION_KEY,
                                "session=MTIzfGVIaDRlQUpwWkFOcGJuU3F1d0RfVkhsNWVYa0lkWE5sY201aGJXVUdjM1J5YVc1bkRCQUFCV0ZzYVdObHxzaWc",
                            ).expect("session cookie should encrypt"),
                        }
                    }
                }
            })),
        )
    }

    let sign_in_hits = Arc::new(Mutex::new(0usize));
    let sign_in_hits_clone = Arc::clone(&sign_in_hits);
    let balance_hits = Arc::new(Mutex::new(0usize));
    let balance_hits_clone = Arc::clone(&balance_hits);
    let ops = Router::new()
        .route(
            "/",
            get(|| async move {
                (
                    StatusCode::OK,
                    Body::from(
                        "<html><script>var arg1 = '0123456789abcdef0123456789abcdef01234567';</script></html>",
                    ),
                )
            }),
        )
        .route(
            "/api/user/sign_in",
            post(move |headers: axum::http::HeaderMap| {
                let sign_in_hits_inner = Arc::clone(&sign_in_hits_clone);
                async move {
                    *sign_in_hits_inner.lock().expect("mutex should lock") += 1;
                    let cookie = headers
                        .get(axum::http::header::COOKIE)
                        .and_then(|value| value.to_str().ok())
                        .expect("cookie header should exist");
                    assert!(cookie.contains("acw_sc__v2="));
                    assert!(cookie.contains(
                        "session=MTIzfGVIaDRlQUpwWkFOcGJuU3F1d0RfVkhsNWVYa0lkWE5sY201aGJXVUdjM1J5YVc1bkRCQUFCV0ZzYVdObHxzaWc"
                    ));
                    assert_eq!(
                        headers
                            .get("New-Api-User")
                            .and_then(|value| value.to_str().ok()),
                        Some("42")
                    );
                    (
                        StatusCode::OK,
                        Json(json!({
                            "success": true,
                            "message": "Anyrouter 签到成功",
                        })),
                    )
                }
            }),
        )
        .route(
            "/api/user/self",
            get(move |headers: axum::http::HeaderMap| {
                let balance_hits_inner = Arc::clone(&balance_hits_clone);
                async move {
                    *balance_hits_inner.lock().expect("mutex should lock") += 1;
                    let cookie = headers
                        .get(axum::http::header::COOKIE)
                        .and_then(|value| value.to_str().ok())
                        .expect("cookie header should exist");
                    assert!(cookie.contains("acw_sc__v2="));
                    assert_eq!(
                        headers
                            .get("New-Api-User")
                            .and_then(|value| value.to_str().ok()),
                        Some("42")
                    );
                    (
                        StatusCode::OK,
                        Json(json!({
                            "success": true,
                            "data": {
                                "quota": 2500000,
                                "used_quota": 500000
                            }
                        })),
                    )
                }
            }),
        );

    let (ops_url, ops_handle) = start_server(ops).await;
    let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        vec![sample_provider("provider-anyrouter", &ops_url)],
        vec![],
        vec![],
    ));
    let gateway_state = AppState::new()
        .expect("gateway state should build")
        .with_data_state_for_tests(
            crate::data::GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                .with_system_config_values_for_tests([
                    ("enable_provider_checkin".to_string(), json!(true)),
                    ("provider_checkin_time".to_string(), json!("01:05")),
                ])
                .with_encryption_key_for_tests(DEVELOPMENT_ENCRYPTION_KEY),
        );

    let summary = crate::maintenance::perform_provider_checkin_once(&gateway_state)
        .await
        .expect("provider checkin should succeed");

    assert_eq!(
        summary,
        ProviderCheckinRunSummary {
            attempted: 1,
            succeeded: 1,
            failed: 0,
            skipped: 0,
        }
    );
    assert_eq!(*sign_in_hits.lock().expect("mutex should lock"), 1);
    assert_eq!(*balance_hits.lock().expect("mutex should lock"), 1);

    ops_handle.abort();
}

#[tokio::test]
async fn gateway_provider_checkin_skips_when_disabled_via_system_config() {
    let repository = Arc::new(InMemoryProviderCatalogReadRepository::seed(
        vec![],
        vec![],
        vec![],
    ));
    let gateway_state = AppState::new()
        .expect("gateway state should build")
        .with_data_state_for_tests(
            crate::data::GatewayDataState::with_provider_catalog_repository_for_tests(repository)
                .with_system_config_values_for_tests([(
                    "enable_provider_checkin".to_string(),
                    json!(false),
                )]),
        );

    let summary = crate::maintenance::perform_provider_checkin_once(&gateway_state)
        .await
        .expect("provider checkin should short-circuit");

    assert_eq!(
        summary,
        ProviderCheckinRunSummary {
            attempted: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        }
    );
}

fn stored_background_task_run(
    id: &str,
    status: BackgroundTaskStatus,
    updated_at_unix_secs: u64,
) -> StoredBackgroundTaskRun {
    StoredBackgroundTaskRun {
        id: id.to_string(),
        task_key: "model.fetch.worker".to_string(),
        kind: BackgroundTaskKind::Scheduled,
        trigger: "interval".to_string(),
        status,
        attempt: 1,
        max_attempts: 1,
        owner_instance: Some("test-instance".to_string()),
        progress_percent: 0,
        progress_message: Some("worker booted".to_string()),
        payload_json: None,
        result_json: None,
        error_message: None,
        cancel_requested: false,
        created_by: Some("system".to_string()),
        created_at_unix_secs: updated_at_unix_secs,
        started_at_unix_secs: Some(updated_at_unix_secs),
        finished_at_unix_secs: None,
        updated_at_unix_secs,
    }
}

#[tokio::test]
async fn gateway_background_task_cleanup_deletes_expired_runs_in_batches() {
    const DAY: u64 = 24 * 60 * 60;
    let now = 100 * DAY;

    let repository = Arc::new(InMemoryBackgroundTaskRepository::seed_runs([
        stored_background_task_run(
            "run-expired-1",
            BackgroundTaskStatus::Succeeded,
            now - 60 * DAY,
        ),
        stored_background_task_run(
            "run-expired-2",
            BackgroundTaskStatus::Failed,
            now - 40 * DAY,
        ),
        stored_background_task_run("run-fresh", BackgroundTaskStatus::Succeeded, now - 1 * DAY),
    ]));
    let data_state = crate::data::GatewayDataState::disabled()
        .with_background_task_repository_for_tests(Arc::clone(&repository))
        .with_system_config_values_for_tests([
            ("enable_auto_cleanup".to_string(), json!(true)),
            ("background_task_runs_retention_days".to_string(), json!(30)),
            (
                "background_task_runs_cleanup_batch_size".to_string(),
                json!(1),
            ),
        ]);

    let summary = crate::maintenance::cleanup_background_task_runs_at(&data_state, now)
        .await
        .expect("background task cleanup should succeed");

    assert_eq!(
        summary.deleted_runs, 2,
        "both expired runs should be deleted"
    );
    let remaining = repository
        .list_runs(&BackgroundTaskListQuery {
            offset: 0,
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("listing runs should succeed");
    assert_eq!(remaining.total, 1);
    assert_eq!(remaining.items[0].id, "run-fresh");
}

#[tokio::test]
async fn gateway_background_task_cleanup_deletes_stale_worker_boot_runs() {
    const DAY: u64 = 24 * 60 * 60;
    let now = 100 * DAY;

    // A boot row left behind by a previous process, plus one written by the
    // process that is currently running.
    let repository = Arc::new(InMemoryBackgroundTaskRepository::seed_runs([
        stored_background_task_run(
            "boot:model.fetch.worker:old-instance",
            BackgroundTaskStatus::Running,
            now - 2 * DAY,
        ),
        stored_background_task_run(
            "boot:model.fetch.worker:current-instance",
            BackgroundTaskStatus::Running,
            now - 60,
        ),
        stored_background_task_run("run-active", BackgroundTaskStatus::Running, now - 2 * DAY),
    ]));
    let data_state = crate::data::GatewayDataState::disabled()
        .with_background_task_repository_for_tests(Arc::clone(&repository))
        .with_system_config_values_for_tests([
            ("enable_auto_cleanup".to_string(), json!(true)),
            ("background_task_runs_retention_days".to_string(), json!(30)),
        ]);

    let summary = crate::maintenance::cleanup_background_task_runs_at(&data_state, now)
        .await
        .expect("background task cleanup should succeed");

    assert_eq!(summary.deleted_boot_runs, 1);
    assert_eq!(
        summary.deleted_runs, 0,
        "nothing else is past the retention window"
    );

    assert!(
        repository
            .find_run("boot:model.fetch.worker:old-instance")
            .await
            .expect("lookup should succeed")
            .is_none(),
        "a stale boot row from a dead process is removed outright"
    );

    let current = repository
        .find_run("boot:model.fetch.worker:current-instance")
        .await
        .expect("lookup should succeed")
        .expect("current boot run should still exist");
    assert_eq!(
        current.status,
        BackgroundTaskStatus::Running,
        "a boot row from the live process must not be touched"
    );

    let active = repository
        .find_run("run-active")
        .await
        .expect("lookup should succeed")
        .expect("active run should still exist");
    assert_eq!(
        active.status,
        BackgroundTaskStatus::Running,
        "only worker-boot rows are removed by the stale-boot sweep"
    );
}

#[tokio::test]
async fn gateway_background_task_cleanup_respects_auto_cleanup_switch() {
    const DAY: u64 = 24 * 60 * 60;
    let now = 100 * DAY;

    let repository = Arc::new(InMemoryBackgroundTaskRepository::seed_runs([
        stored_background_task_run(
            "run-expired",
            BackgroundTaskStatus::Succeeded,
            now - 90 * DAY,
        ),
    ]));
    let data_state = crate::data::GatewayDataState::disabled()
        .with_background_task_repository_for_tests(Arc::clone(&repository))
        .with_system_config_values_for_tests([("enable_auto_cleanup".to_string(), json!(false))]);

    let summary = crate::maintenance::cleanup_background_task_runs_at(&data_state, now)
        .await
        .expect("background task cleanup should succeed");

    assert_eq!(summary, BackgroundTaskCleanupSummary::default());
    assert!(
        repository
            .find_run("run-expired")
            .await
            .expect("lookup should succeed")
            .is_some(),
        "cleanup must not delete anything while auto cleanup is disabled"
    );
}
