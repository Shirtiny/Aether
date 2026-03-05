from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

from src.services.provider_keys.pool_quota_probe_scheduler import _select_probe_key_ids


def _key(
    key_id: str,
    *,
    last_used_at: datetime | None = None,
    upstream_metadata: dict | None = None,
) -> SimpleNamespace:
    return SimpleNamespace(
        id=key_id,
        last_used_at=last_used_at,
        upstream_metadata=upstream_metadata or {},
    )


def test_select_probe_key_ids_selects_silent_keys_only() -> None:
    now = datetime(2026, 3, 5, 12, 0, 0, tzinfo=timezone.utc)
    now_ts = int(now.timestamp())

    keys = [
        _key("k1"),  # never used, should be probed
        _key("k2", last_used_at=now - timedelta(minutes=2)),  # recently used, skip
        _key(
            "k3",
            upstream_metadata={"codex": {"updated_at": now_ts - (20 * 60)}},
        ),  # long-time no refresh, should be probed
    ]

    selected = _select_probe_key_ids(
        keys=keys,  # type: ignore[arg-type]
        provider_type="codex",
        now_ts=now_ts,
        interval_seconds=10 * 60,
        last_probe_timestamps={},
        limit=0,
    )
    assert selected == ["k1", "k3"]


def test_select_probe_key_ids_resets_probe_window_after_key_usage() -> None:
    now = datetime(2026, 3, 5, 12, 0, 0, tzinfo=timezone.utc)
    now_ts = int(now.timestamp())

    keys = [
        _key(
            "k1",
            last_used_at=now - timedelta(seconds=30),
            upstream_metadata={"codex": {"updated_at": now_ts - (40 * 60)}},
        )
    ]

    # 上一次主动探测非常早，但 key 刚刚被真实流量使用，应跳过本次探测
    selected = _select_probe_key_ids(
        keys=keys,  # type: ignore[arg-type]
        provider_type="codex",
        now_ts=now_ts,
        interval_seconds=10 * 60,
        last_probe_timestamps={"k1": now_ts - (25 * 60)},
        limit=0,
    )
    assert selected == []


def test_select_probe_key_ids_applies_limit_by_oldest_anchor_first() -> None:
    now = datetime(2026, 3, 5, 12, 0, 0, tzinfo=timezone.utc)
    now_ts = int(now.timestamp())

    keys = [
        _key("k1", last_used_at=now - timedelta(minutes=60)),
        _key("k2", last_used_at=now - timedelta(minutes=50)),
        _key("k3", last_used_at=now - timedelta(minutes=40)),
    ]

    selected = _select_probe_key_ids(
        keys=keys,  # type: ignore[arg-type]
        provider_type="codex",
        now_ts=now_ts,
        interval_seconds=10 * 60,
        last_probe_timestamps={},
        limit=2,
    )
    assert selected == ["k1", "k2"]
