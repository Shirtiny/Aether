from __future__ import annotations

import asyncio
from collections.abc import Coroutine, Generator
from concurrent.futures import Future
from contextlib import contextmanager
from types import SimpleNamespace

import pytest

from src.services.provider_keys import batch_delete_task as taskmod


@contextmanager
def _fake_db_context() -> Generator[object, None, None]:
    yield object()


async def _fake_get_redis_client(*, require_redis: bool = False) -> object:
    _ = require_redis
    return object()


async def _noop_delete_side_effects(**_kwargs: object) -> None:
    return None


@pytest.mark.asyncio
async def test_run_batch_delete_waits_for_progress_updates_before_completion(
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

    def fake_sync_delete(
        provider_id: str,
        key_ids: list[str],
        progress_callback: object | None = None,
    ) -> int:
        _ = provider_id, key_ids
        assert callable(progress_callback)
        progress_callback(1)
        return 1

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

    monkeypatch.setattr(taskmod, "get_redis_client", _fake_get_redis_client)
    monkeypatch.setattr(taskmod, "_update_task_field", fake_update_task_field)
    monkeypatch.setattr(taskmod, "_sync_delete", fake_sync_delete)
    monkeypatch.setattr(taskmod.asyncio, "run_coroutine_threadsafe", fake_run_coroutine_threadsafe)
    monkeypatch.setattr("src.database.get_db_context", _fake_db_context)
    monkeypatch.setattr(
        "src.services.provider_keys.key_side_effects.run_delete_key_side_effects",
        _noop_delete_side_effects,
    )

    task = asyncio.create_task(taskmod._run_batch_delete("task-1", "provider-1", ["key-1"]))

    await asyncio.sleep(0.05)

    assert not task.done()
    assert updates == [{"status": taskmod.STATUS_RUNNING}]

    release_progress.set()
    await task

    assert updates == [
        {"status": taskmod.STATUS_RUNNING},
        {"deleted": 1},
        {
            "status": taskmod.STATUS_COMPLETED,
            "deleted": 1,
            "message": "1 keys deleted",
        },
    ]


def test_sync_delete_reports_progress_after_each_batch(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _FakeColumn:
        def __eq__(self, other: object) -> tuple[str, object]:  # type: ignore[override]
            return ("eq", other)

        def in_(self, values: list[str]) -> tuple[str, tuple[str, ...]]:
            return ("in", tuple(values))

    class _FakeProviderAPIKey:
        provider_id = _FakeColumn()
        id = _FakeColumn()

    class _FakeDeleteStatement:
        def where(self, *_conditions: object) -> "_FakeDeleteStatement":
            return self

    class _FakeSession:
        def __init__(self) -> None:
            self.rowcounts = [2, 1]
            self.commits = 0
            self.closed = False

        def execute(self, _statement: object) -> SimpleNamespace:
            # SET LOCAL statement_timeout 不消耗 rowcount
            if hasattr(_statement, "text"):
                return SimpleNamespace(rowcount=0)
            return SimpleNamespace(rowcount=self.rowcounts.pop(0))

        def commit(self) -> None:
            self.commits += 1

        def rollback(self) -> None:
            raise AssertionError("rollback should not be called")

        def close(self) -> None:
            self.closed = True

    session = _FakeSession()
    progress_updates: list[int] = []

    monkeypatch.setattr(taskmod, "_CLEANUP_BATCH_SIZE", 2)
    monkeypatch.setattr("src.database.create_session", lambda: session)
    monkeypatch.setattr(
        "src.models.database.ProviderAPIKey",
        _FakeProviderAPIKey,
    )
    monkeypatch.setattr(taskmod, "sa_delete", lambda _model: _FakeDeleteStatement())

    affected = taskmod._sync_delete(
        "provider-1",
        ["key-1", "key-2", "key-3"],
        progress_updates.append,
    )

    assert affected == 3
    assert progress_updates == [2, 3]
    assert session.commits == 2
    assert session.closed is True
