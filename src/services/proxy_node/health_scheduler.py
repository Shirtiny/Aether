"""
ProxyNode 心跳检测调度器

定期检查 proxy_nodes 的连接健康状态，更新节点状态：
- 心跳正常且 tunnel_connected=True -> ONLINE（自愈）
- 心跳超时（跨 worker 共享信号） -> OFFLINE
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

from src.core.logger import logger
from src.database import create_session
from src.models.database import ProxyNode, ProxyNodeStatus
from src.services.system.scheduler import get_scheduler

# 事件保留天数
_EVENT_RETENTION_DAYS = 30
# 每隔多少次心跳检测执行一次事件清理（15s * 240 = 1h）
_EVENT_CLEANUP_INTERVAL = 240
# 心跳超时判定：max(90s, heartbeat_interval * 3)
HEARTBEAT_STALE_MIN_SECONDS = 90
HEARTBEAT_STALE_MULTIPLIER = 3


def heartbeat_is_stale(node: object, now: datetime) -> bool:
    """根据 last_heartbeat_at 判定节点心跳是否超时。

    接受任意具有 last_heartbeat_at / heartbeat_interval 属性的对象，
    兼容 ProxyNode ORM 实例和在 asyncio.to_thread 中使用的场景。
    """
    last_heartbeat = getattr(node, "last_heartbeat_at", None)
    if not last_heartbeat:
        return True

    # 兼容 DB 中可能出现的 naive datetime
    if last_heartbeat.tzinfo is None:
        last_heartbeat = last_heartbeat.replace(tzinfo=timezone.utc)

    interval = max(int(getattr(node, "heartbeat_interval", None) or 30), 5)
    stale_seconds = max(HEARTBEAT_STALE_MIN_SECONDS, interval * HEARTBEAT_STALE_MULTIPLIER)
    return (now - last_heartbeat).total_seconds() > stale_seconds


class ProxyNodeHealthScheduler:
    """代理节点心跳检测调度器"""

    def __init__(self) -> None:
        self.running = False
        self._check_count = 0

    async def start(self) -> Any:
        if self.running:
            logger.warning("ProxyNodeHealthScheduler already running")
            return

        self.running = True
        logger.info("ProxyNodeHealthScheduler started")

        scheduler = get_scheduler()
        scheduler.add_interval_job(
            self._scheduled_check,
            seconds=15,
            job_id="proxy_node_health_check",
            name="代理节点心跳检测",
        )

        # 启动时立即执行一次
        await self._check_heartbeats()

    async def stop(self) -> Any:
        if not self.running:
            return
        self.running = False
        logger.info("ProxyNodeHealthScheduler stopped")

    async def _scheduled_check(self) -> None:
        await self._check_heartbeats()
        self._check_count = (self._check_count + 1) % _EVENT_CLEANUP_INTERVAL
        if self._check_count == 0:
            await self._cleanup_old_events()

    async def _check_heartbeats(self) -> None:
        import asyncio

        def _sync_check() -> None:
            db = create_session()
            try:
                now = datetime.now(timezone.utc)
                # 检查所有非手动节点（手动节点无心跳，始终保持 ONLINE）
                # 包括 OFFLINE 节点：心跳恢复后可自愈
                nodes = (
                    db.query(ProxyNode)
                    .filter(
                        ProxyNode.is_manual == False,  # noqa: E712
                    )
                    .all()
                )
                if not nodes:
                    return

                changed = 0
                for node in nodes:
                    if heartbeat_is_stale(node, now):
                        if node.tunnel_connected:
                            node.tunnel_connected = False
                            node.tunnel_connected_at = now
                            changed += 1
                        if node.status != ProxyNodeStatus.OFFLINE:
                            node.status = ProxyNodeStatus.OFFLINE
                            node.updated_at = now
                            changed += 1
                        continue

                    # 心跳正常且连接状态为已连时，确保 ONLINE（自愈状态不一致）
                    if node.tunnel_connected and node.status != ProxyNodeStatus.ONLINE:
                        node.status = ProxyNodeStatus.ONLINE
                        node.updated_at = now
                        changed += 1

                if changed:
                    db.commit()
                    logger.info("ProxyNode 心跳状态已更新: {} 个节点", changed)
            except Exception as e:
                try:
                    db.rollback()
                except Exception:
                    pass
                logger.exception("ProxyNode 心跳检测失败: {}", e)
            finally:
                db.close()

        try:
            await asyncio.to_thread(_sync_check)
        except Exception as e:
            logger.warning("ProxyNode 心跳检测线程执行失败: {}", e)

    async def _cleanup_old_events(self) -> None:
        """清理超过保留期的连接事件记录（在线程池中执行，避免阻塞事件循环）"""
        import asyncio

        def _sync_cleanup() -> None:
            from datetime import timedelta

            from src.models.database import ProxyNodeEvent

            db = create_session()
            try:
                cutoff = datetime.now(timezone.utc) - timedelta(days=_EVENT_RETENTION_DAYS)
                deleted = (
                    db.query(ProxyNodeEvent)
                    .filter(ProxyNodeEvent.created_at < cutoff)
                    .delete(synchronize_session=False)
                )
                if deleted:
                    db.commit()
                    logger.info(
                        "清理 {} 条过期代理节点事件 (>{} 天)", deleted, _EVENT_RETENTION_DAYS
                    )
            except Exception as e:
                try:
                    db.rollback()
                except Exception:
                    pass
                logger.warning("清理代理节点事件失败: {}", e)
            finally:
                db.close()

        try:
            await asyncio.to_thread(_sync_cleanup)
        except Exception as e:
            logger.warning("清理代理节点事件线程执行失败: {}", e)


_proxy_node_health_scheduler: ProxyNodeHealthScheduler | None = None


def get_proxy_node_health_scheduler() -> ProxyNodeHealthScheduler:
    global _proxy_node_health_scheduler
    if _proxy_node_health_scheduler is None:
        _proxy_node_health_scheduler = ProxyNodeHealthScheduler()
    return _proxy_node_health_scheduler
