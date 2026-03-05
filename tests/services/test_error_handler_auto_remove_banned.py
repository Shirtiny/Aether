from __future__ import annotations

from types import SimpleNamespace
from typing import Any, cast

from src.services.orchestration.error_handler import ErrorHandlerService


class _FakeDB:
    def __init__(self) -> None:
        self.deleted: list[object] = []
        self.commit_count = 0

    def delete(self, obj: object) -> None:
        self.deleted.append(obj)

    def commit(self) -> None:
        self.commit_count += 1


def _build_key() -> SimpleNamespace:
    return SimpleNamespace(
        id="k1",
        provider_id="p1",
        name="k1-name",
        auth_type="oauth",
        auth_config=None,
        oauth_invalid_at=None,
        oauth_invalid_reason=None,
        is_active=True,
    )


def test_mark_oauth_key_blocked_auto_remove_enabled(monkeypatch: Any) -> None:
    db = _FakeDB()
    service = ErrorHandlerService(db=cast(Any, db))
    key = _build_key()
    provider = SimpleNamespace(config={"pool_advanced": {"auto_remove_banned_keys": True}})

    monkeypatch.setattr(
        ErrorHandlerService,
        "_schedule_auto_cleanup_after_delete",
        staticmethod(lambda **kwargs: None),
    )

    service._mark_oauth_key_blocked(cast(Any, key), "req-1", provider=cast(Any, provider))

    assert db.commit_count == 1
    assert db.deleted == [key]
    assert key.is_active is False
    assert str(key.oauth_invalid_reason).startswith("[ACCOUNT_BLOCK] ")


def test_mark_oauth_key_blocked_auto_remove_disabled() -> None:
    db = _FakeDB()
    service = ErrorHandlerService(db=cast(Any, db))
    key = _build_key()
    provider = SimpleNamespace(config={"pool_advanced": {"auto_remove_banned_keys": False}})

    service._mark_oauth_key_blocked(cast(Any, key), "req-1", provider=cast(Any, provider))

    assert db.commit_count == 1
    assert db.deleted == []
    assert key.is_active is False
    assert str(key.oauth_invalid_reason).startswith("[ACCOUNT_BLOCK] ")
