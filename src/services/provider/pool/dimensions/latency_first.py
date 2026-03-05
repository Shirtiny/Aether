"""latency_first preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import rank_ascending, safe_float
from .registry import PresetDimensionBase, register_preset_dimension


class LatencyFirstDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "latency_first"

    @property
    def label(self) -> str:
        return "延迟优先"

    @property
    def description(self) -> str:
        return "优先选择最近延迟更低的账号"

    @property
    def evidence_hint(self) -> str | None:
        return "依据号池延迟窗口均值（latency_window_seconds）"

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
        latency_avgs = context.get("latency_avgs")
        if not isinstance(latency_avgs, dict):
            latency_avgs = {}

        latency_scores: dict[str, float] = {}
        for kid in all_key_ids:
            latency = safe_float(latency_avgs.get(kid))
            if latency is None or latency < 0:
                continue
            latency_scores[kid] = latency

        if not latency_scores:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, latency_scores, all_key_ids)


register_preset_dimension(LatencyFirstDimension())
