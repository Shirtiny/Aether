# Codex 号池出站 Session / Thread / Turn 合成与复用计划

> Status: **Implemented（第四稿）— 已在 `custom` 分支落地，随 `backend-v0.7.101` 交付；线上未更新，生产号池打开开关仍受 §17 门禁**
> Date: 2026-09-03
> Scope: Codex OAuth 号池在选号之后，按账号、按日合成并复用上游可见的 `session_id` / `thread_id` / `turn_id` / `window_id`
> Production changes: **本轮不更新线上。** 只提交代码并推送 `backend-v0.7.101` tag 触发 CI 构建镜像；**不执行 `update.sh`**，运行中的容器不变，功能缺省关闭
> Deploy: 生产更新路径仍是 tag → CI → ghcr → `update.sh`，不是 `deploy.sh`。何时执行 `update.sh`、何时在某个 provider 上 `enabled: true`，由操作员另行决定

本文是第四稿（最终稿）。第四稿在第三稿基础上只做两处算法修订并补齐实现细则（§18）：turn 槽按 **出站 thread UUID** 而不是 `thread_slot` 分区（root freeze 跨日命中的 thread 不得与当天同槽的另一条 thread 共用 turn UUID）；Redis 槽位 / freeze 只用单键 SET NX，不再引入锁键。§18 记录模块 API、Redis 操作、UUIDv7 生成、调用点、WS 快照、前端开关、测试与 CI 门禁、版本号。第二稿相对第一稿补上审查硬缺口：接续冻结、turn 按 thread 分区、HTTP 身份头泄漏、配置读写语义、Redis 命名空间、握手指纹不得纳入出站 ID。第三稿按代码复核（aether `custom` 分支与官方 codex-rs @ 357696c5）修正：memory 请求改为「blob 无身份、dash / 扁平带合成身份、无 turn」而不是全剥；接续冻结重新定性为跨路径 / 跨连接 / 跨日的 thread 稳定性，freeze miss 不再透传入站；`prompt_cache_key` 与 Aether 自补短头的顺序约束；attestation 已在候选头构建前剥离；turn-state 按出站 turn 来源转发；freeze TTL 滑动；补 `request_kind` 解析与 dash 头改写落点。骨架未改：三套身份平面、选号之后改写、sticky / WS 绑定 / 结算仍读入站、缺省关闭。

## 1. 背景

当前 Codex 号池把真实客户端的官方运行时身份原样转发给 ChatGPT：

- `session_id` / `thread_id` / `turn_id` / `window_id`
- 规范载体 `client_metadata["x-codex-turn-metadata"]`
- 兼容投影：dash 头 `session-id` / `thread-id`、`x-codex-window-id`、扁平 `client_metadata`
- WS 握手把入站 `OfficialRequestIdentity` 直接写成上游握手头

结果是：上游看到的 thread / turn 数量约等于真实多用户、多客户端流量。号池账号因此呈现出远高于单机 Codex 客户端的会话树密度。ChatGPT 会按账号历史里的 session / thread / turn 计数做风控，并可能把流量暗中切到垃圾模型；这与合约不符。对策是在与 Codex 客户端请求头 / 指纹 / 每号稳定 UA+Originator 同一层，生成并复用出站运行时 ID。

2026-07-07 的 profile v1（`sticky-and-profile.md`）把这些字段定义为用户所有、默认透传，并禁止合成。**该约束对本功能作废。** 产品要求现在就是：在选中的号池账号上稳定生成或复用 Codex Session / Thread / Turn，并通过可配置的「每日期望 thread 数 / 每条合成 thread 每日期望 turn 数」把上游可见计数压到真实流量之下。

这不是文档里那套「未来 synthetic-client mode」（给非 Codex 客户端补官方形状 ID，按 Aether 租户 / API Key 命名空间）。那是另一条产品线。本功能是 **选号之后的出站改写**，挂在与 `pool_advanced.codex_client_headers` / `fingerprint.codex_client_profile` 同一层。

官方 Codex 身份是一棵树，不是单个 conversation id：

| 层 | 官方含义 | 生成 | 上游形态 |
|---|---|---|---|
| `installation_id` | 本机安装 | 持久化 UUIDv4 | profile 已改写，本功能不碰 |
| `session_id` | 会话树根 | UUIDv7；根线程上 `SessionId::from(thread_id)`，与 thread 同 UUID | 本功能合成 |
| `thread_id` | 一条 conversation / agent | UUIDv7 | 本功能合成 |
| `turn_id` | 一次用户发起的工作单元 | UUIDv7；steer 不新开 turn | 本功能合成并复用 |
| 传输 `window_id` | `{thread_id}:{n}` | compact 后 `n` 递增 | v1 固定 `{synthetic_thread}:0` |
| compact 内部 window UUID | `first` / `previous` / `current` | UUIDv7 | 不在线上，不合成 |

官方当前 `prompt_cache_key` 默认是 **`session_id`**（`codex-rs/core/src/client.rs`）。`sticky-and-profile.md` 仍写 `thread_id`，已过时。根线程上两者相同；子 agent 共享根 `session_id`、各自有 `thread_id`。

一次用户动作对应 **一个 turn、N 次采样请求**（工具循环、重试、compaction、prewarm）。Aether 不得把「一条 HTTP/WS 记录 = 一个 turn」。`request_kind` 取值：`turn` / `prewarm` / `compaction` / `memory`。Memory 请求官方只在 `x-codex-turn-metadata` blob 里省略 installation/session/thread/turn/window（`has_turn_identity` 与 `has_request_identity` 都为 false，`responses_metadata.rs:349-360`）；扁平 `client_metadata()` 与 dash 头仍无条件带该 thread 自己的 installation/session/thread/window，且没有 turn_id（`memories/write/src/runtime.rs:250-280`）。本功能在开关打开时必须复现这一形状：blob 无身份、dash / 扁平写合成 thread 身份、任何投影都不出现 turn_id；既不注入 turn，也不把入站 UUID 透传到上游。见 7.2。

官方 WS 注释写明：prewarm 会等到完成，好让下一请求复用同一条连接和 `previous_response_id`。Aether 侧 `previous_response_id` 被 WS epoch fence 钉在同一条物理连接上（换候选即重置，见 7.5），HTTP 路径默认剥掉。因此接续冻结要解决的不是「新 UUID 挂旧链」，而是同一入站 root 在 HTTP compact / WS / 重连 / 跨日之间不得换 thread，同一入站 turn 跨午夜不得换 turn。

## 2. 已接受的产品决定

1. **合成是产品要求，不是禁区。** 入站官方 ID 仍归 Aether 内部使用；出站 ID 在开关打开后归选中账号所有。
2. **粘性、WS 连接绑定、Aether 结算继续读入站 ID。** 合成不得发生在选号之前，也不得让调度器读到出站 ID。
3. **按账号、按日、按槽位复用，而不是全账号共用一条 thread。** 每日期望 thread 数 = 该账号当天活跃合成 thread 池大小。
4. **Session 默认与合成 thread 同 UUID**（官方根线程形状）。v1 把 subagent / fork 塌缩到父树，不在上游保留 child thread + 共享 session。
5. **Turn 复用是降计数的主杠杆，也是异常风险点。** 同一入站 `turn_id` 必须稳定映射到同一出站 `turn_id`（工具后续请求仍算同一 turn）。Turn 槽位按合成 thread 分区：不同入站 root / 不同合成 thread **不得** 共用同一个出站 `turn_id`。
6. **缺省关闭。** 未配置或 `enabled: false` 时行为与今天完全一致。
7. **不改 Admin 表单。** v1 只读 `pool_advanced` JSON，与 `codex_client_headers` 相同。前端高级设置 / 调度对话框今天用 `...currentConfig` 整段合并，未知键会保留；实现时不得加白名单把 `codex_runtime_identity` 丢掉。
8. **Redis 不可用时透传入站 ID，不在进程内私自 mint。** 多实例必须看到同一 UUID。这是唯一允许透传的情形；freeze miss 不透传（见 11）。
9. **不改 prompt / instructions / `<environment_context>`。**
10. **本计划不授权生产启用。** 实现落地后仍需操作员显式打开某个 Codex provider 的 JSON 开关。
11. **接续冻结优先于日切换槽。** 同一入站 root 仍有未过期的冻结快照时，出站 session/thread/window（以及该入站 turn 的出站 turn）必须复用冻结值。HTTP compact 与 WS 主 turn 共用这一快照。WS 上以候选进程内快照为准，Redis freeze 是供其它路径读取的副本。「带 `previous_response_id` 而 freeze miss」在 WS 上被 epoch fence 构造性排除，在 HTTP 上只在 operator body_rules 保留该字段时可能；若出现，按槽正常 mint 并打点，**不透传入站身份**。
12. **配置非法时关闭合成，不静默 clamp 成 6/48。** 写路径拒绝非法类型和越界；读路径打点并当 `enabled: false`。`enabled: false` 是合法关闭，不得抄 `validate_codex_client_header_config` 把关闭当成错误。

## 3. 目标

1. 打开开关后，单个号池账号每天向上游 **新暴露** 的 distinct `thread_id` 不超过 `expected_threads_per_day`（跨日冻结续上的旧 UUID 不计入「新暴露」）。
2. 同一条合成 thread 每天向上游 **新暴露** 的 distinct `turn_id` 不超过 `expected_turns_per_day`。账号最坏上限是 `N × M`（所有 thread 槽都活跃）。这是有意的：禁止用账号全局 turn 槽，避免 session A 和 session B 共用一个出站 `turn_id`。
3. 同一入站 root session 在同一天窗口内稳定映射到同一合成 thread；接续冻结未过期时，跨日、重连、HTTP compact 与 WS 之间仍使用同一冻结 thread。
4. 同一入站 `turn_id` 在同一合成 thread 的同一天窗口内稳定映射到同一合成 turn（含工具后续、重试、该 turn 上的 compaction/prewarm）。同一 WS 候选快照上的步骤、以及 per-turn freeze 命中的跨午夜请求，不得换 turn UUID。
5. 出站所有身份投影一致：dash 头、扁平 `client_metadata`、规范 turn-metadata JSON、WS 握手头、HTTP 身份头。
6. 入站 sticky token 格式保持 `session=<官方 root>`。
7. WS `codex_identity_changed` / `matches_connection_binding` 仍比较入站 session/thread/window/responses_lite。`handshake_fingerprint` 继续哈希 **入站** identity，不得纳入出站 runtime ID。
8. Aether 计费 `request_id` 与 API Key 结算不变；usage 分组仍用入站 `client_session_affinity`。
9. 关闭开关时，现有 profile v1 测试全部保持：只改 `installation_id` / UA / originator。

## 4. 非目标

- 给非 Codex 客户端补官方运行时 ID（旧 synthetic-client mode）。
- 在上游保留官方 subagent 树（child `thread_id` + 共享根 `session_id`）。
- 改写 prompt 可见的 `<environment_context>`。
- 把合成 ID 写入 Postgres、`fingerprint.codex_client_profile` 或 `upstream_metadata`。
- Admin UI 表单字段 / 批量操作 / 环境变量。v1 也不把该对象加进 `AdminProviderPoolConfig` 的 typed 字段。
- 一天之内超出 N/M 后再滚动新 ID。
- 自发起生产启用、灰度百分比、影子流量。
- 修改 `client_session_affinity.rs` 的提取顺序或 sticky token 格式。
- 把 HTTP 兼容短头 `session_id` / `conversation_id`（由 `prompt_cache_key` 哈希而来）当成官方运行时身份。
- 让 Codex HTTP 路径开始转发 `previous_response_id`。今天 `CODEX_OPENAI_RESPONSES_UNSUPPORTED_BODY_FIELDS` 会剥掉它（仅 operator body_rules 显式接管该字段时保留）；v1 保持剥离。WS 上该字段经 epoch fence 后原样转发，见 7.5。
- 把出站 runtime ID 写进 `CodexConcreteAccountProfile` 或 `CodexWsCandidate.headers`（后者参与握手指纹）。

## 5. 三套身份平面（禁止混用）

| 平面 | 所有者 | 用途 | 存储 |
|---|---|---|---|
| 入站官方 Codex ID | 真实客户端 | sticky `session=<root>`、调度亲和、WS `OfficialRequestIdentity` 绑定、`logical_turn_id` / `x-codex-turn-state` 客户端侧恢复、Aether usage 分组 | 不持久化合成表；现有 affinity / usage 元数据保持入站值 |
| 选中号池账号 | OAuth key | `chatgpt-account-id`、冻结 UA / originator / `installation_id` | `fingerprint.codex_client_profile`（已有） |
| 出站合成运行时 ID | 选中 ChatGPT 账号（`selection_fp`）+ 按日槽位表 + 入站 root 冻结 | 上游 headers、`client_metadata`、turn-metadata、WS 握手 | Redis，见第 8 节 |

当前代码已经把前两套分开。第三套是本功能新增，且 **只允许出现在选号之后的出站请求**。

请求流水：

```text
inbound request
  -> 解析官方 session/thread/turn/window（不变）
  -> sticky / affinity / WS binding 使用入站 root
  -> scheduler 选出 pool key
  -> profile apply：UA / originator / installation_id
  -> NEW：WS 候选已有快照 / root freeze 未过期 → 复用快照
           否则 入站 ID -> 账号按日槽位 -> mint/复用 -> 写入 freeze
           （memory 只解析 thread，不 mint turn）
  -> 改写出站表面（不得写回 candidate.headers 再参与握手指纹）
  -> upstream ChatGPT
```

若把出站 ID 写回 sticky 提取路径，粘性缓存会 miss，下一跳换号。这是硬故障，不是风格问题。

## 6. 配置

挂在 provider `config.pool_advanced`，与 `codex_client_headers` 一样由 planner **现场读取**。`AdminProviderPoolConfig` v1 **不**增加字段。后端 `normalize_pool_advanced_config` 对 JSON 对象原样保存，不重建 typed 子集，因此未知键（包括本对象）能进库。Admin summary 回传的是原始 `pool_advanced`。前端 `PoolAdvancedDialog` / `PoolSchedulingDialog` 用 `...currentConfig` 合并后再整段 PUT；只要 summary 带回该键，UI 保存不会丢掉它。

```json
{
  "pool_advanced": {
    "codex_runtime_identity": {
      "enabled": false,
      "expected_threads_per_day": 6,
      "expected_turns_per_day": 48
    }
  }
}
```

| 字段 | 缺省 | 规则 |
|---|---|---|
| 对象缺失 | 关闭 | 生产号池保持今天的透传 |
| `enabled` | `false` | 必须是布尔。`true` 才改写。`false` 是合法关闭 |
| `expected_threads_per_day` | 仅当对象存在且 `enabled: true` 时需要 | 整数，范围 `1..=64`。当天该账号活跃合成 thread 槽数 N。缺字段或越界 = 非法 |
| `expected_turns_per_day` | 仅当对象存在且 `enabled: true` 时需要 | 整数，范围 `1..=512`。**每条合成 thread** 的当天 turn 槽数 M。不再要求 `M >= N` |

没有环境变量。

### 6.1 校验：不要抄 `validate_codex_client_header_config`

`validate_codex_client_header_config` 在 `enabled: false` 时返回「稳定客户端请求头已关闭，无法更新账号 UA」。那是 **刷新 UA 任务** 的写语义，不是通用配置校验。本功能若照抄，操作员将无法关闭合成。

单独写 `validate_codex_runtime_identity_config`：

| 路径 | 行为 |
|---|---|
| 写路径（admin 保存 `pool_advanced` 且该对象存在） | 非法类型、缺必填、越界 → **拒绝整次保存**，错误信息只描述本对象。`enabled: false` 接受，可省略 N/M |
| 读路径（planner 现场解析） | 对象缺失 / `enabled: false` → 关闭。对象存在但非法 → **关闭合成**，打点 `codex_rid_config_invalid`，**禁止**静默 clamp 成 6/48 |
| 刷新 Codex UA 的 batch action | 不调用本校验，也不应要求本对象 `enabled: true` |

与 `codex_client_headers.enabled` **独立**：

- 画像改写关、身份合成开：仍然改写 session/thread/turn；`installation_id` 保持现有 profile 行为（可能仍是入站值）。
- 画像改写开、身份合成关：与今天完全一致。
- 两者都开：先 profile，再身份合成。

Admin UI 不在 v1。操作员改 provider JSON。实现时：

- 不要给 `PoolAdvancedConfig` 加会丢掉未知键的白名单。
- `codex_client_headers` 刷新任务只 `insert` 自己那一项，必须继续保留兄弟键。
- 后续可选：号池配置表单。表单必须显式 round-trip 本对象。

## 7. 映射算法

账号选择键 = 现有 `codex_account_selection_key`：解密 auth 的 `account_id` → `key_id` → `key_name`。与 `installation_id` 同一账号。

```text
selection_fp = hex(SHA256("aether:codex:rid:sel:v1" || selection_key)[0..16])
```

32 个 hex 字符。Redis 与日志用 `selection_fp`，不用 ChatGPT account id 明文，也不用 Aether `key_id` 做合成树分区（见 8.1）。

日窗口：

```text
account_jitter_secs = SHA256("aether:codex:rid:jitter:v1" || selection_key) 的前 8 字节
                      解释为 u64，再 % 86400
day_id = floor((unix_secs + account_jitter_secs) / 86400)
```

账号不会在 UTC 0 点同时换池。`day_id` 是整数，进入 Redis key，不进上游。

### 7.1 Thread 与 Session

1. 入站 root = 官方 `session_id`（非空）否则 `thread_id`。与 sticky 的 `CodexSessionIdentity::root_session` 相同。
2. 没有入站 root 时：不合成 thread/session（没有可哈希的稳定输入）。不要用 Aether API Key、trace id 或随机数当 root。
3. 先查 7.5 的 root 冻结快照。命中则出站 `thread_id` / `session_id` / `window_id` 用冻结值，跳过本小节 mint。
4. `thread_slot = SHA256("aether:codex:rid:thread:v1" || NUL || selection || NUL || day_id || NUL || inbound_root) 的前 8 字节 % N`。
5. Redis 保存 `(provider_id, selection_fp, day_id, thread_slot)` → 合成 thread UUID。首个写入者 SET NX 一枚 **UUIDv7**；其余复用。
6. 出站 `thread_id` = 该 UUID。出站 `session_id` = **同一 UUID**（官方 `SessionId::from(ThreadId)`）。
7. 出站 **整字段删除**（不要改写成合成 UUID 再留下形状）：
   - `parent_thread_id`、`forked_from_thread_id`、`x-codex-parent-thread-id`
   - `x-openai-subagent`
   - `parent_turn_id`、`root_turn_id`
   - `subagent_kind`（这是 `review` / `compact` / `collab_spawn` 这类标签，不是 UUID；塌缩子树后整键删除）
   - `thread_source`（官方 reserved；取值 `user` / `subagent` / `memory_consolidation` / 任意 feature 字符串，暴露子树来源。没有 `fork` 值，fork 由上面的 `forked_from_thread_id` 表达）
8. 保留 `request_kind`。`x-oai-attestation` 今天已经在 HTTP 与 WS 候选头构建之前被 `remove_codex_pool_upstream_leak_headers` 无条件剥掉（`planner/standard/codex.rs:153`；WS 候选头取自 `decision/request.rs:286-304` 之后的同一份 map），`official_request` 拷贝名单里的该条目（`codex_ws/runtime.rs:900`）是死代码。本功能不得新写入该头，也不得让 profile 层开始生成它。官方生成上下文只含 thread_id，app-server 包体 `{v,s,t}` 不含身份；剥离是为了不把入站 token 复用到换号后的账号，不是因为它泄漏 ID。
9. `x-client-request-id`：见 9.1。官方 WS 契约是出站 `thread_id`。

禁止全账号共用一条 thread。N 个槽位让上游看起来像少量并行会话，而不是一条无限长对话。

### 7.2 Turn

Turn 槽按 **合成 thread** 分区。第一稿把 `turn_slot` 做成账号全局 `% M`，会导致不同入站 root 撞上同一个出站 `turn_id`。

1. 入站 turn key = 官方 `turn_id`（非空）否则 `inbound_root || inbound_thread || inbound_window`。
2. 没有 turn key 且没有 thread root：不合成 turn。
3. 先查 7.5：WS 候选已有进程内快照 → 直接用；否则查该入站 turn 的 per-turn freeze；都没有才按下面取槽并写 per-turn freeze。任何情况都不透传入站 turn。
4. `turn_slot = SHA256("aether:codex:rid:turn:v1" || NUL || selection || NUL || day_id || NUL || outbound_thread_id || NUL || inbound_turn_key) 的前 8 字节 % M`。
   分区点是 **出站 thread UUID**，不是 `thread_slot`：只把 `inbound_root` 拼进 turn key 不够（fallback turn key 已经含 root，分区点是 Redis 键和取模空间）；用 `thread_slot` 也不够，因为 root freeze 跨日命中的 thread 会与当天同槽位新 mint 的另一条 thread 共用 turn 槽空间，产生「两条 thread 同一个 turn UUID」这种官方永不产生的形状。
5. Redis 保存 `(provider_id, selection_fp, day_id, outbound_thread_id, turn_slot)` → 合成 turn UUIDv7。
6. 同一入站 `turn_id` 在同一合成 thread 上哈希到同一槽，因此一次真实 turn 的工具后续、重试、该 turn 上的 compaction/prewarm 共用出站 `turn_id`。不相关 turn 只在 **同一条合成 thread 内** 碰撞到 M 个槽。

`request_kind=memory`：官方形状是「blob 无身份、其它投影带 thread 自己的身份、没有 turn」。`turn_metadata_payload` 对 Memory 把 installation/session/thread/turn/window 全部置空（`responses_metadata.rs:349-360`），但扁平 `client_metadata()` 与 dash 头仍写 installation/session/thread/window（`:274-311`）；memory 用 `SessionId::from(thread_id)` 与 `{thread}:0`，不生成 turn_id（`memories/write/src/runtime.rs:250-280`）。第二稿要求「剥离全部投影」会产生官方永不产生的「没有任何 thread 的请求」。检测到该 kind 时：

- 按 7.1 / 7.5 解析该入站 root 的合成 thread（root freeze 或按槽 get-or-mint，与普通请求同一棵树）。
- dash `session-id` / `thread-id`、`x-codex-window-id`、扁平 `session_id` / `thread_id` / `x-codex-window-id` 写合成 thread 的值；`x-codex-installation-id` 仍走 profile。
- `x-codex-turn-metadata` blob 内 **不写** installation/session/thread/turn/window；若入站 blob 带了这些键（非官方客户端），整键删除。
- 任何投影都 **不出现** `turn_id`；不 mint turn 槽，不写 per-turn freeze。
- **不要透传** 入站 UUID（否则同一账号上普通请求走合成、memory 走真实客户端 ID，风控侧更脏）。
- 仍剥离 parent/fork/subagent/`thread_source`/`subagent_kind`。

Aether 今天不解析 `request_kind`（代码里没有该键的读取）。实现时从 body `client_metadata["x-codex-turn-metadata"]` 与 header `x-codex-turn-metadata` 两处解析；取值不是官方 `turn` / `prewarm` / `compaction` / `memory` 之一时按普通 turn 处理。

### 7.3 Window

传输 `window_id` 是 `{thread_id}:{n}`，不是 UUID。多条真实 thread 塌缩到一条合成 thread 后，继承入站 `:n` 会上下乱跳。

v1 **始终**发出 `{outbound_thread_id}:0`。不要编造 compact 内部的 `first_window_id` / `previous_window_id`。冻结快照里的 window 也必须是这个形状，不要在续链时改成 `:1`。

### 7.4 `prompt_cache_key`

官方核心现在默认 `prompt_cache_key = session_id`。

| 入站 `prompt_cache_key` | 出站 |
|---|---|
| 缺失 | 有入站 root：写出站合成 session **原值**（官方 `prompt_cache_key = session_id`）。现有两个填充器（`apply_openai_responses_stable_prompt_cache_key`、`codex_prompt_cache_key_to_insert`）生成的是 UUIDv5，不是 session 原值，调顺序得不到官方形状。无入站 root：不合成，填充器行为不变 |
| 等于入站 `session_id` 或 `thread_id` | 改写成出站合成 session |
| `guardian:{parent}` | 改写成出站合成 session（父 thread 已塌缩） |
| 其它显式值 | 保留。禁止用 Aether API Key 身份做种子 |

不要解析或改写 `instructions` / `input` / `<environment_context>`。

HTTP 兼容短头 `session_id` / `conversation_id`（`prompt_cache_key` SHA-256 前 8 字节的 16 hex）不是官方运行时身份，官方 HTTP 客户端根本不发它们（`codex-api/src/requests/headers.rs` 只有 `session-id` / `thread-id`）。WS 握手已经排除它们。不要用它们当映射输入。今天它们由 `apply_codex_openai_responses_special_headers`（`decision/request.rs:893`）在缺失时从 **入站** `prompt_cache_key` 派生，且该步骤在 profile apply（`:902`）之前；若身份改写放在 profile 之后而不处理短头，上游会收到与真实 session 一一对应的 16 hex 指纹。规则：合成开启且有入站 root 时，**删除** Aether 自补的这两个短头（官方形状）；入站显式带来的短头保留。备选是按出站 cache key 重算，但没有理由保留一个官方不发的头。

### 7.5 接续冻结（`previous_response_id` 与跨路径快照）

第一稿 HTTP 按请求/按日重新映射、WS 只把出站 ID 冻在物理连接上，导致同一入站 root 在 HTTP compact 与 WS 之间、重连前后、跨日之后可能落到不同合成 thread。第二稿把它定性为「新 UUIDv7 挂旧 `previous_response_id`」；按代码复核这一情形在 WS 上不可能发生（见下），HTTP 上只在 body_rules 覆盖时可能。冻结的目的因此是 **thread / turn 在路径、连接、日窗口之间的稳定性**。

当前代码事实：

- Codex **HTTP** 正规化把 `previous_response_id` 列为 unsupported，provider body 里会剥掉（`apply_codex_openai_responses_special_body_edits`，仅 operator body_rules 显式接管该字段时保留）。v1 不改变这一点。
- Codex **WS** `parse_response_create` 对 `previous_response_id` 做 epoch fence：首步一律拒绝（`protocol.rs:156-160`）；Bound 但 `expected_previous_response_id == None` 或不相等都拒绝（`:171-181`）。`last_completed_response_id` 只在本物理连接上有完成的 response 后才为 Some（`session.rs:2372-2374`），并在任何换候选（复用连接或新连）时被 `rebind()` 置 None（`session.rs:1513`，调用点 `:752` / `:833`）。因此客户端的 `previous_response_id` 只可能出现在同一条物理连接、同一候选快照上。
- Codex **WS** `materialize_codex_ws_step_body` 在正规化之后把通过 fence 的 `previous_response_id` 写回并原样上游（`runtime.rs:2995-2997`）。
- 官方 prewarm 的目的就是让下一请求复用连接 + `previous_response_id`。
- HTTP compact 与 WS 主 turn 经常共享同一入站 root，但走不同进程内路径。

规则：

1. Redis root 冻结：
   ```text
   ap:{provider_id}:codex_rid:{selection_fp}:freeze:{inbound_root_hash}
     = {"session_id","thread_id","window_id","day_id","last_turn_id","last_inbound_turn_hash"}
   ap:{provider_id}:codex_rid:{selection_fp}:freeze:{inbound_root_hash}:turn:{inbound_turn_hash}
     = outbound_turn_uuid
   ```
   `inbound_root_hash` / `inbound_turn_hash` = hex(SHA256(id)[0..16])，不写原文。
2. 查找顺序：
   1. `request_kind=memory` → 只解析 thread（root freeze 或 7.1 槽位），不查也不写 per-turn freeze，见 7.2。
   2. 无入站 root → 不合成。
   3. WS 候选已有进程内 `outbound_identity` → 直接用，不查 Redis（这覆盖了所有带 `previous_response_id` 的 WS 步骤）。HTTP 请求若 **带** `previous_response_id`（只在 body_rules 覆盖时出现）：读 root freeze；命中则 session/thread/window 用 freeze，turn 用 per-turn freeze，没有则用 `last_turn_id`；未命中则按 7.1 / 7.2 正常 mint 并打点 `codex_rid_chain_freeze_miss`。**不透传入站身份**：透传会把真实会话树暴露到该账号，比一次错位的 `previous_response_id` 更糟。
   4. 若 root freeze 未过期：session/thread/window 用 freeze（HTTP compact 与 WS 共用）；turn 先查 per-turn freeze（同一入站 turn 跨午夜不换 turn），没有则按 7.2 在该冻结 thread（出站 thread UUID）+ 当前 `day_id` 上取槽并写 per-turn freeze（跨日的新入站 turn 允许拿新 turn UUID，但不得换 session/thread）。
   5. 否则按 7.1 / 7.2 mint，SET NX 写入 freeze。
3. Freeze TTL = 日窗口剩余秒数 + 12h 余量（与槽位 TTL 相同），且 **每次命中刷新**（滑动 TTL）：连续活跃超过一天半的会话不得中途换 thread，只有空闲会话过期。槽位键不滑动。不使用 sticky TTL 当 freeze TTL（sticky 可能是 0）。
4. WS 候选上的 `outbound_identity` 是权威快照，Redis freeze 是供 HTTP compact / Search / 新连接读取的副本。物理连接可复用时（入站 `matches_connection_binding` 为真）**拷贝现有 outbound**，不要按新 `day_id` 重算。即使 Redis 里 day 槽已经滚动，这条连接仍用握手时的快照。
5. HTTP compact、Search header、WS `response.create` 只要入站 root 相同、选中同一 `selection_fp`，必须读同一 freeze。这比「WS 冻连接、HTTP 每天重映射」更严。
6. `previous_response_id` 本身 **原样转发**（仅限今天已经转发它的路径）。不要改写、不要删除 WS 上的该字段来「躲避」冻结——正确做法是让出站身份匹配产生它的那次请求。

### 7.6 `x-codex-turn-state`

这是单次 live turn 的 sticky routing token，由上游对它收到的（即 **出站**）turn 签发。官方已经写明：跨 turn 复用 `ModelClientSession` 会把上一 turn 的 token 重放到错误路由上；同一 turn 内则必须原样回带。HTTP 上它是请求头，WS 上它在 `response.create` 的 `client_metadata` 里，不在握手头（官方 `build_websocket_headers` 显式传 `None`）。

- Aether 面向客户端的 WS 恢复继续用 **入站** `logical_turn_id`：`parse_response_create` 在解析阶段先删再按「入站 turn 与绑定 turn 相同」重新插入（`protocol.rs:202-212`），这一步在 materialize 之前，不改。
- 合成打开时按 **出站 turn 的来源** 决定：出站 turn 来自候选进程内快照或 per-turn freeze（与签发 token 的那次请求相同）→ **转发** token，这是官方形状；出站 turn 是本次新 mint → **剥离**（header 与 `client_metadata`），不要把为别的出站 turn 签发的 token 挂到新 turn 上。
- 一律剥离是安全的，但会呈现「客户端从不回带 turn-state」这一官方客户端不会有的特征，v1 不采用。

## 8. Redis

与 sticky 同属共享 runtime backend，多实例可见。不要放进 fingerprint JSON，不要放进 `upstream_metadata`。

```text
ap:{provider_id}:codex_rid:{selection_fp}:{day}:thread:{slot} = uuid
ap:{provider_id}:codex_rid:{selection_fp}:{day}:turn:{outbound_thread_id}:{slot} = uuid
ap:{provider_id}:codex_rid:{selection_fp}:freeze:{inbound_root_hash} = json
ap:{provider_id}:codex_rid:{selection_fp}:freeze:{inbound_root_hash}:turn:{inbound_turn_hash} = uuid
```

第四稿去掉锁键：每个槽位 / freeze 都是单键，`RuntimeState::kv_set_if_absent`（Redis `SET NX PX`）本身原子；两实例对同一键只有一个写入成功，失败方 `kv_get` 回读赢家值。sticky 的 `sticky_init` 锁是为多键初始化准备的，这里不需要。

| 项 | 规则 |
|---|---|
| TTL | 日窗口剩余秒数 + 12h 余量（覆盖 jitter、迟到请求、跨午夜工具循环）；freeze 键每次命中刷新（滑动），槽位键不刷新 |
| Mint | 首次绑定现场生成 UUIDv7，使 ID 看起来是当天创建的。UUIDv7 带时间；把昨天的 ID 配上新的 `turn_started_at_unix_ms` 是可检测异常。freeze 命中时 **保留** 昨天的 UUIDv7，这比会话中途换 thread 更可接受 |
| 并发 mint | 单键 SET NX（`kv_set_if_absent`），失败方回读；两个实例不得为同一槽 / 同一 freeze 得到不同值。root freeze 的 `last_turn_id` 更新用 `kv_set_if_value` CAS，失败即放弃（尽力而为） |
| 日志 | 入站 ID 与 selection 只记哈希；禁止记录原始官方账号 ID、OAuth token、Aether API Key |
| Redis 故障 | 该请求 **透传入站 ID**，打点 `codex_rid_store_unavailable`。禁止进程内私自 mint |
| 独立 HTTP 请求（无 previous_response_id、无 freeze） | 按当前 day 槽映射 |
| WS 候选已有快照 / 未过期 freeze | 见 7.5，忽略 day 换槽 |
| WS | 候选首次握手解析出站 ID，冻结在该物理 binding 上；午夜日切也不换，直到连接排空。同时写入 Redis freeze，供 HTTP compact 对齐 |

### 8.1 `selection_fp` 而不是 Aether `key_id`

第一稿槽位哈希用 `codex_account_selection_key`，Redis 路径用 Aether `key_id`。同一 ChatGPT 账号挂在两个号池 key 上时：`installation_id` 相同（profile 已按 selection key），合成 session 树却是两套。

v1 **Redis 分区用 `selection_fp`**。同一 ChatGPT 账号的两把号池 key 共享合成树，与共享 `installation_id` 一致，上游更像一台 Codex。

仍按 Aether `key_id` 隔离的东西：

- sticky `ap:{provider_id}:sticky:{token}`
- 并发 / RPM / cooldown / 配额

若 sticky miss 后调度选中同一 ChatGPT 账号的另一把 key，出站 runtime ID 仍落在同一棵合成树上。这是有意的。

若操作员需要两套互不可见的合成树，不要把同一 ChatGPT 账号配进同一个 provider 的两把 key。跨 provider 已经用 `provider_id` 前缀隔开。

key 构造函数放在新模块 `codex_runtime_identity.rs` 内（与 sticky 的 `pool/runtime/keys.rs` 同一 `ap:{provider_id}:` 前缀约定），不把 rid 写进 sticky 读写。

## 9. 必须一致的出站表面

规范源是 `client_metadata["x-codex-turn-metadata"]` JSON。session/thread/turn/window 一变，下列投影必须一起变：

| 表面 | 动作 |
|---|---|
| dash `session-id` / `thread-id` | 写成出站 UUID。HTTP planner 今天没有任何投影代码，这两个头是入站透传；改写是对透传头的新 pass（9.1 末段） |
| `x-codex-window-id` | `{outbound_thread}:0` |
| `x-codex-turn-metadata` | 解析后改写 `session_id`/`thread_id`/`turn_id`/`window_id`/`installation_id`（后者仍走 profile）；**只改写已存在的键，不补缺失键**（`request_kind` 缺失时官方 blob 本就没有 installation/window；memory 见 7.2）；删除 7.1 的泄漏键；Unicode 继续 ASCII escape，保持 HTTP 头安全 |
| 扁平 `client_metadata` | `session_id`、`thread_id`、`turn_id`、`x-codex-window-id`、`x-codex-installation-id`；删除 parent/subagent/`thread_source`/`subagent_kind` 扁平键 |
| `x-client-request-id` | 见 9.1 |
| Compact `openai:responses:compact` | body `client_metadata` **继续整段剥离**；请求头仍改写，且必须走 7.5 freeze |
| Search | 只改 header 里的 turn-metadata；继续剥离 `x-codex-installation-id` |
| `chatgpt-account-id` | 仍是选中 key 的 auth，不碰 |
| `installation_id` | 仍是 profile 所有，不由本功能生成 |
| `previous_response_id` | HTTP Codex 继续剥；WS 经 fence 后原样转发 |
| 短头 `session_id` / `conversation_id` | 合成开启且有入站 root：删除 Aether 自补值；入站显式值保留（7.4） |

`turn_started_at_unix_ms`、`sandbox*`、`workspaces`、`tool_namespaces_info`、`compaction`、`request_kind`：v1 原样保留（memory 的 blob 形状见 7.2）。Aether 今天不读 `request_kind`，需新增解析（11 节）。不要重写 `turn_started_at_unix_ms` 去「对齐」UUIDv7 时间戳。

### 9.1 HTTP / WS `x-client-request-id`

官方 Responses 客户端把该头设为 `thread_id`。当前 Aether HTTP special-headers：若入站已有该头则保留，否则填 Aether trace / request id。第一稿只强制改写 WS，HTTP 会把入站 `thread_id` 漏给上游。

| 入站值 | 出站 |
|---|---|
| 缺失 | WS：写出站 `thread_id`。HTTP：保持现有「填 trace」行为，**不要**为了合成去新写 thread_id（避免把非 Codex HTTP 客户端改成官方形状） |
| 等于入站 `thread_id` 或入站 `session_id`（大小写敏感，去空白后全等） | 改写成出站 `thread_id` |
| 其它显式值（trace UUID、Aether request id 等） | 保留 |

WS `official_request` **始终** 写 `x-client-request-id = outbound.thread_id`（官方 WS 契约），不管入站是什么。

HTTP planner 今天对 dash `session-id` / `thread-id` / `x-codex-window-id` 没有任何投影代码，它们是入站透传。改写实现为对透传头的新 pass：值等于入站 session/thread/turn/window（去空白后全等）→ 换成对应出站投影；其它值不动。短头 `session_id` / `conversation_id` 按 7.4 删除。

## 10. WebSocket

今天：

- `CodexWsCandidate.identity` = 首步入站 `OfficialRequestIdentity`
- `official_request()` 把该 identity 写成 `session-id` / `thread-id` / `x-client-request-id` / window / parent / subagent，只把 turn-metadata 里的 `installation_id` 换成 profile
- `matches_connection_binding` 比较 session + thread + window + responses_lite
- `UpstreamBindingIdentity.handshake_fingerprint` 哈希：
  - `provider_headers`（`candidate.headers`）
  - 入站 `official_identity`（session/thread/window/parent/subagent/responses_lite）
  - `account_profile`（UA / originator / installation_id / fingerprint_hash）
  - 若干客户端 beta 头
  - 注释已写明 turn-metadata **不**标识物理 socket
- `can_reuse_physical_binding` 要求 `binding_identity` 相等，且 Codex 适配器下入站 identity 匹配
- 中途身份变化以 `codex_identity_changed` 拒绝
- `logical_turn_id` 来自扁平 `turn_id` 或 turn-metadata，用于同 turn 恢复 `x-codex-turn-state`
- usage `request_id` = `ws-{uuidv5(epoch:generation:correlation)}`
- usage affinity 回退到入站 official session/thread

本功能要求：

1. `candidate.identity` **保持入站**。绑定比较函数不改。
2. 另存 `candidate.outbound_identity`（或等价快照），在选号且合成开启后解析一次，握手与后续 step body 共用。解析走 7.5，结果写入 Redis freeze。
3. `official_request()` 使用 outbound snapshot：`session-id`、`thread-id`、`x-client-request-id`、window、改写后的 turn-metadata；**不要**写出站 parent/subagent/`thread_source`。
4. **禁止**把出站 session/thread/window/turn 写进 `candidate.headers` 再去算 `handshake_fingerprint`。指纹继续哈希入站 `official_identity`。出站 ID 只出现在 `official_request` 的 insert 和 step body materialize。
5. `materialize_codex_ws_step_body` 在 profile apply（`runtime.rs:2972-2978`）之后做 body 身份改写；`x-codex-turn-state` 按 7.6 决定转发或剥离（同一候选快照上的步骤一律转发）。`previous_response_id` 继续按今天的方式通过 fence 后写回并上游（`:2995-2997`）。
6. 面向客户端的 turn-state 恢复仍按入站 `logical_turn_id`。
7. usage / settlement 仍用入站 affinity 与现有 `request_id`。
8. 日切不改变已连接 binding 的 outbound snapshot。重规划时：若物理 binding 可复用（入站 identity 匹配且 `handshake_fingerprint` 因入站未变而相等），继续用冻结的 outbound，即使 Redis day 槽已滚动；若换连接，按新候选走 7.5（`rebind()` 已把 `last_completed_response_id` 置 None，新连接首步不会带可接受的 `previous_response_id`；root freeze 命中则 thread 不换）。

若实现错误地把出站 ID 放进指纹，午夜 remap 会让 `binding_identity` 不相等，`can_reuse_physical_binding` 为假，连接被排空——这会把「WS 冻结合成身份」打穿。

## 11. 代码落点

新建模块，不要把运行时身份塞进 `CodexConcreteAccountProfile`：

`apps/aether-gateway/src/codex_runtime_identity.rs`

- 解析 `pool_advanced.codex_runtime_identity`
- `validate_codex_runtime_identity_config`（写路径）与 `codex_runtime_identity_rewrite_enabled`（读路径，非法即关）
- day id / jitter / slot hash / `selection_fp`
- Redis get-or-mint 与 freeze
- `OutboundCodexRuntimeIdentity { session_id, thread_id, turn_id, window_id }`
- headers + `client_metadata` + turn-metadata JSON 改写（复用 `codex_profile.rs` 的 ASCII JSON / Unicode escape）
- `request_kind` 解析（body / header turn-metadata；Aether 今天没有）、memory 形状、泄漏键删除、短头删除、`x-client-request-id` 条件改写、turn-state 按来源转发 / 剥离

在 **profile apply 之后** 调用，调用点与现有 profile 相同。HTTP 上 `apply_codex_openai_responses_special_headers`（`decision/request.rs:893`）在 profile（`:902`）之前就已按入站 `prompt_cache_key` 补短头，所以身份改写要么提前到它之前，要么在改写后删除其自补短头（7.4）：

| 路径 | 作用 |
|---|---|
| `apps/aether-gateway/src/ai_serving/planner/standard/codex.rs` | 读配置；校验函数 |
| `planner/standard/openai/responses/decision/request.rs` | HTTP Responses 头+体；Search 头 |
| chat / family / image 的 Codex apply 位点 | 若走 Codex provider 则同样改写 |
| Compact | 只改头；走 freeze |
| `codex_ws/runtime.rs` `official_request` | 握手出站 ID；不新增头。拷贝名单里的 `x-oai-attestation` 条目是死代码（候选头已剥），可删可留，不得复活 |
| `codex_ws/runtime.rs` `materialize_codex_ws_step_body` | step body |
| `codex_ws/runtime.rs` `upstream_binding_identity` | 不改哈希输入集；不要加入 outbound |
| `apply_codex_openai_responses_special_headers`（`decision/request.rs:893`） | 身份改写在它之前完成，或改写后删除它补出的短头（7.4）。`apply_openai_responses_stable_prompt_cache_key`（`:795`）与 `codex_prompt_cache_key_to_insert` 本身不改 |
| `request_kind` 解析 | 新增；从 body `client_metadata["x-codex-turn-metadata"]` 与 header `x-codex-turn-metadata` 读取（7.2） |
| HTTP dash 头 pass | 新增；对透传的 `session-id` / `thread-id` / `x-codex-window-id` 按值改写（9.1） |
| Redis key | 构造函数在 `codex_runtime_identity.rs`；沿用 `ap:{provider_id}:` 前缀，不进 sticky 读写 |
| admin 保存 `pool_advanced` | 若对象存在则写路径校验；不要重建 typed 子集 |

**不要改：**

- `client_session_affinity.rs` 提取顺序 / token 格式
- `OfficialRequestIdentity::matches_connection_binding`
- Aether settlement `request_id` / API Key 计费
- prompt / instructions / `<environment_context>`
- `custom` 上无关脏改（`orchestration/mod.rs`、`docker-compose.yml`、未完成的 WS usage 改动）。`runtime.rs` / `session.rs` 只允许为 outbound snapshot 加最小钩子
- HTTP Codex 对 `previous_response_id` 的剥离名单
- WS `previous_response_id` epoch fence（`parse_response_create`）与 `rebind()` 的重置

`codex_profile.rs` 继续只拥有 installation。身份改写是兄弟调用，不是 profile 新字段。

## 12. 测试

当前测试把「运行时 ID 不变」锁死。必须拆分，**不得削弱关闭路径**。

### 关闭（缺省）保持

- `normalizes_installation_id_without_touching_runtime_or_prompt_fields`
- `codex_pool_concrete_account_profile_normalizes_installation_id_only`
- sticky / guardian / search affinity 全套
- compact 剥 body metadata、Search 剥 `x-codex-installation-id`
- HTTP Codex 继续剥 `previous_response_id`

### 打开后新增

- 同一账号、同一天，入站 session A/B 映射出的 distinct 出站 thread ≤ N；A 重复请求稳定
- 入站 turn 映射出的 distinct 出站 turn：同一合成 thread 内 ≤ M；同一入站 `turn_id` 稳定
- **不同出站 thread 不得共用出站 `turn_id`**；同一出站 thread 内的不同入站 root 可以碰撞到同一 turn 槽（这是「每条 thread 每日 ≤ M 个 turn」的必然结果），账号全局 turn 计数上限仍是 N×M
- turn 槽按出站 thread 分区：root freeze 跨日命中的 thread 与当天同槽位新 mint 的 thread 不共用 turn UUID
- 出站 `session_id == thread_id`；`window_id == "{thread}:0"`；WS `x-client-request-id == thread_id`
- 出站剥离 parent/fork/subagent/`thread_source`/`subagent_kind`；WS 绑定仍用入站 ID
- dash 头、扁平 metadata、turn-metadata JSON 三者一致
- compact：body 仍无 `client_metadata`，头已改写，且与同 root 的 WS 主 turn 出站 session/thread 相同
- Search：header metadata 已改写，仍无 `x-codex-installation-id`
- `prompt_cache_key` 等于入站 thread/session 时改为出站 session；无关自定义值保留；缺失且有入站 root 时写出站 session 原值（不是 UUIDv5）
- 改写后 sticky token 仍是 `session=<入站 root>`
- WS：出入站 ID 不同时，`can_reuse_physical_binding` 只要入站匹配仍为 true；握手头用出站；指纹不因出站 ID 改变
- Redis SET NX：两调用者得到同一 UUID
- Redis 宕机：透传、不本地 mint
- 日切 + 账号 jitter：无链的新请求拿到新 UUIDv7；WS 候选冻结忽略午夜
- **同一入站 root 跨日 / 重连：root freeze 命中则 session/thread 不换；同一入站 turn 跨午夜 per-turn freeze 命中则 turn 不换；freeze miss 时按槽 mint，不透传入站**
- WS：`previous_response_id` 只在同一候选快照上被接受（fence 不变），该快照上所有步骤出站 session/thread/turn 相同
- **WS freeze + HTTP compact：同一入站 root、同一 `selection_fp` 得到同一出站 session/thread**
- HTTP `x-client-request-id` 等于入站 thread/session 时改写出站 thread；其它 trace 保留
- `request_kind=memory`：blob 无 installation/session/thread/turn/window；dash 头与扁平 metadata 带合成 thread 的 session/thread/window；任何投影无 `turn_id`；不是透传入站值
- 合成开启时 Aether 自补短头 `session_id` / `conversation_id` 不再出现；入站显式短头保留
- turn-state：出站 turn 来自候选快照 / per-turn freeze 时转发，新 mint 时剥离
- freeze 滑动 TTL：命中后 TTL 被刷新；槽位键不刷新
- 非法 JSON：写路径拒绝；读路径关闭合成，不出现 6/48
- `enabled: false` 保存成功，行为等于缺省关闭
- 同一 `selection_fp`、两个 Aether `key_id` 共用槽位 UUID
- 非 Codex provider 不受影响
- 无入站 root：不合成

建议命令（`custom` 分支套件本就不全绿，不要把既有失败算作本功能回归）：

```bash
cargo test -p aether-gateway --lib codex_runtime_identity
cargo test -p aether-gateway --lib client_session_affinity
cargo test -p aether-gateway --lib pool_sticky_session_token
cargo test -p aether-gateway --lib codex
cargo test -p aether-ai-formats --lib codex
```

不要 7GB 并发 cargo。测试不得打生产流量。

## 13. 实现顺序（仅在本计划获准实现之后）

1. 配置解析（写拒绝 / 读关闭）+ Redis 槽位与 freeze + hash/day/lock 单测。
2. HTTP Responses 改写（`request_kind` 解析 + 头 + 体 + `prompt_cache_key` + 短头删除 + `x-client-request-id` 条件改写 + memory 形状 + turn-state 按来源）。
3. Compact + Search；与 freeze 对齐。
4. WS 握手 outbound snapshot + body materialize；指纹不含出站 ID；候选冻结。
5. 拆分「运行时 ID 不变」测试；补打开路径测试，尤其是 12 节加粗项。
6. 实现落地时同步改文档：
   - 本文件 Status 改为 implemented / 注明版本
   - 仓库根 `sticky-and-profile.md`（不在 docs/）：出站合成是显式号池模式；入站仍拥有 sticky；`:32` / `:35` 的「`prompt_cache_key` 默认 `thread_id`」改为 `session_id`；`:717` / `:720` / `:735` 的「不得合成 / 覆盖」改为「缺省不合成，`codex_runtime_identity` 显式开启除外」
   - `docs/codex-websocket.md` 握手节：入站绑定 vs 出站握手 ID

本 docs 文件的提交早于代码。本轮（2026-09-03）用户已批准实现并推送 tag；**未批准 `update.sh` 更新线上，也未批准在任何生产 provider 上打开开关**。

## 14. 回滚

1. 把目标 provider 的 `codex_runtime_identity.enabled` 设为 `false`（或删除对象）。立即恢复透传，不需要重启。
2. Redis `ap:*:codex_rid:*` 可自然过期，也可按 provider 前缀删除；删除只影响未过期槽位 / freeze 的稳定性，不影响 sticky。删 freeze 后，进行中的会话在下一次非 WS 快照请求上会按槽重新 mint（可能换 thread），不透传。
3. 二进制回退前先关开关，避免旧进程与新进程对同一槽位语义不一致。
4. 不要用删号、清 OAuth、改 `codex_client_profile` 来关这个功能。
5. 不要靠 Admin 表单「保存一次」来关功能——当前表单会保留未知键。必须改 JSON 里的本对象。

## 15. 已知异常（接受）

把大量真实会话塌缩到 N 条 thread、每条 thread M 个 turn **不是**官方 Codex 行为。ChatGPT 侧能看到：

- UUIDv7 时间与 `turn_started_at_unix_ms` 不完全同序（碰撞 turn 会复用较早 mint 的 v7）
- 所有合成会话 `window_id` 停在 `:0`
- 没有 parent/subagent/`thread_source` 树
- 多段无关 prompt 共享同一 `prompt_cache_key`（出站 session）
- 跨午夜的长会话继续使用「昨天」的 UUIDv7（接续冻结，滑动 TTL）。这比会话中途换 thread 更可接受
- memory 请求 dash / 扁平带合成 thread、blob 无身份，与官方形状一致；但该 memory 的 session/thread 与同槽其它真实会话共用
- 同一 ChatGPT 账号的两把号池 key 共享合成树（也已经共享 `installation_id`）
- 账号最坏 turn 计数是 `N × M` 而不是 `M`

这就是功能本身。设计内缓解：每日新 UUIDv7（无 freeze 时）、N>1 槽、session==thread、window `:0`、turn-state 只在同一出站 turn 上回带、不泄漏入站 ID、Redis 协商同一 UUID、freeze 未过期则冻结合成身份。v1 不追求「看起来完全像一台真实 Codex」。

**不接受**（实现前必须避免）：

- 不同出站 thread 共用出站 `turn_id`（turn 槽必须按出站 thread UUID 分区；同一出站 thread 内的槽碰撞是设计内的）
- 同一入站 root 在 HTTP compact / WS / 重连 / 跨日之间换 thread（freeze 未过期时）
- freeze miss 时透传入站身份
- memory 请求透传入站 UUID，或把 memory 剥成没有任何 thread 的请求
- 合成开启后仍向上游发 Aether 自补的 `session_id` / `conversation_id` 短头
- 为新 mint 的出站 turn 回带别的 turn 的 `x-codex-turn-state`
- 出站仍带 `thread_source` / `subagent_kind`
- HTTP 把入站 thread 当 `x-client-request-id` 上游
- 非法配置静默变成 6/48
- 握手指纹因出站 ID 变化而打断可复用连接
- 前端 / admin 保存丢掉本对象

## 16. 与现有文档的关系

| 文档 | 关系 |
|---|---|
| 仓库根 `sticky-and-profile.md`（`/opt/stacks/aether/sticky-and-profile.md`，不在 docs/） | `:717` / `:720` / `:735` 仍描述「默认不合成 / 不覆盖」，`:32` / `:35` 写 `prompt_cache_key` 默认 `thread_id`（已过时）。实现时改为：入站 ID 归 sticky / 绑定；出站合成是 `codex_runtime_identity` 显式模式 |
| `docs/codex-websocket.md` | 绑定与 fence 仍入站；握手出站 ID 在实现后补一小节 |
| `docs/architecture/ws-usage-session-observability.md` | usage 会话身份继续用入站 official session/thread，本功能不改 |
| 官方 `codex-rs` | 生成规则（UUIDv7、session=thread、window=`{thread}:{n}`、cache key=`session_id`、memory blob 无身份但 dash / 扁平带 thread、prewarm+`previous_response_id`）是出站形状与接续规则的参考，不是「禁止合成」的依据 |

## 17. 生产启用门禁（实现完成之后，仍须单独授权）

缺任一项都不应在生产号池打开 `enabled: true`：

- 关闭路径回归：现有 profile / sticky / WS binding 测试全过
- 打开路径：N 上限、每 thread M 上限、跨 root 不共享 turn、稳定映射、表面一致、Redis SET NX、Redis 故障透传
- HTTP + Compact + Search + 官方 WS 握手四条路径都有夹具
- 跨日 / 重连 freeze 稳定性、WS freeze + HTTP compact 共用出站 ID、memory 形状、短头删除、turn-state 按来源转发、HTTP `x-client-request-id` 条件改写都有夹具
- 多实例共享 Redis 槽位与 freeze，进程重启后复用同一 UUID
- 确认 sticky 仍钉入站 root，不会因出站改写换号
- 确认 Aether usage / 结算仍按入站 session 与 API Key
- 确认 Admin 高级设置保存后 `codex_runtime_identity` 仍在 `pool_advanced` 里
- 确认 `handshake_fingerprint` 不做出站 ID 的函数
- 操作员书面确认目标 provider 的 N/M（M 是每条合成 thread 每天，不是全账号），而不是使用代码缺省值当生产容量规划

源码单测通过不等于生产门禁通过。

## 18. 实现细则（最终稿）

本节是对 §6–§11 的落地约定，代码以本节为准；与前文冲突处以本节为准。

### 18.1 模块与 API

新模块 `apps/aether-gateway/src/codex_runtime_identity.rs`（`lib.rs` 加 `mod codex_runtime_identity;`，与 `codex_profile` 并列）。

```rust
pub(crate) struct CodexRuntimeIdentityConfig {
    pub enabled: bool,
    pub expected_threads_per_day: u32, // 1..=64
    pub expected_turns_per_day: u32,   // 1..=512，每条合成 thread
}
/// 写路径：admin 保存 pool_advanced 时对象存在即校验；错误信息只描述本对象
pub(crate) fn validate_codex_runtime_identity_config(value: &Value) -> Result<(), String>;
/// 读路径：对象缺失 / enabled=false / 非法 → None（非法时打 codex_rid_config_invalid）
pub(crate) fn codex_runtime_identity_rewrite_enabled(pool_advanced: Option<&Value>) -> Option<CodexRuntimeIdentityConfig>;

/// 选号后固定的作用域：provider_id + selection_key + config
pub(crate) struct CodexRuntimeIdentityScope { provider_id, selection_key, config }
/// 入站身份（只读，来自 body client_metadata / turn-metadata blob / header）
pub(crate) struct InboundCodexRuntimeIdentity {
    session_id, thread_id, turn_id, window_id: Option<String>,
    request_kind: CodexRequestKind, // Turn | Prewarm | Compaction | Memory | Other
    prompt_cache_key_present: bool,
}
pub(crate) struct OutboundCodexRuntimeIdentity {
    session_id, thread_id, window_id: String,
    turn_id: Option<String>,             // memory → None
    turn_source: OutboundTurnSource,     // Snapshot | Frozen | Minted | None
    inbound_root: String, inbound_turn_key: Option<String>, // WS 快照比对用
}
pub(crate) enum CodexRuntimeIdentityResolution { Rewrite(OutboundCodexRuntimeIdentity), Passthrough }
/// Redis get-or-mint + freeze；任何 kv 错误 → Passthrough + codex_rid_store_unavailable
pub(crate) async fn resolve_outbound_codex_runtime_identity(store: &RuntimeState, scope: &CodexRuntimeIdentityScope, inbound: &InboundCodexRuntimeIdentity, ws_snapshot: Option<&OutboundCodexRuntimeIdentity>, now: SystemTime) -> CodexRuntimeIdentityResolution;
/// 头 + body + turn-metadata 改写；surface 决定短头删除 / x-client-request-id 策略 / prompt_cache_key
pub(crate) fn apply_outbound_codex_runtime_identity(headers: &mut HeaderMap, body: Option<&mut Value>, original_headers: Option<&HeaderMap>, inbound: &InboundCodexRuntimeIdentity, outbound: &OutboundCodexRuntimeIdentity, surface: CodexRuntimeIdentitySurface);
```

planner 侧包装（`ai_serving/planner/standard/codex.rs`，经 `ai_serving/mod.rs` 复导出，与 `resolve_codex_pool_concrete_account_profile` 同一 seam）：

- `resolve_codex_pool_runtime_identity_scope(transport) -> Option<CodexRuntimeIdentityScope>`：provider_type 必须是 `codex`，读 `provider.config.pool_advanced.codex_runtime_identity`，`selection_key` 复用 `codex_pool_client_profile_selection_key`。
- `apply_codex_pool_runtime_identity(runtime: &RuntimeState, transport, headers, body, original_headers, original_body, surface, trace_id).await -> Option<OutboundCodexRuntimeIdentity>`：HTTP / Search / chat / family / image 位点统一调用；在 profile apply **之后**。

### 18.2 UUIDv7

`uuid` crate 锁在 1.22.0，workspace feature 只有 `v4` / `v5`；CI 用 `--locked`，不改 `Cargo.lock`。手写 UUIDv7：16 字节中 `[0..6]` = unix 毫秒大端 48 位，`[6]` = `0x70 | (r & 0x0F)`，`[8]` = `0x80 | (r & 0x3F)`（RFC 4122 variant），其余 74 位随机；随机源取 `Uuid::new_v4()` 的字节。经 `Uuid::from_bytes(..).hyphenated()` 输出小写。单测校验版本 / variant / 时间戳单调。

### 18.3 Redis 操作（全部走 `RuntimeState` kv API）

| 操作 | 调用 |
|---|---|
| 槽位 get-or-mint | `kv_get` → 无则 `kv_set_if_absent(key, uuid, ttl)` → 失败则 `kv_get` 回读 |
| root freeze 读 | `kv_get` → JSON；命中后 `kv_expire_if_value(key, json, ttl)` 滑动 |
| root freeze 写 | `kv_set_if_absent(key, json, ttl)`，失败回读（另一路径先写） |
| root freeze `last_turn_id` 更新 | `kv_set_if_value(key, old_json, new_json, ttl)`，失败放弃 |
| per-turn freeze | `kv_get`；无则 `kv_set_if_absent(key, uuid, ttl)`，失败回读；命中后 `kv_expire_if_value` 滑动 |
| TTL | `day_window_remaining_secs + 43200` |

`RuntimeState` 内存后端（无 Redis）时同样工作，供单测与单实例使用。任何 `Err` → `Passthrough`，`warn!(event_name = "codex_rid_store_unavailable", log_type = "event", ...)`。**不在进程内私自 mint。**

### 18.4 观测

不扩展 `fallback_metrics.rs` 的按路由枚举。三个事件用结构化 tracing：

- `codex_rid_config_invalid`（warn，含 provider_id、error）
- `codex_rid_store_unavailable`（warn，含 provider_id、selection_fp、error）
- `codex_rid_chain_freeze_miss`（debug，含 provider_id、selection_fp、inbound_root_hash）

日志只带 `selection_fp` 与 hash，不带原始 ID。

### 18.5 HTTP 位点顺序

`decision/request.rs` HTTP Responses 构建尾部：

1. `:795` `apply_openai_responses_stable_prompt_cache_key`（不改）
2. `:893` `apply_codex_openai_responses_special_headers`（不改）
3. `:902` `apply_codex_pool_concrete_account_profile_for_api_format`（不改）
4. **新增** `apply_codex_pool_runtime_identity(...)`：入站身份从 **`effective_headers` + 原始 `body_json`** 提取（`prompt_cache_key_present` 看原始 body，因为第 1 步已经补了 UUIDv5）；改写 `provider_request_headers` / `provider_request_body`；删除 Aether 自补短头（原始头没有 `session_id` / `conversation_id` 才删）。

Search（`:1118` 之后、`normalize_openai_search_headers` 之前）、chat / family / image 位点：只传 headers（`body = None`），surface 为 `Headers`。

### 18.6 WS

- `CodexWsCandidate` 新增 `runtime_identity: Option<Arc<CodexWsRuntimeIdentitySnapshot>>`（`{ scope, outbound }`）。`select_candidates` 在 `account_profile` 解析之后、仅当 adapter 为 Codex 时，用首步 `value` + 入站请求头解析一次（`resolve_candidate_runtime_identity`，`ws_snapshot = None`）；`candidate.headers`、`binding_identity`、`upstream_binding_identity`、握手指纹不变。
- `official_request`：有快照时 `session-id` / `thread-id` / `x-client-request-id` 写出站 session/thread；`x-codex-window-id` 在入站带该头时写出站 window；跳过 `x-codex-parent-thread-id` / `x-openai-subagent`；`x-codex-turn-metadata` 头先按 profile 归一化，再按 blob 规则改写（`rewrite_codex_turn_metadata_string`）。入站 `x-codex-turn-state` 本来就不进入握手头，无需剥离。
- `prepare_step`：在取走 `step.value` 之前用本步入站身份 + 候选快照调用 `resolve_step_runtime_identity`（`ws_snapshot = Some(&snapshot.outbound)`）：与快照同一入站 turn → `Snapshot`；否则查 / 写 per-turn freeze 或按快照 thread 取槽（`Frozen` / `Minted`）；memory → 无 turn。结果 `CodexWsStepRuntimeIdentity { inbound, outbound }` 作为最后一个参数传入 `materialize_codex_ws_step_body`，在 profile body apply 与稳定 `prompt_cache_key` 之后、序列化之前以 `WsStepBody` surface 只改写 body（不碰头）。
- 存储不可用：候选已有同一入站 root 的快照时，本步沿用快照的 session/thread/window 与快照 turn（来源 `Snapshot`），不透传入站；首步（尚无快照）存储不可用 → 该候选整段透传，与 HTTP 相同，打点 `codex_rid_store_unavailable`。
- 物理连接复用（`activate_reused_candidate`）沿用被绑定候选对象上的快照；`rebind` 换候选时随新候选重新解析（同一入站 root、同一 `selection_fp` 因 root freeze 得到同一出站 thread）。

### 18.7 配置校验挂点

`handlers/admin/provider/write/normalize.rs::normalize_pool_advanced_config`：对象存在且 `codex_runtime_identity` 非 null → `validate_codex_runtime_identity_config`；create / update 两条写路径共用该函数。不重建 typed 子集，兄弟键原样保留。

### 18.8 前端

`PoolAdvancedDialog.vue` Codex 区块「稳定客户端请求头」开关之后加「出站会话身份合成」开关（缺省关），打开时显示「每日期望 thread 数（1–64）」「每条 thread 每日期望 turn 数（1–512）」两个数字输入；类型 `PoolCodexRuntimeIdentityConfig` 加到 `provider.ts`；load / save 与 `codex_client_headers` 同处，Codex provider 保存时始终写本对象（`enabled: false` 显式关闭），非 Codex 删除该键。校验失败抛中文错误，与 `buildCodexClientHeadersConfig` 同风格。

### 18.9 测试与 CI 门禁

CI（`release.yml` `rust_tests`）跑 `cargo test --locked -p aether-ai-formats --lib`、`-p aether-data --lib`、`-p aether-provider-transport --lib`、`cargo check --locked -p aether-gateway --tests` 与少量 gateway lib 测试（含 `ai_serving_crate_api_is_confined_to_root_seams`）；前端跑 `type-check` + `build` + 两个 spec。本地至少跑：

```bash
cargo test --locked -p aether-gateway --lib codex_runtime_identity
cargo test --locked -p aether-gateway --lib codex
cargo test --locked -p aether-gateway --lib normalize_pool_advanced
cargo check --locked -p aether-gateway --tests
cd frontend && npm run type-check
```

一次只跑一个 cargo；`custom` 既有失败不算回归。

### 18.10 版本与交付

- 提交在 `custom` 分支，只含本功能相关文件；不带 `docker-compose.yml`、`.env.bak.*`、`.gopath/` 等无关脏改。
- tag：`backend-v0.7.101`（当前 `backend-v0.7.100` == `1ab530e2b`）。推 tag 触发 CI → ghcr `latest`。
- **不执行 `update.sh`**。线上运行的仍是 v0.7.100 镜像。
- 之后由操作员按 §17 门禁决定是否更新与打开。
