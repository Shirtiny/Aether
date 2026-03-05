"""Tests for pool_health_policy.py — error code classification."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, patch

import pytest

from src.services.provider.pool.config import (
    PoolConfig,
    UnschedulableRule,
)
from src.services.provider.pool.health_policy import (
    _extract_error_message,
    _parse_retry_after,
    apply_health_policy,
)

PID = "provider-test"
KID = "key-test"


@pytest.fixture()
def config() -> PoolConfig:
    return PoolConfig()


# ---------------------------------------------------------------------------
# Helper tests
# ---------------------------------------------------------------------------


def test_parse_retry_after_none() -> None:
    assert _parse_retry_after(None) is None
    assert _parse_retry_after({}) is None


def test_parse_retry_after_valid() -> None:
    assert _parse_retry_after({"retry-after": "60"}) == 60
    assert _parse_retry_after({"Retry-After": "120"}) == 120


def test_parse_retry_after_clamped() -> None:
    assert _parse_retry_after({"retry-after": "0"}) == 1
    assert _parse_retry_after({"retry-after": "9999"}) == 3600


def test_parse_retry_after_invalid() -> None:
    assert _parse_retry_after({"retry-after": "not-a-number"}) is None


def test_extract_error_message_json_error_object() -> None:
    body = json.dumps({"error": {"message": "bad request"}})
    assert _extract_error_message(body) == "bad request"


def test_extract_error_message_json_error_string() -> None:
    body = json.dumps({"error": "something went wrong"})
    assert _extract_error_message(body) == "something went wrong"


def test_extract_error_message_plain_text() -> None:
    assert _extract_error_message("plain text error") == "plain text error"


def test_extract_error_message_empty() -> None:
    assert _extract_error_message(None) == ""
    assert _extract_error_message("") == ""


# ---------------------------------------------------------------------------
# Status code handling
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_401_invalidates_oauth_cache_and_sets_cooldown(config: PoolConfig) -> None:
    with (
        patch(
            "src.services.provider.pool.redis_ops.invalidate_oauth_token_cache",
            new_callable=AsyncMock,
        ) as mock_inv,
        patch(
            "src.services.provider.pool.redis_ops.set_cooldown",
            new_callable=AsyncMock,
        ) as mock_cd,
    ):
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=401,
            error_body=None,
            response_headers=None,
            config=config,
        )

    mock_inv.assert_called_once_with(KID)
    mock_cd.assert_called_once_with(PID, KID, "auth_failed_401", ttl=60)


@pytest.mark.asyncio
async def test_402_sets_long_cooldown(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=402,
            error_body=None,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "payment_required_402", ttl=3600)


@pytest.mark.asyncio
async def test_403_default_sets_medium_cooldown(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=403,
            error_body=None,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "forbidden_403", ttl=300)


@pytest.mark.asyncio
async def test_403_suspended_body_sets_long_cooldown(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=403,
            error_body=json.dumps({"error": {"message": "account suspended"}}),
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "forbidden_403", ttl=3600)


@pytest.mark.asyncio
async def test_400_with_org_disabled_pattern(config: PoolConfig) -> None:
    body = json.dumps({"error": {"message": "Your organization has been disabled"}})
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=400,
            error_body=body,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once()
    assert "account_disabled_400" in mock_cd.call_args.args[2]


@pytest.mark.asyncio
async def test_400_without_pattern_does_nothing(config: PoolConfig) -> None:
    body = json.dumps({"error": {"message": "invalid json field"}})
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=400,
            error_body=body,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_not_called()


@pytest.mark.asyncio
async def test_429_uses_retry_after_header(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=429,
            error_body=None,
            response_headers={"retry-after": "120"},
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "rate_limited_429", ttl=120)


@pytest.mark.asyncio
async def test_429_falls_back_to_config_default(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=429,
            error_body=None,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "rate_limited_429", ttl=300)


@pytest.mark.asyncio
async def test_529_uses_overload_cooldown(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=529,
            error_body=None,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "overloaded_529", ttl=30)


@pytest.mark.asyncio
async def test_503_uses_retry_after_when_present(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=503,
            error_body=None,
            response_headers={"retry-after": "45"},
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "service_unavailable_503", ttl=45)


@pytest.mark.asyncio
async def test_500_uses_overload_cooldown(config: PoolConfig) -> None:
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=500,
            error_body=None,
            response_headers=None,
            config=config,
        )

    mock_cd.assert_called_once_with(PID, KID, "server_error_500", ttl=30)


# ---------------------------------------------------------------------------
# Unschedulable keyword rules
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_keyword_rule_matches_and_sets_cooldown() -> None:
    cfg = PoolConfig(
        unschedulable_rules=[UnschedulableRule(keyword="capacity", duration_minutes=10)]
    )
    body = json.dumps({"error": {"message": "Server at capacity, try later"}})
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=500,
            error_body=body,
            response_headers=None,
            config=cfg,
        )

    mock_cd.assert_called_once_with(PID, KID, "rule:capacity", ttl=600)


# ---------------------------------------------------------------------------
# health_policy_enabled = False
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_disabled_health_policy_does_nothing() -> None:
    cfg = PoolConfig(health_policy_enabled=False)
    with patch(
        "src.services.provider.pool.redis_ops.set_cooldown",
        new_callable=AsyncMock,
    ) as mock_cd:
        await apply_health_policy(
            provider_id=PID,
            key_id=KID,
            status_code=429,
            error_body=None,
            response_headers=None,
            config=cfg,
        )

    mock_cd.assert_not_called()
