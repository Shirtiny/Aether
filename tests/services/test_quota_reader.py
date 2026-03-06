from __future__ import annotations

from types import SimpleNamespace

import pytest

from src.services.provider.pool.account_state import resolve_pool_account_state
from src.services.provider.pool.dimensions._helpers import (
    extract_plan_type,
    extract_reset_seconds,
    extract_usage_ratio,
)
from src.services.provider_keys import quota_reader
from src.services.provider_keys.quota_reader import get_quota_reader
from src.services.scheduling.quota_skipper import is_key_quota_exhausted


def test_codex_reader_preserves_summary_formats() -> None:
    reader = get_quota_reader(
        "codex",
        {
            "codex": {
                "primary_used_percent": 14.8,
                "primary_reset_seconds": 266400,
                "secondary_used_percent": 27.9,
            }
        },
    )

    assert reader.display_summary() == "周剩余 85.2% (3天2小时后重置) | 5H剩余 72.1%"

    credits_reader = get_quota_reader(
        "codex",
        {"codex": {"has_credits": True, "credits_balance": 12.345}},
    )
    assert credits_reader.display_summary() == "积分 12.35"


def test_antigravity_reader_keeps_used_percent_fallbacks() -> None:
    reader = get_quota_reader(
        "antigravity",
        {
            "antigravity": {
                "quota_by_model": {
                    "claude-sonnet-4": {"used_percent": 100.0},
                    "gemini-2.5-pro": {"remaining_fraction": 0.6},
                }
            }
        },
    )

    exhausted = reader.is_exhausted("claude-sonnet-4")
    assert exhausted.exhausted is True
    assert exhausted.reason == "Antigravity 模型 claude-sonnet-4 配额剩余 0%"
    assert reader.display_summary() == "最低剩余 0.0% (2 模型)"
    assert reader.usage_ratio() == pytest.approx(0.7)


def test_dimension_helpers_delegate_to_unified_reader(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(quota_reader.time, "time", lambda: 100.0)
    key_obj = SimpleNamespace(
        upstream_metadata={
            "kiro": {
                "next_reset_at": 160.0,
                "usage_percentage": 45.0,
                "subscription_title": "Kiro Team",
            }
        },
        oauth_plan_type=None,
    )

    assert extract_plan_type(key_obj) == "team"
    assert extract_reset_seconds(key_obj) == pytest.approx(60.0)
    assert extract_usage_ratio(key_obj) == pytest.approx(0.45)


def test_resolve_pool_account_state_keeps_codex_metadata_block() -> None:
    state = resolve_pool_account_state(
        provider_type="codex",
        upstream_metadata={"codex": {"account_disabled": True, "message": "deactivated_workspace"}},
        oauth_invalid_reason=None,
    )

    assert state.blocked is True
    assert state.code == "account_forbidden"
    assert state.label == "访问受限"
    assert state.reason == "deactivated_workspace"


def test_quota_skipper_uses_unified_reader() -> None:
    codex_key = SimpleNamespace(
        upstream_metadata={"codex": {"primary_used_percent": 100.0, "secondary_used_percent": 20.0}}
    )
    exhausted, reason = is_key_quota_exhausted("codex", codex_key, model_name="")  # type: ignore[arg-type]
    assert exhausted is True
    assert reason == "Codex 周限额剩余 0%"

    antigravity_key = SimpleNamespace(
        upstream_metadata={
            "antigravity": {"quota_by_model": {"gemini-2.5-pro": {"remaining_fraction": 0.0}}}
        }
    )
    exhausted, reason = is_key_quota_exhausted(
        "antigravity",
        antigravity_key,  # type: ignore[arg-type]
        model_name="gemini-2.5-pro",
    )
    assert exhausted is True
    assert reason == "Antigravity 模型 gemini-2.5-pro 配额剩余 0%"
