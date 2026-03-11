"""recent_refresh preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_reset_seconds, rank_ascending
from .registry import PresetDimensionBase, register_preset_dimension


class RecentRefreshDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "recent_refresh"

    @property
    def label(self) -> str:
        return "额度刷新优先"

    @property
    def description(self) -> str:
        return "优先选即将刷新额度的账号"

    @property
    def providers(self) -> tuple[str, ...]:
        return ("codex", "kiro")

    @property
    def evidence_hint(self) -> str | None:
        return "依据账号额度重置倒计时（next_reset / reset_seconds）"

    def compute_metric(
        self,
        *,
        key_id: str,
        all_key_ids: list[str],
        keys_by_id: dict[str, Any],
        lru_scores: dict[str, Any],
        context: dict[str, Any],
        mode: str | None,
    ) -> float:
        provider_type = context.get("provider_type")
        reset_scores: dict[str, float] = {}
        for kid in all_key_ids:
            reset_seconds = extract_reset_seconds(keys_by_id.get(kid), provider_type=provider_type)
            if reset_seconds is not None:
                reset_scores[kid] = reset_seconds
        if not reset_scores:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, reset_scores, all_key_ids)


register_preset_dimension(RecentRefreshDimension())
