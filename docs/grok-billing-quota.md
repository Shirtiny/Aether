# Grok 额度管理：接入 xAI billing 作为权威数据源（aether）

> 设计文档。参考实现：`sub2api` (`/opt/stacks/sub2api`，真上游 `Wei-Shaw/sub2api` @ `5a6143097` / v0.1.168) 的 `GrokQuotaService` + `internal/pkg/xai/billing.go`。
> 前置文档：[grok-xai-oauth-migration.md](./grok-xai-oauth-migration.md)。

## 背景（为什么做）

2026-07-29 一次 `grok-4.5` 请求以 `HTTP 503 / execution_runtime_candidates_exhausted` 失败，上游真实错误是：

```json
{"error":{"type":"server_error","message":"Grok Build usage balance exhausted"}}
```

HTTP 402。但同一时刻 aether 记录的该号额度是**满格**。查库确认（`provider_api_keys.upstream_metadata->'grok'`，两个号数值完全一致）：

| window | limit | remaining | used_value | used_ratio | reset_at |
|---|---|---|---|---|---|
| requests | 8300 | 8300 | 0 | 0.0 | null |
| tokens | 53000000 | 53000000 | 0 | 0.0 | null |

聚合层 `exhausted: false`、`code: "ok"`、`usage_ratio: 0.0`，`observed_at` 冻结在 23 小时前。

三个成因：

1. **上游数字是静态天花板，不是用量计数器。** xAI 在成功响应里回的 `x-ratelimit-remaining-* == x-ratelimit-limit-*` 且不带 reset，`parse_quota_window`（`crates/aether-provider-pool/src/providers/grok.rs:190`）照单全收，`used_ratio` 恒为 0。两个不同账号数值一模一样，说明是套餐常量。`admin_pool_attach_grok_local_usage_observation`（`apps/aether-gateway/src/handlers/admin/provider/pool_admin/payloads.rs:496-501`）的注释已经识别出这件事并标了 `remaining_source: upstream_static_ceiling`——那个标记本身就是在说"这个数别信"。
2. **"本地累计已用"是终身累计。** UI 上并排显示的 `请求 3062 | Token 311446493` 是建库以来的结算总量，跟 8300 / 53,000,000 这个（周期未知的）窗口不可比，所以 token 会远超"上限"。
3. **402 完全不写额度。** `parse_grok_quota_headers`（`grok.rs:164`）在无 ratelimit 头时返回 `None`，402 响应恰好没有这些头，`sync_grok_quota_from_response_headers` 直接 `return Ok(false)`（`apps/aether-gateway/src/orchestration/report_effects.rs:492`），快照原样停在 100%。这是有意为之（`grok.rs:159-163` 注释：怕把无 reset 的 429 写成永久耗尽），代价是余额耗尽在额度视图里永远不可见。

**根因**：402 的 "usage balance" 是**计费余额**维度，与 `x-ratelimit-*` 的**速率窗口**是两套东西。xAI 不在响应头里暴露余额，只看 header 永远预测不到 402。

`cpa` 同样没做（它只给 Antigravity 建了 credits 账本）。`sub2api` 是三者中唯一做对的：它去查了 `/billing`。

## 已确定的决策

- **只把额度数据做准，不改调度语义。** 本轮不让 billing 结果反向影响 `quota_exhausted`（`crates/aether-provider-pool/src/providers/grok.rs:50`）的返回值——aether 已有熔断机制，额度数据驱动选号会引入新的失效模式。`exhausted` 的判据留到 P0 实测拿到真实样本后单独决策。
- **不做 402 冷却。** 熔断已覆盖，不重复建设。
- **billing 优先，header 兜底。** 付费号以 billing 为权威；Free 号（xAI 对其不返回 `usage_percent`）继续用 ratelimit 头 + 滚动 24h 窗口。
- **保持现有存储形状。** billing 作为 `upstream_metadata->grok->billing` 子对象，与现有 `windows[]` 平级，不动 `windows[]` 的语义，老快照继续可读。
- **P0 是阻塞步骤。** 三个关键判据（见「待验证」）全部取决于真实响应，未实测前不写解析以外的任何决策逻辑。

## 数据源分层

```
                    ┌──────────────────────────────────────┐
  付费号 ──────────▶│ GET {base}/billing?format=credits    │ 周/credits 窗口
                    │ GET {base}/billing                   │ 月/订阅窗口
                    └──────────────────────────────────────┘
                              │ 权威判定
                              ▼
                    grok_billing_has_authoritative_quota()
                      = usage_percent.is_some()
                     || used_percent.is_some()
                     || monthly_limit_cents > 0
                     || !plan.is_empty()
                              │
                    ┌─────────┴─────────┐
                   是                   否
                    │                    │
              billing 权威        ┌──────────────────────┐
              不发模型探测   ────▶│ x-ratelimit-* 头      │ Free 号 + 兜底
                                  │（现有路径，不动）      │
                                  └──────────────────────┘
```

请求形态（`sub2api/backend/internal/pkg/xai/billing.go:22-23,117`）：

```
GET {base}/billing?format=credits
GET {base}/billing

Authorization: Bearer <access_token>
x-xai-token-auth: xai-grok-cli
x-grok-client-version: <ver>
User-Agent: <grok cli ua>
```

`{base}` 直接用 key 所属 endpoint 的 `base_url`。**aether 现有四个 grok endpoint 全是 `https://cli-chat-proxy.grok.com/v1`**（已查库确认），与 sub2api 的 OAuth 缺省一致，无需新增配置。CLI 身份头 aether 已有：`crates/aether-provider-transport/src/grok.rs:17-21`（版本 `0.2.103`，比 sub2api 的 `0.2.93` 新）。

## 工作分解

> 进度（2026-07-29）：P0 实测完成，P1 / P2 完成，P3 除「本地用量按 billing period 取窗口」外完成，
> 并已按「billing 投影成通用配额窗口」做了一轮系统化重构（见下）。
> 测试：provider-pool grok 20/20、gateway pool_admin+grok 82/82、前端 providerKeyQuota 11/11，`cargo check -p aether-gateway` 通过。
> **未部署**——代码停在源码层面。

> **决策变更（P0 之后）**：原定「不让 billing 驱动 `exhausted`」的保守选择已推翻。理由是实测发现 billing 自带 `currentPeriod.end` 重置截止时间，
> 「耗尽却无法恢复」的风险不存在。现判据为 `creditUsagePercent >= 100 && onDemandCap == 0 && prepaidBalance == 0`，
> 三者缺一即不封；**任一 overflow 字段缺失按「未知」处理、不封号**——错封一个健康号要付一整个计费周期，漏封只是重复一次已有熔断兜底的 402。

### P0 — 只读验证

**原方案**是在应用外写脚本解密 `auth_config` 拿 token 再直接打 billing。放弃了：那要求把 OAuth 凭据解密到应用之外，是不必要的暴露面（权限分类器也会拦这类脚本模式，理由正当）。

**改为**：验证内建进 P2 的探测路径。`refresh_grok_provider_quota_locally` 在每个 key 的结果里回显未经处理的上游 billing 响应体：

```json
"billing_source": "authoritative" | "rate_limit_headers",
"billing_raw": { "weekly": <原始 body>, "monthly": <原始 body> }
```

凭据始终留在应用内（复用 `resolve_local_oauth_header_auth` + `execute_grok_quota_plan` 的代理/超时/身份头解析），管理员在后台点一次「刷新配额」即可拿到验证所需的 4 份原始响应体。

验证对象仍是这两个号：

| key | 状态 | 用途 |
|---|---|---|
| `09016042425a@gmail.com` | 已 402（余额耗尽） | 确定 `exhausted` 判据 |
| `shirtiny@gmail.com` | 正常 | 对照组 / 确定权威判据 |

`billing_raw` 是验证仪器，不是长期契约——「待验证」三问回答完即可摘除。

### P1 — 采集与存储

**`crates/aether-provider-pool/src/providers/grok.rs`**（现 712 行，新增约 250 行）

- `build_grok_billing_request(key_id, base_url, resolved_oauth_auth, decrypted_api_key, weekly: bool) -> Result<ProviderPoolQuotaRequestSpec, String>`
  - 仿现有 `build_grok_pool_quota_request`（:80）的鉴权分支与 base_url 归一化
  - `method: "GET"`、`json_body: None`、`content_type: None`、`quota_kind: "grok:xai_billing"`、`model_name: Some("grok-billing")`
  - GET 型配额探测在本仓已有先例：`providers/codex.rs:123`（`CODEX_WHAM_USAGE_URL`）、`providers/kiro.rs:105`，`build_provider_quota_execution_plan` 对空 body 已正确处理（`quota/shared.rs:246-252`）

- `parse_grok_billing_payload(body: &[u8], status_code: u16, weekly: bool, observed_at: u64) -> Option<Value>`
  - 入口是 `{"config": {...}}`
  - 取 `creditUsagePercent` / `monthlyLimit` / `used` / `currentPeriod{type,start,end}` / `productUsage[]` / `billingPeriodStart|End`
  - `monthlyLimit`、`used` 有三种形态：`{"val":N}` / 裸数 / 字符串 → 统一走 `parse_cent_value`（对齐 `billing.go:319` 的 `parseCentValue`）
  - 派生：`included_used_cents = min(used, limit)`；`used_percent = included_used / limit * 100`；`plan = resolve_plan(limit)`（`15000 → "SuperGrok"`、`150000 → "SuperGrok Heavy"`、其他 `""`，允许浮点噪声，对齐 `billing.go:311`）
  - **周月不混用**：monthly-only 时不把月百分比写进 `usage_percent`（对齐 `billing.go:214-217`），否则前端周进度条会串味

- `merge_grok_billing_snapshot(previous, weekly, monthly, weekly_ok, monthly_ok) -> Option<Value>`
  - 周/月两域**独立**合并：失败的域保留上次值，成功的域覆盖并写 `weekly_updated_at` / `monthly_updated_at`
  - 失败记 `partial: true` + `failed_windows: ["weekly"]`，并保留 `weekly_status_code` / `monthly_status_code`
  - 两域全失败且无历史 → `None`
  - 对齐 `billing.go:234` 的 `MergeBillingProbeResult`

- `grok_billing_has_authoritative_quota(billing: &Value) -> bool`（判据见上图，对齐 `grok_quota_service.go:115`）

存储形状（`upstream_metadata->grok` 新增 `billing` 子对象，与 `windows[]` 平级）：

```json
{
  "version": 2, "provider_type": "grok",
  "windows": [ … 现有，不动 … ],
  "billing": {
    "period_type": "weekly",
    "usage_percent": 32.1,
    "period_start": "…", "period_end": "…",
    "monthly_limit_cents": 15000, "used_cents": 4820,
    "included_used_cents": 4820, "used_percent": 32.13,
    "billing_period_start": "…", "billing_period_end": "…",
    "plan": "SuperGrok",
    "product_usage": [ {"product": "…", "usage_percent": … } ],
    "weekly_status_code": 200, "monthly_status_code": 200,
    "weekly_updated_at": 1785…, "monthly_updated_at": 1785…,
    "partial": false, "failed_windows": [],
    "observed_at": 1785…, "source": "billing_probe"
  }
}
```

`merge_grok_quota_snapshot`（`grok.rs:318`）需要把 `billing` 加进"整体替换而非逐字段合并"的键（它当前对 `windows` 特判、其余字段浅覆盖），避免 header 观测把 billing 域擦掉。**这是本次改动里最容易漏的一处**——`report_effects.rs:521` 和 `quota/shared.rs:187` 两条路径都会调它。

### P2 — 探测路径改造

**`apps/aether-gateway/src/handlers/admin/provider/oauth/quota/grok.rs`** — `refresh_grok_provider_quota_locally`（:74）改为：

1. 并发两个 billing GET（`tokio::join!`）
2. `grok_billing_has_authoritative_quota` 命中 → 落库、返回，**跳过模型探测**
3. 未命中（Free / billing 失败）→ 回落现有 `POST /responses` 探测

顺带修一个既有问题：现在每次刷新都无条件发 `POST /responses {"input":".","max_output_tokens":1}`（`grok.rs:117-122`），**每个号每次刷新烧一次请求额度**。改造后付费号刷新变成纯 GET，零额度成本——这正是 sub2api 那句注释的意思（*"opening the account list never consumes model quota"*）。

聚合决策 `recompute_grok_quota_aggregate`（`crates/…/providers/grok.rs:384`）：

- 有权威 billing → `usage_ratio = max(usage_percent, used_percent) / 100`
- **`exhausted` 本轮维持现状**（仅由 windows 推导），不接 billing。理由见「已确定的决策」第一条；判据待 P0 样本确定后单独提案。

### P3 — 展示

已完成：

- `build_grok_quota_status_snapshot`（`catalog.rs:1518`）把 `billing` 透传进 `status_snapshot.quota`，header 不带 tier 时用 `billing.plan` 兜底 `plan_type`；billing-only 的 bucket 也能 materialize（原本 `windows` 为空就直接返回 `None`）。
- 前端 `QuotaBillingSnapshot` 类型 + `getGrokQuotaText` 的 billing 分支（`providerKeyQuota.ts`），渲染 `SuperGrok · 周已用 100.0% · 月 $104.06/$150.00`；`partial` 时追加「部分窗口未刷新」。无 billing 时完全走原有逻辑，Free 号路径不受影响。
- `admin_pool_attach_grok_local_usage_observation`（`payloads.rs:502`）在 billing 权威时**直接返回**，不再挂 `upstream_static_ceiling` 和终身累计 `local_used_value`——那两个数和 billing 不同量纲，并排显示会被读成一把尺子。

**未完成**：本地用量按 billing period 取窗口。需要仿 codex 的做法新增 `read_admin_pool_grok_billing_usage_by_key`（`read_routes/keys.rs:154` 是模板），用 `summarize_usage_by_provider_api_key_windows` 按 `billing.period_start/end` 与 `billing_period_start/end` 批量查，再经 `build_admin_pool_key_payload` 已有的那个参数传进来。约 100 行，跨 2 个文件。目前的效果是「不显示误导数字」，而不是「显示对齐窗口的数字」。

其余待做：

**`apps/aether-gateway/src/handlers/admin/provider/pool_admin/payloads.rs`** — `admin_pool_attach_grok_local_usage_observation`（:502）：有权威 billing 时不再挂 `remaining_source: upstream_static_ceiling` + `local_used_value`，改为输出 billing 视图。付费号不该再看到"上游 8300/8300 + 本地累计已用 3.1 亿"这种并排。

**本地用量对齐 billing 窗口**：改用 `summarize_usage_by_provider_api_key_windows`（`crates/aether-data-contracts/src/repository/usage/types.rs:1648`，入参 `ProviderApiKeyWindowUsageRequest{window_code, start_unix_secs, end_unix_secs}`）——Codex 周期窗口已经在用这条路径（`payloads.rs:451`），起点取 `billing.period_start` / `billing_period_start`，并校验 `now ∈ [start, end)`（对齐 `account_usage_service.go:1089` 的 `currentGrokBillingWindow`）。这样本地数和上游数落在同一个窗口，可直接对比。

**`apps/aether-gateway/src/handlers/shared/catalog.rs`** — `build_grok_quota_status_snapshot`（:1518）把 billing 域透传进 `status_snapshot.quota`；`plan_type` 已经会读 `plan` 键（:1552-1555），billing 的 `plan` 可直接喂进去。

**`frontend/src/utils/providerKeyQuota.ts`** — `getGrokQuotaText`（:292）在 dimension 分支前插入 billing 分支：

```
SuperGrok · 周 32.1% · 月 $48.20/$150.00（本周期本地 1240 请求）
```

无 billing 时完全维持现有逻辑（Free 号路径不受影响）。

### P4 — 新鲜度（可选，视 P0~P3 落地情况再定）

aether 的 grok 配额刷新目前**只有管理员手动触发**一条路径（`quota/dispatch.rs:43`），没有 TTL 背景刷新。sub2api 的做法是打开账号列表时按 `openAIProbeCacheTTL = 10min` 判过期 + `grokProbeRetryTTL = 1min` 防抖 + singleflight 去重（`account_usage_service.go:113-114, 966, 1108`）。

最小可行增量：在 `sync_grok_quota_from_response_headers`（`report_effects.rs:476`）里，遇到 402/403 时给 billing 域打 `stale: true`，管理页读到就提示"额度数据可能过期，请刷新"。真正的背景 TTL 刷新是独立的基础设施改动，不在本轮范围。

## 系统化重构：billing 投影成通用配额窗口（2026-07-29）

首版把 billing 做成了 `windows[]` 的**兄弟对象**，结果每一层都要写 grok 专用分支。改为**投影**：`billing` 保留富记录（plan / cents / on-demand / partial 这些塞不进窗口形状的字段），同时派生出两个标准窗口，让通用机制接管。

```
billing_weekly   unit=percent  used_ratio  is_exhausted  reset_at=period_end_unix
billing_monthly  unit=usd      limit/used/remaining_value(美元)  used_ratio  reset_at=billing_period_end_unix
```

投影在 `merge_grok_quota_snapshot` 末尾**重建**（先 `retain` 掉旧的 `billing_*` 再 extend），所以两种表示不可能漂移。

**因此删除的 grok 专用代码**：
- `grok_billing_exhausted_reset_at` —— 耗尽判定回到通用的 `provider_pool_quota_window_is_exhausted`（它优先读显式 `is_exhausted`，所以 overflow budget 的判断仍然生效，只是搬进了投影里）
- `grok_billing_usage_ratio` —— `recompute_grok_quota_aggregate` 的 `usage_ratio` 回到「所有窗口 used_ratio 取 max」
- 前端 `getGrokBillingText` 从手写 billing 字段改为 `getQuotaWindow(quota, 'billing_weekly'|'billing_monthly')` + 现有窗口 helper

**配套收紧**：`normalize_merged_grok_quota_window` 原本对任意窗口用 limit/remaining 反推 `used_ratio`/`is_exhausted`，会覆盖投影窗口的判断。现收敛到只处理 header 观测的 `requests`/`tokens` 两个维度——投影窗口本来就是派生结果，不该再被反推。

**通用改进**：`getQuotaWindowValueText` 变为单位感知，`unit === 'usd'` 时输出 `$45.94/$150.00`（共享的一位小数取整会丢分）。全仓目前只有 `billing_monthly` 用 usd，无外溢。

落到调度上：`ProviderQuotaSnapshotPolicy::GrokDeadlineBounded`（`quota.rs:282`）要求耗尽必须由未来的 reset 背书，投影窗口自带 `reset_at`，周期一过自动恢复由这层免费提供。

> **注意**：grok 池当前 `pool_advanced.skip_exhausted_accounts = false`，而 `aether-pool-core/src/scheduler.rs:228` 是 `skip_exhausted_accounts && quota_exhausted`。所以 `exhausted: true` 目前只改变额度视图，**不门控选号**。要真门控需要打开该开关。

## 窗口用量管线已泛化（2026-07-29）

原先按 `provider_type == "codex"` 早退的那条管线改成了按 provider 分派：

| 原名 | 现名 | 变化 |
|---|---|---|
| `AdminPoolCodexCycleUsageByKey` | `AdminPoolWindowUsageByKey` | 仅改名 |
| `read_admin_pool_codex_cycle_usage_by_key` | `read_admin_pool_window_usage_by_key` | 按 provider 分派请求构造 |
| `admin_pool_apply_codex_window_usage_summaries` | `admin_pool_apply_window_usage_summaries` | 函数体本就通用，仅改名 |
| `codex_cycle_usage_by_code`（参数） | `window_usage_by_code` | 仅改名 |

新增 `admin_pool_grok_billing_usage_request(s)`，并抽出共用的 `admin_pool_window_usage_requests`（原先 codex 那段遍历 `quota.windows` 的样板现在两边共用）。

**codex 与 grok 的口径差异**：codex 用 `reset_at - window_minutes` 反推窗口起点；grok 的计费窗口自带边界，所以投影窗口多带一个 `window_start_at`（来自 `billing.period_start_unix` / `billing_period_start_unix`），直接用真实周期起点。周期已关闭时 `end_unix_secs` 取 `min(now, reset_at)`——否则会把下一周期的流量混进来。

于是 grok 的 `billing_weekly` / `billing_monthly` 窗口现在带上了**与上游同周期**的本地结算用量，跟 codex 的周期统计走同一条路径。终身累计那套 `admin_pool_attach_grok_local_usage_observation` 只在没有权威 billing 时才兜底。

## 仍未泛化的一处 codex 专属基建

**软阈值**：`provider_pool_key_codex_quota_soft_threshold_exceeded`（`quota.rs:48`）硬编码 `provider_type == "codex"`。`cost_soft_threshold_percent` 本来就是每 provider 各自的 `pool_advanced` 配置，去掉这道门等于「各 provider 用自己配的阈值」，未配置的不受影响。但实现体 `codex_quota_soft_threshold_from_status_snapshot` 内含 codex 特有的「primary quota window」概念和 codex metadata 回退，真正泛化要动 codex.rs 内部——**codex 是最高流量的池，这一步应当单独提交、单独评审**，不与 grok 改动混在一起。

## 复用（不要重造）

| 需求 | 已有实现 |
|---|---|
| GET 型配额探测 | `providers/codex.rs:123`、`providers/kiro.rs:105` |
| 探测执行 + 代理 + 超时 | `execute_grok_quota_plan`（`quota/grok.rs:21`） |
| 快照落库 | `persist_provider_quota_refresh_state`（`quota/shared.rs:282`） |
| status_snapshot 派生 | `sync_provider_key_quota_status_snapshot`（`catalog.rs:1812`） |
| 窗口用量统计 | `summarize_usage_by_provider_api_key_windows` |
| Grok CLI 身份头 | `crates/aether-provider-transport/src/grok.rs:17-21` |

## P0 实测结果（2026-07-29）

两个号 × 两个窗口，全部 HTTP 200。

| | `09016042425a`（撞过 402） | `shirtiny`（正常） |
|---|---|---|
| 周 `creditUsagePercent` | **100.0** | 5.0 |
| `productUsage[0]` | `GrokBuild` 100.0% | `GrokBuild` 5.0% |
| `currentPeriod` | 07-25T12:23:24Z → **08-01T12:23:24Z** | 07-26 → 08-02 |
| 月 `monthlyLimit` / `used` | 15000 / 10406（69.4%） | 150000 / 4297（2.9%） |
| 推出的 plan | SuperGrok | SuperGrok Heavy |

**结论：**

1. **`/billing` 与 `/billing?format=credits` 在 `cli-chat-proxy.grok.com/v1` 上都存在**，用 aether 自己的身份头（`xai-grok-workspace/0.2.103`）经 warp 出网即可，无需另 pin host。
2. **402 来自「周 credit 窗口打满」，不是月度余额耗尽。** 出事的号月度才用了 69%，但周窗口 100%。所以 `Grok Build usage balance exhausted` 对应的是 `creditUsagePercent == 100`。
3. **周窗口自带 `currentPeriod.end`**——这正是 header 路径缺失的 reset 截止时间，`grok.rs:159-163` 那个「exhausted 无 reset 会永久封号」的两难在 billing 侧不存在。
4. 解析器**未经修改**即正确吃下真实载荷（19/19 单测通过，夹具已换成实测原文）。照 sub2api 建的字段模型是准确的。

**实测暴露的、sub2api 模型里没有的字段**（当前未解析，记录备查）：
`onDemandCap` / `onDemandUsed` / `prepaidBalance` / `topUpMethod` / `isUnifiedBillingUser` / `history[]`。
其中 **`onDemandCap` 对耗尽判定有意义**：出事的号是 `creditUsagePercent=100` **且** `onDemandCap=0` **且** `prepaidBalance=0`——没有超额预算兜底，才成为硬停。若某号 `onDemandCap > 0`，周窗口 100% 未必等于不可用。任何 billing 驱动的 `exhausted` 判据都应把这三个字段一起纳入。

**仍未观测：Free 档的 billing 形状。** 当前号池里两个号都是付费档，拿不到样本。`grok_billing_has_authoritative_quota` 的四项判据对这两个付费号都成立；Free 回落 header 路径的分支尚未被真实数据验证过，依据仍只是 sub2api 的注释（`openai_gateway_grok_cache.go:318-324`）。

## 验证

**单测**（`crates/aether-provider-pool/src/providers/grok.rs` 的 `mod tests`，现有 :540 起）：
- `parse_cent_value` 三形态：`{"val":15000}` / `15000` / `"15000"`
- `resolve_plan` 边界：15000 / 150000 / 其他 / 浮点噪声
- 周月独立合并：月成功周失败时周域保留旧值，`partial: true` + `failed_windows: ["weekly"]`
- 权威判定四项判据各自单独成立
- `merge_grok_quota_snapshot` 在只有 header 观测进来时不擦除 `billing` 域 ← **回归重点**

**实测**：两个真号跑 P0 只读探测，比对 402 号与正常号的 billing 差异。

**注意**：`custom` 分支套件本来就不全绿（既有失败），建基线时不要用 `git stash`，且机器只有 7GB 内存，`cargo` 不要开并发。
