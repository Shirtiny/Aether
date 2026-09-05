# Codex 号池「会话身份合成」运维手册

> 对象：接手维护 `pool_advanced.codex_runtime_identity` 的人或 AI。设计与算法在
> `docs/architecture/codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`（下称「设计文档」），本文只讲线上怎么看、怎么判、怎么退。
> 最后更新：2026-09-05（线上 `backend-v0.7.106`；`.108`（含 `.107` 候选内容）在 `custom` 分支发版中）。

## 1. 现状速览

| 项 | 值 |
|---|---|
| 功能 | Codex OAuth 号池选号之后，把上游可见的 `session_id` / `thread_id` / `turn_id` / `window_id` 改写成「每账号每日少量 thread / turn」的合成身份；入站官方 ID 一律不动（sticky、WS 绑定、fence、用量都读入站） |
| 开关 | 号池高级设置 →「会话身份合成」卡片；JSON 为 `pool_advanced.codex_runtime_identity {enabled, expected_threads_per_day 1..=64, expected_turns_per_day 1..=512}`；缺省关闭。两个数字是 **每日上限**：.107 起当天实际额度按账号、按天（turn 再按 thread）在 `[⌈N/2⌉, N]` / `[⌈M/2⌉, M]` 内确定性抖动，.106 及之前是固定模数；.108 起新对话按到达顺序各开一条 thread，当天额度用满后复用最久没有新 turn 的那条（.107 及之前按哈希分槽）。一个人日常用 codex 大约几条到十几条 thread，上限建议 8–16；Codex Pro 现填 32 偏高 |
| 线上 | `backend-v0.7.106`（2026-09-05 06:40 UTC）；Codex Pro 号池开着 32 thread/天、256 turn/天 + 请求体/响应体捕获。.104 于 02:36 UTC、.105 于 06:05 UTC 同日上线 |
| 代码 | `apps/aether-gateway/src/codex_runtime_identity.rs`（算法、白名单、四个表面）；HTTP 挂点 `ai_serving/planner/standard/openai/responses/decision/request.rs`、`ai_serving/planner/standard/codex.rs`；WS 挂点 `codex_ws/runtime.rs`；配置校验 `handlers/admin/provider/write/normalize.rs`；前端号池高级设置卡片 |
| 状态存储 | Redis `ap:{provider_id}:codex_rid:{selection_fp}:...`（thread 到达序号、当天 thread 名册 ZSET（.108）、turn 槽、root/turn freeze、window），全部带 TTL；进程内还有 WS 候选快照 |
| 观测 | 日志事件 `codex_rid_config_invalid` / `codex_rid_store_unavailable` / `codex_rid_chain_freeze_miss` / `codex_rid_unknown_metadata_key` / `codex_rid_thread_reused`（§4） |
| 部署记录 | 仓库根 `容器更新历史.md`（操作员本地文件，未纳入 git）+ `.env.bak.<ts>_pre_vX`；更新流程见 `docs/operations/release-and-container-update-spec.md` |
| 官方源码基准 | 本地 checkout `/opt/stacks/openai-codex`（codex-rs），**不要上网查**；核对前先 `git -C /opt/stacks/openai-codex log -1 --format='%h %cd'` 记下版本 |

版本演进（详见设计文档 §18.10–§18.14）：

| tag | 内容 | 线上 |
|---|---|---|
| .101 / .102 | 功能落地 + code-review 修复 | 未单独上线 |
| .104 | window 按合成 thread 跟踪压缩；出站字段白名单 | 2026-09-05 02:36 UTC，**引入缓存回退**（§3.4） |
| .105 | 删 Aether 自补短头、`x-client-request-id` = 出站 thread、无元数据请求合成、`HttpResponses` 补齐官方四头、`x-trace-id` 黑名单 | 2026-09-05 06:05 UTC，缓存恢复 |
| .106 | `HttpCompact` 表面补齐 `session-id` / `thread-id` / `x-codex-window-id`，删 `x-client-request-id` | 2026-09-05 06:40 UTC |
| .107（候选，未单独发版） | 每日 thread / turn 槽数上限按账号、按天抖动（设计文档 §7.0、§18.15）；内置 Codex UA 字典换成 23 组线上观察到的 0.153.x（gpt-6 要求 ≥ 0.153 客户端）；`version` 头随出站 UA 改写（此前透传入站值、中转无此头、Search 遇 `Codex Desktop` UA 会删掉） | 并入 .108 |
| .108 | 含 .107 全部内容；thread 改为按到达顺序 mint、当天名册满后复用最久没有新 turn 的 thread（设计文档 §7.1、§8、§18.16）；新事件 `codex_rid_thread_reused` | 发版中；Redis 新增 `…:{day}:threads` ZSET，无需迁移，切换当天账号 thread 数可能一次性略超上限 |

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
```

期望：Q2 里 `wn_without_ctx`、`window_id_mismatch`、`header_vs_blob_window_mismatch` 为 0；Q3 无 `leak:*` 行、`session!=thread` 为 0；Q4 无 `session_id` / `conversation_id` / `x-codex-parent-thread-id` / `x-openai-subagent` / `x-trace-id`；Q6 两列都等于 `threads`；Q8 每个 provider 的 `out_threads` 不超过 `expected_threads_per_day`×账号数量级。.105 起 Q9 中 Codex Pro 应几乎没有「no blob either side」行（无元数据请求已合成）。

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
| `codex_rid_thread_reused`（debug 级，.108） | 该账号当天 thread 额度已用满，新对话复用了最久没有新 turn 的 thread | 正常行为，忙账号每天都会出现。若某账号几乎每个新对话都触发（复用远多于 mint），说明上限相对该账号的流量偏低，酌情上调 `expected_threads_per_day`；不出现则说明流量没到上限 |
| `codex_rid_unknown_metadata_key` | 客户端带了三表面白名单之外的键，已被删除；每进程每 (surface, key) 只 warn 一次 | 走 §4.1 判定 |

### 4.1 未知键判定流程（白名单维护）

白名单常量都在 `apps/aether-gateway/src/codex_runtime_identity.rs` 顶部：`BLOB_IDENTITY_KEYS` / `BLOB_NORMALIZED_KEYS` / `BLOB_LEAK_KEYS` / `BLOB_PASS_KEYS`（turn-metadata blob），`FLAT_IDENTITY_KEYS` / `FLAT_LEAK_KEYS` / `FLAT_PASS_KEYS`（扁平 `client_metadata`），`HEADER_PASS_KEYS` / `HEADER_STRIP_KEYS`（`x-codex-*` 等前缀头）。

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

## 5. 版本核对：codex-rs 基准

设计文档 §1–§18.12 按 codex-rs `357696c5` 复核，§18.13 起按 `07f18d5f`（2026-09-05）。再次核对时先 `git -C /opt/stacks/openai-codex pull`（shallow clone，历史只到 2026-06-27），关键文件：

- `codex-rs/core/src/responses_metadata.rs`：blob / 扁平 metadata 的键集合、`RESERVED_METADATA_KEYS`、`has_turn_identity`。
- `codex-rs/core/src/turn_metadata.rs`、`sandbox_tags.rs`：turn-metadata 头的形状。
- `codex-rs/core/src/client.rs`：`prompt_cache_key` 规则（override → `internal_<source>:<parent>` → session_id）、`compact_conversation_history`。
- `codex-rs/codex-api/src/requests/headers.rs`：`build_session_headers`（只有 dash 形式 `session-id` / `thread-id`）。
- `codex-rs/codex-api/src/endpoint/responses.rs`、`endpoint/compact.rs`：`/responses` 加 `x-client-request-id` = thread，compact 不加。

## 6. 回滚与关闭

按影响从小到大：

1. **关开关**（秒级，无需部署）：号池高级设置里把「会话身份合成」关掉。出站立刻回到功能前的形状（入站身份透传 + Aether 填充器的 `session_id` / `conversation_id` 短头与随机 `x-client-request-id`）。上游会看到该账号的 thread 从合成身份切回真实身份，这是可接受的一次性跳变。
2. **镜像回滚**：按 `docs/operations/release-and-container-update-spec.md`，恢复对应 `.env.bak.<ts>_pre_vX` 的 `APP_IMAGE`，只重建 `app`，须操作员明确授权。**不要回到 .104**（带缓存回退）；.105 是含缓存修复的最低版本；再往前请回 .103 并关开关。
3. **Redis 键**：`ap:{provider_id}:codex_rid:*` 都有 TTL，回滚后自然过期。不要手动清：清掉等于让所有活跃 thread 换身份，上游看到一批新 thread。

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
