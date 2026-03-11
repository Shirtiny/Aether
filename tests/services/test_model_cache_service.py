from types import SimpleNamespace
from typing import Any, Callable, cast

import pytest

from src.models.database import GlobalModel, Model
from src.services.cache.model_cache import ModelCacheService
from src.core.cache_service import CacheService


class _FakeQuery:
    def __init__(
        self,
        *,
        first_result: Any = None,
        all_result: list[Any] | None = None,
        on_all: Callable[[], None] | None = None,
    ) -> None:
        self._first_result = first_result
        self._all_result = all_result if all_result is not None else []
        self._on_all = on_all

    def join(self, *_args: object, **_kwargs: object) -> "_FakeQuery":
        return self

    def filter(self, *_args: object, **_kwargs: object) -> "_FakeQuery":
        return self

    def first(self) -> Any:
        return self._first_result

    def all(self) -> list[Any]:
        if self._on_all:
            self._on_all()
        return self._all_result


class _FakeSession:
    def __init__(self, *, direct_match: GlobalModel) -> None:
        self._direct_match = direct_match

    def query(self, *entities: object) -> "_FakeQuery":
        if entities == (GlobalModel,):
            return _FakeQuery(first_result=self._direct_match)

        # 如果 direct match 命中，不应再走 provider_model_name 分支
        if entities == (Model, GlobalModel):
            raise AssertionError(
                "provider_model_name query should not run when direct match exists"
            )

        raise AssertionError(f"Unexpected query entities: {entities}")


class _MappingIndexSession:
    def __init__(
        self,
        *,
        provider_mapping_rows: list[tuple[object, GlobalModel]],
    ) -> None:
        self._provider_mapping_rows = provider_mapping_rows
        self.provider_mapping_scan_count = 0
        self.model_global_query_count = 0

    def query(self, *entities: object) -> "_FakeQuery":
        if entities == (GlobalModel,):
            return _FakeQuery(first_result=None, all_result=[])

        if entities == (Model, GlobalModel):
            self.model_global_query_count += 1
            if self.model_global_query_count in {1, 3}:
                return _FakeQuery(all_result=[])
            if self.model_global_query_count == 2:
                return _FakeQuery(
                    all_result=self._provider_mapping_rows,
                    on_all=self._record_provider_mapping_scan,
                )
            raise AssertionError("provider_model_mappings 全量扫描被重复触发")

        raise AssertionError(f"Unexpected query entities: {entities}")

    def _record_provider_mapping_scan(self) -> None:
        self.provider_mapping_scan_count += 1


@pytest.mark.asyncio
async def test_resolve_global_model_prefers_direct_match(monkeypatch: pytest.MonkeyPatch) -> None:
    async def _fake_get(_key: str) -> None:
        return None

    async def _fake_set(_key: str, _value: object, ttl_seconds: int = 60) -> bool:  # noqa: ARG001
        return True

    monkeypatch.setattr(CacheService, "get", staticmethod(_fake_get))
    monkeypatch.setattr(CacheService, "set", staticmethod(_fake_set))

    global_model = GlobalModel(
        id="gm-1",
        name="claude-haiku-4-5-20251001",
        display_name="Claude Haiku 4.5",
        supported_capabilities=[],
        config={},
        default_tiered_pricing=None,
        default_price_per_request=None,
        is_active=True,
    )
    db = _FakeSession(direct_match=global_model)

    resolved = await ModelCacheService.resolve_global_model_by_name_or_mapping(
        cast(Any, db),
        cast(str, global_model.name),
    )
    assert resolved is global_model


@pytest.mark.asyncio
async def test_resolve_global_model_reuses_provider_mapping_index_cache(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cache_store: dict[str, object] = {}

    async def _fake_get(key: str) -> object | None:
        return cache_store.get(key)

    async def _fake_set(key: str, value: object, ttl_seconds: int = 60) -> bool:  # noqa: ARG001
        cache_store[key] = value
        return True

    monkeypatch.setattr(CacheService, "get", staticmethod(_fake_get))
    monkeypatch.setattr(CacheService, "set", staticmethod(_fake_set))

    global_model_one = GlobalModel(
        id="gm-1",
        name="gpt-4o",
        display_name="GPT-4o",
        supported_capabilities=[],
        config={},
        default_tiered_pricing=None,
        default_price_per_request=None,
        is_active=True,
    )
    global_model_two = GlobalModel(
        id="gm-2",
        name="claude-3-7-sonnet",
        display_name="Claude 3.7 Sonnet",
        supported_capabilities=[],
        config={},
        default_tiered_pricing=None,
        default_price_per_request=None,
        is_active=True,
    )

    model_one = SimpleNamespace(
        id="m-1",
        provider_model_mappings=[{"name": "mapped-one"}],
    )
    model_two = SimpleNamespace(
        id="m-2",
        provider_model_mappings=[{"name": "mapped-two"}],
    )

    db = _MappingIndexSession(
        provider_mapping_rows=[
            (model_one, global_model_one),
            (model_two, global_model_two),
        ]
    )

    resolved_one = await ModelCacheService.resolve_global_model_by_name_or_mapping(
        cast(Any, db), "mapped-one"
    )
    resolved_two = await ModelCacheService.resolve_global_model_by_name_or_mapping(
        cast(Any, db), "mapped-two"
    )

    assert resolved_one is not None
    assert resolved_one.name == "gpt-4o"
    assert resolved_two is not None
    assert resolved_two.name == "claude-3-7-sonnet"
    assert db.provider_mapping_scan_count == 1


@pytest.mark.asyncio
async def test_invalidate_model_cache_clears_provider_mapping_index(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    deleted_keys: list[str] = []

    async def _fake_delete(key: str) -> bool:
        deleted_keys.append(key)
        return True

    monkeypatch.setattr(CacheService, "delete", staticmethod(_fake_delete))

    await ModelCacheService.invalidate_model_cache(
        model_id="model-1",
        provider_model_name="provider-model",
        provider_model_mappings=[{"name": "alias-model"}],
    )

    assert "model:id:model-1" in deleted_keys
    assert "global_model:resolve:provider-model" in deleted_keys
    assert "global_model:resolve:alias-model" in deleted_keys
    assert ModelCacheService.PROVIDER_MAPPING_INDEX_CACHE_KEY in deleted_keys
