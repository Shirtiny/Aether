# Pre-Dispatch Latency Investigation (2026-07-26)

## Finding

A request spends ~116ms between resolving its auth context and dispatching to
the upstream. Instrumentation attributes it as:

| span | p50 |
|---|---|
| `request_body_buffer_completed` → `candidate_loop_started` | 82ms |
| `candidate_loop_attempt_started` → `direct_request_prepared` | 24ms |
| `candidate_loop_started` → `upstream_url_resolved` | 15ms |
| `stream_plan_built` → `candidate_loop_attempt_started` | 5ms |

The 82ms is candidate materialization, measured at 80.0ms mean. Splitting it
further:

| phase | mean | p50 | p90 |
|---|---|---|---|
| `page_cursor.next_page()` | 43.3ms | 41.6ms | 63.4ms |
| transport resolution | 23.3ms | 25.0ms | 36.7ms |

`next_priority_page` (`ai_serving/planner/candidate_source.rs`) loops over every
candidate API format and awaits a full candidate listing for each, one at a
time:

```rust
for candidate_api_format in self.candidate_api_formats.clone() {
    let Some(outcome) = self.next_page_for_api_format(&candidate_api_format).await? else {
        continue;
    };
    ...
}
```

For an `openai:responses` request `request_candidate_api_formats` returns four
formats — `openai:chat`, `openai:responses`, `claude:messages`,
`gemini:generate_content` — so the loop runs four listings at roughly 11ms each.

Three of those four exist only to supply format-conversion fallbacks. This
deployment carries 25 `openai:responses` endpoints, so the native format almost
always yields a usable candidate and the conversion listings are paid on every
request for a path that is rarely taken.

## What was ruled out, and how

Each of these was measured, not argued:

| candidate | measurement |
|---|---|
| Postgres execution | one 151ms window issued zero statements; in-window execution totals ~2ms |
| Redis | 0.1% busy, 7 ops/sec |
| CPU | 32ms for the entire request lifecycle, against an 82ms span |
| Thread starvation | five of six tokio workers parked, one in `epoll_wait` |
| Request body parsing | 278µs for a 175KB body |
| Bypass cache key hashing | 43µs |
| Preceding execution steps | `LocalVideoContent` 3µs, `LocalImage` 2µs, `LocalOpenAiChat` 1µs, all declining |
| Auth api key concurrency retry | probe fired zero times |
| GIN index write amplification | dropping two never-scanned GIN indexes changed nothing |
| System config cache TTL | raising 3s → 30s moved p50 by ~10ms, inside noise |
| Proxy node lookups | caching cut reads 33.8 → 4.9 per request with no latency change |

The proxy node result is worth keeping in mind: removing 29 database round trips
per request did not move the latency, which is what first showed that round-trip
count was not the cost.

## Fix options

### A. Run the per-format listings concurrently

The four listings are independent. Running them together turns 4 × 11ms of wall
time into roughly one. Candidate set and ordering are unchanged, because results
are still combined in the existing order after collection.

The obstacle is state: `next_page_for_api_format` takes `&mut self` and mutates
per-format cursor bookkeeping (`requested_name_indexes`, per-format offsets,
scanned-row counters). Running the reads concurrently requires lifting that
per-format state out of `&mut self` so the futures do not alias, then merging it
back. That is a real refactor of a routing hot path and should carry tests that
pin candidate order and the scanned-row budget.

Expected saving: ~32ms of the 43ms page read.

### B. Defer the conversion-format listings

`next_page` already pops deferred pages before reading a format, and
`next_priority_page` already defers the non-matching formats it reads. Reading
only the client's own format up front, and letting the main loop pull conversion
formats on demand, removes three of the four listings.

This is smaller to implement but changes behaviour: `split_priority_conversion_page`
currently promotes some conversion candidates into the first page, so skipping
those reads changes candidate priority rather than just timing. It should not be
taken without deciding that the promotion is not wanted.

Expected saving: ~32ms, same as A, plus the database load of three listings.

A is the safer of the two and is the recommended direction.

## Scope note

This 82ms sits behind a first byte that is measured in seconds. The larger win
on this path was bounding the stream prefetch hold, which removed up to 3s from
roughly a quarter of streaming requests; see
`stream-prefetch-ttfb-investigation-2026-07-26.md`. What remains here is worth
fixing but is not what users feel.

## Reproducing the measurement

The probes used for this were temporary and have been reverted. To measure it
again, time these boundaries and log them at info level:

- `executor/stream_path.rs`, `maybe_execute_via_stream_decision_path` — phase
  breakdown of plan-kind resolution, body parse, stream match, bypass cache key,
  and the execution path call
- `executor/orchestration.rs`,
  `maybe_execute_stream_via_local_openai_responses_decision` — the attempt
  source build, with candidate count
- `ai_serving/planner/candidate_materialization.rs`, `load_next_page` — the page
  read and the transport resolution, split

Correlating the gateway access log with the `usage` table gives the outer window
without any code change:

```
window = time between auth_context_resolved and "direct request prepared"
```

Statement-level attribution needs `log_min_duration_statement = 0` on the
database, which can be set and reset with `pg_reload_conf()` and no restart.
Landing each statement inside a request's window, rather than counting
statements per request, is what separates pre-dispatch work from the usage
writes that follow it.
