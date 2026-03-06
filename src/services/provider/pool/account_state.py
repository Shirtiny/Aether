"""Pool account state helpers.

Provides a shared way to classify account-level hard-block states
from upstream metadata and OAuth invalid reasons.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from src.services.provider_keys.quota_reader import get_quota_reader

OAUTH_ACCOUNT_BLOCK_PREFIX = "[ACCOUNT_BLOCK] "
OAUTH_REFRESH_FAILED_PREFIX = "[REFRESH_FAILED] "
OAUTH_EXPIRED_PREFIX = "[OAUTH_EXPIRED] "

# -- 按原因细分的关键词组 --
# 封禁类 (suspended / banned)
_KEYWORDS_SUSPENDED: tuple[str, ...] = (
    "suspended",
    "account_block",
    "account blocked",
    "封禁",
    "封号",
    "被封",
    "账户已封禁",
    "账号异常",
)

# 停用类 (disabled / deactivated)
_KEYWORDS_DISABLED: tuple[str, ...] = (
    "account has been disabled",
    "account disabled",
    "account has been deactivated",
    "account_deactivated",
    "account deactivated",
    "organization has been disabled",
    "organization_disabled",
    "deactivated_workspace",
    "deactivated",
    "访问被禁止",
    "账户访问被禁止",
)

_TOKEN_INVALID_KEYWORDS: tuple[str, ...] = (
    "authentication token has been invalidated",
    "token has been invalidated",
    "codex token 无效或已过期",
)

# 需要验证类
_KEYWORDS_VERIFICATION: tuple[str, ...] = (
    "validation_required",
    "verify your account",
)

# 合并的完整列表（用于 is_account_level_block_reason 快速判断）
ACCOUNT_BLOCK_REASON_KEYWORDS: tuple[str, ...] = (
    *_KEYWORDS_SUSPENDED,
    *_KEYWORDS_DISABLED,
    *_TOKEN_INVALID_KEYWORDS,
    *_KEYWORDS_VERIFICATION,
)


def _classify_block_reason(text: str) -> tuple[str, str]:
    """Return (code, label) based on the oauth_invalid_reason text."""
    lowered = text.lower()
    if any(kw in lowered for kw in _TOKEN_INVALID_KEYWORDS):
        return "oauth_expired", "Token 失效"
    if any(kw in lowered for kw in _KEYWORDS_VERIFICATION):
        return "account_verification", "需要验证"
    if "deactivated_workspace" in lowered:
        return "workspace_deactivated", "工作区停用"
    if any(kw in lowered for kw in _KEYWORDS_DISABLED):
        return "account_disabled", "账号停用"
    if any(kw in lowered for kw in _KEYWORDS_SUSPENDED):
        return "account_suspended", "账号封禁"
    return "account_blocked", "账号异常"


@dataclass(frozen=True, slots=True)
class PoolAccountState:
    """Resolved account-level state for one key."""

    blocked: bool
    code: str | None = None  # account_banned / account_forbidden / account_blocked
    label: str | None = None
    reason: str | None = None


def _is_truthy_flag(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return value != 0
    if isinstance(value, str):
        normalized = value.strip().lower()
        return normalized in {"1", "true", "yes", "y"}
    return False


def _clean_text(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    text = value.strip()
    return text or None


def _extract_reason(source: dict[str, Any] | None, *fields: str) -> str | None:
    if not isinstance(source, dict):
        return None
    for field in fields:
        text = _clean_text(source.get(field))
        if text:
            return text
    return None


def _resolve_from_metadata(
    provider_type: str | None,
    upstream_metadata: Any,
) -> PoolAccountState | None:
    if not isinstance(upstream_metadata, dict):
        return None

    normalized_provider = str(provider_type or "").strip().lower()
    provider_bucket: dict[str, Any] | None = None
    if normalized_provider:
        maybe_bucket = upstream_metadata.get(normalized_provider)
        if isinstance(maybe_bucket, dict):
            provider_bucket = maybe_bucket

    quota_block = get_quota_reader(normalized_provider, upstream_metadata).account_block()
    if quota_block.blocked:
        return PoolAccountState(
            blocked=True,
            code=quota_block.code,
            label=quota_block.label,
            reason=quota_block.reason,
        )

    for source in (provider_bucket, upstream_metadata):
        if not isinstance(source, dict):
            continue
        if _is_truthy_flag(source.get("is_banned")):
            reason = _extract_reason(source, "ban_reason", "forbidden_reason", "reason", "message")
            return PoolAccountState(
                blocked=True,
                code="account_banned",
                label="账号封禁",
                reason=reason or "账号已封禁",
            )
        if _is_truthy_flag(source.get("is_forbidden")) or _is_truthy_flag(
            source.get("account_disabled")
        ):
            reason = _extract_reason(source, "forbidden_reason", "ban_reason", "reason", "message")
            return PoolAccountState(
                blocked=True,
                code="account_forbidden",
                label="访问受限",
                reason=reason or "账号访问受限",
            )

    return None


def _resolve_from_oauth_invalid_reason(reason: str | None) -> PoolAccountState | None:
    text = _clean_text(reason)
    if not text:
        return None

    if text.startswith(OAUTH_ACCOUNT_BLOCK_PREFIX):
        cleaned = text[len(OAUTH_ACCOUNT_BLOCK_PREFIX) :].strip()
        code, label = (
            _classify_block_reason(cleaned) if cleaned else ("account_blocked", "账号异常")
        )
        return PoolAccountState(
            blocked=True,
            code=code,
            label=label,
            reason=cleaned or "账号异常",
        )

    if text.startswith(OAUTH_EXPIRED_PREFIX):
        cleaned = text[len(OAUTH_EXPIRED_PREFIX) :].strip()
        return PoolAccountState(
            blocked=True,
            code="oauth_expired",
            label="Token 失效",
            reason=cleaned or "OAuth Token 已过期且无法续期",
        )

    lowered = text.lower()
    if any(keyword in lowered for keyword in ACCOUNT_BLOCK_REASON_KEYWORDS):
        code, label = _classify_block_reason(text)
        return PoolAccountState(
            blocked=True,
            code=code,
            label=label,
            reason=text,
        )

    return None


def resolve_pool_account_state(
    *,
    provider_type: str | None,
    upstream_metadata: Any,
    oauth_invalid_reason: str | None,
) -> PoolAccountState:
    """Resolve account-level hard-block state for pool scheduling."""

    from_metadata = _resolve_from_metadata(provider_type, upstream_metadata)
    if from_metadata is not None:
        return from_metadata

    from_oauth = _resolve_from_oauth_invalid_reason(oauth_invalid_reason)
    if from_oauth is not None:
        return from_oauth

    return PoolAccountState(blocked=False)


__all__ = [
    "ACCOUNT_BLOCK_REASON_KEYWORDS",
    "OAUTH_ACCOUNT_BLOCK_PREFIX",
    "OAUTH_EXPIRED_PREFIX",
    "OAUTH_REFRESH_FAILED_PREFIX",
    "PoolAccountState",
    "resolve_pool_account_state",
]
