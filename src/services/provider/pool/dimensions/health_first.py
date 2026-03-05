"""health_first preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_health_score, rank_ascending, safe_float
from .registry import PresetDimensionBase, register_preset_dimension


class HealthFirstDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "health_first"

    @property
    def label(self) -> str:
        return "健康优先"

    @property
    def description(self) -> str:
        return "优先选择健康分更高、失败更少的账号"

    @property
    def evidence_hint(self) -> str | None:
        return "依据 health_by_format 聚合分（含熔断/失败衰减）"

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
        health_scores_ctx = context.get("health_scores")
        if not isinstance(health_scores_ctx, dict):
            health_scores_ctx = {}

        penalty_scores: dict[str, float] = {}
        for kid in all_key_ids:
            score = safe_float(health_scores_ctx.get(kid))
            if score is None:
                score = extract_health_score(keys_by_id.get(kid))
            if score is None:
                continue
            normalized = max(0.0, min(score, 1.0))
            penalty_scores[kid] = 1.0 - normalized

        if not penalty_scores:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, penalty_scores, all_key_ids)


register_preset_dimension(HealthFirstDimension())
