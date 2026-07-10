# Codex Attestation 调查（2026-07-10）

## 结论

`x-oai-attestation` 不是静态的 Codex client profile、`installation_id` 或 TLS
指纹字段。它是由真实的、已连接的 Desktop 宿主在请求当下提供的设备证明。

因此，Aether 的 pooled Codex 路径不能安全地把入站客户端携带的证明与 Aether
选中的 OAuth 账户、具体 profile 和连接一起复用。没有受支持的同账号 Desktop
证明宿主时，符合 Codex 源码语义的行为是**省略该头**。

## Codex 源码依据

调查基线：本机 `/opt/stacks/openai-codex` 已更新到
`1f0566d3f59298d1bb88820a0d35294f1eeb07ea`。

- PR #20619 于 2026-05-08 合并；它描述的是 Desktop 向 app-server 提供
  attestation token，app-server 将其用于受限的 ChatGPT Codex 请求路径。
  [PR #20619](https://github.com/openai/codex/pull/20619)
- `attestation/generate` 是 app-server 发给 Desktop 宿主的 JSON-RPC 请求，参数是
  空对象；宿主返回不透明的 `{ "token": "..." }`，并不是 `/responses` 请求体字段。
  [协议类型](https://github.com/openai/codex/blob/1f0566d3f59298d1bb88820a0d35294f1eeb07ea/codex-rs/app-server-protocol/src/protocol/v2/attestation.rs)
- app-server 将 token 封装为 `x-oai-attestation` 请求头；没有声明
  `capabilities.requestAttestation` 的已初始化宿主时，头会被完全省略。
  [README](https://github.com/openai/codex/blob/1f0566d3f59298d1bb88820a0d35294f1eeb07ea/codex-rs/app-server/README.md#L1477-L1479)
  · [实现](https://github.com/openai/codex/blob/1f0566d3f59298d1bb88820a0d35294f1eeb07ea/codex-rs/app-server/src/attestation.rs)
- 当前开源代码仅在 ChatGPT auth 路径尝试 attestation；普通 API-key 不是该功能的
  目标范围。
  [provider gate](https://github.com/openai/codex/blob/1f0566d3f59298d1bb88820a0d35294f1eeb07ea/codex-rs/model-provider/src/provider.rs#L255-L260)

公开源码没有服务端验签策略、强制启用日期或账户处罚逻辑。因此不能由此推断“没有
该头就会封号”或“检测尚未启用”。

## 线上只读核查

核查过程中不读取、不输出或保存任何 attestation token 值。

- `aether-app` 容器 stdout 最近 24 小时未出现 `x-oai-attestation`。
- PostgreSQL `usage_http_audits` 最近 90 天聚合结果：17,147 条审计记录；其中
  17,072 条包含入站请求头、16,592 条包含上游请求头；两个方向中包含
  `x-oai-attestation` 的记录均为 **0**。

这说明在该采样窗口内，没有证据表明用户已向 Aether 提交该头，或 Aether 已将其
转发到上游。统计只用于存在性核验；审计值没有被导出。

## Aether 决策与实现

提交 [`c68628f3`](../../commit/c68628f304dbba403eed1319c322d211b59c2406)
`fix(codex): strip inbound attestation in pooled requests`：

- 在 `apps/aether-gateway/src/ai_serving/planner/standard/codex.rs` 的
  `CODEX_POOL_UPSTREAM_HEADER_BLOCKLIST` 增加 `x-oai-attestation`。
- 现有移除逻辑大小写不敏感，因此 `X-OAI-Attestation` 也会被移除。
- 仅影响 `provider_type = codex` 的 pooled 上游请求；不会新增 token、包装 token、
  缓存 token 或改变 TLS/profile 的其它行为。
- 即使 client header profile 被禁用，pooled Codex 出站路径仍会移除该头。

这项变更避免把用户的潜在设备证明与另一个池化账户/profile 组合发往上游。它不是
attestation 生成或模拟功能。

## 验证与发布边界

已执行：

```bash
cargo fmt --check -- \
  apps/aether-gateway/src/ai_serving/planner/standard/codex.rs \
  apps/aether-gateway/src/ai_serving/planner/standard/codex/tests.rs
cargo test -p aether-gateway --lib codex -- --nocapture
```

结果：130 个匹配 Codex 测试通过；新增测试覆盖混合大小写的
`X-OAI-Attestation` 被移除。

本调查和代码提交不启动、停止、重启或部署生产服务。发布 tag/CI 产物与生产更新
是独立步骤；生产更新必须另行明确授权。
