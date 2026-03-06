"""load_balance preset dimension."""

from __future__ import annotations

import random
from typing import Any

from .registry import PresetDimensionBase, register_preset_dimension


class LoadBalanceDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "load_balance"

    @property
    def label(self) -> str:
        return "负载均衡"

    @property
    def description(self) -> str:
        return "随机分散 Key 使用，均匀分摊负载"

    @property
    def mutex_group(self) -> str | None:
        return "distribution_mode"

    @property
    def evidence_hint(self) -> str | None:
        return "每次随机分值，实现完全均匀分散"

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
        return random.random()


register_preset_dimension(LoadBalanceDimension())
