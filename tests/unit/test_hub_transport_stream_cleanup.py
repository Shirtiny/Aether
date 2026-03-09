from __future__ import annotations

import pytest

from src.services.proxy_node.hub_transport import HubResponseStream
from src.services.proxy_node.tunnel_manager import _StreamState


class _Manager:
    def __init__(self) -> None:
        self.removed: list[int] = []

    def remove_stream(self, stream_id: int) -> None:
        self.removed.append(stream_id)


@pytest.mark.asyncio
async def test_hub_response_stream_removes_stream_after_normal_iteration() -> None:
    manager = _Manager()
    state = _StreamState(7)
    state.set_response_headers(200, {})
    state.push_body_chunk(b"hello")
    state.set_done()

    stream = HubResponseStream(manager, state, timeout=0.1)
    chunks = []
    async for chunk in stream:
        chunks.append(chunk)

    assert chunks == [b"hello"]
    assert manager.removed == [7]


@pytest.mark.asyncio
async def test_hub_response_stream_removes_stream_after_body_error() -> None:
    manager = _Manager()
    state = _StreamState(9)
    state.set_response_headers(200, {})
    state.set_error("boom")

    stream = HubResponseStream(manager, state, timeout=0.1)

    with pytest.raises(Exception):
        async for _chunk in stream:
            pass

    assert manager.removed == [9]
