# Sticky And Profile

Last updated: 2026-07-07

This document records the Aether sticky-session contract for Codex traffic first. Profile/fingerprint handling is intentionally left as a follow-up section until the implementation is broader than stable client headers.

## Sticky

### Terms

- **official Codex session**: the Codex root session identity supplied by the client in official Codex metadata. Prefer `session_id`; use `thread_id` only when `session_id` is absent.
- **thread id**: the Codex thread identity for the current thread/turn. In normal turns it may match the main conversation shape; in subagent/guardian flows it can differ from the root session.
- **provider short session**: Aether/OpenAI-provider compatibility values such as underscored `session_id`, `conversation_id`, or a short hash derived from `prompt_cache_key`. These are not the official Codex session identity when official metadata exists.
- **ChatGPT account id**: the account id of the Aether-selected pool key/auth config. This belongs to the provider account selected by Aether, not to the inbound user request.
- **sticky token**: the token used by Aether provider-pool runtime to bind a client session to a pool key. For Codex this is currently `session=<official Codex session>`.

### Official Codex Source Basis

Verified first against the local OpenAI Codex source checkout at `/opt/stacks/openai-codex` commit `d2885dc`.
Re-checked against an official `openai/codex` `main` snapshot on 2026-07-07, commit `42156ba007278d9068f1518ac1f627b56c136ef6`. This is a pinned comparison snapshot, not a claim that the document continuously tracks the latest upstream Codex commit.

- `codex-rs/core/src/responses_metadata.rs`
  - `client_metadata["x-codex-turn-metadata"]` is documented as the canonical transport for the full turn metadata blob.
  - Flat `client_metadata` keys and direct HTTP/WebSocket headers are compatibility projections, not independent sources of truth.
  - `CodexResponsesMetadata` carries `installation_id`, `session_id`, `thread_id`, `window_id`, parent/subagent/thread-source fields, sandbox/workspace data, and turn timing.
  - `client_metadata()` emits `x-codex-installation-id`, flat `session_id`, flat `thread_id`, `x-codex-window-id`, and optional `x-codex-turn-metadata`.
- `codex-rs/codex-api/src/requests/headers.rs`
  - Official HTTP session headers are dash-form `session-id` and `thread-id`.
- `codex-rs/core/src/client.rs`
  - Responses requests include `client_metadata: responses_metadata.client_metadata()`.
  - Responses requests extend headers with `session-id` and `thread-id`.
  - Default `prompt_cache_key` is the current `thread_id` unless overridden.
- `codex-rs/core/src/guardian/review_session.rs`
  - Guardian/review sessions can override `prompt_cache_key` to `guardian:<parent_thread_id>`.
  - Therefore `prompt_cache_key` is useful as a compatibility/fallback signal, but it is not the canonical source when official turn metadata is present.

### Requirement Intent

Users are using Aether's Codex pool. They generally do not have, and should not be required to provide, the ChatGPT/OpenAI account id for the upstream account.

The sticky contract must therefore separate two identities:

- The client contributes the official Codex session identity.
- Aether selects the provider pool key/account and owns the upstream `chatgpt-account-id`.

Consequences:

- Do not use inbound `chatgpt-account-id` as the Codex sticky account dimension.
- Do not require inbound `chatgpt-account-id` for Codex sticky behavior.
- Use official Codex session metadata as the stable client-session anchor.
- Let provider-pool sticky runtime map that session anchor to the actual selected pool key.
- Keep upstream `chatgpt-account-id` injection tied to the selected key/auth config, not to user headers.

### Current Aether Implementation

#### Codex Session Extraction

Codex client affinity is built in `apps/aether-gateway/src/client_session_affinity.rs`.

Codex detection currently uses:

- `user-agent` containing `codex`
- `originator` containing `codex`
- exact `session-id`
- exact `thread-id`
- any `x-codex-` header
- body `client_metadata` containing `x-codex-turn-metadata`, `x-codex-installation-id`, or `x-codex-window-id`

It no longer detects Codex only because of inbound `chatgpt-account-id`.

Codex root session extraction order:

1. body `client_metadata["x-codex-turn-metadata"]`, parsed as JSON, `session_id` first then `thread_id`
2. body flat `client_metadata.session_id`, then `client_metadata.thread_id`
3. header `x-codex-turn-metadata`, parsed as JSON, `session_id` first then `thread_id`
4. official dash headers `session-id`, then `thread-id`
5. compatibility headers `session_id`, then `conversation_id`
6. generic body fallback, including `prompt_cache_key`, `conversation_id`, `session_id`, and metadata/conversation-state variants

For Codex, `account_hint` is currently `None`; inbound `chatgpt-account-id` is ignored for affinity and sticky.

When the Codex session value is sourced from body fallback and starts with `guardian:`, Aether strips the prefix when building the scheduler session key. Explicit Aether session headers preserve their literal value.

#### Codex Sticky Token

Sticky token construction is in `apps/aether-gateway/src/ai_serving/planner/mod.rs`.

For Codex traffic:

```text
sticky token = session=<official Codex session>
```

Codex affinity is consulted before generic body sticky extraction, so body/provider compatibility values such as short `session_id`, `conversation_id`, or `prompt_cache_key` do not override official Codex metadata.

If a historical/compatibility affinity key has `account=...;session=...`, Codex sticky strips it down to `session=...`. This preserves the current requirement that the selected ChatGPT account belongs to Aether's pool, not the inbound request.

For non-Codex traffic, the generic sticky order is unchanged.

#### Provider-Pool Runtime Binding

The sticky token is passed to candidate selection before a concrete pool key is chosen.

Provider-pool runtime stores sticky bindings under:

```text
ap:{provider_id}:sticky:{sticky_token}
```

The value is the bound pool `key_id`.

Runtime behavior:

- If a sticky binding exists and the bound key is still eligible, schedule that key.
- If no sticky binding exists, the scheduler proceeds through normal pool selection.
- A sticky init lock/prebind protects the first request for a new session from racing with parallel requests.
- On successful execution, Aether publishes the sticky binding with the pool sticky TTL.
- If the bound key becomes ineligible, scheduler records the ineligible reason and a successful fallback can move the sticky binding away from the old key.

#### Upstream `chatgpt-account-id`

Upstream Codex/OpenAI Responses request header handling is in `crates/aether-ai-formats/src/formats/openai/responses/codex.rs`.

If the original request and provider-request headers do not already contain `chatgpt-account-id`, Aether extracts `account_id` from the decrypted auth config for the selected provider key and inserts:

```text
chatgpt-account-id: <selected pool account_id>
```

This header is for the upstream provider request. It is not the source of Codex sticky identity.

### Current Validation

The current sticky/session behavior is covered by:

```bash
cargo test -p aether-gateway --lib client_session_affinity
cargo test -p aether-gateway --lib pool_sticky_session_token
cargo test -p aether-gateway --lib codex
```

As of this document update, all three pass.

### Known Boundaries

- The Codex sticky token is currently namespaced by provider id through the runtime key and by official Codex session through the token. It is not additionally namespaced by inbound `chatgpt-account-id`.
- The current implementation does not use inbound `chatgpt-account-id` for Codex sticky, by design.
- If future multi-tenant isolation requires a stronger namespace than provider id plus official session, add an Aether-owned namespace such as API-key/user/tenant identity, not the user-supplied `chatgpt-account-id`.

## Profile

This section records the official Codex client-feature inventory that should shape Aether's Codex profile work.

Source snapshot: local official OpenAI Codex checkout `/opt/stacks/openai-codex` at commit `d2885dc`.
Pinned source comparison snapshot: official `openai/codex` `main` at commit `42156ba007278d9068f1518ac1f627b56c136ef6`, checked on 2026-07-07.

### Profile Scope

Do not treat `profile` as only `user-agent`. Official Codex has several separate surfaces:

- **HTTP/WebSocket client headers**: transport-visible request headers such as `user-agent`, `originator`, `session-id`, `thread-id`, `x-codex-window-id`, `x-codex-turn-metadata`, auth headers, and optional attestation.
- **Responses body metadata**: `client_metadata`, especially canonical `client_metadata["x-codex-turn-metadata"]`.
- **Prompt-visible instructions and context**: base instructions plus developer/user context fragments, including permissions, collaboration/personality, apps/plugins/skills, hooks, AGENTS.md, cwd/shell/date/timezone/network/filesystem/subagents, and context diffs.
- **Auth/account routing identity**: `Authorization`, `ChatGPT-Account-ID`, `X-OpenAI-Fedramp`; these come from the selected auth/provider account.
- **Telemetry/analytics/side-channel metadata**: OpenTelemetry session telemetry, product analytics, local thread-store metadata, and remote-control metadata. These are not the same as upstream model request profile.
- **Transport/TLS profile**: HTTP client behavior, proxy/custom CA, HTTP/WS mode. This is separate from Codex request metadata.

### Official Client Headers

Official default client basis is in `codex-rs/login/src/auth/default_client.rs`.

- Default `originator` is `codex_cli_rs`.
- `CODEX_INTERNAL_ORIGINATOR_OVERRIDE` can override it.
- First-party originator values recognized in source include `codex_cli_rs`, `codex-tui`, `codex_vscode`, `Codex ...`, and chat-specific `codex_atlas` / `codex_chatgpt_desktop`.
- Official `User-Agent` format is derived from:
  - originator
  - Codex package version
  - OS type
  - OS version
  - CPU architecture
  - terminal token
  - optional suffix
- Default reqwest headers include `originator`, `user-agent`, and optional `x-openai-internal-codex-residency`.

Terminal token source is `codex-rs/terminal-detection/src/lib.rs`.

- It reads `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `WEZTERM_VERSION`, `ITERM_SESSION_ID`, `ITERM_PROFILE`, `ITERM_PROFILE_NAME`, `TERM_SESSION_ID`, `KITTY_WINDOW_ID`, `ALACRITTY_SOCKET`, `KONSOLE_VERSION`, `GNOME_TERMINAL_SCREEN`, `VTE_VERSION`, `WT_SESSION`, `TERM`, `TMUX`, `TMUX_PANE`, `ZELLIJ`, `ZELLIJ_SESSION_NAME`, and `ZELLIJ_VERSION`.
- If inside tmux, it may run `tmux display-message` to discover the underlying client terminal.
- If inside zellij and env version is absent, it may run `zellij --version`.
- The terminal token is sanitized before entering `User-Agent`.

Aether implication: a real Codex header profile should model `originator + version + OS + arch + terminal token`, not a free-form UA string alone.

### Official Request Identity

Canonical turn metadata is implemented in `codex-rs/core/src/responses_metadata.rs`.

- The canonical source is `client_metadata["x-codex-turn-metadata"]`.
- Flat `client_metadata` keys and direct headers are compatibility projections of the same snapshot.
- `client_metadata()` emits:
  - `x-codex-installation-id`
  - flat `session_id`
  - flat `thread_id`
  - `x-codex-window-id`
  - optional `turn_id`
  - optional `x-openai-subagent`
  - optional `x-codex-parent-thread-id`
  - optional `x-codex-turn-metadata`
- `compatibility_headers()` emits direct headers:
  - `x-codex-window-id`
  - optional `x-codex-turn-metadata`
  - optional `x-codex-parent-thread-id`
  - optional `x-openai-subagent`

HTTP session headers are from `codex-rs/codex-api/src/requests/headers.rs`:

- `session-id`
- `thread-id`

Other request headers from `codex-rs/core/src/client.rs` include:

- `x-client-request-id`, using the thread id on some Responses paths.
- `x-codex-installation-id` on compact/unary paths.
- `x-codex-turn-state`.
- `x-codex-beta-features`.
- `OpenAI-Beta`.
- `x-openai-memgen-request` for memory consolidation.
- `x-responsesapi-include-timing-metrics`.
- `x-openai-internal-codex-responses-lite` / matching WS client-metadata key.
- `x-codex-ws-stream-request-start-ms` in WS client metadata.
- W3C trace metadata in WebSocket `client_metadata` as `ws_request_header_traceparent` and `ws_request_header_tracestate`.

Attestation is in `codex-rs/core/src/attestation.rs` and `codex-rs/core/src/client.rs`.

- Header name is `x-oai-attestation`.
- It is generated only when the model provider supports attestation and an attestation provider is configured.
- Generation context contains the current `thread_id`.

### ID Lifecycle

`installation_id`:

- `codex-rs/core/src/installation_id.rs` resolves it from `${CODEX_HOME}/installation_id`.
- Existing valid UUID is reused.
- Missing/invalid value is replaced by a new UUIDv4.

`thread_id`:

- `codex-rs/protocol/src/thread_id.rs` defines Codex-generated thread IDs as UUIDv7.
- New/cleared/forked sessions default to a new `ThreadId`.
- Resumed sessions reuse the rollout conversation id.

`session_id`:

- `codex-rs/protocol/src/session_id.rs` defines session IDs as UUIDv7 and allows conversion to/from `ThreadId`.
- In `codex-rs/core/src/session/session.rs`, root sessions normally use `SessionId::from(thread_id)`.
- Resumed sessions can reuse the persisted `session_id`.
- Non-root agents can inherit the root `AgentControl` session id instead of using their own thread id.

`window_id`:

- Direct request header/client-metadata `x-codex-window-id` currently uses `thread_id:window_number` from `current_window_id()`.
- Auto-compaction also tracks UUIDv7 `first_window_id`, `previous_window_id`, and `window_id` in compacted metadata.
- Therefore profile logic must distinguish the direct compatibility `x-codex-window-id` string from compaction window UUID lineage.

### Turn Metadata Fields

`x-codex-turn-metadata` can include:

- `installation_id`
- `session_id`
- `thread_id`
- `turn_id`
- `window_id`
- `request_kind`
- `forked_from_thread_id`
- `parent_thread_id`
- `subagent_kind`
- `thread_source`
- `sandbox`
- `workspaces`
- `turn_started_at_unix_ms`
- `compaction`
- filtered extra Codex client metadata

Workspace/git enrichment is in `codex-rs/core/src/turn_metadata.rs`.

- It derives `repo_root` from cwd.
- For git workspaces it can include associated remote URLs, latest commit hash, and whether there are changes.
- It also includes sandbox tagging derived from permission profile, Windows sandbox level, and managed-network enforcement.

### Official Prompt And Context Assembly

The normal Responses turn prompt is assembled from session state, conversation history, model-visible context fragments, tools, and base instructions. It is not just a request header/body profile.

High-level request build path:

- `codex-rs/core/src/session/turn.rs` calls `record_context_updates_and_set_reference_context_item(...)`, records user and injected context items into history, then sends `clone_history().for_prompt(...)` to sampling.
- `codex-rs/core/src/prompt_debug.rs` uses the same sequence for prompt inspection: new turn, capture step context, record context updates, record user input, clone history, build tools, get base instructions, then `build_prompt(...)`.
- `build_prompt(...)` returns `Prompt { input, tools, parallel_tool_calls, base_instructions, output_schema, output_schema_strict }`.
- `codex-rs/core/src/client.rs` sends `prompt.base_instructions.text` as the Responses `instructions` field on the normal path. On `responses_lite`, it inserts the same text as a developer message at the front of `input` instead.

Base instructions are resolved in `codex-rs/core/src/session/mod.rs` with this priority:

1. `config.base_instructions`
2. conversation history `session_meta.base_instructions`
3. current model instructions from `model_info.get_model_instructions(config.personality)`

Initial context assembly is in `Session::build_initial_context_with_world_state_and_mcp(...)` in `codex-rs/core/src/session/mod.rs`. It can add:

- permissions instructions from `codex-rs/prompts/src/permissions_instructions.rs`, role `developer`, markers `<permissions instructions>...</permissions instructions>`;
- configured `developer_instructions`;
- collaboration mode developer instructions from `codex-rs/core/src/context/collaboration_mode_instructions.rs`;
- realtime update/personality/app/plugin/skill/token-budget/multi-agent instructions;
- extension-provided developer/user/separate-developer fragments;
- full world-state context fragments.

Context updates after the initial baseline are handled by `record_context_updates_and_set_reference_context_item(...)` in `codex-rs/core/src/session/mod.rs`. The first turn injects full initial context and stores a world-state baseline; later turns append settings/world-state diffs plus turn-scoped extension context when needed.

`codex-rs/core/src/context/mod.rs` lists the model-visible context fragment families: permissions, apps, plugins, skills, collaboration mode, personality, hooks, current time reminders, token-budget context, subagent notifications, AGENTS.md/user instructions, shell-command fragments, world state, and related update/reminder fragments.

Normal model-visible environment context is a subset of that world state. It is in `codex-rs/core/src/context/world_state/environment.rs` and renders:

- `cwd`
- optional `status`
- `shell`
- `current_date`
- `timezone`
- network context
- filesystem/permission-profile context
- subagents
- selected multi-environment state

`AGENTS.md` instructions are model-visible user fragments from `codex-rs/core/src/context/user_instructions.rs` with markers `# AGENTS.md instructions` and `</INSTRUCTIONS>`.

In the normal Responses prompt/context path, I did not find hostname, local IP, public IP, or OS username rendered as a standard environment field. The fields found there are working-directory/shell/date/timezone/permission/network/subagent/workspace context, plus project/user instructions and extension context.

Realtime prompt has a separate path in `codex-rs/core/src/realtime_prompt.rs`:

- it replaces a first-name placeholder with `whoami::realname()` or `whoami::username()`;
- this is realtime backend prompt behavior, not the normal Codex Responses turn metadata/profile path.

Aether implication: prompt-visible context is user/project/session scoped. It must not be frozen as an account pool profile, and it must not be normalized to the selected ChatGPT account. Aether should preserve official Codex prompt context when proxying official requests. Only a future explicit synthetic-client mode should generate Codex-like prompt context for non-Codex clients, and that generated context should be scoped to the Aether user/session/project, not to the pool account.

### Auth And Account Headers

Official auth provider behavior is in `codex-rs/model-provider/src/auth.rs` and `codex-rs/model-provider/src/bearer_auth_provider.rs`.

- `BearerAuthProvider` sends `Authorization: Bearer <token>` when a token exists.
- It sends `ChatGPT-Account-ID` when `account_id` exists.
- It sends `X-OpenAI-Fedramp: true` for FedRAMP accounts.
- `AgentIdentityAuthProvider` similarly sends generated `Authorization`, `ChatGPT-Account-ID`, and optional FedRAMP header.

Account id source is official auth state, not arbitrary request metadata:

- `codex-rs/login/src/auth/manager.rs` exposes `get_account_id()`.
- ChatGPT token auth reads token data `account_id` or id-token `chatgpt_account_id`.
- Personal access token and agent identity auth expose their own account ids.

Aether implication: for Aether pool traffic, `ChatGPT-Account-ID` must come from the selected pool key/auth config, not from the inbound user request. Inbound users generally do not have Aether's pool account id.

### Telemetry And Side Channels

Official telemetry/session metadata must be documented, but it must not be merged blindly into upstream Responses request profile.

`codex-rs/core/src/session/session.rs` and `codex-rs/otel/src/events/session_telemetry.rs` build session telemetry with:

- conversation/thread id
- auth mode
- auth environment presence flags
- account id/email
- originator
- service name
- session source
- model/slug/service tier/reasoning effort
- app version
- terminal type
- optional user-prompt logging flag

Session startup in `codex-rs/core/src/session/session.rs` takes account id/email from official auth state, originator from session configuration, terminal type from terminal detection, and auth environment presence flags from `codex-rs/login/src/auth_env_telemetry.rs`.

Auth environment telemetry records presence/bucketed state, not secret values:

- `OPENAI_API_KEY` present
- `CODEX_API_KEY` present/enabled
- provider env key configured/present
- refresh-token override present

`SessionTelemetry::conversation_starts(...)` records provider name, auth env flags, reasoning effort/summary, context window, auto-compact limit, approval policy, sandbox policy, and MCP server count/list.

`SessionTelemetry::record_api_request(...)` records request duration/status/error, endpoint, auth-header state, request id, Cloudflare ray id, auth error data, and agent-identity telemetry.

`SessionTelemetry::user_prompt(...)` records prompt length and input counts. The prompt text is logged only when `log_user_prompts` is enabled; otherwise the prompt body is redacted.

`codex-rs/otel/src/provider.rs` detects hostname with `gethostname()` for OpenTelemetry log resource attributes only. It adds `host.name` only to log resources, not to normal Responses headers/body.

Product analytics are a separate channel in `codex-rs/analytics/src/facts.rs` and `codex-rs/analytics/src/events.rs`.

- `TrackEventsContext` includes model slug, thread id, turn id, and product client id.
- `TurnResolvedConfigFact` includes session source, model/provider, permission profile/cwd, reasoning settings, approval policy, sandbox network access, collaboration mode, personality, workspace kind, input-image count, and first-turn state.
- `CodexRuntimeMetadata` includes Codex Rust package version, runtime OS, runtime OS version, and runtime architecture.

Local thread-store metadata is another separate surface in `codex-rs/thread-store/src/types.rs`.

- Thread metadata stores cwd, CLI version, session source, history mode, thread source, optional subagent nickname/role/path, git info, approval mode, permission profile, token usage, first user message, and optional persisted history.
- This is local/resume/listing state, not an upstream model-request profile.

`codex-rs/app-server-transport/src/transport/remote_control/mod.rs` uses hostname as a remote-control server name.

These telemetry, analytics, local-store, and remote-control fields are side channels or local persistence. They are not normal upstream Responses headers/body and should not be put into Aether's model-request profile unless Aether explicitly implements the equivalent channel. If Aether ever emits official-like telemetry, that work should have its own telemetry profile/field ownership matrix; it should not be hidden inside request header/body normalization.

### Negative Findings

For the normal Codex Responses request path, I did not find official source evidence that Codex adds these as standard request profile fields:

- local hostname
- local IP address
- public IP address
- OS username

Found but scoped differently:

- hostname: OpenTelemetry log resource and app-server remote-control server name.
- username/real name: realtime prompt first-name replacement only.
- user env vars such as `USER`, `LOGNAME`, `USERNAME`, `USERPROFILE`: shell environment allowlists/sandbox setup, not standard model request metadata.
- IP handling: network-proxy policy and DNS/private-IP enforcement, not a collected client fingerprint sent as upstream metadata.

### Current Aether Profile State

Aether currently has two separate Codex profile-like behaviors:

- `apps/aether-gateway/src/ai_serving/planner/standard/codex.rs` applies pool-stable client headers for Codex pools:
  - `user-agent`
  - `originator`
  - selected by stable hash of pool key and configured/default header profiles
  - removes known third-party upstream leak headers such as `x-amz-user-agent`
- `crates/aether-ai-formats/src/formats/openai/responses/codex.rs` adds Codex compatibility headers:
  - `chatgpt-account-id` from selected key auth config when absent
  - `x-client-request-id`
  - default `user-agent` and `originator` when absent
  - compatibility short `session_id` / `conversation_id` derived from `prompt_cache_key`

This is not yet a complete Codex profile. It is a narrow stable-header/profile shim plus some compatibility/session headers.

### Desired Aether Profile Shape

Implement Codex profile as layered state, not a single UA string:

- `codex_client_header_profile`
  - originator
  - Codex version/app surface
  - OS type/version/arch
  - terminal token
  - optional residency/FedRAMP account routing headers where appropriate
- `codex_install_identity_profile`
  - installation id
- `codex_runtime_request_identity`
  - session id
  - thread id
  - turn id
  - window id
  - parent/fork/subagent/thread-source fields
  - canonical `x-codex-turn-metadata`
  - compatibility direct headers and flat `client_metadata`
- `codex_auth_profile`
  - selected pool key account id
  - auth mode
  - authorization/account/FedRAMP header behavior
  - never sourced from inbound user `chatgpt-account-id`
- `codex_prompt_context`
  - prompt-visible cwd/shell/date/timezone/network/filesystem/subagent context
  - user/project/session scoped, not account-profile scoped
  - do not invent hostname/IP/username unless a specific official prompt surface requires it
- `codex_workspace_context`
  - repo root
  - git remotes
  - latest commit
  - dirty state
- `codex_transport_profile`
  - HTTP client mode
  - WebSocket mode
  - proxy/custom CA
  - TLS/client transport behavior
  - kept separate from request metadata
- `codex_telemetry_profile`
  - OpenTelemetry/session telemetry fields
  - product analytics runtime fields
  - thread-store/local metadata only if implementing equivalent local persistence
  - hostname only if implementing equivalent telemetry/log-resource behavior

The immediate implementation direction should be:

1. Keep the existing sticky/account separation.
2. Rename or document the current UA/originator mechanism as `client_header_profile`, not full `profile`.
3. Treat prompt context, workspace context, instructions, telemetry, hostname, IP, and username as out-of-scope for the current profile implementation.
4. Implement the first real profile cut around account-owned `installation_id` plus transport/TLS profile wiring.
5. Keep `chatgpt-account-id` bound to Aether's selected pool account.

### Field Ownership Matrix

Use these ownership rules when implementing profile behavior.

Account-owned client profile, applied after pool key selection:

- `user-agent` and its components: originator, Codex version/app surface, OS type/version, arch, terminal token, suffix.
- `originator`.
- stable account/key concrete profile id, selected template id/version, fingerprint hash, deterministic collision salt.
- `installation_id` and all its request surfaces when normalization is enabled: direct `x-codex-installation-id`, flat `client_metadata["x-codex-installation-id"]`, and `installation_id` inside parsed `x-codex-turn-metadata`.

Selected-key auth state, applied after pool key selection but not part of client runtime identity:

- `Authorization`.
- `chatgpt-account-id`.
- FedRAMP/residency headers where appropriate.
- auth mode/account id/account email when used by an equivalent telemetry channel.

User/runtime request identity, parsed before pool key selection and preserved by default:

- official root `session_id`.
- `thread_id`.
- `turn_id`.
- `window_id`.
- parent/fork/subagent/thread-source/request-kind fields.
- direct/session compatibility headers such as `session-id`, `thread-id`, `x-codex-window-id`, and `x-codex-turn-metadata`.
- body `client_metadata` runtime projections.

Prompt/system/context state, never account-normalized on the default proxy path:

- base instructions, which are the closest official source concept to "system prompt" in this code path.
- configured developer instructions.
- permissions/collaboration/personality/apps/plugins/skills/token-budget/multi-agent developer fragments.
- AGENTS.md and contextual user fragments.
- cwd, shell, current date, timezone, network/filesystem permission context, subagent context, and context diffs.
- workspace/git context sent as turn metadata or prompt context.

Telemetry/analytics/local-store state, not a normal upstream Responses header/body profile:

- OTEL session metadata: conversation id, auth mode/env flags, account id/email, originator, session source, model/service tier/reasoning settings, app version, terminal type.
- OTEL API request metrics: duration/status/error, endpoint, request id, Cloudflare ray id, auth error data.
- OTEL log resource `host.name`.
- analytics runtime metadata: Codex Rust version, runtime OS, runtime OS version, runtime arch, product client id.
- local thread-store metadata: cwd, CLI version, history mode, thread source, git info, approval mode, permission profile, first user message, token usage, persisted history.

Negative/default exclusions for normal Responses profile:

- local hostname: telemetry/remote-control only, not normal request header/body.
- local/public IP address: no official normal Responses client fingerprint evidence found.
- OS username/real name: realtime backend prompt name replacement only, not normal Responses turn metadata/profile.

### Current Profile V1 Scope

Prompt and instructions are explicitly deferred for the current implementation cut.

Do not read, parse, normalize, or rewrite these surfaces as part of profile v1:

- top-level Responses `instructions`.
- `input` prompt text.
- `<environment_context>`.
- AGENTS.md/contextual fragments.
- developer/system/tool/history messages.
- prompt cache anchors except preserving existing behavior.

Profile v1 should make only the account-owned client portrait concrete:

1. Existing client header profile:
   - `user-agent`.
   - `originator`.
   - Codex surface/version/OS/arch/terminal fields once templates are normalized.
2. New install identity profile:
   - stable `installation_id` owned by the selected Aether pool account/key.
   - persisted as part of that account/key's concrete Codex profile.
   - normalized only on official Codex metadata surfaces that already exist or are explicitly enabled.
3. Transport/TLS execution profile:
   - stable `transport_profile` owned by the selected provider key/account.
   - source of truth remains the existing `fingerprint.transport_profile` object that Aether already resolves into `ResolvedTransportProfile`.
   - profile metadata may reference the chosen transport profile, but transport execution must not learn a second conflicting source.

Official Codex installation-id source:

- `/opt/stacks/openai-codex/codex-rs/core/src/installation_id.rs` stores `${CODEX_HOME}/installation_id`, reuses a valid UUID if present, otherwise generates and persists a new UUIDv4.
- `/opt/stacks/openai-codex/codex-rs/core/src/responses_metadata.rs` emits this identity as `client_metadata["x-codex-installation-id"]` and, for request-identity turn metadata, inside `client_metadata["x-codex-turn-metadata"].installation_id`.

Aether installation-id target behavior:

- Materialize and persist a UUIDv4-shaped `installation_id` for the concrete selected account profile.
- Derive missing `installation_id` deterministically from the current stable selection identity (`auth_account_id` when available, then key id, then key name as a last fallback), then write it into `codex_client_profile`. This keeps accounts with a real account id stable across re-import/re-key while preventing a changed or unidentified account from inheriting another account's install identity.
- Reuse an existing `installation_id` only when `codex_client_profile.selection_key_kind` and `selection_key_hash` match the current selected account/key identity; mismatches are treated as a profile rebind.
- Store under a key/account scoped object such as `provider_api_keys.fingerprint.codex_client_profile.install_identity.installation_id`.
- Do not store it in `upstream_metadata`, because that field is runtime/provider-observed state and can be updated by report effects.
- Do not take it from inbound user requests. Inbound Codex users are using Aether's account pool; the upstream client-install portrait should belong to the selected pool account/key.
- When normalization is enabled, update all present installation surfaces consistently:
  - direct header `x-codex-installation-id` if the request path uses or already has that surface;
  - body `client_metadata["x-codex-installation-id"]`;
  - parsed object/string `client_metadata["x-codex-turn-metadata"].installation_id`;
  - direct header `x-codex-turn-metadata` only if that compatibility surface is present.
- Do not change runtime fields while normalizing install identity:
  - `session_id`;
  - `thread_id`;
  - `turn_id`;
  - `window_id`;
  - parent/fork/subagent/thread-source/request-kind metadata.

Official Codex HTTP/TLS source:

- `codex-rs/login/src/auth/default_client.rs` builds ordinary Codex HTTP traffic from `reqwest::Client::builder().default_headers(default_headers())`, plus Codex custom CA/proxy handling.
- Snapshot `42156ba` moved the shared wrapper/custom-CA code under `codex-rs/http-client/src/`, but the ordinary default path still starts from `reqwest::Client::builder().default_headers(default_headers())`.
- `codex-rs/Cargo.toml` declares workspace `reqwest = { version = "0.12", features = ["cookies"] }` with default features enabled. In the normal HTTP client path, this means reqwest's default TLS stack is in play.
- `codex-rs/http-client/src/custom_ca.rs` forces `builder.use_rustls_tls()` only when a Codex custom CA bundle is configured.
- Snapshot `42156ba` added route-aware request client construction through `build_default_reqwest_client_for_route(...)`. When the outbound proxy policy is `ReqwestDefault` or the client is sandboxed, it intentionally preserves `build_reqwest_client()` behavior; otherwise proxy/PAC route selection becomes part of the transport profile and can affect the observed network/TLS route.
- The searched official source did not show a normal Responses-path uTLS/JA3/JA4/ClientHello spoofing layer.
- Therefore there is no official `installation_id`-like body/header field for TLS fingerprint. TLS behavior is transport behavior, not request metadata.

Aether transport/TLS target behavior:

- Hard requirements for the Codex default TLS profile:
  - Keep `native-tls-vendored` enabled for Aether builds that claim strict Codex default TLS equivalence.
  - Treat `reqwest_default_tls` as the Codex default TLS backend; `rustls`/`codex-reqwest-rustls-auto` are legacy/non-equivalent for Codex and must not be documented or surfaced as strict official-Codex matches.
- Keep `fingerprint.transport_profile` as the execution source of truth.
- Continue current resolver precedence in `crates/aether-provider-transport/src/network.rs`: key `fingerprint.transport_profile` first, provider config fallback, then provider-specific fallback such as Grok.
- Use the existing `ResolvedTransportProfile` contract:
  - `profile_id`;
  - `backend`;
  - `http_mode`;
  - `pool_scope`;
  - optional `header_fingerprint`;
  - optional `extra`.
- Persist/assign the selected account's transport choice under `provider_api_keys.fingerprint.transport_profile`, not inside request bodies.
- The concrete Codex profile may contain a `transport_profile_id` reference for auditability, but the planner/executor should still read the existing transport field.
- Codex default transport profiles include an expected normalized TLS fingerprint under `fingerprint.transport_profile.extra.tls_fingerprint`. Codex account profiles also persist `codex_client_profile.transport_tls_fingerprint_hash`, and the account `fingerprint_hash` includes that TLS hash. Request usage metadata copies this to `tls_fingerprint.outgoing.expected`, so the profile id, execution backend, expected JA3/hash, and account portrait hash share one persisted source.
- For account isolation, the practical execution key already includes provider id, endpoint id, key id, profile id, backend, and HTTP mode. `pool_scope` should remain `key` for Codex account profiles, but current code must not rely on `pool_scope` alone for isolation.
- Direct gateway execution currently supports `reqwest_default_tls`, `reqwest_rustls`, and a `browser_wreq` path. Tunnel upstream pooling accepts `reqwest_default_tls` plus rustls-style backends, and rejects unsupported values such as `utls`.
- Aether profile v1 assigns a Codex default TLS transport profile (`codex-reqwest-default-tls-auto`) when a Codex key has none.
- The legacy `codex-reqwest-rustls-auto` transport is stable and auditable, but it must not be claimed as strictly equivalent to ordinary official Codex CLI transport.
- Strict ordinary Codex equivalence follows the official normal path: reqwest 0.12 default TLS/native TLS, vendored OpenSSL on the Linux build, no explicit rustls override, no custom CA unless the official custom-CA condition is also true, same target/SNI shape, and the same proxy/tunnel behavior.
- Tunnel paths must use the `reqwest_default_tls` backend to remain aligned with the Codex default TLS profile; rustls tunnel paths remain legacy/non-equivalent for Codex.
- Do not claim full Chrome/JA3/JA4 TLS impersonation for Codex profile v1. Official ordinary Codex does not use a browser impersonation/uTLS layer in the searched source; Chrome impersonation would be a different product profile.
- Profile v1 can still be real and stable: same selected account identity emits the same install id, UA/originator, transport profile id, HTTP mode, and connection-pool identity across restarts.

Strict TLS comparison result on 2026-06-28:

- Capture tool: `tools/tls-clienthello-capture.py`, comparing normalized ClientHello structure instead of raw bytes.
- Official installed `codex exec` against the same local loopback IP target produced JA3 hash `23211f2b48104c7030b93680a2efcfd0`.
- Aether legacy explicit rustls profile produced JA3 hash `15a7254eddf31f45dc492932457ebcef`.
- Legacy rustls result: `MISMATCH`.
- Differences included cipher suite list/order, extension list/order, supported groups, signature algorithms, and ALPN.
- Aether Codex default TLS profile using reqwest default/native TLS with vendored OpenSSL produced JA3 hash `23211f2b48104c7030b93680a2efcfd0`.
- Codex default TLS result: `MATCH`.
- Aether tunnel native-TLS connector produced JA3 hash `23211f2b48104c7030b93680a2efcfd0`.
- Tunnel native-TLS result: `MATCH`.
- The same default/native TLS path using the host system OpenSSL produced JA3 hash `2617ff3a2d7f879546f0aac7afc5f15c`, so vendored OpenSSL is required for strict match on this Linux build.
- The expected JA3/hash is persisted in the Codex transport profile extra metadata, copied into request report context as expected outbound TLS fingerprint metadata, and included in the Codex account portrait hash.

Codex source comparison snapshot on 2026-07-07:

- Compared official `openai/codex` `main` commit `42156ba007278d9068f1518ac1f627b56c136ef6` against the earlier local source snapshot `d2885dc`.
- No profile/sticky contract change was found in `codex-rs/core/src/responses_metadata.rs`, `codex-rs/codex-api/src/requests/headers.rs`, `codex-rs/core/src/installation_id.rs`, `codex-rs/model-provider/src/auth.rs`, `codex-rs/model-provider/src/bearer_auth_provider.rs`, `codex-rs/terminal-detection/src/lib.rs`, `codex-rs/protocol/src/session_id.rs`, `codex-rs/protocol/src/thread_id.rs`, or `codex-rs/core/src/turn_metadata.rs`.
- `client_metadata["x-codex-turn-metadata"]` remains the canonical full turn metadata transport; flat `client_metadata` keys and direct HTTP/WebSocket headers remain compatibility projections.
- Official HTTP session headers remain `session-id` and `thread-id`; the snapshot source does not make `chatgpt-account-id` a Codex runtime sticky identity.
- `installation_id` still resolves from `${CODEX_HOME}/installation_id`, reuses a valid UUID, and creates/persists a UUIDv4 when absent or invalid.
- Auth/account headers still come from selected auth state: `Authorization`, `ChatGPT-Account-ID`, and FedRAMP routing where applicable.
- UA/originator generation remains `originator/version (OS version; arch) terminal-token` plus optional suffix; first-party originator values are still represented by `codex_cli_rs`, `codex-tui`, `codex_vscode`, `Codex ...`, and chat-specific values.
- Transport-relevant snapshot delta: default HTTP construction now has an explicit route-aware wrapper for non-default outbound proxy policies. Aether's strict TLS fingerprint claim is therefore scoped to the same effective route class: default reqwest/native-TLS path, same target/SNI, no custom CA unless the Codex custom-CA condition is intentionally mirrored, and no additional proxy/PAC route transformation.
- Non-profile snapshot delta: `codex-rs/core/src/client.rs` can include `stream_options.reasoning_summary_delivery` when concurrent reasoning summaries are enabled. That field is request-body behavior, not account profile/fingerprint state. Aether preserves the official `{"reasoning_summary_delivery":"sequential_cutoff"}` shape on normal Codex Responses requests while continuing to remove unrelated legacy `stream_options` keys such as `include_usage` and to strip `stream_options` from compact requests.

Validation on 2026-07-07:

- Source snapshot check: `git ls-remote https://github.com/openai/codex.git refs/heads/main` returned `42156ba007278d9068f1518ac1f627b56c136ef6` at the time of comparison.
- Source comparison inspected the official files under `/tmp/openai-codex-latest`, including `responses_metadata.rs`, `headers.rs`, `client.rs`, `default_client.rs`, `http-client/src/custom_ca.rs`, auth providers, terminal detection, session/thread id, and turn metadata.
- Aether test coverage run after the comparison:
  - `cargo fmt --check`
  - `cargo test -p aether-ai-formats codex -- --nocapture`
  - `cargo test -p aether-gateway --lib codex -- --nocapture`
  - `cargo test -p aether-provider-transport --lib codex -- --nocapture`
  - `cargo test -p aether-ai-serving decision_response_records_outgoing_tls_fingerprint -- --nocapture`
  - `cargo test -p aether-ai-formats stream_core -- --nocapture`
- The first sandboxed gateway Codex test run hit local listener bind `PermissionDenied`; the same command passed when rerun with the required non-sandbox permissions. This was an environment permission failure, not a Codex/profile assertion failure.

TLS fingerprint device boundary:

- TLS fingerprint is not sourced from Codex `installation_id`, account id, hostname, OS username, or local/public IP request metadata.
- Normalized TLS fingerprint is mainly determined by TLS/HTTP stack, dependency versions, TLS backend/provider, configuration, ALPN/HTTP mode, target host/SNI shape, proxy/tunnel path, and custom CA path.
- Raw ClientHello bytes also contain per-connection random/session/key-share material, so raw bytes are expected to differ between connections.
- Device/OS can matter when the stack uses native TLS/OpenSSL/Schannel/SecureTransport, system libraries, system certificate behavior, or local proxy/VPN interception.
- For Aether's Codex profile v1, `native-tls-vendored` makes the default TLS profile build-image/profile stable instead of host-OpenSSL stable.
- Because Aether also links `wreq`/BoringSSL for browser-style profiles, `wreq/prefix-symbols` must remain enabled when `native-tls-vendored` is enabled. Without symbol prefixing, binaries that include both BoringSSL and vendored OpenSSL can fail to link with duplicate crypto symbols.
- With a fully bundled rustls configuration, the normalized fingerprint is less device-dependent and more build/config-dependent, but it still depends on Aether's chosen config and route.
- Therefore Aether should persist the selected transport backend/profile per account/key and verify the emitted fingerprint by capture; it should not invent per-account TLS jitter as if TLS were another installation-id field. Many accounts can legitimately share the same official Codex JA3. Account uniqueness should be enforced on the full account-owned portrait hash, not on JA3 alone.

## Implementation Plan For Aether Profile

### Definition Of "Real Profile"

There are several different field owners and they must not be mixed:

- Official Codex runtime passthrough: if the inbound request already carries official session/thread/turn/window metadata, Aether should preserve those runtime fields.
- Aether account profile: when routing through an Aether Codex pool account, Aether should emit that account's stable client install/header/transport portrait, not the end user's local client portrait.
- Aether synthetic Codex shape: if the inbound client is not official Codex and does not carry runtime metadata, Aether can create official-shaped runtime metadata only in a future explicit synthetic-client mode.

The implementation target is therefore:

- preserve official runtime identity fields exactly when present;
- keep the built-in/configured profile library as a realistic template library;
- instantiate one stable concrete profile per pool account/key;
- never let two active pool accounts share the same concrete client fingerprint/portrait;
- never let one account emit multiple different client fingerprints/portraits;
- normalize profile-owned fields to the selected account's concrete profile on the upstream request;
- keep pool account/key profile template selection deterministic through stable hash/rendezvous selection;
- parse and pass through user-supplied Codex runtime identity by default;
- synthesize outbound Codex runtime identity only behind the explicit `pool_advanced.codex_runtime_identity` switch (implemented 2026-09-03, default off; see `docs/architecture/codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`). It rewrites the outbound copy after account selection and never touches the inbound identity used for sticky, WS binding and usage;
- never let profile synthesis change pool-account ownership, user session ownership, or sticky binding semantics.

### Non-Negotiable Invariants

- `chatgpt-account-id` is selected-key/auth state. It must not be copied from inbound user headers.
- pool sticky remains based on Codex session identity, not account id, hostname, IP, UA, or random request id.
- existing sticky token format `session=<id>` remains stable.
- if official `session_id` / `thread_id` / `turn_id` / `window_id` exists, Aether must not overwrite those runtime fields on the default path. With `codex_runtime_identity.enabled` the outbound projections are rewritten to the synthesized per-account tree; sticky, WS binding and usage keep reading the inbound values.
- `installation_id` is profile-owned client-install identity. For Codex pool upstream requests, it should come from the selected account's concrete profile, not from the inbound user's local Codex install.
- if Aether normalizes `installation_id`, it must update every exposed surface consistently: direct headers, flat `client_metadata`, and parsed `x-codex-turn-metadata`.
- Aether should not synthesize `session_id`, `thread_id`, `window_id`, or `turn_id` on the default path when the user request already carries Codex metadata.
- when `codex_runtime_identity` synthesizes runtime ids, every outbound surface must agree on the same ids: dash headers, flat `client_metadata`, the `x-codex-turn-metadata` blob, `prompt_cache_key` when it was derived from the session, and the WS handshake. The sticky token source stays the inbound root.
- the built-in/configured profile library is the durable template source, not the final account portrait.
- each pool account/key should select one template from that library through a stable hash/rendezvous selection key.
- each selected template must be materialized into a frozen concrete account profile for that account/key.
- existing concrete account profiles are reusable only when stored `selection_key_kind` and `selection_key_hash` match the currently selected account/key identity.
- persisted `codex_client_profile.client_headers` are the runtime source for UA/originator when the selection matches; template library selection is only for first materialization or explicit migration.
- active concrete profile fingerprints must be unique across active pool accounts. This means the full emitted account-owned portrait must not collide; individual generic fields such as a Codex UA class or a reqwest/rustls transport class may still be shared by many real clients.
- an account/key must keep the same concrete profile across requests and process restarts.
- the selection/instantiation key should be the selected account/key identity, not a volatile runtime request id and not an inbound user header.
- prefer account id from the selected key's decrypted auth config when available; fall back to provider key id, then key name only if key id is unavailable.
- changing the template library must not automatically change existing account concrete profiles. Hash/rendezvous selection is for first assignment/backfill unless an explicit migration is run.
- concrete account profile state must be namespaced by provider/pool/selected account/key scope, not by inbound user session.
- synthesized runtime identity state is namespaced by provider + selected account (`selection_fp`) by design: the goal is one collapsed tree per account, so two Aether users on the same account share slots. Redis keys carry hashes only (`selection_fp`, `hash16(inbound root)`), never raw ids.
- raw user/session/profile ids should not be used as Redis keys or logs without hashing/redaction.
- prompt cache behavior must be preserved by default. Profile work must not add or overwrite `prompt_cache_key` on the default path.
- prompt/instructions/environment context must not be parsed or rewritten by profile v1.
- profile v1 must persist `installation_id` as a frozen account/key install identity.
- profile v1 must keep transport execution sourced from existing `fingerprint.transport_profile`.
- profile v1 must not add an alternate transport profile source inside request body/profile metadata that can disagree with execution.
- non-Codex providers and non-Codex request paths must be unaffected.

### Data Model

Separate template selection, concrete account profile instantiation, and runtime session identity.

Durable/configured profile template library:

- `template_id`
- `surface`: `codex-tui`, `codex-cli-rs`, `codex-vscode`, `codex-desktop`, etc.
- `originator`
- `codex_version`
- `os_type`, `os_version`, `arch`
- `terminal_token`
- optional UA suffix
- schema version and generation source

Stable account/key template selection:

- `selection_key`: selected account id from key auth config when available, otherwise provider key id, then provider key name only as a last fallback
- selected `template_id` / template content from the configured library
- selection algorithm: rendezvous-style scoring over `selection_key + template content`, matching the current code shape
- persisted/frozen assignment version for existing accounts
- no per-request randomization

Concrete account profile instance:

- `schema_version`
- `account_profile_id`: stable id derived from provider/account/key scope
- `scope`: provider/pool/key/account scope
- `selection_key_kind`: `auth_account_id`, `key_name`, or `key_id`
- `selection_key_hash`: redacted stable selection key hash
- selected `template_id`
- selected template library version
- concrete client headers: `user-agent` and `originator`
- frozen surface fields copied from the selected template: Codex version, app surface, OS/arch/terminal fields
- install identity: persisted UUIDv4-shaped `installation_id` bound to the selection key
- transport profile reference: chosen `transport_profile_id`, backend, and HTTP mode for audit only
- transport TLS fingerprint hash: expected JA3 hash for the chosen transport profile when known
- `fingerprint_hash`: hash of the full emitted client portrait
- `created_at`, `updated_at`, `frozen_at`
- uniqueness state or collision check for active accounts

Important distinction:

- templates may be reused by many accounts;
- concrete emitted portraits must not be reused by active accounts;
- concrete profile materialization happens once per selected account identity and is then frozen; if a provider key is rebound to another `auth_account_id`, the account profile is regenerated for the new identity;
- do not add artificial per-request jitter to fields that are naturally stable in real clients;
- account uniqueness should come from the frozen full portrait, especially `installation_id` plus selected header/transport profile, not from changing static environment fields on each request.

User-supplied runtime session identity:

- official Codex clients normally provide `client_metadata["x-codex-turn-metadata"]`
- flat `client_metadata.session_id` / `client_metadata.thread_id` may also be present
- `x-codex-installation-id` / `x-codex-window-id` may be present in `client_metadata`
- Aether should parse runtime fields for sticky/diagnostics and pass `session_id`, `thread_id`, `turn_id`, and `window_id` through without mutation
- `x-codex-installation-id` and `x-codex-turn-metadata.installation_id` are profile-owned and may be normalized to the selected account profile on the upstream request

Synthetic runtime session identity is out of the default profile path:

- only behind an explicit synthetic-client feature flag
- scoped by Aether user/session, not by account globally
- must include `session_id`, `thread_id`, `window_id`, and per-turn `turn_id` consistently if enabled

Selected-key auth profile:

- selected key id
- account id from decrypted auth config
- FedRAMP/residency flags
- authorization behavior

This auth profile is resolved after candidate/key selection and should not be part of the client runtime profile.

### Persistence Strategy

Use the configured profile template library as the durable source of realistic shapes, and persist concrete account profile instances. Deterministic derivation is allowed only from stable selected account/key identity, not from per-request state.

Template library and account profile instantiation:

- Keep many built-in/configured Codex profile templates available for pool accounts.
- Select a template deterministically from the library for each account/key on first assignment or fallback derivation.
- The current code already uses stable scoring over `selection_key + user_agent + originator`; this is the right model for choosing a base template in a changing account pool.
- The resolver prefers the selected key auth `account_id` when available, then provider key id, then key name only as a last fallback. Key name is not treated as a uniqueness guarantee.
- After template selection, materialize a concrete profile for the stable account/key identity.
- The concrete profile must include enough account-owned identity, especially a unique persisted `installation_id`, that two active accounts do not emit the exact same full client portrait.
- Do not use a request/session id as the selection key; use the stable provider key/account identity.
- Template library changes should be explicit operational changes. Existing account profile assignments must remain frozen unless an explicit migration is run.
- Store the assigned `template_id`/version and concrete profile fields in a namespaced key profile location or a dedicated account-profile store. A first cut can use `provider_api_keys.fingerprint.codex_client_profile` or an equivalent `client_profiles.codex` object, leaving existing `transport_profile` semantics untouched.
- Do not store the durable profile in `upstream_metadata`; that field is already used for provider-observed/runtime upstream state and can be updated by report effects.

Profile v1 storage shape:

- `provider_api_keys.fingerprint.codex_client_profile`
  - owns Codex account portrait state: template assignment, frozen UA/originator fields, install identity, fingerprint hash, and audit metadata.
  - owns `install_identity.installation_id`.
  - may store `transport_profile_id` / `transport_profile_hash` as a reference for audit.
- `provider_api_keys.fingerprint.transport_profile`
  - remains the actual transport execution source.
  - owns `profile_id`, `backend`, `http_mode`, `pool_scope`, `header_fingerprint`, and `extra`.
  - is what `resolve_transport_profile` reads today.
- `provider.config.fingerprint.transport_profile`
  - remains a provider-level fallback only.
  - must not override a selected key's concrete profile once the key has a profile.
- `upstream_metadata`
  - must not be used for durable client profile state.

Example logical shape:

```json
{
  "codex_client_profile": {
    "schema_version": 1,
    "account_profile_id": "codex-profile-...",
    "selection_key_kind": "auth_account_id",
    "selection_key_hash": "sha256:...",
    "template_id": "codex-tui-macos-arm64-...",
    "template_version": 1,
    "client_headers": {
      "user_agent": "...",
      "originator": "codex-tui"
    },
    "install_identity": {
      "installation_id": "uuid-v4"
    },
    "transport_profile_id": "codex-reqwest-default-tls-auto",
    "transport_tls_fingerprint_hash": "23211f2b48104c7030b93680a2efcfd0",
    "fingerprint_hash": "sha256:...",
    "frozen_at": "2026-06-28T00:00:00Z"
  },
  "transport_profile": {
    "profile_id": "codex-reqwest-default-tls-auto",
    "backend": "reqwest_default_tls",
    "http_mode": "auto",
    "pool_scope": "key",
    "extra": {
      "tls_fingerprint": {
        "ja3_hash": "23211f2b48104c7030b93680a2efcfd0",
        "ja3": "771,..."
      }
    }
  }
}
```

Collision handling:

- Compute `fingerprint_hash` from all emitted portrait fields.
- Enforce active-account uniqueness at assignment/backfill time. Prefer a persisted registry or unique index keyed by provider/pool scope plus `fingerprint_hash`; if the first cut stores profiles inside provider keys, the backfill/assignment command must still scan active keys before writing.
- If a collision is found, derive a deterministic collision salt from account/key identity, persist the chosen salt, and re-instantiate.
- Do not resolve collisions with per-request randomness.

Runtime identity:

- Default path: parse user-supplied runtime identity and do not persist or synthesize session/window/turn ids.
- Store `installation_id` per stable account identity as part of the concrete account profile.
- If a legacy/backfill path derives `installation_id`, materialize and freeze it before normal traffic relies on it. Existing profile identity is reused only when its stored selection key kind/hash matches the selected account/key.
- Future synthetic-client mode may store synthetic runtime sessions in Redis/runtime state with a TTL at least as long as the sticky binding TTL.
- Future synthetic-client storage must use a namespace that includes provider/pool scope, selected key/account identity, selected profile identity, and Aether user/API-key/tenant scope when available.
- Future synthetic-client storage must hash inbound client session keys and use an init lock similar to sticky-session initialization.

Base client header profiles can remain in provider/key config initially:

- provider `pool_advanced.codex_client_headers` as the profile library;
- built-in defaults as fallback library;
- key/account identity as the stable selection input;
- later, a DB table can be added only if operators need editable named profile libraries independent of provider config.

### Resolution Flow

Split the work into pre-selection session resolution and post-selection profile application.

Pre-selection:

1. Parse inbound request and existing `ClientSessionAffinity`.
2. Detect official Codex metadata:
   - `client_metadata["x-codex-turn-metadata"]`
   - flat Codex `client_metadata` ids
   - `session-id` / `thread-id`
   - `x-codex-installation-id` / `x-codex-window-id`
3. Resolve `CodexResolvedSession`:
   - official passthrough: use official root `session_id`, fallback `thread_id`;
   - otherwise: use current legacy sticky extraction.
4. Pool sticky selection uses the same resolved session token, keeping `session=<id>`.

Post-selection:

1. Resolve selected-key auth/account profile from the chosen transport/key snapshot.
2. Resolve the selected account identity from Aether's selected key/auth config, never from inbound request headers.
3. Select or load that account's frozen Codex client profile template assignment.
4. Instantiate or load the concrete account profile for the selected key/account.
5. Ensure the concrete profile has a persisted `installation_id`.
6. Resolve transport execution from existing `fingerprint.transport_profile`; if first assignment is responsible for transport, write that field before planning/execution rather than inventing a request-body source.
7. Ensure the emitted concrete portrait is stable for the account and not shared with another active account.
8. Apply profile-owned metadata:
   - set `user-agent` and `originator` from the concrete account profile;
   - set direct `x-codex-installation-id` if this surface is present/enabled;
   - rewrite flat `client_metadata["x-codex-installation-id"]` when `client_metadata` exists;
   - rewrite `installation_id` inside parsed `x-codex-turn-metadata` when that metadata exists;
   - leave `session_id`, `thread_id`, `turn_id`, and `window_id` unchanged.
9. Apply request metadata:
   - preserve official runtime metadata if present;
   - do not synthesize session/window/turn metadata on the default path;
   - insert missing synthetic metadata only in an explicit future synthetic-client mode;
   - keep `chatgpt-account-id` from selected key auth config;
   - keep existing compatibility short `session_id` / `conversation_id` behavior separate from official Codex runtime identity.
10. Do not inspect or rewrite prompt/instructions/environment context as part of profile.

### Cache And Sticky Safety

Sticky and cache must be tested as contracts.

- Preserve inbound `prompt_cache_key`.
- Do not generate or overwrite `prompt_cache_key` from profile work on the default path.
- Existing Codex compatibility body edits may still derive a missing `prompt_cache_key` from official session metadata, cache-control anchors, stable request anchors, or user API key id. That behavior is outside profile and should not be changed by profile rollout.
- Do not derive sticky from short compatibility `session_id` when official Codex metadata exists.
- Do not include selected account id in sticky token; account selection is the result of sticky/provider routing, not the input.
- Do not change candidate-affinity cache keys except to feed them the same resolved session id they already intend to use.
- Keep existing canonicalization for guardian/subagent session tokens until guardian/subagent identity is implemented end to end.

### Rollout Phases

1. Documentation and naming only.
   Rename/document current UA/originator logic as `client_header_profile`, not full profile. Mark prompt/instructions/environment-context work as deferred.

2. Field ownership checks for current behavior.
   Add tests around current behavior first: runtime fields preserve sticky identity, `chatgpt-account-id` is selected-key auth state, prompt/instructions stay untouched, and profile work does not alter prompt-cache behavior.

3. Materialize concrete account profile state.
   Add/load `fingerprint.codex_client_profile` per selected account/key with frozen template assignment, concrete headers, persisted UUIDv4-shaped `installation_id`, transport profile reference, and `fingerprint_hash`.

4. Align transport profile assignment.
   Ensure Codex account profiles have an execution transport in `fingerprint.transport_profile`; do not add a second execution source inside `codex_client_profile`.

5. Normalize installation id only.
   Rewrite account-owned install identity on existing official surfaces while preserving session/thread/turn/window and prompt/cache behavior.

6. Formalize the profile template library.
   Expand the built-in/configured profile entries from UA/originator into normalized template objects with stable `template_id`, surface, version, OS, arch, terminal, and originator fields.

7. Add explicit active-account uniqueness checks.
   Compute a fingerprint hash from emitted account-owned portrait fields and ensure two active pool accounts do not share the same concrete portrait.

8. Passive runtime parser and diagnostics.
   Parse official Codex metadata, report mode/source/hash in logs or request report context, but do not generate runtime ids.

9. Sticky parser alignment.
   Make sticky use the same parsed official runtime identity as diagnostics, while preserving current token format and fallback behavior.

10. Metadata ownership checks.
   Add tests that official runtime fields in `client_metadata` and `x-codex-turn-metadata` pass through unchanged, while profile-owned fields such as installation id are either preserved when normalization is disabled or rewritten consistently when normalization is enabled.

11. Future synthetic-client mode only if required.
   If Aether later needs to serve non-Codex clients as full Codex-shaped clients, add synthetic `session_id`, `thread_id`, `window_id`, `turn_id`, and prompt-cache alignment behind a separate feature flag.

12. Advanced Codex surfaces.
   Add guardian/subagent/window lineage, websocket trace fields, compacted-window metadata, and workspace/environment profiles only after the core identity path is stable.

### Required Tests

- official Codex runtime fields are preserved for `session_id`, `thread_id`, `turn_id`, and `window_id`.
- profile-owned `installation_id` is either preserved when profile normalization is disabled, or normalized consistently to the selected account profile when enabled.
- official Codex request keeps the same sticky token as current code.
- each stable pool key/account emits the same concrete profile across repeated requests.
- two active pool keys/accounts do not emit the same concrete profile fingerprint.
- existing account concrete profile does not change when template library entries are added, removed, or reordered.
- account/profile selection and instantiation do not depend on user session, sticky token, or request id.
- installation id is normalized consistently across direct headers, flat `client_metadata`, and parsed `x-codex-turn-metadata`.
- missing installation id is deterministically derived as a UUIDv4-shaped value from the selected account/key identity and then persisted/frozen.
- session/thread/turn/window ids are preserved while installation id is normalized.
- `fingerprint.transport_profile` is used as the execution source, and `codex_client_profile.transport_profile_id` cannot override it.
- gateway direct `browser_wreq`, `reqwest_default_tls`, tunnel backend mapping, and legacy rustls limitations are tested or explicitly gated by route capability.
- unsupported transport backends such as `utls` fail closed rather than silently downgrading.
- template library changes are covered by explicit migration/versioning tests if assignment preservation is required.
- `chatgpt-account-id` comes from selected key auth config even if inbound request sends a different value.
- existing `prompt_cache_key` is preserved.
- top-level `instructions`, `input`, and `<environment_context>` are byte-for-byte unchanged by profile v1.
- non-Codex requests are unchanged.
- disabling profile flags restores current behavior.

### First Implementation Cut

The first implementation cut is profile v1 only: account-owned `installation_id` plus transport/TLS profile integration. Prompt/instructions, telemetry, workspace context, and synthetic runtime ids stay out.

1. Rename/document current UA/originator as `CodexClientHeaderProfile`.
2. Add a `CodexConcreteAccountProfile` representation with:
   - schema version;
   - account profile id;
   - selection key kind/hash;
   - template id/version;
   - concrete headers;
   - persisted UUIDv4-shaped `installation_id` bound to selection key kind/hash;
   - transport profile reference;
   - `fingerprint_hash`;
   - frozen timestamps.
3. Load/store it under `provider_api_keys.fingerprint.codex_client_profile` for the selected key/account.
4. Keep transport execution under existing `provider_api_keys.fingerprint.transport_profile`.
5. If a Codex key has no transport profile, profile v1 assigns the Codex default `reqwest_default_tls` + `auto` + `key` and writes it to `fingerprint.transport_profile`. Legacy `codex-reqwest-rustls-auto` fingerprints are normalized to the new default during Codex profile materialization unless the operator configured a custom transport profile.
6. Apply concrete headers and installation id after selected key/account resolution.
7. Update only existing installation-id surfaces by default; optional insertion of missing official surfaces should be behind an explicit flag.
8. Preserve `session_id`, `thread_id`, `turn_id`, `window_id`, sticky token source, and `prompt_cache_key`.
9. Do not parse or mutate `instructions`, `input`, or `<environment_context>`.
10. Add active-key collision checks for the full account-owned portrait hash.
11. Add tests proving same key/account repeats the same profile, different active keys do not share the same full portrait hash, and transport execution still comes from `fingerprint.transport_profile`.
12. Defer runtime identity synthesis and prompt/instructions processing until there is a separate product requirement and evidence path.

This keeps profile v1 narrow enough to compare sticky, cache, and request mutation behavior before any synthetic runtime or prompt-visible changes are allowed to affect traffic.

### Production Log Check On 2026-06-28

After request capture was enabled, recent `Codex Pro` `openai:responses` audit samples showed:

- request headers had `x-codex-turn-metadata` on 316/330 captured records.
- request headers did not have `session-id`, `thread-id`, `session_id`, or `conversation_id`.
- provider request headers had `session_id` and `conversation_id` on 330/330 records; these are Aether compatibility headers, not user-supplied official session headers.
- request bodies had `client_metadata` on 277/350 sampled records.
- request bodies had `client_metadata.session_id`, `client_metadata.thread_id`, and `client_metadata["x-codex-turn-metadata"]` on 194/350 sampled records.
- the parsed turn metadata included `installation_id`, `session_id`, `thread_id`, `turn_id`, and `window_id` on 194/350 sampled records.
- provider request bodies matched the same user-supplied `client_metadata` counts, meaning Aether is preserving those fields rather than needing to synthesize them on the default path.

Implication: default profile work should not generate session/window/turn ids. Aether should parse and preserve user-supplied Codex runtime identity, and reserve synthetic runtime identity for a separate future mode.

### Production Prompt/Context Log Check On 2026-06-28

This check specifically validated the prompt/context claim against online Aether request logs without printing user prompt content.

Current/latest-body caveat:

- Recent 500 Codex `openai:responses` audit rows from `2026-06-28 10:16:39+08` to `2026-06-28 10:52:22+08` had `body_capture_mode = none`.
- For those newest rows, request/provider body state was `disabled`, with no body refs. They can validate headers/metadata, but not prompt body contents.

Historical body-capture sample:

- 723 Codex request/provider body pairs were available in `usage_body_blobs`.
- Time window: `2026-06-26 09:41:06+08` to `2026-06-28 10:13:58+08`.
- Both request and provider body captures were checked structurally; the check emitted only counts and JSON paths.

Observed prompt/body shape:

- `instructions` existed and was non-empty in most captured Codex Responses bodies: request `631/723`, provider `633/723`.
- `input` existed in `633/723` request/provider bodies.
- `client_metadata` existed in `631/723`; parsed `x-codex-turn-metadata` existed in `534/723`.
- `<environment_context>` blocks existed in `665/723`, always under `input[].content[].text`.
- Environment tags observed there included `cwd`, `shell`, `current_date`, `timezone`, `filesystem`, `permission_profile`, `workspace_roots`, `root`, `entry`, `path`, and `special`.

Negative prompt findings from the captured environment blocks:

- No `hostname`, `host.name`, `local_ip`, `public_ip`, or `whoami` token was found inside `<environment_context>`.
- No `username` token was found inside `<environment_context>`.
- No IP literal was found inside `<environment_context>` by the check.
- The top-level `instructions` field did not match host/IP/username/user-path patterns in this sample.

Path privacy finding:

- `<environment_context>` did contain user-path-shaped values in `276/723` samples.
- This matches the official source expectation: `cwd` and `workspace_roots` can reveal local path components such as `/Users/<name>/...`, `/home/<name>/...`, or `C:\Users\<name>\...`.
- Therefore the correct statement is: ordinary Codex turn prompt does not add a separate host/IP/username fingerprint field, but model-visible path context can still expose usernames through filesystem paths.

Other token hits:

- Host-like and username-like tokens did appear elsewhere in the full captured bodies, mostly under historical `input` items, tool outputs, tool call arguments, and tool schema descriptions.
- Those hits are not evidence that official prompt assembly injects host/IP/username as a client fingerprint; they are user/history/tool content surfaces.
