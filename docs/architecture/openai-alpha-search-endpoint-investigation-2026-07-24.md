# OpenAI Codex `alpha/search` 端点调查与 Aether 接入评估

- **状态：** 调查完成；实现结果见实施报告
- **调查日期：** 2026-07-24
- **OpenAI Codex 源码基线：** `/opt/stacks/openai-codex`，提交 `81da9deb`
- **Aether 源码基线：** `/opt/stacks/aether`，提交 `a6920836`
- **Sub2API 上游基线：** `/opt/stacks/sub2api`，`origin/main` 提交 `cb24522dd`
- **Sub2API 目标分支基线：** `/opt/stacks/sub2api`，`custom-prod` 提交 `05cb36dd`
- **后续执行方案：** `docs/architecture/openai-alpha-search-execution-plan-2026-07-24.md`
- **实施报告：** `docs/architecture/openai-alpha-search-implementation-report-2026-07-24.md`

---

## 1. 调查范围与方法

本报告通过只读检查本机 OpenAI Codex、Aether 与 Sub2API 源码，回答以下问题：

1. Codex 中的 `alpha/search` 端点采用什么 HTTP 路径和调用方式；
2. 请求携带哪些字段、请求头和上下文；
3. 响应格式及 Codex 对各字段的实际使用方式；
4. 该端点是否适合作为 Aether 中与 `openai:chat`、`openai:responses`
   并列的新 API surface；
5. 如果接入，需要遵守哪些路由、认证、粘性、计费和兼容性约束。
6. 在 `Codex 客户端 -> Sub2API -> Aether -> 官方 Codex` 链路中，Sub2API
   `origin/main` 的实现对 Aether 提出了哪些实际协议要求。

本次调查未向 OpenAI、ChatGPT 或其他上游发送真实搜索请求，也未读取线上数据库、
日志或凭据。因此，报告中的线协议结论来自当前开源客户端实现与测试；服务端内部
行为、计费细节和未公开限制不在本次调查的可验证范围内。

Sub2API 部分先通过 `git ls-remote` 确认远端 `main`，再将本地远端跟踪引用更新至
`cb24522dd` 后读取提交对象。随后按用户要求获取 `origin/custom-prod` 并使用
`git merge --ff-only origin/custom-prod` 将目标分支从 `f3535e36` 快进到 `05cb36dd`；
快进前已检查来向文件与本地改动不重叠，工作树原有的未提交和未跟踪文件均保留，
不属于本次调查或拉取操作。

本次调查未修改、重启或部署任何生产服务。

---

## 2. 结论摘要

### 2.1 已确认事实

1. `alpha/search` 是一个 `POST`、JSON、非流式端点。
2. Codex 将它作为独立联网搜索执行器使用，而不是作为最终对话生成端点使用。
3. 请求同时携带搜索命令、模型、搜索会话 ID、有限的对话上下文、搜索设置和输出
   token 上限。
4. 响应核心字段是模型可消费的文本 `output`；可选的 `results` 是给客户端/UI 使用
   的不透明结构化结果。
5. 当前响应结构没有标准 `usage`、Responses 生命周期对象或 SSE 事件。
6. Codex 对自定义 Responses provider 默认关闭该能力，只有
   `supports_standalone_web_search = true` 时才启用。
7. Codex 使用同一个 model provider 的 base URL 和认证信息发送 Responses 请求与
   standalone search 请求。
8. 端点路径仍包含 `alpha`，且 `results` 被客户端明确按不透明 JSON 保留，说明当前
   协议应按实验性、可演进接口处理。
9. Sub2API `origin/main` 已将指向 Aether 的 OpenAI API Key 账号纳入 `alpha/search`
   调度，实际会请求 Aether 的 `/v1/alpha/search`。
10. Sub2API 对成功搜索按次计费；Aether 若也结算上游成本，必须使用独立的
    Search surface 定价，不能直接复用 Responses 共用模型的无格式区分
    `price_per_request`。

### 2.2 接入判断

| 判断维度 | 结论 |
| --- | --- |
| 作为 Aether 公共路由 | 适合 |
| 作为独立权限、限流、审计维度 | 适合 |
| 作为同步 JSON 透传格式 | 适合 |
| 与 Chat/Responses 做请求或响应转换 | 不适合 |
| 默认宣称所有 OpenAI-compatible provider 都支持 | 不适合 |
| 完全独立于 Responses endpoint、模型和账号池配置 | 技术上可做，但不符合 Codex 当前 provider 语义 |
| 作为 Responses provider 的伴生能力 | 最符合当前 Codex 实现 |

因此，较稳妥的定位是：

> 在路由、权限、监控和计费层面暴露独立的 `openai:search` API surface；在上游能力、
> 模型映射和账号选择层面，将其作为 `openai:responses` provider 的伴生能力。

它不应加入 Chat、Responses、Claude Messages、Gemini Generate Content 之间的通用
格式转换矩阵。

在当前部署拓扑中，这一定位对应的具体链路是：Sub2API 继续作为客户端入口和外层账号
调度器，Aether 作为其 OpenAI API Key 上游，接收 `/v1/alpha/search` 后选择最终的官方
Codex OAuth 账号并调用 `https://chatgpt.com/backend-api/codex/alpha/search`。

---

## 3. Codex 中的调用链

当前 standalone web search 的典型调用链如下：

```text
客户端向 /v1/responses 提交一次模型请求
        ↓
模型返回 namespace=web、name=run 的 function call
        ↓
Codex 将 web.run 参数转换为 SearchRequest
        ↓
Codex POST {provider_base_url}/alpha/search
        ↓
服务端返回 output、可选 results、可选 encrypted_output
        ↓
Codex 将 output 包装为 function_call_output
        ↓
Codex 再次调用 /v1/responses，让模型基于搜索输出生成最终回答
```

对应源码：

- 端点客户端：`/opt/stacks/openai-codex/codex-rs/codex-api/src/endpoint/search.rs`
- standalone web search 工具：
  `/opt/stacks/openai-codex/codex-rs/ext/web-search/src/tool.rs`
- 工具输出包装：
  `/opt/stacks/openai-codex/codex-rs/ext/web-search/src/output.rs`
- app-server 端到端测试：
  `/opt/stacks/openai-codex/codex-rs/app-server/tests/suite/v2/web_search.rs`

这一调用链说明，`alpha/search` 是 Responses 工具循环中的伴生执行端点，不是
`/v1/chat/completions` 或 `/v1/responses` 的等价替代品。

---

## 4. HTTP 路径与认证

### 4.1 请求方式

```http
POST {provider_base_url}/alpha/search
Content-Type: application/json
```

`SearchClient` 使用普通 HTTP execute 路径，不使用 SSE 或 WebSocket。

### 4.2 URL 拼接

端点实现中的相对路径固定为：

```text
alpha/search
```

最终 URL 由 provider base URL 直接追加该相对路径。例如：

| Provider base URL | 最终搜索 URL |
| --- | --- |
| `https://api.openai.com/v1` | `https://api.openai.com/v1/alpha/search` |
| `https://chatgpt.com/backend-api/codex` | `https://chatgpt.com/backend-api/codex/alpha/search` |
| `https://gateway.example/v1` | `https://gateway.example/v1/alpha/search` |

测试中也覆盖了自定义 `/api/codex` base URL，因此实际规则不是识别某个固定 host，
而是保留 provider 已配置的 API root，再追加 `/alpha/search`。

### 4.3 认证

搜索请求通过当前 model provider 的认证实现发送。已确认的形态包括：

- API key provider：`Authorization: Bearer <api-key>`；
- ChatGPT/Codex 登录 provider：Bearer 或其他 Codex provider 已解析的认证头；
- ChatGPT 账号路由可能还需要 `ChatGPT-Account-ID` 等账号相关头；
- 自定义 provider 使用其自身配置的认证方式。

Codex 的 app-server 测试验证了 standalone search 会携带 provider authorization。

### 4.4 附加请求头

Codex standalone search 还可能发送：

```http
originator: <thread originator>
x-codex-turn-metadata: <JSON string>
```

`x-codex-turn-metadata` 是不透明的 turn metadata。测试覆盖的内容包括嵌套的：

- `mcp_request_meta`；
- `openai/search_context`；
- 模型 ID、模型 slug 等 telemetry attributes。

实际 turn metadata 也可能包含 session/thread 标识。Aether 若接入，不应把该头当作
固定 schema 解析后重建；更安全的方式是仅提取必要的粘性信息，同时保留原始头，
再应用既有敏感头和账号隔离策略。

---

## 5. 请求格式

请求类型定义位于：

```text
/opt/stacks/openai-codex/codex-rs/codex-api/src/search.rs
```

### 5.1 顶层结构

```json
{
  "id": "search-session-id",
  "model": "gpt-5.x",
  "reasoning": {},
  "input": [],
  "commands": {},
  "settings": {},
  "max_output_tokens": 2500
}
```

| 字段 | 类型 | Codex 类型层是否可省略 | 用途 |
| --- | --- | --- | --- |
| `id` | string | 否 | standalone search 会话标识；当前工具使用 Codex session ID |
| `model` | string | 否 | 搜索输出生成/整理所使用的模型标识 |
| `reasoning` | object | 是 | 与 Codex Responses 请求共用的 reasoning 控制结构 |
| `input` | string 或 Responses Item 数组 | 是 | 提供与搜索有关的有限对话上下文 |
| `commands` | object | 是 | 一次调用中要执行的搜索、打开、查找等操作 |
| `settings` | object | 是 | 位置、上下文大小、域名过滤、外网访问等设置 |
| `max_output_tokens` | unsigned integer | 是 | 搜索端点文本输出预算 |

`Option` 字段在序列化时会被省略，而不是发送 `null`。

### 5.2 `reasoning`

`reasoning` 复用 Codex API 公共类型，可包含：

```json
{
  "effort": "...",
  "summary": "...",
  "context": "auto | current_turn | all_turns"
}
```

当前 standalone web search 工具构造请求时明确设置 `reasoning: None`，因此正常请求
通常不携带该字段。类型保留它意味着服务端协议允许未来或其他调用方使用 reasoning
设置，但本次调查没有发现 Codex 当前 standalone search 主链使用它。

### 5.3 `input`

`input` 是未加 tag 的联合类型，支持两种形式。

#### 纯文本

```json
{
  "input": "查找 OpenAI 最近发布的内容"
}
```

#### Responses API Item 数组

```json
{
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": [
        {
          "type": "input_text",
          "text": "查找 OpenAI 最近发布的内容"
        }
      ]
    }
  ]
}
```

类型层面复用 `ResponseItem`，单元测试也验证了 `input_image` 可以被序列化：

```json
{
  "type": "input_image",
  "image_url": "https://example.com/image.png"
}
```

但是当前 standalone web search 的实际 history builder 会：

1. 保留最近两个可见用户文本消息；
2. 保留二者之间的助手文本；
3. 将助手文本截断到约 1000 tokens；
4. 过滤用户消息中的图片和非文本内容；
5. 不携带 system/developer 消息、函数调用等无关历史；
6. 清除被复制消息的 response item ID。

因此，“协议能表达的 input”比“当前 Codex 实际发送的 input”更宽。

### 5.4 `commands`

`commands` 支持一次请求携带多个操作列表。

| 字段 | 元素结构 | 说明 |
| --- | --- | --- |
| `search_query` | `{q, recency?, domains?}` | 普通网页搜索 |
| `image_query` | `{q, recency?, domains?}` | 图片搜索 |
| `open` | `{ref_id, lineno?}` | 按引用 ID 或 URL 打开页面 |
| `click` | `{ref_id, id}` | 打开已解析页面中的编号链接 |
| `find` | `{ref_id, pattern}` | 在页面中查找文本 |
| `screenshot` | `{ref_id, pageno}` | 对 PDF 的零基页码截图 |
| `finance` | `{ticker, type, market?}` | 查询证券或加密资产价格 |
| `weather` | `{location, start?, duration?}` | 查询天气 |
| `sports` | `{fn, league, ...}` | 查询赛程或排名 |
| `time` | `{utc_offset}` | 查询 UTC offset 对应时间 |
| `response_length` | `short`、`medium`、`long` | 控制返回文本长度 |

#### 搜索和图片查询

```json
{
  "search_query": [
    {
      "q": "OpenAI news",
      "recency": 7,
      "domains": ["openai.com"]
    }
  ],
  "image_query": [
    {
      "q": "waterfalls"
    }
  ]
}
```

`recency` 是最近天数；`domains` 是查询级域名过滤。

Codex 的工具说明还声明：

- 单次 `search_query` 最多四项；
- 超过三项时，`response_length` 应为 `medium` 或 `long`。

这些限制出现在工具说明中，Rust 请求类型本身没有执行对应校验。它们应被视为当前
调用约束，而不是由客户端类型强制保证的线协议不变量。

#### 打开、点击、页面内查找和截图

```json
{
  "open": [
    {
      "ref_id": "turn0search0",
      "lineno": 120
    }
  ],
  "click": [
    {
      "ref_id": "turn0fetch3",
      "id": 17
    }
  ],
  "find": [
    {
      "ref_id": "turn0fetch3",
      "pattern": "installation"
    }
  ],
  "screenshot": [
    {
      "ref_id": "turn1view0",
      "pageno": 0
    }
  ]
}
```

`ref_id` 可以是服务端先前返回的引用，也可以在部分操作中直接使用 URL。

#### Finance

```json
{
  "finance": [
    {
      "ticker": "AMD",
      "type": "equity",
      "market": "USA"
    },
    {
      "ticker": "BTC",
      "type": "crypto",
      "market": ""
    }
  ]
}
```

`type` 可取：

- `equity`
- `fund`
- `crypto`
- `index`

#### Weather

```json
{
  "weather": [
    {
      "location": "US, CA, San Francisco",
      "start": "2026-07-24",
      "duration": 7
    }
  ]
}
```

#### Sports

```json
{
  "sports": [
    {
      "tool": "sports",
      "fn": "schedule",
      "league": "nba",
      "team": "GSW",
      "opponent": "LAL",
      "date_from": "2026-07-24",
      "date_to": "2026-07-31",
      "num_games": 5,
      "locale": "en-US"
    }
  ]
}
```

`fn` 可取 `schedule` 或 `standings`。

`league` 可取：

- `nba`
- `wnba`
- `nfl`
- `nhl`
- `mlb`
- `epl`
- `ncaamb`
- `ncaawb`
- `ipl`

#### Time

```json
{
  "time": [
    {
      "utc_offset": "+03:00"
    }
  ]
}
```

### 5.5 `settings`

```json
{
  "user_location": {
    "type": "approximate",
    "country": "US",
    "region": "CA",
    "city": "San Francisco",
    "timezone": "America/Los_Angeles"
  },
  "search_context_size": "low",
  "filters": {
    "allowed_domains": ["openai.com"],
    "blocked_domains": ["example.com"]
  },
  "image_settings": {
    "max_results": 4,
    "caption": true
  },
  "allowed_callers": ["direct"],
  "external_web_access": true
}
```

| 字段 | 取值 |
| --- | --- |
| `user_location.type` | 当前仅 `approximate` |
| `search_context_size` | `low`、`medium`、`high` |
| `filters.allowed_domains` | 允许的域名数组 |
| `filters.blocked_domains` | 禁止的域名数组 |
| `image_settings.max_results` | 图片结果上限 |
| `image_settings.caption` | 是否生成图片说明 |
| `allowed_callers` | `direct`、`shell`、`code_interpreter` |
| `external_web_access` | boolean，或 `cached`、`indexed`、`live` |

当前 Codex 配置映射的实际行为是：

| Codex web search mode | 发送值 |
| --- | --- |
| disabled | `false`，但通常工具不会被暴露 |
| cached | `false` |
| indexed | `"indexed"` |
| live | `true` |

当前 extension 还固定发送：

```json
{
  "allowed_callers": ["direct"]
}
```

---

## 6. 完整请求示例

下面示例覆盖主要字段，但不代表 Codex 每次都会发送所有字段：

```http
POST /v1/alpha/search HTTP/1.1
Authorization: Bearer <token>
Content-Type: application/json
originator: chatgpt_cca
x-codex-turn-metadata: {"session_id":"session-1"}
```

```json
{
  "id": "session-1",
  "model": "gpt-5.6-luna",
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": [
        {
          "type": "input_text",
          "text": "查找 OpenAI 最近一周的新闻"
        }
      ]
    }
  ],
  "commands": {
    "search_query": [
      {
        "q": "OpenAI news",
        "recency": 7,
        "domains": ["openai.com"]
      }
    ],
    "response_length": "short"
  },
  "settings": {
    "user_location": {
      "type": "approximate",
      "country": "US",
      "city": "San Francisco"
    },
    "search_context_size": "low",
    "filters": {
      "allowed_domains": ["openai.com"]
    },
    "allowed_callers": ["direct"],
    "external_web_access": true
  },
  "max_output_tokens": 2500
}
```

---

## 7. 响应格式

响应类型为：

```json
{
  "encrypted_output": "ciphertext",
  "output": "Search result formatted for the model",
  "results": [
    {
      "type": "text_result",
      "ref_id": "turn0search0",
      "url": "https://example.com/search-result",
      "title": "Search Result",
      "snippet": "A result snippet",
      "future_field": {
        "preserved": true
      }
    }
  ]
}
```

### 7.1 `output`

`output` 是必需字符串。Codex 将它直接转换为：

```json
{
  "type": "function_call_output",
  "call_id": "web-run-1",
  "output": [
    {
      "type": "input_text",
      "text": "Search result formatted for the model"
    }
  ]
}
```

随后该 item 会被送回 Responses 模型。因此，Aether 不应对 `output` 做 Chat、Responses
或 Claude response conversion，也不应重新组织其中的引用文本。

### 7.2 `results`

`results` 是可选数组，当前类型是 `Vec<serde_json::Value>`，即不透明 JSON。

源码注释明确说明：

- 它与模型可见的 `output` 分离；
- 客户端应保留未知 result type 和未知字段；
- 旧服务端不返回 `results` 时必须兼容；
- `results: []` 与字段缺失是两个不同状态，当前客户端会保留这一差异。

因此，Aether 若代理该端点，应进行完整 JSON 透传，不能只保留当前已知的
`text_result/ref_id/url/title/snippet`。

Codex 还专门记录 `results` 序列化后的 payload byte histogram，说明结构化结果体积是
当前实现已关注的运行指标。Aether 应继续使用现有请求/响应大小上限，并避免在高记录
级别下无上限复制该数组。

### 7.3 `encrypted_output`

`encrypted_output` 是可选字符串。当前 standalone web search 主链读取响应后只使用
`output` 和 `results`，没有继续使用或持久化 `encrypted_output`。

本次调查无法从公开客户端代码确定该字段的服务端语义。Aether 不应据此删除它；
最安全的代理行为仍是原样返回。

### 7.4 当前响应中没有的字段

当前 `SearchResponse` 不包含：

- `usage`
- `model`
- `id`
- `status`
- Responses output item 数组
- SSE event type

这会直接影响 Aether 的 token 计费、格式转换和通用响应终态判定。

---

## 8. 与 Chat 和 Responses 的差异

| 特性 | OpenAI Chat | OpenAI Responses | `alpha/search` |
| --- | --- | --- | --- |
| 主要用途 | 对话生成 | 通用多模态/工具生成 | 执行搜索及页面操作 |
| 公共路径 | `/v1/chat/completions` | `/v1/responses` | `/v1/alpha/search` |
| 流式 | 通常支持 | 支持 SSE/WS | 当前客户端仅同步 JSON |
| 请求历史 | `messages` | `input` items | 可选 string 或有限 Responses items |
| 工具定义/调用 | 有 | 有 | 请求本身就是命令执行 |
| 响应终态 | choices/message | response lifecycle/output | `output` 字符串 |
| 结构化客户端结果 | 非核心 | output items | 可选、不透明 `results` |
| 标准 usage | 通常有 | 通常有 | 当前类型没有 |
| 可参与通用格式转换 | 是 | 是 | 不适合 |
| 会话引用 | tool call ID | response/tool item ID | `id` + `turn...search/fetch/view...` ref ID |

`alpha/search` 虽然包含 `model` 和有限对话上下文，但其主要语义是搜索命令执行，不能
仅因为属于 OpenAI provider 就按“另一种聊天格式”处理。

---

## 9. 自定义 Provider 支持边界

Codex 的 `ModelProviderInfo` 新增了：

```toml
supports_standalone_web_search = true
```

该字段默认是 `false`。内置 OpenAI provider 显式设置为 `true`。

extension 暴露 standalone `web.run` 的条件包括：

- provider 是内置 OpenAI；或
- provider 使用 OpenAI actor authorization；或
- 自定义 provider 显式设置 `supports_standalone_web_search = true`；
- 同时 web search mode 不是 disabled。

这说明不能假设所有 OpenAI-compatible Responses endpoint 都实现 `/alpha/search`。
Aether 必须设置显式 capability gate，默认关闭，并且不能把普通只支持
`/chat/completions` 或 `/responses` 的 provider 自动纳入搜索候选。

---

## 10. Aether 当前状态

调查时，Aether 已注册的 OpenAI API format 包括：

- `openai:chat`
- `openai:responses`
- `openai:responses:compact`
- `openai:embedding`
- `openai:rerank`
- `openai:image`
- `openai:video`

当前没有 `openai:search`，也没有 `/v1/alpha/search` 公共路由。

### 10.1 当前缺失点

| 层 | 当前情况 | 接入需要 |
| --- | --- | --- |
| Axum public route | 未挂载 `/v1/alpha/search` | 新增 POST route |
| route classification | 未识别 search | 识别为 `ai_public/openai/search` |
| plan kind | 无 search sync plan | 新增 sync-only plan kind |
| API format registry | 无 `openai:search` | 增加管理和前端可见定义，或使用专用 surface registry |
| candidate matrix | 未返回 search 候选 | 增加同格式或 Responses-companion 候选策略 |
| upstream URL builder | 无 `/alpha/search` builder | 在 provider API root 后追加 `/alpha/search` |
| response finalize | 无 search 响应语义 | 同格式原样 JSON finalize |
| usage mapping | 只会得到空 token usage | 明确 request-count 结算规则 |
| session affinity | 通用解析不读取顶层 `id` | 增加 search session ID 提取 |
| frontend/admin definitions | 无标签和默认路径 | 增加 Alpha 标识及配置入口 |
| loop guard/route inventory | 未列出该路径 | 加入公共 AI 路径清单 |

### 10.2 与 Aether canonical conversion 的关系

Aether 当前 Chat、Responses、Claude Messages、Gemini Generate Content 之间有 canonical
request/response conversion。Embedding 和 Rerank 有各自的数据格式矩阵；Image、Video
采用专用 planner/surface。

Search 更接近 Image/Video 的专用 surface，而不是 Chat/Responses canonical matrix：

- 它有独立路径和专用 request contract；
- 没有可转换的通用 assistant response；
- `results` 要求保留未知字段；
- 搜索引用具有自己的连续性要求；
- 仅支持同步请求。

因此，实现上应优先采用专用 sync planner，或扩展同格式 opaque JSON planner，而不是
扩充 canonical message 类型来承载搜索命令和结果。

---

## 11. 会话粘性与引用连续性

### 11.1 已观察到的协议特征

每个请求携带顶层 `id`。当前 Codex 将 session ID 放入该字段。

后续操作可以引用先前返回或先前输出中的：

- `turn0search0`
- `turn0fetch3`
- `turn1view0`

这类引用会被传给 `open`、`click`、`find`、`screenshot`。

### 11.2 工程推论

公开客户端代码没有说明这些 ref ID 是完全自包含，还是需要服务端按 `id`、账号或
其他上下文解析。因此不能断言服务端一定保存会话状态。

但从 `id` 与跨调用 `ref_id` 的组合可以客观得出：代理层至少应把“同一搜索会话的
引用连续性”作为设计约束，而不能假设任意 provider/key 都能继续处理既有引用。

### 11.3 Aether 当前差距

Aether 当前 client session affinity 的通用 body adapter会读取：

- `prompt_cache_key`
- `conversation_id`
- `session_id`
- 若干 metadata/session 变体

但不会把任意请求的顶层 `id` 当作 session ID。Search 请求如果没有在
`x-codex-turn-metadata` 中同时携带 session/thread ID，当前逻辑无法从请求中提取粘性。

另外，Aether scheduler affinity key 当前包含 `api_format`。即使 Responses 请求和
Search 请求解析出相同 session，它们仍会进入不同格式的 affinity namespace。

接入时至少需要：

1. 仅在 `openai:search` route 下把顶层 `id` 解释为 search session ID；
2. 保证同一 search `id` 的请求优先固定到同一 provider/key；
3. 在预期的 Sub2API -> Aether 链路中，将该 scope 的 `client_family` 明确设为
   `codex`；不能依赖 User-Agent 检测，因为 Sub2API 的 API Key Search builder 默认
   不转发原始 Codex User-Agent；
4. 内部调度建议让 Search 与 Responses 共享同一个 Responses-companion affinity
   namespace，但对外审计、权限和计费仍保持 `openai:search`；这样相同 Aether API Key、
   模型和 Codex session 可以复用同一最终账号；
5. 对包含非 URL `ref_id` 的 `open`、`click`、`find`、`screenshot` 后续请求采用强
   粘性；既有绑定不可用时返回不可重试的会话连续性错误，不能静默切换账号。

第四、第五点属于基于当前两层代理结构作出的工程建议，不是 Codex 客户端明确规定的
服务端协议。建议会话连续性错误使用 `409`，例如
`search_session_affinity_lost`；Sub2API 当前不会对 `409` 做账号 failover，可以避免它
在 Aether 已拒绝迁移后继续切换外层账号。

---

## 12. 认证和账号一致性

Codex 当前使用同一个 model provider 的：

- base URL；
- 认证实现；
- provider capability；

发送 Responses 和 Search 请求。

对于 Aether 普通 API-key upstream，Search 应使用 OpenAI bearer auth，而不是普通
“标准 provider API key 默认映射为 `x-api-key`”的逻辑。

对于 `provider_type = codex` 的 OAuth 池，Search 还应复用现有 Codex 账号处理：

- 正确注入上游 Authorization；
- 正确注入或更新 `chatgpt-account-id`；
- 使用池化账号对应的稳定 user-agent/originator profile；
- 不复用属于另一个客户端设备或另一个账号的敏感证明头；
- 保留安全的 `x-codex-turn-metadata`。

Search 请求不能复用 Responses 的特殊 body edit：例如 Responses 路径对 `store`、
`include`、`instructions`、`max_output_tokens` 的处理不适用于 Search。Search 的
`max_output_tokens` 是已定义字段，应按 Search 语义保留。

---

## 13. 模型和候选选择

Search 请求有必需的 `model` 字段。当前 Codex 会把工具调用上下文中的 model 放入该
字段；app-server 测试还覆盖了从 search context 选择 model slug 的场景。

Aether 有三种可选建模方式。

### 方案 A：完全独立的 `openai:search` endpoint 和模型映射

优点：

- 配置和权限最直观；
- 可以让 Search 使用与 Responses 不同的上游；
- 健康、限流和价格完全隔离。

缺点：

- 管理员需要为同一模型重复维护 Search 映射；
- 容易将 Search 路由到与 Responses 不同的账号或 provider；
- 不符合 Codex 当前“同一个 provider 同时提供 Responses 和 Search”的配置模型；
- 普通 OpenAI-compatible provider 容易被误配置为支持 Search。

### 方案 B：公共格式独立，候选锚定 Responses provider

行为：

- 对客户端公开 `/v1/alpha/search`；
- 权限和审计记录 `client_api_format = openai:search`；
- 候选从 `openai:responses` endpoint 中筛选；
- 仅接受显式声明 standalone search capability 的 endpoint/provider；
- 使用 Responses 的模型映射、base URL 和 key；
- Search planner 构造 `/alpha/search` URL 并原样转发 Search body。

优点：

- 最符合 Codex provider 语义；
- 不重复模型目录；
- 容易复用 Codex OAuth、账号 ID 和 profile 逻辑；
- 可以独立做用户权限、计费和限流。

缺点：

- 调度层需要支持“client surface 与 provider capability anchor 不同”；
- 需要定义 Search/Responses 的跨 surface affinity 规则；
- 管理界面必须清楚表达它不是另一个可转换的模型协议。

### 方案 C：把 `/alpha/search` 直接归类为 `openai:responses`

优点：

- 候选和账号天然复用；
- 改动面可能较小。

缺点：

- 权限、限流、计费和监控无法区分 Responses 推理与外网搜索；
- Responses body/finalize 特殊逻辑可能错误作用于 Search；
- API format 语义不真实；
- 未来排障和策略控制困难。

### 评估结果

方案 B 在协议真实性、账号一致性和运营可控性之间最平衡。方案 A 只有在明确需要将
Search 服务独立运营时才更合适。方案 C 不建议作为长期方案。

---

## 14. 响应透传和格式转换边界

建议将 Search 定义为：

```text
sync-only
JSON-only
opaque request/response
same-capability only
no canonical conversion
```

具体边界：

1. 请求体仅允许执行模型映射、显式 header/body rule 和必要的安全清理；
2. 不把 `input` 转成 Chat messages；
3. 不把 `commands` 转成 Responses tools；
4. 不向请求注入 `stream`、`store`、`include`、`parallel_tool_calls` 等 Responses 字段；
5. 响应成功时保留整个 JSON object；
6. `output`、`results`、`encrypted_output` 和未知字段全部保留；
7. 不生成伪造的 Responses response object；
8. 上游错误状态和错误 body 应按 Aether 既有公共错误策略处理，但不能把成功 Search
   JSON 当成 Chat/Responses JSON 解析。

---

## 15. 计费与 usage

当前 Search 响应类型没有 `usage`，因此 Aether 的通用 OpenAI usage mapper 会得到零
token usage。Aether 的 `StandardizedUsage::new()` 和结算 enrichment 对成功调用仍会
产生 `request_count = 1`，因此现有按次计费能力可以复用；问题在于单价必须按 Search
surface 隔离。

如果不增加专门规则，将出现以下结果：

- 请求统计仍可记录一次成功调用；
- token 数可能为零；
- 仅配置 per-token 价格时，该调用可能不会产生费用；
- 无法从当前响应可靠获知实际搜索、抓取或输出 token 成本。

可选处理方式：

1. 将 Search 默认按 `request_count = 1` 结算；
2. 增加 `openai:search` 格式级单价、Search 专用 billing rule，或等价的 surface-scoped
   pricing snapshot；
3. 不应仅为了 Search 给与 Responses 共用的 global/provider model 设置普通
   `price_per_request`，因为该字段目前不按 API format 隔离，会同时影响普通 Responses
   请求；
4. Search 的 usage event 应明确记录 `api_format = openai:search`、
   `task_type = web_search` 和零 token usage，不能让内部锚定的
   `endpoint_api_format = openai:responses` 覆盖其计费 surface；
5. 如果未来响应出现 usage，再按向后兼容方式读取；
6. 不使用 `max_output_tokens` 冒充实际输出 token；
7. 不根据 `output` 字符长度伪造官方 usage，除非明确标记为估算值。

按请求计价是当前信息条件下最可审计的方式，但具体价格需要根据实际上游成本和产品
策略另行决定。Sub2API `origin/main` 当前内置默认值为 `0.01 USD/次`，这是 Sub2API
自己的用户结算规则，不会通过 Search 响应传递给 Aether，也不应自动成为 Aether 的
价格配置。

---

## 16. 隐私、日志与安全边界

Search 请求可能包含：

- 用户搜索词；
- 最近对话文本；
- 用户的大致国家、区域、城市和时区；
- 域名 allowlist/blocklist；
- MCP metadata；
- Codex session/thread 标识；
- 搜索或页面引用 ID。

Search 响应可能包含：

- URL、标题、摘要；
- 图片或 PDF 相关字段；
- 未来新增的不透明 DTO；
- 体积较大的结构化结果。

Aether 接入时应复核：

1. request trace 是否会完整持久化 `input`、`commands` 和位置；
2. `x-codex-turn-metadata` 是否按现有敏感头规则脱敏；
3. `results` 是否受响应体大小和日志截断限制；
4. 用户访问策略是否允许单独禁止外网搜索；
5. Search API key 权限是否必须与 Responses 权限分开授予；
6. live search 请求是否默认禁止响应缓存。

独立的 `openai:search` 权限维度有实际价值：允许使用模型生成不应自动等价于允许把
对话内容发送给外部搜索服务。

---

## 17. 缓存、重试和故障转移

### 17.1 缓存

Search 可能具有实时性，并且后续请求可能依赖先前引用。建议默认禁用网关响应缓存。
即使 `external_web_access` 为 `false` 或 `indexed`，也不应自动使用通用模型响应缓存，
除非未来有单独设计并完整纳入 `id`、commands、settings、provider 和账号维度。

### 17.2 重试

Codex provider 自身可以对 transport error 和部分 5xx 做请求重试。Aether 再叠加内部
重试时，需要避免形成不可控的重试倍增。

搜索查询通常是读操作，但重复请求可能：

- 产生重复上游成本；
- 返回不同实时结果；
- 生成不同引用；
- 延长工具调用时间。

### 17.3 故障转移

首次、尚未产生引用的 `search_query` 可以按常规候选失败策略考虑故障转移。

对于携带服务端引用的 `open/click/find/screenshot` 请求，切换 provider 或 key 可能破坏
引用连续性。建议至少在同一 `id` 已有成功候选后优先固定候选，并为“原候选不可用”
定义显式失败策略，而不是无提示切换。

这属于基于当前线协议结构作出的工程建议，仍需要真实上游测试确认 ref ID 的具体作用域。

在 Sub2API -> Aether 的两层链路中，Aether 的错误还可能触发 Sub2API 再次切换账号。
Sub2API 当前会对 `401`、`402`、`403`、`429`、`529`、大多数 `5xx` 以及部分瞬态错误
做外层 failover；对于 API Key 上游的 `404/405`，它会按“上游未实现 alpha/search”
处理。Aether 因此不能只保证本层不切换：对 stateful ref 请求，Aether 应在无法保持
绑定时返回不会触发外层 failover 的明确错误，例如
`409 search_session_affinity_lost`。

---

## 18. Alpha 与兼容性策略

当前路径为 `/alpha/search`，且 Codex 最近才增加：

- standalone web search extension；
- structured `results` 透传；
- 自定义 provider opt-in。

因此接入时应：

1. 在 UI 和文档中显示 `OpenAI Search (Alpha)`；
2. 默认关闭 provider capability；
3. 不将其描述为稳定、通用的 OpenAI 标准 API；
4. 对请求和响应未知字段采用保留策略；
5. 将 path builder 与 API surface 名称解耦，方便未来由 `/alpha/search` 迁移；
6. 使用 feature flag 或 capability gate 控制上线；
7. 为缺少 `results`、空 `results`、未知 result type、新增顶层字段建立兼容测试。

内部格式名可以采用：

```text
openai:search
```

同时通过 label/capability 标记 alpha。若 Aether 希望把 wire contract 版本直接编码进
格式名，也可以采用 `openai:search:alpha`，但这会增加未来迁移和权限配置成本。

---

## 19. 建议目标架构

建议的逻辑模型如下：

```text
Public route
  POST /v1/alpha/search
        │
        ├─ client_api_format = openai:search
        ├─ sync-only
        ├─ explicit user permission / RPM / request pricing
        └─ extract search session affinity from body.id
                 │
                 ▼
Responses companion candidate selector
        │
        ├─ provider endpoint format anchored at openai:responses
        ├─ requires supports_standalone_web_search
        ├─ reuses model mapping and provider key
        └─ preserves Codex account/profile handling
                 │
                 ▼
Search-specific request builder
        │
        ├─ upstream URL = provider API root + /alpha/search
        ├─ preserve Search JSON body
        ├─ replace inbound auth with selected upstream auth
        ├─ preserve safe Codex metadata
        └─ no Responses body edits
                 │
                 ▼
Opaque sync finalize
        │
        ├─ preserve status and safe response headers
        ├─ preserve complete successful JSON
        ├─ request_count usage
        └─ no Chat/Responses conversion
```

---

## 20. 预计代码变更面

以下是接入时需要检查的主要文件/模块，不代表最终必须逐一修改：

### Public route 与分类

- `apps/aether-gateway/src/api/ai/registry.rs`
- `apps/aether-gateway/src/api/ai/openai.rs`
- `apps/aether-gateway/src/control/route/ai.rs`
- `apps/aether-gateway/src/constants.rs`
- `apps/aether-gateway/src/frontdoor_loop_guard.rs`

### Plan 与执行

- `crates/aether-ai-formats/src/contracts/plan_kinds.rs`
- `crates/aether-ai-formats/src/formats/shared/routing.rs`
- `apps/aether-gateway/src/ai_serving/planner/specialized/` 或新的 search planner
- `apps/aether-gateway/src/ai_serving/finalize/`

### Provider capability、URL 与认证

- `crates/aether-provider-transport/src/request_url/mod.rs`
- `crates/aether-provider-transport/src/url.rs`
- `crates/aether-provider-transport/src/auth.rs`
- `apps/aether-gateway/src/ai_serving/planner/standard/codex.rs`

### 候选、模型和粘性

- `crates/aether-ai-formats/src/formats/matrix.rs`
- `apps/aether-gateway/src/client_session_affinity.rs`
- `crates/aether-scheduler-core/src/affinity.rs`
- Aether candidate resolution/ranking 模块

如果采用“Responses companion candidate”方案，不一定需要把 Search 加入通用 canonical
`FormatId` 解析/emit registry；可以像 Image/Video 一样使用专用 surface。管理、权限、
审计所使用的字符串 API format 仍可以是 `openai:search`。

### Usage、管理端和前端

- `crates/aether-usage-runtime/`
- `crates/aether-billing/`
- `crates/aether-admin/src/system.rs`
- `frontend/src/api/endpoints/types/api-format.ts`
- provider endpoint 默认路径与相关 UI 测试

---

## 21. 最低验收测试

如果后续实施，至少应覆盖以下测试。

### Route 和格式

1. `POST /v1/alpha/search` 被分类为 `ai_public/openai/search`；
2. 非 POST 方法被拒绝或不进入 Search planner；
3. frontdoor manifest、route inventory 和 loop guard 包含该路径；
4. 用户缺少 `openai:search` 权限时返回明确拒绝。

### 请求透传

1. 保留 `id`、`commands`、`settings`、`max_output_tokens`；
2. 正确映射顶层 `model`；
3. 不注入 Responses 专用字段；
4. 保留未知请求字段；
5. 正确转发 `originator` 和 `x-codex-turn-metadata`；
6. 入站 Authorization 不泄露给上游，使用选中 provider key 重建认证。

### Provider 和账号

1. 未声明 capability 的 Responses provider 不成为候选；
2. API-key provider 使用 Bearer auth；
3. Codex OAuth provider 注入正确 account ID；
4. Search 使用 Responses endpoint 的 API root，并生成 `/alpha/search`；
5. custom path/base URL 不丢失已有路径前缀。

### 响应

1. 只含 `output` 的旧响应正常通过；
2. `results` 缺失、空数组、未知类型均原样通过；
3. `encrypted_output` 和未知顶层字段不丢失；
4. `output` 不被转成 Chat/Responses response；
5. 上游非 2xx 状态按公共错误策略返回。

### 粘性和故障转移

1. 顶层 `id` 能形成 client session affinity；
2. 同一 `id` 连续请求优先命中同一 key；
3. 不同 `id` 可以独立调度；
4. 带 `ref_id` 的后续请求不会在无记录的情况下静默切换候选；
5. Responses/Search 是否共享账号 affinity 的行为有明确测试。

### Usage 和隐私

1. 成功 Search 记录 `request_count = 1`；
2. 缺少 usage 时不伪造官方 token 数；
3. per-request pricing 可生效；
4. request trace 对位置、对话、metadata 和 results 遵守现有记录级别与截断规则；
5. Search 默认不进入响应缓存。

---

## 22. 尚未验证的问题

以下问题无法仅依靠当前客户端源码回答，需要真实兼容环境或上游文档确认：

1. `encrypted_output` 的实际语义；
2. `ref_id` 是否绑定 search `id`、账号、provider、时间窗口或其他服务端状态；
3. `/alpha/search` 的上游计费方式和是否返回未被当前客户端类型声明的 usage；
4. 不同 OpenAI/ChatGPT 认证模式下必须携带的完整 header 集；
5. 服务端对 commands 数量、请求体大小、输出大小和 timeout 的硬限制；
6. `reasoning` 在 Search 端点中的实际支持程度；
7. `external_web_access` 的字符串模式与 boolean 兼容性的长期稳定性；
8. Alpha 路径未来是否会改名、版本化或并入其他公共 API。

实施前建议使用受控测试账号和 mock/recorded fixture 做协议验证，不应直接在生产流量上
试探这些未知行为。

---

## 23. 源码依据

### OpenAI Codex

- 端点路径和同步客户端：
  `codex-rs/codex-api/src/endpoint/search.rs`
- 请求、命令、设置和响应类型：
  `codex-rs/codex-api/src/search.rs`
- standalone web search 工具请求组装：
  `codex-rs/ext/web-search/src/tool.rs`
- 最近对话上下文选择：
  `codex-rs/ext/web-search/src/history.rs`
- 搜索输出转换为 function call output：
  `codex-rs/ext/web-search/src/output.rs`
- provider capability 与 OpenAI 默认值：
  `codex-rs/model-provider-info/src/lib.rs`
- extension capability gate 和 settings 映射：
  `codex-rs/ext/web-search/src/extension.rs`
- app-server 路径、认证、metadata、results 端到端测试：
  `codex-rs/app-server/tests/suite/v2/web_search.rs`
- app-server webSearch item 文档：
  `codex-rs/app-server/README.md`

### Aether

- OpenAI public paths：
  `apps/aether-gateway/src/api/ai/openai.rs`
- public AI route mount registry：
  `apps/aether-gateway/src/api/ai/registry.rs`
- route classification：
  `apps/aether-gateway/src/control/route/ai.rs`
- API format ID：
  `crates/aether-ai-formats/src/formats/id.rs`
- candidate/conversion matrix：
  `crates/aether-ai-formats/src/formats/matrix.rs`
- sync/stream plan routing：
  `crates/aether-ai-formats/src/formats/shared/routing.rs`
- provider request URL builder：
  `crates/aether-provider-transport/src/request_url/mod.rs`
- passthrough header/auth policy：
  `crates/aether-provider-transport/src/auth.rs`
- Codex provider 账号/profile 处理：
  `apps/aether-gateway/src/ai_serving/planner/standard/codex.rs`
- client session affinity：
  `apps/aether-gateway/src/client_session_affinity.rs`
- scheduler affinity key：
  `crates/aether-scheduler-core/src/affinity.rs`
- usage mapping：
  `crates/aether-usage-runtime/src/usage_mapper.rs`
- 默认 request count：
  `crates/aether-contracts/src/usage.rs`
- billing usage enrichment 和 API-format 选择：
  `crates/aether-billing/src/event_enrichment.rs`
- 按次价格计算：
  `crates/aether-billing/src/service.rs`
- 管理端 API format definitions：
  `crates/aether-admin/src/system.rs`
- 前端 API format definitions：
  `frontend/src/api/endpoints/types/api-format.ts`

### Sub2API `origin/main`

- 公共路由与三条兼容入口：
  `backend/internal/server/routes/gateway.go`
- Search handler、外层调度和按次用量提交：
  `backend/internal/handler/openai_alpha_search.go`
- Search 请求构造、URL、header、body 清理、PAT fallback 和响应透传：
  `backend/internal/service/openai_alpha_search.go`
- `alpha_search` 账号能力门控：
  `backend/internal/service/account.go`
- canonical inbound/upstream endpoint：
  `backend/internal/handler/endpoint.go`
- API Key base URL 拼接规则：
  `backend/internal/service/openai_endpoint_url.go`
- 网关路由和转发测试：
  `backend/internal/server/routes/gateway_test.go`
- Search wire、错误和 API Key 兼容测试：
  `backend/internal/service/openai_alpha_search_test.go`
- Search 调度回归测试：
  `backend/internal/service/openai_account_scheduler_test.go`
- Search 按次计费测试：
  `backend/internal/service/openai_alpha_search_billing_test.go`
- 分组价格迁移：
  `backend/migrations/174_group_web_search_price_per_call.sql`
- 嵌入前端 bypass：
  `backend/internal/web/embed_on.go`

关键提交：

- `52071d391`：增加独立 Search 转发；
- `7cbb36f27`、`64a2a3172`、`e5af699d0`：按次计费及修复；
- `b0fa2b352`：避免 `/alpha/search` 落入嵌入前端；
- `776f3f0de`、`695665cbc`、`72fada40f`：PAT header 对齐和 Responses
  web-search fallback；
- `d2b080e88`：恢复 API Key 账号的 Search 调度，并将 API Key 上游
  `404/405` 识别为端点级 failover。

---

## 24. Sub2API `origin/main` 协同检查

### 24.1 目标拓扑

用户确认的目标链路是：

```text
Codex 客户端
  -> Sub2API /v1/alpha/search
  -> Aether /v1/alpha/search
  -> https://chatgpt.com/backend-api/codex/alpha/search
```

在 Sub2API 中，Aether 表现为 `PlatformOpenAI + AccountTypeAPIKey` 账号，典型配置为：

```json
{
  "api_key": "<Aether API key>",
  "base_url": "http://aether:8080/v1"
}
```

Sub2API 的 `buildOpenAIEndpointURL` 会识别 base URL 已以版本段 `/v1` 结尾，因此最终
请求 URL 是：

```text
http://aether:8080/v1/alpha/search
```

不需要在 Sub2API 的 Aether 账号 base URL 中手工填写 `/alpha/search`。

### 24.2 Sub2API 当前线行为

| 边界 | `origin/main` 当前行为 | Aether 需要满足的条件 |
| --- | --- | --- |
| 路径 | 支持 `/v1/alpha/search`、`/alpha/search`、`/backend-api/codex/alpha/search` | Aether 至少必须支持 API Key builder 实际调用的 `/v1/alpha/search` |
| 方法 | `POST`，同步 JSON | Aether 不应要求 SSE 或 WebSocket |
| 入站认证 | Sub2API 验证客户端 key | 与 Aether 无直接关系 |
| 到 Aether 认证 | `Authorization: Bearer <Aether API key>` | Aether 用该 key 完成本地认证，不能把它转发给官方 Codex |
| 模型 | 先做 Sub2API channel mapping，再做账号 model mapping | Aether 模型目录必须接受映射后的名称，并可继续映射到官方模型 |
| body | 保留未知字段；删除顶层 `prompt_cache_key`、`prompt_cache_retention` | Aether 不能依赖这两个 Responses 字段，也不应再套用 Responses body edits |
| query | 将客户端 query 参数追加到 Aether URL | Aether 应继续安全透传 query |
| session | 用 body `id` 作为缺省粘性种子，并原样保留在 body | Aether 必须从 Search route 的顶层 `id` 提取最终账号粘性 |
| headers | API Key 分支默认不复制原始 `Originator`、`Version`、User-Agent、`X-Codex-Turn-Metadata` | Aether 必须用所选 Codex key 的认证和稳定 profile 自行构造官方请求，不能依赖这些入站头 |
| response | 读取完整响应体并按状态、content type 和安全响应头透传 | Aether 成功响应必须保持 Search JSON；不能转成 Responses/SSE |
| 成功判定 | 仅 2xx 生成 `WebSearchCalls=1` | Aether 本地失败不能伪装为 2xx |
| Sub2API 计费 | 成功请求按一次结算，默认代码价格为 `0.01 USD/次` | Aether 的内部结算独立，响应无需返回 Sub2API usage |

Sub2API 的 API Key 分支缺少原始 Codex metadata 不是 Aether 路由的阻断条件，因为 body
`id` 仍在。它意味着 Aether 必须把 Search 识别为 Codex companion surface，并从自己
持有的最终账号 materialized profile 生成 `Authorization`、`ChatGPT-Account-ID`、
User-Agent 和 `originator`。

### 24.3 合并 capability 版本时必须保留的 API Key 修复

`776f3f0de` 曾在引入 `alpha_search` capability 时把候选限制为 OAuth，导致指向 Aether
的 API Key 账号在选号阶段被排除。`d2b080e88` 修复了这一行为：

- OpenAI OAuth 与 API Key 账号都可以承接 `alpha_search`；
- 显式 capability 集中的 `chat_completions` 继续隐含允许 `alpha_search`；
- API Key 上游的 `404/405` 被视为该上游未实现 Search，触发换号但不写账号错误状态。

因此，如果 Sub2API 合并的是 PAT capability 之后的实现，`d2b080e88` 必须同时存在。
只移植初始 route/handler 或只移植 PAT 提交，均不能代表当前 `origin/main` 的最终行为。

这里要求的是保留修复后的行为，不是要求 Aether 仓库依赖或 cherry-pick 这个 Sub2API
提交。如果目标分支只移植最初的 `52071d391`，尚未引入后续 OAuth-only capability
门控，则不会单独遇到该回归；如果按当前 `origin/main` 的最终实现移植，则冲突解决后
必须保证 `AccountTypeAPIKey` 仍能通过 `OpenAIEndpointCapabilityAlphaSearch` 检查。

建议为 Aether 账号显式保存 `alpha_search` capability，以表达运营意图；依赖
`chat_completions` 的隐含兼容虽然当前可用，但不利于以后单独关闭 Search。

### 24.4 Aether 路由、候选与认证

Aether 的实现应保持两套身份：

1. 客户端 surface：`openai:search`，用于本地认证、权限、审计、限流和计费；
2. provider anchor：`openai:responses`，用于复用同一个 Codex provider endpoint、模型
   映射和 OAuth key pool。

Search planner 只能选择显式支持 standalone search 的候选。对于当前官方 Codex pool，
可以复用 provider key 的 `capabilities` JSON，增加例如
`supports_standalone_web_search: true` 的硬门控；自定义 Responses provider 默认不得
获得该能力。

构造官方请求时需要：

1. URL 使用选中 Responses endpoint 的 API root，追加 `/alpha/search`；
2. 对官方 Codex endpoint 生成
   `https://chatgpt.com/backend-api/codex/alpha/search`；
3. 使用最终 key 的 OAuth access token 和 account ID；
4. 复用最终 key 已物化的 User-Agent、originator、installation/profile 选择；
5. 不执行 Responses 的 `store/include/instructions/stream/prompt_cache_key` body edits；
6. 保留安全的 `X-Codex-Turn-Metadata`（如果存在），但不能信任或复用客户端账号身份；
7. 将 Aether 入站 Authorization、内部控制头和属于其他账号的身份头全部终止在本地。

### 24.5 两层粘性

Sub2API 已以 body `id` 选择 Aether 账号。Aether 还需要用相同 `id` 选择最终 Codex key，
形成两层粘性：

```text
search id
  -> Sub2API sticky Aether account
  -> Aether sticky official Codex provider/endpoint/key
```

由于 Aether 当前 affinity cache key 包含 API Key ID、API format、模型、client family 和
session hash，建议 Search route：

1. 把 body `id` 解析成 `client_family=codex` 的 session；
2. 内部使用 Responses-companion affinity namespace；
3. 保持客户端 format 仍为 `openai:search`；
4. 对相同 Aether key、模型和 Codex session 尽量与 Responses 复用最终账号；
5. 对含非 URL `ref_id` 的操作要求已存在且仍可用的绑定。

若绑定已丢失，建议 Aether 返回：

```http
HTTP/1.1 409 Conflict
Content-Type: application/json
```

```json
{
  "error": {
    "type": "search_session_affinity_lost",
    "message": "The search session can no longer resolve its prior references"
  }
}
```

Sub2API 当前不对 `409` 做外层 failover，也不会为该失败计费，因而该状态可以作为无需
修改 Sub2API 的最小 fail-closed 契约。

### 24.6 错误与外层 failover

Sub2API 当前行为如下：

| Aether 状态 | Sub2API 行为 | 协同风险 |
| --- | --- | --- |
| 2xx | 返回客户端并按次计费 | Aether 只能在真实成功时返回 2xx |
| 400、409、422 等普通 4xx | 原样返回，不换外层账号，不计费 | 适合参数错误和 affinity lost |
| 401 | 换外层账号，但不因 Search 单独永久置错 | 认证配置错误可能被暂时掩盖 |
| 403 | 通用 failover，并可能产生账号错误副作用 | Aether Search 权限必须在启用前预检 |
| 404/405 | 认为 Aether 未实现该端点，换外层账号且不置错 | 官方上游若也返回 404/405，语义可能被误判 |
| 429、529、5xx | 通用 failover | stateful ref 请求可能跨 Aether 账号丢失引用 |

最低可行策略是由 Aether 在 stateful ref 请求无法维持最终账号时主动转为 `409`，不要把
内部账号不可用暴露成会触发 Sub2API failover 的 `429/5xx`。

仍有一个需要联合测试的边界：官方 Search 对无效或过期 `ref_id` 是否会返回 `404`。
如果会，Sub2API 会把来自 Aether 的该状态误判为“Aether 没有 Search 端点”。完整解决
方案是在 Aether 已实际 dispatch 到官方上游后返回可信的 disposition header，并让
Sub2API 仅在没有该证明时把 API Key `404/405` 解释为 endpoint unsupported。该扩展在
当前两个仓库中都尚未实现，不能写成已确认能力。

### 24.7 两层计费

两层各自记录一次费用并不天然等于重复扣费：

- Aether 记录 Sub2API 使用其官方账号池产生的上游成本；
- Sub2API 记录终端用户使用 Search surface 的产品价格。

但两层必须遵循一致的成功边界：只有最终 2xx 计费，failover 中间失败和返回客户端的
错误不计费。

Aether 已具备 `request_count` 和 `price_per_request` 基础能力，但 Responses 与 Search
复用同一模型。如果直接给共享模型设置普通 `price_per_request`，普通 Responses 调用也
会被加收按次价格。因此 Aether 需要 format-scoped Search 定价、Search 专用 billing
rule，或等价的 surface pricing snapshot。usage event 必须保持
`api_format=openai:search`，不能被 provider anchor 的 `openai:responses` 覆盖。

### 24.8 Sub2API `custom-prod` 合并风险

当前 `custom-prod@05cb36dd` 与 `origin/main@cb24522dd` 的 merge base 是
`635ad81c`。Alpha Search 涉及的多个文件在两边都已修改，包括：

- `backend/internal/handler/endpoint.go`
- `backend/internal/server/routes/gateway.go`
- `backend/internal/service/account.go`
- `backend/internal/service/openai_account_scheduler.go`
- `backend/internal/service/openai_gateway_service.go`
- `backend/ent/schema/group.go`
- `backend/internal/repository/api_key_repo.go`
- `frontend/src/views/admin/GroupsView.vue`

因此不应假设整组提交可以无冲突 cherry-pick。当前还存在以下明确接口差异：

1. 两个分支的 `SelectAccountWithSchedulerForCapability` 参数集不同；
2. `ReportOpenAIAccountScheduleResult` 在 `origin/main` 带模型参数，而当前
   `custom-prod` 版本不带；
3. `ShouldStopOpenAIOAuth429Failover` 在 `origin/main` 使用额外状态对象，而当前
   `custom-prod` 版本签名不同；
4. 最新 upstream handler 调用的 security-audit 接口在当前 target 代码中没有同名
   调用点；
5. upstream 的迁移名为 `174_group_web_search_price_per_call.sql`，而 `custom-prod` 已有
   `174` 至 `180` 的自定义迁移；Search 合并应使用
   `181_group_web_search_price_per_call.sql` 并保持 SQL 幂等；
6. Ent 生成文件应以合并后的 schema 重新生成和测试，不能只手工拼接部分生成代码。

此外，`05cb36dd` 已为 usage log 增加 `first_byte_ms`，并通过
`beginUsageResponseTiming`、`finishOpenAIUsageResponseTiming` 与 HTTP upstream body wrapper
记录首字节边界。移植 Search handler 时必须使用派生后的 timing context 发起 upstream
请求，并在转发结束后回填结果；否则 Search 端点会绕过新指标。

较安全的合并方式是以 `origin/main` 最终语义为参考逐层移植，而不是只按提交顺序机械
cherry-pick。尤其要在编译前确认 `d2b080e88` 的 API Key capability 语义没有在冲突
解决中丢失。

### 24.9 联合验收

除第 21 节的 Aether 单仓测试外，至少增加以下跨仓库 fixture：

1. Sub2API Aether 账号 base URL 为 `http://aether:8080/v1` 时，请求准确落到
   `/v1/alpha/search`；
2. Sub2API 仅发送 Bearer、JSON body 和最小 header 时，Aether 仍能生成正确的官方
   OAuth、account ID、User-Agent 和 originator；
3. 两层模型映射后，Aether 收到的模型可解析且官方请求模型正确；
4. `future_field`、未知 `results`、`encrypted_output` 和 query 参数端到端不丢失；
5. 相同 `id` 的 `search_query -> open/click/find/screenshot` 连续命中同一最终 key；
6. stateful ref 的最终 key 不可用时，Aether 返回 `409`，Sub2API 不换号且两层都不
   计费；
7. Aether Search 权限缺失的预检能在启用账号前失败，避免运行时 `403` 触发 Sub2API
   账号副作用；
8. Aether 成功一次、Sub2API 成功一次的 usage 能按 request ID、时间和模型对账，但
   两层金额分别遵循各自价格；
9. 普通 `/v1/responses` 在启用 Search 定价后不增加按次费用；
10. Aether route 未安装时的 `404` 与已 dispatch 官方上游后的 `404` 行为有明确测试。

---

## 25. 最终评估

`/v1/alpha/search` 适合成为 Aether 的一等公共 API surface，但其一等性应体现在：

- 独立路由；
- 独立权限；
- 独立限流和审计；
- 独立请求计价；
- 明确的 Alpha Search endpoint capability。

它不适合成为 Chat/Responses canonical conversion matrix 中的第三种通用生成格式。

在当前 Codex provider 设计下，最符合事实的实现是：对外使用 `openai:search` 标识，
对内只选择 active `openai:search` Codex provider endpoint、模型映射和账号凭据，通过
Search 专用同步透传 planner 调用 `{provider_api_root}/alpha/search`。Aether 不需要再为
同一能力增加账号级开关。

结合 Sub2API `origin/main` 的实际实现，Aether 不是可选的旁路，而是 API Key account
所指向的最终账号池网关。Aether 的最低协同要求是：提供 `/v1/alpha/search`、接受
Bearer Aether key、从 body `id` 建立 Codex affinity、使用最终 Codex key 重建官方
身份、保持 Search JSON 不透明透传，并为 stateful ref 失败提供不会触发 Sub2API 外层
换号的 fail-closed 状态。

Sub2API 侧必须保留 `d2b080e88` 修复后的 API Key Search 调度语义，无需让 Aether
仓库依赖该提交；Aether 侧必须使用
surface-scoped Search 定价，不能通过共享 Responses 模型的普通 `price_per_request`
实现。官方 `ref_id` 返回 `404` 时的两层语义仍需联合 fixture 或真实受控测试确认。

---

## 26. 设计修正记录（2026-07-26）

本调查报告第 25 节描述的是最初实施方案（Search 绑定 Responses endpoint，并要求账号级
`supports_standalone_web_search`）。在实际线上诊断和管理端使用反馈后，该开关被判定为
重复配置，现已改为以下最终设计：

- Codex 固定 provider 模板新增 `openai:search` endpoint；
- Search 候选只查询该 endpoint，不能通过 Responses/Chat/其他格式转换绕过；
- endpoint `is_active` 是唯一的 Aether provider-level Search 调度开关，通用端点管理界面
  可直接停用或启用；
- Codex OAuth key 的旧 `api_formats` 列表和旧 Search capability 不再阻塞 Search，避免
  历史账号数据排除新端点；
- Codex 客户端配置文件中的 `supports_standalone_web_search` 仍可保留，因为那是客户端
  provider 声明，不是 Aether 账号级能力开关；
- Aether 启动后台节点会 reconcile 固定 provider endpoint，使已有 Codex provider 能看到
  Search endpoint；本轮修正未部署、未重启，也未执行生产数据库操作。旧版 Search 已在
  此前上线，其线上诊断记录见实施报告 6.2。

因此，本节修正后的结论仍支持 Search 作为 Chat/Responses 同级公共 surface，但其内部
提供商选择应理解为“独立 Search endpoint”，而不是“Responses companion + key capability”。
