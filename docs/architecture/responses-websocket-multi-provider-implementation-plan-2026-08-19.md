# Responses WebSocket 多 Provider 实施计划

## 1. 背景

当前 `custom` 分支已经实现 Codex 官方原生 WebSocket，但实现集中在
`apps/aether-gateway/src/codex_ws`，协议解析、连接管理、Codex 身份约束、账号池调度、
用量结算和 sub2api 控制协议彼此耦合，因此无法直接支持其他原生 Responses WebSocket
Provider。

上游 `main` 分支已经具备通用 Responses WebSocket 会话、Provider adapter、物理连接
binding、终态观察和 Standard/Codex 两种 adapter。由于 `custom` 与 `main` 的 planner、
contract 和 usage 路径已经长期分叉，本次不直接 cherry-pick `main` 的完整实现，而是在
`custom` 现有调度和结算体系上按行为移植通用 WS 内核。

本次改造作为一个完整功能交付，不设计双栈迁移、分阶段发布或灰度测试。

## 2. 业务边界

链路保持为：

```text
用户侧 Codex/Responses 客户端
  -> sub2api
  -> Aether GET /v1/responses (route-v1)
  -> Provider 原生 Responses WebSocket
```

系统职责保持明确分离：

- sub2api 负责用户身份、套餐、订阅、余额、用户侧计费和用户请求记录；
- Aether 负责 Provider/Key 账号池、凭据、路由、并发限制、上游 WS 连接、Provider
  用量和账号侧结算；
- Aether 不接管 sub2api 的用户订阅和余额逻辑；
- sub2api 不判断 Provider 账号健康、上游连接复用或账号侧额度状态。

## 3. 目标范围

### 3.1 包含范围

- 保留 `/v1/responses` 的 HTTP POST 行为，同时为 GET 提供原生 Responses WS；
- 支持 Codex 官方原生 WS；
- 支持显式启用的 OpenAI Responses WS 兼容 Provider；
- 支持 Provider adapter 扩展，而不在共享 session 中写 Provider 类型分支；
- 保留 sub2api `route-v1` 控制协议和 step fence；
- 支持连接复用、continuation binding、完整终态识别和 Provider usage 结算；
- 支持直接事件和 `chunks` 批量事件；
- 对未知合法 Responses 事件和字段透明转发。

### 3.2 不包含范围

- OpenAI Realtime API；
- Chat Completions WebSocket；
- 将 HTTP/SSE 转换成伪原生 WS；
- Aether 用户订阅或用户余额管理；
- Provider write 结果不确定后的自动重放；
- 任意 Provider 私有 WS 协议的自动兼容；
- 生产部署、生产配置修改和生产服务重启。

## 4. 核心语义

### 4.1 `route-v1`

`route-v1` 是 sub2api 与 Aether 之间的私有执行控制协议，不是 Provider 协议。它必须
与上游 Provider adapter 解耦，并继续包含：

- Upgrade 能力协商；
- `sub2api_step_correlation_id`；
- `sub2api_binding_epoch_id`；
- `sub2api_binding_generation`；
- `client_reconnect`；
- `close_after_terminal`；
- `proven_not_executed`；
- `execution_unknown`。

Wire 版本继续使用 `route-v1`，本次不创建 `route-v2`。现有 Codex 客户端与 sub2api
无需因为内部模块重构改变控制消息格式。

### 4.2 执行边界

Aether 必须在每个 step 上维护明确的 Provider write 状态：

```rust
enum ProviderExecutionDisposition {
    ProvenNotExecuted,
    ExecutionUnknown,
    ProviderTerminal,
}
```

- Provider write 开始前的本地校验、调度、连接或 readiness 失败，可以证明未执行；
- 一旦尝试向 Provider 写入，若没有收到可信终态，执行结果必须视为未知；
- 执行结果未知时不得把同一 step 自动投递给其他 Key 或 Provider；
- 收到可信 Provider 终态后，Provider 结算不受下游投递成功与否影响；
- route-control 必须携带原始 step fence，以便 sub2api 关联对应的用户请求。

### 4.3 Turn 与 Binding

- 同一连接只允许一个 in-flight `response.create`；
- 携带非空 `previous_response_id` 的 continuation 必须使用创建该 response 的同一物理
  binding；
- continuation 不得切换 Provider、Endpoint、Key、上游 URL、认证凭据、代理或
  transport profile；
- 不携带 `previous_response_id` 的独立 turn 可以重新规划；
- 若新规划的物理 binding 与当前 binding 相同，则复用连接；否则先建立新连接，再关闭
  旧上游连接，下游 WS 保持打开；
- Codex adapter 继续校验其官方 session/thread 身份和模型约束；这些约束不施加给
  Standard adapter。

## 5. 模块落点

为避免在长期分叉的 `custom` 分支中同时迁移目录和改变协议行为，实现保留现有
`apps/aether-gateway/src/codex_ws` 作为唯一 Responses WS 引擎，不创建并行 session。
Provider 无关能力在原模块中泛化，Standard 上游 transport 独立成文件：

```text
apps/aether-gateway/src/codex_ws/
  ingress.rs
  session.rs
  runtime.rs
  protocol.rs
  standard_transport.rs
  hot_state.rs
  candidate_lifecycle.rs
```

模块职责：

| 模块 | 职责 |
|---|---|
| `ingress` | Upgrade、Aether API Key、IP 规则、敏感请求头清理和连接准入 |
| `standard_transport` | Standard URL、握手头、代理、transport profile 和有界帧桥接 |
| `session` | 下游生命周期、turn 调度、binding 复用/替换、relay 和投递 |
| `runtime` | 候选规划、adapter 决策、上游连接、请求物化和 Provider 结算 |
| `protocol` | `route-v1`、step fence、帧分类、`chunks`、终态和 relay directive |
| `hot_state` | Provider/Endpoint/Key generation 与执行 fence |
| `candidate_lifecycle` | Provider attempt 生命周期和有界结算 |

以下现有能力继续由共享引擎复用：

- hot-state；
- catalog fence；
- Codex 账号级 capability；
- 固定 TLS/transport profile；
- candidate lifecycle；
- CPU budget；
- bounded usage/settlement reporter。

GET `/v1/responses` 改用通用命名的入口；route-v1 wire contract 与已有 Codex 入口保持
兼容。内部保留少量 `codex_ws` 类型名属于兼容性和低风险改造，不代表存在第二套引擎。

## 6. Provider Adapter

共享 session 只处理标准 Responses WS 状态，不直接判断 Provider 类型。当前仅有 Standard 与
Codex 两种封闭策略，使用 `ResponsesWebSocketAdapter` enum 和配套策略函数，以便编译器检查
所有分支；新增第三种私有协议时再提升为 trait。逻辑 adapter contract 包含：

```rust
trait ResponsesWebSocketAdapter: Send + Sync {
    fn kind(&self) -> ResponsesWebSocketAdapterKind;
    fn build_upstream_request(&self, decision: &AiExecutionDecision)
        -> Result<UpstreamWebSocketRequest, AdapterError>;
    fn normalize_response_create(
        &self,
        decision: &AiExecutionDecision,
        client_event: &serde_json::Value,
    ) -> Result<bytes::Bytes, AdapterError>;
    fn observe_upstream_event(
        &self,
        event: &serde_json::Value,
    ) -> AdapterObservation;
    fn relay_directive(
        &self,
        frame: &ParsedResponsesWebSocketFrame,
    ) -> RelayDirective;
}
```

### 6.1 Standard Responses Adapter

- 使用 planner 生成的 Provider URL、认证头、代理和请求 body；
- 仅允许 Provider API 格式为 `openai:responses`；
- 保留客户端明确提供的 `store`、`previous_response_id` 和 `generate`；
- 移除只属于 HTTP transport 的 `stream` 和 `background`；
- 默认原样转发合法上游事件；
- 不解释 Codex 私有 quota 或 metadata 事件；
- 不声明任何 write 后的 replay-safe 行为。

### 6.2 Codex Official Adapter

- 复用现有 `aether_codex_ws_connector`；
- 仅接受 Codex OAuth Key 和官方 Endpoint；
- 继续使用固定、不可替换的 Codex transport profile；
- 继续执行官方 session/thread、模型和 continuation 校验；
- 识别并过滤明确的 Codex 私有批量事件；
- 解析 Codex quota/rate-limit 元数据并同步账号状态；
- 使用当前 hot-state、catalog/key generation 和 lease 校验；
- 不将 Codex 专用字段或限制泄漏到 Standard adapter。

## 7. 帧与终态处理

每个上游文本 frame 只解析一次，同时保留原始字节。解析结果至少包含：

- 原始 frame；
- 根事件；
- `chunks` 中按顺序排列的事件；
- 是否包含公开 Responses 事件；
- Provider 终态摘要；
- usage 信息；
- adapter observation。

必须识别以下终态：

| 事件 | 默认语义 |
|---|---|
| `response.completed` | Provider 正常完成 |
| `response.incomplete` | 有合法 reason 且无显式错误时为正常终态 |
| `response.cancelled` | Provider 取消，状态 499 |
| `response.failed` | Provider 失败 |
| `error` | Provider 错误 |

未知合法事件和未知字段不得因为 Aether 的本地强类型 schema 不认识而产生 502。直接标准
frame 应按原始字节转发；只有 adapter 明确识别的私有 envelope 才允许拆包、过滤或重新序列化。

无效帧日志继续使用当前捕获机制，但日志内容必须受统一开关、大小边界和敏感信息策略控制，
不得把 Provider 凭据或下游认证头写入日志。

## 8. 物理 Binding Identity

物理连接身份至少包含：

- adapter kind；
- Provider ID；
- Endpoint ID；
- Key ID；
- 规范化上游 WS URL；
- 非 turn 级握手头；
- 不可逆的凭据代次或凭据指纹；
- 代理配置；
- transport profile。

身份比较必须使用实际用于建立连接的规范化值。日志只记录发生变化的字段名，不记录认证头
值、凭据指纹原值或代理密码。

连接保存有限数量的 `response_id -> binding` 所属关系。缓存必须有条目数和总字节上限，防止
长连接通过 response ID 无限增长内存。

## 9. Planner 与配置

Provider 配置增加：

```json
{
  "responses_websocket": {
    "enabled": true
  }
}
```

规则如下：

- 缺少配置或 `enabled=false` 时不进入通用 WS 候选集；
- 配置必须由管理 API 校验为 JSON object 和 boolean；
- Endpoint 必须启用并声明 `openai:responses`；
- Key、模型、认证通道、配额、熔断、代理和并发仍使用 `custom` 现有 planner 规则；
- WS URL 必须通过结构化 URL API 从 Provider Endpoint 解析，不使用字符串替换；
- Standard provider 使用 Provider 级开关；
- Codex 除 Provider 级开关外，仍要求 Key 的 `codex_official_ws=true` 和有效 transport
  profile；
- HTTP/SSE 候选和行为不因 WS 开关改变。

管理 API、Provider summary、导入导出类型和前端 Provider 表单同步增加该字段。

## 10. Settlement 与计费

结算必须区分两个正交事实：

```rust
struct AttemptTerminalFacts {
    provider: AttemptProviderOutcome,
    delivery: AttemptClientDelivery,
}
```

- `provider` 表示 Provider 是否完成、失败、取消或状态未知；
- `delivery` 表示终态是否成功写回 sub2api；
- Provider 已经完成但下游断开时，不能将 Provider usage 记为 void；
- 下游已经收到部分输出但 Provider 未给终态时，不能伪造成功终态；
- 同一 step 的 Provider usage 最多结算一次；
- Aether 的 usage/report context 记录 Provider、Endpoint、Key、模型、token 和账号侧效果；
- sub2api 通过标准 Responses 事件、usage 和 route-control 完成用户侧账务处理；
- Aether 不读取或修改 sub2api 用户余额。

## 11. 错误和重试策略

统一策略如下：

| 失败位置 | Aether 行为 | sub2api 可否重试 |
|---|---|---|
| Provider write 前 | 返回 `proven_not_executed` 并关闭当前 binding | 可以 |
| Provider write 已尝试、无终态 | 返回 `execution_unknown` 并关闭 | 不可自动重放 |
| Provider 明确终态 | 转发终态并结算 | 按终态处理 |
| 配置/账号软排空 | 当前 turn 完成后发送 `close_after_terminal` | 下一 turn 重连 |
| continuation binding 失效 | 拒绝当前 step，不切换 Provider | 由 sub2api 决定后续动作 |

本次不引入 `main` 中 Codex quota 触发的同连接透明跨 Key 重试。未来若需要此能力，必须先
证明 Provider 没有创建公开 response 状态，并单独修改 route-control 契约。

## 12. 安全与资源边界

- Upgrade 继续执行 Aether API Key、访问状态和 IP 规则校验；
- 下游 Authorization、API Key、Cookie、Host 和所有 WS hop-by-hop header 不得进入
  Provider planner 或上游握手；
- Provider 认证仅由 Aether planner 生成；
- frame/message、首帧、首事件、read idle、turn total 和写入操作必须有硬上限；
- 同一连接只允许一个 in-flight turn；
- response ID、model、control ID 和 metadata 必须限制长度；
- 大帧 JSON 处理继续受 CPU permit 约束；
- usage、settlement 和清理任务继续使用有界队列；
- 原始帧捕获不得记录下游或 Provider 认证信息。

## 13. 实现工作清单

- [x] 将现有唯一 WS ingress/session 泛化为 Standard/Codex 共享引擎；
- [x] 支持原始帧、`chunks`、未知 `response.*` 事件和完整终态；
- [x] 保留 `route-v1`、step fence 和 write 前后执行状态语义；
- [x] 实现完整 `UpstreamBindingIdentity` 和有界 response ownership；
- [x] 实现独立 turn 重规划、物理 binding 复用和无断流替换；
- [x] 保证 Provider 终态先结算，再独立尝试下游投递；
- [x] 实现 Standard adapter 策略和独立上游 transport；
- [x] 保留 Codex connector、身份约束、quota 和 hot-state 策略；
- [x] 在 `custom` 现有 planner 中增加 WS candidate 决策，不移植 `main` 整套 planner；
- [x] 增加 Provider `responses_websocket.enabled` 的后端配置和校验；
- [x] 增加管理端 Provider WS 开关和相关 API 类型；
- [x] 将 `/v1/responses` GET 切换到通用 Responses WS 入口；
- [x] 更新 `docs/codex-websocket.md` 为 Standard/Codex adapter 运行说明；
- [x] 增加并通过合入前自动化验证。

## 14. 合入前验收

验收属于代码正确性检查，不包含生产灰度或部署：

- `cargo check` 和相关 Rust 单元测试通过；
- 前端类型检查和 Provider 表单测试通过；
- 当前 Codex route-v1 握手与控制事件保持兼容；
- Codex 连续两轮 continuation 使用同一 binding 且分别结算；
- Standard provider 可以建立 WS、转发响应并结算 usage；
- 独立 turn 可根据新规划替换上游 binding；
- continuation 不允许跨 binding；
- `chunks` 中的业务事件、usage 和 terminal 均按顺序处理；
- 未知请求字段和未知响应字段完整保留；
- `completed`、合法 `incomplete`、`cancelled`、`failed` 和 `error` 均正确结算；
- Provider write 前失败产生可验证的 `proven_not_executed`；
- Provider write 后断开产生 `execution_unknown`，且不重放；
- Provider 完成后下游写失败仍保留 Provider 结算；
- 下游凭据和握手头不会到达上游；
- Provider WS 开关关闭时只影响 WS，不影响 HTTP/SSE；
- 日志中不出现认证头、访问令牌或代理密码。

## 15. 完成定义

完成后仓库只存在一套 `/v1/responses` 原生 WebSocket 引擎。该引擎通过 adapter 同时
支持 Codex 官方和 Standard Responses Provider，通过 `route-v1` 与 sub2api 保持明确的
执行及计费边界，并继续使用 `custom` 现有 Provider 账号池、调度和用量结算基础设施。
