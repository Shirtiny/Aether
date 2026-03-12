from __future__ import annotations

import asyncio
from collections.abc import Coroutine
from concurrent.futures import Future
from unittest.mock import AsyncMock

import pytest

from src.services.provider import delete_task as taskmod


class _FakeRedis:
    def __init__(self) -> None:
        self.store: dict[str, str] = {}

    async def get(self, key: str) -> str | None:
        return self.store.get(key)

    async def setex(self, key: str, _ttl: int, value: str) -> None:
        self.store[key] = value


async def _fake_get_redis_client(*, require_redis: bool = False) -> _FakeRedis:
    _ = require_redis
    return _FakeRedis()


@pytest.mark.asyncio
async def test_run_provider_delete_waits_for_progress_updates_before_completion(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    release_progress = asyncio.Event()
    updates: list[dict[str, object]] = []

    async def fake_update_task_field(
        task_id: str,
        r: object | None = None,
        **fields: object,
    ) -> None:
        _ = task_id, r
        updates.append(dict(fields))

    def fake_sync_delete_provider(
        provider_id: str,
        progress_callback: object | None = None,
    ) -> dict[str, object]:
        _ = provider_id
        assert callable(progress_callback)
        progress_callback(
            {
                "stage": "deleting_keys",
                "total_keys": 4,
                "deleted_keys": 2,
                "message": "deleted key batch 1/2",
            }
        )
        return {
            "total_keys": 4,
            "deleted_keys": 4,
            "total_endpoints": 2,
            "deleted_endpoints": 2,
            "elapsed_seconds": 1.2,
        }

    def fake_run_coroutine_threadsafe(
        coro: Coroutine[object, object, object],
        loop: asyncio.AbstractEventLoop,
    ) -> Future[object]:
        future: Future[object] = Future()

        async def runner() -> None:
            await release_progress.wait()
            try:
                result = await coro
            except Exception as exc:
                future.set_exception(exc)
            else:
                future.set_result(result)

        loop.call_soon_threadsafe(lambda: asyncio.create_task(runner()))
        return future

    monkeypatch.setattr(taskmod, "_update_task_field", fake_update_task_field)
    monkeypatch.setattr(taskmod, "_sync_delete_provider", fake_sync_delete_provider)
    monkeypatch.setattr(taskmod.asyncio, "run_coroutine_threadsafe", fake_run_coroutine_threadsafe)
    monkeypatch.setattr(taskmod, "get_redis_client", _fake_get_redis_client)
    monkeypatch.setattr(taskmod, "invalidate_models_list_cache", AsyncMock())
    monkeypatch.setattr(taskmod.ModelCacheService, "invalidate_all_resolve_cache", AsyncMock())
    monkeypatch.setattr(taskmod.ProviderCacheService, "invalidate_provider_cache", AsyncMock())

    task = asyncio.create_task(taskmod._run_provider_delete("task-1", "provider-1"))

    await asyncio.sleep(0.05)

    assert not task.done()
    assert updates == [
        {"status": taskmod.STATUS_RUNNING, "stage": "queued", "message": "delete task started"}
    ]

    release_progress.set()
    await task

    assert updates == [
        {"status": taskmod.STATUS_RUNNING, "stage": "queued", "message": "delete task started"},
        {
            "stage": "deleting_keys",
            "total_keys": 4,
            "deleted_keys": 2,
            "message": "deleted key batch 1/2",
        },
        {
            "status": taskmod.STATUS_COMPLETED,
            "stage": "completed",
            "total_keys": 4,
            "deleted_keys": 4,
            "total_endpoints": 2,
            "deleted_endpoints": 2,
            "message": "provider deleted: keys=4, endpoints=2",
        },
    ]


@pytest.mark.asyncio
async def test_submit_provider_delete_reuses_running_task(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    redis = _FakeRedis()
    started = asyncio.Event()
    finish = asyncio.Event()

    async def fake_get_redis(*, require_redis: bool = False) -> _FakeRedis:
        _ = require_redis
        return redis

    async def fake_run_provider_delete(task_id: str, provider_id: str) -> None:
        _ = task_id, provider_id
        started.set()
        await finish.wait()

    monkeypatch.setattr(taskmod, "get_redis_client", fake_get_redis)
    monkeypatch.setattr(taskmod, "_run_provider_delete", fake_run_provider_delete)

    task_id_1 = await taskmod.submit_provider_delete("provider-1")
    await started.wait()
    task_id_2 = await taskmod.submit_provider_delete("provider-1")

    assert task_id_2 == task_id_1

    finish.set()
    await asyncio.sleep(0)
