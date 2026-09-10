# Codex `<environment_context>` 时区 / 日期严格归一化

> Status: Implemented on `custom`（目标版本 `backend-v0.7.122`，未部署）。发版仍走 tag → CI → ghcr → `update.sh`，且只在操作员明确授权后执行。运维手册见 `docs/operations/codex-runtime-identity-runbook.md` §3.7 / §4 / §6 / §7。

## 1. 背景

Aether 用号池里的一个 Codex 账号承接多个下游用户。账号的出站头身份（UA / originator / installation-id）和 Redis 合成的 session / thread / turn 树都已经「像一台机器」（`codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`），但 `input[]` 里模型可见的 `<environment_context>` 块仍逐字节透传，里面有两个**由客户端本机时区推出的值**：

- `<timezone>`：`iana_time_zone::get_timezone()` 的 IANA 名（codex-rs `core/src/session/turn_context.rs:634-643`，失败时字面 `Etc/UTC`）。
- `<current_date>`：`chrono::Local` 的 `%Y-%m-%d`（`core/src/session/world_state.rs:227-236`）。

同一账号因此会交替呈现 `Asia/Shanghai` / `Asia/Hong_Kong` / `UTC`，且 `<current_date>` 与同请求里 UTC 毫秒的 `turn_started_at_unix_ms` 一比就暴露 UTC 偏移。官方 codex-rs 出站的其余时间值（`turn_started_at_unix_ms`、`x-codex-ws-stream-request-start-ms`、UUIDv7 id）全是 UTC epoch，不含时区；`<timezone>` + `<current_date>` 是**仅有的两个**带本机时区特征的字段（对官方源码全量盘点的结论）。

已锁定的决策：

1. **只改 `<timezone>` 与 `<current_date>`**。`<cwd>` `<shell>` `<shell_version>` `<filesystem>` `<network>` `<subagents>` `<environments>`、AGENTS.md 文本、prompt 一律逐字节不动（下游用户需要它们正常工作）。
2. **目标时区 = 网关容器系统时区**（本机 `America/New_York`；荷兰那台应解析成 `Europe/Amsterdam`）。解析失败 / UTC 类 / 非法 → **保底 `America/New_York`**。**中国时区硬性禁止**（即便系统配错也不得出现），命中时回退并 warn。
3. 网关改不了真实请求时刻；「作息节律」只能靠调度层按账号塑形，本次不做（§9）。
4. 跨 turn 必须与官方渲染语义完全一致，不能只做字面替换。

## 2. 官方渲染语义（被镜像的部分）

`core/src/context/world_state/environment.rs`：

- 块体 = `"\n"` + 每个子元素 `  <name>value</name>\n`，顺序 `cwd`/`<environments>` → `shell` → `shell_version` → `current_date` → `timezone` → `network` → `filesystem` → `subagents`；文本 XML 转义 `& < > " '`（`:250-343`）。`filesystem` / `network` 单行；`subagents` 多行。
- **首 turn**：全量块（`PreviousSectionState::Absent|Unknown` → 与空快照比对，`:138-141`），作为 `role:"user"` 消息的一个 `content[]` 片段，同消息可能先有 AGENTS.md 片段；`internal_chat_message_metadata_passthrough.content_item_kinds` 与 `content[]` 下标对齐。item 形状 `{"type","id":"msg_<uuidv7>","role","content","internal_chat_message_metadata_passthrough":{"turn_id",…}}`。`create_time`（浮点秒）由 `stamp_response_item_for_history` 补给新建 item（`core/src/session/mod.rs:3225-3237`），有的客户端构建带、有的不带。
- **后续 turn**：`render_diff`（`:132-190`）：`shell_version/current_date/timezone/network/filesystem` 任一变化（**subagents 单独变化不触发**，`:144-148`）或某 environment 变化时才发块；发块时**重述全部标量**并只对变化的 environment 带 `<cwd>/<shell>`。什么都没变 → **不发**。日切块形状：`\n  <current_date>D</current_date>\n  <timezone>TZ</timezone>\n  <filesystem>…</filesystem>\n`（旧版客户端只发 `<current_date>`）。
- **位置**：turn 开始时 world-state 各 section 按固定顺序落在新 prompt 之前：ModelInstructions → Personality → TokenBudget → ContextWindowGuidance → Realtime → AgentsMd → Permissions → CompactPermissions → CollaborationMode → PersistentMode → **Environments** → EnvironmentsInstructions → Apps → Plugins → Tools → multi-agent hint → MultiAgentMode → ManagedDeveloperInstructions；扩展 section（`skills_instructions`）插在 Permissions 位置（Environments 之前）。同一批 item 的 UUIDv7 共用同一毫秒，低位递增。turn 中段每次采样前 `record_step_world_state_if_changed`（`core/src/session/turn.rs:413`）也可能在工具输出之后、模型输出之前插一块。压缩 / 新 window / 无基线恢复后重发全量块。
- 不变量：任一采样时刻，历史里最后生效的 `current_date` == 声明时区下的今天。

本机真实样本：`/var/tmp/9142aa36-7d3f-416b-92bf-a338cd8f5e49.request.json`（全量块 `input[33]`；日切 diff `input[343]`、`input[385]`）。下游真实值 `Asia/Shanghai` / `Asia/Hong_Kong` / `UTC`。

## 3. 实现

模块：`apps/aether-gateway/src/codex_environment_context.rs`（纯函数，无 I/O；`lib.rs` 注册 `mod codex_environment_context;`）。

### 3.1 时区策略（进程级，一次解析）

`process_environment_timezone()`（`OnceLock`）按顺序取第一个通过校验的候选：

1. `AETHER_CODEX_ENVIRONMENT_TIMEZONE` 环境变量；
2. `TZ` 环境变量（`docker-compose.yml` 已给 `app` 传 `TZ: America/New_York`）；
3. `/etc/localtime` 符号链接目标去掉 `…zoneinfo/` 前缀（不新增 `iana-time-zone` 依赖）；
4. 保底 `America/New_York`。

校验 `validate_environment_timezone`：去空白、去前导 `:`；**先**在原始字符串上查中国时区禁止名单（大小写不敏感），再 `chrono_tz::Tz::from_str`，再在规范名上查一次禁止名单；名字须为 `Region/City` 形（含 `/` 且不以 `Etc/` 开头，排除 `UTC` `GMT` `EST` 等）。禁止名单：`Asia/Shanghai` `Asia/Chongqing` `Asia/Chungking` `Asia/Harbin` `Asia/Urumqi` `Asia/Kashgar` `PRC` `Asia/Hong_Kong` `Hongkong` `Asia/Macau` `Asia/Macao` `Asia/Taipei` `ROC`。拒绝原因：`empty` / `denied_region` / `unknown_iana_name` / `not_a_region_city_zone`。

首次调用记 info `codex_env_tz_resolved`（tz、source ∈ `env_override|tz_env|localtime|fallback`），每个被拒候选记 warn `codex_env_tz_rejected`（source、value、reason）。

### 3.2 kill switch

`AETHER_CODEX_ENVIRONMENT_CONTEXT_REWRITE=off|0|false|disabled|no`（大小写不敏感）→ 跳过整个 pass（重建 `app` 容器生效）。不动身份合成。默认开启。门控：本 pass 只在 `codex_runtime_identity` 表面被调用（HTTP 在 `apply_codex_pool_runtime_identity` 末尾，WS 在 `materialize_codex_ws_step_body` 且候选有 `runtime_identity` 快照），所以事实上跟随「会话身份合成」开关；不新增 per-provider JSON 子键（前端保存会重建 `codex_runtime_identity` 对象，嵌套子键会丢）。

### 3.3 解析与字节保真

只认 `text` 以 `<environment_context>` 开头、以 `</environment_context>` 结尾的片段。块体须以 `\n` 开头，按行扫描 `  <tag` 顶层子元素：

| 行 | 归类 |
|---|---|
| `cwd` / `status` / `shell` 简单元素、`  <environments>`…`  </environments>` | EnvironmentSpecific（有它 = 全量块） |
| `<shell_version status="…" />`、`shell_version` / `current_date` / `timezone` 简单元素、`network` / `filesystem` 单行 | 标量 |
| `  <subagents>`…`  </subagents>` | 标量（多行 raw，不参与判重） |
| 其它 | Unknown（`codex_env_unknown_child_tag`，每进程每标签首次 warn、之后 debug，只记标签名） |

改写只替换 `  <timezone>X</timezone>\n` 与 `  <current_date>Y</current_date>\n` 两行的值（值经与官方一致的 XML 转义）；块内其它字节原样。

### 3.4 回放算法（`apply_codex_environment_context`）

输入 `EnvironmentContextRewriteInput { tz, now_unix_ms, turn_started_at_unix_ms, outbound_thread_id, allow_tail_append, prior_state }`；输出 `(EnvironmentContextRewriteReport, Option<EnvironmentEffectiveState>)`。

- `tools[]` 里 `web_search` / `web_search_preview` 的 `user_location`（含 timezone）整个删除并计数。
- `input[]` 里 `msg_` / `at_` 且后缀为 UUIDv5 的前缀缓存 id（codex-rs `responses_lite` 的基础指令 developer 消息与 `additional_tools` 项，官方为 `uuid5(uuid5(NAMESPACE_OID, thread_id), payload)`，`core/src/client.rs:936-990`）按**出站** thread 重推并计数（`rewrite_prefix_cache_item_ids`，.123 候选）：`at_` 的 payload 是 `tools` 的 JSON 序列化，`msg_` 的是 `content` 文本；取不到 payload、后缀不是 v5（UUIDv7 的用户 / developer 消息）或非 UUID（服务端 `fc_` / `rs_`）的一律不动。
- `input` 为字符串：只做 tz / 日期字面改写（日期取 `turn_started_at` 否则 `now`），不返回状态。
- `input` 为数组：单次遍历，维护 `EnvironmentEffectiveState { date, timezone, shell_version, network, filesystem, subagents, last_diff_shape, carries_current_date, uses_content_item_kinds }`。`prior_state` 只在 body 里没有任何 env 块时作为初始状态（WS 增量步）。

**瞬时来源** `item_instant`（每级计数进 `instant_sources`）：
0. passthrough `create_time`（秒 → ms）；
1. 自身 `id` 最后一个 `_` 之后的 UUIDv7 时间戳（`msg_` / `fco_` / `ctco_` 命中，服务端 `rs_` / `fc_` 落空）；
2. 后继：向后第一个有瞬时的 item；
3. `turn_started_at` 否则 `now`（仅尾部）；
4. 启发式：源日期 + 源时区当天 12:00 换算到目标时区（非尾部且无任何瞬时时）。

**item 分类**：RealPrompt（`role=user`、`type` 缺省或 `message`、文本非包装标签、非 AGENTS.md、非压缩摘要）/ WrapperMessage（developer / system；或 user 文本以包装标签开头；或 user 文本以 `# AGENTS.md instructions` 开头——官方 AgentsMd section 渲染为无包装标签的 user 消息，`core/src/context/user_instructions.rs`，是唯一一种无 `<tag>` 的 user 侧片段，其余无标记片段全是 developer）/ ToolOutput（`*_output`）/ ModelSide（reasoning / function_call / custom_tool_call / assistant message / compaction）/ Other（`compaction_trigger`、空文本、非对象）。AGENTS.md 消息同时算「Environments 之前的 section」（位置判定用）。本机样本 `input[383..=388]`（AGENTS.md → `<skills_instructions>` → 客户端 diff → 两条 developer → prompt，同一 UUIDv7 批次）曾因把 AGENTS.md 当 prompt 而在它之前插块、再把客户端自己的 diff 判冗余删掉，已修并有回归测试。

**逐块处理**：
- 全量块：改写 tz，日期 = date(瞬时, tz)；用块内标量**整体替换**状态，`last_diff_shape = None`，永不删除。
- diff 块（无 environment-specific 行）：先改写；`has_unknown` → 合并、保留；否则比较块内出现的标量（subagents 除外）与状态，**全等 → 删除**并记 `last_diff_shape = 该块形状`（官方在这种状态下不会发块）；不等 → 保留并按「出现者覆盖、未出现者保持」合并（兼容旧版只发 `<current_date>` 的 diff）。
- 删除时同步删 `content_item_kinds` 对应下标；`content` 变空或为字符串 → 删整个 item。

**turn 开始插入**（遇到 RealPrompt）：run = 紧贴 prompt 之前的连续 WrapperMessage（passthrough `turn_id` 不同则截断）。**run 里任一 item 已带 env 块 → 不插**（客户端自己的块在同一批）。位置 = run 中最后一个属于 Environments 之前 section 的 item 之后（`model_instructions` / `personality` / `context_window` / `realtime_conversation` / `user_instructions` / `permissions` / `compact_permissions` / `collaboration_mode` / `persistent_mode` / `skills_instructions` 等），否则 run 起点。瞬时 = 同批前一 item 的瞬时（同毫秒、UUIDv7 低位 +1+hash）；否则后继瞬时 − delta（`delta = 1 + hash % 64` ms）；否则 `turn_started_at` / `now`。仅当 `carries_current_date` 且 date != 状态日期时插入。

**工具输出后插入 / 尾部追加**：ToolOutput 之后紧跟 ModelSide，或它是最后一个 item 且 `allow_tail_append`（HttpResponses / WsStepBody 为 true，HttpCompact 为 false）：id 毫秒 = 工具输出瞬时 + delta；尾部无瞬时用 `turn_started_at` / `now`，非尾部无瞬时跳过。模板 = 最近的前置 prompt / wrapper 消息；`turn_id` = 最近的前置 passthrough。

**合成块**：标量集合 = `last_diff_shape`；尚无 diff 时按官方当前形状取状态中存在者 `shell_version → current_date → timezone → network → filesystem`（不含 subagents）。item 镜像模板键序（`type, id, role, content, internal_chat_message_metadata_passthrough`），`role` 恒 `user`，`content: [{"type":"input_text","text":…}]`，模板有 `id` 才加 `id = "msg_" + uuid_v7`；passthrough 带 `turn_id`、模板有 `create_time` 才带、状态 `uses_content_item_kinds` 时带 `["environments.environment_context"]`。UUID 随机位 = `sha256("aether-codex-env-ctx\0" + outbound_thread_id + "\0" + anchor_key)` 前 16 字节（anchor_key = 锚点 id / `call:{call_id}` / `text:{hash16}`），因此同一线程同一锚点跨请求得到同一 id；同批则复制前一 id 并低 48 位递增。

**确定性**：变换只依赖（历史字节、tz、各 item 自带瞬时、outbound_thread_id）；`now` / `turn_started_at` 只影响没有任何瞬时来源的尾部 item。对同一线程 `H_n` 的输出是 `H_{n+1}` 输出的前缀。

### 3.5 接线

- **HTTP** `ai_serving/planner/standard/codex.rs` `apply_codex_pool_runtime_identity`：身份改写之后、返回之前调用 `apply_codex_pool_environment_context`；表面 `HttpResponses`（`allow_tail_append = true`）/ `HttpCompact`（false）；`Headers` / `WsStepBody` 表面跳过。`Passthrough`（Redis 不可用）分支同样跑，种子退化为入站 thread id 或 `""`，`turn_started_at` 取入站 turn id 的 v7 时间戳。它仍是 body 管线最后一步，`prompt_cache_key` 与 `synthesize_missing_root` 不受影响。
- **WS** `codex_ws/runtime.rs` `materialize_codex_ws_step_body`：身份改写之后、序列化之前，`allow_tail_append = true`，表面 `ws_step_body`。**增量步**（turn 内后续步只带 `previous_response_id` + 新工具输出、无 env 块）靠 `GatewayCodexWsRuntime.env_context_state: Mutex<Option<(outbound_thread_id, EnvironmentEffectiveState)>>`：`step_environment_context` 只在候选有 `runtime_identity` 快照时构造输入，且 `prior_state` 仅在存储的 thread id 与本步一致时传入；每步返回的状态由 `remember_environment_context_state` 写回（None 不覆盖）。同一线程下一 turn 的首步全量回放会在同一锚点（同一 `fco_` id）生成同一个块和同一个 id。状态只活在进程内，不进 Redis。

### 3.6 日志

`codex_env_context_rewritten`（有删除 / 插入 / 追加时 info，否则 debug；report 全零不记）字段：`surface`（`http_responses` / `http_compact` / `ws_step_body`）、`thread = hash16(outbound_thread_id)`、`blocks_seen`、`timezone_rewritten`、`date_rewritten`、`blocks_removed`、`blocks_inserted`、`blocks_appended`、`instant_source_{create_time,own_id,next_item,turn_start,heuristic}`、`unknown_child_tags`、`user_location_removed`、`prefix_cache_ids_rewritten`（.123 候选）。不记 prompt 文本、不记 cwd。

## 4. 测试

- `codex_environment_context.rs` 内 29 个单测（.123 候选加 `prefix_cache_item_ids_follow_the_outbound_thread`、`prefix_cache_rewrite_leaves_items_it_cannot_derive_alone`）：时区策略（优先级、UTC 类与禁止名单全部被拒、大小写、`Europe/Amsterdam` 通过、全拒 → 保底）；解析 / 字节保真（全量含 AGENTS.md 双片段、日切 diff、`<current_date>`-only diff、`<cwd>`-only、空块、`<environments>`、`<shell_version status="unavailable" />`、Windows cwd、转义、`<subagents>`、未知子标签）；DST 边界；瞬时来源分级计数；冗余 diff 删除与 `content_item_kinds` 对齐；turn 开始插入（prompt 前 / developer 串内的位置、同批 id；AGENTS.md 批次保留客户端 diff、缺 diff 时插在 `<skills_instructions>` 之后）；中段插入与尾部追加；HttpCompact 不追加；旧形状合并；`H_n` 前缀性；合成 id 跨请求相同；`user_location` 删除；`input` 为字符串。另有 `#[ignore]` 的 `environment_context_sample_eyeball` 读本机样本打印改写结果。
- `planner/standard/codex/tests.rs`：`http_responses_environment_context_pass_normalizes_clock_and_appends_at_the_tail`、`http_compact_environment_context_pass_rewrites_but_never_appends`、`header_only_surfaces_skip_the_environment_context_pass`。
- `codex_ws/runtime.rs`：`materialized_full_step_normalizes_environment_context_and_returns_state`、`materialized_incremental_step_appends_a_day_change_only_when_the_date_moved`。
- 现有 `apply_outbound_codex_runtime_identity` 的「input untouched」断言保持不变：env pass 是独立函数不在其中。

## 5. 验证清单

```bash
cd /opt/stacks/aether
rustfmt --edition 2021 apps/aether-gateway/src/codex_environment_context.rs   # 只格式化触碰的文件；`cargo fmt -p` 会顺带格式化整个 crate，不要用
RUST_MIN_STACK=16777216 cargo check -p aether-gateway --all-targets
RUST_MIN_STACK=16777216 cargo test -p aether-gateway 2>&1 | tail -40
cargo clippy -p aether-gateway --all-targets 2>&1 | tail -20
cargo test -p aether-ai-formats 2>&1 | tail -10
RUST_MIN_STACK=16777216 cargo test -p aether-gateway environment_context_sample -- --ignored --nocapture   # 眼看本机样本
```

上线后（需授权）：`codex_env_tz_resolved` 应为 `America/New_York` / `tz_env`；抓 30 分钟 `codex_env_context_rewritten` 计数；按运维手册 §3.7 核对出站 `<timezone>` 单一值。

## 6. 配置

| 项 | 值 | 说明 |
|---|---|---|
| `TZ`（compose `app`） | `America/New_York` / `Europe/Amsterdam` | 目标时区来源；荷兰机器 compose 必须显式给 |
| `AETHER_CODEX_ENVIRONMENT_TIMEZONE` | IANA 名 | 覆盖 `TZ`，同样受校验与禁止名单约束 |
| `AETHER_CODEX_ENVIRONMENT_CONTEXT_REWRITE` | `off` | kill switch |

## 7. 文档联动

- `codex-pool-runtime-identity-synthesis-plan-2026-09-03.md` 原则 9 / 非目标 / 「不要解析或改写」/ 「不要改」四处改为「`<environment_context>` 只由独立 pass 改 `<timezone>` / `<current_date>`」。
- 仓库根 `sticky-and-profile.md` 的三处 byte-for-byte 陈述补注：profile v1 pass 本身仍不动，时区 / 日期由本 pass 处理。
- 运维手册 §3.7 复核 SQL、§4 事件、§6 kill switch、§7 残余风险。

## 8. 官方源码基准

codex-rs `07f18d5f`（2026-09-05，`/opt/stacks/openai-codex`）。再次核对时看：`core/src/context/world_state/environment.rs`（渲染与 diff）、`core/src/context/world_state/mod.rs`（section 顺序）、`core/src/session/turn_context.rs`（timezone 探测）、`core/src/session/world_state.rs`（current_date）、`core/src/session/turn.rs` `record_step_world_state_if_changed`、`codex-api/src/search.rs`（`user_location`）。

## 9. 残余风险 / 未纳入本次

- **作息节律**：14 天 833,035 条 Codex 请求峰值在 UTC 02–03 与 06–09，按美国时区 41–45% 落在本地 00–07 点。网关改不了请求时刻，`turn_started_at_unix_ms` 是 UTC epoch，上游一直看得到真实节律。要治只能在调度层给每个账号一个「活跃时段」偏好，属于独立改动，建议作为下一个计划。
- `<cwd>` / `<shell>` 与出站 UA 的 OS 可能不一致（PowerShell cwd 配 macOS UA），用户已接受。
- 上线一刻正在进行的线程历史会被一次性改写（tz 换、部分 diff 删 / 插）→ 对上游像一次历史编辑，prompt cache 失一次；之后稳定。
- 无 id 的旧客户端形状只能 best-effort（尾部用 turn start，其余用启发式；WS 增量步追加的块 id 因锚点无 id 而不可复现）。
- WS 连接级状态只活在进程内：网关重启或换绑后的第一个增量步若正好跨午夜会漏一块，下一 turn 的全量回放会补上。
- AGENTS.md / prompt 里用户自述「我在上海」不在范围。
- 宿主 `/etc/timezone` 陈旧为 `Europe/Berlin`：不影响容器（走 `TZ`），建议择机修正。
