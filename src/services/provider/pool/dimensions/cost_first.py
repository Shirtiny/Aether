"""cost_first preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_usage_ratio, rank_ascending, safe_float
from .registry import PresetDimensionBase, register_preset_dimension


class CostFirstDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "cost_first"

    @property
    def label(self) -> str:
        return "成本优先"

    @property
    def description(self) -> str:
        return "优先选择窗口消耗更低的账号"

    @property
    def evidence_hint(self) -> str | None:
        return "依据窗口成本/Token 用量，缺失时回退配额使用率"

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
        cost_totals = context.get("cost_totals")
        if not isinstance(cost_totals, dict):
            cost_totals = {}
        cost_limit = safe_float(context.get("cost_limit_per_key_tokens"))

        cost_scores: dict[str, float] = {}
        for kid in all_key_ids:
            used = safe_float(cost_totals.get(kid))
            if used is not None and used >= 0:
                if cost_limit is not None and cost_limit > 0:
                    cost_scores[kid] = max(0.0, min(used / cost_limit, 1.0))
                else:
                    cost_scores[kid] = min(1.0, used / (used + 10000.0))
                continue

            usage_ratio = extract_usage_ratio(keys_by_id.get(kid))
            if usage_ratio is not None:
                cost_scores[kid] = usage_ratio

        if not cost_scores:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, cost_scores, all_key_ids)


register_preset_dimension(CostFirstDimension())
