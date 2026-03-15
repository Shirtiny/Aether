from __future__ import annotations

from collections import OrderedDict

import pytest

import src.clients.curl_cffi_transport as transport_module


class _DummySession:
    def __init__(self, **kwargs: object) -> None:
        self.kwargs = kwargs
        self.close_calls = 0

    async def close(self) -> None:
        self.close_calls += 1


@pytest.mark.asyncio
async def test_get_or_create_session_uses_lru_eviction(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(transport_module, "AsyncSession", _DummySession, raising=False)
    monkeypatch.setattr(transport_module, "_MAX_SESSIONS", 2, raising=False)
    monkeypatch.setattr(transport_module, "_session_pool", OrderedDict(), raising=False)

    s1 = await transport_module._get_or_create_session("chrome120", "http://p1")
    s2 = await transport_module._get_or_create_session("chrome120", "http://p2")

    # 命中 s1 使其变成最近使用，随后新增 s3 应淘汰 s2。
    s1_hit = await transport_module._get_or_create_session("chrome120", "http://p1")
    assert s1_hit is s1

    s3 = await transport_module._get_or_create_session("chrome120", "http://p3")
    assert isinstance(s3, _DummySession)

    assert len(transport_module._session_pool) == 2
    assert "chrome120::http://p1" in transport_module._session_pool
    assert "chrome120::http://p3" in transport_module._session_pool
    assert "chrome120::http://p2" not in transport_module._session_pool
    assert s2.close_calls == 1
    assert s1.close_calls == 0
