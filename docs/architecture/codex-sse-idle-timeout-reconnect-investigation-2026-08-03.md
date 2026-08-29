# Codex 客户端「一直在重连」调查记录（SSE idle 超时 / openai:* 心跳）

- 日期：2026-08-03
- 触发工单：Cafecode / Revisit 用户（编号 192）反馈「客户端一直在重连」
- 客户端上报错误：`stream disconnected before completion: idle timeout waiting for sse`
- 结论性质：**本文为客观调查记录，含源码依据；不含已实施的改动，修复方向仅作记录，暂不执行。**

> 说明：文中区分「已证实事实（源码/线上数据）」与「推断」。凡标注〔推断〕者为基于证据的解释，非日志中直接记录的事件。

---

## 1. 系统拓扑

```
Codex Desktop (0.146.0-alpha.9.2)
   → sub2api           (Wei-Shaw/sub2api fork，直面客户端)
   → aether-gateway    (本仓库，号池/上游路由)
   → 官方上游 (chatgpt.com / 各 provider)
```

- 涉事账号：`布韩buihanhdev.52v@gmail.com`（客户专属绑定的单一上游 Codex 账号）。
- 该账号在 aether 的记录：`provider_api_keys.id = 4da4ba36-b7bc-493d-9fbd-48c9e26236e1`
  - 全量统计：`request_count=167367, success_count=164533, error_count=464`，`is_active=t, status=active`，`learned_rpm_limit` 空、`rpm_429_count=0`、`concurrent_429_count=0`（账号本身健康、未被限流）。

---

## 2. 客户端错误的源码判定（Codex）

客户端报错 `stream disconnected before completion: idle timeout waiting for sse` 的两段来源：

- 外层包装：`codex-rs/protocol/src/error.rs:91`
  ```rust
  #[error("stream disconnected before completion: {0}")]
  ```
- 内层原因：`codex-rs/codex-api/src/sse/responses.rs:523-528`
  ```rust
  Err(_) => {
      let _ = tx_event
          .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
          .await;
      return;
  }
  ```

**关键区分**：错误文案是「waiting for **SSE**」而非「waiting for **websocket**」。后者存在于 `codex-rs/codex-api/src/endpoint/responses_websocket.rs:699`（`"idle timeout waiting for websocket"`）。
→ 说明客户端本次走的是 **HTTP SSE** 传输，而非 WebSocket。此点与线上数据一致（见 §7，`openai_ws_mode=false`）。

---

## 3. Codex 的 SSE idle 超时机制（源码）

`codex-rs/codex-api/src/sse/responses.rs:503-528`：

```rust
loop {
    let start = Instant::now();
    let response = timeout(idle_timeout, stream.next()).await;   // :505
    ...
    let sse = match response {
        Ok(Some(Ok(sse))) => sse,
        Ok(Some(Err(e))) => { ... return; }
        Ok(None)          => { ... "stream closed before response.completed" ... return; }
        Err(_)            => { ... "idle timeout waiting for SSE" ... return; }  // :523
    };
    ...
}
```

- idle 计时套在 `stream.next()`（`stream.eventsource()` 之上）。**两个 event 之间静默超过 `idle_timeout` 即触发**。
- `idle_timeout` 取值：`codex-rs/codex-api/src/endpoint/responses.rs:159` 传入 `self.session.provider().stream_idle_timeout`。
- 默认值：`codex-rs/model-provider-info/src/lib.rs:26`
  ```rust
  const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;   // 5 分钟
  ```
  可被 provider 的 `stream_idle_timeout_ms` 覆盖（`lib.rs:319-323`）。

**推论（源码）**：只要流式响应中出现 > `idle_timeout` 的静默 gap，Codex 即断开并（在上层重试逻辑下）重连。

### 3.1 compact 走的是另一套超时（排除项）

`codex-rs/core/src/client.rs`：
- `:161` `const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";`
- `:162-163` 注释：`/responses/compact` is **unary**，超时覆盖整个响应而非单次事件间空闲。
- `:164` `const COMPACT_REQUEST_TIMEOUT_IDLE_MULTIPLIER: u32 = 4;`
- `:630-633` `compact_request_timeout = provider.stream_idle_timeout * 4`（默认 300s × 4 = **1200s**）。

→ compact 是一元调用，超时 1200s（默认），与 `idle timeout waiting for SSE` 无关。**日志中 18–38s 的 compact 首字不构成超时。**

---

## 4. `eventsource-stream` 对注释行的处理（源码，决定"哪种心跳有效"）

Codex 依赖 `eventsource-stream = 0.2.3`（`codex-rs/Cargo.lock`）。其解析逻辑（`src/event_stream.rs`，jpopesculian/eventsource-stream）：

- 注释行处理为空操作：
  ```rust
  RawEventLine::Comment(_) => {}
  ```
- 派发时对空 data 直接返回 None：
  ```rust
  if event.data.is_empty() {
      return None;
  }
  ```

→ 仅含注释的块（如 `": keepalive\n\n"`）data 缓冲为空，`dispatch()` 返回 `None`，**`stream.next()` 不产出任何 item**。

**推论（源码）**：**注释型（`:` 开头）心跳不会重置 Codex 的 idle 计时器**（§3 的 `timeout` 只在 `stream.next()` 产出时重置）。注释仅能维持 TCP/中间代理层的连接，无法阻止 Codex 应用层的 `idle timeout waiting for SSE`。

---

## 5. Codex 对"多余/未知事件"的容错（源码，决定"能否发心跳")

`codex-rs/codex-api/src/sse/responses.rs`：

- 解析失败即跳过、不报错：`:533-537`
  ```rust
  let event: ResponsesStreamEvent = match serde_json::from_str(&sse.data) {
      Ok(event) => event,
      Err(e) => { debug!("Failed to parse SSE event: {e}, data: {}", &sse.data); continue; }
  };
  ```
- 事件结构宽松：`:161-174` `ResponsesStreamEvent` 仅 `type`（`#[serde(rename="type")] kind: String`）必填，其余字段全部 `Option<...>`。
- 未知事件类型静默忽略：`process_responses_event` 末尾 `:467-472`
  ```rust
  _ => { trace!("unhandled responses event: {}", event.kind); }
  // ...
  Ok(None)
  ```

**推论（源码）**：
- 一个带 `data:` 的合法 JSON、`type` 为未知值（如 `{"type":"response.aether_keepalive"}`）→ 反序列化成功 → 落 `_` 分支 → `Ok(None)` → 静默丢弃、不报错、不对用户可见；
- 同时因为 `stream.next()` 产出了 item → **idle 计时被重置**。
- 即便发不可解析的 data → `:535` `continue`，同样不报错、且计时被重置。

→ **Codex 侧不会因"多出来的事件"崩溃；能够接收 aether 生成的心跳，前提是心跳为带 `data:` 的真事件，而非注释。**

---

## 6. aether 侧现状：对 openai:* 不注入流式心跳（源码）

`apps/aether-gateway/src/execution_runtime/stream/execution.rs`：

- 心跳常量：
  - `:121` `const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);`
  - `:122` `const SSE_KEEPALIVE_BYTES: &[u8] = b": aether-keepalive\n\n";`（**注释形式**）
- 是否允许对该客户端格式注入 proxy 生成的 SSE 控制块：`:1443-1452`
  ```rust
  fn client_format_allows_proxy_generated_sse_control_blocks(plan: &ExecutionPlan) -> bool {
      // OpenAI-compatible clients commonly parse every client-visible SSE event as
      // an OpenAI JSON payload or [DONE]. Keep the downstream wire format strict:
      // do not inject proxy-generated comments, pings, or keepalives for openai:*.
      !plan.client_api_format.trim().to_ascii_lowercase().starts_with("openai:")
  }
  ```
- 流体构建：`:1454-1508` `build_sse_body_stream(..., emit_keepalive, keepalive_interval)`
  - `emit_keepalive=true`：`:1471-1498` 走带 `tokio::time::interval` 的 `select!`，每 tick `yield SSE_KEEPALIVE_BYTES`（`:1494-1496`）。
  - `emit_keepalive=false`：`:1502-` 纯转发上游 chunk，无任何心跳。

**已证实事实**：`client_api_format` 以 `openai:` 开头（即 Codex 的 `openai:responses`）时 `emit_keepalive=false`，aether 在上游静默期**不向客户端发送任何字节**。

**附带事实**：即使将上述开关对 openai:* 打开，现有心跳体是**注释**（`SSE_KEEPALIVE_BYTES`），据 §4 对 Codex 无效。要对 Codex 生效，需将心跳体改为带 `data:` 的合成事件。

### 6.1 对比：sync 路径已有心跳（仅 sync，且需配置开关）

`apps/aether-gateway/src/executor/orchestration.rs:61-68` 存在 sync 心跳：
- `ENABLE_STANDARD_TEXT_SYNC_HEARTBEAT_CONFIG_KEY = "enable_standard_text_sync_heartbeat"`
- `ENABLE_OPENAI_IMAGE_SYNC_HEARTBEAT_CONFIG_KEY = "enable_openai_image_sync_heartbeat"`
- `build_sync_json_whitespace_heartbeat_stream` / `build_openai_image_sync_json_whitespace_heartbeat_stream`

即 sync（缓冲）路径有 whitespace 心跳（配置门控），**流式 SSE 路径无对应机制**（除 §6 的注释心跳且对 openai:* 关闭）。

---

## 7. 涉事客户的线上真实数据（sub2api 库）

来源：`sub2api-postgres` → `usage_logs` / `ops_error_logs`，`user_id=192`，日期 2026-08-03。

- 当日 `usage_logs` 记录 131 条，时间跨度 10:56:44 → 13:28:24。
- **`openai_ws_mode = false`（全部）** → 客户端全程 HTTP SSE，未用 WebSocket。模型 `gpt-5.6-sol`。
- `ops_error_logs` 中 `user_id=192` 当日 **0 条**；`usage_logs` 中 `stream 且 output_tokens=0` **0 条**。→ sub2api 侧对该客户**无服务端错误、无残缺流**。
- **上下文规模巨大**：`cache_read_tokens` 在 12:23 达 **207,616**；压缩后 13:2x–13:3x 回落至约 84K–88K。
- **compact 频繁且慢**（`request_type=1`，5 次，均在 12:23–12:41）：

  | 时间 | input_tokens | cache_read | output | first_byte_ms | duration_ms |
  |---|---|---|---|---|---|
  | 12:23:06 | 205502 | 3840 | 1508 | 38369 | 38408 |
  | 12:27:59 | 1775 | 207616 | 940 | 24457 | 24491 |
  | 12:34:06 | 1775 | 207616 | 1269 | 31808 | 31841 |
  | 12:35:12 | 195264 | 14080 | 876 | 27469 | 27506 |
  | 12:41:10 | 1728 | 207616 | 582 | 18256 | 18286 |

- **25 分钟空档**：12:45:44 → 13:10:58 期间 `usage_logs` 与 `ops_error_logs` 对 `user_id=192` 均 0 条；空档后推理档由 `medium` 切为 `low`，请求恢复。

### 7.1 该账号在 aether 侧的上游连接失败（旁证，非本次 SSE 客户之直接因）

`aether-app` 日志，key `4da4ba36`，24h：
- `codex_ws_official_protocol_failed` 11 次（`protocol_phase="idle"` 8 次 / `"response"` 3 次），`transport_detail` 多为 `Connection reset without closing handshake`。
- 频率约每 2 小时一次，且属 aether→上游的 **WebSocket** 传输；本次客户端为 SSE。〔推断〕两者不构成本工单的直接因果，仅说明该账号上游存在偶发空闲重置。

---

## 8. 排除项（附理由）

| 假设 | 结论 | 依据 |
|---|---|---|
| compact 18–38s 触发超时 | 排除 | compact 一元、超时默认 1200s（§3.1） |
| 号池候选重试风暴 | 排除（对本客户） | 单一专属账号 4da4ba36，无跨号候选（§1、§7） |
| `gpt-5.6-luna` 模型 404 | 排除（非本客户） | luna 404 来自 group 8 其他 `user_id`（162/251/255/274/286/69/82）；本客户全程 sol |
| WebSocket 传输/重连迁移 | 排除（对本客户） | `openai_ws_mode=false`，客户端 SSE；错误为 waiting for **SSE**（§2、§7） |
| 账号被限流/不健康 | 排除 | 账号 429 计数为 0、status active（§1） |
| 服务端报错导致断流 | 排除 | sub2api 侧对该客户 0 错误、0 残缺流（§7） |

---

## 9. 结论

1. **已证实**：客户端错误为 Codex 的 **SSE 应用层 idle 超时**（§2、§3），触发条件是流式响应中出现超过 `idle_timeout` 的静默 gap。
2. **已证实**：aether 对 `openai:*` 流式**不注入心跳**（§6），且其现有心跳为注释形式、对 Codex 无效（§4）。
3. **已证实（线上）**：本客户为 HTTP SSE、单一健康账号、服务端 0 错误；异常集中在**超大上下文（约 20 万 token）驱动的频繁且缓慢的自动压缩**（§7）。
4. **〔推断〕根因组合**：
   - 触发端（客户侧，不可由本网关控制）：超大长会话 → 频繁慢压缩 + 大上下文推理，产生较长静默 gap；
   - 放大端（网关侧，可控）：aether 对 openai:* 无有效流式心跳 → 静默 gap 打穿 Codex idle 阈值 → `idle timeout waiting for sse` → 断开重连。
5. **证据边界**：具体"重连"请求**在服务端无日志**（客户端在完成前 abort，重试多数成功，仅表现为成功记录）。故 §4 机制为「客户端报错 + 超大上下文 + aether 无心跳」三者的推断，而非某条 logged 的重连事件。25 分钟空档「与客户端重连循环一致」，但无法从服务端日志区分"重连循环"与"用户离开"。

---

## 10. 可选修复方向（仅记录，暂不执行）

> 以下为方向性记录，未实施；如推进需按正常 tag→CI→ghcr→update 链路，且线上以真实 Codex 客户端验证。

- **A. 网关侧（可控的直接放大器）**：在 `build_sse_body_stream`（`execution.rs:1454-1508`）对 openai:*（或专门对 codex/responses 路径）启用心跳 tick，并将心跳体由注释改为**合成的空事件**，例如：
  ```
  event: response.aether_keepalive
  data: {"type":"response.aether_keepalive"}

  ```
  依据：§5（Codex 未知类型 → `_` → `Ok(None)`，静默丢弃且重置计时）、§4（注释无效，必须带 `data:`）。15s 间隔 ≪ 默认 300s，余量充足。
  - 兼容性考量：若需对**其他** OpenAI 兼容客户端保持严格线格（§6 注释原意），可将合成心跳限定在 Codex 路径（按 UA/路由判定），而非全局放开。
  - 待验证项：以真实 Codex 客户端确认该合成事件确实重置 `stream.next()` 计时且界面无副作用。
- **B. 客户侧（触发端，网关不可控，运营沟通）**：会话上下文已约 20 万 token，建议开新会话 / 清理上下文、并保持较低推理档（客户已自行降为 low），以消除长静默 gap。
- **C. 其他记录项**：
  - compact 在 20 万上下文下 18–38s 属一元缓冲路径固有开销，只要 Codex 1200s 一元超时不变，其自身不致断流。
  - `sub2api` 文件日志（容器内 `/app/data/logs/sub2api.log`）在本次排查时停在 11:50，落后于实际请求约 1 小时；如需 session 级追踪需先修复该断档（stdout 仅输出 ERROR 级）。

---

## 11. 源码/数据引用清单

Codex（`/opt/stacks/openai-codex`）：
- `codex-rs/protocol/src/error.rs:91` — `stream disconnected before completion: {0}`
- `codex-rs/codex-api/src/sse/responses.rs:505` — `timeout(idle_timeout, stream.next())`
- `codex-rs/codex-api/src/sse/responses.rs:523-528` — `idle timeout waiting for SSE`
- `codex-rs/codex-api/src/sse/responses.rs:533-537` — 解析失败 `continue`
- `codex-rs/codex-api/src/sse/responses.rs:467-472` — 未知事件 `_ => Ok(None)`
- `codex-rs/codex-api/src/sse/responses.rs:161-174` — `ResponsesStreamEvent`（仅 `type` 必填）
- `codex-rs/codex-api/src/endpoint/responses.rs:159` — idle_timeout 取自 `provider().stream_idle_timeout`
- `codex-rs/codex-api/src/endpoint/responses_websocket.rs:699` — `idle timeout waiting for websocket`（对比）
- `codex-rs/model-provider-info/src/lib.rs:26` — `DEFAULT_STREAM_IDLE_TIMEOUT_MS = 300_000`
- `codex-rs/model-provider-info/src/lib.rs:319-323` — `stream_idle_timeout()`
- `codex-rs/core/src/client.rs:161-164,630-633` — compact 一元 & `stream_idle_timeout * 4`
- 依赖：`eventsource-stream 0.2.3`（`codex-rs/Cargo.lock`），`event_stream.rs`：`Comment(_) => {}`、`if event.data.is_empty() { return None; }`

aether（本仓库）：
- `apps/aether-gateway/src/execution_runtime/stream/execution.rs:121-122` — `SSE_KEEPALIVE_INTERVAL=15s`、`SSE_KEEPALIVE_BYTES=": aether-keepalive\n\n"`
- `apps/aether-gateway/src/execution_runtime/stream/execution.rs:1443-1452` — `client_format_allows_proxy_generated_sse_control_blocks`（openai:* → false）
- `apps/aether-gateway/src/execution_runtime/stream/execution.rs:1454-1508` — `build_sse_body_stream`（emit_keepalive 分支）
- `apps/aether-gateway/src/executor/orchestration.rs:61-68` — sync 心跳配置键（对比）

线上数据：
- `aether-postgres` / `aether`：`provider_api_keys.id=4da4ba36-…`（布韩账号）
- `sub2api-postgres` / `sub2api`：`usage_logs` / `ops_error_logs`（`user_id=192`，2026-08-03）
- `aether-app` 日志：`codex_ws_official_protocol_failed`（key 4da4ba36，24h×11）

---

## 12. 2026-08-05 实施状态

本节是对原调查记录的后续更新。修复已在本地源码实现并加入测试，**尚未部署到生产环境**。

- Codex `openai:responses` 且 `client_family=codex` 时，从首个 15 秒周期到期后发送协议有效的未知 JSON 事件 `response.aether_keepalive`；其他 `openai:*` 客户端仍保持严格线格式，不注入该事件。延迟首个心跳可避免兼容代理把它误记为真实首字节并提前关闭 failover 窗口。
- 流执行优先使用 `ExecutionTimeouts.stream_idle_ms` 作为活动期限，并兼容旧计划的 `read_ms` 作为上游读取期限；未配置专用值时，默认 300 秒无上游帧失败、120 秒无客户端可见内容失败。控制帧可以维持连接，但不能无限延长用户等待。该限制不是总耗时上限，持续有客户端可见活动的长流不受影响。
- 首个 execution-runtime headers 帧、JSON 成功探测、预取阶段和已返回响应后的主消费循环均受活动期限保护。
- 下游写入使用 `ExecutionTimeouts.write_ms`，未配置时默认 30 秒；客户端连接未断但停止读取时会以 `downstream_write_timeout` 取消，不再永久阻塞生产任务。
- 客户端断开后仅继续排空最多 30 秒（若 `read_ms` 更短则取更短值），随后取消上游任务并写入 `cancelled/499` 终态。
- 中途超时对 OpenAI Responses 输出原生 `response.failed`，对 Claude Messages 输出原生 `error`，使客户端立即得到明确终态。
- SSE 注释/控制块不再被计作客户端可见进展；合成心跳只维持 Codex 客户端连接，不会重置网关自身的上游或业务进展 watchdog。
