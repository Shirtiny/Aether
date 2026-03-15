from __future__ import annotations

import time

import pytest

from src.clients.http_client import HTTPClientPool


class _DummyClient:
    def __init__(self, *, is_closed: bool = False) -> None:
        self.is_closed = is_closed
        self.close_calls = 0

    async def aclose(self) -> None:
        self.close_calls += 1
        self.is_closed = True


@pytest.mark.asyncio
async def test_cleanup_idle_clients_closes_stale_entries(monkeypatch: pytest.MonkeyPatch) -> None:
    now = time.time()
    stale_proxy = _DummyClient()
    active_proxy = _DummyClient()
    already_closed_proxy = _DummyClient(is_closed=True)
    stale_tunnel = _DummyClient()

    monkeypatch.setattr(
        HTTPClientPool,
        "_proxy_clients",
        {
            "stale": (stale_proxy, now - 1200),
            "active": (active_proxy, now - 10),
            "closed": (already_closed_proxy, now - 1200),
        },
        raising=False,
    )
    monkeypatch.setattr(
        HTTPClientPool,
        "_tunnel_clients",
        {"tunnel-stale": (stale_tunnel, now - 1200)},
        raising=False,
    )

    stats = await HTTPClientPool.cleanup_idle_clients(max_idle_seconds=600)

    assert stats["proxy_closed"] == 1
    assert stats["tunnel_closed"] == 1
    assert stats["proxy_already_closed"] == 1
    assert stats["tunnel_already_closed"] == 0

    assert stale_proxy.close_calls == 1
    assert stale_tunnel.close_calls == 1
    assert active_proxy.close_calls == 0

    assert "active" in HTTPClientPool._proxy_clients
    assert "stale" not in HTTPClientPool._proxy_clients
    assert "closed" not in HTTPClientPool._proxy_clients
    assert "tunnel-stale" not in HTTPClientPool._tunnel_clients
