# System Config 与 Prompt Capture 性能优化设计

- 状态：已实现，验证记录见文末
- 日期：2026-08-19
- 范围：Aether Gateway、usage runtime、PostgreSQL usage repository
- 交付方式：代码、测试和必要的观测项一次合入、一次发布；不分段发布，不做灰度，不保留新旧双路径

## 结论

本设计解决两个相互关联的性能问题：

1. `GatewayDataState` 的系统配置读取绕过了 `AppState` 已有的 30 秒本地缓存，usage 生命周期又重复解析同一份捕获策略，造成 `system_configs` 和用户组关系表的读放大。
2. Prompt Capture 在终态事件中深拷贝大 JSON、为每段文本构造完整规范化字符串，并对重复 prompt 持续执行 PostgreSQL 冲突更新，造成 CPU、内存分配、WAL 和 dead tuple 放大。

采用以下直接方案：

- 把现有 `SystemConfigCache` 下沉到 `GatewayDataState`，所有配置读取共用一份缓存；`system_config_values` 继续只作为测试替身。
- HTTP execution attempt 只解析一次 `UsageBodyCapturePolicy`，并通过现有 lifecycle/terminal seed 传给 pending、stream started、stream buffer 和 terminal 路径。
- Prompt Capture 按当前语义逆序提取“最后 N 个唯一 prompt”，规范化、SHA-256、字符计数和 preview 在一次流式遍历中完成，不再保留完整规范化文本。
- Basic 模式在构造终态 usage seed 前生成 Prompt Capture 摘要，不再为了随后丢弃的 body 深拷贝几十 MiB JSON。
- PostgreSQL 事务内只保证 prompt 字典项存在并按需改善元数据；`seen_count` 和 `last_seen_at` 由进程内 5 秒聚合器批量累加。

不新增分布式缓存、Redis key、消息队列、outbox、数据表或功能开关。

## 现状与证据

一次生产高峰的只读采样显示：

| 指标 | 观测值 |
|---|---:|
| 主机 CPU | 4 核，峰值 idle 约 14% 到 33% |
| 8 分钟进入网关的 request body | 约 1.73 GiB / 1,424 次 |
| `/v1/responses` 占比 | 约 95% |
| 单次最大 request body | 约 38.7 MiB |
| `system_configs` 10 秒 seq scan | 1,112 次 |
| Prompt Capture 字典当前行数 | 约 185,000 |
| Prompt Capture 10 秒变化 | 1 次插入、287 次更新 |

流量是正常的长上下文请求，不是异常请求或重试风暴。问题在于服务端对合法大请求做了与 body 大小和历史上下文重复度不相称的工作。

需要区分两类重复：

- Prompt 文本的大 JSON 扫描只发生在 terminal body capture，不是 pending、stream started、terminal 各扫描一次。
- 捕获策略读取会在多个生命周期节点重复发生；流式响应缓冲还会额外读取一次策略。

## 目标

- 缓存命中时，系统配置读取不访问数据库。
- 同一 execution attempt 的捕获策略和用户组范围只解析一次。
- Prompt Capture 的额外持有内存由 `max_items` 和 `preview_chars` 决定，不再与整个 request body 大小成正比。
- 保持当前 Prompt Capture 的选择、去重、顺序、role、hash、preview 和 request/provider 优先级语义。
- 显著减少 `usage_prompt_capture_entries` 的重复 UPDATE、WAL、索引维护和 dead tuple。
- 不改变请求转发、路由、计费和结算结果。

## 非目标

- 不把 `system_configs` 做成 Redis 或跨节点一致性系统。
- 不把 Prompt Capture 改造成独立服务、消息管道或持久化 outbox。
- 不修改 Prompt Capture 的产品开关、用户组范围或 UI 字段。
- 不新增扫描预算或采样率；这些会改变当前捕获结果，不属于本次等价优化。
- 不针对这次修改增加 feature flag、shadow write、dual read 或兼容期。
- 不包含生产部署、容器重启或数据库运维操作。

## 问题一：系统配置读取放大

### 根因

`GatewayDataState::from_config` 将 `system_config_values` 初始化为 `None` 是正确行为。该字段是 `with_system_config_values_for_tests` 使用的内存存储替身，不是生产缓存。它一旦为 `Some`，配置的读取、写入、删除和 purge 都会在内存分支提前返回，绕过 PostgreSQL，重启后数据也会丢失。因此不能通过把 `None` 改成 `Some` 来优化生产。

真正的缓存已经存在于 `AppState.system_config_cache`：

- TTL 为 30 秒；
- 同进程写入后会刷新对应缓存项；
- 删除写入 negative cache；
- purge 会清空缓存。

但 `UsageRuntimeAccess` 实现在 `GatewayDataState` 上，`body_capture_policy_for_user` 直接调用 `GatewayDataState::find_system_config_values`，因此绕过 `AppState` 缓存。现有批量读取已经把 5 个 key 合成一次 SQL，但这次 SQL 仍会在每次策略解析时执行。

当 scope 为 `include_groups` 或 `exclude_groups` 时，每次解析还会查询一次用户组关系。pending、stream started、terminal 和流式响应缓冲分别解析策略，进一步放大查询量。

### 设计

#### 1. 下沉并复用现有缓存

将现有 `SystemConfigCache` 的所有权从 `AppState` 移到 `GatewayDataState`，保持同一套 `ExpiringMap<String, Option<Value>>`、30 秒 TTL 和 512 项上限。`AppState::read_system_config_json_value` 改为委托给 data state，不再持有第二份缓存。

读取规则：

1. `system_config_values` 为 `Some` 时仍走测试替身，行为不变。
2. `find_system_config_value` 和 `find_system_config_values` 先查同一份本地缓存。
3. 批量读取只把 miss 或过期的 key 交给 backend，并保持一条批量 SQL。
4. 查不到的 key 也写入 negative cache，避免默认值 key 每次落库。
5. backend 错误不写缓存，沿用当前错误处理。

缓存增加一个简单的异步 reload/mutation mutex。冷缓存并发请求在加锁后重新检查缓存，仅第一个请求访问 backend，避免每 30 秒形成一次并发穿透。配置写入也经过同一把锁，防止一个较早开始的 reload 在写入完成后把旧值重新放回缓存。不增加通用 single-flight 框架，缓存命中读取也不获取该异步锁。

写入规则：

1. 先完成 PostgreSQL upsert/delete/purge。
2. 数据库成功后再更新或清空本地缓存。
3. 数据库失败时不修改缓存。
4. 进程外直接改库的可见延迟继续由既有 30 秒 TTL 约束。

#### 2. 捕获策略按 attempt 解析一次

不新增独立的 capture context 类型。`LifecycleUsageSeed` 和
`TerminalUsageContextSeed` 增加可选的 `UsageBodyCapturePolicy`，沿用现有 attempt
seed/guard 的所有权和取消语义，避免再引入一层生命周期抽象。

在 authenticated user 和最终 provider request body 都已确定后、记录 pending 前解析策略。一次解析包含：

- 从系统配置缓存读取 5 个 body capture key；
- 解析 `request_capture_policy`；
- scope 需要时查询一次该用户的 group membership；
- 返回本 attempt 后续生命周期共用的 policy。

policy 随现有 execution attempt 的 seed/guard 传递：

| 路径 | 使用方式 |
|---|---|
| pending | 只应用已解析 policy，不再查询配置或用户组 |
| stream started | 只应用已解析 policy |
| stream response buffer | 直接使用 policy 的 record level 和 response limit |
| terminal | 使用相同 policy；从借用的 request/provider JSON 生成一次有界 prompt metadata |

Prompt 扫描原本只发生在 terminal，不需要在 pending 阶段提前扫描。每个 provider
retry 可以产生新的 attempt 和 provider body，因此按 attempt 解析；用户请求级的重试语义不变。该边界避免建立跨候选、跨请求的用户组缓存。

## 问题二：Prompt Capture CPU 与写放大

### 必须保持的现有语义

优化后的提取器必须与当前实现输出等价：

- 文本规范化为 `split_whitespace()` 后用单个 ASCII 空格连接；
- 相同规范化文本去重，后出现的 occurrence 覆盖先出现的 occurrence；
- 最终保留最后 `max_items` 个唯一文本，并按各自最后出现位置排序；
- request prompts 优先，只有不足 `max_items` 时才由 provider request 补足；
- role 开关、source path、message index、SHA-256、字符数、preview 和 truncated 含义不变；
- metadata 继续使用当前 schema，repository 仍将内联 item 转为 version 2 引用。

### 根因

当前 terminal 路径存在三类额外成本：

1. `build_terminal_usage_context_seed` 对 original/provider body 执行 `Value::clone` 和递归脱敏；Basic 模式随后只提取 prompt metadata，再把 body 清空。
2. 每个 prompt 都通过 `split_whitespace().collect::<Vec<_>>().join(" ")` 构造完整规范化字符串，之后又分别扫描它来计算 hash、字符数、preview 和 truncated。
3. `usage_prompt_capture_entries` 对每次重复观察执行 `ON CONFLICT DO UPDATE`，即使 role、chars、preview 和 truncated 都没有改善，也会更新 `last_seen_at`、`seen_count` 和 `last_seen_at` 索引。

### 设计

#### 1. 逆序、有界、流式提取

内部结果改为只保存最终需要的数据。实现使用私有的 `CapturedPrompt`，其字段与下述结构等价，不新增公共 API：

```rust
struct CapturedPrompt {
    source: String,
    index: Option<usize>,
    role: PromptCaptureRole,
    sha256: [u8; 32],
    chars: usize,
    preview: String,
    truncated: bool,
}

// items 和 role_counts 随即编码进现有 request_metadata JSON。
```

提取顺序改为当前遍历顺序的严格逆序：

1. 从最后一个候选 prompt 向前遍历。
2. 对规范化文本计算 digest；digest 第一次出现代表该文本的最后一次 occurrence。
3. 收集到 `max_items` 个唯一 digest 后立即停止。
4. 将结果 reverse，恢复当前输出顺序。
5. request 先完成上述过程；provider 仅按剩余容量补足，并排除 request 已选 digest。

该算法与“正序遍历、重复项移到末尾、超限删除最早项”严格等价，但常见长上下文只需检查 body 尾部的有限数量 prompt。

文本处理不再创建完整 normalized `String`。对 `split_whitespace()` 返回的 token 进行一次遍历：

- token 之间向 SHA-256 输入一个 ASCII 空格；
- 同时累计 Unicode char 数；
- 只在尚未达到 `preview_chars` 时向 preview 追加字符；
- 最终以 `chars > preview_chars` 计算 truncated；
- digest 在内部保持 32 字节，生成 metadata 时才编码一次 hex。

用于去重的是 digest。SHA-256 碰撞不作为业务可达情况处理，这与数据库以 SHA-256 为主键的既有模型一致。

#### 2. Basic 模式不再复制完整 body

terminal seed builder 在借用现有 request/provider `Value` 时生成 metadata：

- `record_level == Basic`：terminal seed 中不再拥有 request/provider body，只携带 body state 和有界 `prompt_capture` metadata；
- `record_level == Full`：继续执行现有脱敏、大小限制和 body 持久化逻辑；Prompt Capture 仍复用同一摘要，不再二次扫描；
- prompt capture 未启用：不扫描 prompt，也不为此复制 body。

`UsageBodyCaptureEngine` 检测到 seed 已包含 `prompt_capture` 时直接复用，不再扫描
terminal body。HTTP sync/stream/取消热路径都显式传入已解析 policy；兼容性的直接
usage event 入口在没有预计算 metadata 时仍沿用同一个新提取器，不保留第二套生产提取实现。

#### 3. 字典元数据与观察计数分开写

usage 事务仍必须保证 version 2 引用对应的字典行已经存在，避免刚写入的 usage 无法 hydrate。事务内 SQL 调整为一个 writable CTE：

- 新 digest：插入 role、chars、preview、truncated、`first_seen_at`、`last_seen_at` 和本次 `seen_count`，并返回 inserted digest 集合；
- 已存在 digest：只有 chars、preview 或 truncated 按当前合并规则确实会变化时才产生新 tuple；每条 usage metadata ref 已保存自己的 role，因此不再反复改写不参与 hydrate 的字典 role；
- 普通重复 digest：不更新 `last_seen_at` 和 `seen_count`，因此不产生无意义 tuple、WAL 和索引更新。

事务提交成功后，只把本次未出现在 inserted 集合中的 digest occurrence 数累加到进程内 map。这样新行的初始 `seen_count` 与后续聚合不会重复计数。map 的刷新窗口为 5 秒：usage 提交后在窗口到期时机会刷新，已有 usage counter worker 的 1 秒循环也调用同一刷新入口；正常 serve 退出时再强制 drain 一次。不增加新的常驻任务。批量 SQL 为：

```sql
UPDATE usage_prompt_capture_entries AS stored
SET seen_count = stored.seen_count + incoming.delta,
    last_seen_at = GREATEST(stored.last_seen_at, incoming.last_seen_at)
FROM UNNEST($1::text[], $2::bigint[], $3::timestamptz[])
    AS incoming(sha256, delta, last_seen_at)
WHERE stored.sha256 = incoming.sha256;
```

规则如下：

- 多实例分别累加 delta，SQL 加法保证不会互相覆盖；
- flush 失败时把 drain 出的 delta 合并回 map，等待下一次 flush；
- 正常 serve 退出时执行一次最终 flush；
- 进程崩溃最多丢失一个 5 秒窗口的 `seen_count`/`last_seen_at` 更新；
- version 2 引用和字典正文已在 usage 事务内持久化，不受统计 flush 影响。

`seen_count` 和 `last_seen_at` 只用于管理界面展示，不参与请求处理、路由、计费或结算，因此允许上述最多 5 秒的最终一致性和极小的崩溃窗口。这里不使用 durable outbox，避免为非关键展示统计引入新的表、清理任务和积压治理。

## 代码改动边界

| 文件/模块 | 改动 |
|---|---|
| `apps/aether-gateway/src/cache/system_config.rs` | 保留现有缓存实现，增加批量命中和简单 reload mutex |
| `apps/aether-gateway/src/data/state/{mod,core}.rs` | 持有共享配置缓存；统一读、写、删、purge 的缓存规则 |
| `apps/aether-gateway/src/state/{app,core}.rs` | 移除重复缓存所有权，改为委托 data state |
| `apps/aether-gateway/src/data/state/integrations.rs` | 从共享缓存解析 policy；scope 每 attempt 只查询一次 |
| `crates/aether-usage-runtime/src/{runtime,write}.rs` | 在现有 seed 中传递 policy，Basic seed 不再复制 body |
| `crates/aether-usage-runtime/src/body_capture.rs` | 逆序流式提取器和 summary metadata 编码 |
| `crates/aether-data/src/repository/usage/postgres/mod.rs` | 条件字典 upsert、批量统计 flush |

不修改数据库 schema。MySQL 和 SQLite 保持现有字典写法；CPU/内存提取优化和 request-scoped policy 对所有 backend 生效。生产观测到的写放大位于 PostgreSQL，本次不为未出现的其他 backend 问题增加后台聚合实现。

## 正确性与测试

### 系统配置

- 单 key、多 key、缺失 key 的缓存命中测试。
- 200 个并发冷读只触发一次 backend batch read。
- upsert 成功后立即读到新值；upsert 失败不污染缓存。
- delete 使用 negative cache；config purge 清空缓存。
- 30 秒过期后重新读取 backend。
- `with_system_config_values_for_tests` 的读写删除行为保持不变。
- include/exclude group scope 在同一 attempt 内只查询一次 membership。

### Prompt Capture

- 保留现有 golden tests，并用旧实现作为 test-only reference，对新旧提取器做差分测试。
- 覆盖 OpenAI Responses、Chat Completions、Claude Messages、Gemini Contents 的嵌套形态。
- 覆盖 Unicode whitespace、空文本、重复文本、相同文本不同 role、tool/developer 开关、provider 补足和 `max_items` 边界。
- 随机生成嵌套 prompt JSON 和 policy，断言 items、顺序、source、index、hash、chars、preview、truncated、role_counts 完全一致。
- 断言 Basic terminal seed 不包含 request/provider body，Full 模式仍按原规则脱敏和持久化。

### PostgreSQL

- 新 digest 在 usage 事务提交后可立即 hydrate。
- 重复 digest 且元数据无改善时不产生字典 UPDATE。
- 更长 preview 等元数据改善仍能更新。
- 并发 flush 的 `seen_count` 采用加法且不丢更新。
- flush 失败后 delta 回填并能在下一次成功 flush。
- 同一 request 的重复 terminal upsert 不重复计算 occurrence；沿用 request revision/idempotency 判定。

### 性能基准

增加 1 MiB、10 MiB、40 MiB 三组长上下文 benchmark，并包含“最近 32 条唯一”和“全部为同一重复文本”两种数据：

- 提取器 CPU 时间和输出等价性；
- Basic terminal seed 是否持有 request/provider body；
- PostgreSQL 重复摘要是否产生 heap tuple 更新；
- PostgreSQL 聚合后的 `seen_count` 是否准确。

本次不引入自定义 allocator 或数据库观测框架。额外持有内存由结构和测试直接约束；
WAL bytes、dead tuple 及压力下的更新速率使用既有 PostgreSQL 指标在发布后观测。

## 验收标准

- 缓存预热后，每个请求的系统配置 SQL 为 0；并发过期只产生 1 条 batch read。
- 每个 execution attempt 的用户组 membership 查询不超过 1 次。
- `system_config_values` 在生产构造中仍为 `None`，所有生产配置写入仍先落数据库。
- 新旧 Prompt Capture 差分测试 100% 等价。
- Basic 模式不再为 usage capture 深拷贝 original/provider body。
- Prompt Capture 自身持有内存随 `max_items * preview_chars` 增长，不随 1/10/40 MiB body 线性增长。
- 40 MiB、最近 32 条唯一的 benchmark 吞吐至少达到当前实现的 3 倍。
- 重复上下文场景下，`usage_prompt_capture_entries` UPDATE 数相对当前实现下降至少 90%。
- 请求成功率、转发内容、计费、结算、usage body capture 和管理端 Prompt Capture 展示无回归。

## 一次性交付要求

上述代码和测试作为一个变更集交付。合入前必须同时通过：

```sh
cargo test -p aether-usage-runtime
cargo test -p aether-data repository::usage::postgres
cargo test -p aether-gateway data::state
```

并完成本地 40 MiB benchmark。具备 `AETHER_TEST_POSTGRES_URL` 时执行真实 PostgreSQL
边界、重复写和并发批次测试。测试通过后按正常完整版本发布；不设置灰度用户、不按
group 分批启用、不保留 feature flag，也不先上线 schema 或后台任务。

## 实现与本地验证记录

实现保持以下边界：

- 生产构造中的 `system_config_values` 仍为 `None`；共享缓存只位于
  `GatewayDataState`，写、删、purge 均在 backend 成功后更新缓存。
- HTTP sync、stream、prefetch/midstream failure 和取消 guard 使用同一次 policy
  解析结果；Basic 流式响应缓冲上限为 0。
- 未新增 schema、feature flag、Redis、队列、outbox、服务或独立 worker。
- 未执行任何生产部署、重启或数据库写操作。

本地 benchmark（test-only 旧实现作为基准）结果：

| body | 最近 32 条唯一 | 全部重复 |
|---|---:|---:|
| 1 MiB | 0.78x | 13.01x |
| 10 MiB | 5.43x | 83.25x |
| 40 MiB | 15.61x | 133.89x |

40 MiB“最近 32 条唯一”达到至少 3x 的验收标准；1 MiB 同场景为 0.78x，说明小
body 的固定成本没有被掩盖。差分测试覆盖固定形态和 128 组生成历史。当前结果：

- `aether-usage-runtime`：156 passed，1 个手动 benchmark ignored；手动 benchmark
  另行执行并通过。
- `aether-data` PostgreSQL usage repository：106 passed，410 filtered。
- `aether-gateway data::state`：32 passed，3339 filtered。
- `aether-gateway system_config`：36 passed（包含 2 个共享缓存测试）；另有 12 个
  prefetch/流式失败用例和 1 个同步取消 guard 用例通过。

真实 PostgreSQL 集成测试依赖 `AETHER_TEST_POSTGRES_URL`，当前环境未配置，测试明确
打印 skip 后返回；因此不能声称完成了真实数据库 WAL/dead tuple 压测。
