from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from src.api.admin.providers.routes import (
    AdminDeleteProviderAdapter,
    AdminProviderDeleteTaskStatusAdapter,
)


@pytest.mark.asyncio
async def test_delete_provider_adapter_submits_async_task_and_deactivates_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = MagicMock()
    provider = SimpleNamespace(id="provider-1", name="Provider 1", is_active=True)
    db.query.return_value.filter.return_value.first.return_value = provider

    submit_task_mock = AsyncMock(return_value="task-1")
    invalidate_models_mock = AsyncMock()
    invalidate_resolve_mock = AsyncMock()
    invalidate_provider_cache_mock = AsyncMock()

    monkeypatch.setattr(
        "src.api.admin.providers.routes.submit_provider_delete",
        submit_task_mock,
    )
    monkeypatch.setattr(
        "src.api.admin.providers.routes.invalidate_models_list_cache",
        invalidate_models_mock,
    )
    monkeypatch.setattr(
        "src.api.admin.providers.routes.ModelCacheService.invalidate_all_resolve_cache",
        invalidate_resolve_mock,
    )
    monkeypatch.setattr(
        "src.api.admin.providers.routes.ProviderCacheService.invalidate_provider_cache",
        invalidate_provider_cache_mock,
    )

    audit_calls: list[dict[str, object]] = []
    context = SimpleNamespace(
        db=db,
        request=SimpleNamespace(state=SimpleNamespace()),
        add_audit_metadata=lambda **kwargs: audit_calls.append(kwargs),
    )

    adapter = AdminDeleteProviderAdapter(provider_id="provider-1")
    result = await adapter.handle(context)

    assert result == {
        "task_id": "task-1",
        "status": "pending",
        "message": "删除任务已提交，提供商已进入后台删除队列",
    }
    submit_task_mock.assert_awaited_once_with("provider-1")
    assert provider.is_active is False
    db.commit.assert_called_once()
    invalidate_models_mock.assert_awaited_once()
    invalidate_resolve_mock.assert_awaited_once()
    invalidate_provider_cache_mock.assert_awaited_once_with("provider-1")
    assert audit_calls[0]["action"] == "delete_provider"
    assert audit_calls[1]["task_id"] == "task-1"
    assert audit_calls[1]["provider_deactivated"] is True


@pytest.mark.asyncio
async def test_delete_provider_adapter_reuses_task_without_extra_commit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = MagicMock()
    provider = SimpleNamespace(id="provider-1", name="Provider 1", is_active=False)
    db.query.return_value.filter.return_value.first.return_value = provider

    submit_task_mock = AsyncMock(return_value="task-1")

    monkeypatch.setattr(
        "src.api.admin.providers.routes.submit_provider_delete",
        submit_task_mock,
    )

    context = SimpleNamespace(
        db=db,
        request=SimpleNamespace(state=SimpleNamespace()),
        add_audit_metadata=lambda **kwargs: None,
    )

    adapter = AdminDeleteProviderAdapter(provider_id="provider-1")
    result = await adapter.handle(context)

    assert result["task_id"] == "task-1"
    db.commit.assert_not_called()


@pytest.mark.asyncio
async def test_delete_provider_task_status_adapter_returns_task_payload(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    task = SimpleNamespace(
        task_id="task-1",
        provider_id="provider-1",
        status="running",
        stage="deleting_keys",
        total_keys=100,
        deleted_keys=25,
        total_endpoints=8,
        deleted_endpoints=2,
        message="deleted key batch 1/2",
    )

    monkeypatch.setattr(
        "src.api.admin.providers.routes.get_provider_delete_task",
        AsyncMock(return_value=task),
    )

    context = SimpleNamespace(db=MagicMock(), request=SimpleNamespace(state=SimpleNamespace()))
    adapter = AdminProviderDeleteTaskStatusAdapter(provider_id="provider-1", task_id="task-1")

    result = await adapter.handle(context)

    assert result.task_id == "task-1"
    assert result.status == "running"
    assert result.stage == "deleting_keys"
    assert result.deleted_keys == 25
    assert result.deleted_endpoints == 2
