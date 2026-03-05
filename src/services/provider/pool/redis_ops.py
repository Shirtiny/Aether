"""Redis operations for the Account Pool (provider-agnostic).

All pool transient state is stored in Redis. This module centralises key
naming, Lua scripts, and graceful fallbacks so that the rest of the pool
layer is free of Redis specifics.

Key schema
----------
ap:{pid}:sticky:{session_uuid}             STRING  -> key_id   (TTL: config)
ap:{pid}:lru                               ZSET    member=key_id, score=unix_ts
ap:{pid}:cooldown:{key_id}                 STRING  -> reason   (TTL: error-specific)
ap:{pid}:cost:{key_id}                     ZSET    member=req_id, score=unix_ts
ap:{pid}:latency:{key_id}                  ZSET    member=req_id:ttfb_ms, score=unix_ts
provider_oauth_token_cache:{key_id}        STRING  -> access_token (TTL: expires - 60)
"""

from __future__ import annotations

import asyncio
import time
import uuid
from typing import TYPE_CHECKING, Any

from src.clients.redis_client import get_redis_client
from src.core.logger import logger

if TYPE_CHECKING:
    import redis.asyncio as aioredis

PREFIX = "ap"


def _sticky_key(provider_id: str, session_uuid: str) -> str:
    return f"{PREFIX}:{provider_id}:sticky:{session_uuid}"


def _lru_key(provider_id: str) -> str:
    return f"{PREFIX}:{provider_id}:lru"


def _cooldown_key(provider_id: str, key_id: str) -> str:
    return f"{PREFIX}:{provider_id}:cooldown:{key_id}"


def _cost_key(provider_id: str, key_id: str) -> str:
    return f"{PREFIX}:{provider_id}:cost:{key_id}"


def _latency_key(provider_id: str, key_id: str) -> str:
    return f"{PREFIX}:{provider_id}:latency:{key_id}"


def _oauth_cache_key(key_id: str) -> str:
    return f"provider_oauth_token_cache:{key_id}"


# ---------------------------------------------------------------------------
# Lua scripts
# ---------------------------------------------------------------------------

# Sticky-select: GET binding, verify it's not in cooldown, refresh TTL.
# KEYS[1] = sticky key, KEYS[2] = cooldown key prefix (ap:{pid}:cooldown:)
# ARGV[1] = ttl
# Returns: key_id or nil
_STICKY_SELECT_LUA = """
local binding = redis.call("GET", KEYS[1])
if not binding then
    return nil
end
-- Check cooldown for the bound key
local cooldown_key = KEYS[2] .. binding
local in_cooldown = redis.call("EXISTS", cooldown_key)
if in_cooldown == 1 then
    redis.call("DEL", KEYS[1])
    return nil
end
redis.call("EXPIRE", KEYS[1], tonumber(ARGV[1]))
return binding
"""

# Cost window cleanup + sum tokens in a single round-trip.
# KEYS[1] = cost zset key, ARGV[1] = window_start timestamp
# Returns total token count within the window.
_COST_WINDOW_SUM_LUA = """
local key = KEYS[1]
local window_start = tonumber(ARGV[1])
redis.call("ZREMRANGEBYSCORE", key, "-inf", window_start)
local members = redis.call("ZRANGEBYSCORE", key, window_start, "+inf")
local total = 0
for _, m in ipairs(members) do
    local colon = string.find(m, ":", 1, true)
    if colon then
        local n = tonumber(string.sub(m, colon + 1))
        if n then total = total + n end
    end
end
return total
"""

# Latency window cleanup + average in a single round-trip.
# KEYS[1] = latency zset key, ARGV[1] = window_start timestamp
# Returns nil when there are no samples, or avg(ms) as number.
_LATENCY_WINDOW_AVG_LUA = """
local key = KEYS[1]
local window_start = tonumber(ARGV[1])
redis.call("ZREMRANGEBYSCORE", key, "-inf", window_start)
local members = redis.call("ZRANGEBYSCORE", key, window_start, "+inf")
local total = 0
local count = 0
for _, m in ipairs(members) do
    local colon = string.find(m, ":", 1, true)
    if colon then
        local n = tonumber(string.sub(m, colon + 1))
        if n then
            total = total + n
            count = count + 1
        end
    end
end
if count == 0 then
    return nil
end
return total / count
"""


async def _get_redis() -> "aioredis.Redis | None":
    return await get_redis_client(require_redis=False)


# ---------------------------------------------------------------------------
# Sticky session
# ---------------------------------------------------------------------------


async def get_sticky_binding(provider_id: str, session_uuid: str, ttl: int) -> str | None:
    """Get and refresh sticky session binding. Returns key_id or None."""
    redis = await _get_redis()
    if redis is None:
        return None
    try:
        result = await redis.eval(
            _STICKY_SELECT_LUA,
            2,
            _sticky_key(provider_id, session_uuid),
            f"{PREFIX}:{provider_id}:cooldown:",
            str(ttl),
        )
        if result:
            return result.decode() if isinstance(result, bytes) else str(result)
        return None
    except Exception:
        logger.debug("Pool: sticky GET failed for session {}", session_uuid[:8])
        return None


async def set_sticky_binding(provider_id: str, session_uuid: str, key_id: str, ttl: int) -> None:
    """Create or update sticky session binding."""
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.setex(_sticky_key(provider_id, session_uuid), ttl, key_id)
    except Exception:
        logger.debug("Pool: sticky SET failed for session {}", session_uuid[:8])


async def delete_sticky_binding(provider_id: str, session_uuid: str) -> None:
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.delete(_sticky_key(provider_id, session_uuid))
    except Exception:
        pass


# ---------------------------------------------------------------------------
# LRU
# ---------------------------------------------------------------------------


async def get_lru_scores(provider_id: str, key_ids: list[str]) -> dict[str, float]:
    """Batch-fetch LRU timestamps. Missing keys get score 0 (highest priority)."""
    redis = await _get_redis()
    if redis is None:
        return {}
    try:
        lru_k = _lru_key(provider_id)
        scores = await redis.zmscore(lru_k, key_ids)
        result: dict[str, float] = {}
        for kid, score in zip(key_ids, scores):
            result[kid] = float(score) if score is not None else 0.0
        return result
    except Exception:
        logger.debug("Pool: LRU ZMSCORE failed for provider {}", provider_id[:8])
        return {}


async def touch_lru(provider_id: str, key_id: str) -> None:
    """Update last-used timestamp."""
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.zadd(_lru_key(provider_id), {key_id: time.time()})
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Cooldown
# ---------------------------------------------------------------------------


async def set_cooldown(provider_id: str, key_id: str, reason: str, ttl: int) -> None:
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.setex(_cooldown_key(provider_id, key_id), ttl, reason)
        logger.info(
            "Pool: key {} cooldown set: {} ({}s)",
            key_id[:8],
            reason,
            ttl,
        )
    except Exception:
        logger.debug("Pool: cooldown SET failed for key {}", key_id[:8])


async def get_cooldown(provider_id: str, key_id: str) -> str | None:
    redis = await _get_redis()
    if redis is None:
        return None
    try:
        val = await redis.get(_cooldown_key(provider_id, key_id))
        if val:
            return val.decode() if isinstance(val, bytes) else str(val)
        return None
    except Exception:
        return None


async def clear_cooldown(provider_id: str, key_id: str) -> None:
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.delete(_cooldown_key(provider_id, key_id))
    except Exception:
        pass


async def batch_get_cooldowns(
    provider_id: str,
    key_ids: list[str],
    *,
    include_ttl: bool = False,
) -> dict[str, str | None] | dict[str, tuple[str | None, int | None]]:
    """Batch check cooldown status for multiple keys.

    When *include_ttl* is ``True``, each value is a ``(reason, ttl_seconds)``
    tuple instead of a plain reason string.  The TTL commands are batched in
    the same pipeline so there is no extra round-trip.
    """
    redis = await _get_redis()
    if redis is None:
        if include_ttl:
            return {k: (None, None) for k in key_ids}
        return {k: None for k in key_ids}
    try:
        pipe = redis.pipeline()
        for kid in key_ids:
            ck = _cooldown_key(provider_id, kid)
            pipe.get(ck)
            if include_ttl:
                pipe.ttl(ck)
        results = await pipe.execute()

        if include_ttl:
            out_ttl: dict[str, tuple[str | None, int | None]] = {}
            # results interleave GET/TTL: [val0, ttl0, val1, ttl1, ...]
            for i, kid in enumerate(key_ids):
                val = results[i * 2]
                ttl_val = results[i * 2 + 1]
                reason: str | None = None
                if val:
                    reason = val.decode() if isinstance(val, bytes) else str(val)
                ttl_sec: int | None = None
                if isinstance(ttl_val, int) and ttl_val > 0:
                    ttl_sec = ttl_val
                out_ttl[kid] = (reason, ttl_sec)
            return out_ttl

        out: dict[str, str | None] = {}
        for kid, val in zip(key_ids, results):
            if val:
                out[kid] = val.decode() if isinstance(val, bytes) else str(val)
            else:
                out[kid] = None
        return out
    except Exception:
        if include_ttl:
            return {k: (None, None) for k in key_ids}
        return {k: None for k in key_ids}


# ---------------------------------------------------------------------------
# Cost tracking
# ---------------------------------------------------------------------------


async def add_cost_entry(provider_id: str, key_id: str, tokens: int, window_seconds: int) -> None:
    """Record a cost entry (tokens used) with automatic window expiry."""
    redis = await _get_redis()
    if redis is None:
        return
    try:
        now = time.time()
        cost_k = _cost_key(provider_id, key_id)
        member = f"{uuid.uuid4().hex}:{tokens}"
        pipe = redis.pipeline()
        pipe.zadd(cost_k, {member: now})
        # Set a TTL slightly larger than the window to auto-clean abandoned keys.
        pipe.expire(cost_k, window_seconds + 600)
        await pipe.execute()
    except Exception:
        logger.debug("Pool: cost ADD failed for key {}", key_id[:8])


async def get_cost_window_total(provider_id: str, key_id: str, window_seconds: int) -> int:
    """Sum tokens used within the rolling window (single key)."""
    redis = await _get_redis()
    if redis is None:
        return 0
    try:
        now = time.time()
        window_start = now - window_seconds
        cost_k = _cost_key(provider_id, key_id)
        result = await redis.eval(_COST_WINDOW_SUM_LUA, 1, cost_k, str(window_start))
        return int(result) if result else 0
    except Exception:
        logger.debug("Pool: cost SUM failed for key {}", key_id[:8])
        return 0


async def batch_get_cost_totals(
    provider_id: str, key_ids: list[str], window_seconds: int
) -> dict[str, int]:
    """Batch-fetch cost totals for multiple keys using pipeline + Lua."""
    redis = await _get_redis()
    if redis is None:
        return {k: 0 for k in key_ids}
    try:
        now = time.time()
        window_start = now - window_seconds
        pipe = redis.pipeline()
        for kid in key_ids:
            cost_k = _cost_key(provider_id, kid)
            pipe.eval(_COST_WINDOW_SUM_LUA, 1, cost_k, str(window_start))
        results = await pipe.execute()
        out: dict[str, int] = {}
        for kid, val in zip(key_ids, results):
            out[kid] = int(val) if val else 0
        return out
    except Exception:
        logger.debug("Pool: batch cost SUM failed for provider {}", provider_id[:8])
        return {k: 0 for k in key_ids}


async def record_latency(
    provider_id: str,
    key_id: str,
    ttfb_ms: int,
    window_seconds: int,
    sample_limit: int,
) -> None:
    """Record one TTFB sample with rolling-window cleanup."""
    redis = await _get_redis()
    if redis is None:
        return
    try:
        now = time.time()
        latency_k = _latency_key(provider_id, key_id)
        sample = max(int(ttfb_ms), 0)
        member = f"{uuid.uuid4().hex}:{sample}"
        pipe = redis.pipeline()
        pipe.zadd(latency_k, {member: now})
        window_start = now - max(int(window_seconds), 1)
        pipe.zremrangebyscore(latency_k, "-inf", window_start)
        capped_limit = max(int(sample_limit), 1)
        pipe.zremrangebyrank(latency_k, 0, -(capped_limit + 1))
        pipe.expire(latency_k, max(int(window_seconds), 1) + 600)
        await pipe.execute()
    except Exception:
        logger.debug("Pool: latency ADD failed for key {}", key_id[:8])


async def batch_get_latency_avgs(
    provider_id: str,
    key_ids: list[str],
    window_seconds: int,
) -> dict[str, float]:
    """Batch-fetch latency averages (ms) for keys in a rolling window."""
    redis = await _get_redis()
    if redis is None:
        return {}
    try:
        now = time.time()
        window_start = now - max(int(window_seconds), 1)
        pipe = redis.pipeline()
        for kid in key_ids:
            pipe.eval(_LATENCY_WINDOW_AVG_LUA, 1, _latency_key(provider_id, kid), str(window_start))
        results = await pipe.execute()
        out: dict[str, float] = {}
        for kid, val in zip(key_ids, results):
            if val is None:
                continue
            try:
                out[kid] = float(val)
            except Exception:
                continue
        return out
    except Exception:
        logger.debug("Pool: batch latency AVG failed for provider {}", provider_id[:8])
        return {}


async def clear_cost(provider_id: str, key_id: str) -> None:
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.delete(_cost_key(provider_id, key_id))
    except Exception:
        pass


# ---------------------------------------------------------------------------
# OAuth token cache
# ---------------------------------------------------------------------------


async def cache_oauth_token(key_id: str, token: str, ttl: int) -> None:
    redis = await _get_redis()
    if redis is None:
        return
    try:
        if ttl > 0:
            await redis.setex(_oauth_cache_key(key_id), ttl, token)
    except Exception:
        pass


async def get_cached_oauth_token(key_id: str) -> str | None:
    redis = await _get_redis()
    if redis is None:
        return None
    try:
        val = await redis.get(_oauth_cache_key(key_id))
        if val:
            return val.decode() if isinstance(val, bytes) else str(val)
        return None
    except Exception:
        return None


async def invalidate_oauth_token_cache(key_id: str) -> None:
    redis = await _get_redis()
    if redis is None:
        return
    try:
        await redis.delete(_oauth_cache_key(key_id))
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Pool status query (admin)
# ---------------------------------------------------------------------------


async def get_sticky_session_count(provider_id: str) -> int:
    """Approximate count of active sticky sessions (via SCAN, for admin display only)."""
    redis = await _get_redis()
    if redis is None:
        return 0
    try:
        pattern = f"{PREFIX}:{provider_id}:sticky:*"
        count = 0
        async for _ in redis.scan_iter(match=pattern, count=100):
            count += 1
        return count
    except Exception:
        return 0


async def get_key_sticky_count(provider_id: str, key_id: str) -> int:
    """Count sticky sessions bound to a specific key (admin only).

    Uses batched SCAN + pipeline MGET to reduce Redis round-trips.
    """
    redis = await _get_redis()
    if redis is None:
        return 0
    try:
        pattern = f"{PREFIX}:{provider_id}:sticky:*"
        count = 0
        batch: list[bytes | str] = []
        async for k in redis.scan_iter(match=pattern, count=200):
            batch.append(k)
            if len(batch) >= 200:
                vals = await redis.mget(batch)
                for val in vals:
                    if val:
                        bound_id = val.decode() if isinstance(val, bytes) else str(val)
                        if bound_id == key_id:
                            count += 1
                batch.clear()
        if batch:
            vals = await redis.mget(batch)
            for val in vals:
                if val:
                    bound_id = val.decode() if isinstance(val, bytes) else str(val)
                    if bound_id == key_id:
                        count += 1
        return count
    except Exception:
        return 0


async def batch_get_key_sticky_counts(
    provider_id: str,
    key_ids: list[str],
) -> dict[str, int]:
    """Count sticky sessions for multiple keys in a single scan (admin only)."""
    if not key_ids:
        return {}

    redis = await _get_redis()
    if redis is None:
        return {kid: 0 for kid in key_ids}

    target_ids = set(key_ids)
    counts: dict[str, int] = {kid: 0 for kid in key_ids}

    try:
        pattern = f"{PREFIX}:{provider_id}:sticky:*"
        batch: list[bytes | str] = []
        async for key in redis.scan_iter(match=pattern, count=200):
            batch.append(key)
            if len(batch) >= 200:
                vals = await redis.mget(batch)
                for val in vals:
                    if not val:
                        continue
                    bound_id = val.decode() if isinstance(val, bytes) else str(val)
                    if bound_id in target_ids:
                        counts[bound_id] = counts.get(bound_id, 0) + 1
                batch.clear()

        if batch:
            vals = await redis.mget(batch)
            for val in vals:
                if not val:
                    continue
                bound_id = val.decode() if isinstance(val, bytes) else str(val)
                if bound_id in target_ids:
                    counts[bound_id] = counts.get(bound_id, 0) + 1

        return counts
    except Exception:
        return {kid: 0 for kid in key_ids}


async def get_cooldown_ttl(provider_id: str, key_id: str) -> int | None:
    """Get remaining cooldown TTL in seconds. None = no cooldown."""
    redis = await _get_redis()
    if redis is None:
        return None
    try:
        ttl = await redis.ttl(_cooldown_key(provider_id, key_id))
        return ttl if ttl > 0 else None
    except Exception:
        return None


async def batch_get_cooldown_ttls(provider_id: str, key_ids: list[str]) -> dict[str, int | None]:
    """Batch-fetch cooldown TTLs for multiple keys using pipeline."""
    redis = await _get_redis()
    if redis is None:
        return {k: None for k in key_ids}
    try:
        pipe = redis.pipeline()
        for kid in key_ids:
            pipe.ttl(_cooldown_key(provider_id, kid))
        results = await pipe.execute()
        out: dict[str, int | None] = {}
        for kid, ttl in zip(key_ids, results):
            out[kid] = int(ttl) if isinstance(ttl, int) and ttl > 0 else None
        return out
    except Exception:
        return {k: None for k in key_ids}


async def batch_count_provider_cooldowns(provider_ids: list[str]) -> dict[str, int]:
    """Count cooldown entries per provider using per-provider SCAN.

    Each provider's cooldown keys are scanned independently with a
    targeted pattern ``ap:{pid}:cooldown:*``, avoiding full-keyspace traversal.
    Multiple providers are scanned concurrently via ``asyncio.gather``.
    """
    if not provider_ids:
        return {}

    redis = await _get_redis()
    if redis is None:
        return {pid: 0 for pid in provider_ids}

    async def _count_one(r: Any, pid: str) -> tuple[str, int]:
        pattern = f"{PREFIX}:{pid}:cooldown:*"
        count = 0
        async for _key in r.scan_iter(match=pattern, count=200):
            count += 1
        return pid, count

    try:
        results = await asyncio.gather(
            *[_count_one(redis, pid) for pid in provider_ids],
            return_exceptions=True,
        )
        counts: dict[str, int] = {}
        for r in results:
            if isinstance(r, Exception):
                continue
            counts[r[0]] = r[1]
        for pid in provider_ids:
            counts.setdefault(pid, 0)
        return counts
    except Exception:
        return {pid: 0 for pid in provider_ids}


# ---------------------------------------------------------------------------
# Stream timeout counter
# ---------------------------------------------------------------------------

_STREAM_TIMEOUT_KEY_FMT = f"{PREFIX}:{{}}:stream_timeout:{{}}"


def _stream_timeout_key(provider_id: str, key_id: str) -> str:
    return _STREAM_TIMEOUT_KEY_FMT.format(provider_id, key_id)


async def incr_stream_timeout_count(
    provider_id: str,
    key_id: str,
    window_seconds: int,
) -> int:
    """Increment stream timeout counter and return count within the window.

    Uses a ZSET with timestamps as scores. Old entries beyond the window
    are pruned on each call. Returns the count of timeouts in the window.
    """
    redis = await _get_redis()
    if redis is None:
        return 0
    try:
        now = time.time()
        window_start = now - window_seconds
        key = _stream_timeout_key(provider_id, key_id)
        member = f"{uuid.uuid4().hex}"
        pipe = redis.pipeline()
        pipe.zremrangebyscore(key, "-inf", window_start)
        pipe.zadd(key, {member: now})
        pipe.zcard(key)
        pipe.expire(key, window_seconds + 60)
        results = await pipe.execute()
        count = int(results[2]) if results[2] else 0
        return count
    except Exception:
        logger.debug("Pool: stream timeout INCR failed for key {}", key_id[:8])
        return 0
