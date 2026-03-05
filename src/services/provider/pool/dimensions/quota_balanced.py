"""quota_balanced preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_usage_ratio, rank_ascending, safe_float
from .registry import PresetDimensionBase, register_preset_dimension


class QuotaBalancedDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "quota_balanced"

    @property
    def label(self) -> str:
        return "额度平均"

    @property
    def description(self) -> str:
        return "优先选额度消耗最少的账号"

    @property
    def evidence_hint(self) -> str | None:
        return "依据账号配额使用率；无配额时回退到窗口成本使用"

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
        usage_scores: dict[str, float] = {}
        cost_totals = context.get("cost_totals")
        if not isinstance(cost_totals, dict):
            cost_totals = {}
        cost_limit = safe_float(context.get("cost_limit_per_key_tokens"))
        for kid in all_key_ids:
            key_obj = keys_by_id.get(kid)
            usage_ratio = extract_usage_ratio(key_obj)
            if usage_ratio is None:
                used = safe_float(cost_totals.get(kid))
                if used is not None and used >= 0:
                    if cost_limit is not None and cost_limit > 0:
                        usage_ratio = max(0.0, min(used / cost_limit, 1.0))
                    else:
                        # 无明确上限时用 log 归一化，确保维度仍有区分能力。
                        usage_ratio = min(1.0, used / (used + 10000.0))
            if usage_ratio is not None:
                usage_scores[kid] = usage_ratio
        if not usage_scores:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, usage_scores, all_key_ids)


register_preset_dimension(QuotaBalancedDimension())
