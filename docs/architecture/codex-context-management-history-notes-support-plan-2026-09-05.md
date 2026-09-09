# Codex `context_management`（history / notes 扩展）在 sub2api → Aether 链路上的支持计划

> Status: **Plan only** — 未实现、未授权实现。本文不含代码改动，不改线上任何配置。
> Date: 2026-09-05
> Scope: codex-rs `features.context_management.experimental_mode` 开启后新增的 10 个 `alpha/history/v2/*` / `alpha/notes/v2/*` 端点，以及它带来的 `/responses` 形状变化，在 **codex 客户端 → sub2api（`/backend-api/codex` 组）→ Aether（`/v1/...`）→ ChatGPT** 链路上的路由、粘性、头/体处理、计费、审计，以及与 Codex 号池「会话身份合成」的一致性。sub2api 与 Aether 两侧的改动写在同一份计划里，**不分期**。
> Production changes: 无。线上 Aether `backend-v0.7.109`（2026-09-05 15:28 UTC）、sub2api `cafecode-v0.0.72` 都不含本计划内容。
> Deploy: Aether 走 tag → CI → ghcr → `.env` 钉 digest → 只重建 `app`（`docs/operations/release-and-container-update-spec.md`）；sub2api 走其自身发布流程。两侧上线、以及在 Codex 号池上补端点行，都需要操作员另行明确授权。

### 维护者速览（2026-09-05）

| 项 | 内容 |
|---|---|
| 这是什么 | codex 0.15x 的实验功能：不再把上下文反复压成一份摘要，而是把历史 ingest 到后端，模型通过 `history` / `notes` 两组工具按需检索；压缩变成客户端本地「开新窗口」。开启后客户端会向 `{base_url}/alpha/history/v2/*`、`{base_url}/alpha/notes/v2/*` 发 10 种 POST。 |
| 线上现状 | 两侧都不认这些路径：sub2api 没挂路由（gin 404），Aether `classify_control_route` 不认识（404）。近 14 天 Codex Pro 0 条 `history_ingest_requested`，近 7 天 0 条 alpha/history、alpha/notes 日志——还没有客户端在用。 |
| 链路 | 客户端只连 sub2api：`base_url = https://<sub2api>/backend-api/codex`（满足 codex-rs 的门槛）。sub2api 对 API-Key 类账号（= Aether）拼 `{base}/v1/...`。**Aether 不需要 `/backend-api/codex` 别名**，前缀由 sub2api 负责。sub2api 组内账号全是 Aether 的 API-Key 账号、作用相同，所以 sub2api 层只是转发，选哪个账号都落到同一个 Aether。`/v1/alpha/search` 今天就是这么走的（7 天 Codex Pro 1339 条）。 |
| 关键决定 | ① 这些请求**没有 `model`**：账号归属只在 Aether 决定（sub2api 组里的账号全是对接 Aether 的同质 API-Key 账号，任选其一即可，**sub2api 不做粘性**）；Aether 侧路由只认「粘性绑定」，thread_hint 是新 thread 的第一个请求、负责建立绑定；② 号池合成开启时改写 body `context.session_id` / `current_agent_name`，thread_hint 视为 root 首次到达；③ 合成窗口号改为**跟随入站 `window_number`**（本地压缩不再有 compaction 请求）；④ `/responses` 里 developer message `token_budget.context_window` 的三行 context window id 与 `Agent name` 行同步改写；⑤ 头用**白名单**（真实客户端在这些端点上的头集合很小且封闭）。 |
| 官方基准 | 本地 `/opt/stacks/openai-codex/codex-rs` @ `07f18d5f`（不上网查）；sub2api 基准 `/opt/stacks/sub2api` @ `62ac15ba`（= 线上 cafecode-v0.0.72）。 |
| 不做 | 不给 Aether 加 `/backend-api/codex` 路由；不解析/改写加密工具参数；不改写上游响应；不改 sticky / 绑定 / fence / 用量读入站身份的规则；不改 `/responses` 的现有合成算法（只加窗口号来源与 developer message 改写）。 |

---

## 1. 背景与目标

用户目标不变：**每个号池账号看起来只有一个 codex 在用，threads / turns 少量，账号之间没有关联**。评判口径：任何真实单一版本 codex-rs 产生不了的确定性形状都是缺陷，优先级高于「少泄漏」；「系统性的修复，但不要过度」。

`features.context_management.experimental_mode` 是 codex 的新功能。一旦有下游用户在客户端里打开它：

1. 客户端会对 sub2api 发 10 种新端点的 POST，今天全部 404 → 工具调用失败、thread_hint 缺失。上游看到的是「每个 `/responses` 都声明 `history_ingest_requested: true`、带 `history` / `notes` 工具、却从不调用 history/notes 端点」的账号——真实客户端只有在后端持续故障时才会呈现这种形状。
2. 压缩不再发 `/responses/compact` 或 `request_kind=compaction`，而是本地开新窗口：`window_number` +1、`context_window_id` 换新。Aether 现有合成只在 compaction 请求到达时前进窗口号（`codex_runtime_identity.rs` `resolve_window`），会把「压了 N 次却一直是 window 0」的形状送到上游。
3. `/responses` 的 input 里多一条 developer message，**明文**写着 first / current / previous context window id 与 agent 名。它们来自入站客户端，而 blob 里的 `context_window_id` 已被合成——两处不一致。

所以支持这个功能 = 打通两侧路由 + 让合成在新形状下仍然自洽。

## 2. 线上链路事实（证据）

- Aether 入站 Codex Pro 1 天 23221 条全部带 `cafecode-uid` / `cafecode-uname` / `x-trace-id`，`x-codex-turn-metadata` 15128、`x-codex-window-id` 15083、`session_id` 3556、`session-id`/`thread-id` 0——正是 sub2api 头白名单（`openai_gateway_service.go` `openaiAllowedHeaders`：`session_id`、`conversation_id`、`x-codex-turn-metadata`、`x-codex-window-id`、`x-codex-installation-id`… 不含 dash 头）与 `applyCafecodeIdentityHeaders` 的产物。
- sub2api 路由（`backend/internal/server/routes/gateway.go:205-232`）：`/responses`、`/responses/*subpath`、`/alpha/search`、GET `/responses`（WS），同一组挂在 `/v1`、根、`/backend-api/codex` 三个前缀下。**没有** alpha/history、alpha/notes。
- sub2api alpha/search（`handler/openai_alpha_search.go`、`service/openai_alpha_search.go`）：要求 body `model`；OAuth 账号 → `https://chatgpt.com/backend-api/codex/alpha/search`，APIKey 账号 → `buildOpenAIEndpointURL(base, "/v1/alpha/search")`（`openai_endpoint_url.go:8-19`，去掉 base 尾部 `/v1` 后拼）；能力 `OpenAIEndpointCapabilityAlphaSearch`（`account.go:76`，OAuth/APIKey 都入候选）；粘性 `GenerateSessionHashWithFallback(c, nil, body.id)`。
- Aether：`/v1/alpha/search` 是 `openai:search` 格式（`constants.rs:139`、`control/route/ai.rs:65`、`FormatId` 别名 `id.rs:105-110`），上游 URL `{base_url}/alpha/search`（`request_url/mod.rs:234`），决策在 `planner/standard/openai/responses/decision/request.rs:990-1180`，同步执行 `openai_search_sync`，计费 `search_price_per_request`。Codex Pro / Plus / Free 三个号池各有 `openai:responses`、`openai:responses:compact`、`openai:search`、`openai:image` 四行端点，base_url 全是 `https://chatgpt.com/backend-api/codex`。
- Aether 对未知路径返回 404（`classify_control_route`，`control/route/mod.rs:155-180`，不做前缀归一）。

## 3. 功能开启后客户端的线上形状（codex-rs `07f18d5f`）

### 3.1 门槛（`core/src/session/token_budget.rs`、`features/src/lib.rs:324,776,829,1594`）

`apply_experimental_context` 同时满足才把 `token_budget.use_history_notes_extension` 置 true：内置 OpenAI provider 且 `base_url` 为 None 或以 `/backend-api/codex` 结尾；ChatGPT 登录态；客户端**自己的** id_token 载荷里 plan ∈ Plus / Pro / ProLite（只解码不验签，手写 auth.json 也过）；TokenBudget 启用（config 里 `features.context_management.experimental_mode = true` 会显式打开，不受 models.json 里 `token_budget.enabled` 缺省影响）。

对本链路的含义：sub2api 的 `https://<host>/backend-api/codex` 组正好满足 base_url 条件；Aether 的 `/v1` 路径客户端看不到。

### 3.2 十个端点（`ext/history-notes/src/backend.rs:30-95`、`tools.rs`）

| 端点（POST `{base}/…`） | 触发 | 关键参数 | 加密参数头 |
|---|---|---|---|
| `alpha/history/v2/list_windows` | 模型调用 `history.list_windows` | `agent_name?` | 否 |
| `alpha/history/v2/list_items` | `history.list_items` | `window_id?`, `agent_name?` | 否 |
| `alpha/history/v2/read_item` | `history.read_item` | `item_id`, `window_id`, `agent_name?` | 否 |
| `alpha/history/v2/search_contents` | `history.search_contents` | `query`, `window_id?`, `agent_name?` | **是** |
| `alpha/notes/v2/list_files_by_prefix` | `notes.list_files_by_prefix` | `prefix?` | 否 |
| `alpha/notes/v2/read_file` | `notes.read_file` | `path` | 否 |
| `alpha/notes/v2/search_contents` | `notes.search_contents` | `query`, `path?` | **是** |
| `alpha/notes/v2/append_to_file` | `notes.append_to_file` | `path`, `content` | **是** |
| `alpha/notes/v2/write_file` | `notes.write_file` | `path`, `content` | **是** |
| `alpha/notes/v2/thread_hint` | 客户端构建**完整上下文**时自动调用（新 thread 第一轮 `/responses` 之前；每次本地开新窗口后再调） | `{}` | 否 |

- body = 工具参数对象 + 客户端注入的 `"context": {"session_id": <session 级 id>, "current_agent_name": <agent 路径，根为 "/root">}`（`backend.rs:43-44`）。`session_id` 取 `session_store.level_id()`（thread_hint：`extension.rs:113`；10 个工具构造：`extension.rs:172`），即 codex **session** id——根 thread 上等于 root thread id，也等于 `/responses` 的 `prompt_cache_key`（`core/src/client.rs:540-552`：非 Internal 源时 `responses_metadata.session_id`）。
- **没有 `model` 字段**（对比 `alpha/search` body 有 `id` + `model`）。
- 响应必须是 JSON（`serde_json::from_slice`），否则工具报错；超时 35 s。thread_hint 失败 → 提示行省略并上报 analytics，不影响 `/responses`。
- history 的 `window_id` / `item_id` 是**服务端返回的不透明 id**（工具描述要求原样回传；`[id: …]` 后缀由服务端生成，codex-rs 源码里没有）。notes 路径是虚拟路径：相对路径落在当前 agent 的 `<agent_name>/notes`，绝对路径可读其他 agent。

### 3.3 请求头（`backend.rs:60-80`、`codex-api/src/provider.rs` `build_request`、`apply_auth`）

只有 provider 缺省头（`User-Agent`、`originator`、`version` 等客户端固定头）+ `Authorization: Bearer` + `chatgpt-account-id` + `Content-Type: application/json` + `x-openai-tool-output-truncation-policy`（JSON 字符串）+ 四个加密端点上的 `x-openai-encrypted-tool-arguments: true`。**没有** `x-codex-turn-metadata`、`session-id`、`thread-id`、`x-codex-window-id`、`x-client-request-id`、`x-codex-installation-id`。

### 3.4 `/responses` 变化

- tools 多出 `history` / `notes` 两个 namespace 与 `new_context` / `get_context_remaining`。
- turn-metadata blob 多 `history_ingest_requested: true`（`core/src/session/session.rs:625`，该 thread 所有 responses metadata 都带）。Aether `.109` 的 `request_identity_blob` 已原样复制该键（`BLOB_PASS_KEYS`）。
- input 里多一条 `role: developer` 的独立消息（kind `token_budget.context_window`，`core/src/context/token_budget_context.rs:41-74`），正文由 `<context_window>` / `</context_window>` 标记包住（`protocol/src/protocol.rs:137-138`），行：`Agent name: {agent_path}`、`First context window id: {uuid}`、`Current context window id: {uuid}`、`Previous context window id: {uuid}`（有前一窗口时）、以及 thread_hint 返回的提示文本。三个 uuid 就是 `AutoCompactWindowIds {first_window_id, previous_window_id, window_id}`（`core/src/state/auto_compact_window.rs:5-9`），`window_id` 同时写进 blob `context_window_id`。另有一条 guidance 消息（kind `token_budget.context_window_guidance`）。

### 3.5 压缩变为本地

`start_new_context_window`（`core/src/state/session.rs:266`）：`window_number` +1、mint 新 `window_id`（v7），不再发 `/responses/compact`，也没有 `request_kind=compaction` 的 `/responses`。下一轮 `/responses` 直接带新的 `window_number` / `context_window_id` / `x-codex-window-id`，developer message 里 Previous = 旧 Current，并再次调用 thread_hint。

### 3.6 时序（单个新 thread）

```
client                      sub2api                     Aether                      ChatGPT
  | POST …/alpha/notes/v2/thread_hint {context:{session_id:S, agent:/root}}
  |─────────────────────────>|  任选一个 Aether 账号转发   |  (S 尚无绑定) 选 key K，绑 S→K，合成 S→S'  ──>|
  |<── {hint}  ──────────────|<───────────────────────────|<──────────────────────────|
  | POST …/responses (blob.session_id=S, prompt_cache_key=S, dev-msg ctx ids c0)
  |─────────────────────────>|── 转发 ───────────────────>|── S→K，S→S'，c0→c0' ──────>|
  |  … 若干 turn，模型调用 history.* / notes.* → 同样 转发→K→S' …
  |  本地压缩：window 1，ctx c1；再调 thread_hint；下一轮 /responses 带 window_number=1
  |─────────────────────────>|── 转发 ───────────────────>|── 见入站 wn 0→1，合成窗口 +1，c1→c1' ──>|
```

要点：thread_hint 的返回文本会被写进随后 `/responses` 的 developer message。若 thread_hint 打到 ChatGPT 账号 A 而 `/responses` 打到账号 B，B 会收到自己从未产生的提示文本——所以**Aether 侧绑定一致是硬约束**，不是优化。sub2api 层不存在这个问题：它的账号全是 Aether 的 API-Key，无论选哪个，请求都进同一个 Aether、由同一套绑定决定 ChatGPT 账号。

## 4. 现状缺口

### 4.1 sub2api（`cafecode-v0.0.72`）
- 三个前缀组都没有 alpha/history、alpha/notes 路由 → gin 404。
- alpha/search 的 handler 强依赖 body `model`（校验、channel mapping、`classifyNoAccountErrorFromGin`），不能直接复用。
- 粘性：sub2api 对 `/responses` 用 header `session_id` → `conversation_id` → body `prompt_cache_key` 作种子；history/notes 请求没有这三者。但 codex 组内账号全是对接 Aether 的同质 API-Key 账号，选哪个都进同一个 Aether，**sub2api 层不需要为这些端点做粘性**——真正的账号归属在 Aether（§7.4）。
- 调度器对空 `requestedModel` 是放行的（`openai_gateway_service.go:1407,1458` 都是 `requestedModel != ""` 才做模型支持过滤；`:519` channel mapping 空 model 直接跳过），无需改调度器空 `sessionHash` 同样放行：`openai_account_scheduler.go:351,1381` 与 `openai_gateway_service.go:1331,1359` 都是 `sessionHash != ""` 才读/写粘性，传空串即「无粘性」。
- alpha/search 会跨账号 failover（`failedAccountIDs` 循环）。对 history/notes 沿用即可：换到另一个 Aether 账号仍是同一个 Aether，无副作用。

### 4.2 Aether（`backend-v0.7.109`）
- 路由不认识 → 404；`FormatId` 无对应格式；没有上游 URL 构造；没有 plan kind；执行运行时没有同步 JSON 转发的对应项（`openai_search_sync` 是最接近的）。
- 所有 `ai_public` 路径都要求 `model`（embeddings / rerank / messages 都有），没有「无 model 只认绑定」的候选选择。
- 号池头处理是黑名单（`CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST`：anthropic-version、x-amz-*、x-oai-attestation、x-trace-id），对新表面不够：`cafecode-uid` / `cafecode-uname` / `session_id` 等会随 same-format 透传（`build_complete_passthrough_headers_with_auth`）到 ChatGPT。
- 计费没有对应键。

### 4.3 会话身份合成（Codex Pro 池已开启）
- body `context.session_id` 是入站 root session；不改写就把真实 thread id 送到上游，与 `/responses` 上的合成 id 对不上。
- `resolve_window(…, compaction: bool, …)`（`codex_runtime_identity.rs:1284-1335`）只在 compaction 请求时 `number + 1`，本地压缩下永远 0。`ThreadWindow` 只存当前 `context_window_id`，没有历史。
- developer message 三行 ctx id 与 `Agent name` 行不在改写范围。
- thread_hint 早于第一轮 `/responses`，而 root 的 freeze / mint 目前只在 `HttpResponses` 表面发生。

## 5. 设计原则

1. **真实客户端形状优先**：出站一律模仿「同一个 0.153.x codex 在同一账号上开了这条 thread 并打开了该功能」的样子；宁可返回错误也不制造上游看得见的矛盾。
2. **模型无关的请求只跟绑定走（在 Aether）**：history/notes 没有 `model`，也不该有；ChatGPT 账号 = 这条 session 被 `/responses` 绑到的 key。第一个到达的 thread_hint 负责建立绑定。sub2api 是纯转发层：其账号全是 Aether 的同质 API-Key，不参与账号归属，不做粘性。
3. **只改身份，不改内容**：body 只碰顶层 `context`；不解密、不解析加密参数；响应不改；`/responses` 只多改 developer message 里那几行。
4. **单一窗口号来源**：入站 blob 有 `window_number` 就以它为准；只有旧客户端（0.146/0.147，无该键）才退回「compaction 请求 +1」。
5. **关闭即透传**：合成未开启的号池，`context` 原样；无 Codex 头的非 codex 客户端不受影响（这些端点只有 codex 调）。
6. **两侧改动各自最小**：sub2api 只做「多 10 条路由 + 一个无 model 的 handler」；Aether 只加一个格式家族，复用 search 的 planner / 执行 / 审计骨架。

## 6. sub2api 侧改动

### 6.1 路由（`routes/gateway.go`）
在现有三个前缀组（根、`/v1`、`/backend-api/codex`）各挂：
```
POST /alpha/history/v2/{list_windows|list_items|read_item|search_contents}
POST /alpha/notes/v2/{list_files_by_prefix|read_file|search_contents|append_to_file|write_file|thread_hint}
```
用显式 10 条（不用通配），中间件与 `/alpha/search` 同一套（bodyLimit、clientRequestID、usageResponseTiming、opsErrorLogger、endpointNorm、鉴权、group 校验）。未列出的子路径保持 404。

### 6.2 handler `OpenAIGateway.HistoryNotes`（新文件 `handler/openai_history_notes.go`）
以 `AlphaSearch` 为模板，差别：
- 不要求、不读取 `model`；`setOpsRequestContext(c, "", false)`；跳过 channel mapping。
- 不解析 body（连 `context` 也不读）：body 是 JSON 对象即可，否则 400 `invalid_request_error`。
- **不做粘性**：`sessionHash` 传空串，不调用 `GenerateSessionHash*`，不 `BindStickySession`。理由：组内账号全是对接 Aether 的同质 API-Key 账号，选哪个都进同一个 Aether，ChatGPT 账号归属由 Aether 的绑定决定（§7.4）。
- 账号选择：`SelectAccountWithSchedulerForCapability(ctx, groupID, "", "" /*sessionHash*/, "" /*model*/, failed, OpenAIUpstreamTransportHTTPSSE, OpenAIEndpointCapabilityHistoryNotes, false, PlatformOpenAI)`。
- failover：沿用 alpha/search 的 `failedAccountIDs` 循环（换账号 = 换一个 Aether API-Key，无副作用）。Aether 返回的 4xx 视为终态原样透传，不重试。
- 并发槽、计费资格检查、`OpsLatency` 与 AlphaSearch 相同。
- 转发：`ForwardHistoryNotes(ctx, c, account, op, body)`。

### 6.3 能力（`service/account.go`）
新增 `OpenAIEndpointCapabilityHistoryNotes = "history_notes"`；`SupportsOpenAIEndpointCapability` 中与 `alpha_search` 同一分支（OAuth + APIKey 都入候选；老账号只声明 `chat_completions` 时同样视为支持，与 alpha_search 的兼容分支一致）。

### 6.4 上游 URL（`service/openai_history_notes.go`）
- OAuth：`https://chatgpt.com/backend-api/codex/alpha/{history|notes}/v2/{op}`。
- APIKey（Aether）：`buildOpenAIEndpointURL(base, "/v1/alpha/{history|notes}/v2/{op}")`。
- `op` 只接受 6.1 的白名单常量，拒绝其它值（防路径拼接）。

### 6.5 头处理（白名单，写死）
向上游只发：`Accept: application/json`、`Content-Type: application/json`、`Authorization`（账号凭据）、OAuth 时 `ChatGPT-Account-ID`、`User-Agent`（账号自定义 UA > 入站 UA > `codexCLIUserAgent`，与 alpha/search 同顺序）、`Originator`（入站 > `codex_cli_rs`）、`Version`（入站 > 派生）、`x-openai-tool-output-truncation-policy`（入站原值，缺失时**不补**）、`x-openai-encrypted-tool-arguments`（入站有才发）。对 Aether 账号另加 `cafecode-uid` / `cafecode-uname`（`applyCafecodeIdentityHeaders`，Aether 侧会剥掉，见 7.6）。**不发** `X-Codex-Turn-Metadata`、`session_id`、`conversation_id`、`OpenAI-Beta`、`X-Codex-*`。

### 6.6 body
原样转发字节（不做 `prompt_cache_key` 剥除之类的 responses 逻辑）。大小走 bodyLimit。

### 6.7 响应
状态码、`Content-Type`、body 原样透传；`writeOpenAIPassthroughResponseHeaders` + `x-aether-upstream-disposition` 透传与 alpha/search 相同；`UpdateCodexUsageSnapshotFromHeaders`（OAuth 限额头）沿用。

### 6.8 用量 / 计费
`recordHistoryNotesUsage`：每次请求一条，`model` 记为空串、`request_type` sync、endpoint 记 `alpha/{history|notes}/v2/{op}`；费用沿用 alpha/search 的「按请求」口径（组价格表里增一项 `history_notes_per_request`，缺省 0）。不入 token 计数。

### 6.9 ops 端点归一化
`endpointNorm` 把 10 条路径归为 `alpha/history/*`、`alpha/notes/*` 两类，避免按 op 打散统计。

### 6.10 测试
- 路由表：三个前缀 × 10 个 op 都可达，其它 `alpha/*` 404。
- handler：无 `model` 不报错；非 JSON 对象 body → 400；不写、不读 sticky（`sessionHash` 为空）；Aether 5xx 时换账号重试、4xx 不重试。
- 转发：APIKey 账号 base 为 `https://h/v1` 与 `https://h` 都得到 `https://h/v1/alpha/notes/v2/thread_hint`；头集合恰为 6.5 白名单；加密头只在入站有时出现。

## 7. Aether 侧改动

### 7.1 格式与路由
- `FormatId::OpenAiContext`，id 字符串 `openai:context`；别名 `openai_context`、`alpha_history`、`alpha_notes`、`/v1/alpha/history`、`/v1/alpha/notes`（`crates/aether-ai-formats/src/formats/id.rs`）。单一格式覆盖两个 namespace；op 由路径决定，不进格式 id。
- `constants.rs` 公开路径表加 10 条 `/v1/alpha/{history,notes}/v2/{op}`。
- `control/route/ai.rs`：`POST` 且路径匹配 10 条之一 → `classified("ai_public", "openai", "context", "openai:context", true)`；planner 从路径尾段取 `op`（枚举 `ContextOp`，含 namespace）。其它 `/v1/alpha/*` 仍 404。**不加 `/backend-api/codex` 前缀**。

### 7.2 端点配置
- 号池需新增一行端点 `api_format = openai:context`、`api_family = openai`、`endpoint_kind = context`、base_url 与其它行相同。Codex Pro / Plus / Free 各加一行（操作员动作，另行授权）。
- 候选契约：与 search 相同，provider 必须有该格式端点（`openai_context_endpoint_required`），不回落到 `openai:responses` 行。
- `custom_path` 对该格式不支持：admin normalize 拒绝非空 `custom_path`（明确报错，不静默忽略）。
- 前端端点类型下拉、api_format 列表、端点校验同步加 `openai:context`。

### 7.3 计划构建
新 `LocalOpenAiContextDecision`（放在 `planner/standard/openai/responses/decision/` 旁，模板 = search 的 `request.rs:990-1180`）：
1. 亲和与候选（7.4）。
2. `prepare_header_authenticated_candidate`（号池 key 鉴权）。
3. body：`serde_json::Value` 原样；只在 8.1 生效时改 `context`。
4. `build_standard_provider_request_headers` → `apply_codex_openai_responses_special_headers` **不调用**（那是 responses 的短头/prompt_cache_key 逻辑）→ `apply_codex_pool_stable_client_headers`（account profile UA / originator / `version` 随 UA）→ `apply_codex_pool_runtime_identity(..., Surface::ContextBody)` → `normalize_openai_context_headers`（7.6 白名单）。
5. `ExecutionStrategy::LocalSameFormat`，新同步 kind `openai_context_sync`（`planner/route.rs`、`execution_runtime/fallback.rs`；行为 = `openai_search_sync`：一次 POST，JSON 响应透传，状态码透传，不解析 SSE）。
6. 超时：请求级 35 s 上限（客户端 35 s 就放弃，更长没有意义），取 `min(provider.request_timeout_secs, 35)`。

### 7.4 候选选择：无 model，只认绑定
- `client_session_affinity.rs` Codex 适配器加一条：格式为 `openai:context` 时 root_session = body `context.session_id`（字符串，非空）。其余 detect / scope 逻辑不变，因此走的是与该 thread `/responses` 完全相同的 scope（`/responses` 的 root_session = blob `session_id`，见 `CodexSessionIdentity::root_session`：`session_id.or(thread_id)`）。
- 候选集合 = API key 允许的 provider 中有启用 `openai:context` 端点且有可用 key 的；**不做模型映射、不做模型权限**（该请求没有 model）。
- 绑定命中 → 只保留绑定的 provider+key；该 key 不可用（禁用 / 健康分降级 / 限流）→ 直接返回 503 `bound_account_unavailable`，**不换 key、不重绑**。
- 未绑定 → 常规池调度（新入口：`select_candidate_without_model`，只跑 provider/key 层的权重、并发、健康分，跳过 model 相关过滤），成功后写与 `/responses` 同一把 sticky 绑定，使随后的 `/responses`（blob `session_id` 相同）落到同一 key。
- 上游 4xx/5xx 原样回给客户端，不 failover（数据在 ChatGPT 账号本地，换 key 只会拿到别人的历史）。sub2api 层的 failover 只是换一个 Aether API-Key 再进同一个 Aether，与此不冲突。

### 7.5 上游 URL
`build_local_openai_context_upstream_url(transport, op)` → `{base_url}/alpha/{history|notes}/v2/{op}`；同 `build_transport_request_url` 体系但忽略 `custom_path`（7.2 已拒绝）。

### 7.6 头（白名单）
`normalize_openai_context_headers` 只保留：`authorization`、`chatgpt-account-id`、`content-type`、`accept`、`accept-encoding`、`user-agent`、`originator`、`version`、`x-openai-tool-output-truncation-policy`、`x-openai-encrypted-tool-arguments`。其余一律删除（含 `cafecode-*`、`x-trace-id`、`session_id`、`conversation_id`、`x-codex-*`、`x-client-request-id`、`anthropic-*`）。`accept` 缺失时补 `application/json`。`CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST` 不动。

### 7.7 body
只在 8.1 条件下改顶层 `context`；不新增键、不删键、不动其它字段；非对象 body 直接 400。

### 7.8 执行与响应
同步 POST；响应 body 原样（不解析 JSON、不改写）；`x-aether-upstream-disposition` 沿用；上游非 JSON 也原样透传（客户端自己报错）。

### 7.9 计费
`crates/aether-billing/src/pricing.rs` 加 `context_tool_price_per_request`（与 `search_price_per_request` 同形，缺省 0）；usage 行 `request_type = context`、`api_format = openai:context`、`model` 空、token 全 0。

### 7.10 审计 / 用量
`usage_http_audits` 复用 body capture；`usage` 元数据记 `operation = {namespace}/{op}`（进 `metadata` JSON，不加列）。

### 7.11 前端
号池端点类型加 `openai:context`；计费页加价格键；「会话身份合成」卡片文案补一句「history/notes 端点的 `context.session_id` 与 developer message 窗口 id 同步改写」。

### 7.12 测试
- 路由：10 条 POST → `openai:context` / op 正确；GET、`/alpha/...`（无 `/v1`）、`/backend-api/codex/...`、未知 op → 404。
- 候选：绑定命中只留绑定 key；绑定 key 不可用 → 503 不重绑；未绑定 → 调度 + 写绑定；随后 `/responses`（blob `session_id` 相同）命中同一 key。
- 头：白名单外一律删；`version` 随 UA；`x-openai-*` 两头透传；加密头缺失不补。
- URL：三个 namespace/op 组合；`custom_path` 非空被 normalize 拒绝。
- 计费：每请求计价一次，token 0。

## 8. 与会话身份合成的交互（核心）

前提：`pool_advanced.codex_runtime_identity.enabled` 为 true 的号池（今天只有 Codex Pro）。以下都在 `apps/aether-gateway/src/codex_runtime_identity.rs`。

### 8.1 `context` 改写（新表面 `CodexRuntimeIdentitySurface::ContextBody`）
- 入站身份只有 `context.session_id`（= root session，无 turn）。走 `resolve_inner` 的 root 分支：freeze 命中 → 出站 thread；miss → `assign_thread` mint（占当天 thread 额度，同 `/responses`）。**不**解析 turn、不 touch turn 槽。
- 改写：`context.session_id` → 出站 thread id（root 上 session == thread）；`context.current_agent_name` → `"/root"`（子 agent 折叠规则与 blob `agent_name` 一致）。只在值等于已知入站 id / 以 `/root` 开头时改，否则原样。
- 明文 `agent_name` 参数（history 四个工具可带，`tools.rs:148-184`）：值以 `/root/` 开头（子 agent）→ 改为 `/root`；相对名无法判断，原样。加密端点的参数不动（残余，见 §12）。
- 关闭或 store 不可用 → 原样透传（与其它表面的 `store_unavailable_falls_back_to_passthrough` 一致）。

### 8.2 thread_hint 作为 root 首次到达
thread_hint 是新 thread 的第一个上游请求（3.6）。它在 8.1 中 mint 的出站 thread 会被随后 `/responses` 的 root freeze 命中——天然一致，不需要特殊处理。需要确认的只有：`assign_thread` 的名册 `ZADD`/score 在无 turn 的请求上也要写（当前 `resolve_turn` 才 touch 名册的规则改为「root mint 时也写初始 score」），否则一个只发了 thread_hint 就被用户放弃的 thread 会占序号但不在 LRU 名册里。

### 8.3 窗口号跟随入站
`resolve_window` 签名改为 `resolve_window(store, scope, outbound_thread_id, inbound: WindowSignal, ttl, now)`，其中
```
enum WindowSignal { Number { inbound_thread_hash: String, number: u64 }, CompactionRequest, None }
```
- `ThreadWindow` 扩为 `{ number, context_window_id, windows: Vec<{n, ctx}>, inbound_last: BTreeMap<inbound_thread_hash16, u64> }`（`windows` 上限 64 条，超出丢最旧但保留 n=0）。
- `Number`：查 `inbound_last[hash]`；首次见到该入站 thread → 记录，不前进；`number > last` → 出站 `number += 1`（**每次只加 1**，不按差值跳——差值 >1 只可能来自折叠的多个子 agent 交错或丢包，真实单 thread 一次只压一次），mint 新 ctx 追加到 `windows`，更新 `inbound_last`；`number <= last` → 不动。
- `CompactionRequest`：仅当该出站 thread 从未收到过 `Number` 信号（`inbound_last` 为空，即旧客户端）时 `number += 1`；否则忽略（避免 remote compaction 与随后的 wn+1 重复计数）。
- `None`（memory、无 blob 的合成请求、ContextBody 表面）：只读当前值。
- 全部通过现有 `set_if_value` CAS 写回；并发下最多一次前进。
- 影响面：`request_identity_blob` 与 header 投影不变（仍读 `ThreadWindow.number` / `context_window_id`）。现有测试 `thread_window_advances_on_compaction_and_mints_context_lazily` 改为旧客户端分支，新增 8.3 三条路径。

### 8.4 developer message 三行 context window id
在 `HttpResponses` 与 `WsStepBody` 表面，对 body `input[]` 中满足「`role == developer` 且 content 文本含 `<context_window>` 开闭标记」的**唯一**一条消息做行级改写：
- `Current context window id: X` → 出站当前 ctx（= blob `context_window_id`）。
- `Previous context window id: Y` → `windows[n-1].ctx`；出站 n == 0 时**删除该行**（真实客户端窗口 0 没有 previous）。
- `First context window id: Z` → `windows[0].ctx`；若 `windows` 因 64 上限丢了 0 号（不会，0 号保留）或 thread 早于合成开启（无 0 号记录）→ 以出站 thread 的 v7 时间戳 +1 ms mint 一个并写入 `windows[0]`（确定性、只 mint 一次）。
- 只改这三行的 UUID 部分；其余行（thread_hint 文本、guidance）不动；找不到标记 → 不动。
- 为什么不直接跟随入站 ctx id：入站 ctx 是客户端本机 v7，时间戳可能早于出站 thread id 的时间戳（thread 早于合成开启），会呈现「窗口比 thread 还老」的不可能形状；而且与 blob 里已合成的 `context_window_id` 不一致。
- `windows` 的存在也保证了 Q5 里「ctx_ids == compactions + 1」的可核验性。

### 8.5 `Agent name` 行
同一条消息里 `Agent name: /root/…` → `Agent name: /root`（与 blob `agent_name` / `context.current_agent_name` 一致）。

### 8.6 不改的东西
- `history_ingest_requested`：原样复制（已在 `BLOB_PASS_KEYS`）。
- tools 数组、`new_context` / `get_context_remaining`、guidance 消息：不动。
- 上游响应（history 的 window/item id 是服务端 id，客户端原样回传，两端一致）。
- 加密参数体。
- sticky / 绑定 / fence / logical_turn_id / 用量：一律读入站（既有规则）。

### 8.7 LRU 复用与窗口单调
复用最久未用 thread（.108）时，`ThreadWindow` 随出站 thread 走：新 root 接管一条旧 thread 后，其入站 `window_number` 从 0 开始，`inbound_last` 里没有它的 hash → 首次只记录不前进 → 出站窗口号保持单调不回退；此后按 8.3 前进。ctx 历史保留，`First` 行仍指向该出站 thread 真正的 0 号窗口——与「同一 thread 一直在用」的真实形状一致。

### 8.8 Redis 结构变化
- `{prefix}:window:{outbound_thread_id}` 值从 `{number, context_window_id}` 变为 8.3 的扩展 JSON；旧值缺字段按 `serde(default)` 读（`windows` 空、`inbound_last` 空），首次写回补 `windows[0] = {0, ctx}`。向后兼容，回滚后旧代码忽略新字段。
- 无新键、无新 TTL 规则。

## 9. 不变量与验收

1. 同一入站 root 的 thread_hint / history / notes / `/responses` 在 Aether 出站落在同一 key、同一出站 thread（审计 join 校验）。
2. 出站 history/notes 请求头集合 ⊆ 7.6 白名单；body 除 `context` 外逐字节相同。
3. 号池开启时：`context.session_id` 与同 root `/responses` 出站 blob `session_id` 相同；`current_agent_name == "/root"`。
4. 出站 `window_number` 单调、每次 +1、`context_window_id` 随之更换；developer message 的 Current == blob `context_window_id`，Previous == 上一窗口，First == 0 号窗口；n == 0 无 Previous 行。
5. `history_ingest_requested` 原样；tools 原样。
6. 关闭合成的号池：所有 8.x 改写不发生，只做 6/7 的转发。
7. 客户端未开该功能时（今天全部流量）：`/responses` 形状与 .109 完全相同（无 `<context_window>` 消息 → 8.4/8.5 不触发；`window_number` 来源切换只影响有 compaction 的 thread，且与现状一致：远程 compaction 请求后下一轮入站 wn+1 → 出站 +1，一次）。

## 10. 观测

- 事件：`codex_context_request`（info：op、绑定命中/新建、状态码、耗时）、`codex_context_unbound_rejected`（warn：绑定 key 不可用）、`codex_rid_window_signal`（debug：`Number`/`CompactionRequest`、是否前进）、`codex_rid_devmsg_rewritten`（debug：改了哪几行）。
- runbook 新增 SQL：① 按 op 的请求量/状态分布；② 同 root 的 context 请求与 `/responses` 出站 key 是否一致；③ 出站 thread 的 `ctx_ids == wn_max - wn_min + 1`；④ developer message 抽样（body capture）核对三行与 blob 一致；⑤ `history_ingest_requested` 出现率（功能采用率）。
- sub2api：ops 面板按 `alpha/history/*`、`alpha/notes/*` 归类的成功率与延迟。

## 11. 上线顺序与门禁（实现之后，仍须逐步授权）

1. Aether：实现 §7 + §8，定向测试 + `review.sql` 扩展跑绿，tag → CI → ghcr → 更新线上（授权）。上线后功能对现有流量的唯一可见变化是 8.3 的窗口号来源，先按 §9.7 复核 24 h。
2. 操作员在 Codex Pro / Plus / Free 加 `openai:context` 端点行、设 `context_tool_price_per_request`（授权）。
3. sub2api：实现 §6，其自身流程发布（授权）。发布前 Aether 已就位，所以不会出现 sub2api 转发到 404 的窗口期。
4. 验证：用一台测试机的 codex（`features.context_management.experimental_mode = true`，`base_url = https://<sub2api>/backend-api/codex`，Pro 计划的 ChatGPT auth.json）在测试 group 上跑一条会触发压缩的长 thread，核对 §9 全部不变量与 §10 的 SQL。
5. 顺序 1→3 之间，客户端开该功能的行为与今天相同（sub2api 404）。

## 12. 开放问题（实现前需用真实账号核实）

1. 上游对未知 session 的 history/notes 响应形状（空列表还是 404），以及 `list_windows` 返回的 window id 是否就是客户端的 `context_window_id`。若是后者，8.4 的映射方向仍成立（客户端只回传服务端给的值），但需确认服务端不会因为看到「合成 ctx 与 ingest 时的 ctx」不一致而拒绝——ingest 走的是 `/responses`，blob 里已经是合成 ctx，理论上一致。
2. 加密参数（4 个端点）里可能带 `agent_name` / notes 绝对路径 `/root/<sub>/notes/...`——折叠后这些名字上游不存在。现状：残余，无法改写；观察是否真实出现。
3. 子 agent 折叠对 history 质量的影响（全部 ingest 到 `/root`）。若下游大量使用子 agent，需评估是否在该功能开启的 thread 上保留 agent 树——那是对既定折叠规则的修改，超出本计划。
4. `SessionSource::Internal`（memory consolidation）线程是否会调用 history/notes；其 `prompt_cache_key` 形如 `internal_<source>:<parent>`，与 `context.session_id` 不同源，若真会调用，Aether 侧的 `/responses` 绑定（读 blob `session_id`）与 `context.session_id` 仍同源，不受影响。目前判断：这类线程不开 token_budget，不会调用；实现时用真实客户端确认。
5. WS 传输：`/responses` 走 WS 时 8.4/8.5 在 `WsStepBody` 表面做；history/notes 本身永远是 HTTP。需要确认 WS 首步 body 的 `input` 完整包含 developer message（codex-rs WS 首步发完整 input，后续步只发增量——增量步里不会再出现该消息）。
6. sub2api 侧 `requestedModel = ""` 在 `classifyNoAccountErrorFromGin` 等错误分类分支的文案是否合理（不是功能问题）。
7. codex-api 传输层对非 2xx 的处理（是按状态码报错还是把 body 交给 JSON 解析）本次未定位到源文件；影响的只是 §6.2/§7.4 「上游错误原样透传」时客户端看到的报错文案，不影响设计。

## 附录 A：头对照

| 头 | 真实 codex → 上游 | sub2api → Aether（6.5） | Aether → ChatGPT（7.6） |
|---|---|---|---|
| `authorization` | Bearer（用户） | Aether API key | 号池 key |
| `chatgpt-account-id` | 有 | 无 | 号池 key 的 account id |
| `content-type` | application/json | 同 | 同 |
| `accept` | reqwest 缺省 | application/json | application/json |
| `user-agent` / `originator` / `version` | 客户端固定 | 账号自定义 UA > 入站 | account profile UA，`version` 随 UA |
| `x-openai-tool-output-truncation-policy` | 有 | 透传 | 透传 |
| `x-openai-encrypted-tool-arguments` | 4 个端点 | 透传（有才发） | 透传（有才发） |
| `x-codex-turn-metadata` / `session-id` / `thread-id` / `x-codex-window-id` / `x-client-request-id` | **无** | 无 | 无（白名单外） |
| `cafecode-uid` / `cafecode-uname` / `x-trace-id` | 无 | 有（sub2api 自加） | **删** |

## 附录 B：现状取证 SQL（`docker exec -i aether-postgres psql -U postgres -d aether`）

```sql
-- 功能采用率：出站 blob 带 history_ingest_requested 的请求数（应为 0 直到有客户端开启）
select count(*) from usage u join usage_http_audits h on h.request_id=u.request_id
where u.created_at > now() - interval '14 day' and u.provider_name like 'Codex%'
  and h.provider_request_headers::jsonb->>'x-codex-turn-metadata' like '%history_ingest_requested%';

-- 实现后：同 root 的 context 请求与 /responses 是否落在同一 key（按 usage.key_id / provider_key_id 列名以实现时为准）
-- select ... where u.api_format in ('openai:context','openai:responses') group by root, key having count(distinct key) > 1;
```
