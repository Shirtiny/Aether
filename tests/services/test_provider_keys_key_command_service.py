from __future__ import annotations

import sys
import types
from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any, cast

import pytest

from src.core.exceptions import InvalidRequestException
from src.models.endpoint_models import EndpointAPIKeyCreate, EndpointAPIKeyUpdate


async def _noop_invalidate_models_list_cache() -> None:
    return None


_fake_models_service_module = types.ModuleType("src.api.base.models_service")
setattr(
    _fake_models_service_module, "invalidate_models_list_cache", _noop_invalidate_models_list_cache
)
sys.modules.setdefault("src.api.base.models_service", _fake_models_service_module)

from src.services.provider_keys import key_command_service as command_module
from src.services.provider_keys import key_side_effects as side_effects_module


class _NoQueryDB:
    def query(self, *args: Any, **kwargs: Any) -> Any:  # pragma: no cover - 防御断言
        _ = args, kwargs
        raise AssertionError("unexpected query call")


class _FakeQuery:
    def __init__(self, key: Any) -> None:
        self._key = key

    def filter(self, *args: Any, **kwargs: Any) -> _FakeQuery:
        _ = args, kwargs
        return self

    def first(self) -> Any:
        return self._key


class _FakeClearOAuthDB:
    def __init__(self, key: Any) -> None:
        self._key = key
        self.commit_count = 0

    def query(self, model: Any) -> _FakeQuery:
        _ = model
        return _FakeQuery(self._key)

    def commit(self) -> None:
        self.commit_count += 1


class _FakeQueryAll:
    def __init__(self, rows: list[Any]) -> None:
        self._rows = rows

    def filter(self, *args: Any, **kwargs: Any) -> _FakeQueryAll:
        _ = args, kwargs
        return self

    def all(self) -> list[Any]:
        return self._rows


def _build_key(**overrides: Any) -> SimpleNamespace:
    base: dict[str, Any] = {
        "auto_fetch_models": False,
        "allowed_models": None,
        "model_include_patterns": None,
        "model_exclude_patterns": None,
        "provider_id": "provider-1",
        "auth_type": "api_key",
        "api_formats": [],
        "provider": None,
    }
    base.update(overrides)
    return SimpleNamespace(**base)


def test_key_create_accepts_zero_max_probe_interval_minutes() -> None:
    payload = EndpointAPIKeyCreate.model_validate(
        {
            "name": "key-1",
            "api_key": "secret",
            "api_formats": ["openai:chat"],
            "max_probe_interval_minutes": 0,
        }
    )

    assert payload.max_probe_interval_minutes == 0


def test_key_update_accepts_zero_max_probe_interval_minutes() -> None:
    payload = EndpointAPIKeyUpdate.model_validate({"max_probe_interval_minutes": 0})

    assert payload.max_probe_interval_minutes == 0


def test_prepare_update_payload_auth_type_null_ignored() -> None:
    key = _build_key(auth_type="api_key")
    key_data = EndpointAPIKeyUpdate.model_validate({"auth_type": None})

    prepared = command_module._prepare_update_key_payload(
        db=cast(Any, _NoQueryDB()),
        key=cast(Any, key),
        key_id="key-1",
        key_data=key_data,
    )

    assert "auth_type" not in prepared.update_data


def test_prepare_update_payload_rejects_empty_api_key() -> None:
    key = _build_key(auth_type="api_key")
    key_data = EndpointAPIKeyUpdate.model_validate({"api_key": "   "})

    with pytest.raises(InvalidRequestException, match="api_key 不能为空"):
        command_module._prepare_update_key_payload(
            db=cast(Any, _NoQueryDB()),
            key=cast(Any, key),
            key_id="key-1",
            key_data=key_data,
        )


def test_prepare_update_payload_encrypts_empty_auth_config_dict(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    key = _build_key(auth_type="vertex_ai")
    key_data = EndpointAPIKeyUpdate.model_validate({"auth_config": {}})

    monkeypatch.setattr(command_module.crypto_service, "encrypt", lambda raw: f"ENC:{raw}")

    prepared = command_module._prepare_update_key_payload(
        db=cast(Any, _NoQueryDB()),
        key=cast(Any, key),
        key_id="key-1",
        key_data=key_data,
    )

    assert prepared.update_data["auth_config"] == "ENC:{}"


def test_prepare_update_payload_vertex_to_oauth_clears_auth_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    key = _build_key(auth_type="vertex_ai")
    key_data = EndpointAPIKeyUpdate.model_validate({"auth_type": "oauth"})

    monkeypatch.setattr(command_module.crypto_service, "encrypt", lambda raw: f"ENC:{raw}")

    prepared = command_module._prepare_update_key_payload(
        db=cast(Any, _NoQueryDB()),
        key=cast(Any, key),
        key_id="key-1",
        key_data=key_data,
    )

    assert prepared.update_data["auth_config"] is None
    assert prepared.update_data["api_key"] == "ENC:__placeholder__"


def test_clear_oauth_invalid_response_invalidates_caches(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cache_calls: list[tuple[str, str | None]] = []

    async def _fake_invalidate_key_cache(key_id: str) -> None:
        cache_calls.append(("key", key_id))

    async def _fake_invalidate_models_cache() -> None:
        cache_calls.append(("models", None))

    fake_provider_cache_module = types.ModuleType("src.services.cache.provider_cache")

    class _FakeProviderCacheService:
        @staticmethod
        async def invalidate_provider_api_key_cache(key_id: str) -> None:
            await _fake_invalidate_key_cache(key_id)

    setattr(fake_provider_cache_module, "ProviderCacheService", _FakeProviderCacheService)
    monkeypatch.setitem(
        sys.modules, "src.services.cache.provider_cache", fake_provider_cache_module
    )

    fake_models_service_module = types.ModuleType("src.services.cache.model_list_cache")
    setattr(
        fake_models_service_module, "invalidate_models_list_cache", _fake_invalidate_models_cache
    )
    monkeypatch.setitem(
        sys.modules, "src.services.cache.model_list_cache", fake_models_service_module
    )

    key = SimpleNamespace(
        oauth_invalid_at=datetime.now(timezone.utc),
        oauth_invalid_reason="forbidden",
        is_active=False,
    )
    db = _FakeClearOAuthDB(key=key)

    result = command_module.clear_oauth_invalid_response(cast(Any, db), key_id="key-1")

    assert result["message"] == "已清除 OAuth 失效标记，Key 已自动启用"
    assert key.oauth_invalid_at is None
    assert key.oauth_invalid_reason is None
    assert key.is_active is True
    assert db.commit_count == 1
    assert cache_calls == [("key", "key-1"), ("models", None)]


@pytest.mark.asyncio
async def test_run_delete_key_side_effects_skip_disassociate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    async def _fake_on_key_allowed_models_changed(**kwargs: Any) -> None:
        captured.update(kwargs)

    from src.services.model import global_model as global_model_module

    monkeypatch.setattr(
        global_model_module,
        "on_key_allowed_models_changed",
        _fake_on_key_allowed_models_changed,
    )

    await side_effects_module.run_delete_key_side_effects(
        db=cast(Any, object()),
        provider_id="provider-1",
        deleted_key_allowed_models=None,
    )

    assert captured["provider_id"] == "provider-1"
    assert captured["skip_disassociate"] is True


def test_cleanup_key_references_preserves_usage_and_video_tasks(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _FakeDeleteStatement:
        def __init__(self, model: Any) -> None:
            self.kind = "delete"
            self.model = model
            self.conditions: list[Any] = []

        def where(self, *conditions: Any) -> _FakeDeleteStatement:
            self.conditions.extend(conditions)
            return self

    class _FakeUpdateStatement:
        def __init__(self, model: Any) -> None:
            self.kind = "update"
            self.model = model
            self.conditions: list[Any] = []
            self.values_dict: dict[str, Any] = {}

        def where(self, *conditions: Any) -> _FakeUpdateStatement:
            self.conditions.extend(conditions)
            return self

        def values(self, **values: Any) -> _FakeUpdateStatement:
            self.values_dict.update(values)
            return self

    class _FakeDB:
        def __init__(self) -> None:
            self.statements: list[Any] = []

        def execute(self, statement: Any) -> None:
            self.statements.append(statement)

        def get_bind(self) -> Any:
            return SimpleNamespace(dialect=SimpleNamespace(name="postgresql"))

    monkeypatch.setattr(side_effects_module, "sa_delete", lambda model: _FakeDeleteStatement(model))
    monkeypatch.setattr(side_effects_module, "sa_update", lambda model: _FakeUpdateStatement(model))

    db = _FakeDB()
    side_effects_module.cleanup_key_references(cast(Any, db), ["key-1", "key-2"])

    assert [(stmt.kind, stmt.model.__name__) for stmt in db.statements] == [
        ("delete", "RequestCandidate"),
        ("delete", "GeminiFileMapping"),
        ("update", "Usage"),
        ("update", "VideoTask"),
    ]
    assert db.statements[2].values_dict == {"provider_api_key_id": None}
    assert db.statements[3].values_dict == {"key_id": None}


@pytest.mark.asyncio
async def test_batch_delete_endpoint_keys_response_cleans_related_references(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cleanup_calls: list[list[str]] = []
    side_effect_calls: list[str | None] = []

    class _FakeBatchDeleteDB:
        def __init__(self, keys: list[Any]) -> None:
            self._keys = keys
            self.commit_count = 0
            self.executed: list[Any] = []

        def query(self, model: Any) -> _FakeQueryAll:
            _ = model
            return _FakeQueryAll(self._keys)

        def execute(self, statement: Any) -> None:
            self.executed.append(statement)

        def commit(self) -> None:
            self.commit_count += 1

        def rollback(self) -> None:
            raise AssertionError("rollback should not be called")

    async def _fake_run_delete_key_side_effects(
        db: Any,
        provider_id: str | None,
        deleted_key_allowed_models: list[str] | None,
    ) -> None:
        _ = db, deleted_key_allowed_models
        side_effect_calls.append(provider_id)

    monkeypatch.setattr(
        command_module,
        "cleanup_key_references",
        lambda _db, key_ids: cleanup_calls.append(list(key_ids)),
    )
    monkeypatch.setattr(
        command_module,
        "run_delete_key_side_effects",
        _fake_run_delete_key_side_effects,
    )

    keys = [
        SimpleNamespace(id="key-1", provider_id="provider-1"),
        SimpleNamespace(id="key-2", provider_id="provider-1"),
    ]
    db = _FakeBatchDeleteDB(keys)

    result = await command_module.batch_delete_endpoint_keys_response(
        cast(Any, db),
        ["key-1", "key-2"],
    )

    assert result["success_count"] == 2
    assert result["failed_count"] == 0
    assert cleanup_calls == [["key-1", "key-2"]] or cleanup_calls == [["key-2", "key-1"]]
    assert side_effect_calls == ["provider-1"]
    assert db.commit_count == 1
    assert len(db.executed) == 1
