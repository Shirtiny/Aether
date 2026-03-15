from __future__ import annotations

import inspect
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

import src.services.system.maintenance_scheduler as maintenance_scheduler_module
from src.config.settings import config
from src.services.system.maintenance_scheduler import MaintenanceScheduler


@pytest.mark.asyncio
async def test_maintenance_scheduler_start_skips_startup_task_when_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(config, "maintenance_startup_tasks_enabled", False)

    scheduler = MaintenanceScheduler()

    created = False

    def fake_create_task(coro):  # type: ignore[no-untyped-def]
        nonlocal created
        created = True
        if inspect.iscoroutine(coro):
            coro.close()
        return object()

    monkeypatch.setattr(maintenance_scheduler_module.asyncio, "create_task", fake_create_task)
    monkeypatch.setattr(scheduler, "_get_checkin_time", lambda: (1, 5))
    monkeypatch.setattr(
        maintenance_scheduler_module,
        "get_scheduler",
        lambda: SimpleNamespace(
            add_cron_job=lambda *args, **kwargs: None,
            add_interval_job=lambda *args, **kwargs: None,
        ),
    )

    await scheduler.start()

    assert created is False


def test_http_client_idle_cleanup_interval_env_invalid(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("HTTP_CLIENT_IDLE_CLEANUP_INTERVAL_MINUTES", "bad")
    assert MaintenanceScheduler._get_http_client_idle_cleanup_interval_minutes() == 5


@pytest.mark.asyncio
async def test_candidate_cleanup_uses_dedicated_retention_and_batch_settings(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    scheduler = MaintenanceScheduler()

    class _FakeLoop:
        async def run_in_executor(self, _executor, func):  # type: ignore[no-untyped-def]
            return func()

    class _ConfigSession:
        def close(self) -> None:
            return None

    class _BatchSession:
        def __init__(self, ids: list[str]) -> None:
            self.ids = ids
            self.closed = False
            self.committed = False
            self.query_obj = MagicMock()
            filtered = self.query_obj.filter.return_value
            filtered.order_by.return_value.limit.return_value.all.return_value = [
                SimpleNamespace(id=value) for value in ids
            ]

        def query(self, _model):  # type: ignore[no-untyped-def]
            return self.query_obj

        def execute(self, _statement):  # type: ignore[no-untyped-def]
            return SimpleNamespace(rowcount=len(self.ids))

        def commit(self) -> None:
            self.committed = True

        def rollback(self) -> None:
            raise AssertionError("rollback should not be called")

        def close(self) -> None:
            self.closed = True

    config_session = _ConfigSession()
    batch_one = _BatchSession(["candidate-1", "candidate-2"])
    batch_two = _BatchSession([])
    sessions = iter([config_session, batch_one, batch_two])

    def fake_create_session():  # type: ignore[no-untyped-def]
        return next(sessions)

    config_values = {
        "enable_auto_cleanup": True,
        "request_candidates_retention_days": 21,
        "request_candidates_cleanup_batch_size": 2,
    }

    monkeypatch.setattr(maintenance_scheduler_module, "create_session", fake_create_session)
    monkeypatch.setattr(
        maintenance_scheduler_module.SystemConfigService,
        "get_config",
        lambda _db, key, default=None: config_values.get(key, default),
    )
    monkeypatch.setattr(
        maintenance_scheduler_module.asyncio, "get_running_loop", lambda: _FakeLoop()
    )

    await scheduler._perform_candidate_cleanup()

    batch_one.query_obj.filter.return_value.order_by.return_value.limit.return_value.all.assert_called_once()
    batch_one.query_obj.filter.return_value.order_by.return_value.limit.assert_called_once_with(2)
    assert batch_one.committed is True
    assert batch_one.closed is True
    assert batch_two.closed is True
