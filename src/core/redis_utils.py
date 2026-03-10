"""Redis batch-delete utilities.

Provides SCAN-based pattern deletion with UNLINK preference to minimise
blocking on the Redis server.  Used by cache monitoring and affinity
manager modules.
"""

from __future__ import annotations

from typing import Any

from src.core.logger import logger

# Defaults matching existing usage across the codebase.
DEFAULT_SCAN_BATCH_SIZE = 200
DEFAULT_DELETE_BATCH_SIZE = 500


async def delete_redis_keys(redis: Any, keys: list[str]) -> int:
    """Batch-delete Redis keys, preferring UNLINK over DELETE."""
    if not keys:
        return 0

    try:
        unlink = getattr(redis, "unlink", None)
        if callable(unlink):
            return int(await unlink(*keys))
    except Exception as exc:
        logger.debug("Redis UNLINK failed, falling back to DELETE: {}", exc)

    return int(await redis.delete(*keys))


async def scan_delete_pattern(
    redis: Any,
    pattern: str,
    *,
    scan_batch_size: int = DEFAULT_SCAN_BATCH_SIZE,
    delete_batch_size: int = DEFAULT_DELETE_BATCH_SIZE,
) -> int:
    """SCAN + batch-delete keys matching *pattern* without blocking Redis."""
    deleted_count = 0
    cursor: int | str = 0
    while True:
        cursor, keys = await redis.scan(cursor=cursor, match=pattern, count=scan_batch_size)
        if keys:
            for i in range(0, len(keys), delete_batch_size):
                batch = keys[i : i + delete_batch_size]
                deleted_count += await delete_redis_keys(redis, batch)

        if int(cursor) == 0:
            break

    return deleted_count
