# Gateway Latency Investigation (2026-07-26)

Entry point for the day's latency work. Detailed findings live in
`stream-prefetch-ttfb-investigation-2026-07-26.md` and
`pre-dispatch-latency-investigation-2026-07-26.md`; this record covers what
prompted the investigation, what was measured, what shipped, what was tried and
did not work, and what is left.

## Trigger

Sub2API and Aether reported different time-to-first-byte for the same request.
One sampled request showed 6.54s against 3.43s. A later one showed 5.24s against
5.12s. The two systems measure different spans, so some difference is expected,
but 3.11s is not a measurement artifact.

## What the gap turned out to be

Two unrelated causes, of very different size.

### 1. Stream prefetch hold — up to 3s, fixed

Before releasing a streaming response downstream, Aether prefetches part of the
upstream stream so an error carried inside a `200` body can still be failed over.
Reasoning models open every stream with control-only events, which the inspection
cannot treat as proof of success, so the prefetch entered its control extension
and ran to a fixed 3s ceiling.

Measured over 90 minutes, 2077 correlated streaming requests:

| percentile | hold |
|---|---|
| p50 | -30 ms |
| p75 | 53 ms |
| p90 | 2630 ms |
| p95 | 2910 ms |
| p99 | 3011 ms |
| max | 23037 ms |

23.3% of streaming requests were held longer than 200ms, for 934.7s of added
client wait in that window. The p90-p99 values cluster just under the 3000ms
ceiling rather than reflecting anything about the upstream.

Across the same window every failed candidate carried a non-2xx status or a
transport error. No candidate failed with `status_code = 200`, which is the case
the extension exists to catch.

`CONTROL_STREAM_PREFETCH_EXTENSION_TIMEOUT` was reduced from 3s to 500ms. The
inspection logic, control-event classification, and frame and byte limits are
unchanged. After the change `control_extension_expired` appeared twice in 979
requests, down from being the dominant release reason.

This was the change that mattered to users.

### 2. Pre-dispatch overhead — ~120ms, understood but not fixed

The remainder of the gap is the span from resolving the auth context to
dispatching upstream: n=581, p50 117ms, p90 164ms, mean 121ms.

Instrumentation attributed 82ms of it to candidate materialization, and inside
that to the candidate page read rather than transport resolution:

| phase | mean | p50 | p90 |
|---|---|---|---|
| `page_cursor.next_page()` | 43.3ms | 41.6ms | 63.4ms |
| transport resolution | 23.3ms | 25.0ms | 36.7ms |

`next_priority_page` loops over every candidate API format and awaits a full
candidate listing for each, sequentially. An `openai:responses` request resolves
to four formats — `openai:chat`, `openai:responses`, `claude:messages`,
`gemini:generate_content` — at roughly 11ms per listing. Three of the four exist
only to supply format-conversion fallbacks, and this deployment carries 25 native
`openai:responses` endpoints, so those three listings are paid on every request
for a path that is rarely taken.

## What shipped

| change | effect |
|---|---|
| Prefetch extension 3s → 500ms | up to 3s removed from ~23% of streaming requests |
| `x-aether-ttfb-ms` / `x-aether-prefetch-ms` / `x-aether-prefetch-release` response headers | Sub2API can report Aether's own TTFB; present on ~20% of requests, absent when prefetch is skipped because Aether has read no body yet |
| Sub2API adopts the reported TTFB | one call site in `attachUsageResponseTiming` covers every forwarding path |
| Proxy node lookup cache, 250ms window | `proxy_nodes` reads 33.8 → 4.9 per request; no latency change |
| System config cache TTL 3s → 30s | ~10ms, inside noise |
| Dropped three never-scanned indexes | 25MB reclaimed; no latency change |

## What was tried and did not work

Recorded because each cost a build and deploy cycle, and because the pattern in
the failures is more useful than the failures themselves.

| attempt | reasoning at the time | measured outcome |
|---|---|---|
| Drop two GIN indexes on hot-updated JSONB columns | 22MB of never-scanned GIN on the most-updated table looked like write amplification | p50 127 → 128, mean 131 → 131 |
| Raise system config cache TTL 3s → 30s | 54 `system_configs` reads per request looked like cache misses | p50 127 → 117, inside noise |
| Cache proxy node lookups | 33.8 reads per request of a five-row table | reads 33.8 → 4.9, p50 117 → 118 |
| Auth api key concurrency poll loop | 10ms poll against a 150ms budget matched the latency shape | probe fired zero times; concurrency measured 0-1 against a limit of 20 |
| Request body size | bodies run 362KB at p50, 5.4MB at max | r = 0.042, slope 11ms/MB, and sub-50KB requests still took 88ms |

Every one of these was inferred from correlation. Each was disproved by direct
measurement. The proxy node cache is the clearest case: it removed 29 database
round trips per request and changed nothing, which is what first showed that
round-trip count was not the cost.

Ten explanations were ruled out in total. The ones that survived scrutiny were
all eliminated by a measurement that could only have one interpretation:

- one 151ms window issued zero SQL statements — not the database
- Redis sits at 0.1% busy — not the cache
- 32ms of CPU for the entire request lifecycle against an 82ms span — not CPU
- five of six tokio workers parked — not thread starvation
- a probe inside the suspected sleep fired zero times — not that loop

## Recommendations

### Do first: nothing

The 82ms sits behind a first byte measured in seconds. The 3s prefetch hold is
already gone. Unless first-byte latency becomes a stated goal again, the
remaining work is not worth the risk of touching candidate selection.

### If the pre-dispatch cost is taken up again

**Run the per-format listings concurrently.** The four listings are independent.
Running them together turns 4 × 11ms of wall time into roughly one, saving ~32ms
of the 43ms page read. The candidate set and its ordering are unchanged, because
results are still combined in the existing order after collection.

The obstacle is state: `next_page_for_api_format` takes `&mut self` and mutates
per-format cursor bookkeeping. Concurrency requires lifting that state out so the
futures do not alias, then merging it back. This is a refactor of a routing hot
path and needs tests pinning candidate order and the scanned-row budget.

**Batching the transport reads** addresses the other 23ms. A batched variant
already sits beside the per-candidate one, selected by
`LocalCandidateTransportReadMode`. Note that the format narrowing which made
`BatchedPage` risky is applied at cursor construction, not at the read, so the
batching can be adopted without narrowing the candidate set — but that separation
has to be made explicit in the code rather than relied on.

**Do not defer the conversion-format listings** without a deliberate decision.
It saves the same ~32ms and is far simpler, but `split_priority_conversion_page`
currently promotes some conversion candidates into the first page, so skipping
those reads changes candidate priority rather than only timing.

### On the reported-TTFB headers

`x-aether-ttfb-ms` is absent whenever prefetch is skipped, because Aether
releases the response before reading any body and genuinely does not know the
value yet. Sub2API falls back to its own observation, so roughly 20% of requests
currently show Aether's number and the rest show Sub2API's. If a single
consistent figure matters more than accuracy, the options are to emit the header
on the skip path by reading one byte first — which gives back what skipping
prefetch saves — or to accept the mixed source and label it.

Throughput is derived downstream as `output tokens / (duration - first byte)`.
After adoption the first byte comes from Aether's clock and the duration from
Sub2API's, which differ by Aether's routing overhead, understating throughput by
roughly 3% on a multi-second response. Closing that needs Aether to report
elapsed time, which is only known at stream end and so cannot travel in a
response header.

## Method notes

What worked, for the next time something like this comes up:

- **Land measurements on the timeline, not on the request.** Counting statements
  per request cannot say whether they run before or after dispatch. Landing each
  statement inside a request's `[auth_context_resolved, direct request prepared]`
  window is what separated planning from the usage writes that follow it.
- **Prefer a measurement that can only mean one thing.** The zero-SQL window,
  the parked workers, and the probe that never fired each closed a line of
  inquiry outright. Correlations reopened them.
- **Normalise before dismissing.** `docker stats` reporting 1.5% CPU was read as
  ruling out CPU. Against four cores and roughly one request per second that is
  ~60ms per request, the same order as the span under investigation. It was
  eventually ruled out by cgroup accounting, at 32ms, not by the percentage.
- **Temporary probes are cheap and decisive.** Three probe rounds settled what
  five rounds of inference could not. They were reverted in the same session.

Statement-level attribution needs `log_min_duration_statement = 0` on the
database, which can be set and reset with `pg_reload_conf()` and no restart.
Note that the extended protocol logs `bind` and `execute` separately; folding
`bind` lines into the preceding statement corrupts the attribution, which
produced one round of wrong table-level numbers here before it was caught.
