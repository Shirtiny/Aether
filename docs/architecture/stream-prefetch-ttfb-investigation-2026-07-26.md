# Stream Prefetch TTFB Investigation (2026-07-26)

## Summary

Aether holds a streaming response after it has already received the first
upstream byte, while it inspects the stream for errors embedded in a `200`
response. For reasoning models the hold ran to a fixed 3s ceiling on roughly a
quarter of all streaming requests, and the whole hold is paid in client-visible
time to first byte.

Measured over a 90 minute production window (2077 correlated streaming
requests), the hold distribution was:

| percentile | hold |
|---|---|
| p50 | -30 ms |
| p75 | 53 ms |
| p90 | 2630 ms |
| p95 | 2910 ms |
| p99 | 3011 ms |
| max | 23037 ms |
| mean | 470 ms |

23.3% of streaming requests were held longer than 200 ms, for a cumulative
934.7s of added client wait in that 90 minute window. The p90-p99 values cluster
just under 3000 ms, which is the `CONTROL_STREAM_PREFETCH_EXTENSION_TIMEOUT`
ceiling rather than a property of the upstream.

## How the delay was found

The gap first showed up as a discrepancy between two dashboards for the same
request. Sub2API reported a 6.54s TTFB; Aether reported 3.43s for its own view
of the same request. The two systems measure different spans:

- Aether's `first_byte_time_ms` starts at `Instant::now()` immediately before
  `send_request` in `execution_runtime/transport.rs`, so it measures upstream
  behaviour only.
- Sub2API's TTFB ends when the first byte is read off the response body Aether
  returns.

Correlating one trace made the mechanism explicit:

```
11:49:54.757  auth_context_resolved
11:49:54.917  execution runtime direct request prepared   <- Aether's clock starts
11:49:58.35   first upstream byte                          <- Aether reports ttfb 3.43s
              (2.93s with data in hand and nothing forwarded)
11:50:01.276  http_request_completed, elapsed_ms=6522      <- response released downstream
11:50:01.30   Sub2API records first byte at 6.54s
```

`elapsed_ms=6522` and Sub2API's 6.54s agree to within 20 ms, which identifies the
release moment rather than the upstream as the thing Sub2API was waiting on.

## Mechanism

`execution_runtime/stream/execution.rs` prefetches frames before finalizing the
downstream response, so that an error carried inside a `200` body can still be
failed over to another candidate. Once headers are sent downstream, failover is
no longer possible.

Prefetch is bounded by `MAX_STREAM_PREFETCH_FRAMES` (5) and
`MAX_STREAM_PREFETCH_BYTES` (16 KiB), and
`should_skip_direct_finalize_prefetch` deliberately does not skip prefetch for
`text/event-stream`.

Each frame is classified by `inspect_prefetched_stream_body`
(`execution_runtime/stream/error.rs`). `stream_json_body_is_prefetch_control`
treats these as control-only, returning `NeedMore`:

- `response.created`, `response.in_progress`, `response.queued`
- `response.output_item.added` carrying a `reasoning` item
- `response.content_part.added` / `response.reasoning_summary_part.added` with no
  visible content
- any body whose `response.status` is `created` / `in_progress` / `queued`

`NeedMore` sets `continue_prefetching_control_stream = true`, which switches the
loop into the control extension: the byte limit rises to 256 KiB, the frame limit
stops applying, and the only remaining bound is
`CONTROL_STREAM_PREFETCH_EXTENSION_TIMEOUT`.

Reasoning models open every stream with exactly this control-only preamble and
then pause while they reason. The extension therefore engages on essentially
every reasoning request and, because visible content does not arrive during the
pause, runs to the full timeout before releasing with
`control_extension_expired`.

Treating a control event as proof of success is not a valid shortcut. The
existing tests
`inspect_prefetched_stream_body_keeps_openai_response_control_events_pending` and
`inspect_prefetched_stream_body_detects_openai_response_failed_after_control_event`
encode the reason: `response.failed` can and does arrive after a control
preamble.

## What the wait actually bought

Across the same 90 minute window, every failed candidate carried a non-2xx
status or a transport-level error:

```
status_code | error_type                                  | count
------------+---------------------------------------------+------
        400 | execution_runtime_stream_non_success_status  |    12
        503 |                                              |     5
        400 | server_error                                 |     4
        503 | retryable_upstream_status                    |     4
        504 | local_stream_candidate_watchdog_timeout      |     3
            | execution_runtime_unavailable                |     3
        402 |                                              |     3
        401 |                                              |     3
        403 | retryable_upstream_status                    |     2
        429 |                                              |     1
        400 | upstream_error                               |     1
```

No candidate failed with `status_code = 200`. Every failover in the window was
decidable from the status line, before any body prefetch. The embedded-error
path that the extension exists to serve did not fire once in 2091 streaming
requests.

This is a bounded observation, not a proof that the path never fires. The
provider session risk-control blocks that
`list_risk_control_provider_ids_by_client_session_key` guards against are exactly
the rare-but-real case it protects, so the inspection is worth keeping. What the
data does establish is that the *premium* was mispriced: a 3s ceiling charged
against roughly a quarter of streaming requests, for insurance that did not pay
out across a 2000-request sample.

## Change

`CONTROL_STREAM_PREFETCH_EXTENSION_TIMEOUT` reduced from 3s to 500ms.

The inspection logic, the control-event classification, and the frame and byte
limits are all unchanged. Only the ceiling on how long the control-only
extension may wait moves.

500ms is anchored on two things: it sits under the sibling
`REWRITTEN_STREAM_PREFETCH_TIMEOUT` (750ms), which is the codebase's existing
answer to "how long may we hold a stream to inspect it"; and an upstream that
rejects a request emits the rejection close behind its control preamble, well
inside that budget, because the rejection is a decision the upstream has already
made.

`bounds_control_stream_prefetch_extension_below_one_second` guards the constant
so a future edit cannot silently restore a multi-second stall.

## Trade-off accepted

An embedded error arriving between 500ms and 3s after the control preamble will
no longer be caught by prefetch. The client receives the upstream error inline
instead of Aether transparently retrying another candidate. This degrades
gracefully: the request still returns the upstream's own error, and errors
arriving promptly, which is the realistic shape, are still caught.

## Reporting the measurement downstream

A downstream proxy cannot derive any of this on its own: it sees one number,
first byte arriving, which folds together upstream latency, the hop, and the
hold described above. So the release also publishes what Aether measured, on the
same response that already carries `x-trace-id` and `x-aether-execution-path`:

| header | meaning |
|---|---|
| `x-aether-ttfb-ms` | upstream TTFB, measured from upstream dispatch |
| `x-aether-prefetch-ms` | how long the stream sat buffered after its first upstream data frame |
| `x-aether-prefetch-release` | why prefetch released: `frame_limit`, `byte_limit`, `non_error_detected`, `control_extension_expired`, `eof`, `skipped`, `not_started` |

`x-aether-prefetch-ms` is measured from the first data frame rather than from
prefetch start. Prefetch begins once upstream response headers arrive, which is
before the first body byte, so measuring from there would fold the upstream's own
first-byte latency into a number that is supposed to isolate latency this gateway
added.

All three values are already computed on the release path, so emitting them costs
no additional work, no additional queries, and no additional round trips.

### Consumer notes

Sub2API adopts `x-aether-ttfb-ms` in place of its own observation when the header
is present, at the single point where it instruments the upstream body
(`repository/http_upstream.go`, `attachUsageResponseTiming`). That one site
covers every forwarding path it has. Three consequences are worth recording:

- **The stored `first_byte_ms` changes meaning at deploy time.** Rows written
  before the switch measure Sub2API's own observation; rows after measure
  Aether's, for Aether-routed traffic. Percentile and trend charts over that
  column will show a step at the switchover. The column was introduced only
  hours earlier, in `cafecode-v0.0.34`, so the discontinuity spans a negligible
  amount of history and a separate column was judged not worth its cost.
- **Throughput is derived from two clocks.** The frontend computes
  `outputTokens / (durationMs - firstByteMs)`. After the switch `firstByteMs` is
  on Aether's clock, which starts at upstream dispatch, while `durationMs` stays
  on Sub2API's, which starts before the hop and before Aether's own routing
  work. The denominator is therefore too large by that offset, understating
  throughput by roughly 3% on a multi-second response. Closing it would require
  Aether to report elapsed time as well, which is only known at stream end and so
  cannot travel in a response header.
- **The header is absent on some responses.** When prefetch does not run there is
  no telemetry to report, and the non-streaming path does not emit these headers
  at all. Sub2API falls back to its own observation, which is the pre-existing
  behaviour. For a non-streaming response there is no meaningful first byte
  distinct from total duration, and no prefetch hold exists to correct for, so
  the remaining difference there is just the hop plus routing overhead.

## How to re-measure

Hold time is not logged directly. It is reconstructed by correlating the gateway
access log with the usage table:

```
hold = (t_auth + elapsed_ms) - (t_prep + first_byte_time_ms)
```

where `t_auth` is the `auth_context_resolved` timestamp, `t_prep` is the
`gateway execution runtime direct request prepared` timestamp, `elapsed_ms` comes
from `http_request_completed` on `execution_path="execution_runtime_stream"`, and
`first_byte_time_ms` comes from `usage` joined on `request_id`.

The prefetch release reasons (`frame_limit`, `byte_limit`, `non_error_detected`,
`control_extension_expired`, `eof`) are logged at `debug!` level and are not
visible at production log levels, which is why the reconstruction above is
needed. Raising the gateway log level would expose `prefetch_release_reason`
directly and is the more precise instrument if the question comes up again.
