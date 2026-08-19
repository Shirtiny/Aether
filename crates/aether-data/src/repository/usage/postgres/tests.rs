use std::{collections::BTreeMap, time::Duration};

use chrono::{TimeZone, Utc};
use serde_json::{json, Value};
use sqlx::{Postgres, QueryBuilder};

use super::{
    aggregate_usage_prompt_capture_entries, attach_candidate_client_session_affinity_metadata,
    attach_compressed_body_refs, attach_usage_http_audit_body_refs,
    attach_usage_routing_snapshot_metadata, attach_usage_settlement_pricing_snapshot_metadata,
    attach_ws_session_prompt_capture_metadata, inflate_usage_json_value,
    prepare_prompt_capture_metadata, prepare_request_metadata_for_body_storage,
    prepare_usage_body_storage, resolved_read_usage_body_ref, resolved_write_usage_body_ref,
    split_dashboard_daily_aggregate_range, split_dashboard_hourly_aggregate_range,
    sync_usage_prompt_capture_entries, usage_body_ref, usage_http_audit_body_refs,
    usage_http_audit_capture_mode, usage_metadata_has_prompt_capture, usage_metadata_is_ws_step,
    usage_routing_snapshot_from_usage, usage_settlement_pricing_snapshot_from_usage,
    AggregateRangeSplit, SqlxUsageReadRepository, UsageHttpAuditRefs, UsagePromptCaptureEntry,
    UsagePromptCaptureObservation, UsagePromptCaptureObservationBuffer, UsageRoutingSnapshot,
    UsageSettlementPricingSnapshot, WsPromptCaptureScope, COUNT_USAGE_AUDITS_WITH_CANDIDATE_PREFIX,
    FIND_WS_CONNECTION_PROMPT_CAPTURE_SQL, FIND_WS_PROMPT_CAPTURE_SCOPE_SQL,
    FIND_WS_SESSION_PROMPT_CAPTURE_SQL, FIND_WS_USAGE_SESSION_PROMPT_CAPTURE_SQL,
    FLUSH_USAGE_PROMPT_CAPTURE_OBSERVATIONS_SQL, MAX_INLINE_USAGE_BODY_BYTES,
    UPSERT_USAGE_PROMPT_CAPTURE_ENTRIES_SQL,
};
use crate::driver::postgres::{PostgresPoolConfig, PostgresPoolFactory};
use crate::repository::usage::UpsertUsageRecord;
use aether_data_contracts::repository::usage::UsageBodyField;

#[test]
fn cafecode_filter_uses_exact_identity_match_for_numeric_input() {
    let mut builder = QueryBuilder::<Postgres>::new(r#"SELECT * FROM "usage""#);
    let mut has_where = false;

    super::push_postgres_usage_cafecode_filter(&mut builder, &mut has_where, "45");

    let sql = builder.sql();
    assert!(sql.contains("request_metadata->>'cafecode_uid'"));
    assert!(sql.contains("request_metadata->>'cafecode_uname'"));
    assert!(sql.contains("BTRIM"));
    assert!(sql.contains(" = "));
    assert!(!sql.contains("LIKE"));
}

#[test]
fn cafecode_filter_uses_exact_identity_match_for_non_numeric_input() {
    let mut builder = QueryBuilder::<Postgres>::new(r#"SELECT * FROM "usage""#);
    let mut has_where = false;

    super::push_postgres_usage_cafecode_filter(&mut builder, &mut has_where, "xiapeng8618");

    let sql = builder.sql();
    assert!(sql.contains("request_metadata->>'cafecode_uid'"));
    assert!(sql.contains("request_metadata->>'cafecode_uname'"));
    assert!(sql.contains("BTRIM"));
    assert!(sql.contains(" = "));
    assert!(!sql.contains("LIKE"));
}

#[test]
fn client_family_filter_uses_exact_metadata_match() {
    let mut builder = QueryBuilder::<Postgres>::new(r#"SELECT * FROM "usage""#);
    let mut has_where = false;

    super::push_postgres_usage_client_family_filter(&mut builder, &mut has_where, " web ");

    let sql = builder.sql();
    assert!(sql.contains("client_session_affinity,client_family"));
    assert!(sql.contains("request_metadata->>'client_family'"));
    assert!(sql.contains("request_candidates.extra_data"));
    assert!(sql.contains("BTRIM"));
    assert!(sql.contains("NULLIF"));
    assert!(sql.contains(" = "));
    assert!(!sql.contains("LIKE"));
}

#[test]
fn session_id_filter_reads_candidate_and_usage_affinity() {
    let mut builder = QueryBuilder::<Postgres>::new(COUNT_USAGE_AUDITS_WITH_CANDIDATE_PREFIX);
    let mut has_where = false;

    super::push_postgres_usage_session_id_filter(
        &mut builder,
        &mut has_where,
        "session=session-1",
        true,
    );

    let sql = builder.sql();
    assert!(sql.contains("request_metadata#>>'{client_session_affinity,session_key}'"));
    assert!(sql.contains("request_candidates.extra_data"));
    assert!(sql.contains("LEFT JOIN request_candidates"));
    assert!(sql.contains(" = "));
}

#[test]
fn count_queries_only_join_candidate_affinity_for_relevant_filters() {
    assert!(!super::usage_filters_need_candidate_affinity_join(
        None, None
    ));
    assert!(!super::usage_filters_need_candidate_affinity_join(
        Some(" "),
        Some("")
    ));
    assert!(super::usage_filters_need_candidate_affinity_join(
        Some("session-1"),
        None
    ));
    assert!(super::usage_filters_need_candidate_affinity_join(
        None,
        Some("codex")
    ));
}

#[test]
fn hide_unknown_filter_excludes_unknown_model_and_provider_labels() {
    let mut builder = QueryBuilder::<Postgres>::new(r#"SELECT * FROM "usage""#);
    let mut has_where = false;

    super::push_postgres_usage_hide_unknown_filter(&mut builder, &mut has_where);

    let sql = builder.sql();
    assert!(sql.contains(r#""usage".model"#));
    assert!(sql.contains(r#""usage".provider_name"#));
    assert!(sql.contains("NOT IN ('unknown', 'unknow')"));
}

#[test]
fn compaction_filter_matches_the_partial_index_predicate() {
    let mut builder = QueryBuilder::<Postgres>::new(r#"SELECT * FROM "usage""#);
    let mut has_where = false;

    super::push_postgres_usage_compaction_filter(&mut builder, &mut has_where);

    let predicate = r#"("usage".request_metadata->>'is_compaction') = 'true'"#;
    assert!(builder.sql().contains(predicate));

    let migration = include_str!(
        "../../../../migrations/postgres/20260818155000_add_usage_compaction_filter_index.sql"
    );
    assert!(migration.contains("-- no-transaction"));
    assert!(migration.contains("(created_at DESC, id ASC)"));
    assert!(migration.contains("(request_metadata->>'is_compaction') = 'true'"));
}

#[tokio::test]
async fn repository_constructs_from_lazy_pool() {
    let factory = PostgresPoolFactory::new(PostgresPoolConfig {
        database_url: "postgres://localhost/aether".to_string(),
        min_connections: 1,
        max_connections: 4,
        acquire_timeout_ms: 1_000,
        idle_timeout_ms: 5_000,
        max_lifetime_ms: 30_000,
        statement_cache_capacity: 64,
        require_ssl: false,
    })
    .expect("factory should build");

    let pool = factory.connect_lazy().expect("pool should build");
    let repository = SqlxUsageReadRepository::new(pool);
    let _ = repository.pool();
    let _ = repository.transaction_runner();
}

#[tokio::test]
async fn validates_upsert_before_hitting_database() {
    let factory = PostgresPoolFactory::new(PostgresPoolConfig {
        database_url: "postgres://localhost/aether".to_string(),
        min_connections: 1,
        max_connections: 4,
        acquire_timeout_ms: 1_000,
        idle_timeout_ms: 5_000,
        max_lifetime_ms: 30_000,
        statement_cache_capacity: 64,
        require_ssl: false,
    })
    .expect("factory should build");

    let pool = factory.connect_lazy().expect("pool should build");
    let repository = SqlxUsageReadRepository::new(pool);
    let result = repository
        .upsert(UpsertUsageRecord {
            request_id: "".to_string(),
            user_id: None,
            api_key_id: None,
            username: None,
            api_key_name: None,
            provider_name: "openai".to_string(),
            model: "gpt-5".to_string(),
            target_model: None,
            provider_id: None,
            provider_endpoint_id: None,
            provider_api_key_id: None,
            request_type: Some("chat".to_string()),
            api_format: Some("openai:chat".to_string()),
            api_family: Some("openai".to_string()),
            endpoint_kind: Some("chat".to_string()),
            endpoint_api_format: Some("openai:chat".to_string()),
            provider_api_family: Some("openai".to_string()),
            provider_endpoint_kind: Some("chat".to_string()),
            has_format_conversion: Some(false),
            is_stream: Some(false),
            input_tokens: Some(10),
            output_tokens: Some(20),
            total_tokens: Some(30),
            cache_creation_input_tokens: None,
            cache_creation_ephemeral_5m_input_tokens: None,
            cache_creation_ephemeral_1h_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_cost_usd: None,
            cache_read_cost_usd: None,
            output_price_per_1m: None,
            total_cost_usd: None,
            actual_total_cost_usd: None,
            status_code: Some(200),
            error_message: None,
            error_category: None,
            response_time_ms: Some(100),
            first_byte_time_ms: None,
            status: "completed".to_string(),
            billing_status: "pending".to_string(),
            request_headers: None,
            request_body: None,
            request_body_ref: None,
            provider_request_headers: None,
            provider_request_body: None,
            provider_request_body_ref: None,
            response_headers: None,
            response_body: None,
            response_body_ref: None,
            client_response_headers: None,
            client_response_body: None,
            client_response_body_ref: None,
            request_body_state: None,
            provider_request_body_state: None,
            response_body_state: None,
            client_response_body_state: None,
            candidate_id: None,
            candidate_index: None,
            key_name: None,
            planner_kind: None,
            route_family: None,
            route_kind: None,
            execution_path: None,
            local_execution_runtime_miss_reason: None,
            request_metadata: None,
            finalized_at_unix_secs: None,
            created_at_unix_ms: Some(100),
            updated_at_unix_secs: 101,
        })
        .await;

    assert!(result.is_err());
}

#[test]
fn dashboard_daily_aggregate_split_keeps_partial_days_raw() {
    let start_utc = Utc
        .with_ymd_and_hms(2026, 4, 20, 13, 15, 0)
        .single()
        .unwrap();
    let end_utc = Utc
        .with_ymd_and_hms(2026, 4, 23, 4, 45, 0)
        .single()
        .unwrap();
    let cutoff_utc = Utc.with_ymd_and_hms(2026, 4, 23, 0, 0, 0).single().unwrap();

    let split = split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc);

    assert_eq!(
        split,
        AggregateRangeSplit {
            raw_leading: Some((
                Utc.with_ymd_and_hms(2026, 4, 20, 13, 15, 0)
                    .single()
                    .unwrap(),
                Utc.with_ymd_and_hms(2026, 4, 21, 0, 0, 0).single().unwrap(),
            )),
            aggregate: Some((
                Utc.with_ymd_and_hms(2026, 4, 21, 0, 0, 0).single().unwrap(),
                Utc.with_ymd_and_hms(2026, 4, 23, 0, 0, 0).single().unwrap(),
            )),
            raw_trailing: Some((
                Utc.with_ymd_and_hms(2026, 4, 23, 0, 0, 0).single().unwrap(),
                Utc.with_ymd_and_hms(2026, 4, 23, 4, 45, 0)
                    .single()
                    .unwrap(),
            )),
        }
    );
}

#[test]
fn dashboard_hourly_aggregate_split_keeps_partial_hours_raw() {
    let start_utc = Utc
        .with_ymd_and_hms(2026, 4, 20, 10, 15, 0)
        .single()
        .unwrap();
    let end_utc = Utc
        .with_ymd_and_hms(2026, 4, 20, 15, 30, 0)
        .single()
        .unwrap();
    let cutoff_utc = Utc
        .with_ymd_and_hms(2026, 4, 20, 15, 0, 0)
        .single()
        .unwrap();

    let split = split_dashboard_hourly_aggregate_range(start_utc, end_utc, cutoff_utc);

    assert_eq!(
        split,
        AggregateRangeSplit {
            raw_leading: Some((
                Utc.with_ymd_and_hms(2026, 4, 20, 10, 15, 0)
                    .single()
                    .unwrap(),
                Utc.with_ymd_and_hms(2026, 4, 20, 11, 0, 0)
                    .single()
                    .unwrap(),
            )),
            aggregate: Some((
                Utc.with_ymd_and_hms(2026, 4, 20, 11, 0, 0)
                    .single()
                    .unwrap(),
                Utc.with_ymd_and_hms(2026, 4, 20, 15, 0, 0)
                    .single()
                    .unwrap(),
            )),
            raw_trailing: Some((
                Utc.with_ymd_and_hms(2026, 4, 20, 15, 0, 0)
                    .single()
                    .unwrap(),
                Utc.with_ymd_and_hms(2026, 4, 20, 15, 30, 0)
                    .single()
                    .unwrap(),
            )),
        }
    );
}

#[test]
fn usage_sql_does_not_require_updated_at_column() {
    assert!(!super::FIND_BY_REQUEST_ID_SQL.contains("COALESCE(updated_at, created_at)"));
    assert!(!super::LIST_USAGE_AUDITS_PREFIX.contains("COALESCE(updated_at, created_at)"));
    assert!(!super::UPSERT_SQL.contains("\n  updated_at\n"));
    assert!(!super::UPSERT_SQL.contains("updated_at = CASE"));
}

#[test]
fn usage_sql_summarizes_tokens_by_api_key_ids_in_database() {
    assert!(super::SUMMARIZE_TOTAL_TOKENS_BY_API_KEY_IDS_SQL.contains("GROUP BY api_key_id"));
    assert!(super::SUMMARIZE_TOTAL_TOKENS_BY_API_KEY_IDS_SQL.contains("ANY($1::TEXT[])"));
}

#[test]
fn usage_sql_summarizes_usage_by_provider_api_key_ids_in_database() {
    assert!(super::SUMMARIZE_USAGE_BY_PROVIDER_API_KEY_IDS_SQL.contains("FROM provider_api_keys"));
    assert!(super::SUMMARIZE_USAGE_BY_PROVIDER_API_KEY_IDS_SQL
        .contains("COALESCE(request_count, 0) > 0"));
    assert!(super::SUMMARIZE_USAGE_BY_PROVIDER_API_KEY_IDS_SQL.contains("ANY($1::TEXT[])"));
}

#[test]
fn usage_sql_rebuilds_provider_key_window_usage_into_status_snapshot() {
    assert!(super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL
        .contains("UPDATE provider_api_keys AS keys"));
    assert!(super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL
        .contains("usage_billing_facts"));
    assert!(super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL
        .contains("provider_type', ''))) = 'codex'"));
    assert!(
        super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL.contains("'{quota,windows}'")
    );
    assert!(super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL.contains("'{usage}'"));
    assert!(
        super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL.contains("WHEN '5h' THEN 300")
    );
    assert!(super::REBUILD_PROVIDER_API_KEY_CODEX_WINDOW_USAGE_STATS_SQL
        .contains("WHEN 'weekly' THEN 10080"));
}

#[test]
fn usage_sql_serializes_request_id_upserts_before_reading_previous_usage() {
    assert!(super::LOCK_USAGE_REQUEST_ID_SQL.contains("pg_advisory_xact_lock"));
    assert!(super::LOCK_USAGE_REQUEST_ID_SQL.contains("hashtext($1)::BIGINT"));
    assert!(include_str!("mod.rs")
        .contains("lock_usage_request_id_in_tx(tx, &usage.request_id).await?;"));
}

#[test]
fn usage_sql_moves_shared_counter_updates_behind_outbox() {
    let source = include_str!("mod.rs");
    assert!(super::INSERT_USAGE_COUNTER_DELTA_SQL.contains("usage_counter_deltas"));
    assert!(super::CLAIM_USAGE_COUNTER_DELTAS_SQL.contains("FOR UPDATE SKIP LOCKED"));
    assert!(super::MARK_USAGE_COUNTER_DELTAS_PROCESSED_SQL.contains("processed_at = NOW()"));
    assert!(super::TRY_LOCK_USAGE_COUNTER_FLUSH_SQL.contains("pg_try_advisory_xact_lock"));
    assert!(source.contains("enqueue_api_key_usage_delta_in_tx("));
    assert!(source.contains("enqueue_provider_api_key_usage_delta_in_tx("));
    assert!(source.contains("enqueue_model_usage_delta_in_tx("));
    assert!(source.contains("apply_provider_api_key_main_usage_delta_in_tx("));
    assert!(source.contains("USAGE_COUNTER_KIND_PROVIDER_MONTHLY"));
    assert!(source.contains("apply_provider_monthly_usage_delta_in_tx("));
}

#[test]
fn proxy_usage_counter_replay_uses_a_stable_scoped_outbox_identity() {
    let first = super::proxy_node_counter_delta_idempotency_id("node-1", "request-1");
    let replay = super::proxy_node_counter_delta_idempotency_id("node-1", "request-1");
    let other_request = super::proxy_node_counter_delta_idempotency_id("node-1", "request-2");
    let other_node = super::proxy_node_counter_delta_idempotency_id("node-2", "request-1");

    assert_eq!(first, replay);
    assert_ne!(first, other_request);
    assert_ne!(first, other_node);
    assert!(super::INSERT_USAGE_COUNTER_DELTA_SQL.contains("ON CONFLICT (id) DO NOTHING"));
}

#[test]
fn usage_counter_delta_aggregation_preserves_net_response_time_delta() {
    fn provider_key_delta_row(id: &str, response_time_delta: i64) -> super::UsageCounterDeltaRow {
        super::UsageCounterDeltaRow {
            id: id.to_string(),
            kind: super::USAGE_COUNTER_KIND_PROVIDER_API_KEY.to_string(),
            target_id: "provider-key-1".to_string(),
            request_count_delta: 0,
            total_requests_delta: 0,
            success_count_delta: 0,
            error_count_delta: 0,
            dns_failures_delta: 0,
            stream_errors_delta: 0,
            total_tokens_delta: 0,
            total_cost_usd_delta: 0.0,
            total_response_time_ms_delta: response_time_delta,
            last_used_at_unix_secs: None,
            last_used_ip: None,
            candidate_last_used_at_unix_secs: None,
            removed_last_used_at_unix_secs: None,
            usage_created_at_unix_secs: None,
        }
    }

    let cap = crate::repository::usage::PROVIDER_API_KEY_TOTAL_RESPONSE_TIME_MS_MAX;
    let aggregates = super::UsageCounterDeltaAggregates::from_rows(&[
        provider_key_delta_row("delta-1", cap),
        provider_key_delta_row("delta-2", cap),
        provider_key_delta_row("delta-3", -cap),
    ])
    .expect("aggregation should succeed");

    let delta = aggregates
        .provider_api_keys
        .get("provider-key-1")
        .expect("provider key aggregate");
    assert_eq!(delta.total_response_time_ms, cap);
}

#[test]
fn usage_sql_exposes_counter_outbox_health() {
    assert!(super::READ_USAGE_COUNTER_HEALTH_SQL.contains("pending_rows"));
    assert!(super::READ_USAGE_COUNTER_HEALTH_SQL.contains("oldest_pending_created_at_unix_secs"));
    assert!(super::READ_USAGE_COUNTER_HEALTH_SQL.contains("latest_processed_at_unix_secs"));
    assert!(super::READ_PENDING_USAGE_COUNTER_DELTAS_BY_KIND_SQL.contains("GROUP BY kind"));
}

#[test]
fn usage_sql_rebuild_matches_online_api_key_usage_semantics() {
    assert!(super::REBUILD_API_KEY_USAGE_STATS_SQL.contains("COUNT(*)::BIGINT"));
    assert!(super::REBUILD_API_KEY_USAGE_STATS_SQL.contains("COALESCE("));
    assert!(super::REBUILD_API_KEY_USAGE_STATS_SQL.contains("total_tokens,"));
    assert!(super::REBUILD_API_KEY_USAGE_STATS_SQL
        .contains("COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)"));
    assert!(super::REBUILD_API_KEY_USAGE_STATS_SQL.contains("AND BTRIM(api_key_id) <> ''"));
    assert!(super::REBUILD_API_KEY_USAGE_STATS_SQL
        .contains("AND status NOT IN ('pending', 'streaming')"));
}

#[test]
fn usage_sql_rebuild_matches_online_provider_key_usage_semantics() {
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL.contains("COUNT(*)::BIGINT"));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL
        .contains("NULLIF(BTRIM(error_message), '') IS NULL"));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL.contains("COALESCE("));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL.contains("total_tokens,"));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL
        .contains("COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)"));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL
        .contains("AND BTRIM(provider_api_key_id) <> ''"));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL
        .contains("WHEN status NOT IN ('pending', 'streaming')"));
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL
        .contains("WHEN status IN ('pending', 'streaming') THEN 0"));
}

#[test]
fn usage_sql_supports_recent_usage_audits_query() {
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("FROM \"usage\""));
}

#[test]
fn usage_sql_cache_affinity_interval_query_coalesces_nullable_model_values() {
    assert!(include_str!("mod.rs").contains("COALESCE(\"usage\".model, '') AS model"));
}

#[test]
fn usage_sql_cache_affinity_interval_query_casts_interval_minutes_to_double_precision() {
    assert!(include_str!("mod.rs").contains("AS DOUBLE PRECISION) AS interval_minutes"));
}

#[test]
fn usage_sql_summarize_usage_audits_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_audits_from_daily_aggregates"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("cache_creation_ephemeral_5m_tokens"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_cache_hit_summary_supports_global_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_cache_hit_summary_from_daily_aggregates"));
    assert!(source.contains("summarize_usage_cache_hit_summary_from_hourly_aggregates"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_hourly"));
    assert!(
        source.contains("split_dashboard_hourly_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_cache_affinity_hit_summary_supports_global_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_cache_affinity_hit_summary_from_daily_aggregates"));
    assert!(source.contains("summarize_usage_cache_affinity_hit_summary_from_hourly_aggregates"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_hourly"));
    assert!(source.contains("completed_total_requests"));
    assert!(
        source.contains("split_dashboard_hourly_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_settled_cost_supports_user_and_global_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_settled_cost_from_daily_aggregates"));
    assert!(source.contains("summarize_usage_settled_cost_from_hourly_aggregates"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("FROM stats_hourly"));
    assert!(source.contains("FROM stats_hourly_user"));
    assert!(source.contains("settled_total_cost"));
    assert!(
        source.contains("split_dashboard_hourly_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_error_distribution_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_error_distribution_from_daily_aggregates"));
    assert!(source.contains("FROM stats_daily_error"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_performance_percentiles_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_performance_percentiles_from_daily_aggregates"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("p50_response_time_ms"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_cost_savings_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_cost_savings_from_daily_aggregates"));
    assert!(source.contains("FROM stats_daily_cost_savings"));
    assert!(source.contains("FROM stats_daily_cost_savings_model_provider"));
    assert!(source.contains("FROM stats_user_daily_cost_savings"));
    assert!(source.contains("FROM stats_user_daily_cost_savings_model_provider"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_time_series_supports_global_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_time_series_from_daily_aggregates"));
    assert!(source.contains("summarize_usage_time_series_from_hourly_aggregates"));
    assert!(source.contains("FROM stats_hourly"));
    assert!(
        source.contains("split_dashboard_hourly_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_summarize_usage_daily_heatmap_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_daily_heatmap_from_daily_aggregates"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("total_requests::BIGINT AS total_requests"));
    assert!(source.contains("total_cost::DOUBLE PRECISION AS total_cost"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_daily_cutoff_falls_back_to_imported_stats_daily() {
    let source = include_str!("mod.rs");
    assert!(source.contains("FROM stats_summary"));
    assert!(source.contains("SELECT MAX(date) AS latest_date"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("FROM stats_daily_api_key"));
    assert!(source.contains("value + chrono::Duration::days(1)"));
}

#[test]
fn usage_sql_dashboard_daily_breakdown_falls_back_to_daily_totals() {
    let source = include_str!("mod.rs");
    assert!(source.contains("list_dashboard_daily_breakdown_from_daily_totals"));
    assert!(source.contains("'aggregate'::TEXT AS model"));
    assert!(source.contains("FROM stats_daily"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("detailed_dates.contains(&item.date)"));
    assert!(source.contains("query.tz_offset_minutes != 0"));
    assert!(source.contains("return self.list_dashboard_daily_breakdown_raw(query).await;"));
    assert!(!source.contains("aggregate_dates.insert(item.date.clone())"));
}

#[test]
fn usage_sql_dashboard_daily_breakdown_keeps_all_local_day_model_provider_rows() {
    let source = include_str!("mod.rs");
    let raw_breakdown = source
        .split("async fn list_dashboard_daily_breakdown_raw")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub async fn list_dashboard_daily_breakdown")
                .next()
        })
        .expect("raw daily breakdown function should be present");
    assert!(raw_breakdown.contains("GROUP BY date, \"usage\".model, \"usage\".provider_name"));
    assert!(raw_breakdown.contains("ORDER BY date ASC, total_cost_usd DESC"));
    assert!(raw_breakdown.contains("(\"usage\".created_at AT TIME ZONE 'UTC')"));
    assert!(!raw_breakdown.contains("date_trunc('day', \"usage\".created_at +"));
    assert!(!source.contains("if aggregate_dates.insert(item.date.clone())"));
}

#[test]
fn usage_sql_summarize_usage_leaderboard_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("summarize_usage_leaderboard_from_daily_aggregates"));
    assert!(source.contains("FROM stats_daily_model"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("FROM stats_daily_api_key"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_aggregate_usage_audits_supports_daily_model_and_provider_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("aggregate_usage_audits_from_daily_aggregates"));
    assert!(source.contains("stats_user_daily_model"));
    assert!(source.contains("stats_user_daily_provider"));
    assert!(source.contains("stats_user_daily_api_format"));
    assert!(source.contains("absorb_usage_audit_aggregation_rows"));
    assert!(
        source.contains("split_dashboard_daily_aggregate_range(start_utc, end_utc, cutoff_utc)")
    );
}

#[test]
fn usage_sql_provider_aggregation_excludes_unknown_provider_labels() {
    let source = include_str!("mod.rs");
    assert!(source.contains("const USAGE_PROVIDER_IDENTITY_FILTER_SQL"));
    assert!(source.contains("const USAGE_PROVIDER_IDENTITY_SOURCE_SQL"));
    assert!(source.contains(r#"BTRIM(COALESCE("usage".provider_id, '')) <> ''"#));
    assert!(source.contains(r#"BTRIM(COALESCE("usage".provider_name, '')) <> ''"#));
    assert!(source.contains("LEFT JOIN providers AS provider_by_id"));
    assert!(source.contains("provider_by_id.id = BTRIM(\"usage\".provider_id)"));
    assert!(source.contains("COALESCE(\n      provider_by_id.id,\n      CASE"));
    assert!(
        source.contains(
            "ELSE BTRIM(\"usage\".provider_id)\n      END,\n      CASE\n        WHEN BTRIM(COALESCE(\"usage\".provider_name, ''))"
        )
    );
    assert!(source.contains("COALESCE(\n      provider_by_id.name,"));
    assert!(!source.contains("provider_by_name.name = BTRIM(\"usage\".provider_name)"));
    assert!(source.contains("{provider_identity_source_expr} AS provider_identity_source"));
    assert!(source.contains(r#"secondary_name_expr: "provider_identity_source""#));
    assert!(source
        .contains("COUNT(*) FILTER (WHERE secondary_name = 'provider_id') > 0 THEN 'provider_id'"));
    assert!(source.contains(
        "if matches!(query.group_by, UsageAuditAggregationGroupBy::Provider) {\n            return self.aggregate_usage_audits_raw(query).await;"
    ));
    assert!(source.contains("exclude_reserved_provider_labels"));
    assert!(source.contains(
        r#"lower(BTRIM(COALESCE(provider_name, ''))) NOT IN ('unknown', 'unknow', 'pending')"#
    ));
    assert!(source.contains("MAX(display_name)"));
    assert!(!source.contains("COALESCE(MAX(NULLIF(display_name, 'Unknown')), 'Unknown')"));
}

#[test]
fn usage_sql_summarize_total_tokens_by_api_key_ids_supports_daily_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("FROM stats_daily_api_key"));
    assert!(source.contains("read_stats_daily_cutoff_date().await?"));
}

#[test]
fn dashboard_aggregate_schema_mismatch_detector_matches_legacy_schema_failures() {
    assert!(super::dashboard_aggregate_schema_mismatch_message(
        "postgres error: error occurred while decoding column \"cutoff_date\": \
         mismatched types; Rust type `chrono::DateTime<Utc>` (as SQL type `TIMESTAMPTZ`) \
         is not compatible with SQL type `INT8`"
    ));
    assert!(super::dashboard_aggregate_schema_mismatch_message(
        "postgres error: db error: ERROR: relation \"stats_daily_model_provider\" does not exist"
    ));
    assert!(super::dashboard_aggregate_schema_mismatch_message(
        "postgres error: db error: ERROR: column \"effective_input_tokens\" does not exist"
    ));
    assert!(!super::dashboard_aggregate_schema_mismatch_message(
        "postgres error: db error: ERROR: permission denied for relation stats_daily"
    ));
}

#[test]
fn dashboard_aggregate_reads_fallback_to_raw_on_schema_mismatch() {
    let source = include_str!("mod.rs");
    assert!(source.contains("dashboard_should_fallback_to_raw_on_aggregate_error"));
    assert!(
        source.contains("Err(err) if dashboard_should_fallback_to_raw_on_aggregate_error(&err)")
    );
    assert!(source.contains("return self.list_dashboard_daily_breakdown_raw(query).await;"));
}

#[test]
fn usage_sql_summarize_usage_totals_by_user_ids_supports_user_summary_aggregates() {
    let source = include_str!("mod.rs");
    assert!(source.contains("FROM stats_user_summary"));
    assert!(source.contains("all_time_input_tokens"));
    assert!(source.contains("FROM stats_user_daily"));
    assert!(source.contains("date < $2"));
    assert!(source.contains("summary_user_ids.contains(&user_id)"));
}

#[test]
fn usage_sql_raw_aggregates_use_canonical_billing_facts() {
    let source = include_str!("mod.rs");
    assert!(source.contains("FROM usage_billing_facts AS \"usage\""));
    assert!(
        super::REBUILD_API_KEY_USAGE_STATS_SQL.contains("FROM usage_billing_facts AS \"usage\"")
    );
    assert!(super::REBUILD_PROVIDER_API_KEY_USAGE_STATS_SQL
        .contains("FROM usage_billing_facts AS \"usage\""));
    assert!(super::SUMMARIZE_TOTAL_TOKENS_BY_API_KEY_IDS_SQL
        .contains("FROM usage_billing_facts AS \"usage\""));
    assert!(super::SUMMARIZE_USAGE_TOTALS_BY_USER_IDS_SQL
        .contains("FROM usage_billing_facts AS \"usage\""));
    assert!(!source.contains("apply_provider_api_key_codex_window_usage_delta_in_tx"));
}

#[test]
fn usage_sql_provider_performance_reads_upstream_stream_from_billing_facts() {
    let source = include_str!("mod.rs");
    assert!(source.contains("\"usage\".upstream_is_stream"));
    assert!(
        !source.contains("usage_base.request_metadata->>'upstream_is_stream'"),
        "provider performance queries should not rejoin public.usage to resolve upstream stream mode"
    );
    assert!(
        !source.contains("LEFT JOIN public.usage AS usage_base"),
        "provider performance queries should stay on usage_billing_facts to avoid an extra usage scan"
    );
}

#[test]
fn usage_billing_facts_projects_upstream_stream_mode() {
    let migration = include_str!(
        "../../../../migrations/postgres/20260505130000_project_upstream_stream_in_usage_billing_facts.sql"
    );

    assert!(migration.contains("AS upstream_is_stream"));
    assert!(migration.contains("COALESCE(usage_rows.upstream_is_stream"));
    assert!(migration.contains("COALESCE(usage_rows.is_stream, FALSE)"));
    assert!(migration.contains("ADD COLUMN IF NOT EXISTS upstream_is_stream boolean"));
    assert!(
        !migration.contains("request_metadata->>'upstream_is_stream'"),
        "migration should avoid backfilling historical usage rows from request metadata"
    );
}

#[test]
fn usage_sql_reads_http_audits_for_single_record_fetches() {
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("LEFT JOIN usage_http_audits"));
    assert!(super::FIND_BY_ID_SQL.contains("LEFT JOIN usage_http_audits"));
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("http_request_body_ref"));
    assert!(super::FIND_BY_ID_SQL.contains("http_client_response_body_ref"));
}

#[test]
fn usage_sql_reads_routing_snapshots_for_single_record_fetches() {
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("LEFT JOIN usage_routing_snapshots"));
    assert!(super::FIND_BY_ID_SQL.contains("LEFT JOIN usage_routing_snapshots"));
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("LEFT JOIN request_candidates"));
    assert!(super::FIND_BY_ID_SQL.contains("LEFT JOIN request_candidates"));
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("routing_client_session_affinity"));
    assert!(super::FIND_BY_ID_SQL.contains("routing_client_session_affinity"));
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("routing_candidate_id"));
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("routing_candidate_index"));
    assert!(super::FIND_BY_ID_SQL.contains("routing_local_execution_runtime_miss_reason"));
}

#[test]
fn usage_sql_reads_settlement_snapshots_for_single_record_fetches() {
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("LEFT JOIN usage_settlement_snapshots"));
    assert!(super::FIND_BY_ID_SQL.contains("LEFT JOIN usage_settlement_snapshots"));
    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("settlement_billing_snapshot_schema_version"));
    assert!(super::FIND_BY_ID_SQL.contains("settlement_price_per_request"));
    for sql in [super::FIND_BY_REQUEST_ID_SQL, super::FIND_BY_ID_SQL] {
        assert!(sql.contains("CAST(\"usage\".input_tokens AS INTEGER) AS input_tokens"));
        assert!(sql.contains(
            "usage_settlement_snapshots.billing_input_tokens AS settlement_billing_input_tokens"
        ));
        assert!(sql.contains("usage_settlement_snapshots.billing_cache_creation_5m_tokens"));
        assert!(sql.contains(
            "CAST(usage_settlement_snapshots.billing_total_cost_usd AS DOUBLE PRECISION)"
        ));
    }
}

#[test]
fn usage_sql_qualifies_shared_usage_columns_for_single_record_fetches() {
    for sql in [super::FIND_BY_REQUEST_ID_SQL, super::FIND_BY_ID_SQL] {
        assert!(sql.contains("\"usage\".request_id"));
        assert!(
                sql.contains(
                    "COALESCE(usage_settlement_snapshots.billing_status, \"usage\".billing_status) AS billing_status"
                )
            );
        assert!(sql
            .contains("CAST(usage_settlement_snapshots.output_price_per_1m AS DOUBLE PRECISION)"));
        assert!(sql.contains("EXTRACT(EPOCH FROM \"usage\".created_at)"));
        assert!(sql.contains("usage_settlement_snapshots.finalized_at"));
        assert!(!sql.contains("CAST(EXTRACT(EPOCH FROM created_at) AS BIGINT)"));
        assert!(!sql.contains("CAST(output_price_per_1m AS DOUBLE PRECISION)"));
    }

    assert!(super::FIND_BY_REQUEST_ID_SQL.contains("WHERE \"usage\".request_id = $1"));
    assert!(super::FIND_BY_ID_SQL.contains("WHERE \"usage\".id = $1"));
}

#[test]
fn usage_sql_uses_json_null_placeholders_for_usage_payload_columns() {
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("NULL::json AS request_headers"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("NULL::json AS provider_request_body"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("NULL::bytea AS request_body_compressed"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("NULL::varchar AS http_request_body_ref"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("NULL::varchar AS http_request_body_state"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX
        .contains("NULL::varchar AS http_client_response_body_state"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("usage_routing_snapshots.candidate_id"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("usage_routing_snapshots.candidate_index"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("request_metadata->>'candidate_index'"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("LEFT JOIN usage_routing_snapshots"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("LEFT JOIN request_candidates"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("routing_client_session_affinity"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("LEFT JOIN usage_settlement_snapshots"));
    assert!(super::LIST_USAGE_AUDITS_PREFIX.contains("settlement_billing_snapshot_schema_version"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("NULL::json AS request_headers"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("NULL::json AS provider_request_body"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX
        .contains("NULL::bytea AS client_response_body_compressed"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX
        .contains("NULL::varchar AS http_client_response_body_ref"));
    assert!(
        super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("NULL::varchar AS http_request_body_state")
    );
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX
        .contains("NULL::varchar AS http_client_response_body_state"));
    assert!(
        super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("usage_routing_snapshots.candidate_index")
    );
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("request_metadata->>'candidate_index'"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("LEFT JOIN usage_routing_snapshots"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("LEFT JOIN request_candidates"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("routing_client_session_affinity"));
    assert!(
        super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("usage_routing_snapshots.execution_path")
    );
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("LEFT JOIN usage_settlement_snapshots"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("settlement_price_per_request"));
    for sql in [
        super::LIST_USAGE_AUDITS_PREFIX,
        super::LIST_RECENT_USAGE_AUDITS_PREFIX,
    ] {
        assert!(sql.contains("jsonb_strip_nulls(jsonb_build_object("));
        assert!(sql.contains("jsonb_typeof(\"usage\".provider_request_body::jsonb)"));
        assert!(!sql.contains("jsonb_typeof(\"usage\".provider_request_body)"));
        assert!(sql.contains("'client_ip'"));
        assert!(sql.contains("request_metadata->>'client_ip'"));
        assert!(sql.contains("'user_agent'"));
        assert!(sql.contains("request_metadata->>'user_agent'"));
        assert!(sql.contains("'cafecode_uid'"));
        assert!(sql.contains("request_metadata->>'cafecode_uid'"));
        assert!(sql.contains("'cafecode_uname'"));
        assert!(sql.contains("request_metadata->>'cafecode_uname'"));
        assert!(sql.contains("'is_risk_control'"));
        assert!(sql.contains("request_metadata->>'is_risk_control'"));
        assert!(sql.contains("'is_compaction'"));
        assert!(sql.contains("request_metadata->>'is_compaction'"));
        assert!(sql.contains("'compaction_version'"));
        assert!(sql.contains("request_metadata->>'compaction_version'"));
        assert!(sql.contains("AS client_family"));
        assert!(sql.contains("'client_session_affinity'"));
        assert!(sql.contains("'session_key'"));
        assert!(sql.contains("request_metadata->'client_session_affinity'->>'session_key'"));
        assert!(sql.contains("request_metadata->'client_session_affinity'->>'client_family'"));
        assert!(sql.contains("request_metadata->>'client_family'"));
        assert!(sql.contains("'reasoning_output_tokens'"));
        assert!(sql.contains("request_metadata->'dimensions'->>'reasoning_output_tokens'"));
        assert!(sql.contains("request_metadata->'dimensions'->'reasoning_output_tokens'"));
        assert!(sql.contains("CAST(\"usage\".input_tokens AS INTEGER) AS input_tokens"));
        assert!(sql.contains(
            "usage_settlement_snapshots.billing_input_tokens AS settlement_billing_input_tokens"
        ));
        assert!(sql.contains("usage_settlement_snapshots.billing_cache_creation_1h_tokens"));
        assert!(sql.contains(
            "CAST(usage_settlement_snapshots.billing_total_cost_usd AS DOUBLE PRECISION)"
        ));
    }
    assert!(!super::LIST_USAGE_AUDITS_PREFIX.contains("NULL::jsonb"));
    assert!(!super::LIST_RECENT_USAGE_AUDITS_PREFIX.contains("NULL::jsonb"));
}

#[test]
fn usage_sql_filters_risk_control_flag_in_database() {
    let source = include_str!("mod.rs");
    assert!(source.contains(r#"("usage".request_metadata->>'is_risk_control') = 'true'"#));
}

#[test]
fn usage_sql_reads_list_output_price_from_settlement_snapshots_before_legacy_usage_column() {
    assert!(super::LIST_USAGE_AUDITS_PREFIX
        .contains("CAST(usage_settlement_snapshots.output_price_per_1m AS DOUBLE PRECISION)"));
    assert!(super::LIST_RECENT_USAGE_AUDITS_PREFIX
        .contains("CAST(usage_settlement_snapshots.output_price_per_1m AS DOUBLE PRECISION)"));
}

#[test]
fn usage_sql_casts_json_payload_bind_parameters_explicitly() {
    for placeholder in [41, 42, 44, 45, 47, 48, 50, 51, 53] {
        assert!(
            super::UPSERT_SQL.contains(format!("${placeholder}::json").as_str()),
            "missing ::json cast for placeholder ${placeholder}"
        );
    }
}

#[test]
fn usage_sql_insert_values_aligns_request_metadata_and_timestamps() {
    assert!(super::UPSERT_SQL.contains("\n  $51::json,\n  $52,\n  $53::json,\n  CASE"));
    assert!(super::UPSERT_SQL.contains("WHEN $54 IS NULL THEN NULL"));
    assert!(super::UPSERT_SQL.contains("TO_TIMESTAMP($55::double precision)"));
}

#[test]
fn usage_sql_upsert_materializes_upstream_stream_mode() {
    assert!(super::UPSERT_SQL.contains("upstream_is_stream,"));
    assert!(super::UPSERT_SQL.contains("$53::json->>'upstream_is_stream'"));
    assert!(super::UPSERT_SQL.contains("COALESCE($21, FALSE)"));
    assert!(super::UPSERT_SQL.contains("upstream_is_stream = CASE"));
}

#[test]
fn usage_sql_upsert_returning_includes_routing_placeholders() {
    assert!(super::UPSERT_SQL.contains("NULL::varchar AS http_request_body_state"));
    assert!(super::UPSERT_SQL.contains("NULL::varchar AS http_client_response_body_state"));
    assert!(super::UPSERT_SQL.contains("NULL::varchar AS routing_candidate_id"));
    assert!(super::UPSERT_SQL.contains("NULL::varchar AS routing_planner_kind"));
    assert!(super::UPSERT_SQL.contains("NULL::varchar AS routing_execution_path"));
    assert!(
        super::UPSERT_SQL.contains("NULL::varchar AS settlement_billing_snapshot_schema_version")
    );
    assert!(super::UPSERT_SQL.contains("NULL::double precision AS settlement_input_price_per_1m"));
    assert!(super::UPSERT_SQL.contains("input_output_total_tokens"));
    assert!(super::UPSERT_SQL.contains("input_context_tokens"));
}

#[test]
fn usage_sql_writes_usage_settlement_pricing_snapshots() {
    assert!(super::UPSERT_USAGE_SETTLEMENT_PRICING_SNAPSHOT_SQL
        .contains("INSERT INTO usage_settlement_snapshots"));
    assert!(super::UPSERT_USAGE_SETTLEMENT_PRICING_SNAPSHOT_SQL
        .contains("billing_snapshot_schema_version"));
    assert!(super::UPSERT_USAGE_SETTLEMENT_PRICING_SNAPSHOT_SQL.contains("price_per_request"));
}

#[test]
fn usage_sql_upsert_recovers_missing_provider_links_after_billing_finalizes() {
    for assignment in [
        "provider_id = CASE WHEN \"usage\".billing_status = 'pending' OR (\"usage\".provider_id IS NULL AND (\"usage\".provider_endpoint_id IS NULL OR \"usage\".provider_endpoint_id = EXCLUDED.provider_endpoint_id) AND (\"usage\".provider_api_key_id IS NULL OR \"usage\".provider_api_key_id = EXCLUDED.provider_api_key_id)) THEN COALESCE(EXCLUDED.provider_id, \"usage\".provider_id) ELSE \"usage\".provider_id END",
        "provider_endpoint_id = CASE WHEN \"usage\".billing_status = 'pending' OR (\"usage\".provider_endpoint_id IS NULL AND (\"usage\".provider_id IS NULL OR \"usage\".provider_id = EXCLUDED.provider_id) AND (\"usage\".provider_api_key_id IS NULL OR \"usage\".provider_api_key_id = EXCLUDED.provider_api_key_id)) THEN COALESCE(EXCLUDED.provider_endpoint_id, \"usage\".provider_endpoint_id) ELSE \"usage\".provider_endpoint_id END",
        "provider_api_key_id = CASE WHEN \"usage\".billing_status = 'pending' OR (\"usage\".provider_api_key_id IS NULL AND (\"usage\".provider_id IS NULL OR \"usage\".provider_id = EXCLUDED.provider_id) AND (\"usage\".provider_endpoint_id IS NULL OR \"usage\".provider_endpoint_id = EXCLUDED.provider_endpoint_id)) THEN COALESCE(EXCLUDED.provider_api_key_id, \"usage\".provider_api_key_id) ELSE \"usage\".provider_api_key_id END",
    ] {
        assert!(
            super::UPSERT_SQL.contains(assignment),
            "missing provider link recovery assignment: {assignment}"
        );
    }
}

#[test]
fn usage_sql_updates_usage_mirror_columns_from_terminal_events_only() {
    for assignment in [
        "input_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".input_tokens, EXCLUDED.input_tokens) ELSE \"usage\".input_tokens END",
        "output_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".output_tokens, EXCLUDED.output_tokens) ELSE \"usage\".output_tokens END",
        "total_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".total_tokens, EXCLUDED.total_tokens) ELSE \"usage\".total_tokens END",
        "input_output_total_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".input_output_total_tokens, EXCLUDED.input_output_total_tokens) ELSE \"usage\".input_output_total_tokens END",
        "input_context_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".input_context_tokens, EXCLUDED.input_context_tokens) ELSE \"usage\".input_context_tokens END",
        "cache_creation_input_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".cache_creation_input_tokens, EXCLUDED.cache_creation_input_tokens) ELSE \"usage\".cache_creation_input_tokens END",
        "cache_creation_input_tokens_5m = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".cache_creation_input_tokens_5m, EXCLUDED.cache_creation_input_tokens_5m) ELSE \"usage\".cache_creation_input_tokens_5m END",
        "cache_creation_input_tokens_1h = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".cache_creation_input_tokens_1h, EXCLUDED.cache_creation_input_tokens_1h) ELSE \"usage\".cache_creation_input_tokens_1h END",
        "cache_read_input_tokens = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".cache_read_input_tokens, EXCLUDED.cache_read_input_tokens) ELSE \"usage\".cache_read_input_tokens END",
        "cache_creation_cost_usd = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".cache_creation_cost_usd, EXCLUDED.cache_creation_cost_usd) ELSE \"usage\".cache_creation_cost_usd END",
        "cache_read_cost_usd = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".cache_read_cost_usd, EXCLUDED.cache_read_cost_usd) ELSE \"usage\".cache_read_cost_usd END",
        "total_cost_usd = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".total_cost_usd, EXCLUDED.total_cost_usd) ELSE \"usage\".total_cost_usd END",
        "actual_total_cost_usd = CASE WHEN \"usage\".billing_status = 'pending' AND EXCLUDED.status IN ('completed', 'failed', 'cancelled') THEN GREATEST(\"usage\".actual_total_cost_usd, EXCLUDED.actual_total_cost_usd) ELSE \"usage\".actual_total_cost_usd END",
    ] {
        assert!(
            super::UPSERT_SQL.contains(assignment),
            "missing terminal mirror assignment: {assignment}"
        );
    }
}

#[test]
fn usage_sql_binds_usage_mirror_values_for_terminal_upserts() {
    let source = include_str!("mod.rs");
    for binding in [
        ".bind(usage.input_tokens.map(to_i32).transpose()?)",
        ".bind(usage.output_tokens.map(to_i32).transpose()?)",
        ".bind(usage.total_tokens.map(to_i32).transpose()?)",
        ".bind(usage.cache_creation_input_tokens.map(to_i32).transpose()?)",
        ".bind(usage.cache_read_input_tokens.map(to_i32).transpose()?)",
        ".bind(usage.cache_creation_cost_usd)",
        ".bind(usage.cache_read_cost_usd)",
        ".bind(usage.total_cost_usd)",
        ".bind(usage.actual_total_cost_usd)",
    ] {
        assert!(
            source.contains(binding),
            "missing usage upsert bind: {binding}"
        );
    }
}

#[test]
fn usage_sql_clears_legacy_output_price_column_on_upsert() {
    assert!(super::UPSERT_SQL.contains("output_price_per_1m = NULL"));
    assert!(include_str!("mod.rs").contains(".bind(None::<f64>)"));
}

#[test]
fn usage_sql_clears_legacy_header_columns_on_upsert() {
    assert!(super::UPSERT_SQL.contains("request_headers = NULL"));
    assert!(super::UPSERT_SQL.contains("provider_request_headers = NULL"));
    assert!(super::UPSERT_SQL.contains("response_headers = NULL"));
    assert!(super::UPSERT_SQL.contains("client_response_headers = NULL"));
}

#[test]
fn usage_sql_detached_body_flags_clear_inline_and_compressed_columns() {
    assert!(super::UPSERT_SQL
        .contains("WHEN EXCLUDED.request_body_compressed IS NOT NULL OR $56 THEN NULL"));
    assert!(super::UPSERT_SQL
        .contains("WHEN EXCLUDED.provider_request_body_compressed IS NOT NULL OR $57 THEN NULL"));
    assert!(super::UPSERT_SQL
        .contains("WHEN EXCLUDED.response_body_compressed IS NOT NULL OR $58 THEN NULL"));
    assert!(super::UPSERT_SQL
        .contains("WHEN EXCLUDED.client_response_body_compressed IS NOT NULL OR $59 THEN NULL"));
}

#[test]
fn usage_sql_clears_stale_failure_fields_for_non_failed_status_updates() {
    assert!(super::UPSERT_SQL.contains(
            "WHEN EXCLUDED.status IN ('pending', 'streaming', 'completed', 'cancelled') AND EXCLUDED.status_code IS NULL THEN NULL"
        ));
    assert!(super::UPSERT_SQL.contains(
            "WHEN EXCLUDED.status IN ('pending', 'streaming', 'completed', 'cancelled') THEN EXCLUDED.error_message"
        ));
    assert!(super::UPSERT_SQL.contains(
            "WHEN EXCLUDED.status IN ('pending', 'streaming', 'completed', 'cancelled') THEN EXCLUDED.error_category"
        ));
}

#[test]
fn stale_cleanup_failed_candidate_sql_orders_by_effective_timestamp() {
    let sql = super::SELECT_LATEST_FAILED_CANDIDATE_FOR_STALE_REQUESTS_SQL;
    assert!(sql.contains("COALESCE(finished_at, started_at, created_at) DESC"));
    assert!(!sql.contains("finished_at DESC NULLS LAST"));
    assert!(!sql.contains("started_at DESC NULLS LAST"));
}

#[test]
fn usage_sql_does_not_allow_streaming_to_regress_back_to_pending() {
    assert!(super::UPSERT_SQL.contains(
            "WHEN \"usage\".status = 'streaming' AND EXCLUDED.status = 'pending' THEN \"usage\".status_code"
        ));
    assert!(super::UPSERT_SQL.contains(
            "WHEN \"usage\".status = 'streaming' AND EXCLUDED.status = 'pending' THEN \"usage\".error_message"
        ));
    assert!(super::UPSERT_SQL.contains(
        "WHEN \"usage\".status = 'streaming' AND EXCLUDED.status = 'pending' THEN \"usage\".status"
    ));
}

#[test]
fn usage_sql_recovers_void_failures_before_upsert_and_settlement() {
    assert!(super::RESET_STALE_VOID_USAGE_SQL.contains("UPDATE \"usage\""));
    assert!(super::RESET_STALE_VOID_USAGE_SQL.contains("billing_status = 'pending'"));
    assert!(super::RESET_STALE_VOID_USAGE_SQL.contains("finalized_at = NULL"));
    assert!(super::RESET_STALE_VOID_USAGE_SQL.contains("status IN ('failed', 'cancelled')"));
    assert!(super::RESET_STALE_VOID_USAGE_SETTLEMENT_SNAPSHOT_SQL
        .contains("UPDATE usage_settlement_snapshots"));
    assert!(super::RESET_STALE_VOID_USAGE_SETTLEMENT_SNAPSHOT_SQL
        .contains("billing_status = 'pending'"));
    assert!(super::RESET_STALE_VOID_USAGE_SETTLEMENT_SNAPSHOT_SQL.contains("finalized_at = NULL"));
}

#[test]
fn prepare_usage_body_storage_detaches_small_payloads_into_blob_storage() {
    let payload = json!({"message": "hello"});
    let storage = prepare_usage_body_storage(Some(&payload)).expect("storage should serialize");

    assert!(storage.inline_json.is_none());
    let compressed = storage
        .detached_blob_bytes
        .as_deref()
        .expect("small payload should now be ref-backed");
    assert_eq!(
        inflate_usage_json_value(compressed).expect("payload should inflate"),
        payload
    );
}

#[test]
fn prepare_usage_body_storage_compresses_large_payloads() {
    let payload = json!({
        "content": "x".repeat(MAX_INLINE_USAGE_BODY_BYTES + 128)
    });
    let storage = prepare_usage_body_storage(Some(&payload)).expect("storage should serialize");

    assert!(storage.inline_json.is_none());
    let compressed = storage
        .detached_blob_bytes
        .as_deref()
        .expect("large payload should be compressed");
    assert_eq!(
        inflate_usage_json_value(compressed).expect("payload should inflate"),
        payload
    );
}

#[test]
fn prepare_request_metadata_for_body_storage_strips_body_ref_compatibility_keys() {
    let detached = prepare_usage_body_storage(Some(&json!({
        "content": "x".repeat(MAX_INLINE_USAGE_BODY_BYTES + 32)
    })))
    .expect("detached storage should build");
    let inline =
        prepare_usage_body_storage(Some(&json!({"message": "inline"}))).expect("inline body");

    let metadata = prepare_request_metadata_for_body_storage(
        Some(json!({
            "trace_id": "trace-1",
            "request_body_ref": "blob://old-request",
            "provider_request_body_ref": "blob://old-provider"
        })),
        [
            (
                UsageBodyField::RequestBody,
                &detached,
                Some(&json!({"request": true})),
                Some("usage://request/req-123/request_body"),
            ),
            (
                UsageBodyField::ProviderRequestBody,
                &inline,
                Some(&json!({"provider": true})),
                None,
            ),
        ],
    )
    .expect("metadata should be present");

    assert_eq!(
        metadata,
        json!({
            "trace_id": "trace-1"
        })
    );
}

#[test]
fn prepare_prompt_capture_metadata_moves_preview_fields_to_entries() {
    let (metadata, entries) = prepare_prompt_capture_metadata(Some(json!({
        "trace_id": "trace-1",
        "prompt_capture": {
            "version": 1,
            "item_count": 2,
            "role_counts": { "system": 1, "user": 1 },
            "items": [
                {
                    "source": "request",
                    "role": "system",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "index": 0,
                    "chars": 120,
                    "preview": "system prompt preview",
                    "truncated": true
                },
                {
                    "source": "provider_request",
                    "role": "user",
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "chars": 12,
                    "preview": "hello",
                    "truncated": false
                }
            ]
        }
    })));

    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0],
        UsagePromptCaptureEntry {
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            role: "system".to_string(),
            chars: 120,
            preview: "system prompt preview".to_string(),
            truncated: true,
        }
    );
    assert_eq!(
        metadata,
        Some(json!({
            "trace_id": "trace-1",
            "prompt_capture": {
                "version": 2,
                "item_count": 2,
                "role_counts": { "system": 1, "user": 1 },
                "items": [
                    {
                        "source": "request",
                        "role": "system",
                        "index": 0,
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    {
                        "source": "provider_request",
                        "role": "user",
                        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    }
                ]
            }
        }))
    );
}

#[test]
fn prepare_prompt_capture_metadata_handles_large_sanitized_prompt_capture() {
    let items = (0..32)
        .map(|index| {
            json!({
                "source": "request",
                "role": if index < 2 { "system" } else { "user" },
                "sha256": format!("{index:064x}"),
                "index": index,
                "chars": 20_000,
                "preview": "prompt preview ".repeat(700),
                "truncated": true
            })
        })
        .collect::<Vec<_>>();
    let (metadata, entries) = prepare_prompt_capture_metadata(Some(json!({
        "prompt_capture": {
            "version": 1,
            "item_count": 32,
            "role_counts": { "system": 2, "user": 30 },
            "items": items
        }
    })));

    assert_eq!(entries.len(), 32);
    let capture = metadata
        .as_ref()
        .and_then(|value| value.get("prompt_capture"))
        .and_then(Value::as_object)
        .expect("prompt capture metadata should remain");
    assert_eq!(capture.get("version"), Some(&json!(2)));
    assert_eq!(capture.get("item_count"), Some(&json!(32)));
    let refs = capture
        .get("items")
        .and_then(Value::as_array)
        .expect("hash refs should remain");
    assert_eq!(refs.len(), 32);
    assert!(refs.iter().all(|item| item.get("preview").is_none()));
}

#[test]
fn aggregate_prompt_capture_entries_matches_sequential_reference_at_supported_boundaries() {
    for seed in 0..8usize {
        for size in [0usize, 1, 32, 256] {
            let entries = (0..size)
                .map(|index| {
                    let hash_index = (index.wrapping_mul(17).wrapping_add(seed)) % 11;
                    let preview = match (index + seed) % 4 {
                        0 => "😀".repeat((index % 5) + 1),
                        1 => "x".repeat((index % 9) + 1),
                        2 => format!("equal-{}", index % 3),
                        _ => format!("preview-{seed}-{index}"),
                    };
                    UsagePromptCaptureEntry {
                        sha256: format!("{hash_index:064x}"),
                        role: match (index + seed) % 3 {
                            0 => "system",
                            1 => "user",
                            _ => "unknown",
                        }
                        .to_string(),
                        chars: i32::try_from((index * 13 + seed) % 97)
                            .expect("test chars should fit i32"),
                        preview,
                        truncated: (index + seed) % 7 == 0,
                    }
                })
                .collect::<Vec<_>>();

            let actual = aggregate_usage_prompt_capture_entries(&entries)
                .expect("valid prompt capture entries should aggregate")
                .into_iter()
                .map(|entry| (entry.entry, entry.seen_count))
                .collect::<Vec<_>>();
            let expected = sequential_prompt_capture_reference(&entries);

            assert_eq!(actual, expected, "seed={seed}, size={size}");
        }
    }
}

fn sequential_prompt_capture_reference(
    entries: &[UsagePromptCaptureEntry],
) -> Vec<(UsagePromptCaptureEntry, i64)> {
    let mut rows = BTreeMap::<String, (UsagePromptCaptureEntry, i64)>::new();
    for entry in entries {
        match rows.get_mut(&entry.sha256) {
            Some((stored, seen_count)) => {
                if !entry.role.is_empty() {
                    stored.role.clone_from(&entry.role);
                }
                stored.chars = stored.chars.max(entry.chars);
                if stored.preview.chars().count() < entry.preview.chars().count() {
                    stored.preview.clone_from(&entry.preview);
                }
                stored.truncated |= entry.truncated;
                *seen_count += 1;
            }
            None => {
                rows.insert(entry.sha256.clone(), (entry.clone(), 1));
            }
        }
    }
    rows.into_values().collect()
}

#[test]
fn aggregate_prompt_capture_entries_preserves_sequential_upsert_semantics() {
    let sha_a = "a".repeat(64);
    let sha_b = "b".repeat(64);
    let entries = vec![
        UsagePromptCaptureEntry {
            sha256: sha_b.clone(),
            role: "system".to_string(),
            chars: 10,
            preview: "😀😀😀".to_string(),
            truncated: false,
        },
        UsagePromptCaptureEntry {
            sha256: sha_a.clone(),
            role: "user".to_string(),
            chars: 5,
            preview: "first".to_string(),
            truncated: false,
        },
        UsagePromptCaptureEntry {
            sha256: sha_b.clone(),
            role: "unknown".to_string(),
            chars: 20,
            preview: "abcd".to_string(),
            truncated: true,
        },
        UsagePromptCaptureEntry {
            sha256: sha_b.clone(),
            role: String::new(),
            chars: 15,
            preview: "wxyz".to_string(),
            truncated: false,
        },
    ];

    let aggregated = aggregate_usage_prompt_capture_entries(&entries)
        .expect("valid prompt capture entries should aggregate");

    assert_eq!(aggregated.len(), 2);
    assert_eq!(aggregated[0].entry.sha256, sha_a);
    assert_eq!(aggregated[1].entry.sha256, sha_b);
    assert_eq!(aggregated[1].entry.role, "unknown");
    assert_eq!(aggregated[1].entry.chars, 20);
    assert_eq!(aggregated[1].entry.preview, "abcd");
    assert!(aggregated[1].entry.truncated);
    assert_eq!(aggregated[1].seen_count, 3);
}

#[test]
fn prepare_prompt_capture_metadata_keeps_duplicate_refs_when_entries_are_batched() {
    let sha = "c".repeat(64);
    let (metadata, entries) = prepare_prompt_capture_metadata(Some(json!({
        "prompt_capture": {
            "items": [
                {
                    "source": "request",
                    "role": "system",
                    "sha256": sha,
                    "chars": 8,
                    "preview": "original",
                    "truncated": false
                },
                {
                    "source": "provider_request",
                    "role": "user",
                    "sha256": sha,
                    "chars": 12,
                    "preview": "transformed",
                    "truncated": true
                }
            ]
        }
    })));

    let refs = metadata
        .as_ref()
        .and_then(|value| value.pointer("/prompt_capture/items"))
        .and_then(Value::as_array)
        .expect("prompt capture refs should remain");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].get("source"), Some(&json!("request")));
    assert_eq!(refs[1].get("source"), Some(&json!("provider_request")));

    let aggregated = aggregate_usage_prompt_capture_entries(&entries)
        .expect("valid prompt capture entries should aggregate");
    assert_eq!(aggregated.len(), 1);
    assert_eq!(aggregated[0].seen_count, 2);
}

#[test]
fn aggregate_prompt_capture_entries_rejects_invalid_values_before_deduplication() {
    let sha = "d".repeat(64);
    let valid = UsagePromptCaptureEntry {
        sha256: sha.clone(),
        role: "user".to_string(),
        chars: 12,
        preview: "longer valid preview".to_string(),
        truncated: false,
    };
    let invalid_role = UsagePromptCaptureEntry {
        sha256: sha.clone(),
        role: "r".repeat(33),
        chars: 1,
        preview: "short".to_string(),
        truncated: false,
    };
    assert!(matches!(
        aggregate_usage_prompt_capture_entries(&[invalid_role, valid.clone()]),
        Err(crate::DataLayerError::InvalidInput(_))
    ));

    let invalid_preview = UsagePromptCaptureEntry {
        sha256: sha,
        role: "system".to_string(),
        chars: 1,
        preview: "bad\0preview".to_string(),
        truncated: false,
    };
    assert!(matches!(
        aggregate_usage_prompt_capture_entries(&[invalid_preview, valid]),
        Err(crate::DataLayerError::InvalidInput(_))
    ));
}

#[test]
fn prompt_capture_batch_upsert_uses_fixed_shape_arrays() {
    assert!(UPSERT_USAGE_PROMPT_CAPTURE_ENTRIES_SQL.contains("FROM UNNEST("));
    assert!(UPSERT_USAGE_PROMPT_CAPTURE_ENTRIES_SQL.contains("$6::BIGINT[]"));
    assert!(UPSERT_USAGE_PROMPT_CAPTURE_ENTRIES_SQL.contains("ORDER BY incoming.sha256"));
    assert!(UPSERT_USAGE_PROMPT_CAPTURE_ENTRIES_SQL.contains("ON CONFLICT (sha256) DO NOTHING"));
    assert!(UPSERT_USAGE_PROMPT_CAPTURE_ENTRIES_SQL.contains("stored.chars < incoming.chars"));
    assert!(FLUSH_USAGE_PROMPT_CAPTURE_OBSERVATIONS_SQL
        .contains("seen_count = stored.seen_count + incoming.seen_count"));
}

#[test]
fn prompt_capture_observation_buffer_merges_counts_and_latest_timestamp() {
    let first_at = Utc.timestamp_opt(1_700_000_000, 0).single().expect("time");
    let second_at = Utc.timestamp_opt(1_700_000_005, 0).single().expect("time");
    let mut buffer = UsagePromptCaptureObservationBuffer::default();
    buffer.extend(BTreeMap::from([(
        "a".repeat(64),
        UsagePromptCaptureObservation {
            seen_count: 2,
            last_seen_at: first_at,
        },
    )]));
    buffer.extend(BTreeMap::from([(
        "a".repeat(64),
        UsagePromptCaptureObservation {
            seen_count: 3,
            last_seen_at: second_at,
        },
    )]));

    assert_eq!(
        buffer.take_all(),
        BTreeMap::from([(
            "a".repeat(64),
            UsagePromptCaptureObservation {
                seen_count: 5,
                last_seen_at: second_at,
            },
        )])
    );
}

#[test]
fn prompt_capture_observation_buffer_restores_failed_drain_without_losing_new_counts() {
    let observed_at = Utc.timestamp_opt(1_700_000_000, 0).single().expect("time");
    let sha256 = "b".repeat(64);
    let mut buffer = UsagePromptCaptureObservationBuffer::default();
    buffer.extend(BTreeMap::from([(
        sha256.clone(),
        UsagePromptCaptureObservation {
            seen_count: 2,
            last_seen_at: observed_at,
        },
    )]));
    let failed_drain = buffer.take_all();
    buffer.extend(BTreeMap::from([(
        sha256.clone(),
        UsagePromptCaptureObservation {
            seen_count: 1,
            last_seen_at: observed_at,
        },
    )]));
    buffer.extend(failed_drain);

    assert_eq!(
        buffer
            .take_all()
            .get(&sha256)
            .map(|observation| observation.seen_count),
        Some(3)
    );
}

#[test]
fn empty_prompt_capture_flush_does_not_postpone_the_next_observation() {
    let observed_at = Utc.timestamp_opt(1_700_000_000, 0).single().expect("time");
    let mut buffer = UsagePromptCaptureObservationBuffer::default();
    let due_at = buffer.last_flush + super::PROMPT_CAPTURE_OBSERVATION_FLUSH_INTERVAL;

    assert!(buffer.take_if_due(due_at).is_empty());
    buffer.extend(BTreeMap::from([(
        "c".repeat(64),
        UsagePromptCaptureObservation {
            seen_count: 1,
            last_seen_at: observed_at,
        },
    )]));

    assert_eq!(buffer.take_if_due(due_at).len(), 1);
}

#[tokio::test]
async fn prompt_capture_batch_upsert_executes_when_postgres_url_is_set() {
    let Some(database_url) = std::env::var("AETHER_TEST_POSTGRES_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!(
            "skipping prompt capture batch postgres test because AETHER_TEST_POSTGRES_URL is unset"
        );
        return;
    };

    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres test pool should connect");
    crate::lifecycle::migrate::run_migrations(&pool)
        .await
        .expect("postgres migrations should run");

    for size in [0usize, 1, 32, 256] {
        let prefix = uuid::Uuid::new_v4().simple().to_string();
        let entries = (0..size)
            .map(|index| UsagePromptCaptureEntry {
                sha256: format!("{prefix}{index:032x}"),
                role: "user".to_string(),
                chars: i32::try_from(index).expect("test index should fit i32"),
                preview: format!("preview-{index}"),
                truncated: index % 2 == 0,
            })
            .collect::<Vec<_>>();
        let hashes = entries
            .iter()
            .map(|entry| entry.sha256.clone())
            .collect::<Vec<_>>();
        let mut tx = pool
            .begin()
            .await
            .expect("postgres transaction should begin");
        sync_usage_prompt_capture_entries(&mut tx, &entries)
            .await
            .expect("prompt capture batch should execute");
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM usage_prompt_capture_entries WHERE sha256 = ANY($1::TEXT[])",
        )
        .bind(&hashes)
        .fetch_one(&mut *tx)
        .await
        .expect("prompt capture batch should be visible in transaction");
        assert_eq!(
            count,
            i64::try_from(size).expect("test size should fit i64")
        );
        tx.rollback()
            .await
            .expect("prompt capture boundary transaction should roll back");
        let count_after_rollback: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM usage_prompt_capture_entries WHERE sha256 = ANY($1::TEXT[])",
        )
        .bind(&hashes)
        .fetch_one(&pool)
        .await
        .expect("prompt capture rows should be queried after rollback");
        assert_eq!(count_after_rollback, 0);
    }

    let sha = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let initial = UsagePromptCaptureEntry {
        sha256: sha.clone(),
        role: "system".to_string(),
        chars: 3,
        preview: "😀😀😀".to_string(),
        truncated: false,
    };
    let mut conn = pool
        .acquire()
        .await
        .expect("postgres connection should acquire");
    sync_usage_prompt_capture_entries(&mut conn, &[initial])
        .await
        .expect("initial prompt capture should insert");
    let ctid_before_repeat: String =
        sqlx::query_scalar("SELECT ctid::TEXT FROM usage_prompt_capture_entries WHERE sha256 = $1")
            .bind(&sha)
            .fetch_one(&mut *conn)
            .await
            .expect("initial prompt capture tuple should read");
    let repeated_observations = sync_usage_prompt_capture_entries(
        &mut conn,
        &[UsagePromptCaptureEntry {
            sha256: sha.clone(),
            role: "system".to_string(),
            chars: 3,
            preview: "😀😀😀".to_string(),
            truncated: false,
        }],
    )
    .await
    .expect("unchanged prompt capture should be observed");
    let ctid_after_repeat: String =
        sqlx::query_scalar("SELECT ctid::TEXT FROM usage_prompt_capture_entries WHERE sha256 = $1")
            .bind(&sha)
            .fetch_one(&mut *conn)
            .await
            .expect("repeated prompt capture tuple should read");
    assert_eq!(ctid_after_repeat, ctid_before_repeat);
    super::flush_usage_prompt_capture_observations(&mut conn, &repeated_observations)
        .await
        .expect("repeated prompt capture observation should flush");
    let observations = sync_usage_prompt_capture_entries(
        &mut conn,
        &[
            UsagePromptCaptureEntry {
                sha256: sha.clone(),
                role: "user".to_string(),
                chars: 20,
                preview: "abcd".to_string(),
                truncated: true,
            },
            UsagePromptCaptureEntry {
                sha256: sha.clone(),
                role: "unknown".to_string(),
                chars: 15,
                preview: "wxyz".to_string(),
                truncated: false,
            },
        ],
    )
    .await
    .expect("existing prompt capture should update");
    super::flush_usage_prompt_capture_observations(&mut conn, &observations)
        .await
        .expect("prompt capture observations should flush");
    let row: (String, i32, String, bool, i64) = sqlx::query_as(
        "SELECT role, chars, preview, truncated, seen_count FROM usage_prompt_capture_entries WHERE sha256 = $1",
    )
    .bind(&sha)
    .fetch_one(&mut *conn)
    .await
    .expect("updated prompt capture should read");
    assert_eq!(row, ("system".to_string(), 20, "abcd".to_string(), true, 4));

    let sha_a = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let sha_b = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let left_entries = vec![
        UsagePromptCaptureEntry {
            sha256: sha_b.clone(),
            role: "user".to_string(),
            chars: 1,
            preview: "left-b".to_string(),
            truncated: false,
        },
        UsagePromptCaptureEntry {
            sha256: sha_a.clone(),
            role: "user".to_string(),
            chars: 1,
            preview: "left-a".to_string(),
            truncated: false,
        },
    ];
    let right_entries = vec![
        UsagePromptCaptureEntry {
            sha256: sha_a.clone(),
            role: "user".to_string(),
            chars: 2,
            preview: "right-a".to_string(),
            truncated: false,
        },
        UsagePromptCaptureEntry {
            sha256: sha_b.clone(),
            role: "user".to_string(),
            chars: 2,
            preview: "right-b".to_string(),
            truncated: false,
        },
    ];
    let left_pool = pool.clone();
    let right_pool = pool.clone();
    let (left_observations, right_observations) =
        tokio::time::timeout(Duration::from_secs(30), async move {
            tokio::join!(
                async move {
                    let mut conn = left_pool
                        .acquire()
                        .await
                        .expect("left postgres connection should acquire");
                    sync_usage_prompt_capture_entries(&mut conn, &left_entries)
                        .await
                        .expect("left prompt capture batch should execute")
                },
                async move {
                    let mut conn = right_pool
                        .acquire()
                        .await
                        .expect("right postgres connection should acquire");
                    sync_usage_prompt_capture_entries(&mut conn, &right_entries)
                        .await
                        .expect("right prompt capture batch should execute")
                }
            )
        })
        .await
        .expect("overlapping prompt capture batches should not deadlock");
    let mut observation_buffer = UsagePromptCaptureObservationBuffer::default();
    observation_buffer.extend(left_observations);
    observation_buffer.extend(right_observations);
    super::flush_usage_prompt_capture_observations(&mut conn, &observation_buffer.take_all())
        .await
        .expect("overlapping prompt capture observations should flush");
    let seen_counts = sqlx::query_as::<_, (String, i64)>(
        "SELECT sha256, seen_count FROM usage_prompt_capture_entries WHERE sha256 = ANY($1::TEXT[]) ORDER BY sha256",
    )
    .bind(vec![sha_a.clone(), sha_b.clone()])
    .fetch_all(&pool)
    .await
    .expect("overlapping prompt capture rows should read");
    let mut expected_seen_counts = vec![(sha_a.clone(), 2), (sha_b.clone(), 2)];
    expected_seen_counts.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(seen_counts, expected_seen_counts);

    sqlx::query("DELETE FROM usage_prompt_capture_entries WHERE sha256 = ANY($1::TEXT[])")
        .bind(vec![sha, sha_a, sha_b])
        .execute(&pool)
        .await
        .expect("prompt capture test rows should clean up");
}

#[test]
fn ws_prompt_capture_queries_use_connection_fast_path_then_session_fallback() {
    assert!(FIND_WS_CONNECTION_PROMPT_CAPTURE_SQL.contains("source_routing.candidate_id = $1"));
    assert!(FIND_WS_CONNECTION_PROMPT_CAPTURE_SQL.contains("current.request_id = $2"));
    assert!(FIND_WS_CONNECTION_PROMPT_CAPTURE_SQL.contains("current.api_key_id"));
    assert!(
        FIND_WS_CONNECTION_PROMPT_CAPTURE_SQL.contains("source.created_at <= current.created_at")
    );
    assert!(!FIND_WS_CONNECTION_PROMPT_CAPTURE_SQL.contains("source_candidate"));

    assert!(FIND_WS_PROMPT_CAPTURE_SCOPE_SQL.contains("current_candidate.id = $1"));
    assert!(FIND_WS_PROMPT_CAPTURE_SCOPE_SQL.contains("current.request_id = $2"));
    assert!(FIND_WS_PROMPT_CAPTURE_SCOPE_SQL.contains("usage_session_key"));
    assert!(FIND_WS_PROMPT_CAPTURE_SCOPE_SQL.contains("client_family"));

    assert!(!FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("WITH current_scope"));
    assert!(
        FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("FROM request_candidates AS source_candidate")
    );
    assert!(FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("source_candidate.extra_data"));
    assert!(FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("source_candidate.user_id"));
    assert!(FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("source.api_key_id = $4"));
    assert!(FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("source.created_at <= $5"));
    assert!(FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("client_family"));
    assert!(!FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("source_routing.candidate_id = $1"));
    assert!(
        FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("source.request_metadata->>'ws_step' = 'true'")
    );
    assert!(FIND_WS_SESSION_PROMPT_CAPTURE_SQL.contains("ORDER BY source.created_at DESC"));

    assert!(FIND_WS_USAGE_SESSION_PROMPT_CAPTURE_SQL
        .contains("request_metadata#>>'{client_session_affinity,session_key}'"));
    assert!(FIND_WS_USAGE_SESSION_PROMPT_CAPTURE_SQL.contains("source.user_id"));
    assert!(FIND_WS_USAGE_SESSION_PROMPT_CAPTURE_SQL.contains("client_family"));
    assert!(!FIND_WS_USAGE_SESSION_PROMPT_CAPTURE_SQL.contains("request_candidates"));
}

#[test]
fn ws_prompt_capture_scope_requires_an_identity_for_cross_candidate_lookup() {
    let identified = WsPromptCaptureScope {
        user_id: Some("user-1".to_string()),
        api_key_id: None,
        created_at: Utc::now(),
        session_key: "session=session-1".to_string(),
        usage_session_key: None,
        client_family: Some("codex".to_string()),
    };
    assert!(identified.has_identity());

    let api_key_identified = WsPromptCaptureScope {
        user_id: None,
        api_key_id: Some("key-1".to_string()),
        ..identified.clone()
    };
    assert!(api_key_identified.has_identity());

    let anonymous = WsPromptCaptureScope {
        user_id: None,
        api_key_id: None,
        ..identified
    };
    assert!(!anonymous.has_identity());
}

#[test]
fn attaches_candidate_session_affinity_when_terminal_usage_metadata_lacks_it() {
    let metadata = attach_candidate_client_session_affinity_metadata(
        Some(json!({
            "ws_step": true,
            "trace_id": "trace-1"
        })),
        Some(&json!({
            "client_family": "codex",
            "session_key": "session=session-1"
        })),
    )
    .expect("metadata should remain");

    assert_eq!(metadata["trace_id"], json!("trace-1"));
    assert_eq!(
        metadata["client_session_affinity"],
        json!({
            "client_family": "codex",
            "session_key": "session=session-1"
        })
    );
}

#[test]
fn candidate_session_affinity_only_fills_missing_metadata_fields() {
    let metadata = attach_candidate_client_session_affinity_metadata(
        Some(json!({
            "client_session_affinity": {
                "session_key": "session=usage-session"
            }
        })),
        Some(&json!({
            "client_family": "codex",
            "session_key": "session=candidate-session"
        })),
    )
    .expect("metadata should remain");

    assert_eq!(
        metadata["client_session_affinity"]["session_key"],
        json!("session=usage-session")
    );
    assert_eq!(
        metadata["client_session_affinity"]["client_family"],
        json!("codex")
    );
}

#[test]
fn attaches_prior_prompt_capture_to_ws_session_step_metadata() {
    let metadata = attach_ws_session_prompt_capture_metadata(
        Some(json!({
            "ws_step": true,
            "trace_id": "ws-step-2"
        })),
        json!({
            "version": 2,
            "item_count": 1,
            "role_counts": { "user": 1 },
            "items": [{
                "source": "request.input[0].content[0].text",
                "role": "user",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }),
        "ws-step-1",
    )
    .expect("metadata should remain");

    assert!(usage_metadata_is_ws_step(Some(&metadata)));
    assert!(usage_metadata_has_prompt_capture(Some(&metadata)));
    assert_eq!(metadata["trace_id"], json!("ws-step-2"));
    assert_eq!(metadata["prompt_capture"]["scope"], json!("ws_session"));
    assert_eq!(metadata["prompt_capture"]["inherited"], json!(true));
    assert_eq!(
        metadata["prompt_capture"]["source_request_id"],
        json!("ws-step-1")
    );
    assert_eq!(
        metadata["prompt_capture"]["items"][0]["role"],
        json!("user")
    );
}

#[test]
fn ignores_empty_ws_session_prompt_capture_metadata() {
    let original = Some(json!({ "ws_step": true }));
    let metadata = attach_ws_session_prompt_capture_metadata(
        original.clone(),
        json!({ "items": [] }),
        "ws-step-1",
    );

    assert_eq!(metadata, original);
    assert!(usage_metadata_is_ws_step(metadata.as_ref()));
    assert!(!usage_metadata_has_prompt_capture(metadata.as_ref()));
}

#[test]
fn attach_compressed_body_refs_adds_missing_ref_metadata() {
    let metadata = attach_compressed_body_refs(
        "req-123",
        Some(json!({
            "candidate_id": "cand-1",
            "provider_request_body_ref": "blob://existing"
        })),
        true,
        true,
        true,
        false,
    )
    .expect("metadata should remain");

    assert_eq!(
        metadata,
        json!({
            "candidate_id": "cand-1",
            "request_body_ref": usage_body_ref("req-123", UsageBodyField::RequestBody),
            "provider_request_body_ref": "blob://existing",
            "response_body_ref": usage_body_ref("req-123", UsageBodyField::ResponseBody)
        })
    );
}

#[test]
fn usage_http_audit_body_refs_extracts_only_non_empty_values() {
    let refs = usage_http_audit_body_refs(Some(&json!({
        "request_body_ref": "usage://request/req-123/request_body",
        "provider_request_body_ref": "  ",
        "response_body_ref": "usage://request/req-123/response_body"
    })));

    assert_eq!(
        refs,
        UsageHttpAuditRefs {
            request_body_ref: Some("usage://request/req-123/request_body".to_string()),
            provider_request_body_ref: None,
            response_body_ref: Some("usage://request/req-123/response_body".to_string()),
            client_response_body_ref: None,
        }
    );
}

#[test]
fn resolved_read_usage_body_ref_prefers_typed_then_http_audit_then_compressed_then_metadata() {
    let metadata = json!({
        "request_body_ref": "usage://request/req-123/request_body"
    });
    let invalid_metadata = json!({
        "request_body_ref": "blob://metadata-request"
    });
    let mismatched_metadata = json!({
        "request_body_ref": "usage://request/req-other/request_body"
    });

    assert_eq!(
        resolved_read_usage_body_ref(
            Some("usage://request/req-123/request_body"),
            metadata.as_object(),
            "req-123",
            UsageBodyField::RequestBody,
            true,
            Some("usage://request/req-123/request_body"),
        ),
        Some("usage://request/req-123/request_body".to_string())
    );
    assert_eq!(
        resolved_read_usage_body_ref(
            None,
            metadata.as_object(),
            "req-123",
            UsageBodyField::RequestBody,
            false,
            Some("usage://request/req-123/request_body"),
        ),
        Some("usage://request/req-123/request_body".to_string())
    );
    assert_eq!(
        resolved_read_usage_body_ref(
            None,
            metadata.as_object(),
            "req-123",
            UsageBodyField::RequestBody,
            true,
            None,
        ),
        Some(usage_body_ref("req-123", UsageBodyField::RequestBody))
    );
    assert_eq!(
        resolved_read_usage_body_ref(
            None,
            invalid_metadata.as_object(),
            "req-123",
            UsageBodyField::RequestBody,
            false,
            None,
        ),
        None
    );
    assert_eq!(
        resolved_read_usage_body_ref(
            None,
            mismatched_metadata.as_object(),
            "req-123",
            UsageBodyField::RequestBody,
            false,
            None,
        ),
        None
    );
    assert_eq!(
        resolved_read_usage_body_ref(
            None,
            None,
            "req-123",
            UsageBodyField::ResponseBody,
            true,
            Some("usage://request/req-123/response_body"),
        ),
        Some(usage_body_ref("req-123", UsageBodyField::ResponseBody))
    );
    assert_eq!(
        resolved_read_usage_body_ref(
            None,
            None,
            "req-123",
            UsageBodyField::ClientResponseBody,
            false,
            Some("usage://request/req-123/client_response_body"),
        ),
        Some("usage://request/req-123/client_response_body".to_string())
    );
}

#[test]
fn resolved_write_usage_body_ref_ignores_metadata_compatibility_keys() {
    assert_eq!(
        resolved_write_usage_body_ref(None, "req-123", UsageBodyField::RequestBody, false, None,),
        None
    );
    assert_eq!(
        resolved_write_usage_body_ref(
            Some("usage://request/req-123/request_body"),
            "req-123",
            UsageBodyField::RequestBody,
            true,
            Some("usage://request/req-123/request_body"),
        ),
        Some("usage://request/req-123/request_body".to_string())
    );
    assert_eq!(
        resolved_write_usage_body_ref(
            None,
            "req-123",
            UsageBodyField::ResponseBody,
            true,
            Some("usage://request/req-123/response_body"),
        ),
        Some(usage_body_ref("req-123", UsageBodyField::ResponseBody))
    );
    assert_eq!(
        resolved_write_usage_body_ref(
            None,
            "req-123",
            UsageBodyField::ClientResponseBody,
            false,
            Some("usage://request/req-123/client_response_body"),
        ),
        Some("usage://request/req-123/client_response_body".to_string())
    );
}

#[test]
fn usage_http_audit_capture_mode_prefers_refs_over_inline_legacy() {
    let refs = UsageHttpAuditRefs {
        request_body_ref: Some("usage://request/req-123/request_body".to_string()),
        ..UsageHttpAuditRefs::default()
    };
    assert_eq!(
        usage_http_audit_capture_mode(&refs, [Some(&json!({"request": true})), None, None, None]),
        "ref_backed"
    );
    assert_eq!(
        usage_http_audit_capture_mode(
            &UsageHttpAuditRefs::default(),
            [Some(&json!({"request": true})), None, None, None]
        ),
        "inline_legacy"
    );
    assert_eq!(
        usage_http_audit_capture_mode(&UsageHttpAuditRefs::default(), [None, None, None, None]),
        "none"
    );
}

#[test]
fn attach_usage_http_audit_body_refs_adds_missing_metadata_without_overwriting_existing_keys() {
    let metadata = attach_usage_http_audit_body_refs(
        Some(json!({
            "candidate_id": "cand-1",
            "request_body_ref": "blob://existing"
        })),
        &UsageHttpAuditRefs {
            request_body_ref: Some("usage://request/req-123/request_body".to_string()),
            provider_request_body_ref: Some(
                "usage://request/req-123/provider_request_body".to_string(),
            ),
            response_body_ref: None,
            client_response_body_ref: Some(
                "usage://request/req-123/client_response_body".to_string(),
            ),
        },
    )
    .expect("metadata should remain");

    assert_eq!(
        metadata,
        json!({
            "candidate_id": "cand-1",
            "request_body_ref": "blob://existing",
            "provider_request_body_ref": "usage://request/req-123/provider_request_body",
            "client_response_body_ref": "usage://request/req-123/client_response_body"
        })
    );
}

#[test]
fn usage_routing_snapshot_from_usage_only_activates_for_routing_metadata() {
    let snapshot = usage_routing_snapshot_from_usage(
        &UpsertUsageRecord {
            request_id: "req-123".to_string(),
            user_id: None,
            api_key_id: None,
            username: None,
            api_key_name: None,
            provider_name: "openai".to_string(),
            model: "gpt-5".to_string(),
            target_model: None,
            provider_id: Some("provider-1".to_string()),
            provider_endpoint_id: Some("endpoint-1".to_string()),
            provider_api_key_id: Some("provider-key-1".to_string()),
            request_type: Some("chat".to_string()),
            api_format: Some("openai:chat".to_string()),
            api_family: Some("openai".to_string()),
            endpoint_kind: Some("chat".to_string()),
            endpoint_api_format: Some("openai:chat".to_string()),
            provider_api_family: Some("openai".to_string()),
            provider_endpoint_kind: Some("chat".to_string()),
            has_format_conversion: Some(false),
            is_stream: Some(false),
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
            cache_creation_input_tokens: None,
            cache_creation_ephemeral_5m_input_tokens: None,
            cache_creation_ephemeral_1h_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_cost_usd: None,
            cache_read_cost_usd: None,
            output_price_per_1m: None,
            total_cost_usd: None,
            actual_total_cost_usd: None,
            status_code: Some(200),
            error_message: None,
            error_category: None,
            response_time_ms: Some(100),
            first_byte_time_ms: None,
            status: "completed".to_string(),
            billing_status: "pending".to_string(),
            request_headers: None,
            request_body: None,
            request_body_ref: None,
            provider_request_headers: None,
            provider_request_body: None,
            provider_request_body_ref: None,
            response_headers: None,
            response_body: None,
            response_body_ref: None,
            client_response_headers: None,
            client_response_body: None,
            client_response_body_ref: None,
            request_body_state: None,
            provider_request_body_state: None,
            response_body_state: None,
            client_response_body_state: None,
            candidate_id: None,
            candidate_index: None,
            key_name: None,
            planner_kind: None,
            route_family: None,
            route_kind: None,
            execution_path: None,
            local_execution_runtime_miss_reason: None,
            request_metadata: None,
            finalized_at_unix_secs: None,
            created_at_unix_ms: Some(100),
            updated_at_unix_secs: 100,
        },
        Some(&json!({
            "candidate_id": "cand-1",
            "key_name": "primary",
            "planner_kind": "claude_cli_sync",
            "route_family": "claude",
            "route_kind": "cli",
            "execution_path": "local_execution_runtime_miss",
            "local_execution_runtime_miss_reason": "all_candidates_skipped"
        })),
    );

    assert_eq!(
        snapshot,
        UsageRoutingSnapshot {
            candidate_id: Some("cand-1".to_string()),
            candidate_index: None,
            key_name: Some("primary".to_string()),
            planner_kind: Some("claude_cli_sync".to_string()),
            route_family: Some("claude".to_string()),
            route_kind: Some("cli".to_string()),
            execution_path: Some("local_execution_runtime_miss".to_string()),
            local_execution_runtime_miss_reason: Some("all_candidates_skipped".to_string()),
            selected_provider_id: Some("provider-1".to_string()),
            selected_endpoint_id: Some("endpoint-1".to_string()),
            selected_provider_api_key_id: Some("provider-key-1".to_string()),
            has_format_conversion: Some(false),
        }
    );

    let empty_snapshot = usage_routing_snapshot_from_usage(
        &UpsertUsageRecord {
            request_id: "req-124".to_string(),
            user_id: None,
            api_key_id: None,
            username: None,
            api_key_name: None,
            provider_name: "openai".to_string(),
            model: "gpt-5".to_string(),
            target_model: None,
            provider_id: Some("provider-2".to_string()),
            provider_endpoint_id: Some("endpoint-2".to_string()),
            provider_api_key_id: Some("provider-key-2".to_string()),
            request_type: Some("chat".to_string()),
            api_format: Some("openai:chat".to_string()),
            api_family: Some("openai".to_string()),
            endpoint_kind: Some("chat".to_string()),
            endpoint_api_format: Some("openai:chat".to_string()),
            provider_api_family: Some("openai".to_string()),
            provider_endpoint_kind: Some("chat".to_string()),
            has_format_conversion: Some(true),
            is_stream: Some(false),
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
            cache_creation_input_tokens: None,
            cache_creation_ephemeral_5m_input_tokens: None,
            cache_creation_ephemeral_1h_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_cost_usd: None,
            cache_read_cost_usd: None,
            output_price_per_1m: None,
            total_cost_usd: None,
            actual_total_cost_usd: None,
            status_code: Some(200),
            error_message: None,
            error_category: None,
            response_time_ms: Some(100),
            first_byte_time_ms: None,
            status: "completed".to_string(),
            billing_status: "pending".to_string(),
            request_headers: None,
            request_body: None,
            request_body_ref: None,
            provider_request_headers: None,
            provider_request_body: None,
            provider_request_body_ref: None,
            response_headers: None,
            response_body: None,
            response_body_ref: None,
            client_response_headers: None,
            client_response_body: None,
            client_response_body_ref: None,
            request_body_state: None,
            provider_request_body_state: None,
            response_body_state: None,
            client_response_body_state: None,
            candidate_id: None,
            candidate_index: None,
            key_name: None,
            planner_kind: None,
            route_family: None,
            route_kind: None,
            execution_path: None,
            local_execution_runtime_miss_reason: None,
            request_metadata: None,
            finalized_at_unix_secs: None,
            created_at_unix_ms: Some(100),
            updated_at_unix_secs: 100,
        },
        Some(&json!({"trace_id": "trace-1"})),
    );

    assert_eq!(empty_snapshot, UsageRoutingSnapshot::default());
}

#[test]
fn usage_routing_snapshot_from_usage_prefers_typed_routing_fields_without_metadata() {
    let snapshot = usage_routing_snapshot_from_usage(
        &UpsertUsageRecord {
            request_id: "req-typed-routing-1".to_string(),
            user_id: None,
            api_key_id: None,
            username: None,
            api_key_name: None,
            provider_name: "openai".to_string(),
            model: "gpt-5".to_string(),
            target_model: None,
            provider_id: Some("provider-1".to_string()),
            provider_endpoint_id: Some("endpoint-1".to_string()),
            provider_api_key_id: Some("provider-key-1".to_string()),
            request_type: Some("chat".to_string()),
            api_format: Some("openai:chat".to_string()),
            api_family: Some("openai".to_string()),
            endpoint_kind: Some("chat".to_string()),
            endpoint_api_format: Some("openai:chat".to_string()),
            provider_api_family: Some("openai".to_string()),
            provider_endpoint_kind: Some("chat".to_string()),
            has_format_conversion: Some(true),
            is_stream: Some(false),
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
            cache_creation_input_tokens: None,
            cache_creation_ephemeral_5m_input_tokens: None,
            cache_creation_ephemeral_1h_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_cost_usd: None,
            cache_read_cost_usd: None,
            output_price_per_1m: None,
            total_cost_usd: None,
            actual_total_cost_usd: None,
            status_code: Some(200),
            error_message: None,
            error_category: None,
            response_time_ms: Some(100),
            first_byte_time_ms: None,
            status: "completed".to_string(),
            billing_status: "pending".to_string(),
            request_headers: None,
            request_body: None,
            request_body_ref: None,
            provider_request_headers: None,
            provider_request_body: None,
            provider_request_body_ref: None,
            response_headers: None,
            response_body: None,
            response_body_ref: None,
            client_response_headers: None,
            client_response_body: None,
            client_response_body_ref: None,
            request_body_state: None,
            provider_request_body_state: None,
            response_body_state: None,
            client_response_body_state: None,
            candidate_id: Some("cand-typed".to_string()),
            candidate_index: Some(2),
            key_name: Some("primary".to_string()),
            planner_kind: Some("claude_cli_sync".to_string()),
            route_family: Some("claude".to_string()),
            route_kind: Some("cli".to_string()),
            execution_path: Some("local_execution_runtime_miss".to_string()),
            local_execution_runtime_miss_reason: Some("all_candidates_skipped".to_string()),
            request_metadata: Some(json!({
                "trace_id": "trace-1"
            })),
            finalized_at_unix_secs: None,
            created_at_unix_ms: Some(100),
            updated_at_unix_secs: 100,
        },
        None,
    );

    assert_eq!(
        snapshot,
        UsageRoutingSnapshot {
            candidate_id: Some("cand-typed".to_string()),
            candidate_index: Some(2),
            key_name: Some("primary".to_string()),
            planner_kind: Some("claude_cli_sync".to_string()),
            route_family: Some("claude".to_string()),
            route_kind: Some("cli".to_string()),
            execution_path: Some("local_execution_runtime_miss".to_string()),
            local_execution_runtime_miss_reason: Some("all_candidates_skipped".to_string()),
            selected_provider_id: Some("provider-1".to_string()),
            selected_endpoint_id: Some("endpoint-1".to_string()),
            selected_provider_api_key_id: Some("provider-key-1".to_string()),
            has_format_conversion: Some(true),
        }
    );
}

#[test]
fn attach_usage_routing_snapshot_metadata_adds_missing_keys_without_overwriting_existing_values() {
    let metadata = attach_usage_routing_snapshot_metadata(
        Some(json!({
            "candidate_id": "cand-existing",
            "route_kind": "cli"
        })),
        &UsageRoutingSnapshot {
            candidate_id: Some("cand-1".to_string()),
            candidate_index: Some(2),
            key_name: Some("primary".to_string()),
            planner_kind: Some("claude_cli_sync".to_string()),
            route_family: Some("claude".to_string()),
            route_kind: Some("chat".to_string()),
            execution_path: Some("local_execution_runtime_miss".to_string()),
            local_execution_runtime_miss_reason: Some("all_candidates_skipped".to_string()),
            selected_provider_id: None,
            selected_endpoint_id: None,
            selected_provider_api_key_id: None,
            has_format_conversion: None,
        },
    )
    .expect("metadata should remain");

    assert_eq!(
        metadata,
        json!({
            "candidate_id": "cand-existing",
            "key_name": "primary",
            "planner_kind": "claude_cli_sync",
            "route_family": "claude",
            "route_kind": "cli",
            "execution_path": "local_execution_runtime_miss",
            "local_execution_runtime_miss_reason": "all_candidates_skipped"
        })
    );
}

#[test]
fn usage_settlement_pricing_snapshot_from_usage_extracts_typed_billing_fields() {
    let snapshot = usage_settlement_pricing_snapshot_from_usage(
        &UpsertUsageRecord {
            request_id: "req-125".to_string(),
            user_id: None,
            api_key_id: None,
            username: None,
            api_key_name: None,
            provider_name: "openai".to_string(),
            model: "gpt-5".to_string(),
            target_model: None,
            provider_id: Some("provider-1".to_string()),
            provider_endpoint_id: Some("endpoint-1".to_string()),
            provider_api_key_id: Some("provider-key-1".to_string()),
            request_type: Some("chat".to_string()),
            api_format: Some("openai:chat".to_string()),
            api_family: Some("openai".to_string()),
            endpoint_kind: Some("chat".to_string()),
            endpoint_api_format: Some("openai:chat".to_string()),
            provider_api_family: Some("openai".to_string()),
            provider_endpoint_kind: Some("chat".to_string()),
            has_format_conversion: Some(false),
            is_stream: Some(false),
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
            cache_creation_input_tokens: None,
            cache_creation_ephemeral_5m_input_tokens: None,
            cache_creation_ephemeral_1h_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_cost_usd: None,
            cache_read_cost_usd: None,
            output_price_per_1m: Some(15.0),
            total_cost_usd: None,
            actual_total_cost_usd: None,
            status_code: Some(200),
            error_message: None,
            error_category: None,
            response_time_ms: Some(100),
            first_byte_time_ms: None,
            status: "completed".to_string(),
            billing_status: "pending".to_string(),
            request_headers: None,
            request_body: None,
            request_body_ref: None,
            provider_request_headers: None,
            provider_request_body: None,
            provider_request_body_ref: None,
            response_headers: None,
            response_body: None,
            response_body_ref: None,
            client_response_headers: None,
            client_response_body: None,
            client_response_body_ref: None,
            request_body_state: None,
            provider_request_body_state: None,
            response_body_state: None,
            client_response_body_state: None,
            candidate_id: None,
            candidate_index: None,
            key_name: None,
            planner_kind: None,
            route_family: None,
            route_kind: None,
            execution_path: None,
            local_execution_runtime_miss_reason: None,
            request_metadata: None,
            finalized_at_unix_secs: None,
            created_at_unix_ms: Some(100),
            updated_at_unix_secs: 100,
        },
        Some(&json!({
            "rate_multiplier": 0.5,
            "is_free_tier": false,
            "billing_snapshot": {
                "schema_version": "2.0",
                "status": "complete",
                "resolved_variables": {
                    "input_price_per_1m": 3.0,
                    "output_price_per_1m": 15.0,
                    "cache_creation_price_per_1m": 3.75,
                    "cache_read_price_per_1m": 0.30,
                    "price_per_request": 0.02
                }
            }
        })),
    )
    .expect("snapshot should build");

    assert_eq!(
        snapshot,
        UsageSettlementPricingSnapshot {
            billing_status: Some("pending".to_string()),
            billing_snapshot_schema_version: Some("2.0".to_string()),
            billing_snapshot_status: Some("complete".to_string()),
            billing_input_tokens: Some(1),
            billing_effective_input_tokens: Some(1),
            billing_output_tokens: Some(2),
            billing_total_input_context: Some(1),
            rate_multiplier: Some(0.5),
            is_free_tier: Some(false),
            input_price_per_1m: Some(3.0),
            output_price_per_1m: Some(15.0),
            cache_creation_price_per_1m: Some(3.75),
            cache_read_price_per_1m: Some(0.30),
            price_per_request: Some(0.02),
            ..UsageSettlementPricingSnapshot::default()
        }
    );
}

#[test]
fn usage_settlement_pricing_snapshot_with_billing_status_only_is_still_persisted() {
    let snapshot = UsageSettlementPricingSnapshot {
        billing_status: Some("pending".to_string()),
        ..UsageSettlementPricingSnapshot::default()
    };

    assert!(snapshot.any_present());
}

#[test]
fn attach_usage_settlement_pricing_snapshot_metadata_adds_missing_values_without_overwriting() {
    let metadata = attach_usage_settlement_pricing_snapshot_metadata(
        Some(json!({
            "rate_multiplier": 1.0,
            "billing_snapshot_status": "complete"
        })),
        &UsageSettlementPricingSnapshot {
            billing_status: None,
            billing_snapshot_schema_version: Some("2.0".to_string()),
            billing_snapshot_status: Some("incomplete".to_string()),
            rate_multiplier: Some(0.5),
            is_free_tier: Some(false),
            input_price_per_1m: Some(3.0),
            output_price_per_1m: Some(15.0),
            cache_creation_price_per_1m: Some(3.75),
            cache_read_price_per_1m: Some(0.30),
            price_per_request: Some(0.02),
            ..UsageSettlementPricingSnapshot::default()
        },
    )
    .expect("metadata should remain");

    assert_eq!(
        metadata,
        json!({
            "rate_multiplier": 1.0,
            "billing_snapshot_status": "complete",
            "billing_snapshot_schema_version": "2.0",
            "is_free_tier": false,
            "input_price_per_1m": 3.0,
            "output_price_per_1m": 15.0,
            "cache_creation_price_per_1m": 3.75,
            "cache_read_price_per_1m": 0.30,
            "price_per_request": 0.02
        })
    );
}

#[test]
fn settlement_fallback_normalizes_cache_write_and_read_tokens_once() {
    let effective = super::usage_effective_input_tokens(Some(100), Some(20), Some(30), "openai");
    assert_eq!(effective, Some(50));
    assert_eq!(
        super::usage_total_input_context(Some(100), effective, Some(20), Some(30),),
        Some(100)
    );

    let claude_effective =
        super::usage_effective_input_tokens(Some(100), Some(20), Some(30), "claude");
    assert_eq!(claude_effective, Some(100));
    assert_eq!(
        super::usage_total_input_context(Some(100), claude_effective, Some(20), Some(30),),
        Some(150)
    );
}
