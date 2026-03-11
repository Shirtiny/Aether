from __future__ import annotations

from types import SimpleNamespace
from typing import Any, cast

import pytest

from src.services.provider_keys import codex_realtime_quota as realtime_module
from src.services.provider_keys.codex_realtime_quota import (
    sync_codex_quota_from_response_headers,
)
from src.services.provider_keys.codex_usage_parser import (
    CodexUsageParseError,
    parse_codex_usage_headers,
)


class _FakeQuery:
    def __init__(self, db: "_FakeDB") -> None:
        self._db = db

    def options(self, *args: Any, **kwargs: Any) -> "_FakeQuery":
        _ = args, kwargs
        return self

    def filter(self, *args: Any, **kwargs: Any) -> "_FakeQuery":
        _ = args, kwargs
        return self

    def first(self) -> Any:
        return self._db.key


class _FakeDB:
    def __init__(self, key: Any) -> None:
        self.key = key
        self.added: list[Any] = []
        self.query_count = 0

    def query(self, _model: Any) -> _FakeQuery:
        self.query_count += 1
        return _FakeQuery(self)

    def add(self, obj: Any) -> None:
        self.added.append(obj)


def _paid_headers(**overrides: Any) -> dict[str, Any]:
    base = {
        "x-codex-plan-type": "team",
        "x-codex-primary-used-percent": "3",
        "x-codex-secondary-used-percent": "64",
        "x-codex-primary-window-minutes": "300",
        "x-codex-secondary-window-minutes": "10080",
        "x-codex-primary-reset-after-seconds": "411",
        "x-codex-secondary-reset-after-seconds": "267545",
        "x-codex-primary-reset-at": "1772259405",
        "x-codex-secondary-reset-at": "1772526539",
        "x-codex-credits-has-credits": "False",
        "x-codex-credits-balance": "",
        "x-codex-credits-unlimited": "False",
    }
    base.update(overrides)
    return base


def _as_session(db: _FakeDB) -> Any:
    # 测试中使用最小假对象模拟 Session 接口
    return cast(Any, db)


def test_parse_codex_usage_headers_paid_windows_mapping() -> None:
    parsed = parse_codex_usage_headers(_paid_headers())
    assert parsed is not None
    assert parsed["plan_type"] == "team"
    # 与 wham 解析保持一致：primary=周限额，secondary=5H 限额
    assert parsed["primary_used_percent"] == 64.0
    assert parsed["secondary_used_percent"] == 3.0
    assert parsed["primary_window_minutes"] == 10080
    assert parsed["secondary_window_minutes"] == 300
    assert parsed["has_credits"] is False
    assert parsed["credits_unlimited"] is False
    assert "credits_balance" not in parsed


@pytest.mark.parametrize("plan_type", ["plus", "enterprise"])
def test_parse_codex_usage_headers_paid_plan_windows_mapping(plan_type: str) -> None:
    parsed = parse_codex_usage_headers(_paid_headers(**{"x-codex-plan-type": plan_type}))
    assert parsed is not None
    assert parsed["plan_type"] == plan_type
    assert parsed["primary_used_percent"] == 64.0
    assert parsed["secondary_used_percent"] == 3.0
    assert parsed["primary_window_minutes"] == 10080
    assert parsed["secondary_window_minutes"] == 300


def test_parse_codex_usage_headers_free_uses_primary_only() -> None:
    parsed = parse_codex_usage_headers(
        {
            "x-codex-plan-type": "FREE",
            "x-codex-primary-used-percent": "12.5",
            "x-codex-primary-window-minutes": "10080",
            "x-codex-primary-reset-after-seconds": "120",
            "x-codex-primary-reset-at": "1700000000",
        }
    )
    assert parsed is not None
    assert parsed["plan_type"] == "free"
    assert parsed["primary_used_percent"] == 12.5
    assert parsed["primary_window_minutes"] == 10080
    assert "secondary_used_percent" not in parsed


def test_parse_codex_usage_headers_invalid_type_raises() -> None:
    with pytest.raises(CodexUsageParseError, match="headers.primary_window.limit_window_minutes"):
        parse_codex_usage_headers({"x-codex-primary-window-minutes": "abc"})


def test_sync_codex_quota_from_headers_updates_and_preserves_existing_fields() -> None:
    realtime_module._header_fingerprint_cache.clear()

    key = SimpleNamespace(
        id="sync-update-key",
        provider=SimpleNamespace(provider_type="codex"),
        upstream_metadata={
            "codex": {
                "primary_used_percent": 50.0,
                "legacy_marker": "keep-me",
            }
        },
    )
    db = _FakeDB(key)

    updated = sync_codex_quota_from_response_headers(
        db=_as_session(db),
        provider_api_key_id="sync-update-key",
        response_headers=_paid_headers(),
    )

    assert updated is True
    assert db.added == [key]
    codex_meta = key.upstream_metadata["codex"]
    assert codex_meta["primary_used_percent"] == 64.0
    assert codex_meta["secondary_used_percent"] == 3.0
    # 旧字段应被保留（解析器只覆盖已知配额字段）
    assert codex_meta["legacy_marker"] == "keep-me"


def test_sync_codex_quota_from_headers_skips_when_only_reset_seconds_changed() -> None:
    realtime_module._header_fingerprint_cache.clear()

    key = SimpleNamespace(
        id="sync-volatile-key",
        provider=SimpleNamespace(provider_type="codex"),
        upstream_metadata={
            "codex": {
                "plan_type": "team",
                "primary_used_percent": 64.0,
                "secondary_used_percent": 3.0,
                "primary_window_minutes": 10080,
                "secondary_window_minutes": 300,
                "primary_reset_at": 1772526539,
                "secondary_reset_at": 1772259405,
                "primary_reset_seconds": 999999,
                "secondary_reset_seconds": 999999,
                "has_credits": False,
                "credits_unlimited": False,
            }
        },
    )
    db = _FakeDB(key)

    updated = sync_codex_quota_from_response_headers(
        db=_as_session(db),
        provider_api_key_id="sync-volatile-key",
        response_headers=_paid_headers(),
    )

    assert updated is False
    assert db.added == []


def test_sync_codex_quota_from_headers_cache_hit_skips_second_query() -> None:
    realtime_module._header_fingerprint_cache.clear()

    key = SimpleNamespace(
        id="sync-cache-key",
        provider=SimpleNamespace(provider_type="codex"),
        upstream_metadata={},
    )
    db = _FakeDB(key)
    headers = _paid_headers()

    first_updated = sync_codex_quota_from_response_headers(
        db=_as_session(db),
        provider_api_key_id="sync-cache-key",
        response_headers=headers,
    )
    second_updated = sync_codex_quota_from_response_headers(
        db=_as_session(db),
        provider_api_key_id="sync-cache-key",
        response_headers=headers,
    )

    assert first_updated is True
    assert second_updated is False
    assert db.query_count == 1


def test_sync_codex_quota_from_headers_non_codex_key_is_ignored() -> None:
    realtime_module._header_fingerprint_cache.clear()

    key = SimpleNamespace(
        id="sync-kiro-key",
        provider=SimpleNamespace(provider_type="kiro"),
        upstream_metadata={},
    )
    db = _FakeDB(key)

    updated = sync_codex_quota_from_response_headers(
        db=_as_session(db),
        provider_api_key_id="sync-kiro-key",
        response_headers=_paid_headers(),
    )

    assert updated is False
    assert db.added == []


def test_sync_codex_quota_from_headers_parse_error_is_non_blocking() -> None:
    realtime_module._header_fingerprint_cache.clear()

    key = SimpleNamespace(
        id="sync-parse-error-key",
        provider=SimpleNamespace(provider_type="codex"),
        upstream_metadata={},
    )
    db = _FakeDB(key)

    updated = sync_codex_quota_from_response_headers(
        db=_as_session(db),
        provider_api_key_id="sync-parse-error-key",
        response_headers={
            "x-codex-primary-window-minutes": "abc",
        },
    )

    assert updated is False
    assert db.query_count == 0
    assert db.added == []


def test_header_fingerprint_cache_prunes_expired_entries() -> None:
    realtime_module._header_fingerprint_cache.clear()

    realtime_module._set_cached_fingerprint("expired-key", "fp-1", now_ts=10.0)
    realtime_module._set_cached_fingerprint("fresh-key", "fp-2", now_ts=10.0 + 31.0)

    assert "expired-key" not in realtime_module._header_fingerprint_cache
    assert "fresh-key" in realtime_module._header_fingerprint_cache


def test_header_fingerprint_cache_respects_max_entries() -> None:
    realtime_module._header_fingerprint_cache.clear()

    max_entries = realtime_module._CACHE_MAX_ENTRIES
    for i in range(max_entries + 8):
        realtime_module._set_cached_fingerprint(f"key-{i}", f"fp-{i}", now_ts=1000.0)

    assert len(realtime_module._header_fingerprint_cache) == max_entries
