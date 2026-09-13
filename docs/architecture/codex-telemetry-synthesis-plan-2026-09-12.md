# Codex 出站遥测合成（analytics-events + OTLP metrics + wham/usage）

> Status: 已锁定决策，进入实现。目标版本 `backend-v0.7.136+`（未部署；发版仍走 tag → CI → ghcr → `update.sh`，且只在操作员明确授权后执行）。本文镜像于 `docs/architecture/codex-telemetry-synthesis-plan-2026-09-12.md`；运维见 `docs/operations/codex-runtime-identity-runbook.md` §8（本次新增）。

## 0. 已锁定的决策（操作员原话）

1. 「这个问题很严重 直接暴露了aether是非官方客户端的事实 需要系统性的修复这个问题 各种方面都要考虑。需要与codex cli相同。」
2. 「都做 注意上报数据都需要合成为少量数据 绝对不能上报真实数据」→ 三层全做（analytics-events 基线 + 工具事件 + OTLP metrics），体量小，**任何下游真实数据（prompt、路径、命令、下游 id、下游 UA、真实时延分布）一律不进上报体**。
3. 「缺省开，直接补齐」→ 默认 ON；按仓库惯例保留 kill switch。
4. 上报体里出现的 id 只允许两种来源：Aether 已经在 `/responses` 出站里使用的**合成身份**（session/thread/turn/window，`codex_runtime_identity.rs`）和**上游服务端自己签发**的 `resp_…`（服务端能校验，伪造反而是缺陷）。

## 1. 缺口与威胁

真实 `codex-tui 0.154.0` 默认（`analytics.enabled` 未设即 true）持续向 OpenAI 打三条**与 `/responses` 无关**的信号；Aether 号池账号三条全无，账号侧画像是「一个从不上报、从不导出指标、从不轮询配额的 codex 客户端」，这是确定性形状差异（治理原则：真实 codex-rs 产生不了的形状 = 缺陷）。

| 通道 | 端点 | 默认 | 节律（实测） | 认证 | Aether 现状 |
|---|---|---|---|---|---|
| analytics-events | `POST https://chatgpt.com/backend-api/codex/analytics-events/events` | 开 | 活动驱动，工具密集 turn 每 10–20 s 一批 2–4 事件，2.7–5.8 KB | Bearer + `chatgpt-account-id` + Cloudflare cookie | 零 |
| OTLP metrics | `POST https://ab.chatgpt.com/otlp/v1/metrics` | 开（release 构建；**同时受 `analytics.enabled` 门控**，见 §2.2 开关矩阵） | 严格 60 s 周期 + 正常退出 flush；20–120 KiB，冷启动批 460–470 KiB | 仅 `statsig-api-key`（包内置 client key，所有安装相同） | 零 |
| wham/usage | `GET https://chatgpt.com/backend-api/wham/usage` | 开（TUI） | 60 s；≥75% 30 s；≥90% 15 s；≥99% 5 s | Bearer + account-id + cookie | 仅管理端按需查询，无周期轮询 |

其余通道不在本次范围：OTel logs / traces（默认 exporter 为 None）、`/feedback` Sentry envelope（用户主动触发）。

## 2. 已验证的线形事实（来源：`/var/tmp/codex-cli-network-analyze` 脱敏抓包 + codex-rs `07f18d5ff` 源码 + 对端 OTLP 补充报告）

### 2.1 analytics-events

- 请求：`POST /backend-api/codex/analytics-events/events HTTP/1.1`，头**顺序**：`authorization, chatgpt-account-id, content-type: application/json, accept: */*, originator, user-agent, cookie, host, content-length`。**没有 `version`，没有 `x-codex-installation-id`**（`analytics/src/client.rs:881`：`.headers(auth_provider.to_auth_headers()).header("Content-Type","application/json").json(&payload)`，10 s 超时，无重试）。
- `originator` / `user-agent` 与同账号 `/responses` 完全一致（同一 profile）。
- 体：`{"events":[{"event_type":"…","event_params":{…}}]}`；响应 `200 {"status":"ok","total_events":N,"accepted_events":N,"skipped_events":0}`。
- 31 种 `event_type`（`analytics/src/*.rs`）。抓包窗口里出现：`codex_command_execution_event`(12) `codex_dynamic_tool_call_event`(10) `codex_web_search_event`(4) `codex_file_change_event`(3)。`codex_thread_initialized` / `codex_turn_event` 源码存在（2026-06-27 起）但窗口内未捕获——按源码结构合成，字段名与序列化顺序以 `analytics/src/events.rs` 为准。
- 公共元数据（所有事件）：`app_server_client{product_client_id, client_name, client_version, rpc_transport:"in_process", experimental_api_enabled:true}`、`runtime{codex_rs_version, runtime_os, runtime_os_version, runtime_arch}`、`thread_source:"user"`、`subagent_source:null`、`parent_thread_id:null`。
- 工具事件公共字段（`CodexToolItemEventBase`，flatten）：thread_id, session_id, turn_id, root_turn_id, item_id(`exec-<uuid>` 或 `call_…`), cell_id(小整数字符串), parent_call_id(`call_…`), originating_response_id(`resp_…`), subsequent_response_id(`resp_…`), app_server_client, runtime, thread_source, subagent_source, parent_thread_id, tool_name, started_at_ms, completed_at_ms, duration_ms, execution_duration_ms(null), review_count:0, guardian_review_count:0, user_review_count:0, final_approval_outcome:"unknown", terminal_status:"completed", failure_kind:null, requested_additional_permissions:false, requested_network_access:false。
  - `codex_file_change_event` 追加：file_change_count, file_add_count, file_update_count, file_delete_count, file_move_count。
  - `codex_command_execution_event` 追加：plugin_id:null, script_path:null, command_execution_source:"unifiedExecStartup", exit_code, command_total/read/list_files/search/unknown_action_count。
  - `codex_dynamic_tool_call_event` 追加：dynamic_tool_name, success, output_content/text/image/audio_item_count。
  - `codex_web_search_event` 追加：web_search_action:"open_page"|…, query_present, query_count。
- `codex_thread_initialized`（`ThreadInitializedEventParams`）：thread_id, session_id, app_server_client, runtime, model, ephemeral:false, thread_source:"user", initialization_mode:"new"|"forked"|"resumed", subagent_source:null, parent_thread_id:null, forked_from_thread_id:null, created_at(秒)。
- `codex_turn_event`（`CodexTurnEventParams`，字段顺序即序列化顺序）：thread_id, session_id, turn_id, root_turn_id, turn_trigger, codex_turn_source, submission_type:null, app_server_client, runtime, ephemeral:false, thread_source, initialization_mode, subagent_source, parent_thread_id, model, model_provider:"openai", sandbox_policy, reasoning_effort, reasoning_summary, service_tier:"default", approval_policy, approvals_reviewer, guardian_v2_enabled, sandbox_network_access, collaboration_mode, personality, workspace_kind, num_input_images, image_preparations:[], is_first_turn, status:"completed"|"failed"|"interrupted", explicit_client_interrupt_requested_at_ms:null, turn_error:null, codex_error_kind:null, codex_error_http_status_code:null, steer_count:0, total_tool_call_count, shell_command_count, file_change_count, mcp_tool_call_count:0, dynamic_tool_call_count, subagent_tool_call_count:0, web_search_count, image_generation_count:0, input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens, reasoning_output_tokens, total_tokens, before_first_sampling_ms, sampling_ms, compaction_ms:0, between_sampling_overhead_ms, tool_blocking_ms, after_last_sampling_ms, sampling_request_count, sampling_retry_count:0, duration_ms, started_at, completed_at。发出前提（`reducer.rs:2364`）：thread/turn 元数据 + resolved_config + profile + completed 全齐。

### 2.2 OTLP metrics

- 请求：`POST /otlp/v1/metrics HTTP/1.1`，头顺序：`content-type: application/json, user-agent: OTel-OTLP-Exporter-Rust/0.31.0, statsig-api-key, content-length, accept: */*, host: ab.chatgpt.com`。无 authorization / cookie / account 头。响应 `202 Accepted` `{"success":true}`。
- `statsig-api-key = client-MkRuleRQBd6qakfnDYqJVR9JuXcY57Ljly3vi5JVUIO`（`otel/src/config.rs:11`，包内置，非用户标识）。
- 体 = `serde_json::to_string_pretty`（2 空格缩进；校验：http-0003 compact 24682 B → indent2 92913 B ≈ 声明 94525 B）。`Temporality::Delta`；`PeriodicReader` 60 s（实测整分对齐 13:12:19 → 13:13:19 …）；正常退出 flush。
- 结构：`resourceMetrics[1]` → `resource.attributes` **恰好 8 个** stringValue：`service.name`(= originator，如 `codex_cli_rs`)、`service.version`(CLI 版本)、`os_version`、`telemetry.sdk.language:"rust"`、`env:"dev"`、`telemetry.sdk.version:"0.31.0"`、`telemetry.sdk.name:"opentelemetry"`、`os`；`droppedAttributesCount:0`、`entityRefs:[]`；`scopeMetrics[1]`：`scope{name:"codex",version:"",attributes:[],droppedAttributesCount:0}`、`schemaUrl:""`；`metrics[]` 每项 `{name, description, unit, metadata:[], sum{dataPoints, aggregationTemporality:1, isMonotonic:true} | histogram{dataPoints, aggregationTemporality:1}}`；sum dp `{attributes[], startTimeUnixNano:"<十进制字串>", timeUnixNano:"<十进制字串>", exemplars:[], flags:0, asInt:<数字>}`；hist dp 追加 `count, sum, bucketCounts[], explicitBounds[], min, max`。
- 桶：`*.duration_ms`（unit `ms`，description `Duration in milliseconds.`）34 界 `[0,5,10,25,50,75,100,250,500,750,1000,1250,1500,1750,2000,2250,2500,3000,3500,4000,4500,5000,6000,7000,7500,8000,9000,10000,12000,15000,20000,30000,60000,120000]`；`*bytes` 18 界 `[128,256,…,16777216]`；`codex.sqlite.logs.write.entries` 15 界 `[0,5,10,25,50,75,100,250,500,750,1000,2500,5000,7500,10000]`。
- 抓包活跃批（16 指标）：`codex.plugins.loaded_cache.request`(sum, outcome=hit)、`codex.remote_models.fetch_update.duration_ms`(hist, 无属性)、`codex.websocket.request`(sum)+`.duration_ms`(hist)、`codex.websocket.event`(sum, 按 `kind` 10 个 dp)+`.duration_ms`(hist)、`codex.sqlite.logs.write.{count(sum),duration_ms,bytes,entries,max_entry_bytes}`(属性 error=none, originator, status=success)、`codex.thread_history.sqlite_projection`(sum, outcome=success)、`codex.responses_api_overhead.duration_ms`、`codex.responses_api_inference_time.duration_ms`、`codex.responses_api_engine_iapi_tbt.duration_ms`、`codex.tool.unified_exec`(sum, 追加 tty=false)，偶见 `codex.web_search.results.payload_bytes`。空闲批缩为 websocket.event + sqlite.* + thread_history（+ remote_models fetch）。
- 会话属性（websocket/responses/tool 指标）：`app.version, auth_mode=Chatgpt, model=<出站模型>, originator, session_source=cli, success=true`；`kind` 取值：response.created / in_progress / output_item.added / output_item.done / output_text.delta / output_text.done / content_part.added / content_part.done / completed / custom_tool_call_input.delta / done / codex.rate_limits / codex.response.metadata / responsesapi.websocket_timing。
- `STATSIG_DISABLED_METRICS`（`otel/src/metrics/config.rs`，**永不出现在线上**）：codex.api_request(.duration_ms)、codex.conversation.turn.count、exec_server_client_requests_total、responses_api_engine_iapi_ttft / service_tbt / service_ttft、**codex.tool.call(.duration_ms)**、codex.turn.cost_microusd、codex.turn.token_usage。
- 冷启动/turn 级指标（源码核实、抓包窗口未含冷启动批）：`codex.process.start`(sum, originator)、`codex.thread.started`(sum, is_git)、`codex.startup.phase.duration_ms`(hist, phase[,status])、`codex.turn.e2e_duration_ms`(hist, 无额外属性)、`codex.turn.ttft.duration_ms`(hist)。**不在** disabled 名单 → 真实客户端会发；本次按源码形状合成并在测试里标注「源码核实」。

#### 2.2.1 对端隔离实验补充（另一台官方直登机，2026-09-12；产物 `otlp-metrics-audit/artifacts/{report.md,metric-catalog.csv,wire-example.synthetic.json}`，不在本仓）

- **开关矩阵（实测四格）**——决定了本方案的 kill switch 耦合规则（§8）：

  | `analytics.enabled` | `otel.metrics_exporter` | OTLP 通道 |
  |---|---|---|
  | true | 默认 statsig | 有上报 |
  | false | 默认 | 未观察到 |
  | true | `"none"` | 未观察到 |
  | false | 显式 `"statsig"` | 未观察到 |

  即 `analytics.enabled=false` 会把 metrics exporter 直接置 None（`core/src/otel_init.rs:70-77`：`metrics_exporter = if analytics_enabled { … } else { OtelExporter::None }`）。**「analytics 关 + OTLP 开」是真实客户端产生不了的组合**；「analytics 开 + exporter=none」合法。
- 指标目录全量 **137 项**（对端逐项标注类型/维度/证据）；本方案只合成抓包实证的 16 项活跃集合 + 5 项源码核实的冷启动/turn 指标，其余不发——「少发」与真实空闲批同构，「发错形状」才是缺陷。
- 纠正：`codex.tool.call` 的维度是 `tool` / `success`（不是 `tool_name`）。该指标在 `STATSIG_DISABLED_METRICS` 内，本来就不上线，仅作对照。
- `codex.turn.cost_microusd` 的记录代码会传入 `turn.id`、`conversation.id` 属性。它同样在禁用名单内（线上流量未见），但据此**不能**断言 metrics body 天然不含关联 ID → 本方案凡涉及 turn/thread 关联的指标一律只用合成身份，且禁用名单断言写进单测（§9）。
- resource 8 项在同一隔离配置目录重启后保持不变，且**未见** `StableID` / `device_id` / `installation_id` / `hostname` / `arch` / 账号 ID。→ 我们按 `(provider_id, key_id)` 稳定派生这 8 项即可，不需要也不应引入额外机器标识。
- 该入口返回 `202`，而 OTLP 规范定义的成功响应是 `200`——属服务端自身偏离，我们只需接受 2xx，不做规范校验。

### 2.3 wham/usage

- `GET /backend-api/wham/usage`，头（抓包 `http-0009` 顺序）：`user-agent, authorization, chatgpt-account-id, x-openai-codex-luna-reserve, accept: */*, cookie, host`。
- 完整调用链已核实：`tui/src/app/startup.rs:789-792,901-910`（`select!` 到期）→ `tui/src/app/background_requests.rs:80,802`（`GetAccountRateLimits`）→ `app-server/src/request_processors/account_processor.rs:1131` → `backend-client/src/client/rate_limit_resets.rs:29→34→68` → `:127 format!("{}/wham/usage", base_url)`；官方测试 `rate_limit_resets_tests.rs:60` 钉死完整 URL。
- 间隔 `tui/src/chatwidget/rate_limits.rs:189-215`：60s，`used_percent` ≥75%→30s、≥90%→15s、≥99%→5s。门是 `:398 requires_openai_auth && has_chatgpt_account`，**与 `analytics.enabled` 无关**——关了遥测的客户端照发。
- **头形状的关键点**：wham 走 `BackendClient`（`backend-client/src/client.rs:245-265`），不是 `core/src/client.rs`，所以它**不带 `originator`、不带 `version`**，且 `user-agent` 在最前。这是全部抓包端点里唯一这样的一条（`/models`、`/alpha/search`、analytics-events 都带 `version`+`originator`）。
- **Aether 现状更正**：不是「只有管理端按需」。`maintenance/runtime/account_self_check.rs` 有常驻 worker，每 60s 扫描、对每个 codex key 周期性发 wham/usage，默认每 key 60 分钟一次（`account_self_check_interval_minutes: 60`，`handlers/admin/provider/pool/config.rs:504`）。管理端按需与 self-check 共用 `build_codex_quota_request_spec` → `execute_codex_quota_plan`。
- **已修（本次）**：`accept` 由 `application/json` 改为 `*/*`、补 `x-openai-codex-luna-reserve: 1`（`providers/codex.rs`）；剥掉 `originator`/`version`、挂 ChatGPT Cloudflare cookie jar、启用新的 `codex-backend-client` 头顺序档（`quota/codex/plan.rs` + `execution_runtime/transport.rs`）。**频率仍是 60 分钟，未改**——提频到 60s 是 60 倍请求量，另行决策。

## 3. 架构

新模块 `apps/aether-gateway/src/codex_telemetry/`：

```
mod.rs               开关、账号作用域、信号类型、有界队列、worker 注册、体量治理
identity.rs          从 CodexConcreteAccountProfile 推导 runtime/app_server_client/resource 属性（纯函数）
analytics_events.rs  事件合成 + 发送（A + B）
otlp_metrics.rs      60 s 周期 metrics 合成 + 导出（C）
wham_usage.rs        60 s 配额轮询（D）
```

### 3.1 信号（生产者只做 enqueue，永不阻塞热路径）

| 信号 | 生产点 | 载荷 |
|---|---|---|
| `ThreadMinted` | `codex_runtime_identity.rs::resolve_inner` 抢到 freeze `set_if_absent` 的分支 → 新增 `OutboundCodexRuntimeIdentity.thread_minted: bool`（`#[serde(default)]`）；planner `standard/codex.rs` 与 WS `resolve_candidate_runtime_identity` 拿到 `outbound` 后 enqueue | account scope, outbound identity, model, now |
| `SamplingStarted` | 同上两处（每次 `/responses` 出站） | request_id → {scope, outbound identity, model, reasoning_effort/summary, service_tier, num_input_images, 本次 input[] 里的工具回执摘要（见 §5）, started_at} 写入进程内 `DashMap<request_id, SamplingContext>`（TTL 清理） |
| `SamplingFinished` | HTTP 流 `record_stream_terminal_usage`、HTTP 同步 `record_sync_terminal_usage`（仅 enqueue）、WS `process_settlement_commit` Step 分支 | request_id, status_code, cancelled, first_byte_ms, elapsed_ms, `terminal_summary.{response_id, standardized_usage, model}` |
| `WsEventsObserved` | `codex_ws/runtime.rs` 终止事件处（`:3034` 附近已有 response_id 采集） | 按 `kind` 的事件计数（只传计数） |

`SamplingContext` 由 `SamplingFinished` 消费后并入按 `(scope, thread_id, turn_id)` 的 `TurnAccumulator`。**turn 关闭判定**：(a) 同 thread 出现新 turn_id，或 (b) 最后一次采样终止后 15 s 无新采样（对应真实客户端 10–20 s 的批发送节律）。关闭时产出 `codex_turn_event`（§4.2）。

### 3.2 账号作用域与认证

- scope key = `(provider_id, key_id)`；同一 key 只允许一个 OTLP 节拍、一个 wham 轮询、一条 analytics 发送串行队列。
- analytics / wham 发送时（而非入队时）通过 `AppState::read_provider_transport_snapshot(key_id)` + `resolve_local_oauth_request_auth` 取**当前** access token 与 `chatgpt-account-id`（`aether-model-fetch/transport.rs:91` 已有同一做法），避免持有过期 token。
- cookie：`chatgpt_cloudflare_cookies::request_cookie_header(key_id, url, now)`；响应 `set-cookie` 经 `ingest_set_cookie_headers` 回灌同一 jar（与 `/responses` 共享账号 jar，`is_allowed_chatgpt_host` 守门）。
- UA / originator / 版本：取该 key 的 `CodexConcreteAccountProfile`（`codex_profile.rs`）；`codex_client_version_from_user_agent` 给 `client_version` / `codex_rs_version` / `service.version`；`OutboundClientOs::from_user_agent` + UA 括号段给 `runtime_os` / `runtime_os_version` / `runtime_arch` / `os` / `os_version`（映射：`Mac OS 26.5.1; arm64` → runtime_os=`macos`, runtime_os_version=`26.5.1`, runtime_arch=`aarch64`, os=`macos`, os_version=`26.5.1`；Debian/Ubuntu/… → `linux`；Windows → `windows`）。`product_client_id` / `client_name` = originator（抓包：`codex-tui`；号池 profile 为 `codex_cli_rs` 时同样直接取 originator）。
- 传输：与执行运行时同款 `http1_only` reqwest client（复用 `execution_runtime/transport.rs:1480` 的构造参数，含代理快照 `plan.proxy` 同源）；头顺序过 `order_request_headers_like_codex_cli`（analytics 需要的顺序是其子序列：authorization → chatgpt-account-id → content-type … 经 `CODEX_CLI_TRAILING_HEADER_ORDER` 排序后为 accept, content-type, authorization, chatgpt-account-id, originator, user-agent, cookie —— **与抓包不同**；因此 analytics 与 OTLP 各自定义**显式**头顺序常量并加顺序测试，不复用 `/responses` 排序器）。

### 3.3 队列与 worker

- 每类信号一个 `mpsc` 有界通道（默认 4096），满则丢并计 `codex_telemetry_dropped_total{channel}`；绝不 await 热路径。
- 三个 supervised worker（`state/core.rs:1589` `supervise_worker`，新增 `TASK_KEY_CODEX_TELEMETRY_{ANALYTICS,OTLP,WHAM}`）：analytics 消费者（串行按 scope 发送、10 s 超时、无重试、失败只记 debug）、OTLP 节拍器、wham 轮询器。
- 进程内状态（不进 Redis）：`SamplingContext` 表、`TurnAccumulator` 表、按 scope 的 `ActivityWindow`（最近活动时刻、60 s 内 ws 事件计数、采样时延分桶）、24 h 滚动配额计数。多副本部署下每副本各自维护——可接受，因为每副本各自在该账号上产生 `/responses` 流量，分别上报与「一个用户开了几个终端」同构；需在 runbook 标注。

## 4. A：analytics-events 基线

### 4.1 `codex_thread_initialized`

- 触发：`ThreadMinted`。
- `created_at` = 当前秒；`model` = 出站模型；`initialization_mode:"new"`；其余按 §2.1 常量。
- 单独成批立即发送（真实客户端在 thread/start 后很快发出）。

### 4.2 `codex_turn_event`

- 触发：turn 关闭（§3.1）。
- 来源→字段：
  - 身份：合成 session/thread/turn；`root_turn_id` = turn_id（顶层 turn）；`turn_trigger:"user"`、`codex_turn_source:"user"`（源码 `TurnAnalyticsMetadata`；抓包未见，取最常见值；测试标注）。
  - 配置：`model` 出站模型；`reasoning_effort` / `reasoning_summary` 来自出站 body（已是出站真实值，服务端同请求可见）；`service_tier:"default"`；`sandbox_policy:"workspace-write"`、`approval_policy:"on-request"`、`approvals_reviewer:"user"`、`guardian_v2_enabled:false`、`sandbox_network_access:false`、`collaboration_mode:"default"`、`personality:null`、`workspace_kind:"git"`（与账号 `workspace_identity` 一致，profile 里已有 git 工作区）。
  - `num_input_images` = 出站 input 中 `input_image` 计数（**上限 2**，只报数）。
  - `is_first_turn` = 该 turn 与 thread 同一请求铸造。
  - `status`：终止 2xx 且未取消 → completed；取消 → interrupted；其他 → failed（此时 `codex_error_kind:"http"`、`codex_error_http_status_code` 填状态码，其余 error 字段 null）。
  - 工具计数：来自 §5 的合成计数（小整数，且 ≤ 观测到的工具回执数）。
  - token：来自 `standardized_usage`（服务端在 `/responses` 里已给出的数字，非下游数据）。
  - 时序：`sampling_request_count` = 该 turn 采样次数；`sampling_ms` = Σ elapsed；`before_first_sampling_ms` ∈ [40,400] 合成；`between_sampling_overhead_ms` = (n−1)×[20,120] 合成；`tool_blocking_ms` = 若有工具回执则 Σ 合成 [300,4000]/次，否则 0；`after_last_sampling_ms` ∈ [5,60]；`duration_ms` = 各段之和；`started_at`/`completed_at` = 秒级，与 duration 自洽。
- 每 thread 每 turn 恰好一条；与本 turn 的工具事件同批发送（真实客户端 turn 结束时会把待发事件一起 flush）。

## 5. B：工具事件（有条件）

- 数据源（**只看出站 input[]，永不解析响应流**）：本次采样 input 中新增的 `function_call_output` / `custom_tool_call_output` / `local_shell_call_output`，按 `call_id` 回找同 input 里的 `function_call.name` / `custom_tool_call.name`；`web_search_call` 项计为 web_search。
- 映射（`analytics/src/reducer.rs` ToolItemKind）：`shell` / `shell_command` / `exec_command` / `write_stdin` / `local_shell` → `codex_command_execution_event`；`apply_patch` → `codex_file_change_event`；`web_search_call` → `codex_web_search_event`；`view_image` / `update_plan` / `request_user_input` 等其它已知 codex 内置名 → 不上报（真实 reducer 归入 ImageView/Control，抓包无对应事件）；未知名称 → `codex_dynamic_tool_call_event`（`dynamic_tool_name` 用**归一化桶名**，不透传真实名：`tool_a`…`tool_f` 按 name 哈希稳定映射）。
- 合成规则（少量、非真实）：每 turn 最多 4 条工具事件（超出只累加进 `codex_turn_event` 计数，且计数上限 12）；`item_id` = `exec-<uuidv4>`（command）/ `call_<22 位 base62>`（其它）；`cell_id` = thread 内递增小整数字串；`parent_call_id` = 合成 `call_…`；`originating_response_id` = 上一采样的真实 `resp_…`，`subsequent_response_id` = 本采样的真实 `resp_…`（服务端签发，非下游数据）；`started_at_ms`/`completed_at_ms` 落在两采样之间；`duration_ms` ∈ [120,3500]；command：`exit_code:0`、`command_total_action_count` ∈ [1,3]，read/list/search 按 total 拆分，unknown=0；file_change：`file_change_count` ∈ [1,2]，add/update 拆分，delete/move=0；dynamic：`success:true`、`output_text_item_count:1`，其余 0；web_search：`web_search_action:"open_page"`、`query_present:true`、`query_count:1`。
- 绝不进入上报体：命令文本、路径、patch、查询词、下游 call_id、下游 item id。

## 6. C：OTLP metrics

- 生命周期（按 scope）：首次 `SamplingStarted` → 进入 `Active`，立即发**冷启动批**（`codex.process.start`、`codex.startup.phase.duration_ms`(phase ∈ {config, auth, models, mcp}…按源码枚举选 3–5 个)、`codex.thread.started`(is_git=true)、`codex.plugins.loaded_cache.request`、`codex.remote_models.fetch_update.duration_ms`、sqlite.*、thread_history）；此后每 60 s（起点 = 启动时刻，抖动 ≤ 500 ms，保持整分对齐特征）发一批；最近活动 > `AETHER_CODEX_OTLP_ACTIVE_WINDOW_SECS`（默认 1800）→ 发**退出 flush 批**后转 `Idle`，停止。
- 批内容：
  - 活跃批（本 60 s 内有采样）：§2.2 活跃 16 指标集合；`codex.websocket.event` 按 `kind` 的 dp 计数来自 `WsEventsObserved`（HTTP 采样则按 `output_text.delta ≈ output_tokens/4`、其余 kind 各 1 合成）；`*.duration_ms` 直方图按测得 elapsed/first_byte 落桶（只报桶计数、sum/min/max 四舍五入到桶界附近的合成值，不报精确毫秒）；`codex.turn.e2e_duration_ms` / `codex.turn.ttft.duration_ms` 在 turn 关闭时入队；`codex.tool.unified_exec` 仅当本批有 command 工具事件。
  - 空闲批：websocket.event(kind ∈ {codex.rate_limits, responsesapi.websocket_timing} 各 1) + sqlite.*（count 1–3，bytes/entries 小桶）+ thread_history(1)；每 5 批附一次 `codex.remote_models.fetch_update.duration_ms`。
- 资源属性恰 8 个，顺序与抓包一致；`service.name` = originator；`service.version` = UA 版本；`os`/`os_version` 见 §3.2；`env:"dev"`。
- 会话属性：`app.version` = UA 版本、`auth_mode:"Chatgpt"`、`model` = 本批出站模型（多模型则多 dp）、`originator`、`session_source:"cli"`、`success:"true"`。
- 序列化：手写 `serde_json::Value` → `to_string_pretty`；时间戳十进制字串；`startTimeUnixNano` = 上一批 `timeUnixNano`（Delta 语义）。
- 头：`content-type, user-agent: OTel-OTLP-Exporter-Rust/0.31.0, statsig-api-key, content-length, accept, host`（显式顺序常量）；无代理外一切与 `/responses` 同代理出口；期待 202，非 202 只记 debug。
- 禁止名单：单元测试断言批内不出现任何 `STATSIG_DISABLED_METRICS`。

## 7. D：wham/usage 轮询（低优先级，最后做）

- scope 处于 `Active` 时每 60 s `GET /backend-api/wham/usage`，头顺序 `user-agent, authorization, chatgpt-account-id, x-openai-codex-luna-reserve, accept, cookie, host`；响应喂给已有的配额头处理（`apply_local_codex_quota_headers_effect` 同源效果），并按官方阶梯（≥75% 30 s / ≥90% 15 s / ≥99% 5 s）调整下次间隔。
- kill switch `AETHER_CODEX_WHAM_USAGE_POLL=off`。

## 8. 开关与体量治理

| 环境变量 | 默认 | 作用 |
|---|---|---|
| `AETHER_CODEX_TELEMETRY` | on | 总开关 |
| `AETHER_CODEX_ANALYTICS_EVENTS` | on | A+B |
| `AETHER_CODEX_ANALYTICS_TOOL_EVENTS` | on | 仅 B |
| `AETHER_CODEX_OTLP_METRICS` | on | C |
| `AETHER_CODEX_WHAM_USAGE_POLL` | on | D |
| `AETHER_CODEX_OTLP_ACTIVE_WINDOW_SECS` | 1800 | Active→Idle |
| `AETHER_CODEX_TELEMETRY_QUEUE_CAPACITY` | 4096 | 各通道队列 |
| `AETHER_CODEX_ANALYTICS_DAILY_CAP` | 2000 | 每 scope 24 h 请求上限 |

- **开关耦合（来自 §2.2.1 实测矩阵，硬约束）**：`AETHER_CODEX_ANALYTICS_EVENTS=off` 时**强制**把 OTLP 也关掉并记一条 warn；反向允许（analytics 开、`AETHER_CODEX_OTLP_METRICS=off` 对应真实客户端的 `metrics_exporter="none"`，是合法形状）。「只发 OTLP 不发 analytics」是真实 codex-rs 产生不了的组合，按治理原则等同缺陷，用启动期校验挡死而不是靠运维记住。
- 自守门：仅 provider_type=codex 且 endpoint 指向 https ChatGPT 域（复用 `is_chatgpt_backend_base_url`）；relay/mirror 不发。
- 与 `AETHER_CODEX_TRANSPORT_FIDELITY=off` 联动：传输保真关闭时本模块也关闭（避免半真半假）。
- 观测：`codex_telemetry_sent_total{channel,status}`、`codex_telemetry_dropped_total{channel,reason}`、日志 `event_name = codex_telemetry_*`，全部不含上报体。

## 9. 测试

- 纯函数：identity 推导（3 种 UA）、事件合成（字段顺序 vs 抓包样本逐键比对、无禁用字段、计数上限）、OTLP 批（8 资源属性且逐项等值、桶界数组、Delta 时间戳链、pretty 缩进、禁用指标不出现、批内不含任何 `turn.id`/`conversation.id` 形态的真实关联键）、头顺序常量 vs 抓包顺序。
- 集成（`wiremock`）：analytics 200 / OTLP 202 / 401 不重试；cookie 回灌；kill switch 生效（含 §8 的 analytics-off ⇒ OTLP-off 耦合断言）；队列满丢弃不阻塞。
- 回归：既有 13 个 custom 分支失败基线不增加；`rustfmt --edition 2021 <file>`；`CARGO_BUILD_JOBS=2`，test 二进制 `timeout 3000`。

## 10. 实施顺序

1. `codex_telemetry/{mod,identity}.rs` 骨架 + 开关 + 队列 + worker 注册 + `thread_minted` 字段。
2. `analytics_events.rs`：thread_initialized + turn_event（A），HTTP 流/同步 + WS 三个钩子。
3. 工具事件（B）：planner 侧 input[] 摘要 + 合成。
4. `otlp_metrics.rs`（C）。
5. `wham_usage.rs`（D）。
6. 文档：本文镜像、runbook §8、`容器更新历史.md` 待发版条目、memory 更新。
7. 版本 `backend-v0.7.136`（打 tag 仍需操作员授权）。

## 11. 风险与未决

- 抓包未含 `codex_turn_event` / `codex_thread_initialized` / 冷启动 OTLP 批的真实样本：按源码合成，字段名与顺序有源码保证，取值分布是推断。若后续拿到样本，只需调常量。对端 137 项目录也标注了「不是所有平台和动态分支的穷尽证明」，同理。
- 服务端可能用 analytics 里的 token 数与 `/responses` 实际 usage 对账：我们用的正是服务端返回的 usage，天然一致。
- 多副本重复上报：每副本一份，等价于多终端；runbook 标注。
- 真实客户端 OTLP 起点是进程启动时刻，节拍不与 turn 对齐；我们以首采样为起点，形状相同。
