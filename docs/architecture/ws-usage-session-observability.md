# WebSocket Usage 会话身份与提示词摘要

本文档说明 `backend-v0.7.49` 中针对 Codex WebSocket 使用记录完成的第一阶段修复：

- WS 请求在使用记录列表和详情中稳定显示会话 ID；
- 页面显示的会话 ID、会话筛选和计数使用相同的数据优先级；
- 没有独立提示词捕获的 WS step 可以从同一会话继承提示词摘要；
- 会话级继承受用户/API Key、客户端族和时间边界约束；
- 常见同连接详情查询继续使用快速路径，不把所有请求退化为全表会话查询。

本文只描述第一阶段。该阶段没有新增数据库 schema、索引、migration 或历史 backfill。

相关文档：

- [Codex 官方 WebSocket](../codex-websocket.md)
- [Session Debug Runbook](../operations/session-debug.md)

## 1. 问题背景

普通 HTTP 请求通常在当前 usage 记录上直接携带请求提示词捕获信息。Codex WS 则有两个
额外特征：

1. 一个 WS 连接可以连续承载多个 `response.create` step；
2. 后续 step 不一定重复发送最初的 system/developer prompt，因此当前 usage 记录可能只有
   `ws_step=true`，没有自己的 `prompt_capture.items`。

旧实现还存在以下不一致：

- 会话身份可能只写在 `request_candidates.extra_data.client_session_affinity`，usage 行本身
  没有会话 ID；
- 列表读取时可以从 candidate 恢复会话信息，但会话筛选只检查 usage 元数据，造成
  “列表能显示、筛选却找不到”；
- 只按 candidate 查提示词摘要无法覆盖 WS 重连或换 candidate 后的同一逻辑会话；
- 只按 session key 扩大查找范围又可能跨用户、API Key 或客户端族串用摘要；
- 无条件执行宽范围会话查询会增加请求详情延迟。

## 2. 规范化会话身份

### 2.1 新 WS step 的持久化

Codex WS runtime 在构造 step usage context 时保留已有的
`client_session_affinity`。如果上游 context 没有有效的 `session_key`，则使用官方请求身份：

1. 优先使用非空 `official_identity.session_id`；
2. `session_id` 为空时使用 `official_identity.thread_id`；
3. 写入：

```json
{
  "client_session_affinity": {
    "client_family": "codex",
    "session_key": "session=<session-or-thread-id>"
  }
}
```

若 planner 已提供更完整的 key，例如
`account=<account>;session=<session>`，runtime 不覆盖它。usage reporter 的紧凑化过程会保留
整个 `client_session_affinity`，因此新 WS usage 行在 candidate 清理后仍可保留逻辑会话身份。

### 2.2 历史记录的读取兼容

历史 WS usage 可能没有持久化会话身份。Postgres 读取路径通过
`usage_routing_snapshots.candidate_id` 关联 `request_candidates`，按以下非空优先级计算有效值：

```text
effective_session_key =
  usage.request_metadata.client_session_affinity.session_key
  -> request_candidates.extra_data.client_session_affinity.session_key

effective_client_family =
  usage.request_metadata.client_session_affinity.client_family
  -> usage.request_metadata.client_family
  -> request_candidates.extra_data.client_session_affinity.client_family
```

箭头表示“左侧为空或仅包含空白时才使用右侧”。candidate 数据只用于补齐缺失字段，不覆盖
usage 已保存的非空值。

该兼容发生在读取阶段，不会反向修改历史 usage 行。

## 3. 列表、筛选和计数一致性

使用记录列表原本已经关联 routing snapshot 和 candidate，以展示路由信息。会话 ID 和
客户端族筛选现在使用与列表展示完全一致的非空优先级。

会话筛选仍兼容以下旧字段：

- `request_metadata.client_session_affinity.session_key`；
- `request_candidates.extra_data.client_session_affinity.session_key`；
- `request_metadata.session_id`；
- `request_metadata.conversation_id`。

计数查询只在存在非空 `session_id` 或 `client_family` 筛选时增加 candidate 关联。普通列表
计数不增加该联表，因此默认使用记录页面不受影响。

`client_family` 同样使用 `NULLIF(BTRIM(...), '')` 跳过空字符串，避免空 usage 字段阻止
candidate 回退。

## 4. WS 提示词摘要继承

详情读取仅在同时满足以下条件时尝试继承：

- 当前记录是 `ws_step=true`；
- 当前记录没有非空 `prompt_capture.items`；
- 当前记录存在 routing candidate ID。

查询按以下层级执行。

### 4.1 第一层：同连接快速路径

首先通过当前 `candidate_id` 查询更早或同时刻的 WS prompt capture。该路径同时校验：

- 来源 usage 与当前 usage 的用户身份一致；
- 当前记录没有用户 ID 时，API Key 身份也必须一致；
- 来源 `created_at <= current.created_at`；
- 来源是 WS step 且存在非空 prompt items。

同一个 candidate 表示同一条已绑定 WS 连接，因此该路径不执行跨 candidate 会话扫描。
这是正常多 step WS 详情的主要路径。

### 4.2 第二层：加载当前会话范围

快速路径未命中后，单独读取当前记录的：

- `user_id`；
- `api_key_id`；
- `created_at`；
- usage 自身持久化的 `usage_session_key`；
- usage/candidate 合并后的有效 `session_key`；
- usage/candidate 合并后的 `client_family`。

如果没有有效 session key，停止继承。如果用户 ID 和 API Key ID 都不存在，也停止跨
candidate 查找。

### 4.3 第三层：受约束的 candidate 会话回退

跨 candidate 查询从 `request_candidates` 的 session key 开始，关联 routing snapshot 和
source usage。来源必须满足：

- session key 与当前有效 session key 精确相等；
- 当前有用户 ID 时，source usage 的用户 ID 必须精确相等；
- 当前没有用户 ID 时，source usage 必须同样没有用户 ID，且 API Key ID 精确相等；
- source candidate 的身份必须兼容当前身份；为兼容旧数据，candidate 身份允许为空，但
  source usage 身份仍必须精确匹配；
- 当前 `client_family` 已知时，来源 family 必须相等；旧记录没有 family 时允许兼容；
- `source.created_at <= current.created_at`；
- 来源是 WS step 且 prompt items 非空。

结果按 `source.created_at`、`source.updated_at_unix_secs` 倒序，只取最近一条。

### 4.4 第四层：usage 元数据保底

如果 candidate 会话回退未命中，并且当前 usage 行自身已经持久化
`client_session_affinity.session_key`，再直接从 source usage 元数据执行保底查询。

这条路径用于处理 candidate 已被清理、但新 usage 仍保留会话身份的情况。它继续应用：

- 用户/API Key 身份约束；
- 客户端族约束；
- 时间边界；
- WS step 和非空 prompt items 约束。

只有当前 usage 自身持久化了 session key 才启用该层。candidate 临时恢复出的 session key
不会无条件触发 usage 宽范围查询，以控制旧详情读取的最坏延迟。

## 5. 详情响应和前端展示

继承成功后，详情读取不会修改数据库，只在返回的 `request_metadata.prompt_capture` 中增加：

```json
{
  "scope": "ws_session",
  "inherited": true,
  "source_request_id": "<source-request-id>"
}
```

原有 `version`、`item_count`、`role_counts` 和 `items` 保持不变。版本 2 的 hash 引用仍通过
`usage_prompt_capture_entries` 补齐 preview、字符数、首次/最近出现时间和出现次数。

使用记录详情页会显示：

- 提示词条目数；
- `会话继承` 标记；
- 截断显示的来源请求 ID；
- 原有角色计数和逐条摘要。

## 6. 隔离和数据安全边界

会话 key 不是唯一的授权边界。跨 candidate 继承必须同时满足身份和客户端族条件。

- 有用户 ID：以用户 ID 作为主要租户边界；
- 无用户 ID：要求 source/current 都无用户 ID，并使用 API Key ID 精确隔离；
- 用户 ID 和 API Key ID 都没有：禁止跨 candidate 继承；
- 已知客户端族不跨 family 匹配；
- 不读取当前请求时间之后的摘要；
- 同连接快速路径仍保留身份校验，避免异常 routing 数据扩大范围。

摘要只包含已脱敏/截断的 prompt capture 元数据及 hash entry 补全结果，不新增原始请求 body
读取路径。

## 7. 性能设计

查询顺序刻意把最常见、范围最小的路径放在前面：

1. candidate ID 快速路径；
2. 当前请求 scope 单行读取；
3. candidate 会话回退；
4. 有持久化 usage session key 时才执行 usage 保底。

普通使用记录列表本来就需要 routing/candidate 数据，不新增列表联表。普通计数在没有会话或
客户端族筛选时仍只读取 usage 表。

显式筛选只存在于 candidate 的历史 session key 时，计数查询需要关联 routing snapshot 和
candidate。这是第一阶段为保证“显示值可筛选”接受的局部成本。通用 session 表达式索引属于
第二阶段，不在本次范围内。

## 8. 异常元数据兼容

历史 `prompt_capture.items` 可能不是数组。SQL 使用带类型分支的 `CASE`：只有
`json_typeof(items) = 'array'` 时才调用 `json_array_length`，其他类型按无有效摘要处理，避免
单条异常 JSON 使整个详情请求失败。

空字符串和仅空白字符串在 session key/client family 合并时均视为缺失值。

## 9. 第一阶段限制

本阶段没有执行：

- 新增 session/filter 索引；
- 历史 usage 会话身份 backfill；
- candidate 会话字段迁移到独立规范化列；
- candidate 清理前的批量物化；
- 数据库 schema 变更。

因此存在以下已知边界：

- 历史 usage 没有 session key，且关联 candidate 已清理时，无法恢复会话 ID；
- 来源和当前记录都只依赖已清理 candidate 时，无法执行会话级摘要继承；
- 显式历史 session 筛选可能比普通列表查询慢；
- 第二阶段如果实施，需要单独评估索引构建、backfill、保留策略和数据库负载。

## 10. 验证

`backend-v0.7.49` 变更完成时执行：

```text
cargo test -p aether-data repository::usage::postgres
  95 passed / 0 failed

cargo test -p aether-gateway codex_ws
  113 passed / 0 failed

cargo check -p aether-gateway --lib
  passed
```

另外使用只读事务验证了一个真实 WS 样例：candidate 可恢复有效 session key，精确会话
筛选能够命中，并能定位到包含 prompt items 的来源 usage。验证事务已回滚，没有数据库写入。

## 11. 主要代码位置

| 范围 | 文件 |
|---|---|
| WS step 会话身份构造 | `apps/aether-gateway/src/codex_ws/runtime.rs` |
| usage context 紧凑化与持久化 | `apps/aether-gateway/src/codex_ws/usage_reporter.rs` |
| Postgres 会话筛选、详情继承和读取兼容 | `crates/aether-data/src/repository/usage/postgres/mod.rs` |
| 使用记录 SQL projection | `crates/aether-data/src/repository/usage/postgres/queries/*.sql` |
| Postgres 回归测试 | `crates/aether-data/src/repository/usage/postgres/tests.rs` |
| 详情页继承标记 | `frontend/src/features/usage/components/RequestDetailDrawer.vue` |
| prompt capture 元数据解析 | `frontend/src/features/usage/utils/promptCapture.ts` |

## 12. 版本记录

| commit | 说明 |
|---|---|
| `17b2cddc` | WS 请求详情按会话继承提示词摘要 |
| `fce92a0a` | 保留 WS 会话身份，并为历史记录读取 candidate affinity |
| `0f0cfb2b` | 拆分同连接快速路径和会话回退路径 |
| `4068af1c` | 统一会话展示/筛选并收紧摘要匹配范围 |

发布 tag：`backend-v0.7.49`。
