import json
from fnmatch import fnmatch

import pytest

from src.services.scheduling.affinity_manager import CacheAffinityManager


class _FakeRedis:
    def __init__(self, store: dict[str, str]):
        self.store = dict(store)
        self.keys_called = 0
        self.scan_calls = 0
        self.unlink_batches: list[tuple[str, ...]] = []
        self.delete_batches: list[tuple[str, ...]] = []

    async def scan(  # type: ignore[override]
        self, cursor: int = 0, match: str | None = None, count: int = 10
    ) -> tuple[int, list[str]]:
        self.scan_calls += 1
        pattern = match or "*"
        matched = [key for key in sorted(self.store) if fnmatch(key, pattern)]
        start = int(cursor)
        batch = matched[start : start + count]
        next_cursor = 0 if start + count >= len(matched) else start + count
        return next_cursor, batch

    async def get(self, key: str) -> str | None:
        return self.store.get(key)

    async def mget(self, keys: list[str]) -> list[str | None]:
        return [self.store.get(k) for k in keys]

    async def unlink(self, *keys: str) -> int:
        self.unlink_batches.append(tuple(keys))
        deleted = 0
        for key in keys:
            if key in self.store:
                deleted += 1
                self.store.pop(key, None)
        return deleted

    async def delete(self, *keys: str) -> int:
        self.delete_batches.append(tuple(keys))
        deleted = 0
        for key in keys:
            if key in self.store:
                deleted += 1
                self.store.pop(key, None)
        return deleted

    async def keys(self, _pattern: str) -> list[str]:
        self.keys_called += 1
        raise AssertionError("clear operations should not call Redis KEYS")


@pytest.mark.asyncio
async def test_clear_all_uses_scan_and_clears_l1_cache() -> None:
    redis = _FakeRedis(
        {
            "cache_affinity:user-1:openai:model-a": json.dumps({"provider_id": "provider-a"}),
            "cache_affinity:user-2:openai:model-b": json.dumps({"provider_id": "provider-b"}),
            "cache_affinity:user-3:claude:model-c": json.dumps({"provider_id": "provider-c"}),
            "other:key": json.dumps({"provider_id": "provider-x"}),
        }
    )
    manager = CacheAffinityManager(redis_client=redis)

    await manager._set_l1_entry(
        "cache_affinity:user-1:openai:model-a", {"provider_id": "provider-a"}
    )
    await manager._set_l1_entry("other:key", {"provider_id": "provider-x"})

    deleted_count = await manager.clear_all()

    assert deleted_count == 3
    assert redis.keys_called == 0
    assert redis.scan_calls >= 1
    assert redis.unlink_batches == [
        (
            "cache_affinity:user-1:openai:model-a",
            "cache_affinity:user-2:openai:model-b",
            "cache_affinity:user-3:claude:model-c",
        )
    ]
    assert "other:key" in redis.store
    assert all(not key.startswith("cache_affinity:") for key in redis.store)
    assert await manager._get_l1_entry("cache_affinity:user-1:openai:model-a") is None
    assert await manager._get_l1_entry("other:key") == {"provider_id": "provider-x"}


@pytest.mark.asyncio
async def test_invalidate_all_for_provider_uses_scan_not_keys() -> None:
    redis = _FakeRedis(
        {
            "cache_affinity:user-1:openai:model-a": json.dumps({"provider_id": "provider-a"}),
            "cache_affinity:user-2:openai:model-b": json.dumps({"provider_id": "provider-b"}),
            "cache_affinity:user-3:claude:model-c": json.dumps({"provider_id": "provider-a"}),
        }
    )
    manager = CacheAffinityManager(redis_client=redis)

    await manager._set_l1_entry(
        "cache_affinity:user-1:openai:model-a", {"provider_id": "provider-a"}
    )
    await manager._set_l1_entry(
        "cache_affinity:user-2:openai:model-b", {"provider_id": "provider-b"}
    )
    await manager._set_l1_entry(
        "cache_affinity:user-3:claude:model-c", {"provider_id": "provider-a"}
    )

    deleted_count = await manager.invalidate_all_for_provider("provider-a")

    assert deleted_count == 2
    assert redis.keys_called == 0
    assert redis.scan_calls >= 1
    # Pipeline path: batch MGET + batch UNLINK (not per-key DELETE).
    assert len(redis.unlink_batches) == 1
    assert set(redis.unlink_batches[0]) == {
        "cache_affinity:user-1:openai:model-a",
        "cache_affinity:user-3:claude:model-c",
    }
    assert redis.delete_batches == []
    assert "cache_affinity:user-2:openai:model-b" in redis.store
    assert "cache_affinity:user-1:openai:model-a" not in redis.store
    assert "cache_affinity:user-3:claude:model-c" not in redis.store
    assert await manager._get_l1_entry("cache_affinity:user-1:openai:model-a") is None
    assert await manager._get_l1_entry("cache_affinity:user-2:openai:model-b") == {
        "provider_id": "provider-b"
    }
