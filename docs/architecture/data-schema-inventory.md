# Aether Data Schema Inventory

This inventory is the maintenance map for the three SQL drivers. The executable
`sqlx` migrations remain under `crates/aether-data/migrations/{postgres,mysql,sqlite}`.
Do not split already-shipped migration files without also deciding how to handle
existing `_sqlx_migrations` rows.

The maintainable schema source is under `crates/aether-data/schema`. Manifests
there compose back into the executable SQL files and are checked by tests, so
the split source is not just documentation.

The schema directory separates human-maintained sources from generated/runtime
outputs:

| Layer | Purpose |
|---|---|
| `schema/logical/*.toml` | Human-maintained long-term table-structure source. |
| `schema/drivers/{postgres,mysql,sqlite}/**` | Human-maintained executable-SQL fragments while generation is being promoted incrementally. |
| `schema/bootstrap/postgres/**` | Human-maintained source fragments for the Postgres empty-database bootstrap snapshot. |
| `schema/generated/**` | Machine-written SQL from logical schema; checked in for audit and drift detection only. |
| `migrations/**` | Runtime SQL artifacts composed from manifests. |

Generated SQL is not hand-maintained and runtime code does not load it. It
exists to prove that the logical schema can emit driver SQL and to provide the
candidate replacement for handwritten fragments.

## Logical Type Map

| Logical type | Postgres | MySQL | SQLite | Notes |
|---|---|---|---|---|
| `id` | `varchar/text` | `varchar` | `text` | Repository DTOs treat ids as strings. |
| `bool` | `boolean` | `tinyint(1)/boolean` | `integer` | Repositories normalize to Rust `bool`. |
| `time_unix` | `bigint` or legacy `timestamptz` | `bigint` | `integer` | New cross-driver paths prefer unix seconds/ms. |
| `json` | `json/jsonb` | `text/json-compatible` | `text` | Application parses through `serde_json::Value`. |
| `decimal_money` | `numeric` or `double precision` legacy | `double` | `real` | Wallet precision should be reviewed before new money tables. |
| `blob` | `bytea` | `longblob/blob` | `blob` | Used for compressed body payloads. |
| `enum` | `enum` or `varchar` legacy | `varchar` | `text` | Repository contracts own allowed values. |

## Baseline Source Plan

The current executable SQL files are intentionally kept stable for runtime
compatibility. Their maintainable sources are:

| Driver | Executable SQL | Source manifest |
|---|---|---|
| Postgres baseline | `migrations/postgres/20260403000000_baseline.sql` | `schema/drivers/postgres/baseline/manifest.txt` |
| Postgres empty-database snapshot | `aether-data` build output (`OUT_DIR/empty_database_snapshot.sql`) | `schema/bootstrap/postgres/manifest.txt` |
| MySQL baseline | `migrations/mysql/20260403000000_baseline.sql` | `schema/drivers/mysql/baseline/manifest.txt` |
| SQLite baseline | `migrations/sqlite/20260403000000_baseline.sql` | `schema/drivers/sqlite/baseline/manifest.txt` |

All driver manifests are kept as a small set of numbered SQL fragments. Postgres
uses execution-phase fragments (`001_types_and_tables.sql`,
`002_defaults.sql`, `003_constraints.sql`, `004_indexes.sql`,
`005_foreign_keys.sql`, `006_footer.sql`) so pg_dump ordering remains stable
when composed. MySQL and SQLite use similarly numbered domain fragments. After
editing fragments, run:

```bash
bash crates/aether-data/schema/compose_schema.sh compose
bash crates/aether-data/schema/compose_schema.sh check
```

Use `schema/logical/*.toml` for new table structure first; handwritten driver
fragments remain for executable migration compatibility and generator gaps.

`crates/aether-data/schema/logical/*.toml` is the single-maintenance source for
table structure. `aether-data-schema` renders it to
`schema/generated/{postgres,mysql,sqlite}/baseline`, and
`compose_schema.sh check` verifies that the generated SQL is current and that
required executable SQL tables are represented in logical schema. The generated
directory carries its own machine-generated README and per-file `Do not edit`
headers; changes there should come only from `compose_schema.sh generate`.

## Table Inventory

| Area | Tables | Owner | Generation target |
|---|---|---|---|
| Identity/auth | `users`, `api_keys`, `management_tokens`, `user_preferences`, `user_sessions`, `user_oauth_links` | `repository/users`, `repository/auth`, `repository/management_tokens`, auth modules | Good first candidate for schema manifest/query helper generation. |
| Provider catalog | `providers`, `provider_api_keys`, `provider_endpoints`, `models`, `global_models`, `api_key_provider_mappings`, `provider_usage_tracking` | `repository/provider_catalog`, `repository/global_models`, scheduler read paths | Keep complex selection SQL handwritten; generate basic CRUD only. |
| Auth config | `auth_modules`, `oauth_providers`, `ldap_configs` | `repository/auth_modules`, `repository/oauth_providers`, `repository/users` | Good candidate for generated CRUD. |
| Proxy nodes | `proxy_nodes`, `proxy_node_events` | `repository/proxy_nodes` | Good candidate for generated CRUD plus handwritten heartbeat update. |
| Wallet/billing | `wallets`, `wallet_transactions`, `wallet_daily_usage_ledgers`, `payment_orders`, `payment_callbacks`, `refund_requests`, `redeem_code_batches`, `redeem_codes`, `billing_rules`, `dimension_collectors` | `repository/wallet`, `repository/billing`, `repository/settlement` | Keep settlement/ledger math explicit; generate table definitions and simple reads. |
| Usage/audit | `usage`, `usage_counter_deltas`, `usage_body_blobs`, `usage_http_audits`, `usage_routing_snapshots`, `usage_settlement_snapshots`, `request_candidates`, `audit_logs` | `repository/usage`, `repository/candidates`, `repository/audit` | Keep core write/audit queries handwritten. |
| Runtime tasks | `video_tasks`, `gemini_file_mappings`, `announcements`, `announcement_reads` | `repository/video_tasks`, `repository/gemini_file_mappings`, `repository/announcements` | Good candidate for generated CRUD except polling claim logic. |
| Stats | `stats_*`, `schema_backfills` | backend aggregation modules | Keep aggregation SQL per-driver; generate table/index definitions only. |
| System | `system_configs` | `repository/system` through backend dispatch | Good candidate for generated CRUD. |

## Logical Schema Coverage

Logical schema currently covers the clean baseline table set plus portable
MySQL/SQLite table-creation migrations. Postgres-only historical follow-up
migrations remain driver-specific until their schema is normalized or promoted
as explicit generated/override fragments.

## Maintenance Rules

1. Keep driver-specific SQL inside driver-specific migration/repository files.
2. Use logical type names in docs and future schema manifests, not raw database
   type names.
3. Keep `jsonb` only in Postgres migrations/repositories/tests.
4. Prefer generated helpers for simple CRUD first; do not rewrite complex usage,
   billing, stats, or candidate-selection queries until contract tests cover the
   behavior.
5. When adding a new table, update this inventory and add it to the export domain
   plan if it must move across databases.
6. If a baseline fragment changes, run `compose_schema.sh compose` before tests
   so the executable SQL artifact is regenerated from the source manifest.

## Performance Notes

- Shared hot counters must use `bigint` and be updated by durable outbox flush
  workers, not request transactions.
- `usage_counter_deltas` is append-only until processed; processed rows are
  retained briefly for audit/replay and then batch-deleted by maintenance.
- Candidate selection joins stay handwritten but are protected in the gateway by
  a short TTL, single-flight cache invalidated by provider/routing writes.
