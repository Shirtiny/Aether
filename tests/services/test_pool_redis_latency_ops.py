"""Tests for pool redis latency operations."""

from __future__ import annotations

from unittest.mock import AsyncMock, patch

import pytest

from src.services.provider.pool import redis_ops


class _FakePipe:
    def __init__(self, execute_result: list[object] | None = None) -> None:
        self.ops: list[tuple] = []
        self._execute_result = execute_result or []

    def zadd(self, key: str, mapping: dict[str, float]):
        self.ops.append(("zadd", key, mapping))
        return self

    def zremrangebyscore(self, key: str, start: str, stop: float):
        self.ops.append(("zremrangebyscore", key, start, stop))
        return self

    def zremrangebyrank(self, key: str, start: int, stop: int):
        self.ops.append(("zremrangebyrank", key, start, stop))
        return self

    def expire(self, key: str, ttl: int):
        self.ops.append(("expire", key, ttl))
        return self

    def eval(self, script: str, numkeys: int, key: str, window_start: str):
        self.ops.append(("eval", script, numkeys, key, window_start))
        return self

    async def execute(self):
        return self._execute_result


class _FakeRedis:
    def __init__(self, pipe: _FakePipe) -> None:
        self._pipe = pipe

    def pipeline(self) -> _FakePipe:
        return self._pipe


@pytest.mark.asyncio
async def test_record_latency_writes_sample_and_trims() -> None:
    pipe = _FakePipe()
    fake_redis = _FakeRedis(pipe)
    with patch(
        "src.services.provider.pool.redis_ops._get_redis",
        new_callable=AsyncMock,
        return_value=fake_redis,
    ):
        await redis_ops.record_latency(
            provider_id="prov-1",
            key_id="key-1",
            ttfb_ms=250,
            window_seconds=3600,
            sample_limit=50,
        )

    op_names = [item[0] for item in pipe.ops]
    assert op_names == ["zadd", "zremrangebyscore", "zremrangebyrank", "expire"]
    assert pipe.ops[0][1] == "ap:prov-1:latency:key-1"
    assert pipe.ops[2][2:] == (0, -51)


@pytest.mark.asyncio
async def test_batch_get_latency_avgs_returns_numeric_results_only() -> None:
    pipe = _FakePipe(execute_result=[120.5, None, "330"])
    fake_redis = _FakeRedis(pipe)
    with patch(
        "src.services.provider.pool.redis_ops._get_redis",
        new_callable=AsyncMock,
        return_value=fake_redis,
    ):
        result = await redis_ops.batch_get_latency_avgs(
            provider_id="prov-1",
            key_ids=["k1", "k2", "k3"],
            window_seconds=3600,
        )

    assert result == {"k1": 120.5, "k3": 330.0}
    assert len([item for item in pipe.ops if item[0] == "eval"]) == 3
