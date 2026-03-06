"""cache_affinity preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import rank_descending
from .registry import PresetDimensionBase, register_preset_dimension


class CacheAffinityDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "cache_affinity"

    @property
    def label(self) -> str:
        return "缓存亲和"

    @property
    def description(self) -> str:
        return "优先复用最近使用过的 Key，利用 Prompt Caching"

    @property
    def mutex_group(self) -> str | None:
        return "distribution_mode"

    @property
    def evidence_hint(self) -> str | None:
        return "依据 LRU 时间戳（最近使用优先，与 LRU 轮转相反）"

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
        return rank_descending(key_id, lru_scores, all_key_ids)


register_preset_dimension(CacheAffinityDimension())
