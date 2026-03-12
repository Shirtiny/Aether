"""
WebSocket 隧道端点

aether-proxy 通过此端点建立 tunnel 连接。
路径: /api/internal/proxy-tunnel
"""

from __future__ import annotations

import asyncio
import os
from datetime import datetime, timezone

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from src.core.logger import logger
from src.services.proxy_node.health_scheduler import heartbeat_is_stale
from src.services.proxy_node.tunnel_manager import (
    TunnelConnection,
    get_tunnel_manager,
)
from src.services.proxy_node.tunnel_protocol import Frame, MsgType

router = APIRouter()

# Per-node 锁: 防止并发的 connect/disconnect 写入 DB 时出现竞态（后断连覆盖先连接）
_node_status_locks: dict[str, asyncio.Lock] = {}

# Per-node idle timeout 连续计数: 用于抑制重复日志
_idle_timeout_counts: dict[str, int] = {}


def _get_node_lock(node_id: str) -> asyncio.Lock:
    lock = _node_status_locks.get(node_id)
    if lock is None:
        lock = asyncio.Lock()
        _node_status_locks[node_id] = lock
    return lock


# 单帧最大 64 MB -- AI API 请求体可能包含多张 base64 图片，需要足够余量
_MAX_FRAME_SIZE = 64 * 1024 * 1024

# 默认 WebSocket 空闲超时（秒）-- 0 表示禁用（依赖 PING/PONG 心跳检测连接存活）
_DEFAULT_IDLE_TIMEOUT = 0.0

# 默认服务端应用层 ping 间隔（秒）-- 与客户端 ping 间隔一致，确保高频心跳
_DEFAULT_SERVER_PING_INTERVAL = 15.0


def _env_float(name: str, default: float, *, min_value: float, max_value: float) -> float:
    """读取并校验浮点环境变量，非法时回退默认值。"""
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        value = float(raw)
    except ValueError:
        logger.warning("invalid {}={}, fallback to {}", name, raw, default)
        return default
    if value < min_value or value > max_value:
        logger.warning(
            "{}={} out of range [{}, {}], fallback to {}",
            name,
            value,
            min_value,
            max_value,
            default,
        )
        return default
    return value


# 允许通过环境变量在弱网环境下调大容忍窗口（无需改代码）
_SERVER_PING_INTERVAL = _env_float(
    "AETHER_PROXY_TUNNEL_SERVER_PING_INTERVAL",
    _DEFAULT_SERVER_PING_INTERVAL,
    min_value=5.0,
    max_value=120.0,
)
_IDLE_TIMEOUT = _env_float(
    "AETHER_PROXY_TUNNEL_SERVER_IDLE_TIMEOUT",
    _DEFAULT_IDLE_TIMEOUT,
    min_value=0.0,
    max_value=600.0,
)

# 避免 idle timeout 过小导致 ping 尚未生效就被服务端断开（0 表示禁用，跳过校验）
if _IDLE_TIMEOUT > 0 and _IDLE_TIMEOUT <= _SERVER_PING_INTERVAL * 2:
    adjusted_idle = max(_SERVER_PING_INTERVAL * 3, 30.0)
    logger.warning(
        "AETHER_PROXY_TUNNEL_SERVER_IDLE_TIMEOUT too low for ping interval, auto-adjust to {}",
        adjusted_idle,
    )
    _IDLE_TIMEOUT = adjusted_idle


async def _authenticate(ws: WebSocket) -> tuple[str, str] | None:
    """验证 WebSocket 连接的认证信息，返回 (node_id, node_name) 或 None

    认证方式：Bearer <management_token>，通过 Management Token 系统验证。
    authenticate_management_token 是 async 方法（内部有 Redis 速率限制），
    因此直接 await 调用。节点存在性检查复用同一 session。
    """
    auth = ws.headers.get("authorization", "")
    if not auth.startswith("Bearer "):
        return None

    token = auth[7:]
    if not token or not token.startswith("ae_"):
        return None

    client_ip = getattr(ws.client, "host", "unknown") if ws.client else "unknown"
    node_id_header = ws.headers.get("x-node-id", "").strip()
    node_name_header = ws.headers.get("x-node-name", "").strip()

    if not node_id_header:
        return None

    from src.database import create_session
    from src.models.database import ProxyNode
    from src.services.auth.service import AuthService

    db = create_session()
    try:
        result = await AuthService.authenticate_management_token(db, token, client_ip)
        if not result:
            return None

        # 节点存在性检查（复用同一 session，避免额外连接开销）
        exists = db.query(
            db.query(ProxyNode).filter(ProxyNode.id == node_id_header).exists()
        ).scalar()
        if not exists:
            logger.warning("tunnel auth: node_id={} not found in DB", node_id_header)
            return None
    finally:
        db.close()

    return node_id_header, node_name_header or node_id_header


@router.websocket("/api/internal/proxy-tunnel")
async def proxy_tunnel_ws(ws: WebSocket) -> None:
    """aether-proxy tunnel WebSocket 端点"""
    # 先 accept，避免认证（DB/Redis）慢时卡在握手阶段导致网关返回 502。
    await ws.accept()

    try:
        auth = await asyncio.wait_for(_authenticate(ws), timeout=10.0)
    except asyncio.TimeoutError:
        logger.warning("tunnel auth timeout")
        await ws.close(code=4002, reason="authentication timeout")
        return
    except Exception as e:
        logger.warning("tunnel auth error: {}", e)
        await ws.close(code=4002, reason="authentication error")
        return

    if not auth:
        await ws.close(code=4001, reason="unauthorized")
        return

    node_id: str = auth[0]
    node_name: str = auth[1]

    # Read proxy-advertised max concurrent streams (backward-compatible:
    # old proxies don't send this header, we fall back to the default).
    max_streams_raw = ws.headers.get("x-tunnel-max-streams", "").strip()
    max_streams: int | None = None
    if max_streams_raw:
        try:
            max_streams = int(max_streams_raw)
        except ValueError:
            pass

    manager = get_tunnel_manager()
    conn = TunnelConnection(node_id, node_name, ws, max_streams=max_streams)
    node_lock = _get_node_lock(node_id)

    manager.register(conn)

    # 在 per-node 锁保护下更新 DB，防止并发的 connect/disconnect 写入竞态
    async with node_lock:
        await _update_tunnel_status(
            node_id,
            connected=True,
            observed_at=datetime.now(timezone.utc),
        )

    # 启动服务端 ping 任务，防止中间代理因空闲超时关闭连接
    ping_task = asyncio.create_task(_ping_loop(conn))

    disconnect_reason: str | None = None
    try:
        oversized_count = 0
        while True:
            try:
                if _IDLE_TIMEOUT > 0:
                    data = await asyncio.wait_for(ws.receive_bytes(), timeout=_IDLE_TIMEOUT)
                else:
                    data = await ws.receive_bytes()
            except asyncio.TimeoutError:
                count = _idle_timeout_counts.get(node_id, 0) + 1
                _idle_timeout_counts[node_id] = count
                # 首次 warning，后续每 10 次打印一条 info，其余 debug
                if count == 1:
                    logger.warning("tunnel idle timeout for node_id={}", node_id)
                elif count % 10 == 0:
                    logger.info(
                        "tunnel idle timeout for node_id={} (repeated {} times)",
                        node_id,
                        count,
                    )
                else:
                    logger.debug("tunnel idle timeout for node_id={}", node_id)
                disconnect_reason = "idle timeout"
                await ws.close(code=4004, reason="idle timeout")
                break
            if len(data) > _MAX_FRAME_SIZE:
                oversized_count += 1
                logger.warning("tunnel frame too large from {}: {} bytes", node_id, len(data))
                if oversized_count >= 5:
                    logger.warning("too many oversized frames from {}, closing", node_id)
                    disconnect_reason = "too many oversized frames"
                    await ws.close(code=4003, reason="too many oversized frames")
                    break
                continue
            oversized_count = 0  # 正常帧重置计数
            _idle_timeout_counts.pop(node_id, None)  # 收到正常帧，重置 idle timeout 计数
            manager.reset_reconnect_count(node_id)  # 连接正常活跃，重置 reconnect 计数
            try:
                frame = Frame.decode(data)
            except ValueError as e:
                logger.warning("tunnel frame decode error from {}: {}", node_id, e)
                continue

            await manager.handle_incoming_frame(conn, frame)

    except WebSocketDisconnect:
        disconnect_reason = "WebSocket disconnected"
        logger.info("tunnel WebSocket disconnected: node_id={}", node_id)
    except Exception as e:
        disconnect_reason = f"error: {e}"
        logger.error("tunnel WebSocket error for node_id={}: {}", node_id, e)
    finally:
        ping_task.cancel()
        # 在 per-node 锁保护下执行 unregister + 连接池计数检查 + DB 更新，
        # 确保整个序列是原子的，避免"断连写 OFFLINE 覆盖新连接写 ONLINE"的竞态
        async with node_lock:
            manager.unregister(conn)
            if manager.connection_count(node_id) == 0:
                await _update_tunnel_status(
                    node_id,
                    connected=False,
                    detail=disconnect_reason,
                    observed_at=datetime.now(timezone.utc),
                )
                # 不清理锁: asyncio.Lock 极轻量，清理可能导致并发新连接拿到不同锁实例
            else:
                logger.info("tunnel connection closed but pool still active: node_id={}", node_id)


async def _ping_loop(conn: TunnelConnection) -> None:
    """定期发送应用层 PING 帧，保持连接活跃"""
    try:
        while True:
            await asyncio.sleep(_SERVER_PING_INTERVAL)
            if not conn.is_alive:
                break
            try:
                await conn.send_frame(Frame(0, MsgType.PING, 0, b""))
            except Exception as e:
                logger.debug("ping loop send failed for node_id={}: {}", conn.node_id, e)
                break
    except asyncio.CancelledError:
        pass


async def _update_tunnel_status(
    node_id: str,
    *,
    connected: bool,
    detail: str | None = None,
    observed_at: datetime | None = None,
) -> None:
    """更新 ProxyNode 的 tunnel 连接状态并记录事件（在线程池中执行）"""

    def _sync_update() -> None:
        from src.database import create_session
        from src.models.database import ProxyNode, ProxyNodeEvent, ProxyNodeStatus

        db = create_session()
        try:
            node = db.query(ProxyNode).filter(ProxyNode.id == node_id).first()
            if node:
                event_time = observed_at or datetime.now(timezone.utc)
                last_transition = node.tunnel_connected_at
                if last_transition and last_transition.tzinfo is None:
                    last_transition = last_transition.replace(tzinfo=timezone.utc)

                # 忽略乱序的旧事件，避免快速重连时旧状态覆盖新状态
                stale_event = bool(last_transition and event_time < last_transition)
                if stale_event:
                    detail_text = f"[stale_ignored] {detail}" if detail else "[stale_ignored]"
                    db.add(
                        ProxyNodeEvent(
                            node_id=node_id,
                            event_type="connected" if connected else "disconnected",
                            detail=detail_text,
                        )
                    )
                    db.commit()
                    return

                event_detail = detail
                if connected:
                    node.tunnel_connected = True
                    node.tunnel_connected_at = event_time
                    node.status = ProxyNodeStatus.ONLINE
                else:
                    # 断连不立即强制 OFFLINE。若心跳仍新鲜，可能仍有其他连接存活
                    # （连接池或跨 worker），避免误判写回 OFFLINE。
                    if heartbeat_is_stale(node, event_time):
                        node.tunnel_connected = False
                        node.tunnel_connected_at = event_time
                        node.status = ProxyNodeStatus.OFFLINE
                    else:
                        event_detail = (
                            f"[heartbeat_fresh] {detail}" if detail else "[heartbeat_fresh]"
                        )

                # 记录连接事件
                event = ProxyNodeEvent(
                    node_id=node_id,
                    event_type="connected" if connected else "disconnected",
                    detail=event_detail,
                )
                db.add(event)
                db.commit()
        finally:
            db.close()

    try:
        await asyncio.to_thread(_sync_update)
    except Exception as e:
        logger.warning("failed to update tunnel status for {}: {}", node_id, e)

    # 清除节点信息缓存，确保后续请求能立即感知连接状态变化
    from src.services.proxy_node.resolver import invalidate_proxy_node_cache

    invalidate_proxy_node_cache(node_id)
