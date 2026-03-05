"""Tests for pool account-state resolution helpers."""

from __future__ import annotations

from src.services.provider.pool.account_state import resolve_pool_account_state


def test_resolve_from_kiro_banned_metadata() -> None:
    state = resolve_pool_account_state(
        provider_type="kiro",
        upstream_metadata={"kiro": {"is_banned": True, "ban_reason": "account suspended"}},
        oauth_invalid_reason=None,
    )
    assert state.blocked is True
    assert state.code == "account_banned"
    assert state.label == "账号封禁"
    assert state.reason == "account suspended"


def test_resolve_from_antigravity_forbidden_metadata() -> None:
    state = resolve_pool_account_state(
        provider_type="antigravity",
        upstream_metadata={"antigravity": {"is_forbidden": True, "forbidden_reason": "403"}},
        oauth_invalid_reason=None,
    )
    assert state.blocked is True
    assert state.code == "account_forbidden"
    assert state.label == "访问受限"
    assert state.reason == "403"


def test_resolve_from_structured_oauth_reason_verification() -> None:
    state = resolve_pool_account_state(
        provider_type="codex",
        upstream_metadata=None,
        oauth_invalid_reason="[ACCOUNT_BLOCK] Google requires verification",
    )
    assert state.blocked is True
    assert state.code == "account_blocked"
    assert state.label == "账号异常"
    assert state.reason == "Google requires verification"


def test_resolve_from_structured_oauth_reason_suspended() -> None:
    state = resolve_pool_account_state(
        provider_type="codex",
        upstream_metadata=None,
        oauth_invalid_reason="[ACCOUNT_BLOCK] account suspended by admin",
    )
    assert state.blocked is True
    assert state.code == "account_suspended"
    assert state.label == "账号封禁"
    assert state.reason == "account suspended by admin"


def test_resolve_from_keyword_oauth_reason_disabled() -> None:
    state = resolve_pool_account_state(
        provider_type=None,
        upstream_metadata={},
        oauth_invalid_reason="organization has been disabled by admin",
    )
    assert state.blocked is True
    assert state.code == "account_disabled"
    assert state.label == "账号停用"


def test_resolve_from_keyword_oauth_reason_verification() -> None:
    state = resolve_pool_account_state(
        provider_type=None,
        upstream_metadata={},
        oauth_invalid_reason="validation_required: please verify your identity",
    )
    assert state.blocked is True
    assert state.code == "account_verification"
    assert state.label == "需要验证"


def test_resolve_healthy_state() -> None:
    state = resolve_pool_account_state(
        provider_type="codex",
        upstream_metadata={"codex": {"primary_used_percent": 30}},
        oauth_invalid_reason="Token expired",
    )
    assert state.blocked is False
    assert state.code is None


def test_bare_forbidden_not_treated_as_account_block() -> None:
    """HTTP 403 'Forbidden' from token refresh should not be misclassified."""
    state = resolve_pool_account_state(
        provider_type="codex",
        upstream_metadata={},
        oauth_invalid_reason="Forbidden",
    )
    assert state.blocked is False


def test_kiro_oauth_reason_text_detected_as_suspended() -> None:
    state = resolve_pool_account_state(
        provider_type="kiro",
        upstream_metadata={},
        oauth_invalid_reason="账户已封禁: Terms of Service violation",
    )
    assert state.blocked is True
    assert state.code == "account_suspended"
    assert state.label == "账号封禁"


def test_antigravity_oauth_reason_text_detected_as_disabled() -> None:
    state = resolve_pool_account_state(
        provider_type="antigravity",
        upstream_metadata={},
        oauth_invalid_reason="账户访问被禁止: 403 Forbidden",
    )
    assert state.blocked is True
    assert state.code == "account_disabled"
    assert state.label == "账号停用"
