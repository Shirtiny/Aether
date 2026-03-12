from __future__ import annotations

import time

import pytest

from src.services.proxy_node.hub_config import HubConfig
from src.services.proxy_node.hub_transport import HubConnectionManager
from src.services.proxy_node.tunnel_manager import TunnelStreamError


def _build_manager() -> HubConnectionManager:
    return HubConnectionManager(
        HubConfig(
            enabled=True,
            url="ws://127.0.0.1:8085",
            connect_timeout_seconds=1.0,
            ping_interval_seconds=1.0,
            send_timeout_seconds=1.0,
            max_streams=16,
            max_frame_size=1024 * 1024,
        )
    )


def test_record_loop_lag_warning_does_not_degrade() -> None:
    manager = _build_manager()

    manager._record_loop_lag(1.5)

    assert manager._degraded_until == 0.0


def test_record_loop_lag_degrades_manager(monkeypatch: pytest.MonkeyPatch) -> None:
    manager = _build_manager()
    now = 1234.0
    monkeypatch.setattr("src.services.proxy_node.hub_transport._time.monotonic", lambda: now)

    manager._record_loop_lag(4.0)

    assert manager._degraded_until == pytest.approx(now + 12.0)


@pytest.mark.asyncio
async def test_send_request_rejects_while_manager_degraded(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = _build_manager()
    manager._degraded_until = time.monotonic() + 5.0

    async def _fake_ensure_connected() -> None:
        return None

    monkeypatch.setattr(manager, "ensure_connected", _fake_ensure_connected)

    with pytest.raises(TunnelStreamError, match="event loop degraded"):
        await manager.send_request(
            "node-1",
            method="POST",
            url="https://example.com/v1/chat/completions",
            headers={"content-type": "application/json"},
            body=b"{}",
            timeout=5.0,
        )

    assert manager._pending_streams == {}
