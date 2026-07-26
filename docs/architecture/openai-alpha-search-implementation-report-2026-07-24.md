# OpenAI Codex `alpha/search` 跨仓实施报告

- **状态：** 原链路已上线并完成线上诊断；2026-07-26 提供商端点级修正已在本地完成，尚未发布
- **日期：** 2026-07-26（基于 2026-07-24 报告补充）
- **Aether 分支/审查基线：** `custom` / `c6647fdb7`（工作区含未提交修改）
- **Sub2API 分支/当前检查基线：** `custom-prod` / `d2cb9e445`
- **Codex 协议参考：** `/opt/stacks/openai-codex@81da9deb`
- **生产状态：** 旧版 Search 已上线；本轮端点级修正未部署、未重启，也未执行生产数据库操作
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
独立 surface；当前修正版还把最终 Codex 选择收敛到独立的 `openai:search` 提供商端点。
它没有加入 Chat、Responses、Claude Messages 或 Gemini 的通用格式转换矩阵，也不再用
Responses 端点或账号 capability 作为 Search 的开关。

本轮 Aether 端点级修正仅完成开发与本地验证，没有部署或重启服务，也没有执行数据库
迁移。旧版 Search 上线时的生产迁移状态未在本轮重新核验，因此本报告不对 migration 181
当前是否已执行作额外断言。

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

### 3.2 提供商端点级 Search planner（2026-07-26 修正版）

Search 客户端格式和候选格式均为 `openai:search`。候选必须满足：

- provider 类型为 Codex；
- provider endpoint 的 `api_format` 为 `openai:search`；
- provider、endpoint、key、model 均处于可调度状态。

Codex OAuth key、模型映射和账号 profile 仍复用现有 Codex 体系，但 Search 是否可用由
提供商端点的 `is_active` 决定。`supports_standalone_web_search` 不再是 Aether 账号级
调度门禁；历史字段只作为兼容数据读取，不参与 Search 选择。

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

截至 2026-07-24 首轮实施验收，上述修复只涉及源码、测试、依赖锁文件和文档，当时没有
执行生产迁移、部署、重启或容器更新。旧版 Search 随后已上线，线上结果见 6.2。

以下事项截至首轮实施验收没有执行；本轮端点级修正同样没有执行任何生产操作：

- 首轮验收时未执行 migration 181；本轮未重新核验其线上状态；
- 本轮未部署或重启 Aether/Sub2API；
- 未修改容器、镜像、systemd、反向代理或数据库；
- 本次诊断未由我们主动调用真实 OpenAI/ChatGPT Search 上游端点（线上客户端请求仍会经过 Aether；见 6.2）；
- 未更改任何生产 endpoint 开关或账号配置；
- 未验证真实生产额度、风控或官方 Alpha 服务端的未公开限制。

本轮修正版发布前至少需要：

1. 确认 Codex provider 的 `openai:search` endpoint 已由模板 reconcile 创建，并按需通过
   endpoint 的启用/停用按钮控制；
2. 在 Sub2API 创建或确认 API Key 类型的 Aether account，配置 Aether base URL 和 key；
3. 核验 migration 181 的线上状态；仅在确认尚未执行且另行获得生产授权后再执行；
4. 确认 Sub2API group 的 Search 单价与 Aether 上游 Search 成本定价，避免双层价格口径错误；
5. 使用非生产凭据完成一组真实或 mock 联合冒烟测试；
6. 由用户另行明确授权后再部署、迁移或重启。

### 6.2 线上更新后的实测诊断（2026-07-26，旧代码行为）

本节记录的是旧实现上线后的客观诊断，保留用于解释为什么需要本次设计修正；其中
“Responses endpoint + 账号 capability”结论已被 6.3 的端点级设计取代。

生产更新后的实测显示“Searching the web”后返回访问限制。需要区分两类请求：

1. `40016fbb-6ef6-4a04-b6d0-2d74c047a42e`（页面只显示前缀 `40016fbb`）是本次诊断主动发出的
   无 Authorization 路由探针，预期结果就是 `503 / missing_auth_context / User: Unknown`，不是用户
   的真实失败请求。
2. 随后真实 Search 请求（例如 trace `21437401-c442-4972-bad0-b9246721d8e0`）已带有效认证并命中
   `/v1/alpha/search`，Aether 记录 `route_kind=search`、`status=allowed`，但最终为
   `no_local_sync_plans`。Sub2API 的错误记录也确认请求进入 `/v1/alpha/search`，而非旧的
   `/v1/responses` 路径。

只读检查得到以下证据：

- Aether 运行镜像 revision 为 `e1343300e`，Sub2API 为 `c76631bc4`，两个容器均健康；
- `gpt-5.6-sol` 已存在于 Codex Pro 的 active `openai:responses` endpoint，模型映射不是缺失；
- 当前 active Codex Pro key 均只声明 `codex_official_ws=true`，
  `supports_standalone_web_search=true` 的 key 数量为 0；
- 真实失败请求的候选诊断为 `candidate_count=31`、`skipped_candidate_count=58`，可见候选全部因
  `openai_search_codex_responses_candidate_required` 被跳过，未进入上游执行。

根因有两个层次：

* **已确认的直接代码缺陷**：Search 入口先用请求体顶层 `id` 生成 `session=<id>` 的 Codex 会话
  亲和键；随后 `attach_routing_policy_to_local_requested_model_input` 又用普通 Responses 的
  通用字段重算亲和键。通用解析不读取顶层 `id`，导致亲和键被清空。Codex Pro provider 配置了
  `pool_advanced.avoid_anonymous=true`，于是该池在候选选择阶段被当成 anonymous 请求排除，剩余
  custom Responses 候选才统一触发 `openai_search_codex_responses_candidate_required`。
* **能力配置/安全门控缺口**：Search 所需的 `supports_standalone_web_search` 在旧实现中只影响
  候选排序，payload 层没有对具体 pool key 做硬门控。这样即使亲和修复后，也可能错误地把未显式
  开启 Search 的 Codex key 用于 Search。

当时的本地源码曾修复上述两点：

- `apps/aether-gateway/src/client_session_affinity.rs` 新增无 `Parts` 依赖的 Search 亲和解析；
- `apps/aether-gateway/src/ai_serving/planner/decision_input.rs` 在 `openai:search` routing
  路径保留顶层 `id` 亲和；
- `apps/aether-gateway/src/ai_serving/planner/standard/openai/responses/decision/request.rs`
  对具体候选增加 `supports_standalone_web_search` 硬门控，并返回明确的
  `openai_search_standalone_web_search_capability_required` 跳过原因。

目标回归测试已通过：Search 亲和测试 1/1、Search 请求/候选契约测试 6/6、客户端亲和全量测试
27/27。在本轮端点级修正开始前，该组源码修复只确认完成了本地验证；本轮没有重新核验
生产是否已采用对应提交，也没有修改生产数据库、provider key、容器或服务。

Codex 源码确认自定义 provider 要稳定选择 standalone Search，客户端配置需要同时满足：

```toml
[model_providers.example]
wire_api = "responses"
supports_standalone_web_search = true

[features]
standalone_web_search = true
```

第一个字段声明自定义 provider 支持该端点；第二个字段在普通 Responses 模式启用 standalone
Search feature。缺少任一条件时，非 Responses Lite 客户端可能继续暴露 hosted Responses
web search，而不会请求 `/v1/alpha/search`。

因此旧方案要求启用账号 capability；该要求在本次修正中取消，线上恢复应按 6.3 的提供商
端点流程执行。

### 6.3 端点级设计修正（2026-07-26）

本次修改的目标不是增加第二个账号开关，而是让 Search 与 OpenAI Chat/Responses 一样，
通过提供商端点的存在和 `is_active` 控制是否参与调度：

1. **固定端点模板**：Codex 固定 provider 模板版本升至 2，端点集合包含
   `openai:responses`、`openai:responses:compact`、`openai:search` 和 `openai:image`。
   `openai:search` 使用 Codex API root，目标路径仍为 `/alpha/search`。
2. **现有 provider 自动补全**：后台节点启动时执行固定端点模板 reconcile；provider 创建或
   更新时沿用既有 reconcile。新 Search endpoint 默认启用，管理员可在通用端点管理界面
   点击 Power 按钮停用；托管端点的显式停用状态由 metadata 保留。
3. **候选选择**：Search 只查询 `openai:search` endpoint，禁止 Search 进入普通格式转换
   矩阵，也不再把 `openai:responses` 作为候选锚点。Codex OAuth 的历史 `api_formats` 列表
   不会遮蔽新端点，但 endpoint inactive 会在候选过滤阶段生效。
4. **账号配置清理**：Aether 前端移除 Key/OAuth 编辑对话框中的 Search capability 开关；
   编辑旧记录时删除 `supports_standalone_web_search` 这个历史字段。该字段仍可能出现在
   Codex 客户端 TOML（`[model_providers.*]` 与 `[features]`），那是客户端声明和功能开关，
   不属于 Aether provider key 配置。
5. **认证与 profile**：Search 仍使用 Codex OAuth 的 Bearer 认证、账号 ID、稳定 user-agent/
   originator 和 TLS profile；`version` 从最终 user-agent 重新派生，不接受客户端残留值。
   可解析的 `x-codex-turn-metadata` 中的 `installation_id` 会
   改写为最终 Codex key 的稳定 installation identity，无法解析的 metadata 会被丢弃；
   客户端直接提供的 `x-codex-installation-id` 不向 Search 上游透传。该处理不会向 Search
   body 注入 Responses 专用的 `client_metadata`。
6. **伴生账号亲和**：Search 精确绑定仍保存在独立的 `openai:search` namespace。首次 Search
   尚无精确绑定时，候选排序会读取同一会话的 `openai:responses` 绑定作为 provider/key
   提示；Search 自身绑定一旦存在便具有更高优先级。非 URL `ref_id` 仍只接受精确 Search
   绑定，不能依靠 Responses 提示绕过 `409 search_session_affinity_lost`。
7. **计费**：本次没有改变 Search 的按次计费规则；`openai:search` 继续使用 surface-scoped
   `price_per_request`，不回退到 Responses token 价格。

本地验证（仅源码和测试，未部署）包括：

```text
cargo check -p aether-gateway                         passed
cargo test -p aether-provider-transport codex_fixed_provider_template --lib  passed
cargo test -p aether-ai-formats search_candidate_registry_is_same_format_only --lib  passed
cargo test -p aether-gateway openai_search_ --lib      7 passed
cargo test -p aether-gateway codex_pool_ --lib         18 passed
cargo test -p aether-data candidate_selection --lib    28 passed
frontend: npm run type-check                           passed
git diff --check                                       passed
```

上线后验证重点是：后台节点日志出现固定端点 reconcile；管理端能看到 `OpenAI Search`；
停用该端点后 Search 返回本地无候选且不会调用 Responses；启用后再用非生产凭据确认
`https://chatgpt.com/backend-api/codex/alpha/search` 的 2xx 透传和独立按次计费。

---

## 7. 最终评价

从代码结构和本地测试看，`alpha/search` 适合作为与 OpenAI Chat、OpenAI Responses 同级
可见的独立 API surface，但不适合作为可互转的第三种生成协议。当前修正版把 surface
隔离、Codex Search provider endpoint、强粘性、按次计费和 opaque wire compatibility 分开
处理；账号级 Search capability 不再承担 provider 开关职责。

剩余风险主要来自 Alpha 协议本身可能变化、真实官方服务端行为尚未联调，以及 PAT 基础
能力尚未进入 `custom-prod`，而不是当前 Aether API Key 链路的已知结构缺口。
