# Missing stream media type and pre-content interruption recovery

## Evidence and scope

The 2026-09-08 incident identified by request prefix `84a217f5` opened an HTTP
200 stream, then recorded a capacity rejection. Saved headers lacked Content-Type
and prefetch was `skipped`. A same-user/model/time Sub2API log reported a missing
terminal event, not a structured overload. Response body capture was disabled;
the precise wire sequence and absence of substantive output are not established.
Regression fixtures below model the observed failure shape, not a captured replay.

## Changes

1. Aether inspects a bounded opening for known public text-stream protocols when
   the upstream media type is absent or misleading. SSE prefixes restore the SSE
   media type before downstream headers are committed. JSON errors in HTTP 200
   are recognized before passthrough and keep their actual error status/message.
   Unknown/binary content and private envelopes retain their existing behavior.
   No fixed inspection timer or whole-SSE-response buffer is added. The existing
   complete-JSON fallback for real Content-Length JSON responses is preserved
   only after positive JSON identification, never for mislabeled SSE.
2. The existing same-plan overload budget remains unchanged. Protocol inspection
   is applied to every attempt, including a retry with different response headers.
3. A missing required Responses terminal event is checked before conversion
   finalizers can fabricate a successful ending. Send a native terminal error once
   while preserving the existing partial-usage failure settlement. Pending
   streaming-lifecycle writes are joined after closing the client body and before
   terminal persistence, preventing a delayed refresh from reopening a failed row
   without putting persistence work on the first-token path.
4. Sub2API uses its existing single-rescue budget for recognized pre-content
   EOF/read/missing-terminal failures as well as overload. Heartbeat-only writes
   do not prevent the rescue. Cancellation, auth/policy/input/quota errors,
   substantive output, tool activity and unknown/malformed output prohibit replay.
   Exhaustion stops outer retry amplification and returns one real protocol error.

## Validation and release discipline

- Missing/wrong Content-Type + SSE preamble + delayed overload; JSON error without
  Content-Type; fragmented prefix/UTF-8; healthy first content released immediately.
- Missing terminal before/after content and after heartbeat, preserving failure
  and usage accounting without duplicate terminal events or fake completion.
  Large native terminal records use the existing observer's pre-EOF state, not
  just the bounded detector, to avoid rejecting valid >1 MiB completions.
- Sub2API receives the exact native error shapes emitted by Aether and also
  handles an older Aether's heartbeat-then-EOF with the same one-rescue budget.
- Mixed overload/interruption exhaustion, context cancellation and tool safety.
- No environment variables, migrations or production actions. Publish only new
  commits/tags when authorized; do not move the existing .114/.73 release tags.

## Local results (2026-09-08)

- Gateway stream/pump tests: 102 passed, including native terminal records larger
  than 1 MiB; overload policy tests: 2 passed.
- Usage-runtime tests: 166 passed. The six-case missing-terminal test also passed
  three consecutive runs after lifecycle-write ordering was fixed.
- Sub2API: 28 focused top-level service/handler tests, all 40 default backend
  test-bearing packages, focused race tests, and lint (0 issues).
- Touched Rust files pass rustfmt; both worktrees pass `git diff --check`.
- Repository-wide Rust checks are not clean on the baseline: full Clippy stops at
  the unchanged `aether-runtime-state::score_add_if_count_below` argument-count
  lint. `--no-deps` also stops at five unchanged gateway lints in codex_profile,
  pool-admin payloads, candidate runtime and catalog state. Full formatting reports
  existing unrelated files as well. No unrelated source or formatting fixes were
  made for this task; these full-check limitations remain explicit.

## Publication

The user authorized source commits, new tags and pushes on 2026-09-08. Release
pair: Aether `backend-v0.7.115` and Sub2API `cafecode-v0.0.74`. CI builds the images;
production deployment requires a separate explicit request. Existing release tags
are not moved. No migrations, service restarts or production configuration changes
are included. Existing unrelated dirty and untracked files are left intact.
