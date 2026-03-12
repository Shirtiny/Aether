"""Pool Key 批量删除异步任务。

接口立即返回 task_id，后台执行删除，前端轮询进度。
任务状态存储在 Redis 中，支持多 worker 进程共享。
"""

from __future__ import annotations

import asyncio
import json
import time
import uuid
from collections.abc import Callable
from concurrent.futures import Future

import redis.asyncio as aioredis
from sqlalchemy import delete as sa_delete
from sqlalchemy import text

from src.clients.redis_client import get_redis_client
from src.core.logger import logger
from src.services.provider_keys.key_side_effects import cleanup_key_references

# 任务状态
STATUS_PENDING = "pending"
STATUS_RUNNING = "running"
STATUS_COMPLETED = "completed"
STATUS_FAILED = "failed"

# 任务完成后保留时长（秒），也用作 Redis key 的 TTL
_TASK_RETAIN_SECONDS = 600

# 批量删除时每个独立事务处理的 key 数量
_CLEANUP_BATCH_SIZE = 50

# 单个批次的数据库 statement 超时（秒）
_BATCH_STATEMENT_TIMEOUT_S = 30

# 整个任务的最大执行时间（秒）
_TASK_TIMEOUT_S = 600

# Redis key 前缀
_REDIS_KEY_PREFIX = "batch_delete_task"

# 持有后台 asyncio.Task 引用，防止 GC 回收（进程内部，不需要跨进程共享）
_running_tasks: set[asyncio.Task[None]] = set()


def _task_key(task_id: str) -> str:
    return f"{_REDIS_KEY_PREFIX}:{task_id}"


class BatchDeleteTaskInfo:
    """任务状态数据对象（从 Redis 反序列化）。"""

    __slots__ = ("task_id", "provider_id", "status", "total", "deleted", "message")

    def __init__(
        self,
        task_id: str,
        provider_id: str,
        status: str = STATUS_PENDING,
        total: int = 0,
        deleted: int = 0,
        message: str = "",
    ):
        self.task_id = task_id
        self.provider_id = provider_id
        self.status = status
        self.total = total
        self.deleted = deleted
        self.message = message

    def to_dict(self) -> dict:
        return {
            "task_id": self.task_id,
            "provider_id": self.provider_id,
            "status": self.status,
            "total": self.total,
            "deleted": self.deleted,
            "message": self.message,
        }

    @classmethod
    def from_dict(cls, data: dict) -> BatchDeleteTaskInfo:
        return cls(
            task_id=data["task_id"],
            provider_id=data["provider_id"],
            status=data.get("status", STATUS_PENDING),
            total=int(data.get("total", 0)),
            deleted=int(data.get("deleted", 0)),
            message=data.get("message", ""),
        )


async def _save_task(
    task: BatchDeleteTaskInfo,
    ttl: int = _TASK_RETAIN_SECONDS,
    r: aioredis.Redis | None = None,
) -> None:
    """将任务状态写入 Redis。"""
    if r is None:
        r = await get_redis_client(require_redis=False)
    if not r:
        return
    try:
        await r.setex(_task_key(task.task_id), ttl, json.dumps(task.to_dict()))
    except Exception as e:
        logger.warning("Failed to save batch delete task to Redis: {}", e)


async def _load_task(task_id: str, r: aioredis.Redis | None = None) -> BatchDeleteTaskInfo | None:
    """从 Redis 加载任务状态。"""
    if r is None:
        r = await get_redis_client(require_redis=False)
    if not r:
        return None
    try:
        data = await r.get(_task_key(task_id))
        if data is None:
            return None
        return BatchDeleteTaskInfo.from_dict(json.loads(data))
    except Exception as e:
        logger.warning("Failed to load batch delete task from Redis: {}", e)
        return None


async def _update_task_field(
    task_id: str, r: aioredis.Redis | None = None, **fields: object
) -> None:
    """局部更新任务状态字段（read-modify-write）。"""
    if r is None:
        r = await get_redis_client(require_redis=False)
    if not r:
        return
    task = await _load_task(task_id, r=r)
    if task is None:
        return
    for k, v in fields.items():
        setattr(task, k, v)
    # 已完成/失败的任务只保留 _TASK_RETAIN_SECONDS
    if task.status in (STATUS_COMPLETED, STATUS_FAILED):
        await _save_task(task, ttl=_TASK_RETAIN_SECONDS, r=r)
    else:
        # 运行中的任务使用更长的 TTL，防止超长任务过期
        await _save_task(task, ttl=_TASK_RETAIN_SECONDS * 2, r=r)


async def submit_batch_delete(provider_id: str, key_ids: list[str]) -> str:
    """提交批量删除任务，返回 task_id。

    Raises:
        RuntimeError: Redis 不可用时无法追踪任务状态。
    """
    r = await get_redis_client(require_redis=False)
    if not r:
        raise RuntimeError("Redis is required for batch delete tasks but is not available")

    task_id = uuid.uuid4().hex[:16]
    task = BatchDeleteTaskInfo(
        task_id=task_id,
        provider_id=provider_id,
        total=len(key_ids),
    )
    await _save_task(task, ttl=_TASK_RETAIN_SECONDS * 2, r=r)

    bg = asyncio.create_task(
        _run_batch_delete(task_id, provider_id, key_ids),
        name=f"batch-delete-{task_id}",
    )
    _running_tasks.add(bg)
    bg.add_done_callback(_running_tasks.discard)
    return task_id


async def get_batch_delete_task(task_id: str) -> BatchDeleteTaskInfo | None:
    return await _load_task(task_id)


def _sync_delete(
    provider_id: str,
    key_ids: list[str],
    progress_callback: Callable[[int], None] | None = None,
) -> int:
    """在线程中执行的同步删除逻辑，避免阻塞事件循环。

    按小批次（_CLEANUP_BATCH_SIZE）先显式处理关联表，再删除 key。
    每个批次独立事务，单批失败跳过并继续。
    """
    from src.database import create_session
    from src.models.database import ProviderAPIKey

    db = create_session()
    try:
        affected = 0
        total_batches = (len(key_ids) + _CLEANUP_BATCH_SIZE - 1) // _CLEANUP_BATCH_SIZE
        batch_idx = 0
        task_start = time.monotonic()
        for i in range(0, len(key_ids), _CLEANUP_BATCH_SIZE):
            # 整体超时保护
            if time.monotonic() - task_start > _TASK_TIMEOUT_S:
                logger.warning(
                    "[BATCH_DELETE] task timeout after {}s, deleted {}/{}",
                    _TASK_TIMEOUT_S,
                    affected,
                    len(key_ids),
                )
                break

            batch = key_ids[i : i + _CLEANUP_BATCH_SIZE]
            batch_idx += 1
            try:
                # 设置 statement_timeout，防止单条 SQL 无限等锁
                timeout_ms = _BATCH_STATEMENT_TIMEOUT_S * 1000
                db.execute(text(f"SET LOCAL statement_timeout = '{timeout_ms}'"))
                cleanup_key_references(db, batch)
                result = db.execute(
                    sa_delete(ProviderAPIKey).where(
                        ProviderAPIKey.provider_id == provider_id,
                        ProviderAPIKey.id.in_(batch),
                    )
                )
                rowcount = getattr(result, "rowcount", 0) or 0
                affected += int(rowcount)
                db.commit()
            except Exception as exc:
                logger.warning(
                    "[BATCH_DELETE] batch {}/{} failed (keys {}-{}): {}",
                    batch_idx,
                    total_batches,
                    i,
                    i + len(batch),
                    exc,
                )
                try:
                    db.rollback()
                except Exception:
                    pass
            # 每个批次都上报一次进度
            if progress_callback is not None:
                progress_callback(affected)
        logger.info(
            "[BATCH_DELETE] sync delete finished: deleted={}/{} batches={} elapsed={:.1f}s",
            affected,
            len(key_ids),
            total_batches,
            time.monotonic() - task_start,
        )
        return affected
    finally:
        try:
            db.close()
        except Exception:
            pass


async def _run_batch_delete(
    task_id: str,
    provider_id: str,
    key_ids: list[str],
) -> None:
    r = await get_redis_client(require_redis=False)
    await _update_task_field(task_id, r=r, status=STATUS_RUNNING)
    logger.info(
        "[BATCH_DELETE_TASK] started task={} provider={} total={}",
        task_id,
        provider_id[:8],
        len(key_ids),
    )

    # 从工作线程安全地触发 Redis 进度更新
    loop = asyncio.get_running_loop()
    progress_futures: list[Future[object]] = []

    def on_progress(current: int) -> None:
        try:
            future = asyncio.run_coroutine_threadsafe(
                _update_task_field(task_id, r=r, deleted=current), loop
            )
            progress_futures.append(future)
        except RuntimeError:
            pass

    try:
        affected = await asyncio.wait_for(
            asyncio.to_thread(_sync_delete, provider_id, key_ids, on_progress),
            timeout=_TASK_TIMEOUT_S + 30,  # 留一点余量给 _sync_delete 内部超时
        )

        if progress_futures:
            results = await asyncio.gather(
                *(asyncio.wrap_future(f) for f in progress_futures),
                return_exceptions=True,
            )
            for exc in results:
                if isinstance(exc, Exception):
                    logger.debug(
                        "[BATCH_DELETE_TASK] progress update failed task={}: {}",
                        task_id,
                        exc,
                    )

        # 副作用（缓存失效等）是异步操作，在事件循环中执行
        if affected > 0:
            try:
                from src.database import get_db_context
                from src.services.provider_keys.key_side_effects import (
                    run_delete_key_side_effects,
                )

                with get_db_context() as db:
                    await run_delete_key_side_effects(
                        db=db,
                        provider_id=provider_id,
                        deleted_key_allowed_models=None,
                    )
            except Exception as exc:
                logger.error("batch delete side effects failed: {}", exc)

        await _update_task_field(
            task_id,
            r=r,
            status=STATUS_COMPLETED,
            deleted=affected,
            message=f"{affected} keys deleted",
        )
        logger.info(
            "[BATCH_DELETE_TASK] completed task={} provider={} total={} deleted={}",
            task_id,
            provider_id[:8],
            len(key_ids),
            affected,
        )

    except asyncio.TimeoutError:
        msg = f"task timeout after {_TASK_TIMEOUT_S + 30}s"
        await _update_task_field(task_id, r=r, status=STATUS_FAILED, message=msg)
        logger.error(
            "[BATCH_DELETE_TASK] {} task={} provider={}",
            msg,
            task_id,
            provider_id[:8],
        )
    except asyncio.CancelledError:
        await _update_task_field(
            task_id,
            r=r,
            status=STATUS_FAILED,
            message="task cancelled (shutdown)",
        )
        logger.warning(
            "[BATCH_DELETE_TASK] cancelled task={} provider={}",
            task_id,
            provider_id[:8],
        )
    except Exception as exc:
        await _update_task_field(
            task_id,
            r=r,
            status=STATUS_FAILED,
            message=str(exc),
        )
        logger.error(
            "[BATCH_DELETE_TASK] failed task={} provider={}: {}",
            task_id,
            provider_id[:8],
            exc,
        )
