# Session Debug Runbook

This runbook traces an API request back to its client session, ingress hop,
Aether routing decision, and upstream provider result. It is intended for
incidents where a user reports a short request id, repeated HTTP 400/429/5xx
errors, or asks whether a session was created by the site itself.

The examples assume the production compose names used by this stack:

- Aether database: `aether-postgres`
- Aether database name: `aether`
- Gateway container: `aether-app`
- Optional upstream aggregator: `sub2api`

All timestamps from Postgres are `timestamptz`; in the current deployment they
are normally displayed as `+08`.

## 1. Expand The Request

Start with the reported request id. The UI may show only the first 8 chars, so
use a prefix search first.

```sh
docker exec aether-postgres psql -U postgres -d aether -x -c "
SELECT
  id,
  request_id,
  user_id,
  username,
  api_key_id,
  api_key_name,
  provider_name,
  provider_id,
  provider_endpoint_id,
  provider_api_key_id,
  model,
  target_model,
  request_type,
  api_format,
  is_stream,
  upstream_is_stream,
  status_code,
  error_category,
  status,
  error_message,
  response_time_ms,
  first_byte_time_ms,
  created_at,
  finalized_at,
  request_metadata
FROM usage
WHERE request_id LIKE '<short-id>%'
   OR id LIKE '<short-id>%'
ORDER BY created_at;
"
```

Then read the HTTP audit row. It carries client headers, provider headers, and
provider response headers. Body capture may be disabled; do not rely on body
columns being present.

```sh
docker exec aether-postgres psql -U postgres -d aether -x -c "
SELECT
  request_id,
  request_headers,
  provider_request_headers,
  response_headers,
  client_response_headers,
  request_body_ref,
  provider_request_body_ref,
  response_body_ref,
  client_response_body_ref,
  request_body_state,
  provider_request_body_state,
  response_body_state,
  client_response_body_state,
  body_capture_mode,
  created_at,
  updated_at
FROM usage_http_audits
WHERE request_id LIKE '<short-id>%'
ORDER BY created_at;
"
```

Read candidate rows to see whether the failure was local, transport-level, or
from the upstream provider. `extra_data.upstream_response` and
`extra_data.error_flow` are usually the fastest path to the real cause.

```sh
docker exec aether-postgres psql -U postgres -d aether -x -c "
SELECT
  id,
  request_id,
  user_id,
  username,
  api_key_id,
  api_key_name,
  candidate_index,
  retry_index,
  provider_id,
  endpoint_id,
  key_id,
  status,
  status_code,
  error_type,
  error_message,
  latency_ms,
  extra_data,
  created_at,
  started_at,
  finished_at
FROM request_candidates
WHERE request_id LIKE '<short-id>%'
ORDER BY candidate_index, retry_index, created_at;
"
```

## 2. Extract The Session Key

Aether normalizes known client session affinity under:

```text
usage.request_metadata.client_session_affinity.session_key
```

For Codex-style requests, this is normally `session=<uuid>`. New WS usage rows
persist this value directly. Historical rows may only have it in the selected
request candidate, so always calculate the effective value with the same
precedence used by the usage list and filters.

```sh
docker exec aether-postgres psql -U postgres -d aether -c "
SELECT
  u.request_id,
  NULLIF(BTRIM(
    u.request_metadata::jsonb #>> '{client_session_affinity,session_key}'
  ), '') AS usage_session_key,
  NULLIF(BTRIM(
    candidate.extra_data::jsonb #>> '{client_session_affinity,session_key}'
  ), '') AS candidate_session_key,
  COALESCE(
    NULLIF(BTRIM(u.request_metadata::jsonb #>> '{client_session_affinity,session_key}'), ''),
    NULLIF(BTRIM(candidate.extra_data::jsonb #>> '{client_session_affinity,session_key}'), '')
  ) AS effective_session_key,
  COALESCE(
    NULLIF(BTRIM(u.request_metadata::jsonb #>> '{client_session_affinity,client_family}'), ''),
    NULLIF(BTRIM(u.request_metadata->>'client_family'), ''),
    NULLIF(BTRIM(candidate.extra_data::jsonb #>> '{client_session_affinity,client_family}'), '')
  ) AS effective_client_family,
  u.request_metadata->>'cafecode_uid' AS cafecode_uid,
  u.request_metadata->>'cafecode_uname' AS cafecode_uname,
  u.request_metadata->>'client_ip' AS client_ip,
  u.request_metadata->>'user_agent' AS user_agent
FROM usage u
LEFT JOIN usage_routing_snapshots routing
  ON routing.request_id = u.request_id
LEFT JOIN request_candidates candidate
  ON candidate.id = routing.candidate_id
WHERE u.request_id = '<full-request-id>';
"
```

Use `effective_session_key` for the remaining session queries. Keep
`usage_session_key` separate when diagnosing candidate cleanup: only a value
persisted on usage can survive after both routing candidate details and their
affinity metadata are removed.

If `effective_session_key` is missing, inspect raw client metadata in
`x-codex-turn-metadata`:

```sh
docker exec aether-postgres psql -U postgres -d aether -x -c "
SELECT
  u.request_id,
  u.created_at,
  a.request_headers->>'originator' AS originator,
  a.request_headers->>'user-agent' AS user_agent,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'session_id' AS session_id,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'thread_id' AS thread_id,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'turn_id' AS turn_id,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'thread_source' AS thread_source,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'workspace_kind' AS workspace_kind,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'turn_started_at_unix_ms' AS turn_started_ms,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->'workspaces' AS workspaces
FROM usage u
LEFT JOIN usage_http_audits a ON a.request_id = u.request_id
WHERE u.request_id = '<full-request-id>';
"
```

Use this distinction in reports:

- `session_id` / `thread_id`: created by the client.
- `client_session_affinity.session_key`: Aether's normalized grouping key.
- `effective_session_key`: usage session key with a non-empty candidate fallback
  for legacy rows.
- `request_id`: a single HTTP request through Aether.
- `turn_id`: a client turn inside the session/thread.

The write path, prompt-summary inheritance layers, identity boundaries, and
first-phase limitations are documented in
[WebSocket Usage Session Identity and Prompt Summaries](../architecture/ws-usage-session-observability.md).

## 3. Build The Session Timeline

Once the session key is known, summarize the whole session.

The effective-session expression may scan usage and candidate metadata on large
datasets. In production diagnostics, add a known `u.user_id`/`u.api_key_id` and
`u.created_at` window inside each `session_usage` CTE whenever possible. Do not
run an unbounded historical session scan as a routine dashboard query.

```sh
docker exec aether-postgres psql -U postgres -d aether -c "
WITH session_usage AS (
  SELECT u.*
  FROM usage u
  LEFT JOIN usage_routing_snapshots routing
    ON routing.request_id = u.request_id
  LEFT JOIN request_candidates candidate
    ON candidate.id = routing.candidate_id
  WHERE COALESCE(
    NULLIF(BTRIM(u.request_metadata::jsonb #>> '{client_session_affinity,session_key}'), ''),
    NULLIF(BTRIM(candidate.extra_data::jsonb #>> '{client_session_affinity,session_key}'), '')
  ) = '<session-key>'
)
SELECT
  count(*) AS total,
  min(created_at) AS first_at,
  max(created_at) AS last_at,
  count(*) FILTER (WHERE status_code = 400) AS status_400,
  min(created_at) FILTER (WHERE status_code = 400) AS first_400_at,
  max(created_at) FILTER (WHERE status_code = 400) AS last_400_at
FROM session_usage;
"
```

List the timeline. Keep the error text short in the first pass.

```sh
docker exec aether-postgres psql -U postgres -d aether -c "
WITH session_usage AS (
  SELECT u.*
  FROM usage u
  LEFT JOIN usage_routing_snapshots routing
    ON routing.request_id = u.request_id
  LEFT JOIN request_candidates candidate
    ON candidate.id = routing.candidate_id
  WHERE COALESCE(
    NULLIF(BTRIM(u.request_metadata::jsonb #>> '{client_session_affinity,session_key}'), ''),
    NULLIF(BTRIM(candidate.extra_data::jsonb #>> '{client_session_affinity,session_key}'), '')
  ) = '<session-key>'
)
SELECT
  to_char(created_at, 'YYYY-MM-DD HH24:MI:SS TZ') AS created_at,
  request_id,
  model,
  api_format,
  is_stream,
  status_code,
  status,
  error_category,
  left(coalesce(error_message, ''), 160) AS error,
  request_metadata->>'client_ip' AS client_ip,
  request_metadata->>'user_agent' AS user_agent
FROM session_usage
ORDER BY created_at;
"
```

Group by status and provider to avoid misreading a mixed session.

```sh
docker exec aether-postgres psql -U postgres -d aether -c "
WITH session_usage AS (
  SELECT u.*
  FROM usage u
  LEFT JOIN usage_routing_snapshots routing
    ON routing.request_id = u.request_id
  LEFT JOIN request_candidates candidate
    ON candidate.id = routing.candidate_id
  WHERE COALESCE(
    NULLIF(BTRIM(u.request_metadata::jsonb #>> '{client_session_affinity,session_key}'), ''),
    NULLIF(BTRIM(candidate.extra_data::jsonb #>> '{client_session_affinity,session_key}'), '')
  ) = '<session-key>'
)
SELECT
  provider_name,
  provider_id,
  provider_endpoint_id,
  provider_api_key_id,
  status_code,
  count(*) AS n,
  min(created_at) AS first_at,
  max(created_at) AS last_at
FROM session_usage
GROUP BY provider_name, provider_id, provider_endpoint_id, provider_api_key_id, status_code
ORDER BY first_at;
"
```

If a user asks "when did it start returning 400", answer with:

- first `status_code = 400` timestamp
- first 400 request id
- last 400 timestamp
- count of 400s in the same session
- whether any non-400 responses happened in the same session

## 4. Decide Whether The Site Created The Session

The session is site-created only if the evidence points to an Aether/frontend
page session. For API clients, the client normally creates the session and
Aether only records it.

Check these fields:

```sh
docker exec aether-postgres psql -U postgres -d aether -c "
SELECT
  to_char(u.created_at, 'YYYY-MM-DD HH24:MI:SS TZ') AS created_at,
  u.request_id,
  u.status_code,
  a.request_headers->>'originator' AS client_originator,
  a.request_headers->>'user-agent' AS client_ua,
  a.request_headers->>'host' AS host,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'session_id' AS client_session_id,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'thread_id' AS client_thread_id,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'thread_source' AS thread_source,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->>'workspace_kind' AS workspace_kind,
  (a.request_headers->>'x-codex-turn-metadata')::jsonb->'workspaces' AS workspaces,
  a.provider_request_headers->>'originator' AS upstream_originator,
  a.provider_request_headers->>'user-agent' AS upstream_ua,
  a.provider_request_headers->>'conversation_id' AS upstream_conversation_id,
  a.provider_request_headers->>'session_id' AS upstream_session_id
FROM usage u
JOIN usage_http_audits a ON a.request_id = u.request_id
WHERE u.request_id = '<full-request-id>';
"
```

Interpretation:

- `originator = Codex Desktop`, `user-agent = Codex Desktop/...`, and
  `workspace_kind = project` means the session came from a local Codex client.
- A browser-originated site session should have browser user-agent and site
  frontend routes or auth-session evidence, not Codex workspace metadata.
- `host = aether-app:80` only means the request reached the app inside Docker;
  it does not prove the browser used the site.
- `provider_request_headers.conversation_id` or upstream `session_id` belongs
  to the provider-side account/session, not necessarily the user's client
  session.

For UUIDv7-like Codex ids, the first 48 bits encode the creation time. Use this
only as supporting evidence; database `created_at` is the authoritative time
Aether received a request.

```sh
python3 - <<'PY'
from datetime import datetime, timezone, timedelta

for value in ["<session-or-turn-id>"]:
    ms = int(value.replace("-", "")[:12], 16)
    dt = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    print(value, dt.isoformat(), (dt + timedelta(hours=8)).isoformat())
PY
```

## 5. Identify The Ingress Hop

`request_metadata.client_ip` and gateway access logs usually show the immediate
peer, not the end user's public IP. In this stack that peer is often `sub2api`.

Map Docker IPs to containers:

```sh
docker inspect aether-app sub2api caddy --format '{{.Name}} {{range $name,$net := .NetworkSettings.Networks}}{{$name}}={{$net.IPAddress}} {{end}}'
```

If the peer is `sub2api`, the chain is:

```text
external client -> sub2api -> aether-app -> provider
```

Use sub2api logs to validate the request before it entered Aether:

```sh
docker logs sub2api \
  --since '<UTC-start-time>' \
  --until '<UTC-end-time>' \
  --tail 5000
```

Look for:

- `path`
- `method`
- `user_id`
- `api_key_id`
- `group_id`
- `model`
- `stream`
- `account_id`
- `client_request_id`
- upstream error text

Use Aether logs for the same window when you need access-log confirmation:

```sh
docker logs aether-app \
  --since '<UTC-start-time>' \
  --until '<UTC-end-time>' \
  --tail 5000
```

In Aether access logs, match:

- `trace_id` / `request_id`
- `remote_addr`
- `method`
- `path`
- `user_id`
- `api_key_id`
- `status_code`
- `execution_path`
- `elapsed_ms`

## 6. Inspect Provider And Key Routing

After a failing request is identified, inspect the selected provider, endpoint,
and provider key. Treat provider key names and account identifiers as sensitive
in external reports.

```sh
docker exec aether-postgres psql -U postgres -d aether -x -c "
SELECT
  p.id AS provider_id,
  p.name AS provider_name,
  p.provider_type,
  p.is_active AS provider_active,
  e.id AS endpoint_id,
  e.name AS endpoint_name,
  e.api_format,
  e.base_url,
  e.custom_path,
  e.proxy AS endpoint_proxy,
  e.body_rules,
  k.id AS key_id,
  k.name AS key_name,
  k.proxy AS key_proxy
FROM providers p
JOIN provider_endpoints e ON e.provider_id = p.id
JOIN provider_api_keys k ON k.provider_id = p.id
WHERE p.id = '<provider-id>'
  AND e.id = '<provider-endpoint-id>'
  AND k.id = '<provider-api-key-id>';
"
```

For session-wide provider behavior, aggregate request candidates:

```sh
docker exec aether-postgres psql -U postgres -d aether -c "
SELECT
  rc.provider_id,
  rc.endpoint_id,
  rc.key_id,
  rc.status,
  rc.status_code,
  rc.error_type,
  count(*) AS n,
  min(rc.created_at) AS first_at,
  max(rc.created_at) AS last_at
FROM request_candidates rc
JOIN usage u ON u.request_id = rc.request_id
LEFT JOIN usage_routing_snapshots affinity_routing
  ON affinity_routing.request_id = u.request_id
LEFT JOIN request_candidates affinity_candidate
  ON affinity_candidate.id = affinity_routing.candidate_id
WHERE COALESCE(
  NULLIF(BTRIM(u.request_metadata::jsonb #>> '{client_session_affinity,session_key}'), ''),
  NULLIF(BTRIM(affinity_candidate.extra_data::jsonb #>> '{client_session_affinity,session_key}'), '')
) = '<session-key>'
GROUP BY rc.provider_id, rc.endpoint_id, rc.key_id, rc.status, rc.status_code, rc.error_type
ORDER BY first_at;
"
```

## 7. Report Template

Use this concise structure when handing results back:

```text
Session: <session-key>
Client: <originator> / <user-agent>
User: <aether-username or user_id>, Cafecode <uname>/<uid>
Ingress: <client_ip> -> <container if known>
Path: <method> <path>, model=<model>, stream=<true|false>
Provider: <provider_name> / <base_url or upstream_url>

First seen in Aether: <timestamp>, request_id=<id>
First <status> in session: <timestamp>, request_id=<id>
Last <status> in session: <timestamp>
Session counts: total=<n>, <status>=<n>, success=<n>

Cause: <upstream/local error summary>
Site-created session? <yes/no/unknown>, because <evidence>
```

Be explicit about the boundary:

- "Aether first saw the session at ..." means database `usage.created_at`.
- "The client session id appears to have been created at ..." means derived
  from the client id timestamp, if applicable.
- "Ingress source" means Aether's immediate peer, unless trusted forwarded
  headers or upstream logs prove the external client IP.

## 8. Common Conclusions

- Repeated identical 400s with the same `session_key`, `model`, provider, and
  upstream error are usually one client session retrying the same bad state.
- A single 200 inside an otherwise failing session often means a different
  provider/key path succeeded; group by provider before concluding the whole
  session is broken.
- `invalid_encrypted_content` from a provider is an upstream client/session
  payload problem unless Aether body rules modified the encrypted field.
- `sub2api` as `client_ip` means Aether did not receive the request directly
  from the end user.
- Codex `originator` and `x-codex-turn-metadata.workspaces` mean the session
  was produced by a local Codex client, not by the Aether web UI.

## 9. OpenAI Responses Non-Stream Regression Note

Recorded 2026-06-26 after investigating repeated standard OpenAI Chat failures
for `gpt-5.4`.

Observed incident:

- Client requests were standard OpenAI Chat sync requests
  (`api_format=openai:chat`, `client_requested_stream=false`).
- Failing attempts were cross-format routed to OpenAI Responses providers with
  `endpoint_api_format=openai:responses` and `upstream_is_stream=false`.
- Representative failure `967b0f82-e9eb-4220-a865-30775ac2c0f9` used
  `G-aisc` endpoint `bfd0cf11-9915-460f-b1cb-858631076029` and upstream
  `/v1/responses`.
- The upstream HTTP status was 200, but the upstream body was error-shaped:
  `{"error":{"message":"Upstream stream ended with an error","type":"server_error"}}`.
- A direct probe using the captured provider request body, with only
  `stream:true` added and `Accept: text/event-stream`, completed successfully:
  `response.completed`, no error, usage `5520/635/6155`.

Important timeline:

- From 2026-06-20 through 2026-06-24, non-stream OpenAI Responses conversion
  was heavily used and normally successful. Example aggregate:
  `G-aisc/openai:responses/upstream_is_stream=false` had thousands of completed
  requests before 2026-06-25.
- For the same user/model, `G-aisc/openai:responses/upstream_is_stream=false`
  was still succeeding at 2026-06-25 16:42 +08.
- Starting around 2026-06-25 16:56 +08, non-stream OpenAI Responses requests
  began failing broadly with `Upstream stream ended with an error`. This was not
  isolated to one user.
- After endpoint configs were changed on 2026-06-26 to force upstream stream for
  affected Responses endpoints, `G-aisc/openai:responses/upstream_is_stream=true`
  requests completed successfully.

Current interpretation:

- The direct cause is the upstream/provider `/v1/responses` non-stream path
  returning HTTP 200 with an embedded error body.
- The local mitigation is to set affected OpenAI Responses endpoints to
  `upstream_stream_policy=force_stream`; downstream can still receive a sync
  OpenAI Chat response after aggregation.
- There is an unresolved suspicion that a 2026-06-25 gateway update contributed
  to exposing or triggering the failure, because the breakage started after that
  update window. However, code history did not show a direct change to
  `openai:chat -> openai:responses` request conversion, `upstream_stream_policy`
  parsing, or default `upstream_is_stream` resolution in that window.
- If this resurfaces, compare provider request body/headers before and after
  2026-06-25 16:56 +08, and check whether candidate/session/failover changes
  caused affected traffic to remain on non-stream Responses providers rather
  than using another working candidate.
