# Session-preserving overload retry

## Scope and evidence

Implement in Aether, without changing Sub2API or production configuration. The
existing classifier deliberately stops account failover for HTTP 503 carrying
`Our servers are currently overloaded. Please try again later.` to preserve
sticky sessions. It lacks a same-account retry. A production HTTP/SSE request on
2026-09-07 returned headers with status 200 and failed about 47 seconds later
with this message; its recorded output usage was zero, but the captured response
body was unavailable, so that alone does not prove it had no visible content.

## Design

- Retry this specific transient capacity rejection twice on the **same prepared
  plan**, endpoint, key, body and session identity. Do not rematerialize candidates
  or penalize/clear pool stickiness. Retain the existing terminal policy when the
  retry budget is exhausted. Other errors retain their existing handling.
- Use bounded backoff (250/750 ms plus request-specific jitter). Respect numeric
  `Retry-After` up to five seconds; do not retry early when the requested delay is
  longer. No new environment variable or database migration.
- HTTP errors are inspected before response headers are committed. Successful
  SSE responses have a small opening gate **before conversion, auditing and
  terminal settlement**. Only known empty lifecycle/preamble events are held;
  an overload before the first content-bearing event discards that attempt and
  reopens the same plan, even after the old 500 ms prefetch window.
- The gate sends neutral SSE comments while holding the preamble. No failed
  attempt's response ID, text, reasoning or tool call may escape. Release the
  buffered preamble immediately with the first substantive event, in order.
  A complete content-bearing JSON `data:` line commits immediately even when the
  final blank line is in a later network chunk. Error detection waits for the
  complete record, so partial or multiline JSON cannot trigger premature replay.
  Unknown events, tool starts, malformed data and bounded-buffer overflow commit
  conservatively and disable replay. After commit, use raw frame passthrough.
- Keep the existing prefetch ceiling, but enforce it from the first control
  frame rather than only after five frames. Do not add a lookahead timer after
  content arrives. Actual time-to-first-content has no artificial wait; header
  or first-byte metrics may measure comments rather than generated content.
  Existing strict-client keepalive filtering still applies; do not reinterpret a
  lifecycle/heartbeat first-byte metric as time to first generated text. The
  [official streaming guide](https://developers.openai.com/api/docs/guides/streaming-responses)
  likewise processes text deltas separately from lifecycle events.
- Once text, reasoning, tool activity or other substantive content has been
  committed, preserve the genuine error. Replaying at that point would duplicate
  output or side effects. No whole-response buffering, output splicing, hidden
  success or unbounded retry. This change is for HTTP/SSE, not stateful native
  WebSocket session replay.
- Failed hidden attempts do not finalize usage or candidate state. Only the
  final attempt reaches the existing lifecycle/reporting pipeline. Log retry,
  recovery, exhaustion and conservative release without request contents.
- A downstream disconnect before any content drops the opening immediately;
  do not dispatch new requests during the usual post-disconnect drain grace.
  Already-committed streams retain their existing bounded usage-drain behavior.
- If the final attempt already delivered a terminal SSE failure, retain its
  failure accounting but do not append a duplicate synthetic terminal event.

## Regression fixture corrections

The broader stream suite exposed six old generic-503 failover fixtures using
the exact capacity message despite the pre-existing classifier explicitly
stopping failover for that message. Use a generic unavailability message for
those tests; the new real HTTP/gate tests cover same-account capacity retry.
An idle-progress fixture also set `read_ms`, which now controls only upstream
idle (client progress defaults to 120 seconds). Set `stream_idle_ms` to exercise
both 40 ms bounds as that test intends. Neither correction relaxes assertions.

## Validation and release gates

1. Unit tests: exact overload matching, bounded retries/backoff, Retry-After,
   chunked UTF-8/CRLF/multiline/no-space SSE, preamble suppression, late overload,
   immediate content release, no replay after text/reasoning/tool start,
   conservative unknown/malformed/oversized prelude handling, cancellation.
2. Local HTTP mock integration: 503 then success; SSE preamble then delayed
   overload then success; persistent overload; non-overload; same request/key;
   protocol conversion and terminal usage/candidate state.
3. Run focused gateway regression tests and the release test gates. Commit only
   this change and its documentation/tests; preserve existing dirty files.
4. Push a new `backend-v*` tag, inspect the tag-triggered Actions jobs and release
   artifact digest. No local production build or canary.
5. Record current app digest/health and migration/backfill state. Pin the verified
   artifact digest and recreate **only app** once. Verify health, version/revision,
   unchanged database migration/backfill state and untouched dependencies.
6. Observe live traffic and retry events after release. Distinguish observed
   recoveries from behavior verified only with deterministic local fault tests.

## Local verification (2026-09-07)

All commands use `cargo test --locked --offline -p aether-gateway --lib <filter>
-- --test-threads=1`, loopback upstreams and in-memory repositories. These are
overlapping suites, not additive counts of unique tests.

| Filter | Result |
| --- | --- |
| `overload_retry` | 15 passed |
| `execution_runtime::stream` | 94 passed |
| `execution_runtime::sync` | 13 passed |
| `orchestration` | 131 passed |
| `prefetch` (same filter as the release workflow) | 21 passed |

Real HTTP fixtures verify a late SSE rejection after the HTTP response is
released, exact repeated headers/body, Responses/Chat/Claude conversion, one
candidate and final usage record, persistent rejection without duplicate terminal
errors, and no retry after pre-content downstream disconnect. Virtual-time tests
cover a 47-second opening and immediate data-line release without waiting for a
record terminator. No production inference request was used for these tests.
