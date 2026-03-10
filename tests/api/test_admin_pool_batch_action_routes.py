from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from src.api.admin.pool.routes import AdminBatchActionKeysAdapter
from src.api.admin.pool.schemas import BatchActionRequest


def _build_context(db: MagicMock) -> SimpleNamespace:
    return SimpleNamespace(
        db=db,
        user=SimpleNamespace(username="admin-1"),
        add_audit_metadata=lambda **_: None,
    )


def _mock_provider_lookup(db: MagicMock, provider_id: str) -> None:
    db.query.return_value.filter.return_value.first.return_value = SimpleNamespace(id=provider_id)


@pytest.mark.asyncio
async def test_batch_delete_uses_single_statement_for_non_sqlite(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = MagicMock()
    provider_id = "provider-1"
    _mock_provider_lookup(db, provider_id)
    db.get_bind.return_value = SimpleNamespace(dialect=SimpleNamespace(name="postgresql"))
    db.execute.return_value = SimpleNamespace(rowcount=1200)
    side_effect = AsyncMock()
    monkeypatch.setattr(
        "src.services.provider_keys.key_side_effects.run_delete_key_side_effects",
        side_effect,
    )

    adapter = AdminBatchActionKeysAdapter(
        provider_id=provider_id,
        body=BatchActionRequest(
            key_ids=[f"key-{idx}" for idx in range(1200)],
            action="delete",
        ),
    )

    result = await adapter.handle(_build_context(db))

    assert result.affected == 1200
    assert db.execute.call_count == 1
    db.commit.assert_called_once()
    side_effect.assert_awaited_once_with(
        db=db,
        provider_id=provider_id,
        deleted_key_allowed_models=None,
    )


@pytest.mark.asyncio
async def test_batch_delete_chunks_sqlite_and_runs_side_effect_once(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = MagicMock()
    provider_id = "provider-2"
    _mock_provider_lookup(db, provider_id)
    db.get_bind.return_value = SimpleNamespace(dialect=SimpleNamespace(name="sqlite"))
    db.execute.side_effect = [
        SimpleNamespace(rowcount=900),
        SimpleNamespace(rowcount=300),
    ]
    side_effect = AsyncMock()
    monkeypatch.setattr(
        "src.services.provider_keys.key_side_effects.run_delete_key_side_effects",
        side_effect,
    )

    adapter = AdminBatchActionKeysAdapter(
        provider_id=provider_id,
        body=BatchActionRequest(
            key_ids=[f"key-{idx}" for idx in range(1200)],
            action="delete",
        ),
    )

    result = await adapter.handle(_build_context(db))

    assert result.affected == 1200
    assert db.execute.call_count == 2
    db.commit.assert_called_once()
    side_effect.assert_awaited_once_with(
        db=db,
        provider_id=provider_id,
        deleted_key_allowed_models=None,
    )
