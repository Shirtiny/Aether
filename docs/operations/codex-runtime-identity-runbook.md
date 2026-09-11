# Codex 号池「会话身份合成」运维手册

> 对象：接手维护 `pool_advanced.codex_runtime_identity` 的人或 AI。设计与算法在
> `docs/architecture/codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`（下称「设计文档」），本文只讲线上怎么看、怎么判、怎么退。
> 最后更新：2026-09-06（线上 `backend-v0.7.108`，2026-09-05 10:27 UTC，含 `.107` 内容；`.109` 已提交到 `custom`，未发版；`.110` 在 `custom` 工作区、未提交，见 §4.2 与设计文档 §18.18）。

## 1. 现状速览

| 项 | 值 |
|---|---|
| 功能 | Codex OAuth 号池选号之后，把上游可见的 `session_id` / `thread_id` / `turn_id` / `window_id` 改写成「每账号每日少量 thread / turn」的合成身份；入站官方 ID 一律不动（sticky、WS 绑定、fence、用量都读入站） |
| 开关 | 号池高级设置 →「会话身份合成」卡片；JSON 为 `pool_advanced.codex_runtime_identity {enabled, expected_threads_per_day 1..=64, expected_turns_per_day 1..=512}`；缺省关闭。两个数字是 **上限**：.107 起实际额度按账号、按天在 `[⌈N/2⌉, N]` / `[⌈M/2⌉, M]` 内确定性抖动，.106 及之前是固定模数；.108 起新对话按到达顺序各开一条 thread，额度用满后复用最久没有新 turn 的那条（.107 及之前按哈希分槽）。**.110（工作区，未发版）起 thread 与 turn 都是账号级、按最近 24 小时滚动统计的硬上限**（.109 及之前 turn 是「每 thread」、名册按日历日分区），同一条出站 thread 内的 turn 只顺序向前、不回访旧 turn id；上限可低至 1。官方风控按账号看：近 24h turn 超过约 100 就可能被切模型，务必压在 100 以内。一个人日常用 codex 大约几条到十几条 thread，上限建议 Thread 8–16、Turn ≤64；Codex Pro 现填 32/256 偏高，.110 发版时应下调到 8/64 以内 |
| 线上 | `backend-v0.7.108`（2026-09-05 10:27 UTC）；Codex Pro 号池开着 32 thread/天、256 turn/天 + 请求体/响应体捕获。.104 于 02:36 UTC、.105 于 06:05 UTC、.106 于 06:40 UTC 同日上线 |
| 代码 | `apps/aether-gateway/src/codex_runtime_identity.rs`（算法、白名单、四个表面）；HTTP 挂点 `ai_serving/planner/standard/openai/responses/decision/request.rs`、`ai_serving/planner/standard/codex.rs`；WS 挂点 `codex_ws/runtime.rs`；配置校验 `handlers/admin/provider/write/normalize.rs`；前端号池高级设置卡片 |
| 状态存储 | Redis `ap:{provider_id}:codex_rid:{selection_fp}:...`（.110 起：账号级 thread 名册 ZSET `…:threads`、账号级 turn 台账 ZSET `…:turns`，均按最近 24h 滚动窗口计数、去掉日期段；每条 thread 一个 open turn `…:open:{thread}`；root / turn freeze；window。.108/.109 是 `…:{day_id}:threads` 名册 + 按 thread 的 turn 槽），全部带 TTL；进程内还有 WS 候选快照 |
| 观测 | 日志事件 `codex_rid_config_invalid` / `codex_rid_store_unavailable` / `codex_rid_chain_freeze_miss` / `codex_rid_unknown_metadata_key` / `codex_rid_thread_reused` / `codex_rid_turn_steered`（.110）/ `codex_rid_turn_budget_exceeded`（.110）（§4） |
| 部署记录 | 仓库根 `容器更新历史.md`（操作员本地文件，未纳入 git）+ `.env.bak.<ts>_pre_vX`；更新流程见 `docs/operations/release-and-container-update-spec.md` |
| 官方源码基准 | 本地 checkout `/opt/stacks/openai-codex`（codex-rs），**不要上网查**；核对前先 `git -C /opt/stacks/openai-codex log -1 --format='%h %cd'` 记下版本 |

版本演进（详见设计文档 §18.10–§18.18）：

| tag | 内容 | 线上 |
|---|---|---|
| .101 / .102 | 功能落地 + code-review 修复 | 未单独上线 |
| .104 | window 按合成 thread 跟踪压缩；出站字段白名单 | 2026-09-05 02:36 UTC，**引入缓存回退**（§3.4） |
| .105 | 删 Aether 自补短头、`x-client-request-id` = 出站 thread、无元数据请求合成、`HttpResponses` 补齐官方四头、`x-trace-id` 黑名单 | 2026-09-05 06:05 UTC，缓存恢复 |
| .106 | `HttpCompact` 表面补齐 `session-id` / `thread-id` / `x-codex-window-id`，删 `x-client-request-id` | 2026-09-05 06:40 UTC |
| .107（候选，未单独发版） | 每日 thread / turn 槽数上限按账号、按天抖动（设计文档 §7.0、§18.15）；内置 Codex UA 字典换成 23 组线上观察到的 0.153.x（gpt-6 要求 ≥ 0.153 客户端）；`version` 头随出站 UA 改写（此前透传入站值、中转无此头、Search 遇 `Codex Desktop` UA 会删掉） | 并入 .108 |
| .108 | 含 .107 全部内容；thread 改为按到达顺序 mint、当天名册满后复用最久没有新 turn 的 thread（设计文档 §7.1、§8、§18.16）；新事件 `codex_rid_thread_reused` | 2026-09-05 10:27 UTC；Redis 新增 `…:{day}:threads` ZSET，无需迁移，切换当天账号 thread 数可能一次性略超上限 |
| .109 | turn / compaction / prewarm 的出站 blob 按当前（0.153.x）客户端形状重建：旧客户端（codex-tui ≤ 0.150）没发的 `agent_name` / `window_number` / `context_window_id` / `sandbox_mode` / 三个 review 标志按默认值补齐；`sandbox` 跟随出站 UA 的操作系统（Mac `seatbelt`、Windows `windows_elevated` 或保留 `windows_sandbox`、其它 `seccomp`；`none` / `external` 保持）（设计文档 §18.17） | 已提交到 `custom`，未发版；上线后用 §3.5 Q11 / Q12 复核 |
| .110 | turn 上限从「每 thread」改为账号级、最近 24h 滚动的硬顶（新 turn 台账 ZSET `…:turns`）；thread 名册同样改账号级滑动 24h（去日期段）；一条出站 thread 只有一个 open turn，被取代的旧入站 turn 回放时 steer 向前、不回访 turn id；thread mint 与 turn 预算耦合 ⇒ distinct thread ≤ min(thread_bound, turn_bound)；上限可低至 1；`turn_trigger` / `workspace_kind` 只在 app-server（codex_vscode）UA 下保留，终端 / 无头 UA 出站 blob 删除。新事件 `codex_rid_turn_steered` / `codex_rid_turn_budget_exceeded`（设计文档 §18.18） | **工作区，未提交 / 未发版**；上线后按 §4 末「.110 上线后复核」核对 |

## 2. 数据源与取数规矩

- 库：`docker exec -i aether-postgres psql -U postgres -d aether`。表 `usage`（`input_tokens`、`cache_read_input_tokens`、`provider_name`、`status_code`、`request_id`，WS 请求的 `request_id` 以 `ws-` 开头）、`usage_http_audits`（`request_headers` 入站头、`provider_request_headers` 出站头、`*_body_ref` 指向 `usage_body_blobs.payload_gzip`）。WS 请求没有 audits 行。
- 只统计 `jsonb_typeof(provider_request_headers::jsonb)='object'` 的行（body capture 开着才有出站头）。
- **只报键名和计数，不报 ID 值**。要分组时用 `left(md5(...),6)` 做假名。下游中转的 `cafecode-uid`、`authorization` 之类一律不落到报告里。
- 时间窗以容器启动时间切：`docker inspect aether-app --format '{{.State.StartedAt}}'`。

分类口径（后文 SQL 通用）：

- **改写请求**：入站带 `x-codex-turn-metadata`（真实 codex 客户端），.104 起出站身份被改写。
- **合成请求**：入站不带任何 codex 元数据（主要是同一家下游中转，约占 Codex Pro 流量 73%），.105 起按下游指纹 + 首条用户 prompt 合成 root/turn。
- 两类合起来就是 Codex Pro 的全部 HTTP 流量；WS 流量另看。

## 3. 例行检查

### 3.1 出站头形状 + 命中率（最常用）

保存为 `cache_check.sql`，`psql -v label=v106 -v since='2026-09-05 06:40:06+00' -v until='2036-01-01+00' -f cache_check.sql`：

```sql
with r as (
  select (h.request_headers::jsonb ? 'x-codex-turn-metadata') in_blob,
         h.provider_request_headers::jsonb oh,
         u.cache_read_input_tokens cr, u.input_tokens it
  from usage_http_audits h join usage u on u.request_id=h.request_id
  where h.created_at >= :'since' and h.created_at < :'until'
    and u.provider_name='Codex Pro' and u.status_code=200
    and jsonb_typeof(h.provider_request_headers::jsonb)='object')
select :'label' win, in_blob, count(*) n,
       round(100.0*count(*) filter (where cr=0)/count(*),1) miss_pct,
       round(100.0*sum(cr)/nullif(sum(it),0),1) cached_share,
       count(*) filter (where oh ? 'session-id' and oh ? 'thread-id'
                          and oh ? 'x-codex-window-id' and oh ? 'x-client-request-id') official4,
       count(*) filter (where oh ? 'session_id' or oh ? 'conversation_id') short_hdr
from r group by 1,2 order by 2;
```

判读：

- `official4` 必须等于 `n`，`short_hdr` 必须为 0。否则是表面回归（§18.14.3）。
- `miss_pct`、`cached_share` 对照 §3.4 基线。刚重启后的 15 分钟内 miss 会偏高（每个存量 thread 首个请求要重新钉路由，新 thread 首请求必 miss），看 §3.2 拆解再下结论。

### 3.2 miss 拆解：首请求 vs 线程中段

线程中段（`rn>1`）的 `turn` 请求 miss 才是问题信号；首请求 miss 和 `compaction` miss 都是正常的。

```sql
-- psql -v since='<container StartedAt>' -f miss_breakdown.sql
with r as (
 select h.created_at ts, (h.request_headers::jsonb ? 'x-codex-turn-metadata') in_blob,
        h.provider_request_headers::jsonb->>'thread-id' th,
        (h.provider_request_headers::jsonb->>'x-codex-turn-metadata')::jsonb->>'request_kind' rk,
        u.cache_read_input_tokens cr, u.input_tokens it
 from usage_http_audits h join usage u on u.request_id=h.request_id
 where h.created_at >= :'since' and u.provider_name='Codex Pro' and u.status_code=200
   and jsonb_typeof(h.provider_request_headers::jsonb)='object'),
x as (select r.*, row_number() over (partition by th order by ts) rn,
        lag(it) over (partition by th order by ts) prev_it,
        lag(cr) over (partition by th order by ts) prev_cr,
        extract(epoch from ts - lag(ts) over (partition by th order by ts))::int gap_s from r)
select in_blob, (cr=0) miss, (rn=1) first_since_start, count(*) n, sum(it) sum_it, sum(cr) sum_cr
from x group by 1,2,3 order by 1,2,3;
\echo --- mid-thread misses ---
with r as (
 select h.created_at ts, (h.request_headers::jsonb ? 'x-codex-turn-metadata') in_blob,
        h.provider_request_headers::jsonb->>'thread-id' th,
        (h.provider_request_headers::jsonb->>'x-codex-turn-metadata')::jsonb->>'request_kind' rk,
        u.cache_read_input_tokens cr, u.input_tokens it
 from usage_http_audits h join usage u on u.request_id=h.request_id
 where h.created_at >= :'since' and u.provider_name='Codex Pro' and u.status_code=200
   and jsonb_typeof(h.provider_request_headers::jsonb)='object'),
x as (select r.*, row_number() over (partition by th order by ts) rn,
        lag(it) over (partition by th order by ts) prev_it, lag(cr) over (partition by th order by ts) prev_cr,
        extract(epoch from ts - lag(ts) over (partition by th order by ts))::int gap_s from r)
select to_char(ts,'HH24:MI:SS') t, in_blob, rk, left(md5(coalesce(th,'')),6) th6, rn, it, prev_it, prev_cr, gap_s
from x where rn>1 and cr=0 order by ts limit 25;
```

### 3.3 命中请求的 cached/input 分位数

排除 miss 之后看「命中了多少」，能发现前缀部分失效之类的软问题。`it>=50000` 过滤掉小请求噪音。

```sql
-- psql -v since='...' -v until='...' -f ratio_check.sql
with r as (
 select (h.request_headers::jsonb ? 'x-codex-turn-metadata') in_blob,
        u.cache_read_input_tokens::numeric/u.input_tokens ratio
 from usage_http_audits h join usage u on u.request_id=h.request_id
 where h.created_at >= :'since' and h.created_at < :'until'
   and u.provider_name='Codex Pro' and u.status_code=200 and u.input_tokens>=50000 and u.cache_read_input_tokens>0)
select in_blob, count(*) n,
       round(percentile_cont(0.25) within group (order by ratio)::numeric,3) p25,
       round(percentile_cont(0.5)  within group (order by ratio)::numeric,3) p50,
       round(percentile_cont(0.75) within group (order by ratio)::numeric,3) p75
from r group by 1 order by 1;
```

### 3.4 基线数值（2026-09-04/05 实测，Codex Pro HTTP 200）

| 窗口 | 改写请求 `turn` miss | 改写请求命中 p50 | 合成/透传请求 miss | 合成/透传命中 p50 | WS cached_share |
|---|---|---|---|---|---|
| .103（未合成，基线） | 2.6% | 0.990 | 2.7% | 0.982 | 93.4% |
| .104（回退） | **32.2%** | 0.972 | 5.7% | 0.979 | 91.2% |
| .105 上线后 15 分钟 | 6.9%（含重启首请求） | 0.986 | 13.8%（全为首请求） | 0.968 | 无流量 |
| .106 上线后 5 分钟 | 8.7%（含重启首请求） | — | 9.0% | — | 无流量 |

- `compaction` 请求本身 miss 率高是正常的（基线 36%），`memory` 请求几乎必 miss。
- 合成请求（中转）命中 p50 略低于基线，样本小，尚未定性；持续低于 0.95 再查（可能是中转端首 prompt 变化导致合成 thread 切换，见设计文档 §18.14.5）。

### 3.5 风控视角复核（出站形状全量）

以下脚本对整段时间窗做出站 blob / 头 / window 的一致性与泄漏检查，Q2/Q5 用来验证压缩推进（`window_number` +1、`context_window_id` 换新）。

```sql
-- psql -v since='<ts>' -f review.sql
create or replace function pg_temp.try_jsonb(t text) returns jsonb language plpgsql immutable as $$
begin return t::jsonb; exception when others then return null; end $$;
create temp table rv as
select h.request_id, h.created_at, u.provider_name, u.model, u.api_format, u.request_type,
       h.request_headers::jsonb ih, h.provider_request_headers::jsonb oh,
       pg_temp.try_jsonb(h.request_headers::jsonb->>'x-codex-turn-metadata') ib,
       pg_temp.try_jsonb(h.provider_request_headers::jsonb->>'x-codex-turn-metadata') ob,
       h.provider_request_body_ref obody, h.request_body_ref ibody
from usage_http_audits h left join usage u on u.request_id = h.request_id
where h.created_at >= :'since'::timestamptz
  and jsonb_typeof(h.provider_request_headers::jsonb) = 'object';
alter table rv add column rewritten boolean;
update rv set rewritten = (ib->>'thread_id' is not null and ob->>'thread_id' is not null and ib->>'thread_id' <> ob->>'thread_id');
\echo === Q0 provider breakdown
select coalesce(provider_name,'?') provider, count(*) total, count(ob) with_blob, count(*) filter (where rewritten) rewritten from rv group by 1 order by 2 desc;
\echo === Q1 outbound blob keys (rewritten only)
select k, count(*) from rv, jsonb_object_keys(ob) k where rewritten group by k order by 2 desc;
\echo === Q2 window consistency (rewritten only)
select count(*) rewritten,
 count(*) filter (where ob ? 'window_number') has_wn,
 count(*) filter (where ob ? 'window_number' and split_part(ob->>'window_id',':',2) = (ob->>'window_number')) wn_matches_window_id,
 count(*) filter (where ob ? 'window_number' and (ob->>'window_number')::int > 0) wn_gt0,
 count(*) filter (where ob ? 'context_window_id') has_ctx,
 count(*) filter (where ob ? 'window_number' and not ob ? 'context_window_id' and ob->>'request_kind' <> 'memory') wn_without_ctx,
 count(*) filter (where ob->>'window_id' <> (ob->>'thread_id')||':'||coalesce(ob->>'window_number','0') and ob->>'request_kind' <> 'memory') window_id_mismatch,
 count(*) filter (where oh->>'x-codex-window-id' is not null and oh->>'x-codex-window-id' <> ob->>'window_id') header_vs_blob_window_mismatch
from rv where rewritten;
\echo === Q3 tree keys (rewritten only)
select 'agent_name='||coalesce(ob->>'agent_name','<absent>') v, count(*) from rv where rewritten group by 1
union all select 'thread_source='||coalesce(ob->>'thread_source','<absent>'), count(*) from rv where rewritten group by 1
union all select 'request_kind='||coalesce(ob->>'request_kind','<absent>'), count(*) from rv where rewritten group by 1
union all select 'root_turn_id==turn_id', count(*) from rv where rewritten and ob->>'root_turn_id' = ob->>'turn_id'
union all select 'root_turn_id!=turn_id', count(*) from rv where rewritten and ob ? 'root_turn_id' and ob->>'root_turn_id' <> ob->>'turn_id'
union all (select 'leak:'||k, count(*) from rv, jsonb_object_keys(ob) k where rewritten and k in ('parent_thread_id','parent_turn_id','forked_from_thread_id','forked_from_ordinal_exclusive','subagent_kind') group by k)
union all select 'session!=thread', count(*) from rv where rewritten and ob->>'session_id' <> ob->>'thread_id'
order by 1;
\echo === Q4 outbound headers (rewritten only)
select k, count(*) from rv, jsonb_object_keys(oh) k where rewritten and (k like 'x-codex-%' or k like 'x-openai-%' or k like 'x-oai-%' or k like 'x-responsesapi-%' or k in ('session-id','thread-id','session_id','conversation_id','x-client-request-id','openai-beta','originator')) group by k order by 2 desc;
\echo === Q5 window progression per outbound thread (rewritten only)
select left(md5(ob->>'thread_id'),6) thread, count(*) reqs, min(created_at) first_seen, max(created_at) last_seen,
 min((ob->>'window_number')::int) wn_min, max((ob->>'window_number')::int) wn_max,
 count(distinct ob->>'context_window_id') ctx_ids,
 count(*) filter (where ob->>'request_kind'='compaction') compactions
from rv where rewritten group by 1 order by 3;
\echo === Q6 uuid v7 shape on outbound thread (rewritten only)
select count(distinct ob->>'thread_id') threads,
 count(distinct ob->>'thread_id') filter (where substr(ob->>'thread_id',15,1)='7' and substr(ob->>'thread_id',20,1) in ('8','9','a','b')) v7_variant_ok,
 count(distinct ob->>'thread_id') filter (where (('x'||substr(ob->>'thread_id',17,2))::bit(8)::int & 12) = 0) byte7_gap_ok
from rv where rewritten;
\echo === Q7 inbound context (all rows with inbound blob)
select count(*) with_inbound_blob, count(*) filter (where (ib->>'window_number')::int > 0) in_wn_gt0, max((ib->>'window_number')::int) in_wn_max,
 count(*) filter (where ib->>'request_kind'='compaction') in_compactions, count(distinct ib->>'thread_id') in_threads
from rv where ib is not null;
\echo === Q8 distinct outbound identities per provider (rewritten only)
select coalesce(provider_name,'?') provider, count(distinct ob->>'thread_id') out_threads, count(distinct ob->>'session_id') out_sessions, count(distinct ob->>'turn_id') out_turns, count(distinct ib->>'thread_id') in_threads, count(distinct ib->>'turn_id') in_turns from rv where rewritten group by 1;
\echo === Q9 synthesis pool: requests NOT rewritten, by shape
select coalesce(api_format,'?') fmt, coalesce(request_type,'?') rtype,
 case when ib is not null and ob is not null and not rewritten then 'inbound blob passed through'
      when ib is not null and ob is null then 'inbound blob, no outbound blob'
      when ib is null and ob is not null then 'no inbound blob, outbound blob'
      else 'no blob either side' end shape, count(*)
from rv where provider_name = 'Codex Pro' and not coalesce(rewritten,false) group by 1,2,3 order by 4 desc;
\echo === Q10 synthesis pool: outbound headers on NOT-rewritten rows that still carry codex identity
select k, count(*) from rv, jsonb_object_keys(oh) k where provider_name='Codex Pro' and not coalesce(rewritten,false) and (k like 'x-codex-%' or k like 'x-openai-%' or k in ('session-id','thread-id','session_id','conversation_id')) group by k order by 2 desc;
\echo === Q11 sandbox tag vs outbound user-agent OS (rewritten, non-memory)  [.109]
select case when oh->>'user-agent' like '%Mac OS%' then 'mac' when oh->>'user-agent' like '%Windows%' then 'windows' else 'other' end ua_os,
 coalesce(ob->>'sandbox','<absent>') sandbox, coalesce(ob->>'sandbox_mode','<absent>') sandbox_mode, count(*)
from rv where rewritten and coalesce(ob->>'request_kind','') <> 'memory' group by 1,2,3 order by 1,4 desc;
\echo === Q12 current-client key completeness on turn / compaction / prewarm (rewritten only)  [.109]
select ob->>'request_kind' kind, count(*) n,
 count(*) filter (where ob ?& array['agent_name','window_number','context_window_id','sandbox','sandbox_mode','auto_review_enabled','node_repl_auto_review_required','node_repl_disabled']) complete,
 count(*) filter (where ob ? 'turn_started_at_unix_ms') stamped
from rv where rewritten and ob->>'request_kind' in ('turn','compaction','prewarm') group by 1 order by 2 desc;
\echo === Q12b turn stamp derives from the outbound turn UUIDv7 (rewritten, turn / compaction)  [.123 候选]
select ob->>'request_kind' kind, count(*) n, count(*) filter (where ob ? 'turn_started_at_unix_ms') stamped,
 count(*) filter (where (ob->>'turn_started_at_unix_ms')::bigint
   = ('x' || replace(left(ob->>'turn_id', 13), '-', ''))::bit(48)::bigint) stamp_matches_turn
from rv where rewritten and ob->>'request_kind' in ('turn','compaction') group by 1 order by 2 desc;
```

期望：Q2 里 `wn_without_ctx`、`window_id_mismatch`、`header_vs_blob_window_mismatch` 为 0；Q3 无 `leak:*` 行、`session!=thread` 为 0；Q4 无 `session_id` / `conversation_id` / `x-codex-parent-thread-id` / `x-openai-subagent` / `x-trace-id`；Q6 两列都等于 `threads`；Q8 每个 provider 的 `out_threads` 不超过 `expected_threads_per_day`×账号数量级。.105 起 Q9 中 Codex Pro 应几乎没有「no blob either side」行（无元数据请求已合成）。

.109 起：Q11 每个 `ua_os` 只能出现自己平台的 `sandbox`（mac：`seatbelt` / `none` / `external`；windows：`windows_sandbox` / `windows_elevated` / `none` / `external`；other：`seccomp` / `none` / `external`），`sandbox=none` 的行 `sandbox_mode` 只能是 `danger-full-access`，非 memory 行不得出现 `<absent>`（.108 时段 3 天有 171 条 Mac / Ubuntu UA 配 Windows 沙箱标签）；Q12 每行 `complete` = `n`，turn / compaction 行 `stamped` = `n`（prewarm 官方就没有 `turn_started_at_unix_ms`，不要求）。.123 候选起 `turn_started_at_unix_ms` 恒等于出站 `turn_id`（UUIDv7）前 48 位的毫秒数，不再复制客户端自己的值：`Q12b` 的 `stamp_matches_turn` 必须等于 `stamped`。Q2 的 `window_id_mismatch` 在 turn / compaction / prewarm 形状上必须为 0——.108 时的 22/154 全部是 codex-tui 0.146 / 0.147 入站没带 `window_number`，.109 按出站 0.153 客户端补齐；只有 `request_kind` 缺失的行（官方该形状本就没有 `window_number`，线上 3 天 10 条）仍会计入。

按账号看每日出站 thread 数（.107 的抖动是否生效、忙账号是否天天停在同一个数）：

```sql
-- psql -v since='<ts>' -f per_account_threads.sql ；rv 由 review.sql 建好
select left(md5(u.provider_api_key_id::text),6) acct, date_trunc('day', rv.created_at) d,
       count(*) reqs, count(distinct ob->>'thread_id') out_threads, count(distinct ib->>'thread_id') in_threads
from rv join usage u on u.request_id = rv.request_id
where rv.rewritten and rv.provider_name = 'Codex Pro'
group by 1,2 order by 2,4 desc;
```

判读：`out_threads` 不得超过 `expected_threads_per_day`（.108 切换当天除外，见 §1 版本表）；.107 起忙账号（日请求数远大于上限）之间的 `out_threads` 应当不同，且同一账号跨日不同；若多数账号恰好等于上限并且彼此相同，抖动没生效或上限太低。.108 起忙账号的 `out_threads` 会 **恰好** 等于它当天抖出来的额度（到达顺序会填满），这是预期；差异体现在账号之间与跨日。请求量少的账号本来就只出现实际用到的 thread 数，不是问题。

### 3.6 `/responses/compact` 表面

线上截至 2026-09-05 连续 72 小时没有任何 compact 请求（入站带 `thread-id` 且无 `x-client-request-id` 的行为 0），.106 的 `HttpCompact` 形状只由单测 `http_rewrite_inserts_missing_official_headers_on_responses_and_compact` 覆盖。若以后出现，用下面语句核对：出站应有 `session-id` / `thread-id` / `x-codex-window-id`，**没有** `x-client-request-id`。

```sql
select count(*) n,
       count(*) filter (where oh ? 'session-id' and oh ? 'thread-id' and oh ? 'x-codex-window-id') dash3,
       count(*) filter (where oh ? 'x-client-request-id') creq_should_be_0
from (select h.provider_request_headers::jsonb oh from usage_http_audits h join usage u on u.request_id=h.request_id
      where h.created_at >= :'since' and u.provider_name in ('Codex Pro','Codex Plus')
        and h.request_headers::jsonb ? 'thread-id' and not (h.request_headers::jsonb ? 'x-client-request-id')) s;
```

### 3.7 `<environment_context>` 时区 / 日期归一化复核（.122）

.122 起出站 `input[]` 里每个 `<environment_context>` 块的 `<timezone>` 应恒等于容器时区（本机 `America/New_York`），`<current_date>` 应等于该 item 自身 UUIDv7 瞬时（或紧随其后 item 的瞬时）在该时区下的日期；冗余日切块被删，缺失的日切块按官方 `render_diff` 形状补上。设计见 `docs/architecture/codex-environment-context-time-normalization-plan-2026-09-10.md`。

出站 body 不落库，复核靠两条路：

1. **日志计数**（部署后前 30 分钟）：
   ```bash
   docker logs aether-app --since "$(docker inspect aether-app --format '{{.State.StartedAt}}')" 2>&1 | grep -E 'codex_env_tz_(resolved|rejected)' | cut -c1-300
   docker logs aether-app --since 30m 2>&1 | grep -c 'codex_env_context_rewritten'
   docker logs aether-app --since 30m 2>&1 | grep 'codex_env_context_rewritten' | grep -oE 'blocks_(removed|inserted|appended)=[0-9]+' | sort | uniq -c | sort -rn | head
   ```
   期望：恰一条 `codex_env_tz_resolved` 且 `tz=America/New_York source=tz_env`；没有 `codex_env_tz_rejected`；`codex_env_context_rewritten` 数量与 Codex Pro 带 env 块的请求量同量级。
2. **抓包 / 样本眼看**：按 `docs/operations/tls-fingerprint-capture.md` 抓一条出站 `/responses`，或在开发机跑 `RUST_MIN_STACK=16777216 cargo test -p aether-gateway environment_context_sample -- --ignored --nocapture`（读 `AETHER_ENV_CONTEXT_SAMPLE` 指向的入站请求 JSON），核对：所有块 `<timezone>` 为目标时区；日切块 `<current_date>` 与紧随 prompt 的 `msg_` id 时间戳换算一致（`TZ=America/New_York date -d @<秒>`）；同一线程连续两次请求的输出前者是后者的前缀。

入站侧分布（用于估计改写量）仍可从 `usage_http_audits.request_body` 若落库时统计 `<timezone>` 值；没有落库时以日志计数为准。

### 3.8 前缀缓存 id 与 turn 时间戳跟随出站身份（.123 候选）

两处泄漏都来自「Aether 换了 thread / turn，但把客户端按**入站** thread / turn 算出来的派生值原样透传」：

- **`turn_started_at_unix_ms`**：多个真实 turn 折到一条合成 turn 上，若照抄客户端的值，同一出站 `turn_id` 会在每个请求上带不同的开始时刻。现在 turn / compaction 一律写出站 turn UUIDv7 的毫秒数（prewarm 不带），客户端自己的值被丢弃（含 `BLOB_PASS_KEYS` 的透传回路）。复核用 §3.5 的 Q12b。
- **`msg_` / `at_` 前缀缓存 id**：codex-rs `responses_lite` 下 `input[]` 里基础指令 developer 消息与 `additional_tools` 项的 id 是 `uuid5(uuid5(OID, thread_id), payload)`（`core/src/client.rs:936-990`），可以从 id 反推「这条历史属于哪个 thread」。`codex_environment_context.rs` 的 `rewrite_prefix_cache_item_ids` 在 env pass 开头把这类 id 按出站 thread 重推：只动后缀为 UUIDv5 且能拿到 payload（`at_` 取 `tools` 序列化，`msg_` 取 `content` 文本）的项；UUIDv7 的用户 / developer 消息 id、服务端 `fc_` / `rs_` id 不动。HTTP 与 WS 两个表面都走同一函数，同受 `AETHER_CODEX_ENVIRONMENT_CONTEXT_REWRITE` 开关。

日志计数：

```bash
docker logs aether-app --since 30m 2>&1 | grep 'codex_env_context_rewritten' | grep -oE 'prefix_cache_ids_rewritten=[0-9]+' | sort | uniq -c | sort -rn | head
```

期望：Codex Pro 带 lite 历史的请求上该计数通常为 1–2（一条 `msg_` 基础指令 + 一条 `at_`），不带 lite 历史的请求为 0。出站 body 不落库，要逐条核对只能抓包：出站 `at_` / `msg_` 的 v5 后缀应等于 `uuid5(uuid5(NAMESPACE_OID, 出站 thread_id), payload)`，与入站请求里的值不同。

**同批改动**（与本手册无观测项）：新导入 / 新建 / 重新授权后缺值的 Codex OAuth 账号 `concurrent_limit` 默认 1（与 Grok 一致），后台可手动改，显式值（含 0 = 不限）不会被覆盖；`instructions` / `service_tier` 按操作员决定不改。~~`workspaces` 不改~~ **`.127` 候选起 `workspaces` 由 profile pass 按账号合成**（见下）。

### 3.9 `workspaces` 合成（`.127`）

出站 blob 的 `workspaces` 不再是 downstream 的真实仓库根 / 私有仓库 / 真实 commit：profile pass 把每个入站仓库根换成该账号自己的合成仓库（home 目录名与 GitHub owner 由 key 的 Codex 授权文件邮箱派生，OS 布局由 profile UA 决定，仓库名与 commit 按账号哈希 + 时间周期推出）。五个表面（HTTP 头、body `client_metadata`、Search 头、WS 握手头、WS step body）同时生效，头和 body 的 blob 字节相同。判定方法：

```bash
# 入站（request_headers）里仍是真实路径；出站（provider_request_headers）才是合成后的
# 注：出站头只在 body capture 打开、且该请求是 HTTP 时才落行；WS 请求没有 audits 行。
docker exec -i aether-postgres psql -U postgres -d aether -x -c "
select
  (request_headers::jsonb->>'x-codex-turn-metadata')::jsonb->'workspaces' as inbound_workspaces,
  (provider_request_headers::jsonb->>'x-codex-turn-metadata')::jsonb->'workspaces' as outbound_workspaces
from usage_http_audits
where jsonb_typeof(provider_request_headers::jsonb) = 'object'
  and provider_request_headers::jsonb ? 'x-codex-turn-metadata'
order by created_at desc limit 1;"
```

```bash
# 账号的合成身份：邮箱来源与 home / owner
docker exec -i aether-postgres psql -U postgres -d aether -x -c "
select id, name, fingerprint->'codex_client_profile'->'workspace_identity' as workspace_identity
from provider_api_keys
where provider_id = '<codex pool provider id>';"
```

期望：`user_name` / `remote_owner` 与该 key 授权文件里的邮箱一致（本地部分去 `+tag` 后转小写，`user_name` 只留字母数字、`remote_owner` 非字母数字变 `-`）；`source=auth_email`。出现 `source=fallback` 说明授权文件里没有可用邮箱——重新授权 / 导入带邮箱的授权文件后，**下一次批量刷新**（或下次 profile materialize）会升级成邮箱名；已落库的身份不会被后续邮箱变更改写。若 `workspace_identity` 整个缺失（旧 profile，没有 `source` 字段就会按 `auth_email` 处理）：解析时按邮箱实时派生，与刷新后落库的值一致。出站 blob 里应看不到 `/Users/<真实用户>`、真实 owner / 仓库名或 40 位真实 commit；合成 commit 会在每账号固定的 1–3 天周期边界换一次，这是预期，不是异常。

**残留**：`<environment_context>` 的 `<cwd>` / `<filesystem>` 仍带真实路径（操作员明确不改）；`tool_namespaces_info` 原样转发。

### 3.10 客户端发版跟随（`.127`）

池账号的冻结 `user-agent` 不再永久停在导入时的 build：注册表按 originator family 记录入站真实客户端跑过的稳定版本，每个账号按自己的错峰窗口（≤4 天，由选择指纹决定）采纳最新版本，**只换版本 token 两处**（产品段与末尾构建后缀），OS / arch / 终端 / originator / `installation_id` / 合成 thread 全冻结。末尾后缀只在它**等于冻结版本**时跟随（`(VS Code; 26.901.22334)` 这类客户端真实 build 号不动）。WS 握手的有效 UA 会抄给同一连接的每步 body，握手与 step body 的 build 一致。

```bash
# 1. 注册表：每个 family 有哪些版本，各自最近一次被真实客户端用到的时间
docker exec -i aether-redis redis-cli --scan --pattern 'aether:codex:client_release:v1:*'
docker exec -i aether-redis redis-cli zrange 'aether:codex:client_release:v1:<provider_id>:codex-tui' 0 -1 WITHSCORES
```

成员形如 `0.154.0:1789094091:1789150000`（版本:首见秒:末见秒），score 是末见秒。期望：成员数在个位数（上限 24），最新版本的首见秒 ≤ 现在；某个版本末见秒超过 30 天会被条目清理自动删掉。

```bash
# 2. 跟随是否发生（每次跟一条 debug，注意日志级别）
docker logs aether-app --since 30m 2>&1 | grep 'codex pool client release follow' | tail
```

事件字段 `frozen_user_agent` / `effective_user_agent` 应只差版本 token（产品段与后缀同时前移）。同一条 connection 的 WS step body 与握手 build 若不一致，属于 bug，不是配置问题。

```bash
# 3. 版本分布：池账号当前各报什么 build（后台 key 行）
docker exec -i aether-postgres psql -U postgres -d aether -c "
select fingerprint->'codex_client_profile'->>'user_agent' as frozen_user_agent, count(*)
from provider_api_keys
where provider_id = '<codex pool provider id>'
group by 1 order by 2 desc;"
```

注：第 3 条查的是**落库的冻结 profile**，不包含运行时的跟随结果——跟随只改单次出站请求，不回写数据库。要看出站真实 UA 只能看 debug 日志（上一条）或抓包。期望分布是集中在最近几个 build 上、彼此相差 1–2 个 patch/minor，而不是全体同一个 build（说明错峰没生效）或散落十几个版本（说明注册表没在收敛）。

```bash
# 4. 账号的合成仓库集合（裁定 4：一个账号一天最多 2 个仓库）
docker exec -i aether-postgres psql -U postgres -d aether -c "
select id, name,
       fingerprint->'codex_client_profile'->'workspace_identity' as workspace_identity
from provider_api_keys
where provider_id = '<codex pool provider id>';"
```

「一天最多 2 个仓库」是**合成侧**的不变量，不在库里有现成列：每个账号有一个 3–8 个仓库的集合（`WORKSPACE_MIN_REPOS` / `WORKSPACE_MAX_REPOS`），任意一个「开发者日」（本地凌晨 03:00–06:00 之间随机起点，`WORKSPACE_DAY_START_*`）内只启用其中两个——主项目（周期 3–10 天）与副项目（1–3 天），入站仓库根按「哈希(种子, 入站根路径)」映射到这两条 lane 中的一条（主:副 = 2:1，`WORKSPACE_PRIMARY_LANE_WEIGHT`）。所以：

- 同一 downstream thread 的多个不同仓库根会落在同一条 lane 上，整条对话只显示一个合成仓库；不同入站仓库根才可能跨到另一条 lane，因此**一个开发者日**最多两个不同合成仓库根。
- 直接按**自然日**从出站 blob 去重计数可能看到 4 个：日界不是本地午夜，而是 03:00–06:00 之间，一个自然日能横跨两个开发者日。要精确核对得按开发者日口径（本地时间减去约 3–6 小时后按日切片），或直接看代码不变量 `synthetic_workspace_layout_is_per_account_and_bounded`（单测里滚动 60 个开发者日、每步断言当日不同仓库根 ≤2）。
- 出站 blob 里应看不到 `/Users/<真实用户>`、真实 owner / 仓库名或 40 位真实 commit；合成 commit 会在每账号每仓库自己的 3h–2d 周期边界换一次（`WORKSPACE_COMMIT_PERIODS_SECS`），这是预期，不是异常。

### 3.11 传输层保真：头顺序 / zstd / Cloudflare cookie / installation-id 头（`.130`）

依据是官方直登 codex-rs 0.154.0 的抓包分析（`github.com/Shirtiny/codex-cli-network-analyze`，`ws-sse/analysis/sse-capture.md`、`sse-all-headers.csv`、`ws-protocol.md`）对照 codex-rs 源码。三处与官方不一致的传输层形状，`.130` 起对**Codex 类型 provider + endpoint base_url 为 `https` 的 ChatGPT 主机**（`chatgpt.com` / `chat.openai.com` / `chatgpt-staging.com` 及其子域，镜像 / 中转不算）自动生效，与「会话身份合成」开关无关：

| 项 | 官方直登 | `.130` 前的 Aether | `.130` |
| --- | --- | --- | --- |
| HTTP 头顺序 | `version` → `x-codex-beta-features` → [`x-codex-turn-state`] → `x-codex-window-id` → `x-codex-turn-metadata` → `x-openai-internal-codex-responses-lite` → `x-codex-routing-hint` → `x-client-request-id` → `session-id` → `thread-id` → `accept` → `content-encoding` → `content-type` → `authorization` → `chatgpt-account-id` → `originator` → `user-agent` → `cookie` → `host` → `content-length` | `BTreeMap` 字母序（`accept, authorization, chatgpt-account-id, content-type, originator, session-id, …`） | 按官方顺序（`execution_runtime/transport.rs` `CODEX_CLI_LEADING_HEADER_ORDER` / `CODEX_CLI_TRAILING_HEADER_ORDER`），未列出的头夹在中段保持原顺序 |
| `/responses` 请求体 | `zstd` level 3 + `content-encoding: zstd`（`http-client/src/request.rs`，`EnableRequestCompression` 自 0.120.0 起默认开） | 明文 JSON，无 `content-encoding` | 只对 `/responses` 表面 zstd；`/responses/compact` 与 header-only 表面（search / chat / family / image）保持明文，与官方一致 |
| Cloudflare cookie | 进程级 jar 只存 `__cf_bm` / `_cfuvid` / `__cflb` 等 CF 名单 cookie（`http-client/src/chatgpt_cloudflare_cookies.rs`，≥0.143.0），每个 HTTP 请求回放 `cookie: _cfuvid=…; __cf_bm=…; __cflb=…`；WS 握手不带 | 每个请求都无 cookie（每一 turn 都像新进程） | 网关内存 jar，**按账号 key id × 主机**隔离（`execution_runtime/chatgpt_cloudflare_cookies.rs`），名单与官方相同，RFC 6265 过期 / 路径 / Domain 处理；WS 握手不带 |
| HTTP 版本 / TLS | HTTP/1.1（reqwest 0.12 native-tls 无 `native-tls-alpn` → 不发 ALPN） | 已一致：Codex 默认 transport profile `codex-reqwest-default-tls-auto` 走 native TLS 无 ALPN | 不变 |
| `x-codex-installation-id` 请求头 | **只在 `/responses/compact` 上发**（`core/src/client.rs` 里它是 compact extra headers 的第一个；`build_responses_options` / `responses_metadata.compatibility_headers()` 都不加）。抓包里 `/responses`、`/alpha/search`、`/models`、analytics 均无此头；installation id 只出现在 `x-codex-turn-metadata` 与 body `client_metadata` 里 | profile pass 给**每个**请求都加该头（身份合成 pass 靠它取 installation id 填 blob 与 `client_metadata`） | 身份合成之后、endpoint header rules 之前，非 compact 表面（`/responses`、header-only、WS 候选头）剥掉该头；blob 与 `client_metadata` 里的 installation id 不变（`align_codex_installation_id_header_with_surface`） |

实现方式：planner 在 `apply_codex_pool_runtime_identity` 末尾给出站头加三个内部控制头 `x-aether-execution-header-order: codex-cli`、`x-aether-execution-request-body-encoding: zstd`（仅 `/responses`）、`x-aether-execution-cookie-jar: chatgpt-cloudflare`；执行传输层读取后**按前缀 `x-aether-execution-` 整体剥掉**再出站（未知控制头也不会漏到上游）。`x-codex-installation-id` 的剥除与三个控制头同一作用域、同一 kill switch。

```bash
# 1. cookie jar 是否在工作（debug 级别；每次响应带 set-cookie 且被名单接受时一条）
docker logs aether-app --since 30m 2>&1 | grep -E 'cookie jar (stored|value)' | tail
```

期望：上线后每个账号首个请求之后就能看到 `stored`，随后大多数请求不再打印（CF 只在刷新 `__cf_bm` 时重发 set-cookie，约 30 分钟一次）。`is not a valid header value` 出现说明上游发了非 ASCII 的 cookie 值，属异常。

```sql
-- 2. 审计里的出站头不应再出现内部控制头；`content-encoding` 在 /responses 上应为 zstd
select count(*) n,
       count(*) filter (where exists (select 1 from jsonb_object_keys(oh) k where k like 'x-aether-execution-%')) leaked_controls,
       count(*) filter (where oh->>'content-encoding' = 'zstd') zstd
from (select h.provider_request_headers::jsonb oh from usage_http_audits h join usage u on u.request_id=h.request_id
      where h.created_at >= :'since' and u.provider_name='Codex Pro'
        and jsonb_typeof(h.provider_request_headers::jsonb)='object') s;
```

注意审计里的 `provider_request_headers` 记录的是 planner 输出（**含**控制头、字母序、还没有 `cookie` / `content-encoding`），不是线缆形状；`leaked_controls` 在审计里为正是**预期**的，真正的出站形状只能抓包（`tcpdump` 到 443 看不到明文；用官方分析仓库的 mitm 方法对网关出口抓）。表中 `zstd` 列同理只有 planner 已写入 `content-encoding` 时才计数，正常为 0。真正要核对的是抓包：每个 `/backend-api/codex/responses` 请求头顺序与上表一致、`content-encoding: zstd`、第二个请求起带 `cookie`。

## 4. 日志事件与处置

```bash
docker logs aether-app --since 2026-09-05T06:40:00Z 2>&1 | grep -E 'codex_rid_' | cut -c1-400
```

容器日志是 json-file 驱动（10×100m），容器重建后旧日志即丢失；仓库 `logs/` 目录为空。要留证据先导出。

| 事件 | 含义 | 处置 |
|---|---|---|
| `codex_rid_config_invalid` | 号池 `pool_advanced.codex_runtime_identity` 形状/范围不合法 | 该池当次请求按关闭处理（透传）。到管理后台重新保存卡片；校验规则见设计文档 §6、§18.7 |
| `codex_rid_store_unavailable` | Redis 不可用，取不到槽位/freeze | 按设计回退：HTTP 透传入站身份或用进程内快照（设计文档 §7.5、§18.3）。先看 `docs/operations/redis-runtime-runbook.md`，Redis 恢复后自愈；持续出现说明 Redis 出问题，不是本功能问题 |
| `codex_rid_chain_freeze_miss` | 带 `previous_response_id` 或跨路径接续时找不到 freeze | 按 §7.1 正常分配 thread（不透传）。偶发正常（freeze TTL 到期、跨日）；集中出现查 Redis TTL 与时钟 |
| `codex_rid_thread_reused`（debug 级，.108） | 该账号 thread 额度已用满（.110 起按最近 24h），新对话复用了最久没有新 turn 的 thread | 正常行为，忙账号每天都会出现。若某账号几乎每个新对话都触发（复用远多于 mint），说明上限相对该账号的流量偏低，酌情上调 `expected_threads_per_day`；不出现则说明流量没到上限 |
| `codex_rid_turn_steered`（debug 级，.110） | 同一出站 thread 上，被取代的旧入站 turn 回放时 steer 到当前 open turn（不回访旧 turn id，只刷 TTL） | 正常行为。thread 内 turn 交错时保证出站 turn id 单调向前；集中出现只说明该账号有大量乱序 / 重放请求，不需处理 |
| `codex_rid_turn_budget_exceeded`（debug 级，.110） | 该账号最近 24h 的 turn 台账已满 `turn_bound`，本请求折回已有 thread、不再开新 turn | 正常的账号级硬顶生效。若某账号频繁触发，说明流量下该上限把可见 turn 压得偏紧，可上调 `expected_turns_per_day`（但别超约 100 的风控线）；这是把账号可见 turn 压到官方阈值以下的预期机制 |
| `codex_rid_unknown_metadata_key` | 客户端带了三表面白名单之外的键，已被删除；每进程每 (surface, key) 只 warn 一次 | 走 §4.1 判定 |
| `codex_env_tz_resolved`（info，.122，每进程一次） | `<environment_context>` 归一化选定的目标时区：`tz` 与 `source`（`env_override` / `tz_env` / `localtime` / `fallback`） | 本机期望 `America/New_York` / `tz_env`。出现 `fallback` 说明 compose 没给 `TZ` 或给了非法 / 被拒值，先看同批 `codex_env_tz_rejected` |
| `codex_env_tz_rejected`（warn，.122） | 某个时区候选被拒：`source`、`value`、`reason`（`empty` / `denied_region` / `unknown_iana_name` / `not_a_region_city_zone`） | `denied_region` = 配了中国时区，绝不能放行，改 `TZ`；`not_a_region_city_zone` = `UTC` / `Etc/*` / `EST` 之类，换成 `Region/City` 形。修完重建 `app` |
| `codex_env_context_rewritten`（有删 / 插 / 追加时 info，否则 debug，.122） | 本请求 `<environment_context>` 改写计数：`surface`、`thread`、`blocks_seen`、`timezone_rewritten`、`date_rewritten`、`blocks_removed`、`blocks_inserted`、`blocks_appended`、`instant_source_*`、`unknown_child_tags`、`user_location_removed`、`prefix_cache_ids_rewritten`（.123 候选：按出站 thread 重推的 `msg_` / `at_` UUIDv5 前缀缓存 id 数） | 正常行为。`instant_source_heuristic` 长期占比高说明大量无 id 旧客户端；`blocks_removed` 持续为 0 而下游时区不是目标时区，检查 kill switch 是否被关 |
| `codex_env_unknown_child_tag`（首次 warn、之后 debug，.122） | `<environment_context>` 里出现解析器不认识的顶层子标签，块按「含未知」处理：只改 tz / 日期，不判重不删 | 到 codex-rs `core/src/context/world_state/environment.rs` 看新标签是否为官方新增标量；是则加进解析器与判重集合并补单测 |

### 4.1 未知键判定流程（白名单维护）

白名单常量都在 `apps/aether-gateway/src/codex_runtime_identity.rs` 顶部：`BLOB_IDENTITY_KEYS` / `BLOB_NORMALIZED_KEYS` / `BLOB_LEAK_KEYS` / `BLOB_PASS_KEYS`（turn-metadata blob），`FLAT_IDENTITY_KEYS` / `FLAT_LEAK_KEYS` / `FLAT_PASS_KEYS`（扁平 `client_metadata`），`HEADER_PASS_KEYS` / `HEADER_STRIP_KEYS`（`x-codex-*` 等前缀头）。.110 起另有 `BLOB_APP_SERVER_KEYS = ["turn_trigger", "workspace_kind"]`：这两个键只在 app-server（`codex_vscode` 等 IDE）originator 下出现，终端 / 无头出站 UA（originator 前缀在 `TERMINAL_ORIGINATOR_PREFIXES`：`codex-tui/` / `codex_exec/` / `codex_cli_rs/`）或无 UA 时按出站客户端形状从 blob 删除。所以某个 pass 键在终端 UA 下不出现是预期，不是漏配。

1. 在本地 codex-rs 里找这个键：
   ```bash
   cd /opt/stacks/openai-codex
   git log -1 --format='%h %cd'
   grep -rn '"<key>"' codex-rs/core/src/responses_metadata.rs codex-rs/core/src/turn_metadata.rs
   git log --date=short --format='%h %ad %s' -S'<key>' -- codex-rs/core/src/responses_metadata.rs
   ```
2. 判定：
   - 键在 `RESERVED_METADATA_KEYS` 里但**没有**发射代码（注释「removed inventory」）→ 官方已移除，当前客户端不发。**不加白**，删除是对的。案例：`code_mode_tool_names`，07-25（#35271）引入、08-07（#37500）移除；2026-09-05 一台 codex-tui/0.147.0 旧客户端还在发，出站 UA 档位（Desktop 0.149/0.150）均在移除之后，删掉与出站版本一致。
   - 键由当前 codex-rs 发射、不含身份（工具清单、开关、时间戳）→ 加进对应 `*_PASS_KEYS`，并在 `whitelist_strips_unknown_keys_on_every_surface` 等单测里补断言。
   - 键含 thread/turn/session 类身份 → 归入 `*_IDENTITY_KEYS`（需要改写规则）或 `*_LEAK_KEYS`（只在子/fork thread 出现的一律删），并补改写逻辑与测试。
   - 键是 Aether 自己的控制字段（`sub2api_*`、`aether.*`）→ 已由 `FLAT_CONTROL_PREFIXES` 处理，不该报；报了就是控制字段命名漂移。
3. 原则：任何真实 codex-rs 单一版本产生不了的确定性形状都算缺陷，优先级高于「少泄漏」。加白之前先问「出站 UA 那个版本的 codex 会不会发这个键」。

### 4.2 `.110` 上线后复核（发版后按此三查）

`.110` 把 turn 上限改成账号级、最近 24h 滚动的硬顶，并保证一条出站 thread 内 turn id 单调不回访、终端 UA 出站 blob 不带 app-server-only 键。上线后用容器启动时间之后的窗口（`docker inspect aether-app --format '{{.State.StartedAt}}'`）跑下面三查。

**查一：每账号最近 24h 出站 distinct thread / turn ≤ 上限。** 按 blob 里的 `installation_id` 假名分组（一账号一 installation）：

```sql
-- psql -v since='<StartedAt 或 now-24h>' -f post_110_ceilings.sql
with r as (
  select (h.provider_request_headers::jsonb->>'x-codex-turn-metadata')::jsonb b,
         h.provider_request_headers::jsonb->>'thread-id' th
  from usage_http_audits h join usage u on u.request_id=h.request_id
  where h.created_at >= :'since' and u.provider_name='Codex Pro' and u.status_code=200
    and jsonb_typeof(h.provider_request_headers::jsonb)='object')
select left(md5(b->>'installation_id'),6) acct6,
       count(distinct th) distinct_threads,
       count(distinct b->>'turn_id') distinct_turns
from r where b ? 'installation_id' group by 1 order by 2 desc, 3 desc;
```

期望：每个 `acct6` 的 `distinct_threads` ≤ 该池 thread 上限、`distinct_turns` ≤ turn 上限（抖动后实际额度在 `[⌈上限/2⌉, 上限]`，可能更低）。任一列超过配置上限即缺陷，查 Redis 台账 `…:threads` / `…:turns` 的 `ZCOUNT` 与准入逻辑。

**查二：同一出站 thread 内 turn id 不回访（应 0 行）。** 用「岛屿分组」把一个 thread 里同一 turn id 的连续出现压成一段，若某 turn id 在同一 thread 出现于两段以上，就是回访：

```sql
with r as (
  select h.created_at ts,
         h.provider_request_headers::jsonb->>'thread-id' th,
         (h.provider_request_headers::jsonb->>'x-codex-turn-metadata')::jsonb->>'turn_id' tid
  from usage_http_audits h join usage u on u.request_id=h.request_id
  where h.created_at >= :'since' and u.provider_name='Codex Pro' and u.status_code=200
    and jsonb_typeof(h.provider_request_headers::jsonb)='object'),
runs as (
  select th, tid,
         row_number() over (partition by th order by ts)
           - row_number() over (partition by th, tid order by ts) grp
  from r where tid is not null and th is not null)
select left(md5(th),6) th6, count(distinct grp) segments
from runs group by th, tid having count(distinct grp) > 1 order by segments desc;
```

期望 0 行。有行说明该 thread 的出站 turn id 来回跳（`.109` 及之前的交错缺陷复现），查 `resolve_turn` 的 open turn / steer 分支。

**查三：终端 / 无头出站 UA 的 blob 不带 app-server-only 键（应 leaked=0）。** `turn_trigger` / `workspace_kind` 只该在 app-server（`codex_vscode` 等）originator 的出站 UA 下出现：

```sql
select count(*) filter (
         where (b ? 'turn_trigger' or b ? 'workspace_kind')
           and (ua is null or ua = '' or ua ~ '^(codex-tui|codex_exec|codex_cli_rs)/')
       ) leaked,
       count(*) n
from (select (h.provider_request_headers::jsonb->>'x-codex-turn-metadata')::jsonb b,
             h.provider_request_headers::jsonb->>'user-agent' ua
      from usage_http_audits h join usage u on u.request_id=h.request_id
      where h.created_at >= :'since' and u.provider_name='Codex Pro'
        and jsonb_typeof(h.provider_request_headers::jsonb)='object') s;
```

期望 `leaked = 0`。有值说明 `OutboundClient::from_user_agent` 的 app-server 判定或 blob 删键漏了（注意池档位 UA 若是 `Codex Desktop` / `codex_vscode` 属 app-server，保留这两键是对的，不计入 leaked）。

## 5. 版本核对：codex-rs 基准

设计文档 §1–§18.12 按 codex-rs `357696c5` 复核，§18.13 起按 `07f18d5f`（2026-09-05）。再次核对时先 `git -C /opt/stacks/openai-codex pull`（shallow clone，历史只到 2026-06-27），关键文件：

- `codex-rs/core/src/responses_metadata.rs`：blob / 扁平 metadata 的键集合、`RESERVED_METADATA_KEYS`、`has_turn_identity`。
- `codex-rs/core/src/turn_metadata.rs`、`sandbox_tags.rs`：turn-metadata 头的形状。
- `codex-rs/core/src/client.rs`：`prompt_cache_key` 规则（override → `internal_<source>:<parent>` → session_id）、`compact_conversation_history`。
- `codex-rs/codex-api/src/requests/headers.rs`：`build_session_headers`（只有 dash 形式 `session-id` / `thread-id`）。
- `codex-rs/codex-api/src/endpoint/responses.rs`、`endpoint/compact.rs`：`/responses` 加 `x-client-request-id` = thread，compact 不加。
- `.130` 传输层保真的基准（按 0.154.0 抓包 + 源码）：`core/src/client.rs` `build_websocket_headers`（WS 握手头顺序）、`build_responses_options`（HTTP 头组装顺序；线缆顺序以抓包为准）；`http-client/src/request.rs`（`zstd::stream::encode_all(.., 3)` + `content-encoding: zstd`，只在 `/responses`）；`http-client/src/chatgpt_cloudflare_cookies.rs`、`chatgpt_hosts.rs`（cookie 名单、允许主机）；`features/src/lib.rs` `EnableRequestCompression`（Stable，默认开）；`core/src/client.rs` compact 路径的 `X_CODEX_INSTALLATION_ID_HEADER`（只有 compact 发这个头）与 `core/src/responses_metadata.rs` `compatibility_headers()`（window-id / turn-metadata / parent-thread / subagent，不含 installation-id）。再核对时看这几处有没有新头 / 新 cookie 名 / 压缩范围变化。

## 6. 回滚与关闭

按影响从小到大：

1. **关开关**（秒级，无需部署）：号池高级设置里把「会话身份合成」关掉。出站立刻回到功能前的形状（入站身份透传 + Aether 填充器的 `session_id` / `conversation_id` 短头与随机 `x-client-request-id`）。上游会看到该账号的 thread 从合成身份切回真实身份，这是可接受的一次性跳变。
2. **镜像回滚**：按 `docs/operations/release-and-container-update-spec.md`，恢复对应 `.env.bak.<ts>_pre_vX` 的 `APP_IMAGE`，只重建 `app`，须操作员明确授权。**不要回到 .104**（带缓存回退）；.105 是含缓存修复的最低版本；再往前请回 .103 并关开关。
3. **Redis 键**：`ap:{provider_id}:codex_rid:*` 都有 TTL，回滚后自然过期。不要手动清：清掉等于让所有活跃 thread 换身份，上游看到一批新 thread。
4. **只关 `<environment_context>` 归一化（.122）**：给 `app` 容器加环境变量 `AETHER_CODEX_ENVIRONMENT_CONTEXT_REWRITE=off`（compose `environment:`）并只重建 `app`，秒级；身份合成不受影响。关掉后出站 `<timezone>` / `<current_date>` 立刻回到下游真实值，上游会看到该账号时区跳变一次。要换目标时区而不是关掉：改 `TZ`（或 `AETHER_CODEX_ENVIRONMENT_TIMEZONE`）后重建 `app`，中国时区会被拒绝并回退到 `America/New_York`（看 `codex_env_tz_rejected`）。
5. **只关传输层保真（.130）**：给 `app` 容器加 `AETHER_CODEX_TRANSPORT_FIDELITY=off` 并只重建 `app`，秒级。关掉后出站头回到字母序、`/responses` 体回到明文、不再回放 CF cookie、`x-codex-installation-id` 头回到每个请求都带，四项一起关（没有单项开关；单项异常请回滚镜像）。WS 握手头顺序不受此开关影响（那是 WS 运行时的固定组装顺序，没有开关）。

## 7. 已知限制（不需要处理，只需知道）

详见设计文档 §15、§18.14.5。要点：

- `prompt_cache_key` 改写只认 `guardian:` 前缀与等于入站 session 两种；官方 Internal 会话（memory_consolidation）发的 `internal_<source>:<parent_thread_id>` 原样透传（真实 parent thread 泄漏，约 1 请求/天）。
- 合成请求（无元数据）的压缩探测不到：window 永远 0；中转端若把摘要替换掉首条 prompt，会被当成新 thread。
- 同一入站 thread 先经第三方中转再切回 Codex Pro，历史里的 65 字符 `rs_` item id 会触发官方 400 重试循环，与身份合成无关，待独立设计。
- 出站 UA 由 `pool_advanced.codex_client_headers`（「稳定客户端请求头」）在账号物化时冻结进 key 指纹；与本功能无关但会影响「该版本会不会发某键」的判定。.107 起内置字典是 23 组 0.153.x，但 Codex Pro 号池自己填了 32 组 0.149–0.151 自定义 UA，改字典不会自动生效：要么在卡片里把自定义列表替换成 0.153.x（或清空以跟随内置字典），再点「一键更新 UA」（会同时保存卡片当前配置；只换 UA / originator，保留 `installation_id`；列表没变时点它不会改任何账号的 UA）。gpt-6 在客户端版本低于 0.153 时上游返回 400，线上 7 天内尚未观察到这种 400，但账号 UA 落后于入站主流本身就是可见偏差。
- `version` 头：codex-rs 对 ChatGPT 后端的每个请求都带 `version: <build 版本>`，恒等于 UA 里的版本。但这个头只挂在内置 `openai` provider 上（`model-provider-info/src/lib.rs:397-401` `create_openai_provider`），客户端把 base_url 指向 Aether 或中转时用的是自定义 provider，所以**入站根本没有这个头**：2026-09-05 线上 30 分钟 808 条 Codex Pro HTTP 请求入站 `version` 为 0 条，出站也为 0 条，即 .106 及之前上游看到的 Aether 流量 100% 缺 `version`，这是真实客户端产生不了的形状。.106 的逻辑是「有入站就透传」（Search 表面遇到 `Codex Desktop/...` UA 还会把它整个删掉），实际从未生效。.107 起所有写 UA 的地方同时把 `version` 写成出站 UA 的版本，出站覆盖率应从 0 变成 100%。核对 SQL：出站头里 `version` 应等于 `user-agent` 第一个括号前 `/` 后的那段：

```sql
select count(*) n,
       count(*) filter (where oh ? 'version') has_version,
       count(*) filter (where oh->>'version' = split_part(split_part(split_part(oh->>'user-agent','(',1),'/',2),' ',1)) version_matches_ua
from (select h.provider_request_headers::jsonb oh from usage_http_audits h join usage u on u.request_id=h.request_id
      where h.created_at >= :'since' and u.provider_name='Codex Pro'
        and jsonb_typeof(h.provider_request_headers::jsonb)='object') s;
```

期望 .107 后 `has_version = version_matches_ua = n`。

- **`<environment_context>` 时区 / 日期归一化（.122）的残余**：只改 `<timezone>` / `<current_date>`，`<cwd>` / `<shell>` 与出站 UA 的 OS 可能不一致（PowerShell 路径配 macOS UA），用户已接受。请求时刻本身改不了：`turn_started_at_unix_ms` 是 UTC epoch，上游始终看得到真实作息节律（14 天样本按美国时区 41–45% 落在本地 00–07 点），要治只能在调度层按账号分「活跃时段」，属独立计划。上线一刻正在进行的线程会被一次性改写历史（tz 换、部分日切块删 / 插），prompt cache 失一次后稳定。无 id 的旧客户端形状只能 best-effort。WS 增量步的状态只活在进程内：重启或换绑后的第一个增量步若正好跨午夜会漏一块，下一 turn 的全量回放会补上。宿主 `/etc/timezone` 陈旧为 `Europe/Berlin` 不影响容器（容器走 `TZ`）。详见 `docs/architecture/codex-environment-context-time-normalization-plan-2026-09-10.md` §9。
- **传输层保真（.130）的残余**：（a）Cloudflare cookie jar 只在网关进程内存里（上限 4096 个账号×主机 jar、每 jar 32 个 cookie，会话 cookie 24h 不用即失效），重启后第一个请求又是无 cookie 的「新进程」形状；多实例部署各自一份 jar，同一账号跨实例会呈现两套 `__cf_bm`，与官方「一个进程一个 jar」的语义相比是可见但轻微的差异。（b）走 `aether-tunnel` 中继（`proxy.mode = tunnel`）的请求，头顺序在中继协议里被转成 `BTreeMap` 后丢失，中继出口仍是字母序；cookie / zstd 不受影响。直连与浏览器 wreq 后端保持顺序。（c）`x-openai-internal-codex-residency` / `x-oai-attestation` / `x-openai-memgen-request` 抓包里没有，位置按源码组装顺序排，若官方线缆顺序不同属可接受偏差。（d）官方 `/responses` 之外的 HTTP（compact、search）本来就不压缩，Aether 同样不压缩；若未来 codex-rs 扩大压缩范围需要跟进。（e）不做任何 TLS 指纹层面的改动：Codex profile 已是 native TLS 无 ALPN、HTTP/1.1，与官方一致。
