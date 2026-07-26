# OpenAI Codex `alpha/search` 跨仓执行方案

- **状态：** 原方案已实施并上线；2026-07-26 端点级配置修正已完成本地验证，尚未发布
- **方案日期：** 2026-07-26（基于 2026-07-24 方案补充）
- **Aether 基线：** `/opt/stacks/aether`，分支 `custom`，提交 `c6647fdb7`（工作区有未提交修改）
- **Sub2API 当前检查基线：** `/opt/stacks/sub2api`，分支 `custom-prod`，提交 `d2cb9e445`
- **Sub2API 参考基线：** `/opt/stacks/sub2api`，`origin/main`，提交 `cb24522dd`
- **Codex 协议参考：** `/opt/stacks/openai-codex`，提交 `81da9deb`
- **调查报告：** `docs/architecture/openai-alpha-search-endpoint-investigation-2026-07-24.md`
- **实施报告：** `docs/architecture/openai-alpha-search-implementation-report-2026-07-24.md`
- **生产变更：** 旧版 Search 已上线；本轮修正不包含部署、重启、容器更新、数据库执行或线上开关操作

---

## 1. 执行摘要

> 2026-07-24 执行结果：Aether 实现提交为 `a832d953`，Sub2API
> `custom-prod` 实现提交为 `36efa398b`。两仓定向测试、编译检查和前端类型检查均通过；
> 当轮没有部署、重启、拉取生产镜像或执行迁移。旧版随后已上线；详细时间线、线上诊断
> 与剩余限制见实施报告。

对内只选择 active `openai:search` Codex provider endpoint、模型映射和账号凭据，通过
Search 专用同步透传 planner 调用 `{provider_api_root}/alpha/search`。Aether 不再使用
Responses endpoint 或账号级 `supports_standalone_web_search` 作为 Search 调度门禁。

目标链路为：

```text
Codex 客户端
  -> Sub2API POST /v1/alpha/search
  -> Aether POST /v1/alpha/search
  -> ChatGPT Codex POST /backend-api/codex/alpha/search
```

建议把 Search 实现为：

1. 对外与 `openai:chat`、`openai:responses` 并列的独立 surface：
   `openai:search`；
2. 只支持同步 JSON，不支持 SSE、WebSocket 或 Chat/Responses 格式转换；
3. 对内以独立的 `openai:search` provider endpoint、模型映射和 Codex key pool 为候选；
4. 由 endpoint `is_active` 控制是否参与 Search，不再要求账号级 Search capability；
5. 以请求体顶层 `id` 建立最终 Codex 账号粘性；
6. 只有最终 2xx 计一次 Search，用 surface-scoped 按次价格结算；
7. 对需要旧 `ref_id` 的状态型操作实施 fail-closed，不能在粘性丢失后静默换号；
8. Sub2API 必须保留 `d2b080e88` 的最终行为：API Key 类型 Aether 账号可进入
   `alpha_search` 候选池，API Key 上游的 404/405 做端点级 failover。

不建议直接 cherry-pick `origin/main` 的整组提交。`custom-prod` 与 `origin/main` 从共同
基线 `635ad81c` 以后在路由、调度、账号、计费、Ent schema 和前端上都有双边改动；
应以最终行为和测试为准，逐层移植到当前接口。

---

## 2. 已完成的准备工作

### 2.1 Sub2API 拉取结果

已执行只包含源码分支更新的操作：

```text
origin/custom-prod: 05cb36dd70a92bb9e41feaa06a929a26300bb1de
local custom-prod:  05cb36dd70a92bb9e41feaa06a929a26300bb1de
```

更新方式为 `git fetch origin custom-prod` 后执行
`git merge --ff-only origin/custom-prod`。本地分支从 `f3535e36` 快进一个提交：

```text
05cb36dd7 feat(usage): report first-byte latency and TPS
```

拉取没有启动服务、执行迁移或变更生产环境。Sub2API 工作树中原有的本地文件保持
不变，实施 Search 时不得把它们带入提交：

```text
 M .claude/settings.local.json
?? cafe-release.md
?? logo.png
?? 容器更新历史-已执行迁移SQL-20260704T211647Z.sql
?? 容器更新历史.md
```

### 2.2 新基线带来的约束

`05cb36dd` 增加了：

- `backend/migrations/180_usage_log_first_byte_ms.sql`；
- `OpenAIForwardResult.FirstByteMs`；
- `beginUsageResponseTiming` / `finishOpenAIUsageResponseTiming`；
- HTTP upstream response body 的首字节观测；
- usage log 和前端延迟展示。

因此 Search 的新迁移必须从 `181` 开始，Search handler 也必须进入同一 timing 链路。

---

## 3. 范围和非目标

### 3.1 本期范围

- Sub2API 接收并转发三条兼容路由：
  - `/v1/alpha/search`
  - `/alpha/search`
  - `/backend-api/codex/alpha/search`
- Sub2API 支持 OAuth、PAT 和 API Key 类型 OpenAI 账号的最终上游语义；
- Aether 提供 `/v1/alpha/search`；
- Aether 以自己的 API key 完成本地认证，再使用选中的 Codex OAuth 凭据请求官方
  Search；
- 请求和响应按 opaque JSON 处理；
- body `id`、query、未知 body 字段、未知响应字段端到端保留；
- Search 独立 endpoint、Sub2API capability、usage、计费、审计、指标与错误分类；
- 两仓单元/集成测试和本地 mock 联合测试。

### 3.2 本期非目标

- 不把 Search 加入 Chat、Responses、Claude、Gemini 的 canonical conversion matrix；
- 不把 Search 响应改造成 Responses object 或 SSE；
- 不在没有独立 active Search endpoint 时向任意 OpenAI-compatible provider 发送 Search；
- 不在本任务中探测或调用真实生产 OpenAI/ChatGPT 账号；
- 不实施生产迁移、部署、重启或账号开关；
- 不顺带合并 `origin/main` 的其他大规模功能；
- 不依赖 mutable image tag 或线上“先试再说”来验证协议。

---

## 4. 必须先冻结的跨仓契约

在写业务代码前，先把以下内容固化为 fixture。两仓测试必须引用同一组语义，避免各自
实现“看起来兼容”但链路不一致。

### 4.1 入站请求契约

```http
POST /v1/alpha/search?optional=value
Authorization: Bearer <local-api-key>
Content-Type: application/json
Accept: application/json
```

最小请求：

```json
{
  "id": "search-session-1",
  "model": "gpt-5.6-sol",
  "input": "Find current primary sources",
  "commands": {
    "search_query": [
      { "q": "OpenAI primary source" }
    ]
  },
  "settings": {
    "max_results": 10
  }
}
```

契约要求：

- `model` 必填且必须是非空字符串；
- `id` 原样传递，并作为粘性信号；
- 未知字段默认保留；
- Sub2API 删除顶层 `prompt_cache_key` 和 `prompt_cache_retention`；
- Aether 不执行 Responses 专用 body edits；
- query 参数按安全策略透传；
- Search 强制同步，body 中即使出现 `stream` 也不得进入流式 planner。

### 4.2 成功响应契约

```json
{
  "output": "Search result text",
  "results": [
    {
      "type": "text_result",
      "ref_id": "turn0search0",
      "url": "https://example.com/source",
      "title": "Example"
    }
  ],
  "encrypted_output": "opaque-if-present",
  "future_field": {
    "must_survive": true
  }
}
```

Aether 与 Sub2API 都不得因当前代码不认识 `results` 子结构、`encrypted_output` 或未来
字段而删除它们。

### 4.3 错误契约

| 场景 | Aether 对 Sub2API 的状态 | Sub2API 行为 |
| --- | --- | --- |
| JSON 无效、缺少 model | 400 | 原样返回，不计费 |
| Search 权限未授权 | 403 或启用前预检失败 | 不计费；启用流程应避免运行时发生 |
| 会话绑定已丢失且请求使用非 URL `ref_id` | 409 | 不做外层换号，不计费 |
| Aether 本身未安装端点 | 404/405 | API Key 端点级 failover，不置错账号 |
| 官方限流/暂时故障 | Aether 按自身候选策略处理 | 只有最终对外错误才交给 Sub2API 判断 |
| 最终成功 | 2xx | 两层各记录一次本层 usage |

推荐的粘性丢失响应：

```json
{
  "error": {
    "type": "search_session_affinity_lost",
    "message": "The search session can no longer resolve its prior references"
  }
}
```

### 4.4 请求关联契约

- 保留或生成稳定 `x-request-id`；
- Aether 记录自己的 trace/request ID，并可将安全的上游 request ID 放入内部报告；
- 不把 OAuth token、ChatGPT account ID、Aether key 或 Sub2API client key 写入普通日志；
- request body 日志默认只保存 hash、尺寸、命令类型和字段存在性，不保存查询全文。

---

## 5. 总体架构决策

### 5.1 Surface 与 provider endpoint 分离

使用两个明确概念：

```text
client_api_format = openai:search
provider_endpoint = openai:search
```

`openai:search` 用于：

- 路由识别；
- API key 权限；
- 限流；
- 审计；
- usage 维度；
- Search 按次计价；
- 管理端可见性。

`openai:search` provider endpoint 用于：

- Search 专用端点启用/停用；
- Search 候选查询和 endpoint-scoped 模型映射；
- Codex OAuth key pool；
- 现有 Codex profile、header 和认证物化；
- Search 请求的官方 `/alpha/search` 路径。

不得为了复用候选而把客户端格式改写为 `openai:responses`，否则权限、账单和指标会
混在一起。

### 5.2 同步不透明透传

Search planner 只需要：

1. 验证最小 envelope；
2. 解析模型和粘性信号；
3. 选择候选；
4. 构造官方 URL、认证和身份头；
5. 原样转发 JSON；
6. 原样返回 status/content-type/body 和允许的响应头。

不要建立 Search canonical request/response schema。协议仍为 Alpha，固定 Rust/Go DTO
会扩大未来字段演进的兼容风险。

### 5.3 提供商端点开关

Search 不再新增或依赖账号级 `supports_standalone_web_search`。规则为：

- Codex 固定 provider 模板包含 `openai:search` endpoint；
- endpoint `is_active=true` 时可参与 Search，停用后在候选查询阶段被排除；
- 通用端点管理界面的 Power 按钮就是 Search 开关；
- Codex OAuth key 的历史 `api_formats` 列表不会遮蔽固定 provider endpoint；
- Codex 客户端 TOML 中的 `supports_standalone_web_search` 仍是客户端 provider 声明，
  不属于 Aether 账号配置。

### 5.4 `d2b080e88` 的准确含义

这是 Sub2API 提交，不是 Aether 依赖。它修复的回归是：

1. `776f3f0de` 增加 `alpha_search` capability 时错误地只允许 OAuth；
2. 指向 Aether 的账号在 Sub2API 中属于 `AccountTypeAPIKey`；
3. 结果是请求尚未发到 Aether，就在 Sub2API 选号阶段被剔除；
4. `d2b080e88` 恢复 OAuth 和 API Key 都可调度；
5. 同时把 API Key Search 上游的 404/405 解释为“该上游未实现端点”，允许换号且不把
   整个账号标记为错误。

因此实施时不要求 Aether cherry-pick 该提交；要求 Sub2API 的最终代码和回归测试保留
这些行为。

---

## 6. Sub2API 实施方案

### 6.1 移植策略

不要直接合并整个 `origin/main`，也不要机械 cherry-pick 全部 Search 提交。推荐方式：

1. 用 `origin/main` 文件和测试作为行为参考；
2. 在 `custom-prod@05cb36dd` 上新建短生命周期开发分支；
3. 按下述 S1-S6 拆分小提交；
4. 每个提交只解决一个层次并通过对应测试；
5. Ent 代码只从最终 schema 重新生成；
6. 不修改工作树原有本地文件。

建议先保存只读参考补丁，便于审查而不是直接套用：

```bash
git diff custom-prod..origin/main -- \
  backend/internal/handler/openai_alpha_search.go \
  backend/internal/service/openai_alpha_search.go \
  backend/internal/service/openai_alpha_search_test.go \
  backend/internal/service/openai_alpha_search_billing_test.go \
  backend/internal/service/account.go \
  backend/internal/server/routes/gateway.go
```

### 6.2 S1：路由、handler 骨架与前端 bypass

主要文件：

- `backend/internal/server/routes/gateway.go`
- `backend/internal/handler/openai_alpha_search.go`
- `backend/internal/handler/endpoint.go`
- `backend/internal/web/embed_on.go`
- `backend/internal/server/routes/gateway_test.go`

路由要求：

```text
POST /v1/alpha/search
POST /alpha/search
POST /backend-api/codex/alpha/search
```

三条路由必须进入与现有 OpenAI gateway 相同的：

- body limit；
- client request ID；
- `UsageResponseTiming`；
- ops error logger；
- endpoint normalization；
- API key auth；
- billing/subscription context。

特别注意：`/v1` group 已统一挂 timing middleware，但根路径 alias 通常需要显式挂载；
不能只给 `/v1/alpha/search` 计时。

Handler 最小职责：

1. 验证 API key、OpenAI group、用户 context；
2. 限制 body 大小并验证 JSON；
3. 验证 `model`；
4. 执行 channel/model mapping；
5. 获取用户并发槽位；
6. 检查 billing eligibility；
7. 从 body `id` 生成 session hash；
8. 用 `OpenAIEndpointCapabilityAlphaSearch` 选号；
9. 获取账号并发槽位；
10. 调用 `ForwardAlphaSearch`；
11. 按 custom-prod 当前接口处理调度结果和 failover；
12. 仅对非 nil 成功结果提交 usage。

`origin/main` 最新 handler 不能原样复制，至少有以下适配：

- custom-prod 的 `SelectAccountWithSchedulerForCapability` 参数集不同；
- custom-prod 的 `ReportOpenAIAccountScheduleResult` 不带 model 参数；
- custom-prod 的 `ShouldStopOpenAIOAuth429Failover` 不使用最新状态对象签名；
- custom-prod 没有最新 handler 同名的 `checkSecurityAudit` 调用点；应接入当前分支已有的
  等价审计链路，而不是为编译临时跳过安全策略；
- 使用 custom-prod 当前 `setOps*`、并发和错误 helper，不反向移植整个上游框架。

嵌入前端 bypass 必须覆盖所有 Search 路径，确保缺少后端路由时不会错误返回 SPA HTML。

### 6.3 S2：Search wire service

主要文件：

- 新增 `backend/internal/service/openai_alpha_search.go`
- `backend/internal/service/openai_endpoint_url.go`
- `backend/internal/repository/http_upstream.go`（通常只复用，不应为 Search 单独绕过）
- 新增 `backend/internal/service/openai_alpha_search_test.go`

#### 6.3.1 URL

| Sub2API account 类型 | 目标 URL |
| --- | --- |
| OAuth | `https://chatgpt.com/backend-api/codex/alpha/search` |
| API Key，无 base URL | `https://api.openai.com/v1/alpha/search` |
| API Key，有 base URL | `buildOpenAIEndpointURL(base, "/v1/alpha/search")` |

Aether 账号建议保留 base URL：

```text
http://aether:8080/v1
```

最终必须拼成：

```text
http://aether:8080/v1/alpha/search
```

#### 6.3.2 Body

- 先应用 Sub2API model mapping；
- 删除 `prompt_cache_key`、`prompt_cache_retention`；
- 保留 `id`、commands、settings、input、reasoning 和未知字段；
- 不注入 Responses 的 `stream`、`store`、`include`、instructions 或状态头；
- 不解析、重建整个 Search schema。

#### 6.3.3 Header

到 Aether 的 API Key 分支：

- `Authorization: Bearer <Aether key>`；
- `Content-Type: application/json`；
- `Accept: application/json`；
- 不转发客户端 Authorization；
- 不假设入站一定含 Codex Originator、Version、UA 或 turn metadata；
- 应用账号显式 header overrides 后，再执行敏感头清理。

OAuth 官方分支需要按现有 Codex profile 生成 ChatGPT account ID、Codex identity 和
允许的 metadata。Search 头集合与 Responses 不同，必须移除：

```text
OpenAI-Beta
Session_ID
Conversation_ID
X-Codex-Beta-Features
X-Codex-Turn-State
Responses-Lite 状态头
```

#### 6.3.4 Response

- 完整读取受大小限制的 body；
- 使用安全响应头 allowlist；
- 原样返回 status、content type 和 JSON bytes；
- 只有 2xx 返回 `OpenAIForwardResult{WebSearchCalls: 1}`；
- 非 2xx 已原样写回客户端时返回 `(nil, nil)`，避免计费；
- 可 failover 的错误必须在写 response 前返回 `UpstreamFailoverError`。

#### 6.3.5 PAT fallback

`origin/main` 对 Codex Personal Access Token 使用 `/responses` hosted `web_search` fallback，
并把 SSE 聚合回 Search JSON。这个能力和 Aether API Key 链路无直接依赖，但如果目标是
与 `origin/main` 最终行为一致，应单独移植，避免将复杂 fallback 与基础 API Key 转发
放在同一提交中。

建议顺序：

1. 先完成 OAuth/API Key direct Search；
2. 再移植 PAT metadata 补全和 Responses fallback；
3. 单独测试 PAT 401 不永久置错账号；
4. API Key 指向 Aether 的分支永远不进入 PAT fallback。

### 6.4 S3：账号 capability 和 `d2b080e88` 行为

主要文件：

- `backend/internal/service/account.go`
- `backend/internal/service/openai_account_scheduler_test.go`
- `backend/internal/service/openai_alpha_search.go`

增加：

```go
OpenAIEndpointCapabilityAlphaSearch
```

最终规则：

- platform 必须是 OpenAI；
- `AccountTypeOAuth` 允许；
- `AccountTypeAPIKey` 允许；
- 其他类型默认拒绝；
- 如果 capability 集显式包含 `alpha_search`，允许；
- 为兼容上游现状，可保留 `chat_completions` 隐含允许 Search 的行为，但新增 Aether
  账号应显式配置 `alpha_search`，便于后续单独关闭；
- 不能恢复 OAuth-only 门控。

API Key Search 404/405：

- 触发本次请求换号；
- 不把账号永久写为 error；
- OAuth 官方 Search 的 404 仍按原语义处理；
- 401 只代表工具端点本次失败，不能据此永久置错没有 refresh token 的 PAT/APIKey
  账号。

必需回归测试：

- 纯 API Key group 能选中 Aether 账号；
- OAuth 与 API Key 混合 group 均可选；
- 不支持 capability 的账号被拒绝；
- API Key 404/405 failover；
- API Key 404/405 不产生账号错误副作用；
- OAuth 404 行为不被误改。

### 6.5 S4：按次计费、schema 与管理端

主要文件可能包括：

- `backend/migrations/181_group_web_search_price_per_call.sql`
- `backend/ent/schema/group.go`
- Ent 生成文件
- `backend/internal/service/group.go`
- `backend/internal/repository/api_key_repo.go`
- `backend/internal/service/api_key.go`
- `backend/internal/service/billing.go`
- `backend/internal/service/openai_gateway_service.go`
- `backend/internal/service/openai_alpha_search_billing_test.go`
- `frontend/src/types/index.ts`
- `frontend/src/api/admin/groups.ts`
- `frontend/src/views/admin/GroupsView.vue`
- 对应 i18n 和前端测试

#### 6.5.1 数据字段

在 group 增加 nullable：

```text
web_search_price_per_call
```

语义：

- `NULL`：使用代码默认价 `0.01 USD/次`；
- `0`：免费；
- 正数：覆盖默认价；
- 负数：API/DB 校验拒绝。

迁移编号必须是 `181`，因为 custom-prod 已使用 `180_usage_log_first_byte_ms.sql`。迁移需
兼容仓库支持的数据库，并遵循现有幂等模式。

#### 6.5.2 结果和费用计算

在 `OpenAIForwardResult` 增加：

```go
WebSearchCalls int
```

费用分支必须早于 token pricing：

```text
if WebSearchCalls > 0:
    total  = calls * configured_or_default_price
    actual = total * base_group_multiplier
    mode   = per_request
```

约束：

- 使用基础 group multiplier，不使用只面向 token 的高峰倍率；
- 负倍率按 0 处理，不回退为 1；
- `WebSearchCalls <= 0` 必须继续走既有 token/image/video 逻辑；
- 2xx 以外没有 `WebSearchCalls`；
- failover 的中间尝试不生成 usage；
- mandatory usage pool 满时同步兜底，不能丢扣费记录。

#### 6.5.3 Snapshot 和 DTO

必须验证该字段经过以下链路不丢失：

```text
DB/Ent Group
 -> repository
 -> service Group
 -> APIKey cached snapshot
 -> snapshot round trip
 -> admin API DTO
 -> frontend edit/readback
```

Ent 文件必须在 schema 合并完成后统一生成，禁止从 `origin/main` 手工复制部分生成文件。

### 6.6 S5：first-byte timing 与 usage 完整性

Handler 调用 upstream 前：

```go
forwardStart := time.Now()
forwardCtx := beginUsageResponseTiming(c, forwardStart)
result, err := h.gatewayService.ForwardAlphaSearch(forwardCtx, c, account, forwardBody)
finishOpenAIUsageResponseTiming(c, forwardStart, result)
```

如果 handler 已有派生 context，应作为第三参数传给 `beginUsageResponseTiming`，避免丢失
取消、trace 或 request-scoped value。

验证点：

- `httpUpstream.Do` 收到的 request context 包含 timing observer；
- 首次读取 upstream body 时记录 `FirstByteMs`；
- failover 新尝试重置内部 upstream boundary；
- 最终成功 usage log 保存 `first_byte_ms`；
- `Duration` 表示最终有效 upstream attempt，而不是整个多账号循环的墙钟时间；
- 不把“完整读取小 JSON body 的结束时间”错误命名为首 token。

### 6.7 S6：Sub2API 测试门

至少运行：

```bash
cd /opt/stacks/sub2api/backend
GOCACHE=/tmp/sub2api-gocache go test ./internal/service -run 'AlphaSearch|WebSearch|FirstByte' -count=1
GOCACHE=/tmp/sub2api-gocache go test ./internal/handler -run 'AlphaSearch|UsageResponseTiming' -count=1
GOCACHE=/tmp/sub2api-gocache go test ./internal/server/routes -run 'AlphaSearch|Gateway' -count=1
GOCACHE=/tmp/sub2api-gocache go test -tags=unit ./internal/service -run 'WebSearch|AlphaSearch' -count=1
```

然后运行受变更影响的完整包测试：

```bash
cd /opt/stacks/sub2api/backend
GOCACHE=/tmp/sub2api-gocache go test ./internal/service ./internal/handler ./internal/server/routes -count=1
```

前端若修改 group 配置：

```bash
cd /opt/stacks/sub2api/frontend
pnpm run typecheck
pnpm run test:run -- GroupsView
pnpm run build
```

命令中的测试过滤词可按最终测试名调整，但不得只以编译成功作为验收。

---

## 7. Aether 实施方案

### 7.1 A1：新增格式身份与公共路由

主要文件：

- `crates/aether-ai-formats/src/formats/id.rs`
- `crates/aether-ai-formats/src/formats/mod.rs`
- `apps/aether-gateway/src/api/ai/registry.rs`
- `apps/aether-gateway/src/api/ai/openai.rs`
- `apps/aether-gateway/src/control/route/ai.rs`
- `apps/aether-gateway/src/control/tests/ai.rs`

增加格式：

```rust
FormatId::OpenAiSearch <-> "openai:search" <-> "/v1/alpha/search"
```

属性：

- family：OpenAI；
- profile：Default；
- 不属于 Responses family helper；
- 不使用 body `stream` 决定执行模式；
- 不加入 Chat/Responses conversion registry。

公共路由：

```rust
POST /v1/alpha/search -> proxy_request
```

控制分类：

```text
route_class = ai_public
route_family = openai
route_kind = search
api_format = openai:search
execution_runtime_candidate = true
request_auth_channel = none（沿用现有 OpenAI public route 的 `classified` 方式）
```

测试必须证明 GET/PUT 不会错误匹配，且 Search 不会分类为 Responses。

是否同时在 Aether 暴露 `/alpha/search` 和 `/backend-api/codex/alpha/search`：本期不需要。
Sub2API API Key builder 实际调用 `/v1/alpha/search`。若以后为 Codex 客户端直连提供
alias，应作为独立兼容提交，避免无意扩大公共面。

### 7.2 A2：plan kind 与 opaque sync planner

主要文件：

- `crates/aether-ai-formats/src/contracts/plan_kinds.rs`
- `crates/aether-ai-formats/src/contracts/report_kinds.rs`
- `crates/aether-ai-formats/src/api.rs`
- `apps/aether-gateway/src/ai_serving/api.rs`
- `apps/aether-gateway/src/ai_serving/planner/decision/control_plan.rs`
- `apps/aether-gateway/src/ai_serving/planner/standard/openai/`
- `apps/aether-gateway/src/ai_serving/finalize/`

建议增加：

```text
OPENAI_SEARCH_SYNC_PLAN_KIND = "openai_search_sync"
OPENAI_SEARCH_SYNC_SUCCESS_REPORT_KIND
OPENAI_SEARCH_SYNC_FINALIZE_REPORT_KIND
```

推荐新建独立模块：

```text
apps/aether-gateway/src/ai_serving/planner/standard/openai/search/
```

不要把它塞进 Responses body normalization。该 planner 可复用 same-format sync 的传输和
finalize 原语，但必须有 Search 专用的：

- `openai:search` endpoint candidate；
- URL builder；
- endpoint active filter；
- body validator；
- header policy；
- affinity policy；
- usage/report context。

Plan 决策要求：

- 只产生 sync plan；
- client format 始终为 `openai:search`；
- provider candidate 查询只使用 `openai:search` endpoint；
- provider actual path 为 Search；
- 不创建 stream fallback；
- 不做 sync-to-stream 或 stream-to-sync 聚合；
- 不调用 OpenAI Chat/Responses response converter。

### 7.3 A3：候选解析和 endpoint 门控

候选来源使用 Codex 固定 provider 的 `openai:search` endpoint，并在 materialization 前后
验证 endpoint/provider/key 状态，防止缓存或旧数据绕过。

候选必须满足：

```text
endpoint api_format == openai:search
AND provider/endpoint/key active and schedulable
AND requested model resolves on that endpoint
AND auth/profile materialization succeeds
```

可能涉及：

- `apps/aether-gateway/src/ai_serving/planner/candidate_resolution.rs`
- `apps/aether-gateway/src/ai_serving/planner/candidate_materialization.rs`
- `apps/aether-gateway/src/ai_serving/planner/standard/openai/search/`
- `crates/aether-provider-pool/src/providers/codex.rs`
- provider endpoint admin DTO 与固定模板 metadata

### 7.4 A4：官方 URL、认证和身份头

目标官方 URL：

```text
https://chatgpt.com/backend-api/codex/alpha/search
```

Search URL builder 必须基于最终候选的 Codex API root 生成，而不是简单在 Aether 入站
URL 或普通 OpenAI base URL 后重复追加 `/v1`。

需要复用最终 Codex key 的：

- OAuth access token；
- ChatGPT account ID；
- Codex user-agent/profile；
- originator；
- proxy/TLS/HTTP upstream profile；
- 现有 header override 和安全清理机制。

边界：

- Aether 入站 bearer key 只用于本地认证；
- 不把 Sub2API 的 bearer key 发给 ChatGPT；
- 不信任客户端提供的 ChatGPT account ID；
- 不允许客户端覆盖最终 OAuth Authorization；
- 不转发内部 control、trace、billing 或 scheduler headers；
- 可以保留经过验证的 `X-Codex-Turn-Metadata`，但链路不能依赖它存在；
- 没有入站 Originator/Version/UA 时，使用最终 key 的稳定 profile 构造。

Body 必须保持 opaque，只进行：

- JSON 有效性和大小检查；
- `model` 非空验证；
- 模型 alias 替换；
- 必需的安全字段清理。

禁止套用 Responses 的 prompt cache、store、include、instructions、stream 等编辑规则。

### 7.5 A5：粘性和 `ref_id` 连续性

主要文件：

- `apps/aether-gateway/src/client_session_affinity.rs`
- `apps/aether-gateway/src/ai_serving/planner/candidate_affinity_cache.rs`
- `apps/aether-gateway/src/scheduler/affinity.rs`
- `apps/aether-gateway/src/cache/scheduler_affinity.rs`
- Search planner/support 模块

当前通用 body affinity 列表不读取顶层 `id`，而 cache key 又包含 API format。若直接使用
现状，Search 请求可能不粘，或与伴生 Responses 使用不同最终 key。

推荐做法：

1. 只在 Search route 上把顶层 `id` 解释为 Codex session；
2. 不把全局 Generic adapter 的任意顶层 `id` 都当 session，避免影响其他 API；
3. 对外 report 仍记录 `client_api_format=openai:search`；
4. affinity cache 使用独立的 `openai:search` namespace，不把 Responses 绑定直接当作
   Search 的状态型候选绑定；
5. Search 尚无精确绑定时，可把同一会话的 `openai:responses` 绑定作为首次请求的账号提示，
   跨 endpoint ID 匹配相同 provider/key；Search 一旦建立精确绑定，必须优先使用自身绑定；
6. Responses 账号提示不能作为非 URL `ref_id` 已建立 Search 状态的证明，状态型操作仍要求
   已存在精确的 Search 绑定；
7. 保证 Search 自身连续请求稳定命中同一 key，并在 endpoint 或 key 状态变化时使绑定失效
   或重新验证。

#### 7.5.1 Stateful ref 识别

请求 commands 中若使用非 URL `ref_id`，例如 open/click/find/screenshot 指向先前搜索
结果，则视为强状态请求。实现时需根据实际 Codex schema 做递归、受限深度的检查，不能
只查一个固定 JSON path。

规则：

- 有可用绑定：只尝试已绑定 candidate；
- 已绑定 key 暂时不可用：返回 409，不跨 key failover；
- 没有绑定但命令只含新 `search_query`/`image_query`：可正常选新 key 并建立绑定；
- 只有可验证为完整 URL 的 ref 才可视为无状态参数；
- 不把 ref 内容写入普通日志。

### 7.6 A6：响应 finalizer

Search finalizer 应：

- 保留上游 status；
- 保留 `application/json` 或上游安全 content type；
- 原样传递 body bytes；
- 使用现有响应头安全 allowlist；
- 保留 request ID 和速率限制类安全头；
- 不把结果规范化为 Responses；
- 不删除未知 JSON 字段；
- 不生成伪 `usage`；
- 只有 2xx 标记 Search success 和 `request_count=1`。

如果上游返回非 JSON 错误，Aether 可按现有核心错误 envelope 规范化给客户端，但内部
报告应保存 sanitized upstream status/category，不能把错误响应计为成功 Search。

### 7.7 A7：内部 failover 与两层错误语义

Aether 内层 failover 应区分：

1. 无状态首次搜索；
2. 已有普通 session 绑定但没有 ref；
3. 使用非 URL ref 的强状态请求。

建议矩阵：

| 场景 | Aether 内层行为 |
| --- | --- |
| 首次 search_query，候选 transport error/429/5xx | 可按普通候选策略换 key |
| 已绑定但无 ref，key 不可用 | 可按策略重建绑定，但记录 affinity break |
| 有非 URL ref，绑定 key 不可用 | 不换 key，返回 409 |
| 官方返回参数 400/422 | 原样返回，不换 key |
| 官方认证 401/403 | 按现有 key health 策略处理；最终不得伪装 2xx |
| 官方 404/405 | 保留“已 dispatch 到官方”的内部 disposition，供联合语义扩展 |

当前 Sub2API 对 API Key Search 的 404/405 一律按 endpoint unsupported failover。为避免
未来把官方返回的 ref 404 误判成“Aether 没路由”，建议预留一个只由 Aether 设置且不会
由客户端伪造的响应 disposition，例如：

```text
X-Aether-Upstream-Disposition: dispatched
```

第一阶段可以只记录内部报告，不立即修改 Sub2API 协议；第二阶段若真实 fixture 证明
官方会为 ref 返回 404，再让 Sub2API 仅在缺少可信 disposition 时执行端点级 404/405
failover。

### 7.8 A8：Search 专用 usage 和计费

主要文件：

- `crates/aether-contracts/src/usage.rs`
- `crates/aether-usage-runtime/src/usage_mapper.rs`
- `crates/aether-billing/src/event_enrichment.rs`
- `crates/aether-billing/src/pricing.rs`
- `crates/aether-billing/src/default_rule.rs`
- `crates/aether-billing/src/service.rs`
- `apps/aether-gateway/src/control/auth/gate.rs`
- `crates/aether-admin/src/system.rs`
- 管理端/前端 pricing DTO 和表单

成功 Search usage：

```text
api_format       = openai:search
request_count    = 1
input_tokens     = 0/unknown
output_tokens    = 0/unknown
provider endpoint = openai:search
status           = success
```

失败和中间 failover attempt 的 `request_count` 必须为 0 或不产生结算事件。

#### 7.8.1 不可直接复用普通 `price_per_request`

Aether 当前 `price_per_request` 主要挂在共享模型/默认 pricing 上。Search 与 Responses
复用模型，如果给模型设置普通按次价格，普通 `/v1/responses` 也可能被加收。

推荐方案按优先级：

1. **surface-scoped price**：pricing key 包含 `api_format=openai:search`；
2. Search 专用 billing rule，条件明确匹配 `api_format`；
3. provider endpoint 的 Search 专用价格 snapshot。

不接受：仅给共享 Responses model 设置 `price_per_request`。

认证前余额估算也必须读取 Search surface 的价格，否则会出现：

- 请求前估算为 0、请求后才扣费；或
- 普通 Responses 被 Search 价格错误阻断。

测试需覆盖：

- Search 2xx 计一次；
- Search 4xx/5xx 不计；
- 内层第一次失败、第二次成功只计一次；
- 同一模型普通 Responses 不增加 Search 按次费；
- API-format scoped override、生效优先级和 0 免费语义；
- 管理端保存、读取和预览一致。

### 7.9 A9：权限、审计和可观测性

新增独立维度：

```text
api_format = openai:search
route_kind = search
plan_kind = openai_search_sync
```

建议指标：

- request count / success / error；
- first-byte latency、total upstream latency；
- candidate switch count；
- affinity hit/miss/break；
- stateful-ref 409 count；
- upstream status category；
- provider/endpoint/key ID（仅内部 ID）；
- billing success/failure；
- capability rejection。

隐私要求：

- 不记录搜索 query 全文；
- 不记录 input 全文；
- 不记录结果正文、URL query string 或 encrypted output；
- 可记录 body hash、字节数、command type 列表、结果条数；
- 日志中的 URL 仅保留 origin/host 或 sanitized path；
- 管理端 trace 详情如需展示 body，必须沿用现有敏感字段脱敏和权限控制。

### 7.10 A10：管理端和前端格式目录

可能涉及：

- `crates/aether-admin/src/system.rs`
- provider endpoint/model/key API DTO
- `frontend/src/api/endpoints/types/api-format.ts`
- provider endpoint 表单（使用通用启用/停用按钮）
- pricing/billing rule 表单
- 对应 i18n 与前端测试

需要展示：

- `openai:search` 格式名称和 `/v1/alpha/search`；
- 它是 Codex provider 的独立 endpoint，不是转换格式；
- endpoint `is_active` 开关；
- Search surface 按次价格；
- 不应把 Search 自动加到 custom/OpenAI endpoint。

---

## 8. 推荐提交拆分

### 8.1 Aether 提交序列

| 提交 | 内容 | 独立验收 |
| --- | --- | --- |
| A1 | `FormatId::OpenAiSearch`、公共 route、route classification | 路由/格式测试 |
| A2 | Search sync plan kind、opaque planner/finalizer | planner 与原样响应测试 |
| A3 | Codex `openai:search` endpoint candidate、endpoint gate | 候选/停用端点测试 |
| A4 | Codex URL/auth/header/body policy | mock upstream wire 测试 |
| A5 | body `id` affinity、stateful ref 409 | affinity/failover 测试 |
| A6 | Search usage、surface-scoped billing、auth estimate | billing 测试 |
| A7 | 管理端/前端 endpoint 开关与 pricing | typecheck/UI 测试 |
| A8 | 跨层 fixture、文档和运行手册 | 本地 E2E |

### 8.2 Sub2API 提交序列

| 提交 | 内容 | 独立验收 |
| --- | --- | --- |
| S1 | routes、handler 骨架、endpoint normalization、SPA bypass | route 测试 |
| S2 | direct OAuth/APIKey Search wire | service wire 测试 |
| S3 | capability、API Key 调度和 d2 404/405 行为 | scheduler/failover 测试 |
| S4 | PAT Responses fallback | PAT 专项测试 |
| S5 | migration 181、group price、WebSearchCalls、usage billing | schema/billing 测试 |
| S6 | first-byte timing、联合 fixture、前端配置 | timing/E2E/UI 测试 |

提交之间不要混入格式化整个仓库、无关重构或已有本地文件。

---

## 9. 开发与联调顺序

虽然 Sub2API 是终端入口，但 Aether 是其 Search 上游。推荐顺序：

1. 冻结 fixture 和错误矩阵；
2. Aether 完成 A1-A5，使本地 mock 可完整跑通 `/v1/alpha/search`；
3. Aether 完成 usage/billing，并完成 fixed provider endpoint reconcile；
4. Sub2API 完成 S1-S3，指向本地 Aether fixture；
5. 如需与 `origin/main` 完全对齐，再完成 PAT fallback；
6. 两仓完成计费和 first-byte timing；
7. 运行本地联合 E2E；
8. 各仓分别提交、推送，让远程 CI 构建；
9. 由用户决定后续非生产/生产发布和开关时间。

该顺序避免先让 Sub2API 将真实请求转向一个尚未支持 Search 的 Aether。

---

## 10. 联合本地测试设计

### 10.1 Mock 拓扑

```text
test client
 -> Sub2API test server
 -> Aether test server
 -> mock ChatGPT Search server
```

Mock upstream 必须捕获：

- method、path、query；
- Authorization 是否为最终 Codex token；
- ChatGPT account ID；
- Originator、Version、User-Agent、turn metadata；
- content type / accept；
- raw body bytes；
- candidate/key identity；
- 调用次数。

### 10.2 必测用例

1. `/v1/alpha/search` 正常成功；
2. Sub2API base URL `/v1` 不产生 `/v1/v1/alpha/search`；
3. 根路径和 Codex direct alias 进入同一 Sub2API handler；
4. Aether 只需收到 Bearer Aether key，也能重建官方 Codex 身份；
5. 客户端 Authorization 不到达官方 upstream；
6. `prompt_cache_key` 和 `prompt_cache_retention` 在 Sub2API 被删除；
7. `id` 和未知 body 字段保留；
8. query 参数端到端保留并正确编码；
9. 未知响应字段、`results`、`encrypted_output` 保留；
10. 同 `id` 的 search -> open/click/find/screenshot 命中同一最终 key；
11. stateful ref 的绑定 key 不可用时返回 409，Aether 与 Sub2API 都不换到破坏状态的 key；
12. 无状态首次搜索在一个 key 429、另一个 key 2xx 时只计一次；
13. Aether route 未安装的 404 触发 Sub2API API Key failover；
14. API Key 404/405 不把 Aether 账号永久置错；
15. OAuth 404 行为保持原有语义；
16. Aether `openai:search` endpoint 停用时不选择候选；
17. Search 计费不影响相同模型的 Responses；
18. first-byte latency 在两层各自正确记录；
19. body 超限、无效 JSON、缺 model 均在发 upstream 前失败；
20. 取消请求会中止 upstream，不产生成功 usage。

### 10.3 对账断言

一次最终成功允许出现两条不同经济含义的 usage：

```text
Aether usage:  Sub2API 消耗官方 Codex pool 的上游成本
Sub2API usage: 终端用户购买 Search 产品的费用
```

两条记录应可通过时间、模型、请求 ID/trace 关联，但：

- 金额可以不同；
- API key ID 属于各自系统；
- 两层都只能有一条成功计费；
- 中间 attempt 只进入运行指标，不进入最终扣费。

---

## 11. Aether 测试门

格式、计划和 transport：

```bash
cd /opt/stacks/aether
cargo fmt --all -- --check
cargo test -p aether-ai-formats search --locked
cargo test -p aether-provider-transport search --locked
cargo test -p aether-scheduler-core affinity --locked
```

Billing 和管理端：

```bash
cd /opt/stacks/aether
cargo test -p aether-billing search --locked
cargo test -p aether-admin search --locked
```

Gateway：

```bash
cd /opt/stacks/aether
cargo test -p aether-gateway search --locked
cargo check -p aether-gateway --tests --locked
```

前端如有改动：

```bash
cd /opt/stacks/aether/frontend
npm run type-check
npm run test:run -- api-format
npm run build
```

最后再运行受影响 workspace 包的非过滤测试。过滤测试用于快速反馈，不能替代完整包
测试和远程 CI。

---

## 12. 发布前检查表

以下只是发布准备条件，不授权执行生产操作。

### 12.1 代码和数据

- [ ] Aether Codex provider 已有 `openai:search` endpoint，且可通过 `is_active` 控制；
- [ ] Aether surface-scoped pricing 有明确默认/NULL/0 语义；
- [ ] Sub2API 使用 migration `181`，不存在编号冲突；
- [ ] 两仓 schema/生成代码一致；
- [ ] 两仓工作树无误带本地文件；
- [ ] CI 产物使用明确 commit/tag/digest；
- [ ] migration 前向兼容，旧应用读取新 nullable 字段不失败；
- [ ] 不需要数据库 downgrade 或破坏性反向迁移。

### 12.2 协议和安全

- [ ] Aether key 不会转发到 ChatGPT；
- [ ] ChatGPT token/account ID 不会返回客户端或进入普通日志；
- [ ] Search body 未进入 Responses converter；
- [ ] stateful ref 失败为 409；
- [ ] Search endpoint 停用时无候选且不回退到 Responses；
- [ ] 未知字段端到端保留；
- [ ] 404/405 两层语义有测试。

### 12.3 计费和观测

- [ ] 只有最终 2xx 计费；
- [ ] Search 与 Responses 定价隔离；
- [ ] pre-auth 余额估算与 post-request 结算使用同一 Search price snapshot；
- [ ] first-byte、总延迟、switch、affinity 指标可见；
- [ ] 两层 usage 可对账；
- [ ] 日志不包含 query/result 正文。

---

## 13. 建议发布顺序与回退边界

生产发布必须由用户另行明确授权。届时建议最小顺序是：

1. 记录 Aether 当前版本/digest、健康状态和迁移状态；
2. 发布包含 `openai:search` fixed endpoint 的 Aether；
3. 在非生产或隔离 provider 上启用 endpoint，跑联合 smoke test；
4. 记录 Sub2API 当前版本/digest、健康状态和 migration 状态；
5. 发布含 Search 路由和 migration 181 的 Sub2API，但先不向用户开放；
6. 验证普通 Chat/Responses 无回归；
7. 小范围启用 Search group/capability；
8. 观察 2xx、409、404/405、429/5xx、affinity 和 billing；
9. 再决定扩大范围。

安全回退优先使用开关：

1. 先在 Sub2API group 关闭 Search 入口；
2. 再在 Aether 停用 Codex provider 的 `openai:search` endpoint；
3. 保留新增 nullable schema，不做数据库 downgrade；
4. 如需代码回退，回到明确的上一版本/digest；
5. 不删除 usage 或 group price 字段，不执行破坏性反向迁移。

---

## 14. 风险清单

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 机械 cherry-pick `origin/main` | 大量冲突或覆盖 custom 逻辑 | 按行为逐层移植、小提交、测试锁定 |
| 丢失 `d2b080e88` 行为 | Aether API Key 账号在 Sub2API 选号前被排除 | scheduler 回归测试 |
| 迁移继续使用 174 | 与 custom 迁移冲突 | 固定使用 181 |
| Search 未进入 timing context | first-byte 指标为空或错误 | handler + httpUpstream 联合测试 |
| 把 Search 当 Responses 转换 | 未知字段丢失、协议破坏 | opaque planner/finalizer |
| Aether 重用入站 bearer | 凭据泄漏/官方 401 | 身份终止和 header 测试 |
| 未从 body `id` 取 affinity | ref 后续操作失败 | Search 专用 session extractor |
| stateful ref 跨 key failover | 引用不可解析、结果错误 | 强绑定 + 409 fail-closed |
| 404 被外层误判 | 不必要换 Aether 账号 | fixture；必要时 disposition 协议 |
| 共用模型普通 `price_per_request` | Responses 被错误加价 | surface-scoped pricing |
| PAT fallback 与 direct path 混合 | 审查和故障定位困难 | 独立提交、独立测试 |
| 日志记录查询或结果 | 隐私泄露 | hash/metadata-only 日志策略 |

---

## 15. 尚需通过实现测试确认的问题

以下内容不能在没有 fixture/受控测试时写成已确认事实：

1. 官方 Search 对过期或跨账号 `ref_id` 的具体状态码；
2. `encrypted_output` 是否参与跨请求状态恢复；
3. 官方是否要求某些 Codex metadata header 才允许特定 command；
4. Aether Responses 与 Search 是否必须共享完全相同的 affinity namespace，还是 Search
   自身稳定粘性已足够；
5. PAT fallback 是否是 custom-prod 的产品必要范围，还是仅跟随 origin/main；
6. Search 定价应挂在全局 surface、provider endpoint 还是 billing rule 的最终产品选择；
7. 是否需要第二阶段让 Sub2API 识别 Aether 的 upstream-dispatched disposition。

这些问题不阻断基础 route、opaque wire、capability、API Key 调度修复和 Search 专用
pricing 结构的实现。

---

## 16. Definition of Done

只有同时满足以下条件，Search 端点才算完成：

- [ ] Sub2API 三条路由均正确；
- [ ] Aether `/v1/alpha/search` 是独立 `openai:search` surface；
- [ ] Aether 候选只查询 active `openai:search` Codex endpoint；
- [ ] Sub2API API Key 类型 Aether 账号可被选中；
- [ ] body/query/未知响应字段端到端保留；
- [ ] OAuth/APIKey/PAT 各自 wire 行为有测试；
- [ ] 客户端和两层上游凭据严格隔离；
- [ ] 同 `id` 搜索链粘到同一最终 key；
- [ ] stateful ref 粘性丢失返回 409；
- [ ] 只有最终 2xx 在每一层各计一次；
- [ ] Search 价格不影响普通 Responses；
- [ ] first-byte timing 在 Sub2API 新基线上有效；
- [ ] migration 使用 181 且 Ent 重新生成；
- [ ] 两仓重点包测试、前端检查和本地联合 E2E 全部通过；
- [ ] 代码已提交并推送，生产仍保持未变更，等待用户明确发布授权。

### 16.1 2026-07-26 设计修正

原方案中的“Responses provider anchor + `supports_standalone_web_search` 账号能力”已
废弃。实施时应以以下最终规则为准：

1. Codex fixed provider template version 2 增加 `openai:search` endpoint；
2. Search planner、candidate selection、model-resolution 和 pre-auth estimate 均使用
   `openai:search`，不再隐式查询 `openai:responses`；
3. Codex OAuth key 的旧 `api_formats` 限制不会遮蔽 fixed provider endpoint；
4. 后台节点启动 reconcile 既有 fixed providers，通用 endpoint UI 的 `is_active` 是唯一
   Aether provider-level Search 开关；
5. Key/OAuth 编辑 UI 不再提供 Search capability 字段；Codex 客户端 TOML 中同名字段仍
   可作为客户端 provider 声明保留；
6. Search 继续保持同步、opaque、same-format-only 和按次计费。

本修正只涉及源码、测试和文档，不包含部署、重启、生产数据库写入或迁移执行。

---

## 17. 关键源码参照

### 17.1 Codex

- `/opt/stacks/openai-codex/codex-rs/codex-api/src/endpoint/search.rs`
- `/opt/stacks/openai-codex/codex-rs/ext/web-search/src/tool.rs`
- `/opt/stacks/openai-codex/codex-rs/ext/web-search/src/output.rs`
- `/opt/stacks/openai-codex/codex-rs/app-server/tests/suite/v2/web_search.rs`

### 17.2 Sub2API `origin/main`

- `backend/internal/handler/openai_alpha_search.go`
- `backend/internal/service/openai_alpha_search.go`
- `backend/internal/service/openai_alpha_search_test.go`
- `backend/internal/service/openai_alpha_search_billing_test.go`
- `backend/internal/service/account.go`
- `backend/internal/service/openai_account_scheduler_test.go`
- `backend/internal/server/routes/gateway.go`
- `backend/internal/handler/endpoint.go`
- `backend/internal/service/openai_endpoint_url.go`
- `backend/internal/web/embed_on.go`
- `backend/migrations/174_group_web_search_price_per_call.sql`（只作 SQL 语义参考）

关键提交：

- `52071d391`：初始 Search route/handler/service；
- `7cbb36f27`、`64a2a3172`、`e5af699d0`：按次计费及修复；
- `b0fa2b352`：嵌入前端 bypass；
- `776f3f0de`、`695665cbc`、`72fada40f`：PAT/capability/fallback；
- `d2b080e88`：恢复 API Key Search 调度和 API Key 404/405 failover。

### 17.3 Sub2API `custom-prod`

- `backend/internal/server/routes/gateway.go`
- `backend/internal/service/account.go`
- `backend/internal/service/openai_account_scheduler.go`
- `backend/internal/service/openai_gateway_service.go`
- `backend/internal/handler/usage_response_timing.go`
- `backend/internal/service/usage_response_timing.go`
- `backend/internal/repository/http_upstream.go`
- `backend/migrations/180_usage_log_first_byte_ms.sql`

### 17.4 Aether

- `crates/aether-ai-formats/src/formats/id.rs`
- `crates/aether-ai-formats/src/contracts/plan_kinds.rs`
- `crates/aether-ai-formats/src/contracts/report_kinds.rs`
- `apps/aether-gateway/src/api/ai/registry.rs`
- `apps/aether-gateway/src/api/ai/openai.rs`
- `apps/aether-gateway/src/control/route/ai.rs`
- `apps/aether-gateway/src/control/tests/ai.rs`
- `apps/aether-gateway/src/ai_serving/planner/`
- `apps/aether-gateway/src/client_session_affinity.rs`
- `apps/aether-gateway/src/ai_serving/planner/candidate_affinity_cache.rs`
- `apps/aether-gateway/src/scheduler/affinity.rs`
- `crates/aether-provider-pool/src/providers/codex.rs`
- `crates/aether-contracts/src/usage.rs`
- `crates/aether-billing/src/event_enrichment.rs`
- `crates/aether-billing/src/pricing.rs`
- `crates/aether-billing/src/default_rule.rs`
- `crates/aether-billing/src/service.rs`
- `crates/aether-admin/src/system.rs`
- `frontend/src/api/endpoints/types/api-format.ts`
