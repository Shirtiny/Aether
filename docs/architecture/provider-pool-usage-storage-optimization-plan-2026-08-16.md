# Provider Pool Usage Storage Optimization Plan

> Status: **Plan only / not approved for execution**
> Date: 2026-08-16
> Scope: provider-pool 5h/weekly and Grok billing-window local usage statistics
> Production changes: **None**

## 1. Background

The provider-pool window-usage read path currently calculates local usage from
`usage_billing_facts`, which is backed by `usage` and
`usage_settlement_snapshots`. To keep Codex 5h/weekly statistics available, the
external Aether cleanup script currently retains these source tables for 15
days while stripping large request/response fields after five minutes.

This keeps the feature correct, but retains far more information than the pool
window calculation needs.

Production observations on 2026-08-16 after a `recent` cleanup:

| Object | Approximate size | Approximate live rows |
| --- | ---: | ---: |
| `usage` | 1,328 MB | 933k |
| `usage_settlement_snapshots` | 376 MB | 934k |
| `usage_prompt_capture_entries` | 132 MB | 190k |
| Whole Aether database | 1,986 MB | — |

Of the 1,328 MB occupied by `usage`, approximately 766 MB is indexes and 561 MB
is heap data. The request-body/metadata TOAST data had already been stripped,
so most remaining space is live statistical rows and their indexes rather than
unreclaimed request bodies.

The pool window calculation only needs:

- provider API key ID;
- usage timestamp;
- request count;
- total tokens;
- settled cost.

It should not require 15 days of complete `usage` and settlement rows.

## 2. Accepted simplifications

This plan intentionally adopts the following product decisions:

1. **Do not backfill historical pool statistics.** Existing historical local
   window data may disappear after the read-path switch.
2. **Do not implement a distributed rollout.** No new leader election,
   cross-region coordination, distributed cache invalidation, or multi-version
   compatibility layer is required.
3. **Do not implement shadow reads, gray traffic, or percentage rollout.** The
   new read path is switched on directly with the release.
4. **Missing historical buckets are normal.** They return zero or the sum of
   whatever new buckets exist; they must not make the pool-list API fail.
5. The first active 5h/weekly/monthly windows after release may be incomplete.
   They become complete naturally after one full window has elapsed.

## 3. Goals

1. Preserve exact inclusive-start/exclusive-end semantics for newly collected
   usage in:
   - Codex 5h;
   - Codex weekly;
   - Grok billing weekly;
   - Grok billing monthly when present.
2. Remove the pool feature's dependency on long-lived `usage` and
   `usage_settlement_snapshots` rows.
3. Avoid adding a synchronous database round trip to the request path.
4. Keep counter application atomic and replay-safe.
5. Permit raw request/settlement retention to be configured independently from
   pool-stat retention.
6. Make missing or not-yet-collected history a non-error condition.
7. Reduce steady-state database size and eliminate daily `VACUUM FULL` work on
   multi-gigabyte pool-stat source tables.

## 4. Non-goals

- This plan does not authorize a production migration, deployment, cleanup, or
  index removal.
- This plan does not change upstream quota probing or scheduler exhaustion
  decisions.
- This plan does not reconstruct statistics that predate the new table.
- This plan does not preserve complete local totals for a quota window that
  began before deployment.
- This plan does not assume that every other `usage_billing_facts` caller can
  tolerate shorter retention; those callers must still be audited.
- This plan does not perform a destructive reverse migration during rollback.

## 5. Precision decision: use second buckets

Minute buckets were considered first, but production quota windows are not
consistently minute-aligned. Observed reset-second offsets include non-zero
values for both Codex and Grok windows. A minute rollup could therefore include
or omit up to one boundary minute.

The existing delta payload already carries `usage_created_at_unix_secs`, and
window bounds are integer Unix seconds. The initial implementation should use
**provider-key + Unix-second buckets**. This retains exact semantics for all
newly collected usage without preserving complete request rows.

Observed cardinality for the retained production data:

| Representation | Rows/buckets |
| --- | ---: |
| Provider-key request rows | 911,510 |
| Provider-key + second buckets | 787,599 |
| Provider-key + minute buckets | 127,683 |

Second buckets provide less row-count compression than minute buckets, but
each row is narrow and needs only one primary query index. They replace two wide
tables and dozens of unrelated indexes. A later minute/cycle rollup may be
considered only if a documented boundary-precision tradeoff is acceptable.

## 6. Target data model

Proposed logical schema:

```sql
CREATE TABLE provider_api_key_usage_seconds (
    provider_api_key_id VARCHAR(36) NOT NULL,
    bucket_unix_secs    BIGINT NOT NULL,
    request_count       BIGINT NOT NULL DEFAULT 0,
    total_tokens        BIGINT NOT NULL DEFAULT 0,
    total_cost_usd      NUMERIC(20, 8) NOT NULL DEFAULT 0,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (provider_api_key_id, bucket_unix_secs)
);
```

PostgreSQL should also receive a small time-oriented BRIN index for retention
scans:

```sql
CREATE INDEX ... ON provider_api_key_usage_seconds
USING BRIN (bucket_unix_secs);
```

Equivalent MySQL and SQLite migrations should preserve repository backend
parity. Backend-specific index syntax may differ.

The table intentionally does not store:

- request/response headers or bodies;
- request metadata;
- user identity;
- routing snapshots;
- settlement JSON;
- model/provider presentation fields.

## 7. Write path

No additional synchronous request-path write is required. Extend the existing
`usage_counter_deltas` flush transaction:

1. Claim a bounded batch of unprocessed deltas.
2. Continue using the existing advisory lock and single-flusher behavior.
3. For `provider_api_key` deltas with a usage timestamp, group in memory by:

   ```text
   (provider_api_key_id, usage_created_at_unix_secs)
   ```

4. Bulk upsert the grouped values:

   ```sql
   INSERT INTO provider_api_key_usage_seconds (...)
   VALUES (...)
   ON CONFLICT (provider_api_key_id, bucket_unix_secs)
   DO UPDATE SET
       request_count  = provider_api_key_usage_seconds.request_count
                        + EXCLUDED.request_count,
       total_tokens   = provider_api_key_usage_seconds.total_tokens
                        + EXCLUDED.total_tokens,
       total_cost_usd = provider_api_key_usage_seconds.total_cost_usd
                        + EXCLUDED.total_cost_usd,
       updated_at     = NOW();
   ```

5. Apply the existing lifetime/provider/model counters.
6. Mark the claimed delta rows processed.
7. Commit all steps in the same transaction.

If any step fails, bucket updates and processed markers roll back together. The
request itself remains unaffected because this is a background worker.

Corrections, removals, and provider-key changes must apply signed deltas rather
than clamping values. Zero-valued bucket rows may be removed asynchronously.
Negative final bucket values should emit an internal metric/log rather than be
silently clamped.

No historical backfill job, cutover timestamp, or replay of previously cleaned
usage is required. The table begins collecting data when the new writer starts.
Any still-pending delta naturally processed after deployment may contribute a
small amount of pre-deployment history; this is acceptable and requires no
special handling.

## 8. Read path and empty-history behavior

Change the implementation behind
`summarize_usage_by_provider_api_key_windows` from
`usage_billing_facts` to the bucket table:

```sql
SELECT
    requested.provider_api_key_id,
    requested.window_code,
    COALESCE(SUM(bucket.request_count), 0) AS request_count,
    COALESCE(SUM(bucket.total_tokens), 0) AS total_tokens,
    COALESCE(SUM(bucket.total_cost_usd), 0) AS total_cost_usd
FROM requested
LEFT JOIN provider_api_key_usage_seconds AS bucket
  ON bucket.provider_api_key_id = requested.provider_api_key_id
 AND bucket.bucket_unix_secs >= requested.start_unix_secs
 AND bucket.bucket_unix_secs < requested.end_unix_secs
GROUP BY ...;
```

Required behavior:

- no matching buckets: return zero totals;
- only part of a window exists: return the available partial total;
- history was cleaned: return zero/partial totals;
- do not return a user-facing error merely because history is absent;
- keep the API and frontend payload schemas unchanged.

Missing data and query failure must be handled differently:

- no matching historical rows is valid data and returns zero/partial totals;
- an actual bucket-query/database failure must be logged internally **and
  returned as an explicit API error to the administrator**;
- the handler must never convert a real query failure into fake zero totals,
  because that would make a storage fault indistinguishable from genuine zero
  usage.

The release switches directly to this bucket read path. No `raw/shadow/bucket`
traffic-splitting mode is required. A simple emergency configuration switch may
be retained to restore the raw implementation during the initial release, but
it is a rollback control rather than a gray-testing mechanism.

## 9. Retention and cleanup

Proposed retained data after the dependency audit:

| Data class | Proposed retention |
| --- | ---: |
| Request/response blob fields | 5 minutes |
| `usage` request skeleton | 48 hours |
| `usage_settlement_snapshots` raw rows | 48 hours |
| Provider-key second buckets | 40 days |
| Risk-control evidence | Existing protected policy |

Forty days covers future Codex 5h/weekly and Grok weekly/monthly windows. At
startup, older portions of active windows are intentionally absent.

Before shortening raw retention, inventory every remaining caller of
`usage_billing_facts`, including:

- wallet daily ledgers;
- hourly/daily statistics aggregation;
- user and API-key historical statistics;
- provider-performance reporting;
- administrative rebuild operations;
- request-history APIs.

Each caller must either:

1. complete within the proposed raw-retention window;
2. read an existing hourly/daily rollup;
3. receive its own compact fact/aggregate table; or
4. explicitly accept that older raw rebuilds are unavailable.

The external `/root/clean` policy should be updated as a separate configuration
change once the pool read path no longer depends on the raw source tables.
Daily `VACUUM FULL` of the large source tables should be replaced by bounded
deletion plus ordinary autovacuum/`VACUUM (ANALYZE)` after an optional one-time
shrink.

Bucket retention should use bounded deletion by `bucket_unix_secs`. At the
expected narrow-table size, ordinary autovacuum can reuse freed pages without a
daily full-table rewrite.

## 10. Runtime errors and metrics

Keep only runtime metrics needed to diagnose the new storage path:

- bucket-flush duration and rows per batch;
- bucket upsert failures;
- pending-delta count and oldest-pending age;
- latest processed usage second;
- bucket query latency;
- bucket table/index size;
- negative/zero bucket counts;
- bucket-query API error count.

No shadow comparison, gray validation, acceptance gate, or observation window
is part of this plan. A real bucket-query failure is an administrator-visible
error; only absent history is treated as zero/partial usage.

## 11. Request identity retention and index optimization

Pool-stat storage is separate from the earlier `cafecode_uid`/
`cafecode_uname` history issue. To retain those values after metadata stripping,
store them as compact scalar audit fields (or in a compact identity-audit table)
instead of only inside `request_metadata`.

The current metadata expression indexes also require correction:

- `idx_usage_cafecode_uid_created_id`: approximately 69 MB;
- `idx_usage_cafecode_uname_created_id`: approximately 69 MB;
- `idx_usage_client_family_created_id`: approximately 69 MB.

Most retained rows have `request_metadata IS NULL`, but the current expressions
still index an empty-string value. Replace them with partial non-empty indexes,
adjust matching query predicates, verify `EXPLAIN` plans, and then remove the
old indexes concurrently.

`usage_pkey(id)` and `ix_usage_id(id)` are duplicate indexes. After a code and
query-plan audit, keep the primary key and remove `ix_usage_id`. These index
changes are expected to save roughly 250 MB at the current row count,
independently of the aggregate-table work.

## 12. Implementation work breakdown

### Change set 1: schema and configuration

- Add PostgreSQL/MySQL/SQLite migrations.
- Update bootstrap/generated schemas as required by repository workflow.
- Add the primary lookup index and backend-appropriate retention index.
- Add one emergency raw-read rollback switch if desired; do not add percentage
  rollout or shadow-mode configuration.

### Change set 2: incremental writer

- Extend `UsageCounterDeltaAggregates` with provider-key second buckets.
- Add one bulk upsert query per backend.
- Apply bucket changes inside the existing flush transaction.
- Add operational metrics and negative-bucket detection.
- Add bounded retention cleanup.
- Do not add a backfill command.

### Change set 3: direct read switch

- Implement bucket-backed window summaries.
- Return zero/partial totals for missing history.
- Return an explicit administrator-visible API error for bucket lookup/database
  failures; never replace a real failure with zero totals.
- Keep API and frontend contracts unchanged.
- Switch the release directly to the new read path.

### Change set 4: raw-retention and indexes

- Audit remaining `usage_billing_facts` callers.
- Change raw retention only after that audit.
- Persist compact CafeCode identity fields independently from metadata blobs.
- Build partial indexes concurrently.
- Verify query plans and remove superseded/duplicate indexes.

## 13. Test plan

Unit tests:

- empty bucket table returns zero totals;
- partially populated windows return available totals;
- positive additions;
- corrections and signed removals;
- provider-key changes;
- several requests in the same second;
- exact inclusive-start/exclusive-end boundaries;
- missing usage timestamps;
- zero-row cleanup and negative-row detection;
- batch retry and transaction rollback;
- bucket-query error is returned explicitly to the administrator;
- missing bucket history still returns zero/partial totals successfully.

Repository/integration tests:

- newly written usage is summarized correctly;
- Postgres/MySQL/SQLite parity;
- a failed upsert does not mark deltas processed;
- a replay does not double count;
- the pool-list response schema is unchanged;
- no historical rows is a successful response, not an error.

Performance tests:

- flush at current load and at least 10x current average load;
- one-key hotspot batches;
- multi-key batches;
- weekly/monthly range queries;
- retention deletion while reads and flushes continue;
- index-size and query-plan comparison.

No historical backfill test, dual-read comparison, distributed rollout test, or
gray-traffic test is required.

## 14. Rollback

During the initial release, the emergency rollback is:

1. switch the read implementation back to the existing raw query if the raw
   source still exists;
2. disable bucket writes if they cause persistent background-flush failures;
3. leave the additive table and migration in place;
4. do not run a destructive reverse migration.

If raw history has already been cleaned, rollback does not reconstruct it. In
that case, the pool list must continue returning zero/partial local totals until
the bucket table accumulates new data. This is an accepted product behavior and
must not become a user-facing API error.

## 15. Expected outcome

The exact size must be measured after implementation. Based on current
production cardinality, the second-bucket design should reduce the approximately
1.7 GB currently occupied by the two pool-stat source tables and their indexes
to a substantially smaller narrow aggregate. With 40 days of exact second
buckets plus 48 hours of raw history, a preliminary steady-state target is
approximately **600 MB–1 GB for the whole database**, subject to traffic growth
and the separate prompt-capture footprint.

Existing local history is intentionally discarded rather than migrated. New
5h/weekly/monthly totals become complete naturally after their first full window
following deployment.
