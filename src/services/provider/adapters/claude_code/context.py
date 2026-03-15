"""Claude Code request context using contextvars."""

from __future__ import annotations

import contextvars
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from src.core.logger import logger
from src.models.admin_requests import ClaudeCodeAdvancedConfig

if TYPE_CHECKING:
    from src.services.provider.pool.config import PoolConfig


@dataclass(frozen=True, slots=True)
class ClaudeCodeRequestContext:
    is_stream: bool = False
    # 使用 Key 级别作用域，确保会话限制按 OAuth 账号隔离。
    scope_key: str | None = None
    key_id: str | None = None
    max_sessions: int | None = None
    session_idle_timeout_minutes: int = 5
    session_id_masking_enabled: bool = False
    cache_ttl_override_enabled: bool = False
    cache_ttl_override_target: str = "ephemeral"
    cli_only_enabled: bool = False
    # Account Pool fields
    provider_id: str | None = None
    pool_config: PoolConfig | None = None
    session_uuid: str | None = None


_claude_code_request_context: contextvars.ContextVar[ClaudeCodeRequestContext | None] = (
    contextvars.ContextVar(
        "claude_code_request_context",
        default=None,
    )
)


def set_claude_code_request_context(ctx: ClaudeCodeRequestContext | None) -> None:
    _claude_code_request_context.set(ctx)


def get_claude_code_request_context() -> ClaudeCodeRequestContext | None:
    return _claude_code_request_context.get()


def build_claude_code_request_context(
    *,
    provider_config: Any,
    key_id: str | None,
    is_stream: bool,
    provider_id: str | None = None,
) -> ClaudeCodeRequestContext:
    """根据 Provider.config 构建 Claude Code 请求上下文。"""
    from src.services.provider.pool.config import parse_pool_config

    normalized_key_id = str(key_id or "").strip() or None

    advanced_config: ClaudeCodeAdvancedConfig | None = None
    provider_config_dict = provider_config if isinstance(provider_config, dict) else {}
    raw_advanced = provider_config_dict.get("claude_code_advanced")

    if raw_advanced is not None:
        try:
            if isinstance(raw_advanced, ClaudeCodeAdvancedConfig):
                advanced_config = raw_advanced
            elif isinstance(raw_advanced, dict):
                advanced_config = ClaudeCodeAdvancedConfig.model_validate(raw_advanced)
            else:
                logger.warning(
                    "Claude Code advanced config 类型无效: {}，已忽略",
                    type(raw_advanced).__name__,
                )
        except Exception as exc:
            logger.warning("Claude Code advanced config 解析失败，已忽略: {}", str(exc))

    max_sessions = advanced_config.max_sessions if advanced_config else None
    idle_timeout_minutes = (
        advanced_config.session_idle_timeout_minutes
        if advanced_config and advanced_config.session_idle_timeout_minutes is not None
        else 5
    )
    session_id_masking_enabled = (
        bool(advanced_config.session_id_masking_enabled) if advanced_config else False
    )
    cache_ttl_override_enabled = (
        bool(advanced_config.cache_ttl_override_enabled) if advanced_config else False
    )
    cache_ttl_override_target = (
        str(advanced_config.cache_ttl_override_target or "ephemeral")
        if advanced_config
        else "ephemeral"
    )
    cli_only_enabled = bool(advanced_config.cli_only_enabled) if advanced_config else False

    # Parse pool config (None = non-pool provider, keep as None for semantic consistency)
    pool_cfg = parse_pool_config(provider_config_dict)

    return ClaudeCodeRequestContext(
        is_stream=bool(is_stream),
        scope_key=f"key:{normalized_key_id}" if normalized_key_id else None,
        key_id=normalized_key_id,
        max_sessions=max_sessions,
        session_idle_timeout_minutes=idle_timeout_minutes,
        session_id_masking_enabled=session_id_masking_enabled,
        cache_ttl_override_enabled=cache_ttl_override_enabled,
        cache_ttl_override_target=cache_ttl_override_target,
        cli_only_enabled=cli_only_enabled,
        provider_id=str(provider_id or "").strip() or None,
        pool_config=pool_cfg,
    )


def build_and_set_claude_code_request_context(
    *,
    provider_config: Any,
    key_id: str | None,
    is_stream: bool,
    provider_id: str | None = None,
) -> ClaudeCodeRequestContext:
    """构建并写入 Claude Code 上下文。"""
    ctx = build_claude_code_request_context(
        provider_config=provider_config,
        key_id=key_id,
        is_stream=is_stream,
        provider_id=provider_id,
    )
    set_claude_code_request_context(ctx)
    return ctx


__all__ = [
    "build_and_set_claude_code_request_context",
    "build_claude_code_request_context",
    "ClaudeCodeRequestContext",
    "get_claude_code_request_context",
    "set_claude_code_request_context",
]
