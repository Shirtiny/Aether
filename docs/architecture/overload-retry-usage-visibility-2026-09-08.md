# Internal overload retries in usage records

## Scope

Expose Aether's existing same-plan overload retries in the admin usage list,
retry filter and request detail. This does not change retry eligibility, budgets,
provider routing, token delivery or billing. Sub2API rescue is a separate hop and
is explicitly excluded from these counters. The local Sub2API cancellation billing
patch is a separate unpublished task and is not modified here.

## Recording and privacy

- Each supported direct stream/sync plan creates a small in-memory audit handle.
  It stores at most three rejected attempts, the actual retry dispatch count,
  planned/completed wait durations and the first replay-closing reason/event type.
- The existing retry policy still permits only two extra requests. A wait that was
  cancelled before dispatch is not counted as a dispatched retry; unfinished waits
  have no actual duration. No prompt, output text, tool arguments, account secrets
  or response identity is stored in this audit.
- The first committing SSE record is categorized as content, reasoning, tool,
  empty output event, unknown/malformed event or terminal event. This is diagnostic
  only: empty/unknown records retain the current conservative replay policy.
- The snapshot is added to the existing terminal report context and persisted in
  `usage.request_metadata.internal_retry` and candidate `extra_data.internal_retry`.
  No independent background writer is introduced, so no new lifecycle/terminal
  ordering race is created. The client body closes before final bookkeeping.
- Existing metadata sanitation permits this bounded field even when body capture
  is disabled. PostgreSQL lightweight list projections retain it without loading
  request or response bodies. No database migration or backfill is required.

## Display semantics

- Version 1 metadata identifies scope `aether`, kind `same_plan_overload`.
- Candidate metadata is aggregated across actually attempted candidates, so later
  successful failover does not hide an earlier candidate's internal retries.
  Current usage metadata is a fallback, not an extra duplicate attempt.
- `has_retry` combines the existing candidate retry marker and internal dispatch
  counts, so the existing server-side retry filter includes both kinds.
- Final success/failure/cancellation/in-progress comes from the usage terminal
  status. `retry_recovered` is never used as proof of complete success.
- List rows show the count and final outcome; details show per-candidate failed
  attempts, waits, dispatch state and replay-window closing reason/event type.
- Missing or unsupported metadata is **not recorded**, not zero retries. A partly
  recorded candidate chain is labeled incomplete and its count is a lower bound.
  In-progress requests may remain unrecorded until terminal persistence.

## Validation / release

Coverage includes same-plan retry dispatch/window metadata, real HTTP recovery and
exhaustion persistence, no-replay after content, pre-content EOF, usage metadata
sanitation, previous-candidate retry aggregation, admin list filtering/detail,
and UI rendering of final failure versus recovery and unknown history.

Only local source/tests are changed. No production lifecycle, logging setting,
body capture, historical record modification, commit, tag or push is part of this
implementation task. Publication and production update require separate actions.

## Local verification results

- Gateway overload tests: 25 passed; stream/pump regression: 104 passed.
- Admin usage API regression: 38 passed, including filtering by internal retry
  across failed/selected candidates and identical list/detail aggregates.
- Gateway synchronous regression: 13 passed.
- Admin library: 157 passed; usage-runtime library: 167 passed.
- Frontend: 46 relevant tests, vue-tsc type check and Vite production build passed.
- New frontend files pass ESLint. The touched legacy RequestDetailDrawer already
  contains three unused-definition lint errors and existing style warnings; they
  are unrelated and were left unchanged. No claim of a clean repository-wide lint.
- Scoped Rust formatting and git whitespace checks pass. Suites overlap and must
  not be summed as a single unique-test count.

Verification evidence is local under `/var/tmp/aether-retry-visibility/`. This
feature and the separate Sub2API cancellation-billing fix remain unpublished;
no production changes or historical retry-data backfill were performed.
