"""priority_first preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_internal_priority, rank_ascending
from .registry import PresetDimensionBase, register_preset_dimension


class PriorityFirstDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "priority_first"

    @property
    def label(self) -> str:
        return "优先级优先"

    @property
    def description(self) -> str:
        return "按账号优先级顺序调度（数字越小越优先）"

    @property
    def evidence_hint(self) -> str | None:
        return "依据 internal_priority（支持拖拽/手工编辑）"

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
        priority_scores = {
            kid: float(extract_internal_priority(keys_by_id.get(kid))) for kid in all_key_ids
        }
        if len(set(priority_scores.values())) <= 1:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, priority_scores, all_key_ids)


register_preset_dimension(PriorityFirstDimension())
