# Grok → 官方 xAI OAuth API 迁移 + 号池现代化（aether）

> 设计文档。对应参考实现：CLIProxyAPI (`/opt/stacks/cpa`) 与 `sub2api` (`/opt/stacks/sub2api`) 的 `xai` 链路。

## 背景（为什么做）

aether 现有的 grok 接入是**旧版 grok.com 网页抓取**（grok2api 风格）：会话 cookie（`sso`/`sso-rw`/`cf_clearance`）、通过 `wreq` 做 Chrome 指纹伪装、硬编码 `x-statsig-id` 与 Sentry `baggage`、一整套自定义 `app-chat` 请求/响应运行时（约 4157 行），**usage token 是伪造的（chars/4）**，且**号池贡献很薄**（复用 aether 的通用调度器，但只根据「魔法配额总数」推断粗粒度 quota/tier 信号）。这套方案脆弱（grok.com 前端一改就失效），也拿不到真实的限流/用量数据。

两个参考实现——**CLIProxyAPI** 和用户自己的 **`sub2api`**——都通过**官方 xAI OAuth API** 访问 Grok：`auth.x.ai` OAuth 2.0 + PKCE（公共 **Grok CLI** client `b1a00492-073a-47ea-816f-4c329264a828`，scope `openid profile email offline_access grok-cli:access api:access`），随后用可刷新的 **Bearer** token 调用 `https://api.x.ai/v1`（OpenAI 兼容的 `/responses` + `/chat/completions`）。这条链路同样落到 SuperGrok/Heavy **订阅**上，但更健壮：拿到**真实** `x-ratelimit-*` 响应头、**真实** usage token、singleflight token 刷新、以及按账号的冷却矩阵。`sub2api` 是用户的 fork，且已经把 **aether** 当作它下游的 OpenAI 兼容「号池网关」——所以 aether 正是应当承载现代 grok 号池的组件。

**目标**：把 aether 的 grok transport 完全迁移到官方 xAI OAuth API，并现代化其号池，最大化复用 aether 现有的通用池/健康/热池基础设施（不新建池引擎）。

## 已确定的决策

- **完全迁移**到官方 xAI OAuth API；**下线** grok.com 网页抓取路径。
- **保留 `provider_type = "grok"`**——不改名为 `xai`，不做 provider_type 字符串的 DB 数据迁移。（"Grok" 保留为模型系列名，底层 vendor 是 xAI。）
- **仅 OAuth 订阅账号**（管理界面同时提供 device flow 与 PKCE 回调 URL 手工回填，均使用可刷新 Bearer）。本轮不做直连 API-key 模式。
- **先做核心**：Chat + Responses + 完整号池现代化。**推迟**多媒体（图像/视频生成与编辑）、composer 图像输入桥接、主动配额探测的精细项、以及按模型的 tool 过滤。

> **2026-07-15 修订（对齐 CLIProxyAPI v7.2.77）**
>
> 首版实现参考的是 cpa v7.1.75（2026-06-13）。上游随后有约 38 个 xai/grok 提交，其中三处与本文档的原始决策冲突，已按上游语义修正：
>
> 1. **base_url**：订阅（OAuth）账号的非多媒体 chat 走 `https://cli-chat-proxy.grok.com/v1`，**不是** `api.x.ai/v1`；后者只服务 `using_api=true` 或 API-key 模式，以及多媒体/websocket。走 chat-proxy 时必须带 Grok CLI 身份头（`X-XAI-Token-Auth: xai-grok-cli`、`x-grok-client-version`、`User-Agent: xai-grok-workspace/<ver>`）。**这解决了原「风险 #3」。**
> 2. **登录方式**：CPA v7.2.77 已改为 **RFC 8628 device flow**（`6e819ab6` 删除了 `pkce.go`），aether 以此作为管理界面的默认推荐方式，同时保留 PKCE 授权码流程。
>     - **与上游的有意偏差**：管理界面允许切换到与 Codex 相同的 PKCE 手工回填流程。浏览器即使无法真正连接 `127.0.0.1:56121`，地址栏仍会包含 `code` 和 `state`；管理员复制完整回调 URL 并粘贴回 aether 即可完成交换，因此服务器无需直接接收 loopback 回调。`capabilities` 如实标注两者都支持。
> 3. **端点解析**：device authorization/token 端点由 **OIDC discovery**（`https://auth.x.ai/.well-known/openid-configuration`）解析。device authorization 端点没有硬编码默认值，discovery 失败即硬失败——不猜测 URL。PKCE 流程仍分别读取 `XAI_OAUTH_AUTHORIZE_URL` 与 `XAI_OAUTH_TOKEN_URL`。
>     - discovery 文档来自网络，因此端点需校验来源：**官方 discovery（https + x.ai）可指定任意 x.ai 主机**（对齐 cpa，容忍 xAI 换子域）；**被 `XAI_OAUTH_DISCOVERY_URL` 覆盖的 discovery 只能描述它自己的 origin**——覆盖只能收窄凭据的可达范围，不能扩大。轮询时用同一规则复核 session 中 pin 的 token 端点。
>
> 未变：client_id `b1a00492-…`、scope 集合、issuer `auth.x.ai`——上游至今没动过。

## 目标架构："grok = codex 形态的 xAI OAuth provider"

Codex 已经是一个 OAuth-Bearer、OpenAI-`responses` 形态的 provider，它走 aether 的**标准**路径，带 token 刷新、真实 usage 与池配额。Grok 应变成同样的形态，仅在 vendor 端点、模型目录、配额语义上不同。

关键简化：**不再把 grok 拦截进专用运行时，而是让它走标准 OpenAI chat/responses 路径**（按 provider 配置 `base_url = https://api.x.ai/v1` + Bearer OAuth + 刷新）。这样可以**删除**网页抓取 transport 构造器与自定义运行时，而不是去移植它们。

## 工作分解

### 1. 认证语义：把 grok 变成 Bearer-OAuth provider
- `apps/aether-gateway/src/provider_key_auth.rs`：把 `"grok"` 加入 `provider_uses_bearer_oauth_runtime()`（与 `codex`、`claude_code` 等并列），并**移除** `provider_uses_grok_session_runtime()` 及其 `RuntimeAuthKind::Unknown` 特例（grok.rs:129, 163）。结果：grok oauth key 解析为 `Bearer` 运行时认证，`can_refresh_oauth = true`（已由 `provider_key_can_refresh_oauth` 基于 auth_config 中的 `refresh_token` 判定，provider_key_auth.rs:86）。

### 2. Transport：用 Bearer + api.x.ai 替换网页抓取
- `crates/aether-provider-transport/src/grok.rs`：本文件基本**删除/掏空**。替换：
  - `GROK_DEFAULT_BASE_URL` → `https://api.x.ai/v1`（host 白名单 `api.x.ai`，可选 `cli-chat-proxy.grok.com`；需验证 grok-cli token 授权的是哪个——见「风险」）。
  - `resolve_grok_session_auth()` → 返回从 `auth_config.access_token` 读取的 `("authorization", format!("Bearer {access_token}"))`（删除 `grok_cookie_from_transport` 及所有 sso/cf/statsig cookie 组装）。
  - **移除** `build_grok_browser_headers`（statsig/baggage/sec-ch-ua/user-agent）、browser-profile/`wreq` 机制（`grok_browser_*`、chrome 版本白名单）、`build_grok_app_chat_body` / `grok_base_app_chat_payload` / `grok_mode_id_for_model`，以及 image-edit/app-chat body 构造器。grok 不再需要自定义 body——直接发送 OpenAI chat/responses payload。
  - grok 改用**默认 reqwest transport**（不再 `browser_wreq`），与其他 OpenAI provider 一致。

### 3. 路由：下线专用 grok 运行时
- 在 planner 请求构造器里**停止设置 marker** `x-aether-grok-runtime`、停止构造 app-chat body：`apps/aether-gateway/src/ai_serving/planner/standard/family/request.rs`（`is_grok && is_grok_text_provider_api_format` 分支，约 :323）、`.../standard/openai/chat/decision/request.rs`、`.../standard/openai/responses/decision/request.rs`、`.../passthrough/provider/family/request.rs`（image 构造器推迟）。grok 请求随后落到 codex/openai 相同的请求路径，`base_url = api.x.ai/v1` + Bearer 认证。
- **移除运行时拦截**：`apps/aether-gateway/src/execution_runtime/sync/execution.rs`（:1553）、`.../stream/execution.rs`（:930）、以及 `.../transport.rs`（:351）中的 marker 门控。**删除** `apps/aether-gateway/src/execution_runtime/grok.rs`（SSE `app-chat` 解析、Imagine WebSocket、`<grok:render>` 重写、`grok_usage_estimate`）——响应变为标准 OpenAI SSE 后全部作废。真实 usage 现在通过既有 OpenAI usage 映射（`aether-usage-runtime`）从上游 `usage` 对象获取，替代 chars/4 估算。

### 4. 号池现代化（增强既有 adapter；复用通用池）
- `crates/aether-provider-pool/src/providers/grok.rs`：**替换**「魔法配额总数」的 tier 推断（`grok_pool_tier_from_quota_bucket`、`GROK_QUOTA_WINDOWS_*`、`grok_mode_id_for_model`），改为**真实 xAI 限流数据**。用 xAI 响应头构建配额快照——移植 `sub2api/backend/internal/pkg/xai/quota.go` 的 `ObserveQuotaHeaders`/`ParseQuotaHeaders`：`x-ratelimit-{limit,remaining,reset}-{requests,tokens}`、`retry-after`、`x-subscription-tier`、`x-entitlement-status`。用这些数据（而非魔法总数）喂入 `PoolMemberSignals` 的 `quota_exhausted` / `quota_usage_ratio` / `quota_reset_seconds`。
- **上游错误的冷却矩阵**（移植 `sub2api/.../openai_gateway_grok.go:659` 的 `handleGrokAccountUpstreamError`），映射到 aether *既有*机制而非新建：`401` → `oauth_invalid_reason`「token 未授权/需重新认证」（约 10m）；`403` → `oauth_invalid_reason`「entitlement/订阅层级被拒」（约 30m）；`429` → 遵循 `Retry-After` 的限流冷却（喂入 `last_429_at` / 熔断器，复用 `aether-scheduler-core::health` 的 learned-RPM + `rate_limit_cooldown_seconds`）；`5xx` → 瞬时过载冷却（约 2m）。挂到标准路径的错误处理里（codex/openai 状态码已在那里翻译为池状态）。
- **可选，对标 codex**（可同一轮落地）：`default_scheduling_presets()` → `recent_refresh`，以及类比 `codex_quota_soft_threshold_exceeded`（codex.rs:353）的 grok 软阈值。下游的一切——评分（`aether-pool-core::scoring`）、调度器（`aether-pool-core::scheduler`）、**主动探测热池**（`apps/aether-gateway/src/maintenance/runtime/pool_quota_probe.rs`）、以及 `pool_member_scores` 持久化——已对 grok 无差别适用。

### 5. OAuth 登录 + token 刷新（对标 codex）
- 在 admin OAuth dispatch 中新增 xAI OAuth，镜像 codex 的 PKCE 登录/交换/刷新（定位 codex OAuth authorize/exchange/refresh 的实现——就是已经在刷新 codex/claude_code Bearer token 的那个子系统——并为 xAI 克隆一份）：authorize URL 在 `https://auth.x.ai/oauth2/authorize`（S256 PKCE、`response_type=code`、`plan=generic`、`referrer=aether`、redirect `http://127.0.0.1:56121/callback` 或 admin 粘贴 code 流程），token 交换/刷新在 `https://auth.x.ai/oauth2/token`（`grant_type=authorization_code|refresh_token`）。
- 改造 grok 导入 `apps/aether-gateway/src/handlers/admin/provider/oauth/dispatch/{import.rs,token_import.rs,helpers.rs}` + `oauth/provisioning.rs`：**停止写 `sso_token`/`auth_method:"sso_token"`**；改写 `auth_config` = `{access_token, refresh_token, id_token, expires_at, token_endpoint, client_id, scope, email, subscription_tier, entitlement_status, base_url}`（字段参照 `sub2api/.../grok_oauth_service.go` 的 `BuildAccountCredentials`）。去掉 browser 指纹打点（`grok_browser_transport_fingerprint_from_auth_config`）。
- 替换 admin 配额刷新：`apps/aether-gateway/src/handlers/admin/provider/oauth/quota/grok.rs` 当前 POST grok.com `/rest/rate-limits`。改为 xAI **1-token 探测**（`POST api.x.ai/v1/responses`，`{input:".", max_output_tokens:1, store:false}`，Bearer），纯粹为采集 `x-ratelimit-*` 头写入状态快照——完全复用 `ProviderPoolQuotaRequestSpec`，与 `build_codex_pool_quota_request`（codex.rs:81）同法。围绕 requests/tokens 窗口 + retry_after + subscription_tier + entitlement_status 重定义 `GrokRateLimitSnapshot`。

### 6. 目录、base_url、配置、数据迁移
- **模型目录 + 映射**：`crates/aether-model-fetch/src/logic.rs:386`——保持 grok 模型集为最新（`grok-4.5`、`grok-4.3`、`grok-build-0.1`，imagine-* 留待后续），并新增默认别名表，镜像 `sub2api/.../xai/models.go` 的 `DefaultModelMapping`（`grok→grok-4.5`、`grok-latest→grok-4.3`、`grok-code-fast*→grok-build-0.1` 等）。tier 门控保留在 `handlers/admin/provider/query/models/mod.rs`。
- **端点 / base_url**：将默认 grok 端点 `base_url` 从 `https://grok.com` 改为 `https://api.x.ai/v1`；在 `crates/aether-data/migrations/{postgres,mysql,sqlite}/` 下新增 SQL 迁移：(a) 把既有 grok `provider_endpoints.base_url` 重写为 `api.x.ai/v1`，(b) 把既有**仅 cookie** 的 grok key 置为 inactive 并写入 `oauth_invalid_reason`，提示运维通过 OAuth 重新接入（"完全迁移"接受的代价）。
- **配置/env**：新增 xAI OAuth 覆盖项，镜像 `sub2api/.../xai/oauth.go`（`XAI_OAUTH_CLIENT_ID`、`XAI_OAUTH_SCOPE`、`XAI_OAUTH_REDIRECT_URI`、`XAI_OAUTH_AUTHORIZE_URL`、`XAI_OAUTH_TOKEN_URL`、`XAI_BASE_URL`）到 `.env.example` + 配置管线；内置合理默认值。
- **默认并发**：grok OAuth key 新建或首次接入时默认 `concurrent_limit = 1`；管理员可在 key 配置中手动调整，OAuth 重新授权会保留已设置的值。
- **候选资格**：本轮把 grok oauth 的 format 收窄为 `openai:chat | openai:responses`，位于 `crates/aether-data/src/repository/candidate_selection/{memory,postgres,sqlite,mysql}.rs`（多媒体回归前先去掉 `openai:image`/`claude:messages`）。
- **计费**：`crates/aether-billing/src/pricing.rs:127`——保留/调整 grok 定价；订阅访问可沿用既有官方 grok 定价条目。

### 7. 前端 + 测试
- **前端**：把 grok cookie/sso 导入对话框换成 xAI OAuth 流程（authorize-url → 粘贴 code / redirect），复用 codex OAuth 对话框模式。更新 `frontend/src/features/providers/...` 与 `OAuthAccountDialog.grok-import.spec.ts`；保留 `ProviderType`/`GrokUpstreamMetadata` 结构。
- **测试**：替换 transport 测试（cookie/browser/app-chat → Bearer + api.x.ai URL），删除运行时 app-chat/websocket 测试，重写池测试（魔法总数 → 基于响应头的配额 + 冷却矩阵），新增 OAuth 登录/交换/刷新 + 配额探测测试。更新 `candidate_selection` 与 billing 测试。

## 复用（不要重造）

- **Codex adapter** `crates/aether-provider-pool/src/providers/codex.rs`——池信号、`build_codex_pool_quota_request`（探测 spec）、`default_scheduling_presets`、`codex_quota_soft_threshold_exceeded` 的模板。
- **通用池** `aether-pool-core::{scoring,scheduler}`、**健康/自适应-RPM/熔断** `aether-scheduler-core::health`、**主动探测热池** `apps/aether-gateway/src/maintenance/runtime/pool_quota_probe.rs`、以及 **`pool_member_scores`** 持久化——均与 provider 无关；grok 免费获得。
- **Usage 管线** `aether-usage-runtime`（真实 prompt/completion/cache-write token）——grok 走标准 OpenAI 路径后自动生效。
- **Bearer-OAuth 运行时 + 刷新**（codex/claude_code 已在用）——grok 加入。
- **xAI 参考代码**（移植语义来源）：`sub2api/backend/internal/pkg/xai/{oauth.go,models.go,quota.go}`、`.../service/{grok_oauth_service.go,grok_token_provider.go,openai_gateway_grok.go}`；`cpa/internal/auth/xai/{const.go,xai.go,pkce.go}`。

## 风险 / 落地时需确认

1. **标准路径覆盖度**：确认 aether 的标准 OpenAI **chat** 路径（不只是 codex 的 `responses`）能承载带 per-provider `base_url` + Bearer OAuth + 刷新 + usage 的 provider。Codex 证明了 `responses`；需验证 `chat/completions`。若 xAI 返回任何非 OpenAI 标准字段（如 reasoning），加最小归一化。
2. ~~**Codex OAuth 定位**~~：已完成，见 `crates/aether-oauth/src/provider/providers/generic.rs`。
3. ~~**订阅 base_url**~~：**已由上游回答**——OAuth 默认走 `cli-chat-proxy.grok.com/v1`，见上方 2026-07-15 修订。两个 host 均在 `XaiUrlKind::ApiBase` 白名单内，可用 `XAI_BASE_URL` 覆盖。**仍需真号实测确认。**
4. **既有账号迁移**：仅 cookie 的 grok 行无法自动转换——迁移将其停用并写明原因，供 OAuth 重新接入（已接受）。
5. **device flow 未经真号验证**：discovery 返回的 `device_authorization_endpoint`、xAI 对 `user_code` 的展示形态、以及 chat-proxy 是否真的回 `x-ratelimit-*` 头，都只按上游实现推导，尚未用真实订阅号跑通。

## 验证

- **单元**（`cargo test -p aether-provider-transport -p aether-provider-pool`）：transport 发出 `Authorization: Bearer …` + `https://api.x.ai/v1/...`；`provider_key_auth` 把 grok oauth 判为 Bearer + 可刷新；池把 `x-ratelimit-*` 头解析为耗尽/占比决策；冷却矩阵把 401/403/429/5xx 映射到正确的池状态 + 时长。
- **OAuth 流程**：跑 admin authorize-url → 对 `auth.x.ai` 做 exchange（或粘贴 code），确认 grok key 落库带 `access_token`+`refresh_token`；强制临近过期，确认后台刷新命中 `auth.x.ai/oauth2/token`。
- **端到端**（用 `/run` 或 gateway）：对 `grok-4.5` 发一个 OpenAI `chat/completions` **和** `/responses` 请求；观察上游以 Bearer 调用 `api.x.ai/v1`、SSE 流式、`usage` 表出现**真实** usage 行、`pool_member_scores` 更新。触发 429，确认账号按 `Retry-After` 冷却且调度器 failover。
- **回归**：`cargo test -p aether-gateway` 的 grok + `candidate_selection` + billing 套件；前端 `OAuthAccountDialog` spec。
- **范围检查**：多媒体路由（图像/视频）走干净的「本轮未启用」路径，而非派发到已删除的运行时。
