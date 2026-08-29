# Codex Pro 请求失败调查与处置记录（2026-08-28）

- 调查日期：2026-08-28
- 统计时区：Asia/Shanghai（CST）
- 统计窗口：`2026-08-28 00:00:00` 至 `2026-08-28 15:26:59`（对应 UTC `2026-08-27 16:00:00` 至 `2026-08-28 07:26:59`）
- 数据来源：生产 `aether-postgres.public.usage`，只读查询
- Provider：`Codex Pro`（provider id `3c7ab28d-b82a-4633-81d0-c524b9fbc7ed`）
- 调查时生产版本：`backend-v0.7.83`，容器镜像 digest `sha256:f7f365f2471e96e1ec61f1ab8dde5c07f176fb7eb7dcfb29f43e182c86c2e2f6`，容器健康
- 涉及账号和代理只记录 ID 前缀，不在本文输出邮箱、密钥或 token

> 本文记录的是一次固定时间点的线上快照。后续请求会改变计数；引用本文数据时应同时引用快照时间。文中“失败占比”默认指 `status = 'failed'` 的 `usage` 行，不把用户主动断开和仍在执行的请求算作服务端失败。

> **语义修正**：代理握手失败不能直接视为账号失败。若在已有 sticky 会话上因代理瞬时故障切换账号，会破坏会话与账号绑定；因此修复只在代理隧道层重试，并在重试仍失败时保留已有 sticky。尚未建立正式绑定的首次请求仍可在 Provider write 前换候选，但不会把网络故障计入账号健康；只有官方明确返回账号级拒绝时才清除已有 sticky。

## 1. 结论摘要

1. **今天的 Codex Pro 失败不是额度/429 问题。** 快照内没有 `401/402/403/429`，也没有官方 429 记录。此前“官方返回 429 后号池额度状态没有及时刷新”的问题属于独立问题，修复代码已纳入 `backend-v0.7.89`；不能用它解释本次失败。
2. **按发生频率，首要问题是 Responses WebSocket 通过代理建立连接失败。** 原始原因出现 25 次，占全部请求 `0.0579%`、服务端失败 `31.25%`；其中 24 次集中在 key 前缀 `9c7f126e`，并指向代理节点 `netcup-ipv6`。
3. **按单次用户体验，最严重的是“600 秒没有客户端可见内容”的流式请求。** 该类有 14 次、均为 `504`；它会让用户等待约 10 分钟后才得到错误。请求 `059c8d0e-027f-44e4-8465-b0fc068b82f3` 是其中的代表案例。
4. 另外还有上游 chunked body 意外 EOF、WebSocket 协议失败、候选池/绑定状态/大帧 CPU 容量不足、上游过载和输入错误。下面保留数据库中的**完整原始错误文本**，不以概括性标签替代原因。
5. `backend-v0.7.90` 尚未上线；其 Release Actions（run `33241849251`）在 Rust release tests 阶段失败，后端二进制和 Docker 构建均被跳过，因此没有可用的新生产镜像。该 tag 仍保留 600 秒 client-progress 上限；后续工作区已将未配置专用值时的客户端无可见内容上限收紧为 120 秒，但尚未构建、打 tag 或上线。

### 1.1 2026-08-29 后续修复状态

- 代理隧道有界重试、脱敏代理观测和“网络失败保留已有 sticky”已进入 `backend-v0.7.90` 源码，但该 tag 未成功产出镜像。
- Codex WS 账号失败判断改为复用统一的状态码加响应正文分类：`400` 的 invalid token/account/workspace disabled 和 `423` account locked 会清除失效 sticky 并允许切换账号；普通 `400` 输入错误仍不会惩罚账号。
- 显式 `stream_idle_timeout` 同时约束上游无帧和客户端无可见内容；未配置时分别使用 300 秒上游读取兼容值和 120 秒客户端可见进度上限，不再由控制帧把用户等待放大到 600 秒。
- prompt capture 的 `max_items` 恢复为总条目硬上限；仍优先保留最初上下文，但不会在配置值之外额外写入 10 条。
- Release Rust 命令拆为独立步骤，后续失败会直接显示具体 crate/检查项。上述工作区修改遵循暂停本地编译测试的要求，尚待远端 CI 验证。

## 2. 总体统计

### 2.1 状态分布

| `usage.status` | 数量 | 占全部请求 | 说明 |
| --- | ---: | ---: | --- |
| `completed` | 42,988 | 99.6407% | 完成态 |
| `failed` | 80 | 0.1854% | 本文的服务端失败样本 |
| `cancelled` | 36 | 0.0834% | 用户/下游主动断开，不计入服务端失败 |
| `pending` | 3 | 0.0070% | 快照时尚未结束 |
| `streaming` | 36 | 0.0834% | 快照时仍在流式执行 |
| **合计** | **43,143** | **100%** |  |

失败率计算：`80 / 43,143 = 0.1854%`。

### 2.2 HTTP 状态分布（失败样本）

| 状态码 | 数量 | 占失败 |
| ---: | ---: | ---: |
| `400` | 4 | 5.00% |
| `502` | 5 | 6.25% |
| `503` | 44 | 55.00% |
| `504` | 16 | 20.00% |
| `200`（响应体随后解码失败） | 11 | 13.75% |
| **合计** | **80** | **100%** |

`200` 不是成功保证：这 11 条请求已经收到上游响应头，但在读取 chunked 响应体时遇到 EOF，最终仍以 `status = failed` 结算。

## 3. 完整原始失败原因

下表的 `原始 error_message` 直接来自 `public.usage.error_message`。占比以本节快照的 43,143 个请求和 80 个失败为分母，四舍五入到 4 位/2 位小数。

| 排名 | 原始 `error_message` | 次数 | 占全部请求 | 占失败 | 最终 HTTP |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | `official_ws_proxy_connect_failed: no Responses WebSocket candidate could be connected` | 25 | 0.0579% | 31.25% | 503 |
| 2 | `provider stream produced no client-visible data for 600000 ms` | 14 | 0.0325% | 17.50% | 504 |
| 3 | `error decoding response body: error reading a body from connection: unexpected EOF during chunk size line` | 11 | 0.0255% | 13.75% | 200* |
| 4 | `candidate_pool_busy: no Responses WebSocket candidate could be connected` | 7 | 0.0162% | 8.75% | 503 |
| 5 | `official Codex WebSocket protocol failed` | 5 | 0.0116% | 6.25% | 502 |
| 6 | `Our servers are currently overloaded. Please try again later.` | 4 | 0.0093% | 5.00% | 503 |
| 7 | `bound_account_ineligible: Codex WebSocket candidate changed before provider execution` | 3 | 0.0070% | 3.75% | 503 |
| 8 | `large_frame_cpu_unavailable: large-frame CPU capacity was unavailable before provider execution` | 3 | 0.0070% | 3.75% | 503 |
| 9 | `bound_provider_changed: Codex WebSocket candidate changed before provider execution` | 2 | 0.0046% | 2.50% | 503 |
| 10 | `Stream first byte timeout` | 2 | 0.0046% | 2.50% | 504 |
| 11 | **`error_message IS NULL`**（数据库未保存具体文本） | 2 | 0.0046% | 2.50% | 400 |
| 12 | `An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID 1ee64351-ca73-455a-ae33-b531716476f4 in your message.` | 1 | 0.0023% | 1.25% | 400 |
| 13 | `Invalid 'input[15].id': 'item_087e297a3d7cd3cfe5a44563'. Expected an ID that begins with 'rs'.` | 1 | 0.0023% | 1.25% | 400 |

\* 这 11 条的 `status_code=200` 仅表示响应头阶段；正文读取失败，不能按成功请求统计。

另有 36 条取消请求，原始原因全部为：

```text
client disconnected while provider response was in flight
```

它们占全部请求 `0.0834%`，应与服务端失败分开看。

## 4. 按用户体验的处理优先级

优先级同时考虑“失败频率”和“用户等待/损失”。以下分组只是为了排期，**不替代第 3 节的原始原因**。

| 优先级 | 问题 | 样本 | 占失败 | 用户影响 | 建议方向 |
| --- | --- | ---: | ---: | --- | --- |
| P0（频率） | 代理 CONNECT 无候选 | 25 | 31.25% | 请求在执行前立即失败，且集中在单一代理路径 | 先核查 `netcup-ipv6` 的 CONNECT/DNS/IPv6 路由；增加按代理节点、key 的失败率告警和临时摘除依据 |
| P0（体验） | 600 秒无 client-visible data | 14 | 17.50% | 用户等待约 10 分钟后收到 504，最容易被感知为“卡死” | 区分控制帧与业务事件；记录最后一个公开事件；评估合理的模型/客户端专用 progress policy，不要只盲目延长超时 |
| P1 | chunked body EOF + Codex WS protocol failed | 16 | 20.00% | 已开始的请求被上游或中间代理截断/关闭 | 分离记录上游响应阶段、代理节点、EOF 位置和 WS protocol phase；优先观察代理相关路径 |
| P1 | pool busy / account binding / provider binding / large-frame CPU | 15 | 18.75% | 请求尚未写入 Provider，直接 503；可能在并发高峰集中出现 | 检查候选生命周期清理、绑定 generation、CPU budget 和并发水位；保留“未执行”语义，禁止不确定写入后的盲目重放 |
| P2 | 上游 overload + first-byte timeout | 6 | 7.50% | 上游繁忙或首字节迟迟未到 | 与 Provider、模型、代理节点交叉统计，设置短窗口趋势告警 |
| P3 | 400/输入和协议参数错误 | 4 | 5.00% | 请求本身不合法或上游拒绝参数 | 修复客户端/转换器请求构造；不要把这类错误计入号池健康度 |

### 4.1 时间集中度

按北京时间小时统计，失败在 `13:00–14:59` 明显集中：

| 小时 | 请求 | 失败 | 失败率 |
| --- | ---: | ---: | ---: |
| 13:00–13:59 | 3,672 | 20 | 0.5447% |
| 14:00–14:59 | 6,117 | 28 | 0.4577% |

这两个小时合计 48 个失败，占本次 80 个失败的 60.00%；其中代理 CONNECT、候选池和流停滞错误共同上升。该现象支持“高峰/路径容量问题”的判断，但不能单凭时间相关性断言唯一根因。

## 5. 重点问题证据

### 5.1 Responses WebSocket 代理 CONNECT 失败

- 25 条原始错误中，24 条使用 key 前缀 `9c7f126e`，1 条使用 `9f8d9eb0`。
- 两个 key 的代理配置都指向节点 `89ef316d`（名称 `netcup-ipv6`，区域 `US-IAD`）。数据库中的节点状态为 `online`，但“online”只表示控制面状态，不能证明每次 CONNECT 都成功。
- 线上日志将该类归类为 `codex_ws_handshake_proxy_connect_error`，安全诊断字段为 `url_kind=proxy_connect`。当前生产版本没有保留足够的上游 HTTP 状态/原因，无法从历史记录进一步判断是 CONNECT 拒绝、DNS、IPv6 路由还是瞬时连接失败。
- 结论：这是**代理/网络路径的间歇性握手失败**，不是账号额度耗尽。应先处理代理节点和 key 路径，而不是先刷新额度或重建用户会话。

候选轨迹补充了最终 `usage` 行没有直接展示的信息：

- 固定快照窗口内实际有 37 个候选记录为 `codex_ws_handshake_proxy_connect_error`，其中 35 个使用 `9c7f126e`，2 个使用 `9f8d9eb0`。
- 这 37 次代理失败最终造成 32 个用户请求失败：25 个保留为 `official_ws_proxy_connect_failed`，另外 7 个在尝试备用账号时因 sticky 状态尚未释放而变成 `candidate_pool_busy`。只有 5 个请求成功切换到备用账号。
- `9c7f126e` 的 35 次失败从候选创建到结束只用了 1–29 ms，而且相同分钟仍有大量成功请求。这不符合节点整体离线或 30 秒连接超时的形态，更符合 SOCKS5 服务即时返回失败 reply 的间歇性故障。生产版本只保存了泛化后的 `url_kind=proxy_connect`，历史 reply code 已无法恢复。
- `netcup-ipv6` 实际是无认证的手动 `socks5h` 节点，不是 Aether tunnel，因此没有 tunnel 心跳和 `proxy_node_metrics_1m` 指标。不能用 `tunnel_connected=false` 判断它离线。
- 2026-08-28 16:32 CST 通过同一代理对 `chatgpt.com/backend-api/codex/responses` 做了 10 次无认证 GET 探测，10 次 CONNECT/TLS 均成功，上游均返回预期的 HTTP 405；TLS 建立耗时约 113–130 ms。这说明节点调查时已恢复，但不能否定高峰期的间歇失败。

源码处置（已进入 `backend-v0.7.90`，但未产出镜像、未上线）：

- 在 WebSocket 请求尚未发送的代理隧道阶段，对 `ProxyConnect`/代理 I/O 做一次 25 ms 退避后的有界重试；代理连接超时也按非账号故障处理。该阶段没有 Provider write，不会引入重复生成风险。重试后成功建立 WebSocket 时会记录 `codex_ws_proxy_tunnel_retry`，包含 request/candidate/provider/key、代理节点 ID、去除凭据和路径的代理端点以及首错类型；重试仍失败则沿用最终握手失败记录。
- 重试仍失败时，已有 sticky 绑定不会切号；尚未建立正式绑定的首次请求可继续使用备用候选。代理持续故障时不通过破坏已有会话绑定来掩盖问题，应单独做代理节点健康摘除或同账号换代理路由。
- 将 SOCKS5 reply code `1–8` 映射为有限的脱敏原因，例如 `proxy_connect_reason=socks5_host_unreachable`，并补齐 HTTP CONNECT 响应格式、SOCKS5 认证/地址等有限分类。未知文本仍只保存为 `url_kind=proxy_connect`，不会记录代理 URL 或凭据。
- `baa1c013f` 的原始行为对所有握手失败都先释放 sticky，这会把传输瞬时故障误当成账号故障；`backend-v0.7.90` 已按失败性质和绑定状态拆分：已有 sticky 遇到非账号握手失败时保留绑定并停止切号；首次初始化的候选可在 Provider write 前继续回退；明确账号拒绝才清除 sticky 并计入账号健康。后续工作区又补齐了 `400/423` 等依赖响应正文的账号失效分类。该规则同时适用于 Codex 直连、代理和 Standard Responses WebSocket。

### 5.2 `059c8d0e`：600 秒没有客户端可见内容

请求详情：

| 字段 | 值 |
| --- | --- |
| request id | `059c8d0e-027f-44e4-8465-b0fc068b82f3` |
| 开始时间 | 2026-08-28 14:21:23 CST |
| client → provider | `claude:messages` → `openai:responses` |
| 模型 | `gpt-5.6-sol` |
| 首字节 | 1,961 ms |
| 总时长 | 602,019 ms |
| key | `9c7f126e` |
| 代理 | `netcup-ipv6` |
| 结果 | HTTP 504，`stream_progress_timeout` |

每分钟的运行日志显示 provider 字节数仍在增长，但 client-visible business bytes 停留在 284；因此更像是上游持续发送控制/非公开事件，而不是客户端真正收到新的业务内容。该请求未保存原始流事件/响应正文（仅保留脱敏元数据），**无法证明每个字节的具体事件类型，也不能完全排除转换器在某个事件上的丢弃**。

本快照内 14 条同类超时全部是 `gpt-5.6-sol`，且转换路径全部为 `claude:messages` → `openai:responses`。这是明确的样本集中度，但尚不能据此证明模型或转换器是唯一根因。

当前代码的相关语义：

- 生产 `backend-v0.7.83` 与最新已打 tag 的 `backend-v0.7.90` 默认 upstream idle 都是 300 秒；tag 内的 `client_progress_idle` 为其 2 倍，即 600 秒。中间的 `backend-v0.7.86`–`backend-v0.7.88` 曾短暂使用 120/240 秒默认值，但未上线。后续工作区保留 upstream 300 秒兼容值，并为未显式配置 `stream_idle_timeout` 的请求设置独立的 120 秒客户端可见进度上限。
- `stream_execution_client_progress_idle_timeout` 在 600 秒触发时写入 `stream_progress_timeout` 和上述原始错误。
- SSE 控制块/keepalive 可用于维持传输连接，但 `client-visible` 计时器会过滤控制块；控制帧不能冒充业务进度，也不能掩盖永久停滞。
- `backend-v0.7.90` 没有改变默认 600 秒 client-progress 上限；后续工作区会在 120 秒终止未配置专用值的同类停滞。它缩短等待时间，但仍不能替代最后公开事件/转换阶段观测，也不能承诺消除上游或转换器停滞本身。

后续应优先补充低成本、脱敏的观测字段：最后一个上游 frame 分类、最后一个公开 Responses 事件、最后一个客户端可见事件、转换器是否产出，以及代理/连接 ID。不要默认保存完整 prompt 或 token。

#### 5.2.1 2026-08-29 同类实时样本

请求 `c05335cd-8bb8-4345-87a9-4eb348338ef4` 再次复现了同一路径：`gpt-5.6-sol`、`claude:messages` → `openai:responses`、本地流转换器启用。客户端可见输出在流开始约 4.2 秒后停在 814 字节，但上游仍持续发送数据；到约 303 秒时累计上游字节已超过 4.65 MB。请求最终在 316.6 秒由上游以 `400 upstream_error / stream_read_error` 结束，未产生 token 或费用。

这个样本确认：HTTP 200 和首字节成功不代表转换后的客户端流仍有进展；持续增长的上游字节也不能作为用户可见进度。它不是额度、Sticky 或候选池故障。现有脱敏捕获无法恢复全部上游事件类型，因此不能仅凭字节增长断言是上游控制事件还是转换器丢弃；120 秒客户端可见进度上限用于先控制用户等待和资源占用，事件级根因仍需后续观测字段验证。后续工作区同时把 HTTP 200 流内的 `upstream_error / stream_read_error` 从误导性的客户端 400 归类改为 502 上游失败。

### 5.3 chunked body EOF

11 条错误完全相同：

```text
error decoding response body: error reading a body from connection: unexpected EOF during chunk size line
```

其中 6 条可追溯到配置了 `netcup-ipv6` 的 key，另外 5 条没有该代理配置。它们的响应头阶段状态为 200，正文阶段被提前截断，因此应按**传输完整性/上游连接关闭**处理，而不是按额度或 HTTP 业务错误处理。

### 5.4 候选池、绑定和本地容量

本次 15 条属于“Provider 写入前”的候选准备/容量问题：

- `candidate_pool_busy`：7
- `bound_account_ineligible`：3
- `bound_provider_changed`：2
- `large_frame_cpu_unavailable`：3

这些错误的共同点是系统可以证明尚未执行 Provider write。账号/绑定状态确实不可用时可以换候选；非账号握手失败不会降低账号健康，已有 sticky 时保留绑定，尚未正式绑定时仍可安全尝试备用候选。`backend-v0.7.85`（`baa1c013f`）加强了 Codex failover settlement 和候选清理，但原始范围过宽；`backend-v0.7.90` 源码已按上述边界收窄，需要成功发布后按同一错误文本复核。

## 6. 与额度/429 问题的边界

此前调查的问题是：官方返回 429 后，号池中的额度状态没有及时更新为耗尽，导致同一账号可能继续被选中，用户表现为无法请求、重开会话也无法请求。

- 该问题的修复提交包括 `13948a5d`（`fix(codex): refresh quota after upstream 429`），并包含在尚未上线的 `backend-v0.7.89`/`backend-v0.7.90` 源码中。
- 本次快照中 Codex Pro 请求的 `401/402/403/429` 数量均为 **0**。
- 因此本次 80 个失败不能归因于额度状态不同步；把代理 CONNECT、EOF 或 600 秒流停滞错误标记为“额度耗尽”会误导处置。

上线后仍应单独监控：官方 429 → 账号状态刷新 → 候选调度是否停止继续使用该账号；该指标不能与本文的网络失败率合并。

## 7. 版本与发布状态

### 7.1 生产与待发布代码

| 项目 | 状态 |
| --- | --- |
| 生产 | `backend-v0.7.83`，`aether-app` healthy |
| 最新已打 tag | `backend-v0.7.90`，commit `6649345949e6a2d982a7c380b2099c466352aacb` |
| P0 代理 CONNECT 修复 | 已进入 `backend-v0.7.90` 源码；该 tag 构建失败，无镜像，未上线 |
| 主要关联修复 | 429 后刷新额度；Codex WS failover settlement；流超时字段/兼容性修订 |
| Release Actions | run `33241849251` 失败，Rust release tests 退出码 101 |
| 产物 | 未生成可确认的新生产镜像/摘要 |

因此本文的线上统计仍对应 `backend-v0.7.83`，不能把待发布源码行为当成生产现状。Release run 链接：<https://github.com/Shirtiny/Aether/actions/runs/33241849251>。

### 7.2 当前不应做的处理

- 不要仅因为 503/504 上升就批量把账号标记为额度耗尽。
- 不要对已发生 Provider write 但结果未知的请求自动换账号重放，避免重复扣费或重复执行。
- 不要只提高 600 秒超时来掩盖控制帧/业务事件缺失；这会把用户等待时间继续拉长。
- 本次调查未执行生产更新、重启、拉取镜像、数据库写入或迁移。

## 8. 建议处置顺序

### 立即（P0）

1. 对 `netcup-ipv6` 按 5 分钟窗口统计 CONNECT 失败率、DNS/IPv6 错误和建立耗时；必要时依据明确阈值临时降低该节点权重/停止新候选（需另行授权生产变更）。
2. 对 `official_ws_proxy_connect_failed`、`candidate_pool_busy`、`bound_*`、`large_frame_cpu_unavailable` 建立按 provider/key/node 的计数和告警，不做模糊的“账号失败”合并。
3. 为 `stream_progress_timeout` 增加请求级告警和最后公开事件摘要，优先复盘 `gpt-5.6-sol + claude:messages`。

### 短期（P1）

1. 在不记录敏感正文的前提下，补充 WS handshake 阶段、代理 CONNECT 状态、EOF 所在阶段和上游 frame 分类。
2. 复核 v0.7.85/v0.7.89 的候选清理、绑定 generation 和 429 状态刷新；以本文原始错误文本做前后版本对比。
3. 对 400 输入错误单独反馈客户端/转换器，不纳入代理节点健康度和额度健康度。

### 验收指标

- `official_ws_proxy_connect_failed`：按节点和 key 分开，不能只看全局平均。
- `stream_progress_timeout`：失败次数、P95/P99 等待时长、模型/客户端族分布。
- EOF：按是否经过代理分层，确认代理路径是否显著高于直连。
- 429：官方响应、账号状态刷新、后续调度停用三者均有可关联事件。

## 9. 只读复核查询（示例）

以下查询只读 `public.usage`，时间边界按本文快照填写；生产执行前应确认数据库连接目标。`usage` 生命周期状态会原地更新，因此第一段只能查看该创建时间窗口的**当前状态**，不能事后精确重放当时的 `pending/streaming` 分布。第二段用 `finalized_at` 排除快照结束后才失败的请求，可复核本文的失败原因集合。

```sql
WITH base AS (
  SELECT *
  FROM public.usage
  WHERE provider_id = '3c7ab28d-b82a-4633-81d0-c524b9fbc7ed'
    AND created_at >= TIMESTAMPTZ '2026-08-27 16:00:00+00'
    AND created_at <  TIMESTAMPTZ '2026-08-28 07:27:00+00'
)
SELECT status, count(*)
FROM base
GROUP BY status
ORDER BY status;

WITH base AS (
  SELECT *
  FROM public.usage
  WHERE provider_id = '3c7ab28d-b82a-4633-81d0-c524b9fbc7ed'
    AND created_at >= TIMESTAMPTZ '2026-08-27 16:00:00+00'
    AND created_at <  TIMESTAMPTZ '2026-08-28 07:27:00+00'
    AND finalized_at < TIMESTAMPTZ '2026-08-28 07:27:00+00'
)
SELECT count(*) AS failures, error_message
FROM base
WHERE status = 'failed'
GROUP BY error_message
ORDER BY failures DESC, error_message;
```

## 10. 证据边界

- 本文能确认的是“数据库中记录的最终错误”和关联的 key/代理/模型维度；它不等于每个上游网络事件的完整 packet-level 根因。
- 代理节点显示 `online` 不能证明 CONNECT、DNS、IPv6 或上游 TLS 每次都正常。
- `provider bytes` 增长而 `client-visible bytes` 不增长支持“控制帧/非业务事件或转换丢弃”的判断，但由于该请求未保存原始事件，结论标为推断。
- 所有百分比都以本文固定快照为分母；不要把后续实时面板数字直接与本文百分比拼接。

## 11. 关联资料

- [流式 SSE idle / 重连调查（2026-08-03）](../architecture/codex-sse-idle-timeout-reconnect-investigation-2026-08-03.md)
- [Responses WebSocket 多 Provider 实施计划](../architecture/responses-websocket-multi-provider-implementation-plan-2026-08-19.md)
- [Release And Container Update Spec](./release-and-container-update-spec.md)
- `1c3348ab8`：初始流生命周期边界和 Codex Responses keepalive
- `baa1c013f`：Codex WS failover settlement
- `a37b02bcb`：流 stalled response 观测/超时字段
- `13948a5df`：官方 429 后刷新额度
- `f6849a890`：`backend-v0.7.89` 待发布整合提交
