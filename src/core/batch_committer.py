"""
批量提交器 - 减少数据库 commit 次数，提升并发能力

核心思想：
- 非关键数据（监控、统计）不立即 commit
- 在后台定期批量 commit
- 关键数据（计费）仍然立即 commit
"""

import asyncio
from typing import Any

from sqlalchemy.orm import Session

from src.core.logger import logger


class BatchCommitter:
    """批量提交管理器"""

    def __init__(self, interval_seconds: float = 1.0):
        """
        Args:
            interval_seconds: 批量提交间隔（秒）
        """
        self.interval_seconds = interval_seconds
        self._pending_sessions: set[Session] = set()
        self._lock = asyncio.Lock()
        self._task = None

    async def start(self) -> Any:
        """启动后台批量提交任务"""
        if self._task is None:
            self._task = asyncio.create_task(self._batch_commit_loop())
            logger.info("批量提交器已启动，间隔: {}s", self.interval_seconds)

    async def stop(self) -> Any:
        """停止后台任务"""
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
            self._task = None
            logger.info("批量提交器已停止")

    def mark_dirty(self, session: Session) -> Any:
        """标记 Session 有待提交的更改"""
        # 请求级事务由中间件统一 commit/rollback；避免后台任务在请求中途误提交。
        if session is None:
            return
        if session.info.get("managed_by_middleware"):
            return
        self._pending_sessions.add(session)

    async def _batch_commit_loop(self) -> None:
        """后台批量提交循环"""
        while True:
            try:
                await asyncio.sleep(self.interval_seconds)
                await self._commit_all()
            except asyncio.CancelledError:
                # 关闭前提交所有待处理的
                await self._commit_all()
                raise
            except Exception as e:
                logger.error("批量提交出错: {}", e)

    async def _commit_all(self) -> None:
        """提交所有待处理的 Session"""
        async with self._lock:
            if not self._pending_sessions:
                return

            sessions_to_commit = list(self._pending_sessions)
            self._pending_sessions.clear()

        committed = 0
        failed = 0

        def _sync_commit_all() -> tuple[int, int]:
            ok = 0
            err = 0
            for session in sessions_to_commit:
                try:
                    session.commit()
                    ok += 1
                except Exception as e:
                    logger.error("提交 Session 失败: {}", e)
                    try:
                        session.rollback()
                    except:
                        pass
                    err += 1
            return ok, err

        try:
            committed, failed = await asyncio.to_thread(_sync_commit_all)
        except Exception as e:
            logger.error("批量提交线程异常: {}", e)

        if committed > 0:
            logger.debug("批量提交完成: {} 个 Session", committed)
        if failed > 0:
            logger.warning("批量提交失败: {} 个 Session", failed)


# 全局单例
_batch_committer: BatchCommitter = None


def get_batch_committer() -> BatchCommitter:
    """获取全局批量提交器"""
    global _batch_committer
    if _batch_committer is None:
        _batch_committer = BatchCommitter(interval_seconds=1.0)
    return _batch_committer


async def init_batch_committer() -> None:
    """初始化并启动批量提交器"""
    committer = get_batch_committer()
    await committer.start()


async def shutdown_batch_committer() -> None:
    """关闭批量提交器"""
    committer = get_batch_committer()
    await committer.stop()
