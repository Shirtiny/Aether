"""single_account preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_internal_priority, rank_ascending, rank_descending
from .registry import PresetDimensionBase, register_preset_dimension


class SingleAccountDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "single_account"

    @property
    def label(self) -> str:
        return "单号优先"

    @property
    def description(self) -> str:
        return "集中使用同一账号（反向 LRU）"

    @property
    def mutex_group(self) -> str | None:
        return "distribution_mode"

    @property
    def evidence_hint(self) -> str | None:
        return "先按账号优先级（internal_priority），同级再按反向 LRU 集中"

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
        priority_rank = rank_ascending(key_id, priority_scores, all_key_ids)
        lru_concentrate_rank = rank_descending(key_id, lru_scores, all_key_ids)
        # 强化“单号优先”的可控性：优先级优先，反向 LRU 作为次级聚合。
        return max(0.0, min(priority_rank * 0.75 + lru_concentrate_rank * 0.25, 1.0))


register_preset_dimension(SingleAccountDimension())
