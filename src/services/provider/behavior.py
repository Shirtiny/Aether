"""Provider behavior 薄封装。

对外保持既有调用接口，内部统一委托给 core.api_format.capabilities 中的 provider registry。
"""

from __future__ import annotations

from dataclasses import dataclass

from src.core.api_format.capabilities import (
    get_provider_behavior_variants,
    register_provider_behavior_variant,
)
from src.core.provider_types import normalize_provider_type
from src.services.provider.envelope import ProviderEnvelope, get_provider_envelope


@dataclass(frozen=True, slots=True)
class ProviderBehavior:
    provider_type: str
    envelope: ProviderEnvelope | None
    same_format_variant: str | None
    cross_format_variant: str | None


def register_behavior_variant(
    provider_type: str,
    *,
    same_format: bool = False,
    cross_format: bool = False,
) -> None:
    """兼容入口：注册 provider 的格式变体标志，真实存储位于 core registry。"""
    register_provider_behavior_variant(
        provider_type,
        same_format=same_format,
        cross_format=cross_format,
    )


def get_provider_behavior(
    *,
    provider_type: str | None,
    endpoint_sig: str | None,
) -> ProviderBehavior:
    pt = normalize_provider_type(provider_type)
    envelope = get_provider_envelope(provider_type=pt, endpoint_sig=endpoint_sig)
    same_format_variant, cross_format_variant = get_provider_behavior_variants(
        provider_type=pt,
        endpoint_sig=endpoint_sig or "",
    )

    return ProviderBehavior(
        provider_type=pt,
        envelope=envelope,
        same_format_variant=same_format_variant,
        cross_format_variant=cross_format_variant,
    )


__all__ = ["ProviderBehavior", "get_provider_behavior", "register_behavior_variant"]
