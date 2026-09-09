# [Bug] Responses WebSocket + Codex Responses Lite continuation 重复写入 tools/instructions，导致每轮上下文增长约 12K tokens

## 摘要

在 Aether 的 Codex Provider 同时启用以下能力时：

- API format：`openai:responses`
- Responses WebSocket
- Codex Responses Lite contract
- 使用 `previous_response_id` 进行多轮 continuation

每个 WebSocket `response.create` 都会重新执行 Codex Responses Lite body normalization：顶层 `tools` 和 `instructions` 被转换成普通 `input` 项；随后 WebSocket framing 又把客户端提供的 `previous_response_id` 接回请求。

由于上一轮 response 已经保存了转换后的 `additional_tools` 和 developer message，下一轮通过 `previous_response_id` 继承旧历史时，又会追加一份相同内容。结果是每个工具调用稳定增加约 **12.2K–13.4K input tokens**，最终造成上下文快速膨胀。

这不是 Aether UI 的统计显示问题，也不是 prompt cache 失效。上游实际收到并计算了不断增长的上下文；缓存读取量也会同步按固定步长增加。

## 影响版本

| 项目 | 版本/提交 | 结果 |
| --- | --- | --- |
| Aether 正式版 | [`v0.7.13` / `535ee098c`](https://github.com/fawney19/Aether/releases/tag/v0.7.13) | 可复现 |
| 检查时最新 `main` | [`342f8b6a5`](https://github.com/fawney19/Aether/commit/342f8b6a5fef1a788a071069cf7f1aeeccf69458) | 相关逻辑仍未修复 |
| `v0.7.13...main` | [GitHub compare](https://github.com/fawney19/Aether/compare/v0.7.13...main) | 仅增加 continuation binding 相关改动；两个根因文件无变化 |

检查日期：2026-08-19。

## 运行条件

- 官方 Aether `v0.7.13` 镜像，无本地源码补丁。
- Provider type：`codex`。
- Client/Endpoint API format：`openai:responses`。
- `responses_websocket.enabled=true`。
- 模型使用 Codex Responses Lite contract。
- Endpoint 未配置自定义 `body_rules` 或 `header_rules`。
- 请求正文捕获只用于保存脱敏证据，不参与请求改写。

## 期望行为

顶层 `tools` 和 `instructions` 是当前 response 的配置，不应在每次 continuation 中被永久追加为新的历史消息。

当下一轮携带 `previous_response_id` 时：

- 上一轮顶层 `instructions` 不应自动作为历史 instructions 再继承；
- 本轮可以继续提供相同或更新后的顶层 `instructions`；
- 相同的工具定义不应在历史链中每轮再增加一份；
- input token 增长应主要来自新的用户输入、工具结果和模型输出，而不是固定大小的静态配置。

OpenAI SDK 对 `instructions + previous_response_id` 的语义说明见：

- [openai-python Responses API 参数说明](https://github.com/openai/openai-python/blob/main/src/openai/resources/responses/responses.py#L2572-L2578)

## 实际行为

每轮 `response.create` 都发生以下转换：

| 内容 | 客户端发给 Aether | Aether 发给上游 |
| --- | --- | --- |
| Tools | 顶层字段，38,626 bytes | `input[0]: additional_tools`，38,681 bytes |
| Developer instructions | 顶层字段，17,947 bytes | `input[1]: developer message`，18,026 bytes |
| 本轮工具结果 | `input`，4,856 bytes | `input[2]`，4,856 bytes |
| 历史链接 | `previous_response_id` | 仍然保留 |

下一轮的实际工具结果从 4,856 bytes 降到 1,179 bytes，但 input tokens 仍然从 `79,807` 增长到 `92,253`，净增加 `12,446` tokens。

后续连续工具调用继续稳定增长：

```text
input_tokens
130,631
142,810   +12,179
155,254   +12,444
168,630   +13,376

cache_read_input_tokens
117,504
129,792   +12,288
142,080   +12,288
154,368   +12,288
```

固定约 `12,288` 的 cache-read 增量与重复插入的静态 `tools + developer instructions` 大小吻合。

## 最小化示例

以下 JSON 只保留与问题有关的字段，工具 schema 和 instructions 内容已缩写。

### 第 1 轮：客户端发送

```json
{
  "type": "response.create",
  "model": "gpt-5.6-sol",
  "store": true,
  "instructions": "<约 17.9 KB、内容固定的 developer instructions>",
  "tools": [
    "<约 38.6 KB、内容固定的工具定义>"
  ],
  "input": [
    {
      "role": "user",
      "content": [
        {
          "type": "input_text",
          "text": "请执行一个工具调用"
        }
      ]
    }
  ]
}
```

### 第 1 轮：Aether normalization 后发给上游

```json
{
  "type": "response.create",
  "model": "gpt-5.6-sol",
  "store": true,
  "input": [
    {
      "type": "additional_tools",
      "role": "developer",
      "tools": [
        "<同一份工具定义>"
      ]
    },
    {
      "type": "message",
      "role": "developer",
      "content": [
        {
          "type": "input_text",
          "text": "<同一份 developer instructions>"
        }
      ]
    },
    {
      "role": "user",
      "content": [
        {
          "type": "input_text",
          "text": "请执行一个工具调用"
        }
      ]
    }
  ]
}
```

假设上游返回 `resp_1`，其中已经保存上述转换后的 `additional_tools` 和 developer message。

### 第 2 轮：客户端发送 continuation

```json
{
  "type": "response.create",
  "model": "gpt-5.6-sol",
  "store": true,
  "previous_response_id": "resp_1",
  "instructions": "<与第 1 轮完全相同>",
  "tools": [
    "<与第 1 轮完全相同>"
  ],
  "input": [
    {
      "type": "function_call_output",
      "call_id": "call_1",
      "output": "<本轮很小的工具结果>"
    }
  ]
}
```

### 第 2 轮：Aether 实际发给上游

```json
{
  "type": "response.create",
  "model": "gpt-5.6-sol",
  "store": true,
  "previous_response_id": "resp_1",
  "input": [
    {
      "type": "additional_tools",
      "role": "developer",
      "tools": [
        "<同一份工具定义再次写入>"
      ]
    },
    {
      "type": "message",
      "role": "developer",
      "content": [
        {
          "type": "input_text",
          "text": "<同一份 developer instructions 再次写入>"
        }
      ]
    },
    {
      "type": "function_call_output",
      "call_id": "call_1",
      "output": "<本轮很小的工具结果>"
    }
  ]
}
```

上游对第 2 轮的有效理解变成：

```text
resp_1 已保存的历史
├─ additional_tools #1
├─ developer instructions #1
├─ user input
└─ assistant tool call

第 2 轮新增 input
├─ additional_tools #2             ← 重复
├─ developer instructions #2       ← 重复
└─ function_call_output
```

第 3、4、5 轮会继续追加 `#3`、`#4`、`#5`。

## 根因树

```text
Responses WebSocket 每轮固定增加约 12K tokens
│
├─ Codex 客户端正常发送本轮配置
│  ├─ 顶层 tools
│  ├─ 顶层 instructions
│  ├─ 本轮 input
│  └─ previous_response_id
│
├─ Aether Responses Lite normalization
│  ├─ remove(top-level tools)
│  ├─ tools → input[].additional_tools
│  ├─ remove(top-level instructions)
│  └─ instructions → input[].developer message
│
├─ Aether WebSocket framing
│  └─ previous_response_id 被重新接回 provider event
│
├─ 上游 continuation
│  ├─ previous_response_id 继承上一轮已保存的 synthetic input
│  └─ 当前 input 再增加相同 synthetic input
│
└─ 每轮结果
   ├─ tools schema 重复一次
   ├─ developer instructions 重复一次
   ├─ input tokens 增长约 12.2K–13.4K
   ├─ cache-read tokens 固定增长约 12,288
   └─ 上下文窗口快速耗尽
```

## 源码定位

### 1. Responses Lite 将顶层配置转换为历史 input

文件：

- [`crates/aether-ai/formats/src/formats/openai/responses/codex.rs`](https://github.com/fawney19/Aether/blob/342f8b6a5fef1a788a071069cf7f1aeeccf69458/crates/aether-ai/formats/src/formats/openai/responses/codex.rs#L1345-L1421)

函数：

```text
apply_codex_responses_lite_body_contract()
```

该函数会：

1. 从顶层移除 `tools`；
2. 在 `input[0]` 插入 `additional_tools`；
3. 从顶层移除 `instructions`；
4. 在 `input[1]` 插入 developer message。

### 2. WebSocket normalization 后重新接回 previous_response_id

文件：

- [`apps/aether-gateway/src/handlers/proxy/websocket/responses/request.rs`](https://github.com/fawney19/Aether/blob/342f8b6a5fef1a788a071069cf7f1aeeccf69458/apps/aether-gateway/src/handlers/proxy/websocket/responses/request.rs#L92-L120)

函数：

```text
finish_response_create_event()
```

该函数会从客户端 WebSocket event 中恢复 `store`、`previous_response_id` 和 `generate`。

### 3. 每个 follow-up turn 都会重新执行 normalization

文件：

- [`request.rs: normalize_followup_response_create()`](https://github.com/fawney19/Aether/blob/342f8b6a5fef1a788a071069cf7f1aeeccf69458/apps/aether-gateway/src/handlers/proxy/websocket/responses/request.rs#L178-L211)

因此该转换不是只发生在 WebSocket 首轮，而是每个 continuation turn 都会重放。

## 为什么最新 main 仍未修复

`v0.7.13` 之后的最新实质提交是：

- [`c50a1c6c4 fix(ws): preserve Codex continuation bindings`](https://github.com/fawney19/Aether/commit/c50a1c6c46c43867ecfe3b4089753fa9959a5328)

该提交修改的是：

```text
binding.rs
client.rs
session.rs
upstream.rs
```

它没有修改：

```text
websocket/responses/request.rs
formats/openai/responses/codex.rs
```

因此 continuation binding 虽然得到修复，但静态 `tools/instructions` 重复写入问题仍然存在。

## 影响

- 长工具链对话的上下文窗口会异常快速增长。
- 每个工具调用固定多消耗约 12K input tokens。
- prompt cache 会命中重复前缀，因此 cache-read 看似正常，但重复内容仍占用上下文。
- 更早触发自动 compact 或上下文上限。
- Aether 用量统计和结算会记录这些真实上游 input tokens。
- 延迟、吞吐和实际资源消耗随轮数持续增加。

## 建议修复

### 方案 A：最小止血修复

为 WebSocket continuation 保存上一轮 synthetic config 的稳定哈希，例如：

```text
hash(normalized additional_tools)
hash(normalized developer instructions)
```

当满足以下条件时：

- `previous_response_id` 非空；
- 当前 tools/instructions 哈希与该 continuation chain 上一轮完全一致；

则不再把相同的 `additional_tools` 和 developer message 作为新的历史 `input` 插入。

如果哈希发生变化，需要明确处理配置替换语义，不能把新旧 instructions 同时永久保留。

### 方案 B：保持标准 Responses 语义

WebSocket continuation 跳过 Responses Lite 的 `tools/instructions → input` 转换，继续把它们作为顶层当前轮配置发送。

该方案语义最干净，但需要先验证 Codex Pro 上游在 WebSocket + Responses Lite 路径是否完整接受标准顶层 contract。

### 方案 C：重建 continuation input

如果 Responses Lite 上游必须使用 synthetic `input`：

1. Aether 自己管理 continuation chain；
2. 展开历史时删除上一轮 synthetic `additional_tools` 和 developer instructions；
3. 只保留当前轮最新配置；
4. 再发送给上游。

该方案能同时解决重复增长和 instructions 更新语义，但实现成本更高。

## 建议回归测试

### 测试 1：相同配置的连续工具调用

1. 在同一 WebSocket 连接发送首轮 `response.create`；
2. 连续发送至少 4 个携带 `previous_response_id` 的工具结果；
3. 每轮使用完全相同的 tools/instructions；
4. 断言有效上下文中静态配置只存在一份；
5. 断言 input token 增量不再固定增加约 12K。

### 测试 2：instructions 发生变化

1. 第 1 轮使用 instructions A；
2. continuation 使用 instructions B；
3. 断言 B 替换 A，而不是 A 与 B 同时永久存在。

### 测试 3：tools 发生变化

1. 第 1 轮使用 tools set A；
2. continuation 使用 tools set B；
3. 断言当前有效工具配置为 B；
4. 断言 A/B 不会在历史中按轮次继续累加。

### 测试 4：连接重建后继续 previous_response_id

1. 完成首轮并取得 response ID；
2. 关闭 WebSocket；
3. 建立新 WebSocket 后使用旧 `previous_response_id`；
4. 断言仍不会重复写入相同 synthetic config。

### 测试 5：token/cache 计量

使用大小固定的 tools/instructions 和逐轮缩小的 tool output，断言：

- input token 增量跟随新 tool output，而不是静态配置大小；
- cache-read 不再每轮固定增加约 12,288；
- usage settlement 不会记录由重复 synthetic config 造成的额外 token。

## 建议 Issue 标签

```text
bug
websocket
codex
responses-lite
previous_response_id
context-growth
usage-accounting
```
