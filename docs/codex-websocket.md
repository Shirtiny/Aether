# Codex 官方 WebSocket

本文档说明 Aether 的 Codex 官方原生 WebSocket 功能、启用条件、账号控制、
`route-v1` 协议、TLS/profile 约束、调度行为、性能参数和上线检查。

> 状态：代码已实现，但所有全局开关和账号开关默认关闭。部署新版本不会自动启用
> Codex WS，也不代表真实凭据、TLS 抓取、多实例或负载门禁已经通过。

## 1. 功能范围

支持的链路：

```text
Codex 客户端 WebSocket
  -> sub2api 账号级 WebSocket 入口
  -> Aether GET /v1/responses（route-v1）
  -> Aether Codex OAuth 号池调度
  -> wss://chatgpt.com/backend-api/codex/responses
```

本功能只支持：

- Provider 类型为 `codex`；
- Key 认证类型为 `oauth`；
- Provider API 格式为 `openai:responses`；
- Codex 官方 `chatgpt.com/backend-api/codex` Endpoint；
- 固定、不可替换的 Codex Rustls WebSocket profile。

明确不支持：

- 其他 Provider 的原生 WebSocket；
- API Key 类型的 Codex 官方账号；
- 任意 OpenAI-compatible 自定义上游复用本 profile；
- 浏览器/uTLS/Chrome JA3 或 JA4 模拟；
- Provider 已经可能执行当前 step 后的自动重放；
- 把 HTTP/SSE 响应重新解析成 Aether 原生 WS。

不满足原生 WS 条件的普通 HTTP/SSE 路由保持原有行为。关闭某个 Codex Key 的
账号级 WS 不会关闭该 Key 的 HTTP 调度。

## 2. 入口和 `route-v1` 协议

### 2.1 WebSocket Upgrade

Aether 在原有 Responses 路径上同时提供 HTTP POST 和 WS GET：

```http
GET /v1/responses
Authorization: Bearer <dedicated-aether-api-key>
Connection: Upgrade
Upgrade: websocket
x-aether-ws-control-accept: route-v1
```

缺少或重复 `x-aether-ws-control-accept`、值不是精确的 `route-v1`，均以
`412 Precondition Failed` 拒绝。成功握手响应包含：

```http
x-aether-ws-control: route-v1
x-aether-ws-capabilities: close-after-terminal,client-reconnect
```

入口仍执行 Aether API Key、访问状态和 IP 规则校验。建议 sub2api 使用专用的
Aether API Key，并把该 Key 绑定到只包含目标 Codex OAuth 号池的组。

### 2.2 首帧与大小限制

- Upgrade 后 10 秒内必须收到第一个 `response.create`；
- 公开请求 payload 上限为 16 MiB；
- Aether route fence 最多增加 4 KiB；
- 客户端最大 WS message 为 16 MiB + 4 KiB；
- 官方上游最大 frame 为 16 MiB、最大 message 为 64 MiB；
- 同一连接同一时间只允许一个 in-flight step。

sub2api 发给 Aether 的每个 `response.create` 必须携带稳定身份和 fence 元数据，
包括：

```text
session_id
thread_id
sub2api_step_correlation_id
sub2api_binding_epoch_id
sub2api_binding_generation
```

这些字段用于识别逻辑会话、阻止旧 binding 重放，并把控制消息关联到唯一 step。
不得用 prompt 内容代替会话身份。

### 2.3 控制事件

Aether 使用 `aether.route_control` 事件表达两种动作：

- `close_after_terminal`：当前 terminal 交付后关闭当前 binding，下一次重新选择；
- `client_reconnect`：仅在 Aether 能证明当前 step 尚未写入官方 Provider 时，要求
  客户端立即重连并迁移。

未执行证明为：

```text
adapter_proof_class=codex_official_ws.not_executed
adapter_proof_version=1
```

一旦 Provider write 的结果不确定，Aether 不会把当前 step 自动投递到另一个账号。
这条限制用于避免重复执行和重复计费。

## 3. 全局开关

系统配置 key 为 `codex_ws`，两个布尔值默认均为 `false`：

```http
PUT /api/admin/system/configs/codex_ws
Content-Type: application/json

{
  "value": {
    "enabled": true,
    "native_codex_ws_enabled": true
  },
  "description": "Official Codex native WebSocket"
}
```

只有两个值都为 `true` 才开放入口和官方原生 Connector。管理 API 写入成功后会
立即更新进程内原子快照，不要求重启。每次限制性变更都会推进 generation；已保留
连接在后续执行 fence 发现 generation 改变后失败关闭，不能继续使用旧配置写上游。

紧急关闭全局功能：

```http
PUT /api/admin/system/configs/codex_ws
Content-Type: application/json

{
  "value": {
    "enabled": false,
    "native_codex_ws_enabled": false
  },
  "description": "Official Codex native WebSocket disabled"
}
```

全局开关不是环境变量。`AETHER_CODEX_WS_*` 环境变量只负责容量和 worker 调优，
不能启用功能。

## 4. Codex 账号级控制

### 4.1 静态启用条件

账号能够进入原生 WS 候选集之前必须同时满足：

1. Provider 类型精确为 `codex`，且 Provider 启用；
2. Key 的 `auth_type` 为 `oauth`，且 Key 启用；
3. `capabilities.codex_official_ws=true`；
4. Key 携带完整且精确匹配的 schema-3 transport profile；
5. Endpoint 启用且 API 格式为 `openai:responses`；
6. Endpoint 为 HTTPS、`chatgpt.com:443`；
7. base path 为 `/backend-api/codex`，允许一个结尾 `/`；
8. Endpoint 没有 query、fragment 或自定义 path 覆盖；
9. 正常的模型、配额、熔断、代理和并发检查通过。

`profile_effective=true` 只说明静态条件成立，不代表某次真实请求必然可调度。

### 4.2 管理界面

进入 **Admin -> Pool Management**，找到 Codex OAuth 账号：

- 单个账号使用“启用账号级 Codex WS”；
- 批量操作使用“启用 Codex WS”；
- “关闭账号级 Codex WS”只软排空 WS，不改变 HTTP 调度状态。

管理界面会自动写入固定 profile，操作员不应手工拼装 fingerprint。

### 4.3 单账号 API

启用：

```http
PUT /api/admin/endpoints/keys/{key_id}/codex-ws
Content-Type: application/json

{
  "enabled": true,
  "profile_id": "codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1"
}
```

关闭：

```json
{
  "enabled": false
}
```

启用操作原子合并现有 Key JSON，并写入：

- `capabilities.codex_official_ws=true`；
- `fingerprint.websocket_transport_profile` 的完整固定 manifest。

关闭时写入 `capabilities.codex_official_ws=false`，保留固定 manifest 和其他无关字段。
响应中的关键字段：

| 字段 | 含义 |
|---|---|
| `configured` | 账号级 capability 是否开启 |
| `profile_effective` | 静态 profile 与 Endpoint 是否匹配 |
| `runtime_eligible` | 当前是否已知可运行；缺少具体请求上下文时为 `null` |
| `profile_id` | 当前固定 profile ID |
| `runtime_state` | `request_scoped`、`profile_blocked`、`soft_draining` 或 `hard_revoked` |
| `profile_reasons` | 静态不满足原因 |
| `runtime_reasons` | 配额、代理、模型、熔断、并发等运行期原因 |

### 4.4 批量 API

```http
POST /api/admin/pool/{provider_id}/keys/batch-action
Content-Type: application/json

{
  "key_ids": ["key-1", "key-2"],
  "action": "enable_codex_ws"
}
```

关闭时使用 `disable_codex_ws`。批量操作只处理 Codex OAuth Key；不匹配的账号会跳过
或返回明确错误，不会被转换成其他认证类型。

## 5. 官方 TLS 和 WebSocket profile

固定 profile：

```text
profile_id: codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1
schema_version: 3
codex_commit: 1f0566d3f59298d1bb88820a0d35294f1eeb07ea
tokio_tungstenite_rev: 0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186
tungstenite_rev: 4fffad30fe373adbdcffab9545e9e9bf4f2fc19f
tungstenite_patch_id: aether-tungstenite-0.27-out-buffer-retention-v1
crypto_provider: aws-lc-rs
write_buffer_size: 128 KiB
max_write_buffer_size: 17 MiB
max_retained_write_buffer_capacity: 256 KiB
```

Connector 固定连接：

```text
wss://chatgpt.com/backend-api/codex/responses
OpenAI-Beta: responses_websockets=2026-02-06
```

实现使用 Rustls 0.23 和 AWS-LC，按固定 Codex revision 对齐握手与 WebSocket 行为。
它不声称模拟 Chrome/JA3/JA4。证书和 SNI 校验不能关闭；自定义 CA 只允许沿用
Codex 支持的 `CODEX_CA_CERTIFICATE` 或 `SSL_CERT_FILE` 条件。

profile 中任意 revision、crypto provider 或 buffer 字段不匹配都会 fail closed，
不会降级为“近似 profile”。升级固定 profile 后，旧 schema/profile 账号必须重新
关闭再启用，由管理 API 安装新 manifest。

更详细的 TLS 抓取方法见 [TLS fingerprint capture](operations/tls-fingerprint-capture.md)。

## 6. 调度、切换与 fence

### 6.1 初始选择和 failover

- 候选在写入 Provider 前完成官方 WS 静态过滤；
- 单次初始规划最多保留 16 个候选；
- 连接、鉴权、限流或明确的握手失败可在 **尚未 Provider write** 时排除当前候选，
  尝试下一个账号；
- candidate、provider 和 key 的并发 permit 只覆盖执行 step，不覆盖连接空闲时间；
- request body 不复制到每个候选，最终选中账号只在写入前物化一次。

### 6.2 后续 turn

长连接的每个 turn 在 Provider write 前重新验证：

- 全局配置 generation；
- Provider/Endpoint/Key catalog generation；
- Key capability、活动状态和固定 profile；
- 模型、配额、熔断、代理和并发；
- 当前 binding fence。

限制性账号变更、配额耗尽或候选失效会阻止后续写入。只有存在
`codex_official_ws.not_executed` 证明时才允许发出 `client_reconnect`；否则关闭连接，
由上层决定是否让用户显式重试。

### 6.3 多实例

多实例生产环境必须使用共享 Redis runtime backend，并验证：

- global/catalog/key generation 跨节点一致；
- 限制性变更的 mutation lock/CAS；
- 节点丢失后的 lease 恢复；
- Provider/Key 并发 permit 不会在节点间超卖。

单节点内存测试不能替代多实例 Redis 验证。

## 7. 用量、结算和资源边界

Aether 使用进程级有界队列，不为每个连接或 terminal 创建无界任务：

1. usage report lane；
2. primary settlement lane；
3. slow settlement lane；
4. 大帧 CPU lane。

Provider dispatch 前必须预留 required usage/settlement 容量。队列满时对入口施加背压，
不会生成同步 fallback、无限 goroutine/task 或无限 waiter。candidate lease、sticky renewer、
健康反馈和并发 permit 在 terminal 后先释放，再进行持久化。

队列只保留紧凑 ID、状态、usage 和无 body plan，不保留 prompt、OAuth 凭据、完整
terminal JSON 或 socket context。若产品要求进程崩溃后仍保证 usage 交付，必须另加
durable outbox；当前内存队列本身不提供 crash-surviving 保证。

## 8. 性能环境变量

所有值在进程启动时读取。未设置、空值、非法值或 `0` 使用默认值，并按范围 clamp。

| 环境变量 | 默认值 | 有效范围 | 用途 |
|---|---:|---:|---|
| `AETHER_CODEX_WS_USAGE_REPORT_QUEUE_CAPACITY` | 16384 | 10000..65536 | usage report 队列 |
| `AETHER_CODEX_WS_USAGE_REPORT_WORKERS` | 32 | 1..128 | usage report worker |
| `AETHER_CODEX_WS_SETTLEMENT_QUEUE_CAPACITY` | 16384 | 10000..65536 | primary settlement 队列 |
| `AETHER_CODEX_WS_SETTLEMENT_WORKERS` | 64 | 1..128 | primary settlement worker |
| `AETHER_CODEX_WS_SETTLEMENT_TIMEOUT_MS` | 2000 | 100..10000 | primary settlement 硬超时 |
| `AETHER_CODEX_WS_SLOW_SETTLEMENT_QUEUE_CAPACITY` | 4096 | 128..16384 | slow retry 队列 |
| `AETHER_CODEX_WS_SLOW_SETTLEMENT_WORKERS` | 8 | 1..32 | slow retry worker |
| `AETHER_CODEX_WS_SLOW_SETTLEMENT_TIMEOUT_MS` | 10000 | 500..30000 | slow retry 硬超时 |
| `AETHER_CODEX_WS_LARGE_FRAME_CPU_WORKERS` | CPU/4，最少 1 | 1..64 | 大于 64 KiB 的 CPU worker |
| `AETHER_CODEX_WS_LARGE_FRAME_CPU_ADMISSION_CAPACITY` | workers*4 | workers..256 | 大帧执行加等待总上限 |

大帧普通工作在 admission 满时立即拒绝；已 admission 的工作最多等待 250 ms 获取 CPU
worker。Provider write 在最后一个异步 fence 后同步取得两个 permit，避免等待期间账号
状态变化。不要仅通过扩大队列掩盖存储、CPU 或结算延迟。

### 8.1 默认超时

超时优先使用 Endpoint/ExecutionPlan 已配置值，否则使用：

| 阶段 | 默认值 |
|---|---:|
| connect | 30 秒 |
| write | 30 秒 |
| first business frame | 30 秒 |
| idle read | 120 秒 |
| step total | 30 分钟 |
| 所有初始候选合计 connect budget | 最多 60 秒 |

可能已经写入 Provider 的超时不得触发另一个账号重放。

## 9. 可观测性

排障时建议临时使用：

```text
RUST_LOG=aether_gateway=debug,sqlx=warn
```

重点事件：

```text
codex_ws_handshake_unauthorized
codex_ws_handshake_connection_limit
codex_ws_handshake_rate_limited
codex_ws_handshake_transport_error
codex_ws_connect_timeout
codex_ws_first_byte_timeout
codex_ws_read_timeout
codex_ws_total_timeout
codex_ws_candidate_fence_changed_before_connect
codex_ws_candidate_fence_changed_after_connect
codex_ws_quota_feedback_backpressure
codex_ws_step_settlement_timeout
codex_ws_slow_settlement_enqueue_failed
codex_ws_slow_settlement_timeout
codex_ws_usage_event_build_failed
```

日志可以记录 trace、candidate、step/attempt、binding generation、profile ID、代理拓扑、
terminal 类型、Provider write 状态、queue depth、TTFT 和总耗时。禁止记录 prompt、
OAuth token、Aether API Key、代理凭据、完整 Provider body 或官方账号原始 ID。

管理 API 返回的常见静态原因包括：

```text
global_disabled
native_codex_ws_disabled
provider_type_unsupported
key_auth_type_unsupported
account_capability_disabled
websocket_transport_profile_invalid
official_endpoint_host_unsupported
official_endpoint_path_unsupported
endpoint_api_format_unsupported
```

## 10. 与 sub2api 的对应配置

Aether 两层开关打开后，sub2api 仍需单独启用：

- `gateway.openai_ws.mode_router_v2_enabled=true`；
- `gateway.openai_ws.responses_websockets_v2=true`；
- `gateway.openai_ws.aether_route_control_enabled=true`；
- 对应 Aether API Key 账号的“作为 Aether WS 账号”开关；
- 只有实测官方 reconnect fixture 通过后才启用 reconnect migration。

sub2api 中保存的 Aether base URL仍是本地 HTTP base，例如：

```text
http://aether:8080/v1
```

不要在 base URL 后手工添加 `/responses`。sub2api 会直接派生
`ws://aether:8080/v1/responses`，并要求无压缩、无系统代理的本地直连。

跨仓库的完整设计和调度契约位于 sub2api 仓库：
`docs/CODEX_WEBSOCKET_TRANSPORT_AND_SCHEDULING_PLAN.md`。

## 11. 启用顺序

先在 staging 执行：

1. 部署代码，保持 Aether 全局开关、账号开关和 sub2api route 开关关闭；
2. 验证数据库没有意外 pending migration；
3. 打开 Aether 全局两个 `codex_ws` 开关；
4. 对一个 Codex OAuth Key 启用账号级 WS；
5. 确认 `configured=true`、`profile_effective=true`、
   `runtime_eligible=null`、`runtime_state=request_scoped`；
6. 打开一个 sub2api Aether 账号；
7. 先只打开 route-v1，不打开 reconnect migration；
8. 运行单 turn、多 turn、初始 failover、账号禁用和配额耗尽测试；
9. 真实 reconnect fixture 通过后再打开 migration；
10. 逐步扩大账号，同时观察连接延迟、CPU admission、队列深度、结算延迟和 RSS。

如果必须保证链路始终经过 Aether，sub2api 客户端所属组必须是 Aether-only。混合组允许
调度器在故障时切换到非 Aether 官方账号，这是设计行为。

## 12. 紧急关闭和回退

最小影响顺序：

1. 在 sub2api 关闭单个 Aether 账号，或关闭 `aether_route_control_enabled`；
2. 在 Aether 关闭受影响 Codex Key 的账号级 WS；
3. 关闭 Aether 全局 `codex_ws` 两个开关；
4. 继续使用现有 HTTP/SSE 路由。

上述动态开关不要求数据库 schema 回滚。不要通过删除 Key、清除 OAuth 凭据或修改固定
profile 来紧急止损。若部署版本同时包含其他数据库迁移，应在更新前单独审查；本文档
只描述 Codex WS，本功能自身没有新增 schema migration。

## 13. 生产启用门禁

以下证据缺一项都不应开启生产流量：

- 使用真实官方凭据完成一步、多步和 reconnect fixture；
- 对照固定 Codex revision 完成 direct、HTTP CONNECT、HTTPS CONNECT TLS 抓取；
- 自定义 CA 行为与固定 Codex source 一致；
- 多实例共享 Redis generation、permit、mutation lock 和节点丢失恢复测试；
- PostgreSQL/MySQL 真实集成测试；
- 反向代理 Upgrade、buffering、idle timeout、最大连接年龄验证；
- 1、100、1,000、10,000 连接负载报告；
- 64 KiB、1 MiB、16 MiB 双向 frame 和 buffer retention 证据；
- usage、primary settlement、slow settlement 和大帧 CPU 饱和测试；
- 所有启用账号都是当前 schema-3 profile；
- HTTP/SSE 在所有新开关关闭时回归通过；
- 没有未解决的 P0/P1 Review 问题。

源码单元测试和 focused test 通过不等于生产门禁通过。生产启用、重启、迁移和发布必须
由操作员单独明确授权。
