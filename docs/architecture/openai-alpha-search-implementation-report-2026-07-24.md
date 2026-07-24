# OpenAI Codex `alpha/search` 跨仓实施报告

- **状态：** 代码已实施并完成本地验证
- **日期：** 2026-07-24
- **Aether 分支/审查基线：** `custom` / `4c6a3970`
- **Sub2API 分支/审查基线：** `custom-prod` / `f08a0b106`
- **Codex 协议参考：** `/opt/stacks/openai-codex@81da9deb`
- **生产状态：** 未部署、未重启、未执行数据库迁移
- **调查报告：** `openai-alpha-search-endpoint-investigation-2026-07-24.md`
- **执行方案：** `openai-alpha-search-execution-plan-2026-07-24.md`

---

## 1. 客观结论

本次已在 Aether 与 Sub2API 两个代码库完成 standalone Search 链路：

```text
Codex client
  -> Sub2API POST /v1/alpha/search
  -> Aether POST /v1/alpha/search
  -> ChatGPT Codex POST /backend-api/codex/alpha/search
```

实现结果符合调查阶段的核心判断：Search 在公共路由、权限、审计、usage 和计费上是
独立 surface，但在 Aether 的最终 provider 选择上复用 Responses/Codex 候选体系。它没有
加入 Chat、Responses、Claude Messages 或 Gemini 的通用格式转换矩阵。

当前代码仅完成开发与本地验证。数据库迁移 `181_group_web_search_price_per_call.sql` 尚未
在任何生产数据库执行，两个服务也没有因本任务被部署或重启。

---

## 2. 源码参照路径

### 2.1 OpenAI Codex

| 内容 | 参照路径 |
| --- | --- |
| Search HTTP endpoint client | `/opt/stacks/openai-codex/codex-rs/codex-api/src/endpoint/search.rs` |
| Search request/response types | `/opt/stacks/openai-codex/codex-rs/codex-api/src/search.rs` |
| standalone web search tool | `/opt/stacks/openai-codex/codex-rs/ext/web-search/src/tool.rs` |
| tool output wrapper | `/opt/stacks/openai-codex/codex-rs/ext/web-search/src/output.rs` |
| app-server web search tests | `/opt/stacks/openai-codex/codex-rs/app-server/tests/suite/v2/web_search.rs` |

### 2.2 Sub2API 上游提交参考

本次核对的 `origin/main` Search 提交链为：

```text
52071d391  initial alpha/search endpoint
7cbb36f27  per-call billing
b0fa2b352  frontend bypass
776f3f0de  PAT forwarding alignment
695665cbc  PAT fallback via Responses web_search
72fada40f  builder lint fixes
d2b080e88  restore APIKey account scheduling
```

`custom-prod` 与 `origin/main` 已发生较大结构分叉，因此没有机械 cherry-pick 整组提交；
实施时以这些提交的最终行为和测试为参考，适配到当前路由、调度、Ent schema、usage timing
和前端结构。

---

## 3. Aether 实施结果

### 3.1 独立 API surface

- 新增格式：`openai:search`；
- 新增公共路由：`POST /v1/alpha/search`；
- 仅允许同步 JSON；
- 不支持 SSE、WebSocket 和格式转换；
- 独立 plan/report kind、usage request type、权限和路由分类；
- 管理端和前端 API format 列表已加入 `openai:search`。

主要文件：

- `crates/aether-ai-formats/src/formats/id.rs`
- `crates/aether-ai-formats/src/formats/openai/responses/spec.rs`
- `apps/aether-gateway/src/api/ai/openai.rs`
- `apps/aether-gateway/src/ai_serving/planner/route.rs`

### 3.2 Responses companion planner

Search 客户端格式保持 `openai:search`，候选锚点使用 `openai:responses`。候选必须满足：

- provider 为 Codex；
- endpoint 为 Responses；
- capability 包含 `supports_standalone_web_search=true`。

这使 Search 能复用现有 Codex OAuth key pool、模型映射、profile 和身份头，同时避免把
Search 的权限、账单和指标混入 Responses。

### 3.3 请求和响应处理

- request body 按 opaque JSON 处理；
- 仅修改映射后的 `model`；
- 删除 Responses 专用顶层字段 `prompt_cache_key`、`prompt_cache_retention`；
- 官方目标 URL 为 ChatGPT Codex API root 加 `/alpha/search`；
- 删除 Responses session/beta 状态头，补齐稳定的 Codex version、originator、UA 和认证头；
- response status、content type 和 body 保持透传；
- 成功实际发出上游请求后返回
  `x-aether-upstream-disposition: dispatched`。

### 3.4 粘性与状态型 `ref_id`

- 使用顶层 `id` 作为 Search session affinity；
- 与 Responses 使用同一候选锚点命名空间；
- 迭代扫描 body 内全部层级的非 URL `ref_id`，不再使用可被深层嵌套绕过的固定深度截断；
- 只有带 authority 的绝对 `http`/`https` URI 被视为无状态 URL；
- 状态型请求没有既有绑定时返回 `409 search_session_affinity_lost`；
- 已绑定候选不匹配时不得执行其他候选；
- 状态型请求一旦执行，不允许跨候选 failover；
- Search 不进入 control fallback。

### 3.5 计费与隐私

- usage request type 为 `search`；
- 只读取 Search surface scoped 的按次价格；
- 支持 `surface_pricing["openai:search"].price_per_request`、
  `api_format_pricing`、`search_price_per_request`、`web_search_price_per_call`；
- 不回退到 Responses/模型共享的无格式 `price_per_request`；
- 即使上游返回 token/image 维度，Search 规则也只按 Search 请求次数结算；
- report context 不保存原始 Search body，仅保存模型、ID 是否存在、command 类型和已脱敏摘要。

---

## 4. Sub2API 实施结果

### 4.1 路由和前端 bypass

已注册：

```text
POST /v1/alpha/search
POST /alpha/search
POST /backend-api/codex/alpha/search
```

三个路径均进入现有 body limit、request ID、usage response timing、ops、API key auth 和
group assignment 链路。嵌入前端不会再吞掉这些 API 路径。

### 4.2 `d2b080e88` 必要行为

`OpenAIEndpointCapabilityAlphaSearch` 允许以下两类账号进入候选池：

- `AccountTypeOAuth`：直接请求 ChatGPT Codex standalone Search；
- `AccountTypeAPIKey`：请求 `{base_url}/v1/alpha/search`，用于指向 Aether。

API Key 上游返回 404/405 时按“端点未实现”处理：允许换号，但不把账号整体标为错误。
这正是 `d2b080e88` 对 Aether 链路的必要性：没有该行为，Aether API Key 账号会在请求
发出前被调度器排除。

### 4.3 Aether 线协议适配

- Aether account 使用 `Authorization: Bearer <aether-key>`；
- base URL 无论是 `https://aether.example` 还是 `https://aether.example/v1`，最终都只生成
  一个 `/v1/alpha/search`，不会出现双 `/v1`；
- 删除 `prompt_cache_key` 和 `prompt_cache_retention`；
- 删除 `OpenAI-Beta`、`Session_ID`、`Conversation_ID` 等 Responses 专用头；
- 同时删除 `X-OpenAI-Internal-Codex-Responses-Lite`，避免 Responses Lite 内部状态泄漏到
  standalone Search；
- 保留未知 Search body 字段；
- 保留 Aether 的 `x-aether-upstream-disposition` 响应头；
- Aether 返回 409 时原样透传，不触发 Sub2API 外层 failover，也不计费；
- 对包含非 URL `ref_id` 的状态型请求，任何已发出上游尝试后的可 failover 错误都禁止
  Sub2API 跨账号重放，而不只限制 409；
- API Key base URL 自带 query 时先正确追加 `/alpha/search`，再与入站 query 合并；
- API Key 上游 404/405 触发端点级 failover。

### 4.4 按次计费

- `OpenAIForwardResult.WebSearchCalls=1` 只在最终 2xx 时设置；
- 上游错误透传或 failover 不产生 Search usage；
- 新增分组字段 `web_search_price_per_call`；
- `NULL` 使用默认价 `$0.01/次`，显式 `0` 表示免费；
- 最终费用为 `单价 × 成功次数 × 分组有效倍率`；
- Search 不进入 token/image/video 定价路径；
- API key auth snapshot 版本从 16 升至 17，避免旧缓存缺少新价格字段。

数据库迁移文件：

```text
/opt/stacks/sub2api/backend/migrations/181_group_web_search_price_per_call.sql
```

### 4.5 PAT 限制

上游 `776f3f0de`、`695665cbc` 依赖更早的 PAT 基础提交 `32df33a1c`。当前
`custom-prod@05cb36dd` 没有该 PAT 账号模型、whoami 校验和 Responses web_search fallback
基础，因此本次没有把 PAT fallback 伪装成已完成。

当前已验证范围是普通 OAuth 和 API Key/Aether。以后若把 `32df33a1c` 或等价 PAT 能力
合入 `custom-prod`，必须同时重新移植并验证 `776f3f0de`、`695665cbc`、`72fada40f` 的
PAT Search 行为。

---

## 5. 验证证据

### 5.1 Aether

```text
cargo test -p aether-ai-formats openai_search --locked
  3 passed

cargo test -p aether-provider-transport openai_search --locked
  1 passed

cargo test -p aether-billing openai_search --locked
  2 passed

cargo test -p aether-gateway search --locked
  26 passed

cargo check -p aether-gateway --tests --locked
  passed

frontend: npm run type-check
  passed

frontend: npm run test:run
  84 files / 479 tests passed

frontend: npm run test:run -- \
  src/utils/__tests__/providerKeyQuota.spec.ts \
  src/views/admin/__tests__/PoolManagement.codex-cycle-stats.spec.ts
  2 files / 22 tests passed

frontend: npm run build
  passed

frontend: npm audit --json
frontend: npm audit --omit=dev --json
  0 vulnerabilities
```

### 5.2 Sub2API

```text
go test ./internal/service ./internal/handler ./internal/repository ./internal/server/routes \
  -run 'AlphaSearch|WebSearch|GroupWebSearch|APIKeyAuthSnapshot' -count=1
  passed

frontend: pnpm typecheck
  passed

frontend: eslint changed Search/group files
  passed

git diff --check
  passed
```

覆盖的关键回归包括：

- 三条路由存在；
- 非 OpenAI group 被拒绝；
- API Key 类型 Aether account 能进入 AlphaSearch 调度；
- base URL 不产生双 `/v1`；
- prompt cache 字段和 Responses 专用头被删除；
- 未知 body 字段保留；
- Aether 409 不外层 failover、不计费；
- API Key 404/405 触发端点级 failover；
- 只有 2xx 设置 `WebSearchCalls=1`；
- Search 使用独立按次价格，不使用 token/image 价格。

---

## 6. 未执行事项与上线前条件

### 6.1 二次审查补充

标签发布前的二次审查额外发现并修复了以下问题：

- Aether 原 32 层 `ref_id` 扫描上限会把更深层的 opaque reference 误判为无状态，现改为
  迭代完整扫描并增加 64 层回归测试；
- Aether Search planner 直接引用 `aether_ai_formats::api`，会触发 release workflow 的架构
  边界测试，现改为通过 `ai_serving` 根 seam 引用；
- Sub2API 原先仅依赖 Aether 409 阻止外层换号，对状态型请求的 401/403/429/5xx 仍可能
  跨账号重放，现统一 fail closed；
- Sub2API API contract fixture 未包含新增的 `web_search_price_per_call` 字段，已补齐；
- Sub2API Security Scan 发现 `golang.org/x/text`、Axios 和 PostCSS 高危公告，已升级到无
  可达漏洞/无高危生产依赖的版本；
- Sub2API golangci-lint 暴露了既有格式、弃用 API 和安全注释问题，已逐项修复并用 CI
  同版 v2.9 验证为 0 issues；
- Aether 前端生产依赖审计发现 Axios、form-data 和 DOMPurify 公告，完整依赖树还包含
  Vite/Vitest 开发工具链的高危或严重公告；已升级 Axios `1.18.1`、form-data `4.0.6`、
  DOMPurify `3.4.12`、Vite `7.3.6`、Vitest/UI `4.1.10`，干净安装后的完整和生产依赖
  审计均为 0 vulnerabilities；
- Aether 全量前端测试暴露了一个既有的过期断言：实现从 2026-05 起会把旧存储状态
  `active` 迁移为 `available`，测试仍期待 `active`。现已修正普通回退用例，并增加旧值
  迁移回归测试；全量 479 项测试通过。

上述修复仍只涉及源码、测试、依赖锁文件和文档，没有执行生产迁移、部署、重启或容器
更新。

以下事项没有在本任务中执行：

- 未执行 migration 181；
- 未部署或重启 Aether/Sub2API；
- 未修改容器、镜像、systemd、反向代理或数据库；
- 未调用真实 OpenAI/ChatGPT Search 端点；
- 未启用任何生产 capability 或账号；
- 未验证真实生产额度、风控或官方 Alpha 服务端的未公开限制。

上线前至少需要：

1. 在 Aether 最终 Codex candidate/key 上显式启用
   `supports_standalone_web_search=true`；
2. 在 Sub2API 创建或确认 API Key 类型的 Aether account，配置 Aether base URL 和 key；
3. 审核并执行 migration 181；
4. 确认 Sub2API group 的 Search 单价与 Aether 上游 Search 成本定价，避免双层价格口径错误；
5. 使用非生产凭据完成一组真实或 mock 联合冒烟测试；
6. 由用户另行明确授权后再部署、迁移或重启。

---

## 7. 最终评价

从代码结构和本地测试看，`alpha/search` 适合作为与 OpenAI Chat、OpenAI Responses 同级
可见的独立 API surface，但不适合作为可互转的第三种生成协议。当前实现已经把 surface
隔离、Responses companion 候选、Aether API Key 调度、强粘性、按次计费和 opaque wire
compatibility 分开处理。

剩余风险主要来自 Alpha 协议本身可能变化、真实官方服务端行为尚未联调，以及 PAT 基础
能力尚未进入 `custom-prod`，而不是当前 Aether API Key 链路的已知结构缺口。
